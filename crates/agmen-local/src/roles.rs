//! Role-file compatibility API, implemented by the synchronous agmen-roles crate.

pub use agmen_roles::compat::{
    BUILT_IN, Edits, ResolvedRole, RoleError, RoleKind, RoleSpec, Tier, kind_may_send, parse,
    resolve, words,
};

#[cfg(test)]
#[path = "roles_tests.rs"]
mod tests;
