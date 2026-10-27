//! Role files (spec 3.2), parsed and resolved by agmen-local so agmend and
//! the desktop read them the same way (Task 8).

pub(crate) use agmen_local::roles::{
    BUILT_IN, Edits, RoleKind, RoleSpec, Tier, kind_may_send, parse, resolve, words,
};
