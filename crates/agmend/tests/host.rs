//! Host mode uses temporary configuration and fake child processes only.
#![cfg(test)]

mod support;

use serde_json::json;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;
use support::{Daemon, result};

#[test]
fn host_init_writes_private_config_and_refuses_overwrite() {
    let home = tempfile::tempdir().unwrap();
    let env = home.path().join("hub.env");
    std::fs::write(&env, "AGMEN_PUBLIC_URL=https://host.example\n").unwrap();
    std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o600)).unwrap();
    let run = |force: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agmend"));
        command
            .args(["host", "init", "--env-file"])
            .arg(&env)
            .args(["--hub-bin", "/fake/agmen-hub"])
            .env("HOME", home.path())
            .env("XDG_STATE_HOME", home.path());
        if force {
            command.arg("--force");
        }
        command.output().unwrap()
    };
    let first = run(false);
    assert!(first.status.success(), "{first:?}");
    let state = if cfg!(target_os = "macos") {
        home.path().join("Library/Application Support/agmen")
    } else {
        home.path().join("agmen")
    };
    let config = state.join("host.toml");
    assert_eq!(
        std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let before = std::fs::read(&config).unwrap();
    assert!(!run(false).status.success());
    assert_eq!(std::fs::read(&config).unwrap(), before);
    assert!(run(true).status.success());
}

#[tokio::test]
async fn host_status_over_local_api_without_host_config() {
    let daemon = Daemon::start().await;
    let mut client = daemon.client().await;
    assert_eq!(
        result(client.call("host_status", json!({})).await),
        json!({
            "configured": false, "state": "stopped", "last_error": null, "public_url": null
        })
    );
    for method in ["host_status", "host_restart"] {
        assert_eq!(
            support::error(client.call(method, json!({"extra": true})).await).code,
            agmen_local::ErrorCode::BadRequest
        );
    }
    assert_eq!(
        support::error(client.call("host_restart", json!({})).await).code,
        agmen_local::ErrorCode::BadRequest
    );
    daemon.stop().await;
}

#[tokio::test]
async fn host_config_parses_and_refuses_group_or_world_access() {
    let mut daemon = Daemon::start().await;
    daemon.pause().await;
    let config = daemon.home.dir.path().join("host.toml");
    std::fs::write(&config, "env_file = '/does/not/exist'\npublic_url = 'https://host.example'\nhub_bin = '/missing/hub'\n").unwrap();
    for mode in [0o644, 0o640, 0o604] {
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(mode)).unwrap();
        daemon.resume();
        let mut client = daemon.client().await;
        let status = result(client.call("host_status", json!({})).await);
        assert_eq!(status["state"], "failed");
        assert!(
            status["last_error"].as_str().unwrap().contains("0600"),
            "{status}"
        );
        daemon.pause().await;
    }
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    daemon.resume();
    let mut client = daemon.client().await;
    for _ in 0..100 {
        if result(client.call("host_status", json!({})).await)["state"] == "failed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let status = result(client.call("host_status", json!({})).await);
    assert!(
        status["last_error"].as_str().unwrap().contains("env file"),
        "{status}"
    );
    daemon.stop().await;
}

#[tokio::test]
async fn supervisor_starts_restarts_and_stops_fake_hub() {
    let mut daemon = Daemon::start().await;
    daemon.pause().await;
    let base = daemon.home.dir.path();
    let env = base.join("hub.env");
    std::fs::write(&env, "AGMEN_LISTEN=127.0.0.1:9876\n").unwrap();
    std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o600)).unwrap();
    let script = base.join("hub");
    std::fs::write(
        &script,
        "#!/bin/sh\necho fake-hub-started\nexec /bin/sleep 120\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = base.join("host.toml");
    std::fs::write(
        &config,
        format!(
            "env_file = {:?}\npublic_url = 'https://host.example'\nhub_bin = {:?}\n",
            env, script
        ),
    )
    .unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    daemon.resume();
    let mut client = daemon.client().await;
    for _ in 0..100 {
        let status = result(client.call("host_status", json!({})).await);
        if status["state"] == "running"
            && std::fs::read_to_string(daemon.home.dir.path().join("hub.log"))
                .unwrap_or_default()
                .contains("fake-hub-started")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        result(client.call("host_status", json!({})).await)["state"],
        "running"
    );
    result(client.call("host_restart", json!({})).await);
    for _ in 0..100 {
        if std::fs::read_to_string(daemon.home.dir.path().join("hub.log"))
            .unwrap_or_default()
            .matches("fake-hub-started")
            .count()
            >= 2
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        std::fs::read_to_string(daemon.home.dir.path().join("hub.log"))
            .unwrap()
            .matches("fake-hub-started")
            .count(),
        2
    );
    daemon.pause().await;
    daemon.stop().await;
}

#[tokio::test]
async fn cancelling_daemon_serving_future_stops_its_fake_hub() {
    let home = support::Home::signed_in("http://127.0.0.1:9/").await;
    let base = home.dir.path();
    let script = base.join("hub");
    let pid_file = base.join("pid");
    std::fs::write(
        &script,
        format!("#!/bin/sh\necho $$ > {:?}\nexec /bin/sleep 120\n", pid_file),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let env = base.join("hub.env");
    std::fs::write(&env, "AGMEN_LISTEN=127.0.0.1:9876\n").unwrap();
    std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = base.join("host.toml");
    std::fs::write(
        &config,
        format!(
            "env_file = {:?}\npublic_url = 'https://host.example'\nhub_bin = {:?}\n",
            env, script
        ),
    )
    .unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let task = tokio::spawn(agmend::Agmend::bind(home.config()).unwrap().serve());
    let mut pid = None;
    for _ in 0..200 {
        if let Ok(text) = std::fs::read_to_string(&pid_file)
            && let Ok(raw) = text.trim().parse::<i32>()
        {
            pid = rustix::process::Pid::from_raw(raw);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let pid = pid.expect("fake hub starts");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    for _ in 0..200 {
        if rustix::process::test_kill_process(pid).is_err() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("fake hub survived cancellation of daemon serving future");
}
