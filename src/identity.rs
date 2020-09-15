use super::{Address, AddressType, Challenge, PubKey, Result, RoomId};
use crate::db::{Database, ScopedDatabase};
use crossbeam::channel::{unbounded, Receiver, Sender};
use failure::err_msg;
use std::collections::HashMap;
use std::convert::TryInto;
use std::sync::atomic::AtomicBool;

#[derive(Serialize, Deserialize)]
pub struct OnChainIdentity {
    pub_key: PubKey,
    // TODO: Should this just be a String?
    display_name: Option<AddressState>,
    // TODO: Should this just be a String?
    legal_name: Option<AddressState>,
    email: Option<AddressState>,
    web: Option<AddressState>,
    twitter: Option<AddressState>,
    matrix: Option<AddressState>,
}

impl OnChainIdentity {
    fn from_json(val: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(&val)?)
    }
    fn to_json(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
}

#[derive(Serialize, Deserialize)]
pub struct AddressState {
    addr: Address,
    addr_type: AddressType,
    addr_validity: AddressValidity,
    challenge: Challenge,
    confirmed: AtomicBool,
}

impl AddressState {
    pub fn new(addr: Address, addr_type: AddressType) -> Self {
        AddressState {
            addr: addr,
            addr_type: addr_type,
            addr_validity: AddressValidity::Unknown,
            challenge: Challenge::gen_random(),
            confirmed: AtomicBool::new(false),
        }
    }
}

#[derive(Eq, PartialEq, Serialize, Deserialize)]
enum AddressValidity {
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "valid")]
    Valid,
    #[serde(rename = "invalid")]
    Invalid,
}

pub enum CommsMessage {
    NewOnChainIdentity(OnChainIdentity),
    Inform {
        context: AddressContext,
        challenge: Challenge,
        room_id: Option<RoomId>,
    },
    ValidAddress {
        context: AddressContext,
    },
    InvalidAddress {
        context: AddressContext,
    },
    RoomId {
        pub_key: PubKey,
        room_id: RoomId,
    },
}

pub struct AddressContext {
    pub pub_key: PubKey,
    pub address: Address,
    pub address_ty: AddressType,
}

pub struct CommsMain {
    sender: Sender<CommsMessage>,
    // TODO: This can be removed.
    address_ty: AddressType,
}

// TODO: Avoid clones
impl CommsMain {
    fn inform(
        &self,
        pub_key: &PubKey,
        address: &Address,
        challenge: &Challenge,
        room_id: Option<RoomId>,
    ) {
        self.sender
            .send(CommsMessage::Inform {
                context: AddressContext {
                    pub_key: pub_key.clone(),
                    address: address.clone(),
                    address_ty: self.address_ty.clone(),
                },
                challenge: challenge.clone(),
                room_id,
            })
            .unwrap();
    }
}

pub struct CommsVerifier {
    tx: Sender<CommsMessage>,
    recv: Receiver<CommsMessage>,
    address_ty: AddressType,
}

// TODO: Avoid clones
impl CommsVerifier {
    pub fn recv(&self) -> CommsMessage {
        self.recv.recv().unwrap()
    }
    /// Receive a `Inform` message. This is only used by the Matrix client as
    /// any other message type will panic.
    pub fn recv_inform(&self) -> (AddressContext, Challenge, Option<RoomId>) {
        if let CommsMessage::Inform {
            context,
            challenge,
            room_id,
        } = self.recv()
        {
            (context, challenge, room_id)
        } else {
            panic!("received invalid message type on Matrix client");
        }
    }
    pub fn valid_feedback(&self, pub_key: &PubKey, addr: &Address) {
        self.tx
            .send(CommsMessage::ValidAddress {
                context: AddressContext {
                    pub_key: pub_key.clone(),
                    address: addr.clone(),
                    address_ty: self.address_ty.clone(),
                },
            })
            .unwrap();
    }
    pub fn invalid_feedback(&self, pub_key: &PubKey, addr: &Address) {
        self.tx
            .send(CommsMessage::InvalidAddress {
                context: AddressContext {
                    pub_key: pub_key.clone(),
                    address: addr.clone(),
                    address_ty: self.address_ty.clone(),
                },
            })
            .unwrap();
    }
    pub fn track_room_id(&self, pub_key: &PubKey, room_id: &RoomId) {
        self.tx
            .send(CommsMessage::RoomId {
                pub_key: pub_key.clone(),
                room_id: room_id.clone(),
            })
            .unwrap();
    }
}

pub struct IdentityManager<'a> {
    idents: Vec<OnChainIdentity>,
    db_idents: ScopedDatabase<'a>,
    db_rooms: ScopedDatabase<'a>,
    comms: CommsTable,
}

struct CommsTable {
    to_main: Sender<CommsMessage>,
    listener: Receiver<CommsMessage>,
    pairs: HashMap<AddressType, CommsMain>,
}

impl<'a> IdentityManager<'a> {
    pub fn new(db: &'a Database) -> Result<Self> {
        let db_idents = db.scope("pending_identities");
        let db_rooms = db.scope("pending_identities");

        let mut idents = vec![];

        // Read pending on-chain identities from storage. Ideally, there are none.
        for (_, value) in db_idents.all()? {
            idents.push(OnChainIdentity::from_json(&*value)?);
        }

        let (tx, recv) = unbounded();

        Ok(IdentityManager {
            idents: idents,
            db_idents: db_idents,
            db_rooms: db_rooms,
            comms: CommsTable {
                to_main: tx,
                listener: recv,
                pairs: HashMap::new(),
            },
        })
    }
    pub fn register_comms(&mut self, addr_type: AddressType) -> CommsVerifier {
        let (tx, recv) = unbounded();

        self.comms.pairs.insert(
            addr_type.clone(),
            CommsMain {
                sender: tx,
                address_ty: addr_type.clone(),
            },
        );

        CommsVerifier {
            tx: self.comms.to_main.clone(),
            recv: recv,
            address_ty: addr_type,
        }
    }
    pub async fn start(&mut self) -> Result<()> {
        use CommsMessage::*;

        loop {
            if let Ok(msg) = self.comms.listener.recv() {
                match msg {
                    CommsMessage::NewOnChainIdentity(ident) => {
                        self.register_request(ident)?;
                    }
                    CommsMessage::Inform { .. } => {
                        // INVALID
                        // TODO: log
                    }
                    ValidAddress { context: _ } => {}
                    InvalidAddress { context: _ } => {}
                    RoomId { pub_key, room_id } => {
                        self.db_rooms.put(pub_key.0, room_id.0.as_bytes()).unwrap();
                    }
                }
            }
        }

        Ok(())
    }
    fn register_request(&mut self, ident: OnChainIdentity) -> Result<()> {
        // Only add the identity to the list if it doesn't exists yet.
        if self
            .idents
            .iter()
            .find(|ident| ident.pub_key == ident.pub_key)
            .is_none()
        {
            // Save the pending on-chain identity to disk.
            self.db_idents
                .put(ident.pub_key.0.to_bytes(), ident.to_json()?)?;
            self.idents.push(ident);
        }

        let ident = self
            .idents
            .last()
            .ok_or(err_msg("last registered identity not found."))?;

        // TODO: Handle additional address types.
        ident.matrix.as_ref().map(|state| {
            let room_id = self
                .db_rooms
                .get(&ident.pub_key.0)
                .unwrap()
                .map(|bytes| bytes.try_into().unwrap());

            self.comms.pairs.get(&state.addr_type).unwrap().inform(
                &ident.pub_key,
                &state.addr,
                &state.challenge,
                room_id,
            );
        });

        Ok(())
    }
}
