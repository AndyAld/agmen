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
mod ids;
mod lanes;
mod limiter;
#[cfg(feature = "testing")]
mod mem_store;
mod record;
mod refs;
mod roster;
mod routing;
mod store;

pub use core::{Core, CoreError};
pub use identity::{cap_verbs, is_child_name};
pub use ids::{HUB_IDENTITY, envelope_hash, hub_identity, hub_now, message_id, new_message_id};
pub use lanes::{
    EditRule, LaneError, LaneHolder, LaneList, LaneRow, LaneSpec, LaneSpecError, Lanes, Lends,
    MAX_ACTIVE_LENDS, MAX_CHECK_PATHS, MAX_GLOBS, MAX_GLOBS_PER_LANE, MAX_LANES, MAX_LEND_DURATION,
    MAX_LOADED_LENDS, NOT_A_REPO_PATH, PathCheck, ReplaceError, ResolvedLane, Stray,
    check_lane_specs, judge_scoped, repo_path, scope_globs, stray_body, stray_body_with,
    validate_edits,
};
pub use limiter::{BURST, MAX_TRACKED, RATE_PER_SEC, RateLimiter};
#[cfg(feature = "testing")]
pub use mem_store::{MEM_MAX_ROWS, MemStore, MemTx};
#[doc(hidden)]
pub use record::{AckCopy, Draft, Recorded, write};
pub use record::{HubPost, PostError, Posted, post_alert, post_in};
pub use refs::{MAX_REFS_PER_MESSAGE, candidate_item_keys, stamp_refs};
pub use roster::{Member, MemberRole, Roster, Sender};
pub use routing::{
    Parent, ack_stream, expand_mentions, fan_out, may_send, mentioned, needs_roster, route,
};
pub use store::{MessageRow, Reads, Store, StoreError, StoreTx, parse_kind};
