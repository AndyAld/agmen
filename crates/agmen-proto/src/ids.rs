//! Identifiers and validated strings: ids, item keys, lane and role names,
//! bodies, mentions and stream names.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use ts_rs::TS;

use crate::error::ProtoError;

/// A point in time, serialized as RFC 3339 in UTC (`2026-10-25T16:04:11.123Z`).
pub type Timestamp = jiff::Timestamp;

/// The largest body, in bytes after UTF-8 encoding (protocol section 8).
pub const MAX_BODY_BYTES: usize = 32_768;

/// Declares a `String` newtype that only holds values `$check` accepts.
///
/// The type parses with `FromStr` and `TryFrom<String>`, serializes as a JSON
/// string, and describes itself to schemars with `$pattern` and to ts-rs as
/// `string`.
macro_rules! checked_string {
    ($(#[$meta:meta])* $name:ident, $what:literal, $pattern:literal, $check:path) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
        pub struct $name(String);

        impl $name {
            /// The value as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = ProtoError;

            fn try_from(value: String) -> Result<Self, ProtoError> {
                if $check(&value) {
                    Ok(Self(value))
                } else {
                    Err(ProtoError::Malformed {
                        what: $what,
                        rule: $pattern,
                    })
                }
            }
        }

        impl FromStr for $name {
            type Err = ProtoError;

            fn from_str(s: &str) -> Result<Self, ProtoError> {
                Self::try_from(s.to_owned())
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::try_from(String::deserialize(deserializer)?).map_err(de::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({ "type": "string", "pattern": $pattern })
            }
        }
    };
}

checked_string!(
    /// A tenant id, minted by the hub.
    TenantId,
    "tenant id",
    "^[a-z0-9]{8,32}$",
    is_hub_id
);

checked_string!(
    /// A project id, minted by the hub.
    ProjectId,
    "project id",
    "^[a-z0-9]{8,32}$",
    is_hub_id
);

checked_string!(
    /// An identity id, minted by the hub. Never holds `/`, `+` or `#`, so it is
    /// safe as an MQTT topic level.
    IdentityId,
    "identity id",
    "^[a-z0-9]{8,32}$",
    is_hub_id
);

checked_string!(
    /// A message id: a ULID the client generates, in its canonical 26-character
    /// uppercase form. With the sender it is the hub's dedupe key.
    MessageId,
    "message id",
    "^[0-7][0-9A-HJKMNP-TV-Z]{25}$",
    is_ulid
);

checked_string!(
    /// An item key such as `B-412`.
    ItemKey,
    "item key",
    "^[A-Z]{1,4}-[0-9]{1,6}$",
    is_item_key
);

checked_string!(
    /// A lane name such as `L-UI`.
    LaneName,
    "lane name",
    "^L-[A-Z0-9-]{1,40}$",
    is_lane_name
);

checked_string!(
    /// A role name such as `reviewer`.
    RoleName,
    "role name",
    "^[a-z][a-z0-9-]{1,31}$",
    is_role_name
);

checked_string!(
    /// A git object id in lowercase hex, abbreviated (7 or more digits) or full
    /// (40 for SHA-1, 64 for SHA-256 repositories).
    GitSha,
    "git sha",
    "^[0-9a-f]{7,64}$",
    is_git_sha
);

checked_string!(
    /// A SHA-256 digest in exactly 64 lowercase hex characters.
    Sha256Hex,
    "sha256 hex",
    "^[0-9a-f]{64}$",
    is_sha256_hex
);

fn is_hub_id(s: &str) -> bool {
    (8..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn is_ulid(s: &str) -> bool {
    const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    // 26 base32 digits hold 130 bits; a first digit over 7 overflows the 128 a ULID has.
    s.len() == 26
        && s.as_bytes()
            .first()
            .is_some_and(|b| (b'0'..=b'7').contains(b))
        && s.bytes().all(|b| CROCKFORD.contains(&b))
}

fn is_item_key(s: &str) -> bool {
    s.split_once('-').is_some_and(|(prefix, number)| {
        (1..=4).contains(&prefix.len())
            && prefix.bytes().all(|b| b.is_ascii_uppercase())
            && (1..=6).contains(&number.len())
            && number.bytes().all(|b| b.is_ascii_digit())
    })
}

fn is_lane_name(s: &str) -> bool {
    s.strip_prefix("L-").is_some_and(|rest| {
        (1..=40).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
    })
}

fn is_role_name(s: &str) -> bool {
    (2..=32).contains(&s.len())
        && s.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn is_git_sha(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A stream's sequence number. The hub numbers each stream from 1 with no gaps.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    Serialize,
    Deserialize,
    JsonSchema,
    TS,
)]
pub struct Seq(pub u64);

impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A message body: markdown, at most 32,768 bytes after UTF-8 encoding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, TS)]
pub struct Body(String);

impl Body {
    /// The body as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Body {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<Body> for String {
    fn from(value: Body) -> Self {
        value.0
    }
}

impl TryFrom<String> for Body {
    type Error = ProtoError;

    /// # Errors
    ///
    /// [`ProtoError::BodyTooLarge`] past [`MAX_BODY_BYTES`].
    fn try_from(value: String) -> Result<Self, ProtoError> {
        if value.len() > MAX_BODY_BYTES {
            return Err(ProtoError::BodyTooLarge { len: value.len() });
        }
        Ok(Self(value))
    }
}

impl TryFrom<&str> for Body {
    type Error = ProtoError;

    /// # Errors
    ///
    /// [`ProtoError::BodyTooLarge`] past [`MAX_BODY_BYTES`].
    fn try_from(value: &str) -> Result<Self, ProtoError> {
        Self::try_from(value.to_owned())
    }
}

impl Serialize for Body {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Body {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for Body {
    fn schema_name() -> Cow<'static, str> {
        "Body".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Markdown, at most 32768 bytes after UTF-8 encoding."
        })
    }
}

/// Who a message mentions (protocol section 3). On the wire it is a plain
/// string: `all`, `agents`, `humans`, a lane name, `role:<name>`, or an
/// identity id. The forms cannot collide: ids are 8 or more characters of
/// `[a-z0-9]`, lanes start with `L-` and roles with `role:`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
#[ts(type = "string")]
pub enum Mention {
    /// Everyone in the project.
    All,
    /// Every identity that is not a human or the hub.
    Agents,
    /// Every human.
    Humans,
    /// Everyone working in a lane.
    Lane(LaneName),
    /// Every live identity holding a role.
    Role(RoleName),
    /// One identity.
    Identity(IdentityId),
}

const ROLE_PREFIX: &str = "role:";

impl fmt::Display for Mention {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All => f.write_str("all"),
            Self::Agents => f.write_str("agents"),
            Self::Humans => f.write_str("humans"),
            Self::Lane(lane) => lane.fmt(f),
            Self::Role(role) => write!(f, "{ROLE_PREFIX}{role}"),
            Self::Identity(id) => id.fmt(f),
        }
    }
}

impl FromStr for Mention {
    type Err = ProtoError;

    fn from_str(s: &str) -> Result<Self, ProtoError> {
        let mention = match s {
            "all" => Self::All,
            "agents" => Self::Agents,
            "humans" => Self::Humans,
            _ => {
                if let Some(role) = s.strip_prefix(ROLE_PREFIX) {
                    Self::Role(role.parse()?)
                } else if s.starts_with("L-") {
                    Self::Lane(s.parse()?)
                } else {
                    Self::Identity(s.parse()?)
                }
            }
        };
        Ok(mention)
    }
}

/// A stream the hub numbers: the project feed or one identity's inbox.
/// On the wire it is `"feed"` or `"inbox:<identity_id>"`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, TS)]
#[ts(type = "string")]
pub enum Stream {
    /// The project-wide feed.
    Feed,
    /// One identity's inbox.
    Inbox(IdentityId),
}

const INBOX_PREFIX: &str = "inbox:";

impl fmt::Display for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Feed => f.write_str("feed"),
            Self::Inbox(id) => write!(f, "{INBOX_PREFIX}{id}"),
        }
    }
}

impl FromStr for Stream {
    type Err = ProtoError;

    fn from_str(s: &str) -> Result<Self, ProtoError> {
        if s == "feed" {
            return Ok(Self::Feed);
        }
        s.strip_prefix(INBOX_PREFIX)
            .ok_or(ProtoError::Malformed {
                what: "stream",
                rule: "feed or inbox:<identity id>",
            })?
            .parse()
            .map(Self::Inbox)
    }
}

/// Serializes `Mention` and `Stream` through their string forms.
macro_rules! string_form_serde {
    ($name:ident, $description:literal) => {
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(de::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({ "type": "string", "description": $description })
            }
        }
    };
}

string_form_serde!(
    Mention,
    "all, agents, humans, a lane name (L-UI), role:<name>, or an identity id."
);
string_form_serde!(Stream, "feed, or inbox:<identity id>.");

#[cfg(test)]
#[path = "ids_tests.rs"]
mod tests;
