//! Repository URLs checked before they are stored or sent to a hub.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// A repository URL without HTTPS user info or an SSH password.
///
/// Accepts HTTPS, SSH and scp forms, at most 512 bytes, with no whitespace
/// or control characters, and no query or fragment. The original spelling is
/// preserved.
///
/// ```
/// # fn main() -> Result<(), agmen_proto::BadRepoUrl> {
/// let url: agmen_proto::RepoUrl = "git@github.com:agmen-dev/agmen.git".parse()?;
/// assert_eq!(url.as_str(), "git@github.com:agmen-dev/agmen.git");
/// # Ok(()) }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct RepoUrl(String);

/// A repository URL outside the allowed forms; never includes the input.
///
/// ```
/// # fn main() -> Result<(), agmen_proto::BadRepoUrl> {
/// let url: agmen_proto::RepoUrl = "https://github.com/agmen-dev/agmen".parse()?;
/// assert!("/tmp/repository".parse::<agmen_proto::RepoUrl>().is_err());
/// # Ok(()) }
/// ```
#[derive(Debug, thiserror::Error)]
#[error(
    "repository URL must be https://host/path without user info, ssh://user@host/path without a password, or user@host:path; at most 512 bytes, no whitespace, control characters, query or fragment, and no leading '-'"
)]
pub struct BadRepoUrl;

impl RepoUrl {
    /// The repository URL as supplied.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RepoUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RepoUrl {
    type Err = BadRepoUrl;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        check(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for RepoUrl {
    type Error = BadRepoUrl;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        check(&value)?;
        Ok(Self(value))
    }
}

fn check(value: &str) -> Result<(), BadRepoUrl> {
    if value.len() > 512
        || value.starts_with('-')
        || value.chars().any(|c| c.is_whitespace() || c.is_control())
        || value.contains(['\\', '?', '#'])
    {
        return Err(BadRepoUrl);
    }
    let (host, path) = if let Some(rest) = value.strip_prefix("https://") {
        rest.split_once('/').ok_or(BadRepoUrl)?
    } else {
        let (rest, separator) = match value.strip_prefix("ssh://") {
            Some(rest) => (rest, '/'),
            None => (value, ':'),
        };
        let (user, remote) = rest.split_once('@').ok_or(BadRepoUrl)?;
        // A leading '-' would read as an option to ssh.
        if user.is_empty()
            || user.starts_with('-')
            || !user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(BadRepoUrl);
        }
        // Brackets keep IPv6 colons out of the scp path separator.
        if separator == ':' && remote.starts_with('[') {
            let end = remote.find(']').ok_or(BadRepoUrl)? + 1;
            let (host, path) = remote.split_at(end);
            (host, path.strip_prefix(':').ok_or(BadRepoUrl)?)
        } else {
            remote.split_once(separator).ok_or(BadRepoUrl)?
        }
    };
    if path.is_empty() || !is_host(host) {
        return Err(BadRepoUrl);
    }
    Ok(())
}

fn is_host(authority: &str) -> bool {
    let port = if let Some(rest) = authority.strip_prefix('[') {
        let Some((host, suffix)) = rest.split_once(']') else {
            return false;
        };
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return false;
        }
        if suffix.is_empty() {
            None
        } else if let Some(port) = suffix.strip_prefix(':') {
            Some(port)
        } else {
            return false;
        }
    } else {
        let (host, port) = authority
            .split_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)));
        if host.is_empty()
            || host.starts_with('-')
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return false;
        }
        port
    };
    port.is_none_or(|port| {
        !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_urls_accept_https_ssh_and_scp_forms() {
        for input in [
            "https://github.com/agmen-dev/agmen".to_owned(),
            "https://github.com/agmen-dev/agmen.git".to_owned(),
            "ssh://git@github.com/agmen-dev/agmen.git".to_owned(),
            "git@github.com:agmen-dev/agmen.git".to_owned(),
            "https://git.example:8443/a/b".to_owned(),
            "ssh://git@git.example:2222/a/b".to_owned(),
            "https://[::1]/a/b".to_owned(),
            "ssh://git@[::1]:2222/a/b".to_owned(),
            "git@[::1]:a/b".to_owned(),
            format!("https://github.com/{}", "a".repeat(493)),
            format!("https://github.com/a{}", "é".repeat(246)),
        ] {
            let parsed: RepoUrl = input.parse().unwrap();
            assert_eq!(parsed.as_str(), input);
            assert_eq!(parsed.to_string(), input);
            assert_eq!(RepoUrl::try_from(input.clone()).unwrap(), parsed);
            let json = serde_json::to_string(&parsed).unwrap();
            assert_eq!(json, serde_json::to_string(&input).unwrap());
            assert_eq!(serde_json::from_str::<RepoUrl>(&json).unwrap(), parsed);
        }
    }

    #[test]
    fn repo_urls_refuse_queries_fragments_and_dashed_names() {
        for input in [
            format!("https://github.com/a/b?token={}", "t"),
            "https://github.com/a/b#frag".to_owned(),
            "git@github.com:a/b?x=1".to_owned(),
            "git@github.com:a/b#frag".to_owned(),
            "ssh://git@github.com/a/b?x=1".to_owned(),
            "ssh://-oFoo@github.com/a".to_owned(),
            "ssh://git@-host/a".to_owned(),
            "git@-host:a".to_owned(),
            "https://-host/a".to_owned(),
            "https://[::1]:80/a?x".to_owned(),
        ] {
            assert!(input.parse::<RepoUrl>().is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn repo_urls_refuse_secrets_options_and_local_paths() {
        for input in [
            "".to_owned(),
            "http://github.com/a/b".to_owned(),
            format!("https://{}:{}@github.com/a/b", "u", "p"),
            format!("https://{}@github.com/a/b", "u"),
            format!("ssh://{}:{}@github.com/a", "git", "pw"),
            "file:///tmp/r".to_owned(),
            "/tmp/r".to_owned(),
            "-oProxyCommand=x".to_owned(),
            "git@github.com:a b".to_owned(),
            "https://github.com/a\nb".to_owned(),
            format!("https://github.com/{}", "a".repeat(495)),
            format!("https://github.com/{}", "a".repeat(494)),
            format!("https://github.com/{}", "é".repeat(247)),
            "https:///a/b".to_owned(),
            "https://github.com".to_owned(),
            "https://github.com/".to_owned(),
            "ssh://git@/a".to_owned(),
            "ssh://@github.com/a".to_owned(),
            "git@:a".to_owned(),
            "@github.com:a".to_owned(),
            "git@github.com:".to_owned(),
            "git@github.com/a".to_owned(),
            "https://github.com:/a".to_owned(),
            "https://github.com:65536/a".to_owned(),
            "https://github.com:port/a".to_owned(),
            "https://[::1/a".to_owned(),
            "https://[host]/a".to_owned(),
            "https://[::1]suffix/a".to_owned(),
            "https://[::1]:/a".to_owned(),
            "git@[::1:a".to_owned(),
            "git@[::1]a".to_owned(),
            "https://github.com/a\0b".to_owned(),
            "https://github.com/a\u{2003}b".to_owned(),
            format!("https://github.com\\{}:{}@other.test/a", "u", "p"),
            format!("ssh://{}:{}@github.com:a", "git", "pw"),
        ] {
            assert!(input.parse::<RepoUrl>().is_err(), "accepted {input:?}");
            assert!(RepoUrl::try_from(input.clone()).is_err());
            let json = serde_json::to_string(&input).unwrap();
            assert!(serde_json::from_str::<RepoUrl>(&json).is_err());
        }
    }
}
