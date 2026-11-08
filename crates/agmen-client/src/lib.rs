//! MQTT 5 session, SQLite outbox, cursors, replay, and one-off reads of
//! retained state for agmen clients: the library `agmen` (the CLI),
//! `agmen mcp` and `agmen watch` share.

#![warn(missing_docs)]

mod body;
mod cursor;
mod db;
mod device;
mod error;
mod event;
mod hub;
mod identity;
mod install_receipt;
mod link;
mod local_credential;
mod login;
mod mqtt_store;
mod origin;
mod outbox;
mod paths;
mod replay;
mod session;
mod state;
mod token;
mod token_store;

pub use body::{
    BodyReadError, MAX_HUB_RESPONSE_BYTES, MAX_PAGE_RESPONSE_BYTES, read_hub_body,
    read_hub_body_with_limit,
};
pub use cursor::{Advance, Cursors, FIRST_WATCH_BACKLOG};
pub use db::{preflight_schema, store_schema};
pub use device::{DeviceStart, device_start, device_wait, signed_in_as};
pub use error::ClientError;
pub use event::{ClientEvent, FIRST_WATCH_NOTICE, Me};
pub use hub::{HubClient, HubUrlError, PAGE_LIMIT, Page, check_hub_url};
pub use identity::{
    IDENTITY_DIR, IDENTITY_FILE, IdentityError, IdentityFile, Joined, find_identity,
};
pub use install_receipt::{
    InstallReceipt, ReceiptError, install_receipt_path, read_install_receipt,
    read_install_receipt_at,
};
pub use link::retry_delay;
pub use local_credential::{LOCAL_TOKEN_NAMESPACE, LocalCredential, read_local_credential};
pub use login::{
    HumanTokens, Login, LoginError, check_login_lock, human_tokens, save_login, store_tokens,
    token_grant,
};
pub use origin::{
    LOCAL_BROKER_URL, LOCAL_HUB_URL, LOCAL_ORIGIN, is_local, local_broker, local_hub_url,
    origin_key,
};
pub use outbox::{Outbox, OutboxState};
pub use paths::{AGMEND_SOCKET, PathError, agmend_socket, config_dir, login_file, state_dir};
#[cfg(unix)]
pub use paths::{AgmendSocketError, check_agmend_socket, check_agmend_socket_for};
pub use session::{Broker, ClientKind, Events, Presence, STATE_DB, Session, SessionConfig};
pub use state::{read_state, resolve_handle};
pub use token::TokenSource;
pub use token_store::{TokenKey, TokenStore, TokenStoreError};
#[cfg(any(test, feature = "test-hooks"))]
pub use token_store::{TokenWriteFailure, fail_token_writes};
