use crate::comms::CommsVerifier;
use crate::db::Database2;
use crate::primitives::{unix_time, Account, AccountType, Challenge, ChallengeStatus, Result};
use crate::verifier::Verifier2;
use reqwest::header::{self, HeaderValue};
use reqwest::{Client, Request};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, Value, ValueRef};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::convert::{TryFrom, TryInto};
use std::result::Result as StdResult;
use tokio::time::{self, Duration};

#[derive(Debug, Clone, Eq, PartialEq, Deserialize)]
pub struct TwitterId(u64);

impl TwitterId {
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl From<u64> for TwitterId {
    fn from(val: u64) -> Self {
        TwitterId(val)
    }
}

impl TryFrom<String> for TwitterId {
    type Error = TwitterError;

    fn try_from(val: String) -> StdResult<Self, Self::Error> {
        Ok(TwitterId(
            val.parse::<u64>()
                .map_err(|_| TwitterError::UnrecognizedData)?,
        ))
    }
}

impl ToSql for TwitterId {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Owned(Value::Integer(self.0 as i64)))
    }
}

impl FromSql for TwitterId {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Integer(val) => Ok(TwitterId(val as u64)),
            _ => Err(FromSqlError::InvalidType),
        }
    }
}

#[derive(Debug, Fail)]
pub enum TwitterError {
    #[fail(display = "The builder was not used correctly")]
    IncompleteBuilder,
    #[fail(display = "Unrecognized data returned from the Twitter API")]
    UnrecognizedData,
    #[fail(display = "Error from Twitter API: {:?}", 0)]
    ApiCode(TwitterApiError),
    #[fail(display = "HTTP error: {}", 0)]
    Http(failure::Error),
    #[fail(display = "Failed to (de-)serialize JSON data: {}", 0)]
    Serde(failure::Error),
    #[fail(display = "Failed to build request: {}", 0)]
    RequestBuilder(failure::Error),
}

#[derive(Debug, Clone, Deserialize)]
pub struct TwitterApiError {
    errors: Vec<ApiErrorObject>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorObject {
    code: i64,
    message: String,
}

pub struct TwitterBuilder {
    screen_name: Option<Account>,
    consumer_key: Option<String>,
    consumer_secret: Option<String>,
    sig_method: Option<String>,
    token: Option<String>,
    token_secret: Option<String>,
    version: Option<f64>,
    db: Database2,
    comms: CommsVerifier,
}

impl TwitterBuilder {
    pub fn new(db: Database2, comms: CommsVerifier) -> Self {
        TwitterBuilder {
            screen_name: None,
            consumer_key: None,
            consumer_secret: None,
            sig_method: None,
            token: None,
            token_secret: None,
            version: None,
            db: db,
            comms: comms,
        }
    }
    pub fn screen_name(mut self, account: Account) -> Self {
        self.screen_name = Some(account);
        self
    }
    pub fn consumer_key(mut self, key: String) -> Self {
        self.consumer_key = Some(key);
        self
    }
    pub fn consumer_secret(mut self, key: String) -> Self {
        self.consumer_secret = Some(key);
        self
    }
    pub fn sig_method(mut self, method: String) -> Self {
        self.sig_method = Some(method);
        self
    }
    pub fn token(mut self, token: String) -> Self {
        self.token = Some(token);
        self
    }
    pub fn token_secret(mut self, secret: String) -> Self {
        self.token_secret = Some(secret);
        self
    }
    pub fn version(mut self, version: f64) -> Self {
        self.version = Some(version);
        self
    }
    pub fn build(self) -> Result<Twitter> {
        Ok(Twitter {
            client: Client::new(),
            screen_name: self.screen_name.ok_or(TwitterError::IncompleteBuilder)?,
            consumer_key: self.consumer_key.ok_or(TwitterError::IncompleteBuilder)?,
            consumer_secret: self
                .consumer_secret
                .ok_or(TwitterError::IncompleteBuilder)?,
            sig_method: self.sig_method.ok_or(TwitterError::IncompleteBuilder)?,
            token: self.token.ok_or(TwitterError::IncompleteBuilder)?,
            token_secret: self.token_secret.ok_or(TwitterError::IncompleteBuilder)?,
            version: self.version.ok_or(TwitterError::IncompleteBuilder)?,
            db: self.db,
            comms: self.comms,
        })
    }
}

pub struct Twitter {
    client: Client,
    screen_name: Account,
    consumer_key: String,
    consumer_secret: String,
    sig_method: String,
    token: String,
    token_secret: String,
    version: f64,
    db: Database2,
    comms: CommsVerifier,
}

use hmac::{Hmac, Mac, NewMac};
use sha1::Sha1;

enum HttpMethod {
    POST,
    GET,
}

impl HttpMethod {
    fn as_str(&self) -> &'static str {
        use HttpMethod::*;

        match self {
            POST => "POST",
            GET => "GET",
        }
    }
}

impl Twitter {
    /// Creates a signature as documented here:
    /// https://developer.twitter.com/en/docs/authentication/oauth-1-0a/creating-a-signature
    fn authenticate_request(
        &self,
        method: &HttpMethod,
        url: &str,
        request: &mut Request,
        params: Option<&[(&str, &str)]>,
    ) -> Result<()> {
        use urlencoding::encode;

        let challenge = Challenge::gen_random();
        let timestamp = unix_time().to_string();
        let version = format!("{:.1}", self.version);

        let mut fields = vec![
            ("oauth_consumer_key", self.consumer_key.as_str()),
            ("oauth_nonce", challenge.as_str()),
            ("oauth_signature_method", self.sig_method.as_str()),
            ("oauth_timestamp", &timestamp),
            ("oauth_token", self.token.as_str()),
            ("oauth_version", &version),
        ];

        if let Some(params) = params {
            fields.append(&mut params.to_vec());
        }

        fields.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut params = String::new();
        for (name, val) in &fields {
            params.push_str(&format!("{}={}&", encode(name), encode(val)));
        }

        // Remove the trailing `&`.
        params.pop();

        let base = format!("{}&{}&{}", method.as_str(), encode(url), encode(&params));

        // Sign the base string.
        let sign_key = format!(
            "{}&{}",
            encode(&self.consumer_secret),
            encode(&self.token_secret)
        );

        let mut mac: Hmac<Sha1> = Hmac::new_varkey(sign_key.as_bytes()).unwrap();
        mac.update(base.as_bytes());

        // Create the resulting hash.
        let sig = base64::encode(mac.finalize().into_bytes());

        // Insert the signature;
        fields.push(("oauth_signature", &sig));
        fields.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut oauth_header = String::new();
        oauth_header.push_str("OAuth ");

        for (name, val) in &fields {
            oauth_header.push_str(&format!("{}={}, ", encode(name), encode(val)))
        }

        // Remove the trailing `, `.
        oauth_header.pop();
        oauth_header.pop();

        // Inserth the authentication header into the request.
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, HeaderValue::from_str(&oauth_header)?);

        Ok(())
    }
    pub async fn get_request<T: DeserializeOwned>(
        &self,
        url: &str,
        params: Option<&[(&str, &str)]>,
    ) -> StdResult<T, TwitterError> {
        let mut full_url = String::from(url);

        if let Some(params) = params {
            full_url.push('?');
            for (key, val) in params {
                full_url.push_str(&format!("{}={}&", key, val));
            }

            // Remove trailing `&` or `?` in case "params" is empty.
            full_url.pop();
        }

        let mut request = self
            .client
            .get(&full_url)
            .build()
            .map_err(|err| TwitterError::RequestBuilder(err.into()))?;

        self.authenticate_request(&HttpMethod::GET, url, &mut request, params)
            .map_err(|err| TwitterError::RequestBuilder(err.into()))?;

        let resp = self
            .client
            .execute(request)
            .await
            .map_err(|err| TwitterError::Http(err.into()))?;

        let txt = resp
            .text()
            .await
            .map_err(|_| TwitterError::UnrecognizedData)?;

        trace!("GET response: {}", txt);

        serde_json::from_str::<T>(&txt).map_err(|err| {
            if let Ok(api_err) = serde_json::from_str::<TwitterApiError>(&txt) {
                TwitterError::ApiCode(api_err)
            } else {
                TwitterError::Serde(err.into())
            }
        })
    }
    pub async fn post_request<T: DeserializeOwned, B: Serialize>(
        &self,
        url: &str,
        body: B,
    ) -> StdResult<T, TwitterError> {
        let mut request = self
            .client
            .post(url)
            .body(serde_json::to_string(&body).map_err(|err| TwitterError::Serde(err.into()))?)
            .build()
            .map_err(|err| TwitterError::RequestBuilder(err.into()))?;

        self.authenticate_request(&HttpMethod::POST, url, &mut request, None)
            .map_err(|err| TwitterError::RequestBuilder(err.into()))?;

        let resp = self
            .client
            .execute(request)
            .await
            .map_err(|err| TwitterError::Http(err.into()))?;

        let txt = resp
            .text()
            .await
            .map_err(|_| TwitterError::UnrecognizedData)?;

        trace!("POST response: {}", txt);

        serde_json::from_str::<T>(&txt).map_err(|err| {
            if let Ok(api_err) = serde_json::from_str::<TwitterApiError>(&txt) {
                TwitterError::ApiCode(api_err)
            } else {
                TwitterError::Serde(err.into())
            }
        })
    }
    async fn request_messages(
        &self,
        exclude_me: &TwitterId,
        watermark: u64,
    ) -> Result<(Vec<ReceivedMessageContext>, u64)> {
        self.get_request::<ApiMessageEvent>(
            "https://api.twitter.com/1.1/direct_messages/events/list.json",
            None,
        )
        .await?
        .get_messages(exclude_me, watermark)
    }
    // TODO: Should return error if empty list gets passed on.
    async fn lookup_twitter_id(
        &self,
        twitter_ids: Option<&[&TwitterId]>,
        accounts: Option<&[&Account]>,
    ) -> Result<Vec<(Account, TwitterId)>> {
        let mut params = vec![];

        // Lookups for UserIds
        let mut lookup = String::new();
        if let Some(twitter_ids) = twitter_ids {
            for twitter_id in twitter_ids {
                lookup.push_str(&twitter_id.as_u64().to_string());
                lookup.push(',');
            }

            // Remove trailing `,`.
            lookup.pop();

            params.push(("user_id", lookup.as_str()))
        }

        // Lookups for Accounts
        let mut lookup = String::new();
        if let Some(accounts) = accounts {
            for account in accounts {
                lookup.push_str(&account.as_str().replace("@", ""));
                lookup.push(',');
            }

            // Remove trailing `,`.
            lookup.pop();

            params.push(("screen_name", lookup.as_str()))
        }

        #[derive(Deserialize)]
        // Only `screen_name` required.
        struct UserObject {
            id: TwitterId,
            screen_name: String,
        }

        let user_objects = self
            .get_request::<Vec<UserObject>>(
                "https://api.twitter.com/1.1/users/lookup.json",
                Some(&params),
            )
            .await?;

        if user_objects.is_empty() {
            return Err(TwitterError::UnrecognizedData.into());
        }

        Ok(user_objects
            .into_iter()
            .map(|obj| (Account::from(format!("@{}", obj.screen_name)), obj.id))
            .collect())
    }
    pub async fn send_message(&self, id: &TwitterId, msg: String) -> StdResult<(), TwitterError> {
        self.post_request::<ApiMessageEvent, _>(
            "https://api.twitter.com/1.1/direct_messages/events/new.json",
            ApiMessageEvent::new(id, msg),
        )
        .await
        .map(|_| ())
    }
    pub async fn start(self) {
        // TODO: Improve error case
        let my_id = self
            .lookup_twitter_id(None, Some(&[&self.screen_name]))
            .await
            .unwrap()
            .remove(0)
            .1;

        let mut interval = time::interval(Duration::from_secs(60));

        loop {
            interval.tick().await;

            let _ = self.handle_incoming_messages(&my_id).await.map_err(|err| {
                error!("{}", err);
            });
        }
    }
    pub async fn handle_incoming_messages(&self, my_id: &TwitterId) -> Result<()> {
        let watermark = self
            .db
            .select_watermark(&AccountType::Twitter)
            .await?
            .or_else(|| Some(0))
            .unwrap();

        let (messages, watermark) = self.request_messages(my_id, watermark).await?;

        if messages.is_empty() {
            trace!("No new messages received");
            return Ok(());
        } else {
            debug!("Received {} new messasge(-s)", messages.len());
        }

        let mut idents = vec![];

        let mut to_lookup = vec![];
        for message in &messages {
            // Avoid duplicates.
            if let Some(_) = idents
                .iter()
                .find(|(_, twitter_id, _)| *twitter_id == &message.sender)
            {
                continue;
            }

            // Lookup TwitterId in database.
            if let Some((account, init_msg)) = self
                .db
                .select_account_from_twitter_id(&message.sender)
                .await?
            {
                debug!(
                    "Found associated match for {}: {}",
                    message.sender.as_u64(),
                    account.as_str()
                );

                // Add items to the identity list, no need to look those up.
                idents.push((account, &message.sender, init_msg));
            } else {
                debug!(
                    "Requiring to lookup screen name for {}",
                    message.sender.as_u64()
                );

                // TwitterIds need to be looked up via the Twitter API.
                to_lookup.push(&message.sender);
            }
        }

        let lookup_results;
        if !to_lookup.is_empty() {
            debug!("Looking up TwitterIds");
            lookup_results = self.lookup_twitter_id(Some(&to_lookup), None).await?;

            for (account, twitter_id) in &lookup_results {
                idents.push((account.clone(), &twitter_id, false));
            }

            self.db
                .insert_twitter_ids(
                    lookup_results
                        .iter()
                        .map(|(account, twitter_id)| (account, twitter_id))
                        .collect::<Vec<(&Account, &TwitterId)>>()
                        .as_slice(),
                )
                .await?;
        }

        for (account, twitter_id, init_msg) in &idents {
            debug!("Starting verification process for {}", account.as_str());

            let challenge_data = self
                .db
                .select_challenge_data(&account, &AccountType::Twitter)
                .await?;

            // TODO: `select_challenge_data` should return an error.
            if challenge_data.is_empty() {
                warn!(
                    "No challenge data found for account {}. Ignoring.",
                    account.as_str()
                );
                continue;
            }

            let mut verifier = Verifier2::new(&challenge_data);

            if !*init_msg {
                self.send_message(&twitter_id, verifier.init_message_builder())
                    .await?;
                self.db.confirm_init_message(&account).await?;
                continue;
            }

            // Verify each message received.
            messages
                .iter()
                .filter(|msg| &msg.sender == *twitter_id)
                .for_each(|msg| verifier.verify(&msg.message));

            for network_address in verifier.valid_verifications() {
                debug!(
                    "Valid verification for address: {}",
                    network_address.address().as_str()
                );

                self.comms
                    .notify_status_change(network_address.address().clone());

                self.db
                    .set_challenge_status(
                        network_address.address(),
                        &AccountType::Twitter,
                        ChallengeStatus::Accepted,
                    )
                    .await?;
            }

            for network_address in verifier.invalid_verifications() {
                debug!(
                    "Invalid verification for address: {}",
                    network_address.address().as_str()
                );

                self.db
                    .set_challenge_status(
                        network_address.address(),
                        &AccountType::Twitter,
                        ChallengeStatus::Rejected,
                    )
                    .await?;
            }

            debug!("Notifying user about verification result");
            self.send_message(&twitter_id, verifier.response_message_builder())
                .await?;
        }

        self.db
            .update_watermark(&AccountType::Twitter, watermark)
            .await?;

        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
struct ApiMessageEvent {
    event: Option<ApiEvent>,
    events: Option<Vec<ApiEvent>>,
}

#[derive(Deserialize, Serialize)]
struct ApiEvent {
    #[serde(rename = "type")]
    t_type: String,
    created_timestamp: Option<String>,
    message_create: ApiMessageCreate,
}

#[derive(Deserialize, Serialize)]
struct ApiMessageCreate {
    target: ApiTarget,
    sender_id: Option<String>,
    message_data: ApiMessageData,
}

#[derive(Deserialize, Serialize)]
struct ApiTarget {
    recipient_id: String,
}

#[derive(Deserialize, Serialize)]
struct ApiMessageData {
    text: String,
}

struct ReceivedMessageContext {
    sender: TwitterId,
    message: String,
    created: u64,
}

impl ApiMessageEvent {
    fn new(recipient: &TwitterId, msg: String) -> Self {
        ApiMessageEvent {
            event: Some(ApiEvent {
                t_type: "message_create".to_string(),
                created_timestamp: None,
                message_create: ApiMessageCreate {
                    target: ApiTarget {
                        recipient_id: recipient.as_u64().to_string(),
                    },
                    sender_id: None,
                    message_data: ApiMessageData { text: msg },
                },
            }),
            events: None,
        }
    }
    fn get_messages(
        self,
        my_id: &TwitterId,
        watermark: u64,
    ) -> Result<(Vec<ReceivedMessageContext>, u64)> {
        let mut msgs = vec![];

        let mut new_watermark = watermark;
        if let Some(events) = self.events {
            for event in events {
                let msg = ReceivedMessageContext {
                    sender: event
                        .message_create
                        .sender_id
                        .ok_or(TwitterError::UnrecognizedData)?
                        .try_into()?,
                    message: event.message_create.message_data.text,
                    created: event
                        .created_timestamp
                        .ok_or(TwitterError::UnrecognizedData)?
                        .parse::<u64>()
                        .map_err(|_| TwitterError::UnrecognizedData)?,
                };

                if &msg.sender != my_id && msg.created > watermark {
                    if msg.created > new_watermark {
                        new_watermark = msg.created;
                    }

                    msgs.push(msg);
                }
            }
        }

        Ok((msgs, new_watermark))
    }
}

#[test]
fn test_twitter() {
    use crate::primitives::Challenge;
    use crate::Database2;
    use tokio::runtime::Runtime;

    // Generate a random db path
    fn db_path() -> String {
        format!("/tmp/sqlite_{}", Challenge::gen_random().as_str())
    }

    let mut rt = Runtime::new().unwrap();
    rt.block_on(async {
        let config = crate::open_config().unwrap();

        let client = TwitterBuilder::new(Database2::new(&db_path()).unwrap(), CommsVerifier::new())
            .screen_name(Account::from("web3registrar"))
            .consumer_key(config.twitter_api_key)
            .consumer_secret(config.twitter_api_secret)
            .sig_method("HMAC-SHA1".to_string())
            .token(config.twitter_token)
            .token_secret(config.twitter_token_secret)
            .version(1.0)
            .build()
            .unwrap();

        #[derive(Debug, Serialize, Deserialize)]
        struct Root {
            next_cursor: Option<String>,
            events: Vec<ApiEvent>,
        }

        #[derive(Debug, Serialize, Deserialize)]
        struct ApiEvent {
            id: Option<String>,
            created_timestamp: Option<String>,
            message_create: ApiMessageCreate,
        }

        #[derive(Debug, Serialize, Deserialize)]
        struct ApiMessageCreate {
            sender_id: String,
            message_data: ApiMessageData,
        }

        #[derive(Debug, Serialize, Deserialize)]
        struct ApiMessageData {
            text: String,
        }

        let _res = client
            .request_messages(&TwitterId::from(1308347585367867393), 0)
            .await
            .unwrap();

        /*
        let res = client
            .lookup_twitter_id(
                Some(&[&TwitterId::from(102128843)]),
                Some(&[&Account::from("@web3registrar")]),
            )
            .await
            .unwrap();

        println!("ACCOUNTS: {:?}", res);

        let res = client
            .send_message(
                &TwitterId::from(1309954318712426496),
                String::from("Hello there, this is a test"),
            )
            .await
            .unwrap();

        client
            .get_request::<Root>(
                "https://api.twitter.com/1.1/direct_messages/events/list.json",
                None,
            )
            .await
            .unwrap();
            */
    });
}
