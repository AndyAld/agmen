//! Storage-independent coordination shared by the hub and solo mode.
//!
//! ```no_run
//! use std::time::Duration;
//! use agmen_core::{Core, Store, StoreError, StoreTx};
//! use agmen_proto::Seq;
//!
//! async fn number<S: Store>(store: S) -> Result<Seq, StoreError> {
//!     let core = Core::new(store, Duration::from_secs(60));
//!     let mut tx = core.store().begin().await?;
//!     let seq = tx.next_seq("feed").await?;
//!     tx.commit().await?;
//!     Ok(seq)
//! }
//! ```

#![warn(missing_docs)]

mod core;
mod identity;
#[cfg(feature = "testing")]
mod mem_store;
mod store;

pub use core::Core;
pub use identity::{cap_verbs, is_child_name};
#[cfg(feature = "testing")]
pub use mem_store::{MEM_MAX_ROWS, MemStore, MemTx};
pub use store::{Reads, Store, StoreError, StoreTx};
