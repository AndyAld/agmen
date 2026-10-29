//! Host directory name, the private env file, and the compose template.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context as _, Result, bail};

/// At most this many bytes of an env file are read.
const ENV_LIMIT: u64 = 64 * 1024;

/// A host directory name: `[a-z0-9][a-z0-9-]{0,31}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostName(String);

impl Default for HostName {
    fn default() -> Self {
        Self("agmen".to_owned())
    }
}

impl fmt::Display for HostName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for HostName {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if crate::paths::is_host_name(text) {
            Ok(Self(text.to_owned()))
        } else {
            Err(format!(
                "{text:?} is not a host name: use 1 to 32 lowercase letters, digits, and hyphens, starting with a letter or digit"
            ))
        }
    }
}

impl HostName {
    /// The name as a directory component.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// `KEY=value` pairs for one host. Debug output names the keys and omits the values.
///
/// Comment lines and blank lines are kept and written back above the sorted
/// keys. They are not pairs.
#[derive(Default, PartialEq, Eq)]
pub(crate) struct HostEnv {
    pairs: BTreeMap<String, String>,
    notes: Vec<String>,
}

impl fmt::Debug for HostEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keys: Vec<&str> = self.pairs.keys().map(String::as_str).collect();
        f.debug_struct("HostEnv").field("keys", &keys).finish()
    }
}

impl HostEnv {
    /// Reads an env file. `Ok(None)` when `path` is missing.
    ///
    /// # Errors
    ///
    /// When the file cannot be read, is larger than 64 KiB, is not UTF-8,
    /// or contains a line this type refuses. A blank line, or a comment
    /// (`#` after leading whitespace), is kept and is not a pair.
    pub(crate) fn load(path: &Path) -> Result<Option<Self>> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err).with_context(|| format!("opening {}", path.display()));
            }
        };
        let mut bytes = Vec::new();
        file.take(ENV_LIMIT + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading {}", path.display()))?;
        if bytes.len() as u64 > ENV_LIMIT {
            bail!("the env file exceeds 64 KiB");
        }
        let text = String::from_utf8(bytes).context("the env file is not UTF-8")?;
        let mut env = Self::default();
        for (number, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                env.notes.push(line.to_owned());
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                bail!("env line {} has no '='", number + 1);
            };
            if env.pairs.contains_key(key) {
                bail!("env key {key} is duplicated");
            }
            env.set(key, value)?;
        }
        Ok(Some(env))
    }

    /// The value stored for `key`.
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.pairs.get(key).map(String::as_str)
    }

    /// Stores `value` under `key`, replacing a previous value.
    ///
    /// # Errors
    ///
    /// When `key` is empty or contains `=` or whitespace, or when `value`
    /// contains a newline. The message does not include `value`.
    pub(crate) fn set(&mut self, key: &str, value: &str) -> Result<()> {
        if key.is_empty()
            || key.contains('=')
            || key.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        {
            bail!("an env key must be non-empty and contain no '=' or whitespace");
        }
        if value.contains('\n') || value.contains('\r') {
            bail!("an env value cannot contain a newline");
        }
        self.pairs.insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    /// Removes `key`, including its value, when it is present.
    pub(crate) fn remove(&mut self, key: &str) {
        self.pairs.remove(key);
    }

    /// Sets `key` to 32 random bytes as 64 hex characters when it is absent.
    ///
    /// # Errors
    ///
    /// When random bytes are unavailable, or when `key` is not a valid env key.
    pub(crate) fn ensure_secret(&mut self, key: &str) -> Result<()> {
        if self.pairs.contains_key(key) {
            return Ok(());
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).context("generating a host secret")?;
        self.set(key, &hex(&bytes))
    }

    /// Writes sorted `KEY=value` lines to `path` via a temp file in the same
    /// directory, mode 0600 on Unix.
    ///
    /// # Errors
    ///
    /// When the file cannot be created, synced, or renamed into place.
    pub(crate) fn write(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("the env path has no directory")?;
        let mut tmp_name = path
            .file_name()
            .context("the env path has no file name")?
            .to_os_string();
        tmp_name.push(".tmp");
        let tmp = parent.join(tmp_name);
        let mut body = String::new();
        for note in &self.notes {
            body.push_str(note);
            body.push('\n');
        }
        for (key, value) in &self.pairs {
            body.push_str(key);
            body.push('=');
            body.push_str(value);
            body.push('\n');
        }
        if let Err(err) = write_private(&tmp, &body) {
            remove_temp(&tmp);
            return Err(err);
        }
        if let Err(err) = fs::rename(&tmp, path) {
            remove_temp(&tmp);
            return Err(err).with_context(|| format!("replacing {}", path.display()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("setting permissions on {}", path.display()))?;
        }
        Ok(())
    }
}

fn write_private(path: &Path, body: &str) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    // shortcut: no private mode on Windows; the unix tests cover 0600
    let mut file = options
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting permissions on {}", path.display()))?;
    }
    file.write_all(body.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {}", path.display()))?;
    Ok(())
}

fn remove_temp(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::debug!(%err, path = %path.display(), "removing a temp env file failed");
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// The compose file with `{{name}}` and `{{version}}` filled in.
pub(crate) fn render_compose(name: &HostName) -> String {
    include_str!("compose.yaml")
        .replace("{{name}}", name.as_str())
        .replace("{{version}}", env!("CARGO_PKG_VERSION"))
}

/// Broker config mounted into the emqx service.
pub(crate) const EMQX_CONF: &str = include_str!("emqx.conf");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        let mut env = HostEnv::default();
        env.set("B", "2").unwrap();
        env.set("A", "1").unwrap();
        env.set("URL", "http://host/a=b").unwrap();
        env.ensure_secret("AGMEN_POSTGRES_PASSWORD").unwrap();
        let secret = env.get("AGMEN_POSTGRES_PASSWORD").unwrap().to_owned();
        env.ensure_secret("AGMEN_POSTGRES_PASSWORD").unwrap();
        assert!(env.get("AGMEN_POSTGRES_PASSWORD") == Some(secret.as_str()));
        env.write(&path).unwrap();

        let loaded = HostEnv::load(&path).unwrap().unwrap();
        assert_eq!(loaded, env);

        let text = fs::read_to_string(&path).unwrap();
        let keys: Vec<_> = text
            .lines()
            .map(|line| line.split_once('=').unwrap().0)
            .collect::<Vec<_>>();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn env_values_with_newlines_are_refused() {
        let mut env = HostEnv::default();
        env.set("A", "kept").unwrap();
        assert!(env.set("A", "bad\nvalue").is_err());
        assert_eq!(env.get("A"), Some("kept"));
        assert!(env.set("A", "bad\rvalue").is_err());
        assert_eq!(env.get("A"), Some("kept"));
        assert!(env.set("A=B", "value").is_err());
        assert!(env.get("A=B").is_none());
        assert!(env.set("", "value").is_err());
        assert!(env.set("HAS SPACE", "value").is_err());
    }

    #[test]
    fn emqx_conf_matches_dev_rules() {
        let dev = include_str!("../../../../../deploy/dev/emqx/emqx.conf");
        for header in ["mqtt {", "authorization {", "durable_sessions {"] {
            assert_eq!(block(EMQX_CONF, header), block(dev, header), "{header}");
        }
        let dev_auth = block(dev, "authentication = [").replace(
            "http://host.docker.internal:8090/v1/jwks",
            "http://hub:8080/v1/jwks",
        );
        assert_eq!(block(EMQX_CONF, "authentication = ["), dev_auth);
        assert!(
            EMQX_CONF
                .lines()
                .all(|line| !line.trim().starts_with("cookie"))
        );
        assert!(EMQX_CONF.contains("name = agmen-host"));
        assert!(!EMQX_CONF.contains("name = agmen-dev"));
    }

    #[test]
    fn compose_publishes_only_the_hub() {
        let rendered = render_compose(&HostName::default());
        assert_eq!(rendered.matches("127.0.0.1:${AGMEN_HOST_PORT").count(), 1);
        assert!(!rendered.contains("1883:"));
        assert!(!rendered.contains("8083:"));
        assert!(!rendered.contains("18083:"));
        assert!(!rendered.contains(":latest"));
    }

    fn block<'a>(text: &'a str, header: &str) -> &'a str {
        let start = text
            .find(header)
            .unwrap_or_else(|| panic!("missing {header}"));
        assert!(
            start == 0 || text.as_bytes()[start - 1] == b'\n',
            "{header} is not at the start of a line"
        );
        let rest = &text[start..];
        let mut depth = 0i32;
        let mut saw_open = false;
        for (idx, ch) in rest.char_indices() {
            match ch {
                '{' | '[' => {
                    depth += 1;
                    saw_open = true;
                }
                '}' | ']' => {
                    depth -= 1;
                    assert!(depth >= 0, "{header} closes too early");
                    if saw_open && depth == 0 {
                        return &rest[..=idx];
                    }
                }
                _ => {}
            }
        }
        panic!("{header} is not balanced");
    }
}
