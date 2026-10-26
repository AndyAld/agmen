//! MQTT 5 session, SQLite outbox, cursors, and replay for agmen clients:
//! the library `agmen` (the CLI), `agmen mcp` and `agmen watch` share.

#![warn(missing_docs)]

mod cursor;
mod db;
mod error;
mod event;
mod hub;
mod identity;
mod link;
mod mqtt_store;
mod outbox;
mod replay;
mod session;
mod token;
mod token_store;

pub use cursor::{Advance, Cursors};
pub use error::ClientError;
pub use event::{ClientEvent, Me};
pub use hub::{HubClient, PAGE_LIMIT, Page};
pub use identity::{
    IDENTITY_DIR, IDENTITY_FILE, IdentityError, IdentityFile, Joined, find_identity,
};
pub use outbox::{Outbox, OutboxState};
pub use session::{Broker, ClientKind, Events, STATE_DB, Session, SessionConfig};
pub use token::TokenSource;
pub use token_store::{TokenKey, TokenStore, TokenStoreError};
