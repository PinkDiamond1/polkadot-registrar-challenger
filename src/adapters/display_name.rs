use crate::comms::{CommsMessage, CommsVerifier};
use crate::primitives::{Account, Result};
use crate::Database2;

pub struct StringMatcher {
    db: Database2,
    comms: CommsVerifier,
}

impl StringMatcher {
    pub fn new(db: Database2, comms: CommsVerifier) -> Self {
        StringMatcher {
            db: db,
            comms: comms,
        }
    }
    pub async fn start(self) {
        loop {
            let _ = self.local().await.map_err(|err| {
                error!("{}", err);
                err
            });
        }
    }
    pub async fn local(&self) -> Result<()> {
        use CommsMessage::*;

        match self.comms.recv().await {
            AccountToVerify {
                net_account,
                account,
            } => self.handle_display_name_matching(&account).await?,
            _ => error!("Received unrecognized message type"),
        }

        Ok(())
    }
    pub async fn handle_display_name_matching(&self, account: &Account) -> Result<()> {
        let display_names = self.db.select_display_names().await?;

        Ok(())
    }
}
