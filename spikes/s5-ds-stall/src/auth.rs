//! Ids and broker tokens in the shapes the hub mints (ADR-0002), signed
//! with the dev key from `AGMEN_IT_JWT_DIR`.

use std::hash::{BuildHasher, Hasher, RandomState};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};

/// Key id of the dev signing key; `deploy/dev/init.sh` writes `<kid>.pem`.
const DEV_KID: &str = "agmen-dev-1";
/// The hub's MQTT username and `sub` (ADR-0002).
pub(crate) const HUB_USER: &str = "agmen-hub";
/// How long a token lasts; a run is far shorter.
const TOKEN_TTL_SECS: u64 = 3600;

/// A fresh 16-character `[a-z0-9]` id, as the hub's tests mint for
/// tenants, projects and identities.
pub(crate) fn new_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    // RandomState is seeded randomly per process and per instance.
    let state = RandomState::new();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    (0..16_u64)
        .map(|position| {
            let mut hasher = state.build_hasher();
            hasher.write_u64(n);
            hasher.write_u64(position);
            // The remainder is below 36, so the index is in range.
            char::from(ALPHABET[(hasher.finish() % 36) as usize])
        })
        .collect()
}

/// Signs broker tokens with the dev key.
#[derive(Debug)]
pub(crate) struct Signer {
    key: EncodingKey,
}

impl Signer {
    /// Reads the dev key from `AGMEN_IT_JWT_DIR`.
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        let dir = std::env::var("AGMEN_IT_JWT_DIR").context(
            "AGMEN_IT_JWT_DIR is unset: `set -a; . deploy/dev/.secrets/dev.env; set +a`",
        )?;
        let path = PathBuf::from(dir).join(format!("{DEV_KID}.pem"));
        let pem = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let key = EncodingKey::from_ec_pem(&pem).context("parsing the dev signing key")?;
        Ok(Self { key })
    }

    /// The identity token of ADR-0002 for `id`, trimmed to what a watch
    /// session subscribes to.
    pub(crate) fn identity(&self, prefix: &str, id: &str) -> anyhow::Result<String> {
        self.sign(&json!({
            "sub": id, "username": id, "exp": exp()?,
            "acl": [
                { "permission": "allow", "action": "subscribe", "topic": format!("eq {prefix}/feed") },
                { "permission": "allow", "action": "subscribe", "topic": format!("eq {prefix}/inbox/{id}") },
                { "permission": "allow", "action": "subscribe", "topic": format!("eq {prefix}/ack/{id}") },
                { "permission": "allow", "action": "subscribe", "topic": format!("{prefix}/state/#") },
                { "permission": "deny",  "action": "all",       "topic": "#" }
            ]
        }))
    }

    /// The hub token of ADR-0002.
    pub(crate) fn hub(&self) -> anyhow::Result<String> {
        self.sign(&json!({
            "sub": HUB_USER, "username": HUB_USER, "exp": exp()?,
            "acl": [
                { "permission": "allow", "action": "subscribe", "topic": "eq agmen/v1/+/+/up/+" },
                { "permission": "deny",  "action": "publish",   "topic": "agmen/v1/+/+/up/#" },
                { "permission": "allow", "action": "publish",   "topic": "agmen/v1/#" },
                { "permission": "deny",  "action": "all",       "topic": "#" }
            ]
        }))
    }

    fn sign(&self, claims: &Value) -> anyhow::Result<String> {
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(DEV_KID.to_owned());
        jsonwebtoken::encode(&header, claims, &self.key).context("signing a broker token")
    }
}

fn exp() -> anyhow::Result<u64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("the clock is before 1970")?;
    Ok(now.as_secs() + TOKEN_TTL_SECS)
}
