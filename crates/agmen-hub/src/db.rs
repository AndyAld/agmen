//! The hub's database: connecting, minting row ids and secrets, and the
//! membership roles the tables store.

use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// The most connections one hub process holds.
const MAX_CONNECTIONS: u32 = 10;

/// 32 letters and digits from `[a-z0-9]`, without `0`, `1`, `l` and `o`, so
/// a random byte masked to 5 bits picks one with no bias.
const ID_ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";

/// Length of the ids the hub mints: 16 characters, 80 random bits, inside
/// the protocol's `[a-z0-9]{8,32}`.
const ID_LEN: usize = 16;

/// The longest tenant or project name, in characters.
const MAX_NAME_CHARS: usize = 64;

pub(crate) fn is_display_name(name: &str) -> bool {
    (1..=MAX_NAME_CHARS).contains(&name.chars().count())
        && !name.chars().any(char::is_control)
        && name.trim() == name
}

/// Connects to Postgres and runs the hub's migrations.
pub(crate) async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(MAX_CONNECTIONS)
        .connect(url)
        .await?;
    crate::MIGRATOR.run(&pool).await?;
    Ok(pool)
}

/// A fresh id for a human, identity or other hub row.
pub(crate) fn new_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; ID_LEN];
    getrandom::fill(&mut bytes)?;
    Ok(bytes
        .iter()
        .map(|b| char::from(ID_ALPHABET[usize::from(b & 31)]))
        .collect())
}

/// A fresh secret for a token, invite code, device code or OAuth state:
/// 32 random bytes in lowercase hex.
pub(crate) fn new_secret() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(hex(&bytes))
}

/// The SHA-256 of a secret, which is all the database keeps of it.
pub(crate) fn hash_secret(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    out
}

/// Whether `err` is a unique constraint violation.
pub(crate) fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.is_unique_violation())
}

/// A person's role in one project (`memberships.role`), lowest rights
/// first. Serializes as `member`, `coordinator` or `admin`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Role {
    /// Can mint agents.
    Member,
    /// Can also mint coordinator identities and invite members.
    Coordinator,
    /// Can also invite coordinators and admins.
    Admin,
}

impl Role {
    /// The name `memberships.role` stores.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Coordinator => "coordinator",
            Self::Admin => "admin",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A person's role in a tenant (`tenant_members.role`), lowest rights first.
///
/// ```
/// use agmen_hub::TenantRole;
/// assert!(TenantRole::Owner > TenantRole::Admin);
/// assert_eq!("member".parse::<TenantRole>()?, TenantRole::Member);
/// # Ok::<(), agmen_hub::UnknownRole>(())
/// ```
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum TenantRole {
    /// Belongs to the group.
    Member,
    /// Can administer the group and its projects.
    Admin,
    /// Also holds the group's ownership.
    Owner,
}

impl TenantRole {
    /// The name `tenant_members.role` stores.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Admin => "admin",
            Self::Owner => "owner",
        }
    }
}

impl fmt::Display for TenantRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TenantRole {
    type Err = UnknownRole;

    fn from_str(s: &str) -> Result<Self, UnknownRole> {
        match s {
            "member" => Ok(Self::Member),
            "admin" => Ok(Self::Admin),
            "owner" => Ok(Self::Owner),
            other => Err(UnknownRole(other.to_owned())),
        }
    }
}

/// A text that names no [`Role`] or [`TenantRole`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown role {0:?}")]
pub struct UnknownRole(String);

impl FromStr for Role {
    type Err = UnknownRole;

    fn from_str(s: &str) -> Result<Self, UnknownRole> {
        match s {
            "member" => Ok(Self::Member),
            "coordinator" => Ok(Self::Coordinator),
            "admin" => Ok(Self::Admin),
            other => Err(UnknownRole(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_fit_the_protocol_alphabet() {
        let id = new_id().unwrap();
        assert_eq!(id.len(), ID_LEN);
        assert!(id.parse::<agmen_proto::IdentityId>().is_ok(), "{id}");
        assert_ne!(new_id().unwrap(), id);
    }

    #[test]
    fn secrets_are_64_hex_digits_and_hash_to_32_bytes() {
        let secret = new_secret().unwrap();
        assert_eq!(secret.len(), 64);
        assert!(secret.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(hash_secret(&secret).len(), 32);
        assert_eq!(hash_secret(&secret), hash_secret(&secret));
    }

    #[test]
    fn roles_are_ordered_by_rights() {
        assert!(Role::Admin > Role::Coordinator && Role::Coordinator > Role::Member);
        for role in [Role::Member, Role::Coordinator, Role::Admin] {
            assert_eq!(role.as_str().parse::<Role>().unwrap(), role);
        }
    }

    #[test]
    fn tenant_roles_are_ordered_by_rights() {
        assert!(TenantRole::Owner > TenantRole::Admin && TenantRole::Admin > TenantRole::Member);
        for role in [TenantRole::Member, TenantRole::Admin, TenantRole::Owner] {
            assert_eq!(role.as_str().parse::<TenantRole>().unwrap(), role);
        }
        assert!("coordinator".parse::<TenantRole>().is_err());
    }
}
