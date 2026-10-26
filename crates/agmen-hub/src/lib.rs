//! The agmen hub library: the parts of the hub that tests and the `agmen-hub`
//! binary share.
//!
//! [`Hub::connect`] builds the hub from a [`Config`] and [`Hub::router`]
//! serves its HTTP API: device login with GitHub, identities, invites,
//! broker tokens and the JWKS EMQX verifies them with
//! (`docs/adr/0002-broker-emqx.md`). [`Hub::start_messaging`] connects to
//! the broker, ingests every client publish ([`Hub::ingest`]) and runs the
//! sweeper that publishes what ingest stored; [`Hub::post_as_hub`] posts the
//! hub's own messages. [`Bootstrap`] makes a tenant, a project and its first
//! admin for the `agmen-hub bootstrap` command.

#![warn(missing_docs)]

mod auth;
mod bootstrap;
mod broker_tokens;
mod config;
mod db;
mod emqx;
mod http;
mod identities;
mod ingest;
mod invites;
mod mqtt;
mod numbering;
mod secrets_scan;
mod sweeper;

pub use auth::{GithubError, GithubId};
pub use bootstrap::{AdminGithub, Bootstrap, BootstrapError, Bootstrapped};
pub use broker_tokens::{BrokerTokens, SigningKey, TokenError};
pub use config::{Config, ConfigError, EmqxConfig, GithubConfig, MqttConfig, Secret};
pub use emqx::{EmqxAdmin, EmqxError};
pub use http::{Hub, HubError};
pub use identities::{BROKER_TOKEN_TTL, CLIENT_ID_SUFFIXES, HumanId, Identity};
pub use ingest::{HUB_IDENTITY, HubPost, IngestError, PostError, Posted};
pub use mqtt::{Messaging, MessagingError};
pub use numbering::{NumberingError, next_seq};

/// The hub's Postgres migrations from `migrations/`, embedded at compile time.
///
/// Run it before any other query: `MIGRATOR.run(&pool).await?`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();
