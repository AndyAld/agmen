//! Where hub tokens are kept (ruling R17): the OS keychain under service
//! `agmen` in normal runs, or one file per token in the directory that
//! `AGMEN_TOKEN_STORE_DIR` names, which tests and the end-to-end runs set
//! so they never touch the user's keychain.
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
    /// This platform has no keychain agmen supports.
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
    /// The OS keychain: Keychain Services on macOS, the kernel key
    /// retention service on Linux, Credential Manager on Windows.
    Keychain,
    /// One file per token, readable only by the owner, in this directory.
    Dir(PathBuf),
}

impl TokenStore {
    /// The variable that names a directory store.
    pub const DIR_VAR: &str = "AGMEN_TOKEN_STORE_DIR";

    /// A directory store when `AGMEN_TOKEN_STORE_DIR` is set and not empty,
    /// else the keychain.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_dir_var(std::env::var_os(Self::DIR_VAR))
    }

    /// [`TokenStore::from_env`] with the variable's value given.
    ///
    /// ```
    /// use agmen_client::TokenStore;
    /// use std::path::PathBuf;
    /// assert_eq!(TokenStore::from_dir_var(None), TokenStore::Keychain);
    /// assert_eq!(TokenStore::from_dir_var(Some("".into())), TokenStore::Keychain);
    /// assert_eq!(
    ///     TokenStore::from_dir_var(Some("/tmp/home".into())),
    ///     TokenStore::Dir(PathBuf::from("/tmp/home"))
    /// );
    /// ```
    #[must_use]
    pub fn from_dir_var(value: Option<OsString>) -> Self {
        match value {
            Some(dir) if !dir.is_empty() => Self::Dir(dir.into()),
            _ => Self::Keychain,
        }
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

#[cfg(target_os = "linux")]
fn os_store() -> Result<Arc<CredentialStore>, TokenStoreError> {
    let store: Arc<CredentialStore> =
        linux_keyutils_keyring_store::Store::new().map_err(TokenStoreError::Keychain)?;
    Ok(store)
}

#[cfg(target_os = "windows")]
fn os_store() -> Result<Arc<CredentialStore>, TokenStoreError> {
    let store: Arc<CredentialStore> =
        windows_native_keyring_store::Store::new().map_err(TokenStoreError::Keychain)?;
    Ok(store)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn os_store() -> Result<Arc<CredentialStore>, TokenStoreError> {
    Err(TokenStoreError::NoKeychain)
}

/// Writes `bytes` to `path` in `dir`, creating `dir` so only the owner can
/// enter it, through a temporary file only the owner can read, then renames
/// it into place, so a reader sees the old token or the new one, never part
/// of one.
fn write_private(dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut folder = fs::DirBuilder::new();
    folder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut folder, 0o700);
    folder.create(dir)?;
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

    #[test]
    fn directory_store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::Dir(dir.path().join("tokens"));
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
            let path = dir.path().join("tokens").join(ours.file_name());
            let mode = fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let folder = fs::metadata(dir.path().join("tokens")).unwrap();
            assert_eq!(folder.permissions().mode() & 0o777, 0o700);
        }
        store.delete(&ours).unwrap();
        store.delete(&ours).unwrap();
        assert_eq!(store.get(&ours).unwrap(), None);
    }
}
