//! Where hub tokens are kept (rulings R17 and R53).
//!
//! - When `AGMEN_TOKEN_STORE_DIR` is set, one file per token in that
//!   directory, on every platform. Tests and the end-to-end runs set it so
//!   they never touch the user's keychain.
//! - Otherwise, on macOS and Windows, the OS keychain under service `agmen`.
//! - Otherwise, on Linux, one file per token in `$XDG_STATE_HOME/agmen`, or
//!   `~/.local/state/agmen` when `XDG_STATE_HOME` is unset (R53). The kernel
//!   key retention service loses its keys on a reboot, and the Secret
//!   Service store pulls in crates `cargo deny` rejects.
//!
//! A directory store keeps its directory at mode 0700 and each token file
//! at 0600, written to a temporary file and renamed into place.
//!
//! Every token is keyed by the hub's origin and the id it acts as (ruling
//! R42), so an identity file that names another hub finds no token to
//! send there.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agmen_proto::IdentityId;
use keyring_core::{CredentialStore, Entry};
use url::Url;

/// The keychain service every agmen token is stored under.
const SERVICE: &str = "agmen";

/// Why the token store failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TokenStoreError {
    /// The hub URL has no origin a token can be keyed by.
    #[error("the hub URL {0} has no origin to key a token by")]
    OpaqueOrigin(Url),
    /// The OS keychain failed.
    #[error("the OS keychain failed")]
    Keychain(#[source] keyring_core::Error),
    /// This platform has no keychain agmen supports: Linux (which keeps
    /// tokens in `$XDG_STATE_HOME/agmen` unless neither that nor `HOME` is
    /// set) and anything but macOS and Windows.
    #[error("this platform has no keychain agmen can use; set AGMEN_TOKEN_STORE_DIR")]
    NoKeychain,
    /// Reading or writing a token file failed.
    #[error("the token file {path} failed")]
    File {
        /// The file.
        path: PathBuf,
        /// What failed.
        #[source]
        source: io::Error,
    },
}

/// Which token: one hub (by origin) and the id it acts as.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenKey(String);

impl TokenKey {
    /// The key for `id`'s token at the hub at `hub`. Only the URL's origin
    /// counts: scheme, host and port.
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use agmen_client::TokenKey;
    /// let id = "idn7k2q9".parse()?;
    /// let key = TokenKey::new(&"https://hub.example.com/api".parse()?, &id)?;
    /// assert_eq!(key.as_str(), "https://hub.example.com idn7k2q9");
    /// assert_ne!(key, TokenKey::new(&"https://evil.example".parse()?, &id)?);
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    ///
    /// [`TokenStoreError::OpaqueOrigin`] for a URL without an origin, such
    /// as `file:`.
    pub fn new(hub: &Url, id: &IdentityId) -> Result<Self, TokenStoreError> {
        let origin = hub.origin();
        if !origin.is_tuple() {
            return Err(TokenStoreError::OpaqueOrigin(hub.clone()));
        }
        Ok(Self(format!("{} {id}", origin.ascii_serialization())))
    }

    /// The key as a string: `<origin> <id>`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The token's file name in a directory store: the key in hex, so no
    /// key can name a path outside the directory.
    fn file_name(&self) -> String {
        let mut name = String::with_capacity(4 + 2 * self.0.len());
        name.push_str("tok-");
        for byte in self.0.bytes() {
            name.push(char::from(HEX[usize::from(byte >> 4)]));
            name.push(char::from(HEX[usize::from(byte & 15)]));
        }
        name
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

impl fmt::Display for TokenKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where tokens are kept.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TokenStore {
    /// The OS keychain: Keychain Services on macOS, Credential Manager on
    /// Windows. Other platforms, Linux included, fail with
    /// [`TokenStoreError::NoKeychain`].
    Keychain,
    /// One file per token, readable only by the owner, in this directory.
    Dir(PathBuf),
}

impl TokenStore {
    /// The variable that names a directory store.
    pub const DIR_VAR: &str = "AGMEN_TOKEN_STORE_DIR";

    /// The store for this run: a directory store in `AGMEN_TOKEN_STORE_DIR`
    /// when it is set and not empty; else, on Linux, a directory store in
    /// `$XDG_STATE_HOME/agmen` or `$HOME/.local/state/agmen` (ruling R53);
    /// else the keychain.
    #[must_use]
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        if let Some(dir) = var(Self::DIR_VAR) {
            return Self::Dir(dir.into());
        }
        if cfg!(target_os = "linux")
            && let Some(dir) = xdg_state_dir(var("XDG_STATE_HOME"), var("HOME"))
        {
            return Self::Dir(dir);
        }
        Self::Keychain
    }

    /// The token stored under `key`, if any. This reads a file or the
    /// keychain; call it off the async runtime's worker threads.
    ///
    /// # Errors
    ///
    /// [`TokenStoreError`] when the store cannot be read.
    pub fn get(&self, key: &TokenKey) -> Result<Option<String>, TokenStoreError> {
        match self {
            Self::Keychain => match entry(key)?.get_password() {
                Ok(token) => Ok(Some(token)),
                Err(keyring_core::Error::NoEntry) => Ok(None),
                Err(err) => Err(TokenStoreError::Keychain(err)),
            },
            Self::Dir(dir) => {
                let path = dir.join(key.file_name());
                match fs::read_to_string(&path) {
                    Ok(token) => Ok(Some(token)),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(source) => Err(TokenStoreError::File { path, source }),
                }
            }
        }
    }

    /// Stores `token` under `key`, replacing any token there.
    ///
    /// # Errors
    ///
    /// [`TokenStoreError`] when the store cannot be written.
    pub fn set(&self, key: &TokenKey, token: &str) -> Result<(), TokenStoreError> {
        match self {
            Self::Keychain => entry(key)?
                .set_password(token)
                .map_err(TokenStoreError::Keychain),
            Self::Dir(dir) => {
                let path = dir.join(key.file_name());
                write_private(dir, &path, token.as_bytes())
                    .map_err(|source| TokenStoreError::File { path, source })
            }
        }
    }

    /// Removes the token under `key`; removing one that is not there
    /// succeeds.
    ///
    /// # Errors
    ///
    /// [`TokenStoreError`] when the store cannot be written.
    pub fn delete(&self, key: &TokenKey) -> Result<(), TokenStoreError> {
        match self {
            Self::Keychain => match entry(key)?.delete_credential() {
                Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
                Err(err) => Err(TokenStoreError::Keychain(err)),
            },
            Self::Dir(dir) => {
                let path = dir.join(key.file_name());
                match fs::remove_file(&path) {
                    Ok(()) => Ok(()),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(source) => Err(TokenStoreError::File { path, source }),
                }
            }
        }
    }
}

/// The keychain entry for `key`.
fn entry(key: &TokenKey) -> Result<Entry, TokenStoreError> {
    os_store()?
        .build(SERVICE, key.as_str(), None)
        .map_err(TokenStoreError::Keychain)
}

#[cfg(target_os = "macos")]
fn os_store() -> Result<Arc<CredentialStore>, TokenStoreError> {
    let store: Arc<CredentialStore> =
        apple_native_keyring_store::keychain::Store::new().map_err(TokenStoreError::Keychain)?;
    Ok(store)
}

#[cfg(target_os = "windows")]
fn os_store() -> Result<Arc<CredentialStore>, TokenStoreError> {
    let store: Arc<CredentialStore> =
        windows_native_keyring_store::Store::new().map_err(TokenStoreError::Keychain)?;
    Ok(store)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn os_store() -> Result<Arc<CredentialStore>, TokenStoreError> {
    Err(TokenStoreError::NoKeychain)
}

/// Linux's token directory (ruling R53): `$XDG_STATE_HOME/agmen`, or
/// `$HOME/.local/state/agmen` when `XDG_STATE_HOME` is unset or relative
/// (the XDG base directory spec says to ignore a relative one). `None` when
/// neither gives an absolute path.
fn xdg_state_dir(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let absolute = |value: Option<OsString>| value.map(PathBuf::from).filter(|p| p.is_absolute());
    let base = absolute(xdg_state_home)
        .or_else(|| absolute(home).map(|home| home.join(".local").join("state")))?;
    Some(base.join("agmen"))
}

/// Writes `bytes` to `path` in `dir`, making `dir` so only the owner can
/// enter it, through a temporary file only the owner can read, then renames
/// it into place, so a reader sees the old token or the new one, never part
/// of one.
fn write_private(dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut folder = fs::DirBuilder::new();
    folder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut folder, 0o700);
    folder.create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // A folder made earlier with the umask's mode (on Linux the
        // identities' state folders share `$XDG_STATE_HOME/agmen`) would let
        // others list which hubs and ids have tokens.
        if fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(hub: &str) -> TokenKey {
        TokenKey::new(&hub.parse().unwrap(), &"idn7k2q9".parse().unwrap()).unwrap()
    }

    #[test]
    fn keys_carry_only_the_origin() {
        assert_eq!(
            key("https://hub.example.com/a/b?x=1").as_str(),
            "https://hub.example.com idn7k2q9"
        );
        assert_eq!(
            key("http://127.0.0.1:8090").as_str(),
            "http://127.0.0.1:8090 idn7k2q9"
        );
        assert_ne!(
            key("https://hub.example.com"),
            key("https://hub.example.com:8443")
        );
        assert_ne!(
            key("https://hub.example.com"),
            key("http://hub.example.com")
        );
        assert!(matches!(
            TokenKey::new(
                &"file:///tmp/x".parse().unwrap(),
                &"idn7k2q9".parse().unwrap()
            ),
            Err(TokenStoreError::OpaqueOrigin(_))
        ));
    }

    #[test]
    fn file_names_stay_in_the_directory() {
        let name = key("https://hub.example.com").file_name();
        assert!(name.starts_with("tok-"));
        assert!(name[4..].bytes().all(|b| b.is_ascii_hexdigit()));
    }

    /// Stores, replaces, reads and deletes a token in `store`, which must
    /// be a directory store in `dir`, and checks the modes on the way.
    fn round_trip(store: &TokenStore, dir: &Path) {
        let (ours, theirs) = (key("https://hub.example.com"), key("https://evil.example"));
        assert_eq!(store.get(&ours).unwrap(), None);
        store.set(&ours, "secret-1").unwrap();
        store.set(&ours, "secret-2").unwrap();
        assert_eq!(store.get(&ours).unwrap().as_deref(), Some("secret-2"));
        assert_eq!(
            store.get(&theirs).unwrap(),
            None,
            "another origin finds nothing"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(dir.join(ours.file_name()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            let folder = fs::metadata(dir).unwrap();
            assert_eq!(folder.permissions().mode() & 0o777, 0o700);
        }
        store.delete(&ours).unwrap();
        store.delete(&ours).unwrap();
        assert_eq!(store.get(&ours).unwrap(), None);
    }

    #[test]
    fn directory_store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = dir.path().join("tokens");
        round_trip(&TokenStore::Dir(tokens.clone()), &tokens);
    }

    #[test]
    fn linux_store_lives_in_xdg_state_home() {
        let state = tempfile::tempdir().unwrap();
        let dir = xdg_state_dir(Some(state.path().into()), Some("/home/nobody".into())).unwrap();
        assert_eq!(dir, state.path().join("agmen"));
        // An identity's state folder made it first, with the umask's mode.
        fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        }
        round_trip(&TokenStore::Dir(dir.clone()), &dir);
    }

    #[test]
    fn linux_store_falls_back_to_home() {
        let home = || Some(OsString::from("/home/andy"));
        let fallback = PathBuf::from("/home/andy/.local/state/agmen");
        assert_eq!(xdg_state_dir(None, home()), Some(fallback.clone()));
        assert_eq!(
            xdg_state_dir(Some("relative/state".into()), home()),
            Some(fallback),
            "a relative XDG_STATE_HOME is ignored"
        );
        assert_eq!(xdg_state_dir(None, None), None);
        assert_eq!(xdg_state_dir(None, Some("relative".into())), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_has_no_keychain() {
        assert!(matches!(
            TokenStore::Keychain.get(&key("https://hub.example.com")),
            Err(TokenStoreError::NoKeychain)
        ));
    }
}
