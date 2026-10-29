//! Live project members used by message rules.

use agmen_proto::{IdentityId, IdentityKind};

/// A human's membership role, with the names stored in `memberships.role`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum MemberRole {
    /// Can mint agents.
    Member,
    /// Can also mint coordinator identities and invite members.
    Coordinator,
    /// Can also invite coordinators and admins.
    Admin,
}

impl MemberRole {
    /// The name stored in `memberships.role`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Coordinator => "coordinator",
            Self::Admin => "admin",
        }
    }
}

impl std::fmt::Display for MemberRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who sent a publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sender {
    /// The sender identity id.
    pub id: IdentityId,
    /// The identity kind.
    pub kind: IdentityKind,
    /// The handle shown on messages.
    pub handle: String,
    /// The human who owns an identity; `None` for a human.
    pub owner: Option<IdentityId>,
    /// Stored verb permissions, or `None` for the kind default.
    pub verbs: Option<Vec<String>>,
}

/// One live member of a project: a non-revoked identity or a human
/// member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The member identity id.
    pub id: IdentityId,
    /// The handle shown on messages.
    pub handle: String,
    /// The identity kind.
    pub kind: IdentityKind,
    /// The owning human, or `None` for a human.
    pub owner: Option<IdentityId>,
    /// The human membership role, or `None` for an identity.
    pub role: Option<MemberRole>,
    /// The stored mint role used by role mentions.
    pub identity_role: Option<String>,
}

/// A project's live members.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roster(
    /// Members in the order supplied by storage.
    pub Vec<Member>,
);

impl Roster {
    /// Finds a live member by id.
    #[must_use]
    pub fn get(&self, id: &IdentityId) -> Option<&Member> {
        self.0.iter().find(|member| &member.id == id)
    }

    /// coordinators(project) of ruling R35: the live `coordinator`
    /// identities and the humans whose membership role is coordinator.
    /// Admins are not included until the user decides.
    pub fn coordinators(&self) -> impl Iterator<Item = &IdentityId> {
        self.0
            .iter()
            .filter(|member| {
                member.kind == IdentityKind::Coordinator
                    || (member.kind == IdentityKind::Human
                        && member.role == Some(MemberRole::Coordinator))
            })
            .map(|member| &member.id)
    }
}
