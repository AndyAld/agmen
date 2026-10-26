//! Hub configuration, read from `AGMEN_*` environment variables.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use url::Url;

/// A secret value: a password, client secret or a URL that embeds one. Its
/// `Debug` output is redacted, so a logged config never shows it.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct Secret(String);

impl Secret {
    /// Wraps `value`.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret itself, for the one call that needs it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// The GitHub OAuth app the approve page signs people in with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubConfig {
    /// `AGMEN_GITHUB_CLIENT_ID`.
    pub client_id: String,
    /// `AGMEN_GITHUB_CLIENT_SECRET`.
    pub client_secret: Secret,
    /// `AGMEN_GITHUB_URL`, default `https://github.com`: where
    /// `/login/oauth/authorize` and `/login/oauth/access_token` live.
    pub url: Url,
    /// `AGMEN_GITHUB_API_URL`, default `https://api.github.com`: where
    /// `/user` lives.
    pub api_url: Url,
}

/// `AGMEN_GITHUB_API_URL`'s default.
const DEFAULT_GITHUB_API_URL: &str = "https://api.github.com";

impl GithubConfig {
    /// Reads only `AGMEN_GITHUB_API_URL` (default `https://api.github.com`)
    /// from the process environment, for `agmen-hub bootstrap`, which needs
    /// GitHub's users API and nothing else of GitHub.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Invalid`] when the value is not an http or https URL.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> Result<(), agmen_hub::ConfigError> {
    /// let api = agmen_hub::GithubConfig::api_url_from_env()?;
    /// println!("{api}");
    /// # Ok(()) }
    /// ```
    pub fn api_url_from_env() -> Result<Url, ConfigError> {
        github_api_url(std::env::var("AGMEN_GITHUB_API_URL").ok())
    }
}

/// `value` as an http or https URL, or [`ConfigError::Invalid`] naming
/// variable `name`.
fn http_url(name: &'static str, value: &str) -> Result<Url, ConfigError> {
    Url::parse(value)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .ok_or(ConfigError::Invalid {
            name,
            what: "http or https URL",
        })
}

/// `AGMEN_GITHUB_API_URL`'s value (unset or empty means the default).
fn github_api_url(value: Option<String>) -> Result<Url, ConfigError> {
    let value = value.filter(|value| !value.is_empty());
    http_url(
        "AGMEN_GITHUB_API_URL",
        value.as_deref().unwrap_or(DEFAULT_GITHUB_API_URL),
    )
}

/// The EMQX REST API the hub kicks clients through (ADR-0002).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmqxConfig {
    /// `AGMEN_EMQX_API_URL`, the API base such as `http://emqx:18083/api/v5`.
    pub api_url: Url,
    /// `AGMEN_EMQX_API_KEY`.
    pub api_key: String,
    /// `AGMEN_EMQX_API_SECRET`.
    pub api_secret: Secret,
}

/// The broker connection the hub ingests and publishes through
/// (ADR-0002: username `agmen-hub`, client id `agmen-hub-<instance>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MqttConfig {
    /// Host from `AGMEN_MQTT_URL` (`mqtt://host:port`).
    pub host: String,
    /// Port from `AGMEN_MQTT_URL`.
    pub port: u16,
    /// `AGMEN_HUB_INSTANCE`, default `main`: `[a-z0-9]{1,32}`, unique per
    /// hub replica, so the client id is `agmen-hub-<instance>`.
    pub instance: String,
    /// `AGMEN_MQTT_SHARE_GROUP`, default `hub`: `[a-z0-9]{1,32}`, the shared
    /// subscription group, `$share/<group>/agmen/v1/+/+/up/+`. Replicas
    /// share one group; tests give each hub its own.
    pub share_group: String,
}

impl MqttConfig {
    /// The broker client id, `agmen-hub-<instance>`.
    #[must_use]
    pub fn client_id(&self) -> String {
        format!("agmen-hub-{}", self.instance)
    }

    /// The shared subscription to every project's `up/` topics.
    #[must_use]
    pub fn up_filter(&self) -> String {
        format!("$share/{}/agmen/v1/+/+/up/+", self.share_group)
    }
}

/// Whether `value` is 1 to 32 of `[a-z0-9]`: safe in a client id and as a
/// share group name.
fn is_short_name(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// `AGMEN_MQTT_URL`'s host and port.
fn mqtt_addr(value: &str) -> Result<(String, u16), ConfigError> {
    let invalid = || ConfigError::Invalid {
        name: "AGMEN_MQTT_URL",
        what: "mqtt://host:port URL",
    };
    let url = Url::parse(value).map_err(|_| invalid())?;
    match (url.scheme(), url.host_str(), url.port(), url.path()) {
        ("mqtt", Some(host), Some(port), "" | "/") if !host.is_empty() => {
            Ok((host.to_owned(), port))
        }
        _ => Err(invalid()),
    }
}

/// How long a claim lasts without a HEARTBEAT (protocol section 8).
pub const DEFAULT_CLAIM_LEASE: Duration = Duration::from_secs(30 * 60);

/// The longest claim lease `AGMEN_CLAIM_LEASE_SECS` may set: one day.
const MAX_CLAIM_LEASE_SECS: u64 = 24 * 3600;

/// Everything the hub needs to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `AGMEN_DATABASE_URL`, a Postgres URL.
    pub database_url: Secret,
    /// `AGMEN_LISTEN`, default `0.0.0.0:8080`.
    pub listen: SocketAddr,
    /// `AGMEN_PUBLIC_URL`: how browsers and the CLI reach the hub. Device
    /// login links and the OAuth redirect are built from it.
    pub public_url: Url,
    /// GitHub sign-in.
    pub github: GithubConfig,
    /// EMQX REST API.
    pub emqx: EmqxConfig,
    /// The broker connection for ingest and the sweeper.
    pub mqtt: MqttConfig,
    /// `AGMEN_JWT_SIGNING_KEY`: the ES256 private key (PKCS#8 PEM) that
    /// signs broker tokens. Its key id is the file name without `.pem`.
    pub jwt_signing_key: PathBuf,
    /// `AGMEN_JWT_EXTRA_KEYS`, comma separated, optional: more private keys
    /// published in `/v1/jwks` but never used to sign. During a rotation
    /// this holds the next key (published at least 60 s before the switch)
    /// or the previous one (kept until its last token has expired).
    pub jwt_extra_keys: Vec<PathBuf>,
    /// `AGMEN_CLAIM_LEASE_SECS`, default 1800 (30 minutes), at most a day:
    /// how long a granted claim lasts unless a HEARTBEAT renews it.
    pub claim_lease: Duration,
}

/// Why the configuration could not be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// A required variable is unset or empty.
    #[error("{0} is not set")]
    Missing(&'static str),
    /// A variable is set but does not parse.
    #[error("{name} is not a valid {what}")]
    Invalid {
        /// The variable.
        name: &'static str,
        /// What it should hold.
        what: &'static str,
    },
}

impl Config {
    /// Reads the configuration from the process environment.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] naming the first variable that is missing or invalid.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads the configuration through `lookup`, which returns a variable's
    /// value or `None` when it is unset.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] naming the first variable that is missing or invalid.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::collections::HashMap;
    /// let vars = HashMap::from([
    ///     ("AGMEN_DATABASE_URL", "postgres://localhost/agmen"),
    ///     ("AGMEN_PUBLIC_URL", "https://api.example.com"),
    ///     ("AGMEN_GITHUB_CLIENT_ID", "id"),
    ///     ("AGMEN_GITHUB_CLIENT_SECRET", "secret"),
    ///     ("AGMEN_EMQX_API_URL", "http://emqx:18083/api/v5"),
    ///     ("AGMEN_EMQX_API_KEY", "agmen-hub"),
    ///     ("AGMEN_EMQX_API_SECRET", "secret"),
    ///     ("AGMEN_MQTT_URL", "mqtt://emqx:1883"),
    ///     ("AGMEN_JWT_SIGNING_KEY", "/keys/hub-2.pem"),
    ///     ("AGMEN_JWT_EXTRA_KEYS", "/keys/hub-1.pem"),
    /// ]);
    /// let config = agmen_hub::Config::from_lookup(|name| vars.get(name).map(|v| v.to_string()))?;
    /// assert_eq!(config.listen.port(), 8080);
    /// assert_eq!(config.github.api_url.as_str(), "https://api.github.com/");
    /// assert_eq!(config.jwt_extra_keys.len(), 1);
    /// assert_eq!(config.mqtt.client_id(), "agmen-hub-main");
    /// assert_eq!(config.mqtt.up_filter(), "$share/hub/agmen/v1/+/+/up/+");
    /// assert_eq!(config.claim_lease, agmen_hub::DEFAULT_CLAIM_LEASE);
    /// # Ok(()) }
    /// ```
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |name: &'static str| lookup(name).filter(|value| !value.is_empty());
        let required = |name: &'static str| get(name).ok_or(ConfigError::Missing(name));
        let url = |name: &'static str, value: String| http_url(name, &value);
        let listen = match get("AGMEN_LISTEN") {
            Some(value) => value.parse().map_err(|_| ConfigError::Invalid {
                name: "AGMEN_LISTEN",
                what: "socket address",
            })?,
            None => SocketAddr::from(([0, 0, 0, 0], 8080)),
        };
        Ok(Self {
            database_url: Secret::new(required("AGMEN_DATABASE_URL")?),
            listen,
            public_url: url("AGMEN_PUBLIC_URL", required("AGMEN_PUBLIC_URL")?)?,
            github: GithubConfig {
                client_id: required("AGMEN_GITHUB_CLIENT_ID")?,
                client_secret: Secret::new(required("AGMEN_GITHUB_CLIENT_SECRET")?),
                url: url(
                    "AGMEN_GITHUB_URL",
                    get("AGMEN_GITHUB_URL").unwrap_or_else(|| "https://github.com".to_owned()),
                )?,
                api_url: github_api_url(get("AGMEN_GITHUB_API_URL"))?,
            },
            emqx: EmqxConfig {
                api_url: url("AGMEN_EMQX_API_URL", required("AGMEN_EMQX_API_URL")?)?,
                api_key: required("AGMEN_EMQX_API_KEY")?,
                api_secret: Secret::new(required("AGMEN_EMQX_API_SECRET")?),
            },
            mqtt: {
                let (host, port) = mqtt_addr(&required("AGMEN_MQTT_URL")?)?;
                let name = |name: &'static str, default: &str| {
                    let value = get(name).unwrap_or_else(|| default.to_owned());
                    if is_short_name(&value) {
                        Ok(value)
                    } else {
                        Err(ConfigError::Invalid {
                            name,
                            what: "name of 1 to 32 of [a-z0-9]",
                        })
                    }
                };
                MqttConfig {
                    host,
                    port,
                    instance: name("AGMEN_HUB_INSTANCE", "main")?,
                    share_group: name("AGMEN_MQTT_SHARE_GROUP", "hub")?,
                }
            },
            jwt_signing_key: required("AGMEN_JWT_SIGNING_KEY")?.into(),
            jwt_extra_keys: get("AGMEN_JWT_EXTRA_KEYS")
                .map(|list| {
                    list.split(',')
                        .map(str::trim)
                        .filter(|path| !path.is_empty())
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default(),
            claim_lease: match get("AGMEN_CLAIM_LEASE_SECS") {
                Some(value) => value
                    .parse::<u64>()
                    .ok()
                    .filter(|secs| (1..=MAX_CLAIM_LEASE_SECS).contains(secs))
                    .map(Duration::from_secs)
                    .ok_or(ConfigError::Invalid {
                        name: "AGMEN_CLAIM_LEASE_SECS",
                        what: "number of seconds from 1 to 86400",
                    })?,
                None => DEFAULT_CLAIM_LEASE,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_variable_is_named() {
        let err = Config::from_lookup(|_| None).unwrap_err();
        assert!(
            matches!(err, ConfigError::Missing("AGMEN_DATABASE_URL")),
            "{err}"
        );
    }

    #[test]
    fn urls_must_be_http() {
        let err = Config::from_lookup(|name| match name {
            "AGMEN_DATABASE_URL" => Some("postgres://localhost/agmen".to_owned()),
            "AGMEN_PUBLIC_URL" => Some("mailto:hub@example.com".to_owned()),
            _ => None,
        })
        .unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::Invalid {
                    name: "AGMEN_PUBLIC_URL",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn mqtt_urls_need_host_and_port() {
        assert_eq!(
            mqtt_addr("mqtt://emqx:1883").unwrap(),
            ("emqx".to_owned(), 1883)
        );
        for bad in [
            "mqtt://emqx",
            "http://emqx:1883",
            "mqtt://emqx:1883/x",
            "emqx:1883",
        ] {
            assert!(mqtt_addr(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn short_names() {
        assert!(is_short_name("main") && is_short_name("a1") && is_short_name(&"x".repeat(32)));
        for bad in ["", "Main", "a-b", "a/b", "+", &"x".repeat(33)] {
            assert!(!is_short_name(bad), "{bad}");
        }
    }

    #[test]
    fn claim_lease_is_bounded() {
        let with_lease = |value: &'static str| {
            Config::from_lookup(|name| match name {
                "AGMEN_CLAIM_LEASE_SECS" => Some(value.to_owned()),
                "AGMEN_DATABASE_URL" => Some("postgres://localhost/agmen".to_owned()),
                "AGMEN_PUBLIC_URL" => Some("https://api.example.com".to_owned()),
                "AGMEN_EMQX_API_URL" => Some("http://emqx:18083/api/v5".to_owned()),
                "AGMEN_MQTT_URL" => Some("mqtt://emqx:1883".to_owned()),
                "AGMEN_JWT_SIGNING_KEY" => Some("/keys/hub-1.pem".to_owned()),
                "AGMEN_GITHUB_CLIENT_ID"
                | "AGMEN_GITHUB_CLIENT_SECRET"
                | "AGMEN_EMQX_API_KEY"
                | "AGMEN_EMQX_API_SECRET" => Some("x".to_owned()),
                _ => None,
            })
        };
        assert_eq!(with_lease("2").unwrap().claim_lease, Duration::from_secs(2));
        for bad in ["0", "-1", "1.5", "86401"] {
            assert!(
                matches!(
                    with_lease(bad),
                    Err(ConfigError::Invalid {
                        name: "AGMEN_CLAIM_LEASE_SECS",
                        ..
                    })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn secrets_are_redacted_in_debug() {
        let secret = Secret::new(format!("pw-{}", "x".repeat(12)));
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    }
}
