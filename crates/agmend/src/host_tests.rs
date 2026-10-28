use super::*;

#[derive(Default)]
struct FakeRunner {
    status: Value,
    calls: Vec<Vec<String>>,
}

impl CommandRunner for FakeRunner {
    async fn output(&mut self, args: &[&str]) -> Result<Value> {
        self.calls
            .push(args.iter().map(|arg| (*arg).to_owned()).collect());
        Ok(self.status.clone())
    }
}

#[tokio::test]
async fn tailscale_is_opt_in_and_existing_https_proxy_is_preserved() {
    let target = "http://127.0.0.1:8080";
    let mut runner = FakeRunner::default();
    setup_tailscale(false, target, &mut runner).await.unwrap();
    assert!(runner.calls.is_empty());
    runner.status = json!({"Web":{"host.example:443":{"Handlers":{"/":{"Proxy":target}}}}});
    setup_tailscale(true, target, &mut runner).await.unwrap();
    assert_eq!(runner.calls, [vec!["serve", "status", "--json"]]);
    runner.calls.clear();
    runner.status = json!({"Foreground":{"session":{"Web":{"host.example:443":{"Handlers":{"/":{"Proxy":target}}}}}}});
    setup_tailscale(true, target, &mut runner).await.unwrap();
    assert_eq!(runner.calls, [vec!["serve", "status", "--json"]]);
    runner.calls.clear();
    runner.status = json!({"Web":{"host.example:8443":{"Handlers":{"/":{"Proxy":target}}}}});
    setup_tailscale(true, target, &mut runner).await.unwrap();
    assert_eq!(
        runner.calls,
        [
            vec!["serve", "status", "--json"],
            vec!["serve", "--bg", "--https=443", target]
        ]
    );
}

async fn wait_state(host: &Host, state: &str) {
    for _ in 0..200 {
        if host.status()["state"] == state {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("host never reached {state}: {}", host.status());
}

fn config_for(dir: &Path) -> HostConfig {
    let script = dir.join("hub");
    fs::write(
        &script,
        "#!/bin/sh\necho $$ > \"$AGMEN_TEST_PID_FILE\"\nexec /bin/sleep 120\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    HostConfig {
        hub_bin: Some(script.clone()),
        env_file: dir.join("hub.env"),
        extra_env_files: Vec::new(),
        public_url: "https://host.example".into(),
        tailscale_serve: false,
    }
}

fn write_env(dir: &Path) {
    write_private(
        &dir.join("hub.env"),
        &format!(
            "AGMEN_TEST_PID_FILE={:?}\nAGMEN_LISTEN=127.0.0.1:9876\n",
            dir.join("pid")
        ),
    );
}

fn write_private(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

async fn wait_pid(path: &Path) -> rustix::process::Pid {
    for _ in 0..200 {
        if let Ok(text) = fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse::<i32>()
            && let Some(pid) = rustix::process::Pid::from_raw(pid)
        {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("fake child never wrote its PID");
}

async fn assert_stopped(pid: rustix::process::Pid) {
    for _ in 0..200 {
        if rustix::process::test_kill_process(pid).is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("fake child survived shutdown");
}

#[tokio::test]
async fn config_created_after_daemon_start_is_discovered_and_child_is_reaped() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::new(dir.path(), dir.path().join("hub.log"));
    let task = tokio::spawn(host.clone().supervise());
    assert_eq!(host.status()["configured"], false);
    config_for(dir.path()).write(dir.path(), false).unwrap();
    write_env(dir.path());
    assert_eq!(host.status()["configured"], true);
    wait_state(&host, "running").await;
    let pid = wait_pid(&dir.path().join("pid")).await;
    host.stop();
    task.await.unwrap();
    assert_stopped(pid).await;
}

#[tokio::test]
async fn restart_reloads_repaired_configuration_and_environment() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_for(dir.path());
    let path = config.write(dir.path(), false).unwrap();
    fs::write(&path, "broken = [").unwrap();
    let host = Host::new(dir.path(), dir.path().join("hub.log"));
    let task = tokio::spawn(host.clone().supervise());
    wait_state(&host, "failed").await;
    config.write(dir.path(), true).unwrap();
    host.restart().unwrap();
    wait_state(&host, "failed").await;
    assert!(
        host.status()["last_error"]
            .as_str()
            .unwrap()
            .contains("env file")
    );
    write_env(dir.path());
    host.restart().unwrap();
    wait_state(&host, "running").await;
    host.stop();
    task.await.unwrap();
}

#[tokio::test]
async fn shutdown_cancels_env_loading_and_reaps_reader() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_for(dir.path());
    config.write(dir.path(), false).unwrap();
    write_private(
        &config.env_file,
        &format!(
            "echo $$ > {:?}\nexec /bin/sleep 120\n",
            dir.path().join("reader-pid")
        ),
    );
    let host = Host::new(dir.path(), dir.path().join("hub.log"));
    let task = tokio::spawn(host.clone().supervise());
    let pid = wait_pid(&dir.path().join("reader-pid")).await;
    host.stop();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert_stopped(pid).await;
}

struct BlockedRunner(Arc<Notify>);

impl CommandRunner for BlockedRunner {
    async fn output(&mut self, _args: &[&str]) -> Result<Value> {
        self.0.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn shutdown_cancels_tailscale_setup_without_starting_hub() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config_for(dir.path());
    config.tailscale_serve = true;
    let host = Host::new(dir.path(), dir.path().join("hub.log"));
    let entered = Arc::new(Notify::new());
    let mut runner = BlockedRunner(entered.clone());
    let running = host.clone();
    let task = tokio::spawn(async move {
        running
            .run(
                &config,
                &BTreeMap::from([("AGMEN_LISTEN".into(), "127.0.0.1:9876".into())]),
                &mut runner,
            )
            .await
    });
    entered.notified().await;
    host.stop();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    );
    assert!(!dir.path().join("hub.log").exists());
}

#[test]
fn bundled_default_and_invalid_toml_are_checked() {
    assert_eq!(
        bundled_hub(Path::new("/Applications/agmen.app/Contents/MacOS/agmend")).unwrap(),
        Path::new("/Applications/agmen.app/Contents/MacOS/agmen-hub")
    );
    assert!(bundled_hub(Path::new("/usr/local/bin/agmend")).is_err());
    assert!(bundled_hub(Path::new("/tmp/not-app/Contents/MacOS/agmend")).is_err());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("host.toml");
    fs::write(&path, "invalid = [").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        HostConfig::load(&path)
            .unwrap_err()
            .to_string()
            .contains("invalid host.toml")
    );
}

#[tokio::test]
async fn crashing_hub_gets_five_restarts_then_reports_its_error() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("hub");
    fs::write(
        &script,
        "#!/bin/sh\necho postgres-unavailable >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let env = dir.path().join("hub.env");
    write_private(&env, "AGMEN_LISTEN=127.0.0.1:8080\n");
    HostConfig {
        hub_bin: Some(script.clone()),
        env_file: env,
        extra_env_files: Vec::new(),
        public_url: "https://host.example".into(),
        tailscale_serve: false,
    }
    .write(dir.path(), false)
    .unwrap();
    let host = Host::new(dir.path(), dir.path().join("hub.log"));
    let task = tokio::spawn(host.clone().supervise());
    tokio::time::timeout(Duration::from_secs(45), async {
        for _ in 0..450 {
            if host.status()["last_error"]
                .as_str()
                .is_some_and(|error| error.contains("5 restarts"))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("restart budget never exhausted: {}", host.status());
    })
    .await
    .unwrap();
    assert_eq!(host.status()["state"], "failed");
    assert!(
        host.status()["last_error"]
            .as_str()
            .unwrap()
            .contains("postgres-unavailable")
    );
    let before = fs::read_to_string(dir.path().join("hub.log")).unwrap();
    assert_eq!(before.matches("postgres-unavailable").count(), 6);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        fs::read_to_string(dir.path().join("hub.log")).unwrap(),
        before
    );
    fs::write(&script, "#!/bin/sh\nexec /bin/sleep 120\n").unwrap();
    host.restart().unwrap();
    wait_state(&host, "running").await;
    host.stop();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(host.status()["state"], "stopped");
}

#[tokio::test]
async fn spawn_failures_are_also_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config_for(dir.path());
    config.hub_bin = Some(dir.path().join("missing-hub"));
    write_env(dir.path());
    config.write(dir.path(), false).unwrap();
    let host = Host::new(dir.path(), dir.path().join("hub.log"));
    let task = tokio::spawn(host.clone().supervise());
    tokio::time::timeout(Duration::from_secs(45), async {
        for _ in 0..450 {
            if host.status()["last_error"]
                .as_str()
                .is_some_and(|error| error.contains("5 restarts"))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("spawn restart budget never exhausted: {}", host.status());
    })
    .await
    .unwrap();
    assert_eq!(host.status()["state"], "failed");
    assert!(
        host.status()["last_error"]
            .as_str()
            .unwrap()
            .contains("starting agmen-hub")
    );
    host.stop();
    task.await.unwrap();
}

#[tokio::test]
async fn shared_env_file_is_refused_before_it_is_sourced() {
    let dir = tempfile::tempdir().unwrap();
    let env = dir.path().join("hub.env");
    let sourced = dir.path().join("sourced");
    write_private(
        &env,
        &format!("touch {sourced:?}\nAGMEN_PUBLIC_URL=https://host.example\n"),
    );
    for mode in [0o644, 0o640, 0o604, 0o620, 0o602] {
        fs::set_permissions(&env, fs::Permissions::from_mode(mode)).unwrap();
        let error = public_url_from_files(std::slice::from_ref(&env))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("0600"), "{error:#}");
        assert!(!sourced.exists(), "mode {mode:o} was sourced");
    }
    fs::set_permissions(&env, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        public_url_from_files(&[env]).await.unwrap(),
        "https://host.example"
    );
    assert!(sourced.exists());
}

#[tokio::test]
async fn hub_receives_only_hub_settings_from_its_environment() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config_for(dir.path());
    let script = dir.path().join("hub");
    let seen = dir.path().join("seen-env");
    fs::write(
        &script,
        "#!/bin/sh\n/usr/bin/env > \"$AGMEN_TEST_ENV_OUT.tmp\"\nmv \"$AGMEN_TEST_ENV_OUT.tmp\" \"$AGMEN_TEST_ENV_OUT\"\nexec /bin/sleep 120\n",
    )
    .unwrap();
    config.hub_bin = Some(script);
    write_private(
        &config.env_file,
        &format!(
            "AGMEN_TEST_ENV_OUT={seen:?}\nAGMEN_LISTEN=127.0.0.1:9876\n\
             AGMEN_GITHUB_CLIENT_SECRET=hub-needs-this\nRUST_LOG=debug\n\
             POSTGRES_PASSWORD=compose-only\nAGMEN_IT_PG=postgres://test-only\n\
             UNRELATED_SETTING=not-for-the-hub\n"
        ),
    );
    config.write(dir.path(), false).unwrap();
    let host = Host::new(dir.path(), dir.path().join("hub.log"));
    let task = tokio::spawn(host.clone().supervise());
    let mut text = String::new();
    for _ in 0..200 {
        if let Ok(read) = fs::read_to_string(&seen) {
            text = read;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    host.stop();
    task.await.unwrap();
    let names: Vec<_> = text
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name))
        .collect();
    for wanted in [
        "AGMEN_GITHUB_CLIENT_SECRET",
        "AGMEN_LISTEN",
        "AGMEN_PUBLIC_URL",
        "RUST_LOG",
        "PATH",
    ] {
        assert!(names.contains(&wanted), "{wanted} missing from {names:?}");
    }
    for unwanted in ["POSTGRES_PASSWORD", "AGMEN_IT_PG", "UNRELATED_SETTING"] {
        assert!(!names.contains(&unwanted), "{unwanted} passed: {names:?}");
    }
}
