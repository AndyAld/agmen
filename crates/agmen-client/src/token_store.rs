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
//! at 0600, written to a temporary file and renamed into place. A conditional
//! write ([`TokenStore::set_if`]) runs its check between the two.
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
        self.set_if(key, token, || true).map(|_stored| ())
    }

    /// Stores `token` under `key` like [`set`](Self::set), unless `check`
    /// returns `false`: then nothing is stored and the answer is `Ok(false)`.
    /// `check` runs as late as the store allows. A directory store has the new
    /// file written and synced by then and renames it into place right after,
    /// or removes it; the keychain is written right after. A check that reads
    /// other files or the store itself needs external synchronization with
    /// their writers: login, logout and renewal share the login lock.
    ///
    /// # Errors
    ///
    /// [`TokenStoreError`] when the store cannot be written.
    pub(crate) fn set_if(
        &self,
        key: &TokenKey,
        token: &str,
        check: impl FnOnce() -> bool,
    ) -> Result<bool, TokenStoreError> {
        match self {
            Self::Keychain => {
                let entry = entry(key)?;
                if !check() {
                    return Ok(false);
                }
                entry
                    .set_password(token)
                    .map_err(TokenStoreError::Keychain)?;
                Ok(true)
            }
            Self::Dir(dir) => {
                let path = dir.join(key.file_name());
                write_private(dir, &path, token.as_bytes(), check)
                    .map_err(|source| TokenStoreError::File { path, source })
            }
        }
    }

    /// Removes the token under `key`, and in a directory store the temporary
    /// files a crashed write of that key left; removing one that is not there
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
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(source) => return Err(TokenStoreError::File { path, source }),
                }
                remove_leftovers(dir, &key.file_name())
                    .map_err(|source| TokenStoreError::File { path, source })
            }
        }
    }
}

/// The keychain entry for `key`.
fn entry(key: &TokenKey) -> Result<Entry, TokenStoreError> {
    #[cfg(test)]
    if let Some(store) = fake_keychain::store(key) {
        return store
            .build(SERVICE, key.as_str(), None)
            .map_err(TokenStoreError::Keychain);
    }
    os_store()?
        .build(SERVICE, key.as_str(), None)
        .map_err(TokenStoreError::Keychain)
}

#[cfg(test)]
#[path = "token_store_fake.rs"]
pub(crate) mod fake_keychain;

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
/// of one. `commit` runs after the file is written and synced and before the
/// rename; when it returns `false` the temporary file is removed instead and
/// the answer is `Ok(false)`.
fn write_private(
    dir: &Path,
    path: &Path,
    bytes: &[u8],
    commit: impl FnOnce() -> bool,
) -> io::Result<bool> {
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
    let (mut file, tmp) = create_temp(path, &mut || getrandom::u64().map_err(io::Error::other))?;
    // Every failure or unwind after the file exists removes it, so no token
    // is left behind in the folder.
    let mut staged = TempFile {
        path: tmp,
        armed: true,
    };
    file.write_all(bytes)?;
    #[cfg(any(test, feature = "test-hooks"))]
    if write_fails(path) {
        return Err(io::Error::other("injected token write failure"));
    }
    file.sync_all()?;
    if !commit() {
        fs::remove_file(&staged.path)?;
        staged.armed = false;
        return Ok(false);
    }
    fs::rename(&staged.path, path)?;
    staged.armed = false;
    Ok(true)
}

/// A staged temporary file, removed when dropped while `armed`.
struct TempFile {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        // The write error is the one to report; a failed cleanup adds nothing.
        if self.armed {
            drop(fs::remove_file(&self.path));
        }
    }
}

/// How many random names `create_temp` tries before giving up.
const TEMP_NAME_TRIES: u32 = 8;

/// Creates `<path>.<16 hex digits>.tmp` beside `path`, owner-only on unix,
/// with the digits from `names`. The open is `create_new` (O_CREAT | O_EXCL),
/// so a file or a symlink already at the name is never followed, written or
/// replaced: the name is just tried again, up to `TEMP_NAME_TRIES` times.
fn create_temp(
    path: &Path,
    names: &mut impl FnMut() -> io::Result<u64>,
) -> io::Result<(fs::File, PathBuf)> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    for _ in 0..TEMP_NAME_TRIES {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".{:016x}.tmp", names()?));
        let tmp = PathBuf::from(tmp);
        match options.open(&tmp) {
            Ok(file) => return Ok((file, tmp)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::from(io::ErrorKind::AlreadyExists))
}

/// Removes the crash leftovers of the token file `name` in `dir`: exactly
/// `<name>.<16 lowercase hex digits>.tmp`, so another key's file never
/// matches, even when its name starts with this one's.
fn remove_leftovers(dir: &Path, name: &str) -> io::Result<()> {
    let is_leftover = |file: &std::ffi::OsStr| {
        file.to_str()
            .and_then(|file| file.strip_prefix(name)?.strip_prefix('.'))
            .and_then(|rest| rest.strip_suffix(".tmp"))
            .is_some_and(|hex| {
                hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            })
    };
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if is_leftover(&entry.file_name()) && !entry.file_type()?.is_dir() {
            match fs::remove_file(entry.path()) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
    }
    Ok(())
}

/// Token files whose next write fails once the temporary file is written.
#[cfg(any(test, feature = "test-hooks"))]
static FAILING_WRITES: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Ends the failure `fail_token_writes` started when dropped.
#[cfg(any(test, feature = "test-hooks"))]
#[must_use = "the failure ends when the guard is dropped"]
#[derive(Debug)]
pub struct TokenWriteFailure(PathBuf);

#[cfg(any(test, feature = "test-hooks"))]
impl Drop for TokenWriteFailure {
    fn drop(&mut self) {
        if let Ok(mut failing) = FAILING_WRITES.lock()
            && let Some(at) = failing.iter().position(|p| *p == self.0)
        {
            failing.swap_remove(at);
        }
    }
}

/// Makes every write to the token file at `path` fail after its temporary
/// file is written, until the guard is dropped, so a test can check the
/// failure path and its cleanup.
#[cfg(any(test, feature = "test-hooks"))]
pub fn fail_token_writes(path: PathBuf) -> TokenWriteFailure {
    if let Ok(mut failing) = FAILING_WRITES.lock() {
        failing.push(path.clone());
    }
    TokenWriteFailure(path)
}

#[cfg(any(test, feature = "test-hooks"))]
fn write_fails(path: &Path) -> bool {
    FAILING_WRITES
        .lock()
        .is_ok_and(|failing| failing.iter().any(|p| p == path))
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
    fn set_if_stores_only_when_the_check_agrees() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = dir.path().join("tokens");
        let store = TokenStore::Dir(tokens.clone());
        let ours = key("https://hub.example.com");
        let files = || {
            let mut names: Vec<_> = fs::read_dir(&tokens)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        assert!(store.set_if(&ours, "first", || true).unwrap());
        assert_eq!(store.get(&ours).unwrap().as_deref(), Some("first"));
        // A refusal keeps the old token and leaves no staged file behind...
        assert!(!store.set_if(&ours, "second", || false).unwrap());
        assert_eq!(store.get(&ours).unwrap().as_deref(), Some("first"));
        // ...and stores nothing for a key that had no token.
        let theirs = key("https://evil.example");
        assert!(!store.set_if(&theirs, "refused", || false).unwrap());
        assert_eq!(store.get(&theirs).unwrap(), None);
        assert_eq!(files(), [OsString::from(ours.file_name())]);
        // The check runs once the new file is staged and before it replaces
        // the old token, which is the latest a store can run it.
        let staged = || {
            let found: Vec<_> = fs::read_dir(&tokens)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "tmp"))
                .collect();
            assert_eq!(found.len(), 1);
            found[0].clone()
        };
        assert!(
            store
                .set_if(&ours, "third", || {
                    assert_eq!(fs::read_to_string(staged()).unwrap(), "third");
                    assert_eq!(store.get(&ours).unwrap().as_deref(), Some("first"));
                    true
                })
                .unwrap()
        );
        assert_eq!(store.get(&ours).unwrap().as_deref(), Some("third"));
        assert_eq!(files(), [OsString::from(ours.file_name())]);
    }

    fn temp_files(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "tmp"))
            .collect()
    }

    #[test]
    fn a_failed_write_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = dir.path().join("tokens");
        let store = TokenStore::Dir(tokens.clone());
        let ours = key("https://hub.example.com");
        let _failure = fail_token_writes(tokens.join(ours.file_name()));
        assert!(store.set_if(&ours, "secret", || true).is_err());
        assert!(temp_files(&tokens).is_empty());
        assert_eq!(store.get(&ours).unwrap(), None);
    }

    /// A name source that hands out `names` in order.
    fn names(names: &[u64]) -> impl FnMut() -> io::Result<u64> + use<'_> {
        let mut names = names.iter();
        move || Ok(*names.next().expect("create_temp asked for too many names"))
    }

    fn temp_name(path: &Path, name: u64) -> PathBuf {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".{name:016x}.tmp"));
        PathBuf::from(tmp)
    }

    #[test]
    fn a_taken_temp_name_is_skipped_and_the_planted_file_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tok-aa");
        fs::write(temp_name(&path, 1), "keep").unwrap();
        let (mut file, tmp) = create_temp(&path, &mut names(&[1, 2])).unwrap();
        file.write_all(b"new").unwrap();
        assert_eq!(tmp, temp_name(&path, 2));
        assert_eq!(fs::read_to_string(temp_name(&path, 1)).unwrap(), "keep");
        assert_eq!(fs::read_to_string(tmp).unwrap(), "new");
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_link_at_a_temp_name_is_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tok-aa");
        let victim = dir.path().join("victim");
        fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, temp_name(&path, 1)).unwrap();
        // A link to a missing file is refused too: create_new never follows.
        std::os::unix::fs::symlink(dir.path().join("missing"), temp_name(&path, 2)).unwrap();
        let (_file, tmp) = create_temp(&path, &mut names(&[1, 2, 3])).unwrap();
        assert_eq!(tmp, temp_name(&path, 3));
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        assert!(!dir.path().join("missing").exists());
        assert!(
            fs::symlink_metadata(temp_name(&path, 1))
                .unwrap()
                .is_symlink()
        );
    }

    #[test]
    fn temp_names_give_up_after_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tok-aa");
        fs::write(temp_name(&path, 1), "keep").unwrap();
        let mut asked = 0;
        let error = create_temp(&path, &mut || {
            asked += 1;
            Ok(1)
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(asked, TEMP_NAME_TRIES);
        assert_eq!(fs::read_to_string(temp_name(&path, 1)).unwrap(), "keep");
    }

    #[test]
    fn two_writes_in_a_row_leave_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = dir.path().join("tokens");
        let store = TokenStore::Dir(tokens.clone());
        let ours = key("https://hub.example.com");
        assert!(store.set_if(&ours, "one", || true).unwrap());
        assert!(store.set_if(&ours, "two", || true).unwrap());
        assert_eq!(store.get(&ours).unwrap().as_deref(), Some("two"));
        assert!(temp_files(&tokens).is_empty());
    }

    #[test]
    fn a_failed_rename_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = dir.path().join("tokens");
        let store = TokenStore::Dir(tokens.clone());
        let ours = key("https://hub.example.com");
        // A directory where the token belongs makes the rename fail.
        fs::create_dir_all(tokens.join(ours.file_name())).unwrap();
        assert!(store.set_if(&ours, "secret", || true).is_err());
        assert!(temp_files(&tokens).is_empty());
    }

    #[test]
    fn a_panicking_check_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = dir.path().join("tokens");
        let store = TokenStore::Dir(tokens.clone());
        let ours = key("https://hub.example.com");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(store.set_if(&ours, "secret", || panic!("check failed")));
        }));
        assert!(result.is_err());
        assert!(temp_files(&tokens).is_empty());
    }

    #[test]
    fn delete_removes_that_keys_leftover_temp_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = dir.path().join("tokens");
        let store = TokenStore::Dir(tokens.clone());
        // A key whose file name is a prefix of the other's (names are `tok-`
        // and hex), so a loose match would delete across them.
        let short = TokenKey("https://hub.example.com idn7k2q".to_owned());
        let long = key("https://hub.example.com");
        assert!(long.file_name().starts_with(&short.file_name()));
        assert!(store.set_if(&short, "secret", || true).unwrap());
        let leftover =
            |key: &TokenKey, hex: &str| tokens.join(format!("{}.{hex}.tmp", key.file_name()));
        let mine = [
            leftover(&short, "0123456789abcdef"),
            leftover(&short, "fedcba9876543210"),
        ];
        let others = [
            leftover(&long, "0123456789abcdef"),
            // Not this writer's naming: wrong length or suffix.
            leftover(&short, "0123"),
            leftover(&short, "0123456789abcdeg"),
            tokens.join(format!("{}.0123456789abcdef.tmp.bak", short.file_name())),
        ];
        for path in mine.iter().chain(&others) {
            fs::write(path, "old").unwrap();
        }
        store.delete(&short).unwrap();
        assert_eq!(store.get(&short).unwrap(), None);
        assert!(mine.iter().all(|path| !path.exists()));
        assert!(others.iter().all(|path| path.exists()));
        // Deleting what is not there is still fine.
        store.delete(&short).unwrap();
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
