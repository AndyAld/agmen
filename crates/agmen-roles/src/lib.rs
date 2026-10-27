//! Synchronous role parsing, inheritance, local layers, and launch rendering.
//!
//! ```
//! use agmen_roles::{Layers, render_prompt};
//!
//! let catalog = Layers::default().load()?;
//! if let Some(reviewer) = catalog.get("reviewer") {
//!     assert!(render_prompt(reviewer).contains("<report verb=\"REVIEW\">"));
//! }
//! # Ok::<(), agmen_roles::RoleError>(())
//! ```
#![warn(missing_docs)]

mod ceiling;
/// Compatibility API retained for the desktop and local daemon.
pub mod compat;
mod error;
mod import;
mod layer;
mod render;
mod schema;

pub use compat::{BUILT_IN, RoleKind as Kind, Tier};
pub use error::RoleError;
pub use import::import_subagent;
pub use layer::{Catalog, Layers, Resolved};
pub use render::{claude_tools, render_prompt};
pub use schema::{
    EditScope, Prompt, Remove, Role, ToolClass, ToolSet, Worktree, WorktreeMode, parse,
};
