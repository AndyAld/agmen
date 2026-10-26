//! The error every agmen-client API returns.

use agmen_proto::MessageId;

/// Why an agmen-client call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// The identity's SQLite file could not be opened, read or written.
    #[error("the local store failed")]
    Store(#[from] rusqlite::Error),
    /// The SQLite file would not switch to WAL mode.
    #[error("the local store is in journal mode {0:?}, not WAL")]
    StoreMode(String),
    /// The SQLite file was written by a newer agmen.
    #[error("the local store has schema version {0}, newer than this agmen knows")]
    StoreVersion(i64),
    /// A row in the local store holds a value this version cannot read.
    #[error("the local store holds an invalid value: {0}")]
    Stored(String),
    /// A seq too large for the local store.
    #[error("seq {0} is out of range")]
    SeqOutOfRange(u64),
    /// The outbox already holds a message with this id.
    #[error("the outbox already holds message {0}")]
    DuplicateMessage(MessageId),
    /// The outbox holds no message with this id.
    #[error("the outbox holds no message {0}")]
    UnknownMessage(MessageId),
    /// An envelope could not be encoded as JSON.
    #[error("encoding an envelope failed")]
    Encode(#[source] serde_json::Error),
    /// An envelope is larger than the protocol allows.
    #[error("the envelope is {len} bytes, over the {limit} byte limit")]
    EnvelopeTooLarge {
        /// Its size.
        len: usize,
        /// The limit.
        limit: usize,
    },
    /// The hub could not be reached, or its answer could not be read.
    #[error("calling the hub failed")]
    Http(#[source] reqwest::Error),
    /// The hub refused a request.
    #[error("the hub answered {status} {code}")]
    Hub {
        /// The HTTP status.
        status: u16,
        /// The hub's error code, such as `forbidden_stream`; empty when the
        /// answer named none.
        code: String,
    },
    /// A broker token this session cannot use.
    #[error("{0}")]
    Token(String),
    /// The broker address is not one the session can connect to.
    #[error("the broker address {0:?} is not mqtt://host:port or wss://host[:port]/path")]
    BrokerAddress(String),
    /// The MQTT client could not be built.
    #[error("the MQTT client settings are invalid")]
    MqttClient(#[source] rumqttc_next::ClientBuildError),
    /// The broker refused or dropped the connection.
    #[error("connecting to the broker failed")]
    Connect(#[source] rumqttc_next::ConnectionError),
    /// The broker refused a subscription.
    #[error("the broker refused the subscription: {0}")]
    Subscribe(String),
    /// The broker did not answer the first connect in time.
    #[error("the broker did not answer within {0:?}")]
    Timeout(std::time::Duration),
    /// The connection failed too many times in a row.
    #[error("the broker connection failed {attempts} times in a row")]
    GaveUp {
        /// Attempts made.
        attempts: u32,
        /// The last failure.
        #[source]
        source: rumqttc_next::ConnectionError,
    },
    /// Replay failed too many times in a row.
    #[error("replay from the hub failed {attempts} times in a row")]
    ReplayGaveUp {
        /// Attempts made.
        attempts: u32,
        /// The last failure.
        #[source]
        source: Box<ClientError>,
    },
    /// The identity's state folder could not be created.
    #[error("creating the state folder {path} failed")]
    StateDir {
        /// The folder.
        path: std::path::PathBuf,
        /// What failed.
        #[source]
        source: std::io::Error,
    },
    /// The session stopped, so the request could not be queued.
    #[error("the session has stopped")]
    Closed,
    /// A session task panicked.
    #[error("a session task panicked")]
    Panicked(#[source] tokio::task::JoinError),
}

impl ClientError {
    /// Whether the hub refused this identity for good (401 or 403), so
    /// asking again will not help.
    #[must_use]
    pub const fn is_refusal(&self) -> bool {
        matches!(
            self,
            Self::Hub {
                status: 401 | 403,
                ..
            }
        )
    }
}
