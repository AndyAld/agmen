//! Hub configuration, read from `AGMEN_*` environment variables.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use url::Url;

use crate::billing::Catalog;

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

/// Public OAuth app used for self-hosted GitHub device sign-in.
pub const DEFAULT_GITHUB_CLIENT_ID: &str = "agmen-dev-self-host-placeholder";

/// The public GitHub OAuth app used for device sign-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubConfig {
    /// `AGMEN_GITHUB_CLIENT_ID`.
    pub client_id: String,
    /// `AGMEN_GITHUB_URL`, default `https://github.com`: where
    /// `/login/device/code` and `/login/oauth/access_token` live.
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

/// `AGMEN_PADDLE_API_URL`'s default. Sandbox hubs set
/// `https://sandbox-api.paddle.com` instead.
const DEFAULT_PADDLE_API_URL: &str = "https://api.paddle.com";

/// The most days `AGMEN_RETENTION_DAYS` may name.
const MAX_RETENTION_DAYS: u32 = 36_500;

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

/// `value` as an https URL with a host and no credentials, or
/// [`ConfigError::Invalid`] naming variable `name`.
fn https_url(name: &'static str, value: &str) -> Result<Url, ConfigError> {
    Url::parse(value)
        .ok()
        .filter(|url| {
            url.scheme() == "https"
                && url.host_str().is_some_and(|host| !host.is_empty())
                && url.username().is_empty()
                && url.password().is_none()
        })
        .ok_or(ConfigError::Invalid {
            name,
            what: "https URL without credentials",
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
    /// `AGMEN_EMQX_API_KEY`, default `agmen-hub`.
    pub api_key: String,
}

/// Supplied secrets or a directory in which the hub generates and keeps them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubSecrets {
    /// Both `AGMEN_JWT_SIGNING_KEY` and `AGMEN_EMQX_API_SECRET` are supplied.
    Given {
        /// ES256 PKCS#8 PEM file; its stem is the signing key id.
        jwt_signing_key: PathBuf,
        /// Secret for the EMQX REST API key.
        emqx_api_secret: Secret,
    },
    /// Neither secret is supplied; keep generated secrets in `AGMEN_DATA_DIR`.
    Generated {
        /// Contains `jwt/hub-1.pem` and `broker/api-keys.txt`.
        data_dir: PathBuf,
    },
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

/// `value` as the broker URL clients connect to, or
/// [`ConfigError::Invalid`]: `mqtt://host:port`, `mqtts://host:port` or
/// `wss://host[:port]/path`, the forms agmen-client's `Broker` takes.
fn public_broker_url(value: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(value).ok().filter(|url| {
        let host = url.host_str().is_some_and(|host| !host.is_empty());
        match url.scheme() {
            "mqtt" | "mqtts" => host && url.port().is_some(),
            "wss" => host,
            _ => false,
        }
    });
    url.ok_or(ConfigError::Invalid {
        name: "AGMEN_PUBLIC_MQTT_URL",
        what: "mqtt://host:port, mqtts://host:port or wss://host[:port]/path URL",
    })
}

fn mqtt_ws_upstream(value: Option<String>) -> Result<Option<Url>, ConfigError> {
    value
        .map(|value| {
            Url::parse(&value)
                .ok()
                .filter(|url| {
                    url.scheme() == "ws"
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.fragment().is_none()
                })
                .ok_or(ConfigError::Invalid {
                    name: "AGMEN_MQTT_WS_UPSTREAM",
                    what: "ws://host[:port]/path URL without credentials or a fragment",
                })
        })
        .transpose()
}

fn proxy_broker_url(public_url: &Url) -> Result<Url, ConfigError> {
    let invalid = || ConfigError::Invalid {
        name: "AGMEN_PUBLIC_MQTT_URL",
        what: "wss URL; set an https AGMEN_PUBLIC_URL",
    };
    if public_url.scheme() != "https" {
        return Err(invalid());
    }
    let mut broker = crate::http::public_link(public_url, "mqtt");
    broker.set_scheme("wss").map_err(|()| invalid())?;
    Ok(broker)
}

/// How long a claim lasts without a HEARTBEAT (protocol section 8).
pub const DEFAULT_CLAIM_LEASE: Duration = Duration::from_secs(30 * 60);

/// The longest claim lease `AGMEN_CLAIM_LEASE_SECS` may set: one day.
const MAX_CLAIM_LEASE_SECS: u64 = 24 * 3600;

/// Whether the hub serves one self-hosted group or hosted tenants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HubMode {
    /// One group, initialized at startup and claimed by its first owner.
    #[default]
    SelfHost,
    /// Hosted tenants, without self-host initialization or claiming.
    Hosted,
}

/// Parses the `AGMEN_HUB_MODE` values `self-host` and `hosted`.
impl std::str::FromStr for HubMode {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, ConfigError> {
        match value {
            "self-host" => Ok(Self::SelfHost),
            "hosted" => Ok(Self::Hosted),
            _ => Err(ConfigError::Invalid {
                name: "AGMEN_HUB_MODE",
                what: "self-host or hosted",
            }),
        }
    }
}

/// Paddle Billing API access for a hosted hub.
///
/// `api_key` and `webhook_secret` are [`Secret`]s, so [`Config`]'s `Debug`
/// output stays redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaddleConfig {
    /// `AGMEN_PADDLE_API_URL`, default `https://api.paddle.com`.
    /// Sandbox hubs set `https://sandbox-api.paddle.com`. Only https URLs
    /// without credentials are accepted.
    pub api_url: Url,
    /// `AGMEN_PADDLE_API_KEY`.
    pub api_key: Secret,
    /// `AGMEN_PADDLE_WEBHOOK_SECRET`.
    pub webhook_secret: Secret,
}

/// Hosted-mode settings. Present on [`Config`] exactly when [`HubMode`] is
/// [`HubMode::Hosted`], so a self-hosted hub never builds a Paddle client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedConfig {
    /// Paddle API access.
    pub paddle: PaddleConfig,
    /// The five Paddle price ids.
    pub catalog: Catalog,
}

fn hub_mode(get: &impl Fn(&'static str) -> Option<String>) -> Result<HubMode, ConfigError> {
    get("AGMEN_HUB_MODE").map_or(Ok(HubMode::default()), |mode| mode.parse())
}

fn configured_group_name(
    get: &impl Fn(&'static str) -> Option<String>,
) -> Result<String, ConfigError> {
    let name = get("AGMEN_GROUP_NAME").unwrap_or_else(|| "agmen".to_owned());
    if crate::db::is_display_name(&name) {
        Ok(name)
    } else {
        Err(ConfigError::Invalid {
            name: "AGMEN_GROUP_NAME",
            what: "name of 1 to 64 characters without controls or surrounding whitespace",
        })
    }
}

fn hosted_settings(
    get: &impl Fn(&'static str) -> Option<String>,
) -> Result<Option<HostedConfig>, ConfigError> {
    if hub_mode(get)? != HubMode::Hosted {
        return Ok(None);
    }
    let api_url = https_url(
        "AGMEN_PADDLE_API_URL",
        get("AGMEN_PADDLE_API_URL")
            .as_deref()
            .unwrap_or(DEFAULT_PADDLE_API_URL),
    )?;
    let required = |name: &'static str| get(name).ok_or(ConfigError::Missing(name));
    let paddle = PaddleConfig {
        api_url,
        api_key: Secret::new(required("AGMEN_PADDLE_API_KEY")?),
        webhook_secret: Secret::new(required("AGMEN_PADDLE_WEBHOOK_SECRET")?),
    };
    let catalog = Catalog {
        team_base: required("AGMEN_PADDLE_PRICE_TEAM_BASE")?,
        team_seat: required("AGMEN_PADDLE_PRICE_TEAM_SEAT")?,
        business_10: required("AGMEN_PADDLE_PRICE_BUSINESS_10")?,
        business_25: required("AGMEN_PADDLE_PRICE_BUSINESS_25")?,
        business_50: required("AGMEN_PADDLE_PRICE_BUSINESS_50")?,
    };
    let prices = [
        ("AGMEN_PADDLE_PRICE_TEAM_BASE", &catalog.team_base),
        ("AGMEN_PADDLE_PRICE_TEAM_SEAT", &catalog.team_seat),
        ("AGMEN_PADDLE_PRICE_BUSINESS_10", &catalog.business_10),
        ("AGMEN_PADDLE_PRICE_BUSINESS_25", &catalog.business_25),
        ("AGMEN_PADDLE_PRICE_BUSINESS_50", &catalog.business_50),
    ];
    for (index, (first, price)) in prices.iter().enumerate() {
        for (second, other) in prices.iter().skip(index + 1) {
            if price == other {
                return Err(ConfigError::DuplicatePriceIds { first, second });
            }
        }
    }
    Ok(Some(HostedConfig { paddle, catalog }))
}

/// `AGMEN_RETENTION_DAYS` in self-host mode. Hosted mode ignores the variable:
/// the plan decides how long messages are kept.
fn self_host_retention(
    get: &impl Fn(&'static str) -> Option<String>,
) -> Result<Option<u32>, ConfigError> {
    if hub_mode(get)? != HubMode::SelfHost {
        return Ok(None);
    }
    let Some(value) = get("AGMEN_RETENTION_DAYS") else {
        return Ok(None);
    };
    value
        .parse::<u32>()
        .ok()
        .filter(|days| (1..=MAX_RETENTION_DAYS).contains(days))
        .map(Some)
        .ok_or(ConfigError::Invalid {
            name: "AGMEN_RETENTION_DAYS",
            what: "number of days from 1 to 36500, or empty for no limit",
        })
}

/// Everything the hub needs to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `AGMEN_HUB_MODE`, default `self-host`; also accepts `hosted`.
    pub mode: HubMode,
    /// `AGMEN_GROUP_NAME`, default `agmen`: the name of a new self-host group.
    /// Uses 1 to 64 characters, without controls or surrounding whitespace.
    pub group_name: String,
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
    /// `AGMEN_MQTT_WS_UPSTREAM`, optional private `ws://host[:port]/path`
    /// broker listener. Enables `GET /mqtt` on the hub's own origin.
    pub mqtt_ws_upstream: Option<Url>,
    /// Paddle settings. `Some` exactly when `mode` is [`HubMode::Hosted`].
    pub hosted: Option<HostedConfig>,
    /// `AGMEN_RETENTION_DAYS`, self-host only: how many days of message
    /// history to keep, from 1 to 36500. Unset or empty means no limit.
    /// Self-host's compose file passes the variable through even when it is
    /// empty, so an empty value is normal. Ignored in hosted mode, where the
    /// plan's retention decides. Pruning runs at startup and hourly.
    pub retention_days: Option<u32>,
    /// `AGMEN_PUBLIC_MQTT_URL`, default `AGMEN_MQTT_URL`, or the public
    /// hub URL with scheme `wss` and `mqtt` appended when proxying: the broker
    /// address clients connect to, which `GET /v1/me` serves as
    /// `broker_url` (ruling R57). In a cluster it differs from the hub's
    /// own connection, such as `wss://agmen.example.com/mqtt`.
    pub public_mqtt_url: Url,
    /// Supplied signing key and EMQX secret, or generated secrets kept in
    /// `AGMEN_DATA_DIR` when both variables are unset.
    pub secrets: HubSecrets,
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
    /// Two Paddle prices use the same id, so the catalog is ambiguous.
    #[error("{first} and {second} must name distinct Paddle price ids")]
    DuplicatePriceIds {
        /// The first variable.
        first: &'static str,
        /// The other variable with the same id.
        second: &'static str,
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
    ///     ("AGMEN_EMQX_API_URL", "http://emqx:18083/api/v5"),
    ///     ("AGMEN_MQTT_URL", "mqtt://emqx:1883"),
    ///     ("AGMEN_DATA_DIR", "/var/lib/agmen"),
    ///     ("AGMEN_JWT_EXTRA_KEYS", "/keys/hub-2.pem"),
    /// ]);
    /// let config = agmen_hub::Config::from_lookup(|name| vars.get(name).map(|v| v.to_string()))?;
    /// assert_eq!(config.listen.port(), 8080);
    /// assert_eq!(config.emqx.api_key, "agmen-hub");
    /// assert_eq!(config.secrets, agmen_hub::HubSecrets::Generated {
    ///     data_dir: "/var/lib/agmen".into(),
    /// });
    /// assert_eq!(config.github.api_url.as_str(), "https://api.github.com/");
    /// assert_eq!(config.jwt_extra_keys.len(), 1);
    /// assert_eq!(config.mqtt.client_id(), "agmen-hub-main");
    /// assert_eq!(config.mqtt.up_filter(), "$share/hub/agmen/v1/+/+/up/+");
    /// assert_eq!(config.claim_lease, agmen_hub::DEFAULT_CLAIM_LEASE);
    /// assert_eq!(config.public_mqtt_url.as_str(), "mqtt://emqx:1883");
    /// assert_eq!(config.mode, agmen_hub::HubMode::SelfHost);
    /// assert_eq!(config.group_name, "agmen");
    /// assert!(config.hosted.is_none());
    /// assert_eq!(config.retention_days, None);
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
        let mut config = Self {
            database_url: Secret::new(required("AGMEN_DATABASE_URL")?),
            mode: hub_mode(&get)?,
            group_name: configured_group_name(&get)?,
            listen,
            public_url: url("AGMEN_PUBLIC_URL", required("AGMEN_PUBLIC_URL")?)?,
            github: GithubConfig {
                client_id: get("AGMEN_GITHUB_CLIENT_ID")
                    .unwrap_or_else(|| DEFAULT_GITHUB_CLIENT_ID.to_owned()),
                url: url(
                    "AGMEN_GITHUB_URL",
                    get("AGMEN_GITHUB_URL").unwrap_or_else(|| "https://github.com".to_owned()),
                )?,
                api_url: github_api_url(get("AGMEN_GITHUB_API_URL"))?,
            },
            emqx: EmqxConfig {
                api_url: url("AGMEN_EMQX_API_URL", required("AGMEN_EMQX_API_URL")?)?,
                api_key: get("AGMEN_EMQX_API_KEY").unwrap_or_else(|| "agmen-hub".to_owned()),
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
            mqtt_ws_upstream: mqtt_ws_upstream(get("AGMEN_MQTT_WS_UPSTREAM"))?,
            hosted: hosted_settings(&get)?,
            retention_days: self_host_retention(&get)?,
            // After `mqtt`, so a bad AGMEN_MQTT_URL is reported by its name.
            public_mqtt_url: public_broker_url(&match get("AGMEN_PUBLIC_MQTT_URL") {
                Some(value) => value,
                None => required("AGMEN_MQTT_URL")?,
            })?,
            secrets: match (get("AGMEN_JWT_SIGNING_KEY"), get("AGMEN_EMQX_API_SECRET")) {
                (Some(key), Some(secret)) => HubSecrets::Given {
                    jwt_signing_key: key.into(),
                    emqx_api_secret: Secret::new(secret),
                },
                (None, None) => HubSecrets::Generated {
                    data_dir: required("AGMEN_DATA_DIR")?.into(),
                },
                (Some(_), None) => return Err(ConfigError::Missing("AGMEN_EMQX_API_SECRET")),
                (None, Some(_)) => return Err(ConfigError::Missing("AGMEN_JWT_SIGNING_KEY")),
            },
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
        };
        if config.mqtt_ws_upstream.is_some() && get("AGMEN_PUBLIC_MQTT_URL").is_none() {
            config.public_mqtt_url = proxy_broker_url(&config.public_url)?;
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_mqtt_url_follows_the_proxy() {
        let config = |public: &str, upstream: Option<&str>, explicit: Option<&str>| {
            Config::from_lookup(|name| match name {
                "AGMEN_PUBLIC_URL" => Some(public.to_owned()),
                "AGMEN_MQTT_WS_UPSTREAM" => upstream.map(str::to_owned),
                "AGMEN_PUBLIC_MQTT_URL" => explicit.map(str::to_owned),
                _ => without_github(name),
            })
        };
        for (public, expected) in [
            (
                "https://box.tail1234.ts.net",
                "wss://box.tail1234.ts.net/mqtt",
            ),
            ("https://example.com/hub/", "wss://example.com/hub/mqtt"),
            ("https://example.com/hub", "wss://example.com/hub/mqtt"),
            (
                "https://example.com:9443/hub/",
                "wss://example.com:9443/hub/mqtt",
            ),
        ] {
            assert_eq!(
                config(public, Some("ws://emqx:8083/mqtt"), None)
                    .unwrap()
                    .public_mqtt_url
                    .as_str(),
                expected
            );
        }
        assert!(matches!(
            config("http://x", Some("ws://emqx:8083/mqtt"), None),
            Err(ConfigError::Invalid {
                name: "AGMEN_PUBLIC_MQTT_URL",
                what: "wss URL; set an https AGMEN_PUBLIC_URL",
            })
        ));
        assert!(matches!(
            config("https://x", Some("http://emqx:8083/mqtt"), None),
            Err(ConfigError::Invalid {
                name: "AGMEN_MQTT_WS_UPSTREAM",
                ..
            })
        ));
        assert_eq!(
            config(
                "http://x",
                Some("ws://emqx:8083/mqtt"),
                Some("wss://other/mqtt")
            )
            .unwrap()
            .public_mqtt_url
            .as_str(),
            "wss://other/mqtt"
        );
        assert_eq!(
            config("http://x", None, None)
                .unwrap()
                .public_mqtt_url
                .as_str(),
            "mqtt://emqx:1883"
        );
    }

    #[test]
    fn upstream_must_be_ws() {
        for upstream in [
            "wss://emqx:8083/mqtt",
            "http://emqx:8083/mqtt",
            "mqtt://emqx:1883",
            "garbage",
            "ws:///",
            "ws://user:password@emqx/mqtt",
            "ws://emqx/mqtt#fragment",
        ] {
            assert!(
                matches!(
                    Config::from_lookup(|name| match name {
                        "AGMEN_MQTT_WS_UPSTREAM" => Some(upstream.to_owned()),
                        _ => without_github(name),
                    }),
                    Err(ConfigError::Invalid {
                        name: "AGMEN_MQTT_WS_UPSTREAM",
                        ..
                    })
                ),
                "accepted {upstream}"
            );
        }
    }

    #[test]
    fn secrets_come_from_env_or_data_dir() {
        let config = |key: Option<&str>, secret: Option<&str>, data: Option<&str>| {
            Config::from_lookup(|name| match name {
                "AGMEN_JWT_SIGNING_KEY" => key.map(str::to_owned),
                "AGMEN_EMQX_API_SECRET" => secret.map(str::to_owned),
                "AGMEN_DATA_DIR" => data.map(str::to_owned),
                "AGMEN_EMQX_API_KEY" => None,
                "AGMEN_JWT_EXTRA_KEYS" => Some("/keys/old.pem".to_owned()),
                _ => without_github(name),
            })
        };
        let given = config(Some("/keys/hub.pem"), Some("provided"), Some("/unused")).unwrap();
        assert_eq!(
            given.secrets,
            HubSecrets::Given {
                jwt_signing_key: PathBuf::from("/keys/hub.pem"),
                emqx_api_secret: Secret::new("provided"),
            }
        );
        assert_eq!(given.emqx.api_key, "agmen-hub");
        assert!(!format!("{:?}", given.secrets).contains("provided"));
        for empty in [None, Some("")] {
            let generated = config(empty, empty, Some("/data")).unwrap();
            assert_eq!(
                generated.secrets,
                HubSecrets::Generated {
                    data_dir: PathBuf::from("/data")
                }
            );
            assert_eq!(generated.emqx.api_key, "agmen-hub");
            assert_eq!(generated.jwt_extra_keys, given.jwt_extra_keys);
            assert!(matches!(
                config(empty, empty, empty),
                Err(ConfigError::Missing("AGMEN_DATA_DIR"))
            ));
            assert!(matches!(
                config(Some("/keys/hub.pem"), empty, Some("/data")),
                Err(ConfigError::Missing("AGMEN_EMQX_API_SECRET"))
            ));
            assert!(matches!(
                config(empty, Some("provided"), Some("/data")),
                Err(ConfigError::Missing("AGMEN_JWT_SIGNING_KEY"))
            ));
        }
    }

    #[test]
    fn hub_mode_parses() {
        assert_eq!(HubMode::default(), HubMode::SelfHost);
        for (value, expected) in [
            (None, HubMode::SelfHost),
            (Some(""), HubMode::SelfHost),
            (Some("self-host"), HubMode::SelfHost),
            (Some("hosted"), HubMode::Hosted),
        ] {
            // Hosted mode requires Paddle settings; the other cases must not.
            let base = if value == Some("hosted") {
                hosted_setting
            } else {
                without_github
            };
            let config = Config::from_lookup(|name| match name {
                "AGMEN_HUB_MODE" => value.map(str::to_owned),
                _ => base(name),
            })
            .unwrap();
            assert_eq!(config.mode, expected);
            assert_eq!(config.hosted.is_some(), expected == HubMode::Hosted);
        }
        for value in ["selfhost", "Hosted", " hosted "] {
            assert!(matches!(
                Config::from_lookup(|name| match name {
                    "AGMEN_HUB_MODE" => Some(value.to_owned()),
                    _ => without_github(name),
                }),
                Err(ConfigError::Invalid {
                    name: "AGMEN_HUB_MODE",
                    what: "self-host or hosted",
                })
            ));
        }
    }

    #[test]
    fn group_name_rules() {
        for (value, expected) in [
            (None, "agmen"),
            (Some(""), "agmen"),
            (Some("My group"), "My group"),
            (Some("é"), "é"),
        ] {
            let config = Config::from_lookup(|name| match name {
                "AGMEN_GROUP_NAME" => value.map(str::to_owned),
                _ => without_github(name),
            })
            .unwrap();
            assert_eq!(config.group_name, expected);
        }
        for value in [" x", "x ", "x\n", &"x".repeat(65)] {
            assert!(matches!(
                Config::from_lookup(|name| match name {
                    "AGMEN_GROUP_NAME" => Some(value.to_owned()),
                    _ => without_github(name),
                }),
                Err(ConfigError::Invalid {
                    name: "AGMEN_GROUP_NAME",
                    ..
                })
            ));
        }
    }

    fn without_github(name: &str) -> Option<String> {
        match name {
            "AGMEN_DATABASE_URL" => Some("postgres://localhost/agmen".into()),
            "AGMEN_PUBLIC_URL" => Some("https://hub.example".into()),
            "AGMEN_EMQX_API_URL" => Some("http://emqx:18083/api/v5".into()),
            "AGMEN_EMQX_API_KEY" | "AGMEN_EMQX_API_SECRET" => Some("x".into()),
            "AGMEN_MQTT_URL" => Some("mqtt://emqx:1883".into()),
            "AGMEN_JWT_SIGNING_KEY" => Some("/keys/hub.pem".into()),
            _ => None,
        }
    }

    #[test]
    fn github_client_id_defaults_to_the_shared_app() {
        for (input, expected) in [
            (None, DEFAULT_GITHUB_CLIENT_ID),
            (Some(""), DEFAULT_GITHUB_CLIENT_ID),
            (Some("Ov23liexample"), "Ov23liexample"),
        ] {
            let config = Config::from_lookup(|name| {
                if name == "AGMEN_GITHUB_CLIENT_ID" {
                    input.map(str::to_owned)
                } else {
                    without_github(name)
                }
            })
            .unwrap();
            assert_eq!(config.github.client_id, expected);
        }
    }

    #[test]
    fn client_secret_is_not_read() {
        let config = Config::from_lookup(|name| {
            assert_ne!(name, "AGMEN_GITHUB_CLIENT_SECRET");
            without_github(name)
        });
        assert!(config.is_ok(), "{config:?}");
    }

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
                "AGMEN_GITHUB_CLIENT_ID" | "AGMEN_EMQX_API_KEY" | "AGMEN_EMQX_API_SECRET" => {
                    Some("x".to_owned())
                }
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
    fn public_mqtt_url_defaults_to_the_hub_connection() {
        let with_public = |value: Option<&'static str>| {
            Config::from_lookup(|name| match name {
                "AGMEN_PUBLIC_MQTT_URL" => value.map(str::to_owned),
                "AGMEN_DATABASE_URL" => Some("postgres://localhost/agmen".to_owned()),
                "AGMEN_PUBLIC_URL" => Some("https://api.example.com".to_owned()),
                "AGMEN_EMQX_API_URL" => Some("http://emqx:18083/api/v5".to_owned()),
                "AGMEN_MQTT_URL" => Some("mqtt://emqx:1883".to_owned()),
                "AGMEN_JWT_SIGNING_KEY" => Some("/keys/hub-1.pem".to_owned()),
                "AGMEN_GITHUB_CLIENT_ID" | "AGMEN_EMQX_API_KEY" | "AGMEN_EMQX_API_SECRET" => {
                    Some("x".to_owned())
                }
                _ => None,
            })
        };
        for (value, want) in [
            (None, "mqtt://emqx:1883"),
            (Some(""), "mqtt://emqx:1883"),
            (
                Some("wss://agmen.example.com/mqtt"),
                "wss://agmen.example.com/mqtt",
            ),
            (Some("wss://agmen.example.com"), "wss://agmen.example.com/"),
            (
                Some("mqtts://agmen.example.com:8883"),
                "mqtts://agmen.example.com:8883",
            ),
            (Some("mqtt://localhost:1883"), "mqtt://localhost:1883"),
        ] {
            assert_eq!(
                with_public(value).unwrap().public_mqtt_url.as_str(),
                want,
                "{value:?}"
            );
        }
        for bad in [
            "https://agmen.example.com",
            "ws://agmen.example.com/mqtt",
            "mqtt://agmen.example.com",
            "mqtts://agmen.example.com",
            "wss://",
            "agmen.example.com:1883",
        ] {
            assert!(
                matches!(
                    with_public(Some(bad)),
                    Err(ConfigError::Invalid {
                        name: "AGMEN_PUBLIC_MQTT_URL",
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

    /// A value built at runtime, so the test source never holds a Paddle key.
    fn runtime_secret(label: &str) -> String {
        format!("runtime-{label}-{}", "z".repeat(24))
    }

    fn hosted_setting(name: &str) -> Option<String> {
        match name {
            "AGMEN_HUB_MODE" => Some("hosted".to_owned()),
            "AGMEN_PADDLE_API_KEY" => Some(runtime_secret("api")),
            "AGMEN_PADDLE_WEBHOOK_SECRET" => Some(runtime_secret("hook")),
            "AGMEN_PADDLE_PRICE_TEAM_BASE" => Some("pri_team_base".to_owned()),
            "AGMEN_PADDLE_PRICE_TEAM_SEAT" => Some("pri_team_seat".to_owned()),
            "AGMEN_PADDLE_PRICE_BUSINESS_10" => Some("pri_business_10".to_owned()),
            "AGMEN_PADDLE_PRICE_BUSINESS_25" => Some("pri_business_25".to_owned()),
            "AGMEN_PADDLE_PRICE_BUSINESS_50" => Some("pri_business_50".to_owned()),
            _ => without_github(name),
        }
    }

    #[test]
    fn hosted_needs_paddle_settings() {
        let missing_key = Config::from_lookup(|name| match name {
            "AGMEN_HUB_MODE" => Some("hosted".to_owned()),
            _ => without_github(name),
        })
        .unwrap_err();
        assert!(
            matches!(missing_key, ConfigError::Missing("AGMEN_PADDLE_API_KEY")),
            "{missing_key}"
        );

        let only_key = Config::from_lookup(|name| match name {
            "AGMEN_PADDLE_API_KEY" => None,
            _ => hosted_setting(name),
        })
        .unwrap_err();
        assert!(
            matches!(only_key, ConfigError::Missing("AGMEN_PADDLE_API_KEY")),
            "{only_key}"
        );

        let missing_hook = Config::from_lookup(|name| match name {
            "AGMEN_PADDLE_WEBHOOK_SECRET" => None,
            _ => hosted_setting(name),
        })
        .unwrap_err();
        assert!(
            matches!(
                missing_hook,
                ConfigError::Missing("AGMEN_PADDLE_WEBHOOK_SECRET")
            ),
            "{missing_hook}"
        );
    }

    #[test]
    fn paddle_url_with_userinfo_is_refused_without_echoing_it() {
        let username = runtime_secret("url-user");
        let password = runtime_secret("url-password");
        for url in [
            format!("https://{username}:{password}@api.paddle.com"),
            format!("https://{username}@api.paddle.com"),
            format!("https://:{password}@api.paddle.com"),
            format!("https://{username}:@api.paddle.com"),
            format!("https://%75{username}:%70{password}@api.paddle.com"),
        ] {
            let Err(error) = Config::from_lookup(|name| match name {
                "AGMEN_PADDLE_API_URL" => Some(url.clone()),
                _ => hosted_setting(name),
            }) else {
                panic!("Paddle API URL with userinfo must be refused");
            };
            assert!(matches!(
                error,
                ConfigError::Invalid {
                    name: "AGMEN_PADDLE_API_URL",
                    ..
                }
            ));
            for rendered in [error.to_string(), format!("{error:?}")] {
                assert!(rendered.contains("AGMEN_PADDLE_API_URL"));
                assert!(!rendered.contains(&username), "username appeared in error");
                assert!(!rendered.contains(&password), "password appeared in error");
                assert!(!rendered.contains(&url), "URL appeared in error");
            }
        }
    }

    #[test]
    fn duplicate_price_ids_are_refused() {
        let names = [
            "AGMEN_PADDLE_PRICE_TEAM_BASE",
            "AGMEN_PADDLE_PRICE_TEAM_SEAT",
            "AGMEN_PADDLE_PRICE_BUSINESS_10",
            "AGMEN_PADDLE_PRICE_BUSINESS_25",
            "AGMEN_PADDLE_PRICE_BUSINESS_50",
        ];
        for (index, first) in names.iter().enumerate() {
            for second in names.iter().skip(index + 1) {
                let Err(error) = Config::from_lookup(|name| {
                    if name == *second {
                        hosted_setting(first)
                    } else {
                        hosted_setting(name)
                    }
                }) else {
                    panic!("duplicate Paddle price ids must be refused");
                };
                let message = error.to_string();
                assert!(message.contains(first), "{message}");
                assert!(message.contains(second), "{message}");
                assert!(
                    !message.contains(&hosted_setting(first).unwrap()),
                    "price id appeared in error"
                );
            }
        }
    }

    #[test]
    fn self_host_ignores_paddle_settings() {
        for mode in [None, Some("self-host")] {
            let config = Config::from_lookup(|name| match name {
                "AGMEN_HUB_MODE" => mode.map(str::to_owned),
                "AGMEN_PADDLE_API_URL" => Some("http://api.paddle.com".to_owned()),
                "AGMEN_PADDLE_API_KEY" => Some(runtime_secret("api")),
                "AGMEN_PADDLE_WEBHOOK_SECRET" => Some(runtime_secret("hook")),
                "AGMEN_PADDLE_PRICE_TEAM_BASE" => Some("pri_team_base".to_owned()),
                "AGMEN_PADDLE_PRICE_TEAM_SEAT" => Some("pri_team_seat".to_owned()),
                "AGMEN_PADDLE_PRICE_BUSINESS_10" => Some("pri_business_10".to_owned()),
                "AGMEN_PADDLE_PRICE_BUSINESS_25" => Some("pri_business_25".to_owned()),
                "AGMEN_PADDLE_PRICE_BUSINESS_50" => Some("pri_business_50".to_owned()),
                _ => without_github(name),
            })
            .unwrap();
            assert!(config.hosted.is_none(), "{mode:?}");
            assert_eq!(config.mode, HubMode::SelfHost);
        }
    }

    #[test]
    fn retention_days_bounded() {
        let with_days = |value: Option<&str>| {
            Config::from_lookup(|name| match name {
                "AGMEN_RETENTION_DAYS" => value.map(str::to_owned),
                _ => without_github(name),
            })
        };
        assert_eq!(with_days(Some("30")).unwrap().retention_days, Some(30));
        assert_eq!(with_days(Some("1")).unwrap().retention_days, Some(1));
        assert_eq!(
            with_days(Some("36500")).unwrap().retention_days,
            Some(36_500)
        );
        assert_eq!(with_days(None).unwrap().retention_days, None);
        assert_eq!(with_days(Some("")).unwrap().retention_days, None);
        for bad in ["0", "36501", "x"] {
            assert!(
                matches!(
                    with_days(Some(bad)),
                    Err(ConfigError::Invalid {
                        name: "AGMEN_RETENTION_DAYS",
                        ..
                    })
                ),
                "{bad}"
            );
        }

        for value in ["30", "x"] {
            let hosted = Config::from_lookup(|name| match name {
                "AGMEN_RETENTION_DAYS" => Some(value.to_owned()),
                _ => hosted_setting(name),
            })
            .unwrap();
            assert_eq!(hosted.retention_days, None, "{value}");
            assert!(hosted.hosted.is_some(), "{value}");
        }
    }

    #[test]
    fn hosted_config_debug_hides_secrets() {
        let api_key = runtime_secret("api");
        let webhook = runtime_secret("hook");
        let config = Config::from_lookup(|name| match name {
            "AGMEN_PADDLE_API_KEY" => Some(api_key.clone()),
            "AGMEN_PADDLE_WEBHOOK_SECRET" => Some(webhook.clone()),
            _ => hosted_setting(name),
        })
        .unwrap();
        let rendered = format!("{config:?}");
        assert!(
            rendered.contains("api_key: Secret(<redacted>)"),
            "api key was not redacted"
        );
        assert!(
            rendered.contains("webhook_secret: Secret(<redacted>)"),
            "webhook secret was not redacted"
        );
        assert!(!rendered.contains(&api_key), "api key appeared in Debug");
        assert!(
            !rendered.contains(&webhook),
            "webhook secret appeared in Debug"
        );
    }

    #[test]
    fn paddle_api_url_must_be_https() {
        let with_url = |url: Option<&str>| {
            Config::from_lookup(|name| match name {
                "AGMEN_PADDLE_API_URL" => url.map(str::to_owned),
                _ => hosted_setting(name),
            })
        };
        let api_url = |url: Option<&str>| {
            with_url(url)
                .unwrap()
                .hosted
                .expect("hosted settings")
                .paddle
                .api_url
        };
        assert_eq!(api_url(None).as_str(), "https://api.paddle.com/");
        assert_eq!(api_url(Some("")).as_str(), "https://api.paddle.com/");
        assert_eq!(
            api_url(Some("https://sandbox-api.paddle.com")).as_str(),
            "https://sandbox-api.paddle.com/"
        );
        for bad in [
            "http://api.paddle.com",
            "http://sandbox-api.paddle.com",
            "not a url",
            "mqtt://api.paddle.com:443",
        ] {
            assert!(
                matches!(
                    with_url(Some(bad)),
                    Err(ConfigError::Invalid {
                        name: "AGMEN_PADDLE_API_URL",
                        ..
                    })
                ),
                "{bad}"
            );
        }
    }
}
