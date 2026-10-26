//! Broker tokens: the ES256 JWTs EMQX verifies, and the JWKS it verifies
//! them with (ADR-0002).

use std::fmt;
use std::path::Path;
use std::time::Duration;

use agmen_proto::{IdentityId, Topics};
use jsonwebtoken::jwk::{Jwk, JwkSet, PublicKeyUse};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Serialize;

use crate::identities::Identity;

/// The hub's MQTT username and `sub`. The `-` keeps it outside the identity
/// id alphabet, so no identity can collide with it.
pub(crate) const HUB_USERNAME: &str = "agmen-hub";

/// Why a key could not be loaded or a token signed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TokenError {
    /// The key file could not be read.
    #[error("reading key file {path}")]
    ReadKey {
        /// The file.
        path: String,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A key file's name gives no key id.
    #[error("key file {0} has no usable name for a key id")]
    NoKeyId(String),
    /// The PEM is not a P-256 private key in PKCS#8 form.
    #[error("key {kid} is not an ES256 private key")]
    BadKey {
        /// The key id.
        kid: String,
        /// The parse error.
        #[source]
        source: jsonwebtoken::errors::Error,
    },
    /// Two keys share a key id.
    #[error("two keys have the key id {0}")]
    DuplicateKid(String),
    /// The TTL is too long to express as `exp`.
    #[error("token TTL {0:?} is out of range")]
    TtlOutOfRange(Duration),
    /// Signing failed.
    #[error("signing a broker token failed")]
    Sign(#[source] jsonwebtoken::errors::Error),
}

/// One ES256 private key with its key id.
#[derive(Clone)]
pub struct SigningKey {
    kid: String,
    encoding: EncodingKey,
    jwk: Jwk,
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigningKey")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl SigningKey {
    /// Parses a P-256 private key in PKCS#8 PEM form.
    ///
    /// # Errors
    ///
    /// [`TokenError::BadKey`] when `pem` is not such a key.
    pub fn from_pem(kid: impl Into<String>, pem: &[u8]) -> Result<Self, TokenError> {
        let kid = kid.into();
        let bad = |source| TokenError::BadKey {
            kid: kid.clone(),
            source,
        };
        let encoding = EncodingKey::from_ec_pem(pem).map_err(bad)?;
        let mut jwk = Jwk::from_encoding_key(&encoding, Algorithm::ES256).map_err(bad)?;
        jwk.common.key_id = Some(kid.clone());
        jwk.common.public_key_use = Some(PublicKeyUse::Signature);
        Ok(Self { kid, encoding, jwk })
    }

    /// Reads a key file; the key id is the file name without `.pem`
    /// (`agmen-dev-1.pem` has key id `agmen-dev-1`).
    ///
    /// # Errors
    ///
    /// [`TokenError::ReadKey`], [`TokenError::NoKeyId`] or
    /// [`TokenError::BadKey`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> Result<(), agmen_hub::TokenError> {
    /// let key = agmen_hub::SigningKey::from_pem_file("/keys/hub-2.pem")?;
    /// assert_eq!(key.kid(), "hub-2");
    /// # Ok(()) }
    /// ```
    pub fn from_pem_file(path: impl AsRef<Path>) -> Result<Self, TokenError> {
        let path = path.as_ref();
        let shown = path.display().to_string();
        let kid = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .ok_or_else(|| TokenError::NoKeyId(shown.clone()))?
            .to_owned();
        let pem = std::fs::read(path).map_err(|source| TokenError::ReadKey {
            path: shown,
            source,
        })?;
        Self::from_pem(kid, &pem)
    }

    /// The key id, sent as the token header's `kid`.
    #[must_use]
    pub fn kid(&self) -> &str {
        &self.kid
    }
}

/// One rule of the `acl` claim (ADR-0002).
#[derive(Debug, Serialize)]
struct AclRule {
    permission: &'static str,
    action: &'static str,
    topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    qos: Option<[u8; 1]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retain: Option<bool>,
}

impl AclRule {
    fn new(permission: &'static str, action: &'static str, topic: String) -> Self {
        Self {
            permission,
            action,
            topic,
            qos: None,
            retain: None,
        }
    }

    /// The rule every `acl` ends with, even though `no_match = deny` would
    /// deny anyway.
    fn deny_all() -> Self {
        Self::new("deny", "all", "#".to_owned())
    }
}

/// The claims EMQX reads. Exactly these four: never `is_superuser`,
/// `clientid_override` or `client_attrs`, which EMQX would honor.
#[derive(Debug, Serialize)]
struct Claims<'a> {
    sub: &'a str,
    username: &'a str,
    exp: i64,
    acl: Vec<AclRule>,
}

/// `topic` with its last level replaced by `wildcard`.
fn parent_with(topic: &str, wildcard: &str) -> String {
    match topic.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{wildcard}"),
        None => wildcard.to_owned(),
    }
}

/// An identity's rules, every topic written in full so an id can never
/// widen one.
fn identity_acl(topics: &Topics, id: &IdentityId) -> Vec<AclRule> {
    let eq = |topic: String| format!("eq {topic}");
    vec![
        AclRule {
            qos: Some([1]),
            ..AclRule::new("allow", "publish", eq(topics.up(id)))
        },
        AclRule {
            retain: Some(true),
            ..AclRule::new("allow", "publish", eq(topics.presence(id)))
        },
        AclRule::new("allow", "subscribe", eq(topics.feed())),
        AclRule::new("allow", "subscribe", eq(topics.inbox(id))),
        AclRule::new("allow", "subscribe", eq(topics.ack(id))),
        AclRule::new("allow", "subscribe", parent_with(&topics.state_head(), "#")),
        AclRule::new("allow", "subscribe", parent_with(&topics.presence(id), "+")),
        AclRule::deny_all(),
    ]
}

/// The hub's rules: read every `up/`, publish anywhere else under
/// `agmen/v1/`.
fn hub_acl() -> Vec<AclRule> {
    vec![
        AclRule::new("allow", "subscribe", "eq agmen/v1/+/+/up/+".to_owned()),
        AclRule::new("deny", "publish", "agmen/v1/+/+/up/#".to_owned()),
        AclRule::new("allow", "publish", "agmen/v1/#".to_owned()),
        AclRule::deny_all(),
    ]
}

/// Signs broker tokens with one key and publishes the JWKS: that key plus
/// any extra keys, so EMQX keeps accepting tokens across a rotation.
#[derive(Debug, Clone)]
pub struct BrokerTokens {
    signing: SigningKey,
    extra: Vec<SigningKey>,
}

impl BrokerTokens {
    /// Signs with `signing`; `extra` keys are published but never sign.
    ///
    /// # Errors
    ///
    /// [`TokenError::DuplicateKid`] when two keys share a key id.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> Result<(), agmen_hub::TokenError> {
    /// use agmen_hub::{BrokerTokens, SigningKey};
    /// let tokens = BrokerTokens::new(
    ///     SigningKey::from_pem_file("/keys/hub-2.pem")?,
    ///     vec![SigningKey::from_pem_file("/keys/hub-1.pem")?],
    /// )?;
    /// assert_eq!(tokens.jwks().keys.len(), 2);
    /// # Ok(()) }
    /// ```
    pub fn new(signing: SigningKey, extra: Vec<SigningKey>) -> Result<Self, TokenError> {
        let mut seen = vec![signing.kid.as_str()];
        for key in &extra {
            if seen.contains(&key.kid.as_str()) {
                return Err(TokenError::DuplicateKid(key.kid.clone()));
            }
            seen.push(&key.kid);
        }
        Ok(Self { signing, extra })
    }

    /// The key id new tokens carry.
    #[must_use]
    pub fn signing_kid(&self) -> &str {
        &self.signing.kid
    }

    /// The public keys, signing key first.
    #[must_use]
    pub fn jwks(&self) -> JwkSet {
        JwkSet {
            keys: std::iter::once(&self.signing)
                .chain(&self.extra)
                .map(|key| key.jwk.clone())
                .collect(),
        }
    }

    /// Signs `identity`'s token, valid for `ttl` from now. The client
    /// connects with username = the identity id and password = the token.
    ///
    /// # Errors
    ///
    /// [`TokenError::TtlOutOfRange`] or [`TokenError::Sign`].
    pub fn mint(&self, identity: &Identity, ttl: Duration) -> Result<String, TokenError> {
        let topics = Topics::new(identity.tenant.clone(), identity.project.clone());
        self.sign(&Claims {
            sub: identity.id.as_str(),
            username: identity.id.as_str(),
            exp: expiry(ttl)?,
            acl: identity_acl(&topics, &identity.id),
        })
    }

    /// Signs the hub's own token (username `agmen-hub`, client id
    /// `agmen-hub-<instance>`), valid for `ttl` from now.
    ///
    /// # Errors
    ///
    /// [`TokenError::TtlOutOfRange`] or [`TokenError::Sign`].
    pub fn mint_hub(&self, ttl: Duration) -> Result<String, TokenError> {
        self.sign(&Claims {
            sub: HUB_USERNAME,
            username: HUB_USERNAME,
            exp: expiry(ttl)?,
            acl: hub_acl(),
        })
    }

    fn sign(&self, claims: &Claims<'_>) -> Result<String, TokenError> {
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.signing.kid.clone());
        jsonwebtoken::encode(&header, claims, &self.signing.encoding).map_err(TokenError::Sign)
    }
}

/// `exp` for a token minted now: whole seconds, rounded down, so a caller
/// that reads the clock before minting never reports a later expiry than
/// the token carries.
fn expiry(ttl: Duration) -> Result<i64, TokenError> {
    let ttl_secs = i64::try_from(ttl.as_secs()).map_err(|_| TokenError::TtlOutOfRange(ttl))?;
    jiff::Timestamp::now()
        .as_second()
        .checked_add(ttl_secs)
        .ok_or(TokenError::TtlOutOfRange(ttl))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_acl_matches_adr_0002() {
        let topics = Topics::new("tenant01".parse().unwrap(), "project1".parse().unwrap());
        let id: IdentityId = "abcdefgh".parse().unwrap();
        let acl = serde_json::to_value(identity_acl(&topics, &id)).unwrap();
        let b = "agmen/v1/tenant01/project1";
        assert_eq!(
            acl,
            serde_json::json!([
                { "permission": "allow", "action": "publish",   "topic": format!("eq {b}/up/abcdefgh"), "qos": [1] },
                { "permission": "allow", "action": "publish",   "topic": format!("eq {b}/presence/abcdefgh"), "retain": true },
                { "permission": "allow", "action": "subscribe", "topic": format!("eq {b}/feed") },
                { "permission": "allow", "action": "subscribe", "topic": format!("eq {b}/inbox/abcdefgh") },
                { "permission": "allow", "action": "subscribe", "topic": format!("eq {b}/ack/abcdefgh") },
                { "permission": "allow", "action": "subscribe", "topic": format!("{b}/state/#") },
                { "permission": "allow", "action": "subscribe", "topic": format!("{b}/presence/+") },
                { "permission": "deny",  "action": "all",       "topic": "#" }
            ])
        );
    }

    #[test]
    fn hub_acl_ends_with_deny_all() {
        let acl = serde_json::to_value(hub_acl()).unwrap();
        assert_eq!(
            acl.as_array().unwrap().last().unwrap(),
            &serde_json::json!({ "permission": "deny", "action": "all", "topic": "#" })
        );
    }

    #[test]
    fn huge_ttl_is_an_error() {
        assert!(matches!(
            expiry(Duration::MAX),
            Err(TokenError::TtlOutOfRange(_))
        ));
    }
}
