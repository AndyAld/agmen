use std::fs::{File, OpenOptions};
use std::time::Duration;

use super::*;
use crate::token_store::fake_keychain::FakeKeychain;

const PENDING_WINDOW: Duration = Duration::from_millis(150);
const TEST_DEADLINE: Duration = Duration::from_secs(4);

fn login() -> Login {
    Login {
        hub_url: "https://lock.example/".parse().unwrap(),
        human_id: "hmn7k2q9".parse().unwrap(),
        handle: "andy".into(),
    }
}

fn tokens() -> HumanTokens {
    HumanTokens::new("test-access".into(), "test-refresh".into(), 3600)
}

fn hold(file: &Path) -> File {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(file.with_file_name("login.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    lock
}

async fn renewal_during_change(store: TokenStore, file: PathBuf, replace: bool) {
    let old = login();
    save_login(&store, &file, &old, &tokens()).await.unwrap();
    // A logout or account replacement has the lock but has not yet changed
    // the login or tokens. Renewal must wait before it checks either one.
    let guard = hold(&file);
    let mut renewal = tokio::spawn({
        let (store, file, old) = (store.clone(), file.clone(), old.clone());
        async move { store_renewed(&store, &file, &old, &tokens()).await }
    });
    let early = tokio::time::timeout(PENDING_WINDOW, &mut renewal).await;
    let pending = early.is_err();
    store.delete(&old.key().unwrap()).unwrap();
    let mut new = old.clone();
    new.human_id = "hmn0new1".parse().unwrap();
    if replace {
        store
            .set(
                &new.key().unwrap(),
                &serde_json::to_string(&tokens()).unwrap(),
            )
            .unwrap();
        std::fs::write(&file, serde_json::to_vec(&new).unwrap()).unwrap();
    } else {
        std::fs::remove_file(&file).unwrap();
    }
    drop(guard);
    let stored = match early {
        Err(_) => tokio::time::timeout(TEST_DEADLINE, renewal)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Ok(result) => result.unwrap().unwrap(),
    };
    assert!(
        pending,
        "renewal wrote while the login change held login.lock"
    );
    assert!(!stored, "renewal stored a pair for the previous login");
    assert!(store.get(&old.key().unwrap()).unwrap().is_none());
    assert_eq!(Login::read(&file).unwrap(), replace.then_some(new));
}

#[tokio::test]
async fn renewal_waits_for_logout_lock() {
    let dir = tempfile::tempdir().unwrap();
    renewal_during_change(
        TokenStore::Dir(dir.path().join("tokens")),
        dir.path().join("login.json"),
        false,
    )
    .await;
}

#[tokio::test]
async fn renewal_waits_for_account_login_lock() {
    let dir = tempfile::tempdir().unwrap();
    renewal_during_change(
        TokenStore::Dir(dir.path().join("tokens")),
        dir.path().join("login.json"),
        true,
    )
    .await;
}

#[tokio::test]
async fn keychain_renewal_waits_for_logout_lock() {
    let dir = tempfile::tempdir().unwrap();
    let _fake = FakeKeychain::new(login().key().unwrap());
    renewal_during_change(TokenStore::Keychain, dir.path().join("login.json"), false).await;
}

#[tokio::test]
async fn renewal_lock_timeout_drops_the_pair() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().join("tokens"));
    let file = dir.path().join("login.json");
    let login = login();
    save_login(&store, &file, &login, &tokens()).await.unwrap();
    let before = store.get(&login.key().unwrap()).unwrap();
    let _guard = hold(&file);
    let stored = tokio::time::timeout(
        TEST_DEADLINE,
        store_renewed(&store, &file, &login, &tokens()),
    )
    .await
    .expect("renewal must stop waiting")
    .unwrap();
    assert!(!stored, "a held lock must make renewal skip its write");
    assert_eq!(store.get(&login.key().unwrap()).unwrap(), before);
}

#[tokio::test]
async fn login_lock_timeout_fails_without_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().join("tokens"));
    let file = dir.path().join("login.json");
    let _guard = hold(&file);
    let result = tokio::time::timeout(
        TEST_DEADLINE,
        save_login(&store, &file, &login(), &tokens()),
    )
    .await
    .expect("login must stop waiting");
    let error = result.expect_err("login must respect login.lock");
    assert!(error.to_string().contains("timed out"), "{error}");
    assert!(!file.exists());
    assert!(store.get(&login().key().unwrap()).unwrap().is_none());
}

#[tokio::test]
async fn logout_lock_timeout_fails_without_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().join("tokens"));
    let file = dir.path().join("login.json");
    save_login(&store, &file, &login(), &tokens())
        .await
        .unwrap();
    let before = store.get(&login().key().unwrap()).unwrap();
    let guard = hold(&file);
    let result = tokio::time::timeout(TEST_DEADLINE, Login::forget(&store, &file))
        .await
        .expect("logout must stop waiting");
    assert!(matches!(result, Err(LoginError::LockTimeout { .. })));
    assert_eq!(Login::read(&file).unwrap(), Some(login()));
    assert_eq!(store.get(&login().key().unwrap()).unwrap(), before);
    drop(guard);
    assert_eq!(Login::forget(&store, &file).await.unwrap(), Some(login()));
    assert!(store.get(&login().key().unwrap()).unwrap().is_none());
    assert!(Login::forget(&store, &file).await.unwrap().is_none());
}

/// Makes `login.lock` beside `file` a directory, which cannot be opened for
/// writing, so no lock can be taken and nobody holds it either.
fn make_lock_unusable(file: &Path) {
    let lock = file.with_file_name("login.lock");
    if lock.exists() {
        std::fs::remove_file(&lock).unwrap();
    }
    std::fs::create_dir(&lock).unwrap();
}

/// A person who signed in while the lock worked, then lost it.
async fn signed_in_with_unusable_lock() -> (tempfile::TempDir, TokenStore, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().join("tokens"));
    let file = dir.path().join("login.json");
    save_login(&store, &file, &login(), &tokens())
        .await
        .unwrap();
    make_lock_unusable(&file);
    (dir, store, file)
}

#[tokio::test]
async fn unusable_login_lock_fails_a_first_login_without_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().join("tokens"));
    let file = dir.path().join("login.json");
    make_lock_unusable(&file);
    let error = save_login(&store, &file, &login(), &tokens())
        .await
        .expect_err("login needs the lock");
    assert!(matches!(error, LoginError::LockUnusable { .. }), "{error}");
    assert!(!file.exists());
    assert!(store.get(&login().key().unwrap()).unwrap().is_none());
}

#[tokio::test]
async fn unusable_login_lock_fails_an_account_switch_without_changes() {
    let (_dir, store, file) = signed_in_with_unusable_lock().await;
    let (old, before) = (login(), std::fs::read(&file).unwrap());
    let mut other = old.clone();
    other.human_id = "hmn0new1".parse().unwrap();
    let error = save_login(&store, &file, &other, &tokens())
        .await
        .expect_err("login needs the lock");
    assert!(matches!(error, LoginError::LockUnusable { .. }), "{error}");
    assert_eq!(std::fs::read(&file).unwrap(), before);
    assert!(store.get(&old.key().unwrap()).unwrap().is_some());
    assert!(store.get(&other.key().unwrap()).unwrap().is_none());
}

#[tokio::test]
async fn unusable_login_lock_fails_logout_without_changes() {
    let (_dir, store, file) = signed_in_with_unusable_lock().await;
    let before = store.get(&login().key().unwrap()).unwrap();
    let error = Login::forget(&store, &file)
        .await
        .expect_err("logout needs the lock");
    assert!(matches!(error, LoginError::LockUnusable { .. }), "{error}");
    assert!(error.to_string().contains("login.lock"), "{error}");
    assert_eq!(Login::read(&file).unwrap(), Some(login()));
    assert_eq!(store.get(&login().key().unwrap()).unwrap(), before);
}

#[tokio::test]
async fn renewal_with_an_unusable_lock_stores_through_the_recheck_and_warns_once() {
    capture_logs();
    let (dir, store, file) = signed_in_with_unusable_lock().await;
    let renewed = HumanTokens::new("test-renewed-a".into(), "test-renewed-r".into(), 3600);
    assert!(
        store_renewed(&store, &file, &login(), &renewed)
            .await
            .unwrap()
    );
    let stored = store.get(&login().key().unwrap()).unwrap().unwrap();
    let stored: HumanTokens = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored.access_token(), "test-renewed-a");
    let warnings = logged_about(dir.path());
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("login.lock"), "{warnings:?}");
    assert!(!warnings[0].contains("test-renewed"), "{warnings:?}");
}

#[tokio::test]
async fn renewal_with_an_unusable_lock_still_drops_a_pair_for_a_changed_login() {
    capture_logs();
    for replace in [false, true] {
        let (dir, store, file) = signed_in_with_unusable_lock().await;
        let old = login();
        store.delete(&old.key().unwrap()).unwrap();
        let mut new = old.clone();
        new.human_id = "hmn0new1".parse().unwrap();
        if replace {
            std::fs::write(&file, serde_json::to_vec(&new).unwrap()).unwrap();
        } else {
            std::fs::remove_file(&file).unwrap();
        }
        let stored = store_renewed(&store, &file, &old, &tokens()).await.unwrap();
        assert!(!stored, "the login-file recheck must still run");
        assert!(store.get(&old.key().unwrap()).unwrap().is_none());
        assert_eq!(logged_about(dir.path()).len(), 1);
    }
}

/// Every event any thread logs: the code under test logs on the blocking
/// pool, where a scoped subscriber does not reach.
static LOGGED: std::sync::Mutex<Vec<(tracing::Level, String)>> = std::sync::Mutex::new(Vec::new());

struct Capture;

struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{}={value:?} ", field.name()));
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        LOGGED
            .lock()
            .unwrap()
            .push((*event.metadata().level(), fields.0));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn capture_logs() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| tracing::subscriber::set_global_default(Capture).unwrap());
}

/// The warnings logged about files under `dir`, each as its fields.
fn logged_about(dir: &Path) -> Vec<String> {
    let dir = dir.display().to_string();
    let logged = LOGGED.lock().unwrap();
    logged
        .iter()
        .filter(|(level, fields)| *level == tracing::Level::WARN && fields.contains(&dir))
        .map(|(_, fields)| fields.clone())
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn login_lock_is_private_and_keeps_its_inode() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().join("tokens"));
    let file = dir.path().join("config/login.json");
    let path = file.with_file_name("login.lock");
    save_login(&store, &file, &login(), &tokens())
        .await
        .unwrap();
    let metadata = std::fs::metadata(&path).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    Login::forget(&store, &file).await.unwrap();
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(after.permissions().mode() & 0o777, 0o600);
    assert_eq!(after.ino(), metadata.ino());
    let _guard = hold(&file);
}

#[cfg(unix)]
#[tokio::test]
async fn logout_file_removal_error_keeps_its_diagnostic() {
    use std::os::unix::fs::PermissionsExt as _;
    // Root can unlink through a read-only directory, so it cannot exercise
    // this filesystem error.
    if rustix::process::geteuid().as_raw() == 0 {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().join("tokens"));
    let config = dir.path().join("config");
    let file = config.join("login.json");
    save_login(&store, &file, &login(), &tokens())
        .await
        .unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = Login::forget(&store, &file).await;
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
    let error = result.unwrap_err();
    assert_eq!(error.to_string(), format!("removing {}", file.display()));
}
