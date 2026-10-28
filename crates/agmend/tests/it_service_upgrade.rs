//! Real service-manager acceptance test. Run only in a disposable account.

#![cfg(test)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use agmen_local::{Outcome, Request, Response, frame};
use serde_json::{Value, json};

async fn command(program: &Path, args: &[&std::ffi::OsStr]) -> std::process::Output {
    tokio::time::timeout(
        Duration::from_secs(125),
        tokio::process::Command::new(program)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap()
}

async fn hello() -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut socket = tokio::net::UnixStream::connect(agmen_client::agmend_socket().unwrap())
            .await
            .unwrap();
        frame::write_async(&mut socket, &Request::new(1, "hello", json!({"api":1})))
            .await
            .unwrap();
        match frame::read_async::<Response>(&mut socket)
            .await
            .unwrap()
            .outcome
        {
            Outcome::Ok { result } => result,
            other => panic!("hello failed: {other:?}"),
        }
    })
    .await
    .unwrap()
}

async fn pid() -> u32 {
    #[cfg(target_os = "macos")]
    {
        let target = format!(
            "gui/{}/dev.agmen.agmend",
            rustix::process::getuid().as_raw()
        );
        let out = command(Path::new("launchctl"), &["print".as_ref(), target.as_ref()]).await;
        assert!(out.status.success());
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .find_map(|line| line.trim().strip_prefix("pid = "))
            .unwrap()
            .parse()
            .unwrap()
    }
    #[cfg(target_os = "linux")]
    {
        let out = command(
            Path::new("systemctl"),
            &[
                "--user".as_ref(),
                "show".as_ref(),
                "agmend.service".as_ref(),
                "--property=MainPID".as_ref(),
                "--value".as_ref(),
            ],
        )
        .await;
        assert!(out.status.success());
        String::from_utf8(out.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        // A bounded synchronous child can be reaped even during a test panic.
        let mut child = std::process::Command::new(&self.0)
            .args(["service", "uninstall"])
            .spawn()
            .unwrap();
        for _ in 0..1250 {
            if child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

#[tokio::test]
#[ignore = "requires two releases and an explicitly opted-in disposable service account"]
async fn it_upgrade_running_daemon_reports_new_version() {
    assert_eq!(
        std::env::var("AGMEN_DISPOSABLE_SERVICE_ACCOUNT").as_deref(),
        Ok("1")
    );
    assert_ne!(
        std::env::var("USER").as_deref(),
        Ok("andyaldrich"),
        "never use the developer account"
    );
    let home = std::env::home_dir().unwrap();
    assert_eq!(
        PathBuf::from(std::env::var_os("AGMEN_SERVICE_TEST_HOME").unwrap()),
        home
    );
    let old = PathBuf::from(std::env::var_os("AGMEN_OLD_AGMEND").unwrap());
    let new = PathBuf::from(std::env::var_os("AGMEN_NEW_AGMEND").unwrap());
    let version = std::env::var("AGMEN_NEW_VERSION").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let stable = dir.path().join("agmend");
    std::os::unix::fs::symlink(&old, &stable).unwrap();
    let status = command(
        &new,
        &["service".as_ref(), "status".as_ref(), "--json".as_ref()],
    )
    .await;
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(
        status["state"], "not installed",
        "disposable account must start without a service"
    );
    assert!(status["program"].is_null());
    let _cleanup = Cleanup(new.clone());
    let installed = command(
        &old,
        &[
            "service".as_ref(),
            "install".as_ref(),
            "--program".as_ref(),
            stable.as_os_str(),
        ],
    )
    .await;
    assert!(installed.status.success(), "{installed:?}");
    let before = pid().await;
    assert_ne!(before, 0);
    assert_ne!(hello().await["version"], version);
    let replacement = dir.path().join("replacement");
    std::os::unix::fs::symlink(&new, &replacement).unwrap();
    std::fs::rename(replacement, &stable).unwrap();
    let upgraded = command(
        &new,
        &[
            "service".as_ref(),
            "install".as_ref(),
            "--program".as_ref(),
            stable.as_os_str(),
        ],
    )
    .await;
    assert!(upgraded.status.success(), "{upgraded:?}");
    let after = pid().await;
    assert_ne!(after, 0);
    assert_ne!(before, after);
    assert_eq!(hello().await["version"], version);
}
