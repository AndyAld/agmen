use std::fs::{File, OpenOptions};
use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::post;

use super::*;
use crate::token_store::fake_keychain::FakeKeychain;

const PENDING_WINDOW: Duration = Duration::from_millis(150);
const TEST_DEADLINE: Duration = Duration::from_secs(8);

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
    logged_at(tracing::Level::WARN, dir)
}

/// The events at `level` logged about files under `dir`, each as its fields.
fn logged_at(level: tracing::Level, dir: &Path) -> Vec<String> {
    let dir = dir.display().to_string();
    let logged = LOGGED.lock().unwrap();
    logged
        .iter()
        .filter(|(at, fields)| *at == level && fields.contains(&dir))
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

/// Holds `login.lock` beside `file` for `time` on another thread.
fn hold_for(file: &Path, time: Duration) -> std::thread::JoinHandle<()> {
    let guard = hold(file);
    std::thread::spawn(move || {
        std::thread::sleep(time);
        drop(guard);
    })
}

#[tokio::test]
async fn login_waits_longer_than_a_renewal_for_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().join("tokens"));
    let file = dir.path().join("login.json");
    let holder = hold_for(&file, Duration::from_secs(2));
    save_login(&store, &file, &login(), &tokens())
        .await
        .expect("a lock freed after 2 s is within a login's wait");
    holder.join().unwrap();
    assert_eq!(Login::read(&file).unwrap(), Some(login()));
}

#[tokio::test]
async fn logout_waits_longer_than_a_renewal_for_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().join("tokens"));
    let file = dir.path().join("login.json");
    save_login(&store, &file, &login(), &tokens())
        .await
        .unwrap();
    let holder = hold_for(&file, Duration::from_secs(2));
    let gone = Login::forget(&store, &file)
        .await
        .expect("a lock freed after 2 s is within a logout's wait");
    holder.join().unwrap();
    assert_eq!(gone, Some(login()));
}

#[tokio::test]
async fn login_gives_up_on_a_busy_lock_after_five_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().join("tokens"));
    let file = dir.path().join("login.json");
    let _guard = hold(&file);
    let start = Instant::now();
    let error = save_login(&store, &file, &login(), &tokens())
        .await
        .unwrap_err();
    assert!(start.elapsed() >= Duration::from_secs(5), "{error}");
    assert!(error.to_string().contains("after 5s"), "{error}");
}

#[tokio::test]
async fn check_login_lock_fails_a_busy_lock_and_leaves_no_login() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("login.json");
    let _guard = hold(&file);
    let error = tokio::time::timeout(TEST_DEADLINE, check_login_lock(&file))
        .await
        .expect("the check must stop waiting")
        .expect_err("a busy lock fails the check");
    assert!(matches!(error, LoginError::LockTimeout { .. }), "{error}");
    assert!(!file.exists());
}

#[tokio::test]
async fn check_login_lock_fails_an_unusable_lock() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("login.json");
    make_lock_unusable(&file);
    let error = check_login_lock(&file).await.unwrap_err();
    assert!(matches!(error, LoginError::LockUnusable { .. }), "{error}");
}

#[tokio::test]
async fn check_login_lock_lets_go_of_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("login.json");
    check_login_lock(&file).await.unwrap();
    // Held across the browser step, the check would keep this out.
    let _again = hold(&file);
    assert!(!file.exists(), "the check wrote the login file");
}

/// A token endpoint that renews any refresh token.
async fn renewing_hub() -> Url {
    let app = Router::new().route(
        "/v1/auth/token",
        post(|| async {
            (
                [("content-type", "application/json")],
                r#"{"access_token":"test-renewed-a","refresh_token":"test-renewed-r","expires_in":3600}"#,
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}").parse().unwrap()
}

/// A person whose access token is about to run out, signed in while the
/// lock worked.
async fn expiring_sign_in() -> (tempfile::TempDir, TokenStore, PathBuf, Login) {
    let dir = tempfile::tempdir().unwrap();
    let store = TokenStore::Dir(dir.path().join("tokens"));
    let file = dir.path().join("login.json");
    let login = Login {
        hub_url: renewing_hub().await,
        ..login()
    };
    let old = HumanTokens::new("test-old-a".into(), "test-old-r".into(), 30);
    save_login(&store, &file, &login, &old).await.unwrap();
    (dir, store, file, login)
}

#[tokio::test]
async fn a_renewal_dropped_for_a_busy_lock_says_the_lock_was_busy() {
    capture_logs();
    let (dir, store, file, login) = expiring_sign_in().await;
    let _guard = hold(&file);
    let error = human_tokens(&reqwest::Client::new(), &store, &file, &login)
        .await
        .unwrap_err();
    assert!(matches!(error, LoginError::NotSignedIn), "{error:?}");
    let notes = logged_at(tracing::Level::WARN, dir.path());
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("busy"), "{notes:?}");
    assert!(!notes[0].contains("sign-in changed"), "{notes:?}");
    assert!(!notes[0].contains("test-renewed"), "{notes:?}");
}

#[tokio::test]
async fn a_renewal_dropped_for_a_changed_login_still_says_so() {
    capture_logs();
    let (dir, store, file, login) = expiring_sign_in().await;
    std::fs::remove_file(&file).unwrap();
    let error = human_tokens(&reqwest::Client::new(), &store, &file, &login)
        .await
        .unwrap_err();
    assert!(matches!(error, LoginError::NotSignedIn), "{error:?}");
    let notes = logged_at(tracing::Level::INFO, dir.path());
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("sign-in changed"), "{notes:?}");
    assert!(!notes[0].contains("busy"), "{notes:?}");
}

#[tokio::test]
async fn an_unusable_lock_warns_on_the_first_fallback_and_logs_repeats_at_debug() {
    capture_logs();
    let (dir, store, file) = signed_in_with_unusable_lock().await;
    for _ in 0..3 {
        assert!(
            store_renewed(&store, &file, &login(), &tokens())
                .await
                .unwrap()
        );
    }
    let warnings = logged_about(dir.path());
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let repeats = logged_at(tracing::Level::DEBUG, dir.path());
    assert_eq!(repeats.len(), 2, "{repeats:?}");
    assert!(
        repeats.iter().all(|r| r.contains("login.lock")),
        "{repeats:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_changed_lock_error_warns_again_and_so_does_a_break_after_a_repair() {
    capture_logs();
    let (dir, store, file) = signed_in_with_unusable_lock().await;
    let lock = file.with_file_name("login.lock");
    let renew = || async {
        assert!(
            store_renewed(&store, &file, &login(), &tokens())
                .await
                .unwrap()
        );
    };
    renew().await;
    renew().await;
    assert_eq!(logged_about(dir.path()).len(), 1);
    // A different failure: the path now leads nowhere, not into a directory.
    std::fs::remove_dir(&lock).unwrap();
    std::os::unix::fs::symlink(dir.path().join("missing/login.lock"), &lock).unwrap();
    renew().await;
    renew().await;
    assert_eq!(logged_about(dir.path()).len(), 2);
    // The lock works again, and then breaks as before: a new warning.
    std::fs::remove_file(&lock).unwrap();
    renew().await;
    assert_eq!(logged_about(dir.path()).len(), 2);
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();
    renew().await;
    assert_eq!(logged_about(dir.path()).len(), 3);
}

#[tokio::test]
async fn an_unusable_lock_error_names_the_path_the_os_error_and_what_to_check() {
    let (dir, store, file) = signed_in_with_unusable_lock().await;
    let lock = dir.path().join("login.lock");
    let login_error = save_login(&store, &file, &login(), &tokens())
        .await
        .unwrap_err();
    let logout_error = Login::forget(&store, &file).await.unwrap_err();
    for error in [login_error, logout_error] {
        let text = error.to_string();
        assert!(text.contains(&lock.display().to_string()), "{text}");
        assert!(text.contains("os error"), "{text}");
        for check in ["owner", "permissions", "filesystem"] {
            assert!(text.contains(check), "{text}");
        }
        assert!(text.contains("nothing was changed"), "{text}");
        // The OS error is in the message once, so `{:#}` does not repeat it.
        assert!(std::error::Error::source(&error).is_none(), "{text}");
    }
}

#[test]
fn fell_back_caps_its_memory_and_clears_on_overflow() {
    let dir = tempfile::tempdir().unwrap();
    let error = std::io::Error::other("cap-test");
    let paths: Vec<_> = (0..9).map(|n| dir.path().join(format!("cap{n}"))).collect();
    for path in &paths[..8] {
        assert!(lock::fell_back(path, &error));
    }
    for path in &paths[..8] {
        assert!(
            !lock::fell_back(path, &error),
            "a repeat inside the cap warns"
        );
    }
    // A ninth path overflows the cap and clears the rest.
    assert!(lock::fell_back(&paths[8], &error));
    assert!(
        lock::fell_back(&paths[0], &error),
        "cleared entries warn again"
    );
}

#[test]
fn a_busy_lock_error_names_login_lock_as_local() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("login.json");
    let _guard = hold(&file);
    let error = lock::LoginLock::hold(&file, Duration::from_millis(20)).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("local login.lock"), "{text}");
    assert!(!text.contains('\n'), "{text}");
}

#[tokio::test]
async fn an_unusable_lock_error_names_login_lock_as_local() {
    let (_dir, store, file) = signed_in_with_unusable_lock().await;
    let error = save_login(&store, &file, &login(), &tokens())
        .await
        .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("local login.lock"), "{text}");
    assert!(!text.contains('\n'), "{text}");
}
