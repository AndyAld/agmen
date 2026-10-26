//! The agmen hub library: the parts of the hub that tests and the `agmen-hub`
//! binary share.

#![warn(missing_docs)]

mod numbering;

pub use numbering::{NumberingError, next_seq};

/// The hub's Postgres migrations from `migrations/`, embedded at compile time.
///
/// Run it before any other query: `MIGRATOR.run(&pool).await?`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();
