//! Login transactions share a lock with other processes.

#![cfg(test)]

mod support;

use std::fs::OpenOptions;
use std::time::Duration;

use agmen_client::{HumanTokens, Login, save_login};
use support::{Home, assert_exit, stderr};

const COMMAND_DEADLINE: Duration = Duration::from_secs(10);

#[tokio::test]
async fn logout_lock_timeout_preserves_the_login_and_tokens() {
    let home = Home::new();
    let file = home.login_file();
    let store = home.tokens();
    let login = Login {
        hub_url: "https://lock.example/".parse().unwrap(),
        human_id: "hmn7k2q9".parse().unwrap(),
        handle: "andy".into(),
    };
    let tokens = HumanTokens::new("test-access".into(), "test-refresh".into(), 3600);
    save_login(&store, &file, &login, &tokens).await.unwrap();
    let before = store.get(&login.key().unwrap()).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(file.with_file_name("login.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let out = tokio::time::timeout(
        COMMAND_DEADLINE,
        tokio::process::Command::from(home.agmen(home.path(), &["logout"]))
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("logout must stop waiting")
    .unwrap();
    assert!(
        !out.status.success(),
        "logout ignored another process's lock"
    );
    assert!(stderr(&out).contains("timed out"), "{}", stderr(&out));
    assert_eq!(Login::read(&file).unwrap(), Some(login.clone()));
    assert_eq!(store.get(&login.key().unwrap()).unwrap(), before);
    drop(lock);
    assert_exit(&home.run(home.path(), &["logout"]).await, 0);
    assert!(Login::read(&file).unwrap().is_none());
    assert!(store.get(&login.key().unwrap()).unwrap().is_none());
    assert!(file.with_file_name("login.lock").exists());
    assert_exit(&home.run(home.path(), &["logout"]).await, 0);
}

#[tokio::test]
async fn logout_with_an_unusable_lock_fails_and_changes_nothing() {
    let home = Home::new();
    let file = home.login_file();
    let store = home.tokens();
    let login = Login {
        hub_url: "https://lock.example/".parse().unwrap(),
        human_id: "hmn7k2q9".parse().unwrap(),
        handle: "andy".into(),
    };
    let tokens = HumanTokens::new("test-access".into(), "test-refresh".into(), 3600);
    save_login(&store, &file, &login, &tokens).await.unwrap();
    let before = store.get(&login.key().unwrap()).unwrap();
    // A directory cannot be opened for writing, so no one can lock it and no
    // one holds it: the lock is unusable, not busy.
    let lock = file.with_file_name("login.lock");
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();
    let out = home.run(home.path(), &["logout"]).await;
    assert!(!out.status.success(), "logout went on without its lock");
    assert!(stderr(&out).contains("login lock"), "{}", stderr(&out));
    assert_eq!(Login::read(&file).unwrap(), Some(login.clone()));
    assert_eq!(store.get(&login.key().unwrap()).unwrap(), before);
    std::fs::remove_dir(&lock).unwrap();
    assert_exit(&home.run(home.path(), &["logout"]).await, 0);
    assert!(Login::read(&file).unwrap().is_none());
    assert!(store.get(&login.key().unwrap()).unwrap().is_none());
}

/// A hub that must not be asked: its address, and the listener that sees any
/// connection.
fn silent_hub() -> (String, std::net::TcpListener) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    (
        format!("http://{}", listener.local_addr().unwrap()),
        listener,
    )
}

fn assert_never_asked(listener: &std::net::TcpListener) {
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "login asked the hub before it had the lock"
    );
}

#[tokio::test]
async fn login_with_an_unusable_lock_fails_before_the_hub_is_asked() {
    let home = Home::new();
    let file = home.login_file();
    let lock = file.with_file_name("login.lock");
    std::fs::create_dir_all(&lock).unwrap();
    let (hub, listener) = silent_hub();
    let out = home.run(home.path(), &["login", "--hub", &hub]).await;
    assert!(!out.status.success(), "login went on without its lock");
    let text = stderr(&out);
    assert!(text.contains(&lock.display().to_string()), "{text}");
    assert_eq!(text.matches("os error").count(), 1, "{text}");
    assert!(text.contains("owner"), "{text}");
    assert!(!text.contains("starting the device login"), "{text}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("Your code"));
    assert_never_asked(&listener);
    assert!(!file.exists());
}

#[tokio::test]
async fn login_with_a_busy_lock_fails_before_the_hub_is_asked() {
    let home = Home::new();
    let file = home.login_file();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(file.with_file_name("login.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let (hub, listener) = silent_hub();
    let out = tokio::time::timeout(
        COMMAND_DEADLINE,
        tokio::process::Command::from(home.agmen(home.path(), &["login", "--hub", &hub]))
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("login must stop waiting")
    .unwrap();
    assert!(
        !out.status.success(),
        "login ignored another process's lock"
    );
    let text = stderr(&out);
    assert!(text.contains("timed out"), "{text}");
    assert!(!text.contains("starting the device login"), "{text}");
    assert_never_asked(&listener);
    assert!(!file.exists());
}

#[tokio::test]
async fn logout_with_an_unusable_lock_names_the_os_error_and_what_to_check() {
    let home = Home::new();
    let file = home.login_file();
    let lock = file.with_file_name("login.lock");
    std::fs::create_dir_all(&lock).unwrap();
    let out = home.run(home.path(), &["logout"]).await;
    assert!(!out.status.success());
    let text = stderr(&out);
    assert!(text.contains(&lock.display().to_string()), "{text}");
    assert_eq!(text.matches("os error").count(), 1, "{text}");
    for check in ["owner", "permissions", "filesystem"] {
        assert!(text.contains(check), "{text}");
    }
}
