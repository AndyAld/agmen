//! Hub configuration, read from `AGMEN_*` environment variables.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

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
    /// `AGMEN_JWT_SIGNING_KEY`: the ES256 private key (PKCS#8 PEM) that
    /// signs broker tokens. Its key id is the file name without `.pem`.
    pub jwt_signing_key: PathBuf,
    /// `AGMEN_JWT_EXTRA_KEYS`, comma separated, optional: more private keys
    /// published in `/v1/jwks` but never used to sign. During a rotation
    /// this holds the next key (published at least 60 s before the switch)
    /// or the previous one (kept until its last token has expired).
    pub jwt_extra_keys: Vec<PathBuf>,
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
    ///     ("AGMEN_JWT_SIGNING_KEY", "/keys/hub-2.pem"),
    ///     ("AGMEN_JWT_EXTRA_KEYS", "/keys/hub-1.pem"),
    /// ]);
    /// let config = agmen_hub::Config::from_lookup(|name| vars.get(name).map(|v| v.to_string()))?;
    /// assert_eq!(config.listen.port(), 8080);
    /// assert_eq!(config.github.api_url.as_str(), "https://api.github.com/");
    /// assert_eq!(config.jwt_extra_keys.len(), 1);
    /// # Ok(()) }
    /// ```
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |name: &'static str| lookup(name).filter(|value| !value.is_empty());
        let required = |name: &'static str| get(name).ok_or(ConfigError::Missing(name));
        let url = |name: &'static str, value: String| {
            Url::parse(&value)
                .ok()
                .filter(|url| matches!(url.scheme(), "http" | "https"))
                .ok_or(ConfigError::Invalid {
                    name,
                    what: "http or https URL",
                })
        };
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
                api_url: url(
                    "AGMEN_GITHUB_API_URL",
                    get("AGMEN_GITHUB_API_URL")
                        .unwrap_or_else(|| "https://api.github.com".to_owned()),
                )?,
            },
            emqx: EmqxConfig {
                api_url: url("AGMEN_EMQX_API_URL", required("AGMEN_EMQX_API_URL")?)?,
                api_key: required("AGMEN_EMQX_API_KEY")?,
                api_secret: Secret::new(required("AGMEN_EMQX_API_SECRET")?),
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
    fn secrets_are_redacted_in_debug() {
        let secret = Secret::new(format!("pw-{}", "x".repeat(12)));
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
    }
}
