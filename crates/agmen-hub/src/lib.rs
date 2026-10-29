//! The agmen hub library: the parts of the hub that tests and the `agmen-hub`
//! binary share.
//!
//! [`Hub::connect`] builds the hub from a [`Config`] and [`Hub::router`]
//! serves its HTTP API: device login with GitHub, who is signed in
//! (`GET /v1/me`), identities, invites, broker tokens and the JWKS EMQX
//! verifies them with (`docs/adr/0002-broker-emqx.md`).
//! [`Hub::start_messaging`] connects to
//! the broker, ingests every client publish ([`Hub::ingest`]) and runs the
//! sweeper that publishes what ingest stored; [`Hub::post_as_hub`] posts the
//! hub's own messages. The hub also serves replay of every stream and a
//! member's chat as server-sent events, keeps
//! each project's retained `state/roster` from presence, and answers
//! ROLLCALL. The ledger (lanes, items and claims, design 6.5) answers
//! CLAIM, FOUND and DONE as they are ingested, serves lanes, items and path
//! checks over HTTP, and [`Hub::run_lease_sweeper`] releases claims whose
//! lease ran out. [`Bootstrap`] makes a tenant, a project and its first
//! admin for the `agmen-hub bootstrap` command.

#![warn(missing_docs)]

mod auth;
mod bootstrap;
mod broker_tokens;
mod claims;
mod config;
mod db;
mod emqx;
mod events;
mod group;
mod health;
mod http;
mod human_post;
mod identities;
mod ingest;
mod invite_page;
mod invites;
mod items;
mod lanes;
mod me;
mod mqtt;
mod mqtt_proxy;
mod numbering;
mod presence;
mod projects;
mod refs;
mod replay;
mod roles;
mod rollcall;
mod secrets_scan;
mod self_config;
mod state;
mod sweeper;
mod tenants;
#[cfg(feature = "test-hooks")]
mod test_github;
#[cfg(feature = "test-hooks")]
mod test_hooks;

pub use auth::{GithubError, GithubId};
pub use bootstrap::{AdminGithub, Bootstrap, BootstrapError, Bootstrapped};
pub use broker_tokens::{BrokerTokens, SigningKey, TokenError};
pub use claims::LeaseSweepError;
pub use config::{
    Config, ConfigError, DEFAULT_CLAIM_LEASE, DEFAULT_GITHUB_CLIENT_ID, EmqxConfig, GithubConfig,
    HubMode, HubSecrets, MqttConfig, Secret,
};
pub use db::{Role, TenantRole, UnknownRole};
pub use emqx::{EmqxAdmin, EmqxError};
pub use group::GroupError;
pub use http::{HEADER_READ_TIMEOUT, Hub, HubError, serve};
pub use identities::{BROKER_TOKEN_TTL, CLIENT_ID_SUFFIXES, HumanId, Identity};
pub use ingest::{HUB_IDENTITY, HubPost, IngestError, PostError, Posted};
pub use mqtt::{Messaging, MessagingError};
pub use numbering::{NumberingError, next_seq};
pub use roles::cap_verbs;
pub use self_config::SelfConfigError;
#[cfg(feature = "test-hooks")]
pub use test_github::{FAKE_GITHUB_CLIENT_ID, FakeGithub};
#[cfg(feature = "test-hooks")]
pub use test_hooks::{HookError, SeededHuman, seed_tenant_member};

/// The hub's Postgres migrations from `migrations/`, embedded at compile time.
///
/// Run it before any other query: `MIGRATOR.run(&pool).await?`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();
