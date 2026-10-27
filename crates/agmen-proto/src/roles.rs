//! Role permissions sent when minting an identity.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{RoleName, Sha256Hex};

/// The resolved role settings sent to `POST /identities`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
pub struct RoleSpec {
    /// The role's name.
    pub role: RoleName,
    /// The verbs the role may send, capped by its identity kind at mint.
    pub verbs: BTreeSet<String>,
    /// What the role may edit.
    pub edits: EditScope,
    /// The SHA-256 of the role text, without storing the text at the hub.
    pub role_sha256: Sha256Hex,
}

/// What a role may edit, as defined by the roles design's section 3.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum EditScope {
    /// Nothing.
    None,
    /// Paths in claimed or lent lanes, subject to the existing lane checks.
    Lanes,
    /// Paths in claimed or lent lanes that also match the listed globs.
    /// The hub must apply both the existing lane checks and the glob restrictions.
    LanesWithin {
        /// Globs relative to the project root, such as `tests/**`.
        globs: Vec<String>,
    },
    /// Paths matching the listed globs.
    Paths {
        /// Globs relative to the project root, such as `docs/**`.
        globs: Vec<String>,
    },
}
