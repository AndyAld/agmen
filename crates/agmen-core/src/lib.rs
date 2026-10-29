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
mod lanes;
#[cfg(feature = "testing")]
mod mem_store;
mod store;

pub use core::{Core, CoreError};
pub use identity::{cap_verbs, is_child_name};
pub use lanes::{
    EditRule, LaneError, LaneHolder, LaneList, LaneRow, LaneSpec, LaneSpecError, Lanes, Lends,
    MAX_ACTIVE_LENDS, MAX_CHECK_PATHS, MAX_LEND_DURATION, MAX_LOADED_LENDS, NOT_A_REPO_PATH,
    PathCheck, ReplaceError, ResolvedLane, Stray, check_lane_specs, judge_scoped, repo_path,
    scope_globs, stray_body, stray_body_with, validate_edits,
};
#[cfg(feature = "testing")]
pub use mem_store::{MEM_MAX_ROWS, MemStore, MemTx};
pub use store::{Reads, Store, StoreError, StoreTx};
