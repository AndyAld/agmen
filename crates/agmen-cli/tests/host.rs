//! `agmen host` against a fake docker and an in-process hub stub.
#![cfg(unix)]
#![cfg(test)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use support::{Home, assert_exit, stderr};
use tokio::task::JoinHandle;

/// Fake `docker`. `fake_program` supplies the shebang and the call log.
const DOCKER: &str = r#"
sub="$1"
if [ "$1" = "compose" ]; then
  shift
  sub=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --project-directory | -f | --file)
        shift
        if [ "$#" -eq 0 ]; then
          exit 1
        fi
        shift
        ;;
      --project-directory=* | -f=* | --file=*)
        shift
        ;;
      -*)
        shift
        ;;
      *)
        sub="$1"
        break
        ;;
    esac
  done
fi
if [ -n "$sub" ] && [ -e "$HOME/fake/fail-$sub" ]; then
  exit 1
fi
case "$sub" in
  version)
    printf '%s\n' "1"
    ;;
  ps)
    if [ -f "$HOME/fake/ps.json" ]; then
      /bin/cat "$HOME/fake/ps.json" || exit 1
    fi
    ;;
  port)
    if [ -f "$HOME/fake/port" ]; then
      /bin/cat "$HOME/fake/port" || exit 1
    fi
    ;;
  logs)
    if [ -f "$HOME/fake/hub.log" ]; then
      /bin/cat "$HOME/fake/hub.log" || exit 1
    fi
    ;;
  up)
    if [ -f "$HOME/fake/up-noise" ]; then
      /bin/cat "$HOME/fake/up-noise" >&2 || exit 1
      /bin/cat "$HOME/fake/up-noise" || exit 1
    fi
    ;;
  restart)
    if [ -f "$HOME/fake/hub2.log" ]; then
      /bin/mv "$HOME/fake/hub2.log" "$HOME/fake/hub.log" || exit 1
    fi
    ;;
  *) ;;
esac
exit 0
"#;

const SECRET_KEYS: [&str; 3] = [
    "AGMEN_POSTGRES_PASSWORD",
    "AGMEN_EMQX_NODE_COOKIE",
    "AGMEN_EMQX_DASHBOARD_PASSWORD",
];

const RUNNING_PS: &str = "{\"Service\":\"hub\",\"State\":\"running\",\"Health\":\"healthy\"}\n";
const STOPPED_PS: &str = "{\"Service\":\"hub\",\"State\":\"exited\",\"Health\":\"\"}\n";

struct HubStub {
    port: u16,
    server: JoinHandle<()>,
}

impl Drop for HubStub {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn hub_stub() -> HubStub {
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;

    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/livez", get(|| async { StatusCode::OK }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            panic!("hub stub stopped: {err}");
        }
    });
    let addr = format!("127.0.0.1:{port}");
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(&addr).await.is_ok() {
            return HubStub { port, server };
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("hub stub did not accept connections");
}

fn install_docker(home: &Home) {
    home.fake_program("docker", DOCKER);
}

fn write_fake(home: &Home, name: &str, bytes: &[u8]) {
    let dir = home.path().join("fake");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), bytes).unwrap();
}

fn command(home: &Home, args: &[&str], allow_http: bool) -> Command {
    let mut cmd = home.agmen(home.path(), args);
    if allow_http {
        cmd.env("AGMEN_HOST_ALLOW_HTTP", "1");
    } else {
        cmd.env_remove("AGMEN_HOST_ALLOW_HTTP");
    }
    cmd
}

fn narrow_path(cmd: &mut Command) {
    let home = cmd
        .get_envs()
        .find(|(key, _)| *key == "HOME")
        .and_then(|(_, value)| value)
        .expect("HOME is set");
    cmd.env("PATH", Path::new(home).join("bin"));
}

async fn run(mut cmd: Command) -> Output {
    tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap()
}

fn host_dir(home: &Home) -> std::path::PathBuf {
    home.path().join(".agmen/host/agmen")
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn env_pairs(dir: &Path) -> Vec<(String, String)> {
    fs::read_to_string(dir.join(".env"))
        .unwrap()
        .lines()
        .map(|line| {
            let (key, value) = line.split_once('=').unwrap();
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

fn env_value<'a>(pairs: &'a [(String, String)], key: &str) -> &'a str {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
        .unwrap_or_else(|| panic!("missing {key}"))
}

fn is_secret(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn docker_log(home: &Home) -> String {
    fs::read_to_string(home.path().join("calls/docker.log")).unwrap()
}

async fn up_with_stub(home: &Home, stub: &HubStub, extra: &[&str]) -> Output {
    let url = format!("http://127.0.0.1:{}", stub.port);
    let mut owned = vec![
        "host".to_owned(),
        "up".to_owned(),
        "--port".to_owned(),
        stub.port.to_string(),
        "--public-url".to_owned(),
        url,
    ];
    owned.extend(extra.iter().map(|arg| (*arg).to_owned()));
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
    run(command(home, &args, true)).await
}

#[tokio::test]
async fn up_without_docker_names_the_install_link() {
    let home = Home::new();
    let mut cmd = command(
        &home,
        &["host", "up", "--public-url", "https://hub.example.com"],
        false,
    );
    narrow_path(&mut cmd);
    let out = run(cmd).await;
    assert_exit(&out, 1);
    assert!(
        stderr(&out).contains("https://docs.docker.com/get-docker/"),
        "{}",
        stderr(&out)
    );
    assert!(!home.path().join(".agmen/host").exists());
}

#[tokio::test]
async fn up_with_docker_stopped_says_so() {
    let home = Home::new();
    install_docker(&home);
    write_fake(&home, "fail-version", b"");
    let out = run(command(
        &home,
        &["host", "up", "--public-url", "https://hub.example.com"],
        false,
    ))
    .await;
    assert_exit(&out, 1);
    assert!(stderr(&out).contains("not running"), "{}", stderr(&out));
    assert!(!home.path().join(".agmen/host").exists());
}

#[tokio::test]
async fn up_writes_private_env_and_pinned_compose() {
    let home = Home::new();
    install_docker(&home);
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    let stub = hub_stub().await;
    let out = up_with_stub(&home, &stub, &[]).await;
    assert_exit(&out, 0);

    let dir = host_dir(&home);
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join(".env")), 0o600);
    let pairs = env_pairs(&dir);
    for key in SECRET_KEYS {
        assert!(
            is_secret(env_value(&pairs, key)),
            "{key} is not 64 lowercase hex characters"
        );
    }
    assert_eq!(env_value(&pairs, "AGMEN_TUNNEL"), "url");
    assert!(pairs.iter().all(|(key, _)| key != "AGMEN_RETENTION_DAYS"));

    let compose = fs::read_to_string(dir.join("compose.yaml")).unwrap();
    let pinned = format!("ghcr.io/agmen-dev/agmen-hub:{}", env!("CARGO_PKG_VERSION"));
    assert!(compose.contains(&pinned), "{compose}");
    assert!(compose.contains("emqx/emqx:6.3.2"), "{compose}");
    assert!(compose.contains("postgres:16.15"), "{compose}");
    assert!(!compose.contains(":latest"), "{compose}");
    for key in SECRET_KEYS {
        assert!(
            !compose.contains(env_value(&pairs, key)),
            "{key} was written into compose.yaml"
        );
    }

    let log = docker_log(&home);
    let needle = format!(
        "compose --project-directory {} -f {} up -d --wait --wait-timeout 180",
        dir.display(),
        dir.join("compose.yaml").display()
    );
    assert!(log.contains(&needle), "{log}");
}

#[tokio::test]
async fn up_twice_keeps_env_secrets() {
    let home = Home::new();
    install_docker(&home);
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    let stub = hub_stub().await;
    let first = up_with_stub(&home, &stub, &[]).await;
    assert_exit(&first, 0);
    let before = env_pairs(&host_dir(&home));

    let url = format!("http://localhost:{}", stub.port);
    let owned = [
        "host".to_owned(),
        "up".to_owned(),
        "--port".to_owned(),
        stub.port.to_string(),
        "--public-url".to_owned(),
        url,
    ];
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
    let second = run(command(&home, &args, true)).await;
    assert_exit(&second, 0);
    let after = env_pairs(&host_dir(&home));
    for key in SECRET_KEYS {
        assert!(
            env_value(&before, key) == env_value(&after, key),
            "{key} changed"
        );
    }
    assert_ne!(
        env_value(&before, "AGMEN_PUBLIC_URL"),
        env_value(&after, "AGMEN_PUBLIC_URL")
    );
}

#[tokio::test]
async fn up_stores_retention_limit() {
    let home = Home::new();
    install_docker(&home);
    for bad in ["0", "36501"] {
        let out = run(command(
            &home,
            &[
                "host",
                "up",
                "--retention-days",
                bad,
                "--public-url",
                "https://hub.example.com",
            ],
            false,
        ))
        .await;
        assert_exit(&out, 2);
    }

    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    let stub = hub_stub().await;
    let out = up_with_stub(&home, &stub, &["--retention-days", "30"]).await;
    assert_exit(&out, 0);
    let dir = host_dir(&home);
    let pairs = env_pairs(&dir);
    assert_eq!(env_value(&pairs, "AGMEN_RETENTION_DAYS"), "30");
    let compose = fs::read_to_string(dir.join("compose.yaml")).unwrap();
    assert!(
        compose.contains("AGMEN_RETENTION_DAYS: ${AGMEN_RETENTION_DAYS:-}"),
        "{compose}"
    );

    let again = up_with_stub(&home, &stub, &[]).await;
    assert_exit(&again, 0);
    assert_eq!(env_value(&env_pairs(&dir), "AGMEN_RETENTION_DAYS"), "30");
}

#[tokio::test]
async fn up_refuses_bad_public_urls() {
    let home = Home::new();
    install_docker(&home);
    for url in [
        "http://hub.example.com",
        "https://hub.example.com/?a=1",
        "ftp://x",
    ] {
        let out = run(command(&home, &["host", "up", "--public-url", url], false)).await;
        assert_exit(&out, 2);
        assert!(stderr(&out).contains("--public-url"), "{}", stderr(&out));
    }
}

#[tokio::test]
async fn up_reports_busy_port() {
    let home = Home::new();
    install_docker(&home);
    write_fake(&home, "ps.json", STOPPED_PS.as_bytes());
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let out = run(command(
        &home,
        &[
            "host",
            "up",
            "--port",
            &port,
            "--public-url",
            "https://hub.example.com",
        ],
        false,
    ))
    .await;
    assert_exit(&out, 1);
    assert!(stderr(&out).contains("is in use"), "{}", stderr(&out));
    assert!(!host_dir(&home).join("compose.yaml").exists());
    drop(listener);
}

#[tokio::test]
async fn up_shows_hub_logs_when_start_fails() {
    let home = Home::new();
    install_docker(&home);
    write_fake(&home, "fail-up", b"");
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    write_fake(&home, "hub.log", b"AGMEN_PUBLIC_URL is not a valid\n");
    let out = run(command(
        &home,
        &["host", "up", "--public-url", "https://hub.example.com"],
        false,
    ))
    .await;
    assert_exit(&out, 1);
    assert!(
        stderr(&out).contains("AGMEN_PUBLIC_URL is not a valid"),
        "{}",
        stderr(&out)
    );
}

#[tokio::test]
async fn host_name_rule() {
    let home = Home::new();
    install_docker(&home);
    let long = "a".repeat(33);
    for name in ["Bad_Name", long.as_str()] {
        let out = run(command(
            &home,
            &[
                "host",
                "up",
                "--name",
                name,
                "--public-url",
                "https://hub.example.com",
            ],
            false,
        ))
        .await;
        assert_exit(&out, 2);
    }
}

#[tokio::test]
async fn status_without_host_exits_3() {
    let home = Home::new();
    let out = run(command(&home, &["host", "status"], false)).await;
    assert_exit(&out, 3);
    assert!(stderr(&out).contains("agmen host up"), "{}", stderr(&out));
    assert!(!home.path().join("calls/docker.log").exists());
}

#[tokio::test]
async fn stop_runs_compose_stop() {
    let home = Home::new();
    install_docker(&home);
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    let stub = hub_stub().await;
    let out = up_with_stub(&home, &stub, &[]).await;
    assert_exit(&out, 0);
    let out = run(command(&home, &["host", "stop"], false)).await;
    assert_exit(&out, 0);
    let log = docker_log(&home);
    let last = log.lines().last().unwrap();
    assert!(last.ends_with("compose.yaml stop"), "{last}");
}

#[tokio::test]
async fn logs_passes_service_and_tail() {
    let home = Home::new();
    install_docker(&home);
    let out = run(command(
        &home,
        &["host", "logs", "--tail", "50", "hub"],
        false,
    ))
    .await;
    assert_exit(&out, 0);
    assert!(
        docker_log(&home).contains("logs --tail 50 hub"),
        "{}",
        docker_log(&home)
    );
}

/// A `docker` in `<home>/work` that records that it ran and answers only
/// `version` and `compose version`. Returns the work directory and the
/// marker file it appends to.
fn plant_docker(home: &Home) -> (std::path::PathBuf, std::path::PathBuf) {
    let work = home.path().join("work");
    fs::create_dir_all(&work).unwrap();
    let marker = home.path().join("planted-docker-ran");
    let script = format!(
        "#!/bin/sh\necho ran >> '{}'\nif [ \"$1\" = version ] || [ \"$2\" = version ]; then printf '1\\n'; exit 0; fi\nexit 1\n",
        marker.display()
    );
    let planted = work.join("docker");
    fs::write(&planted, script).unwrap();
    fs::set_permissions(&planted, fs::Permissions::from_mode(0o700)).unwrap();
    (work, marker)
}

/// `agmen <args>` run in `cwd` with `PATH` set to `path`.
fn command_in(home: &Home, cwd: &Path, path: &str, args: &[&str]) -> Command {
    let mut cmd = home.agmen(cwd, args);
    cmd.env_remove("AGMEN_HOST_ALLOW_HTTP");
    cmd.env("PATH", path);
    cmd
}

#[tokio::test]
async fn a_docker_in_the_working_directory_is_never_run() {
    let home = Home::new();
    install_docker(&home);
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    write_fake(&home, "fail-up", b"");
    let (work, marker) = plant_docker(&home);
    let bin = home.path().join("bin");
    // A relative entry and an empty entry both mean the current directory.
    for path in [
        format!(".:{}:/usr/bin:/bin", bin.display()),
        format!(":{}:/usr/bin:/bin", bin.display()),
    ] {
        let out = run(command_in(
            &home,
            &work,
            &path,
            &["host", "up", "--public-url", "https://hub.example.com"],
        ))
        .await;
        assert_exit(&out, 1);
        assert!(
            !marker.exists(),
            "ran the docker in the working directory with PATH={path}"
        );
    }
    // The docker in an absolute entry did run.
    assert!(
        docker_log(&home).contains("compose"),
        "{}",
        docker_log(&home)
    );
}

#[tokio::test]
async fn a_docker_only_in_the_working_directory_counts_as_missing() {
    let home = Home::new();
    let (work, marker) = plant_docker(&home);
    fs::create_dir_all(host_dir(&home)).unwrap();
    for path in [".", ":"] {
        for args in [
            &["host", "up", "--public-url", "https://hub.example.com"][..],
            &["host", "status"][..],
            &["host", "stop"][..],
            &["host", "logs"][..],
        ] {
            let out = run(command_in(&home, &work, path, args)).await;
            assert_exit(&out, 1);
            assert!(
                stderr(&out).contains("https://docs.docker.com/get-docker/"),
                "PATH={path} {args:?}: {}",
                stderr(&out)
            );
            assert!(
                !marker.exists(),
                "ran the docker in the working directory with PATH={path} {args:?}"
            );
        }
    }
}

#[tokio::test]
async fn status_stop_and_logs_name_the_install_link_without_docker() {
    let home = Home::new();
    fs::create_dir_all(host_dir(&home)).unwrap();
    for sub in ["status", "stop", "logs"] {
        let mut cmd = command(&home, &["host", sub], false);
        narrow_path(&mut cmd);
        let out = run(cmd).await;
        assert_exit(&out, 1);
        assert!(
            stderr(&out).contains("https://docs.docker.com/get-docker/"),
            "host {sub}: {}",
            stderr(&out)
        );
    }
}

#[tokio::test]
async fn docker_never_sees_the_shells_compose_or_agmen_variables() {
    let home = Home::new();
    let logger = r#"printf '%s|%s|%s|%s|%s\n' "${COMPOSE_PROJECT_NAME-unset}" "${COMPOSE_PROFILES-unset}" "${COMPOSE_ENV_FILES-unset}" "${AGMEN_PUBLIC_URL-unset}" "${AGMEN_POSTGRES_PASSWORD-unset}" >> "$HOME/calls/env.log""#;
    home.fake_program("docker", &format!("{logger}\n{DOCKER}"));
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    write_fake(&home, "fail-up", b"");
    let mut cmd = command(
        &home,
        &["host", "up", "--public-url", "https://hub.example.com"],
        false,
    );
    cmd.env("COMPOSE_PROJECT_NAME", "someone-elses-stack")
        .env("COMPOSE_PROFILES", "ngrok")
        .env("COMPOSE_ENV_FILES", "/elsewhere/.env")
        .env("AGMEN_PUBLIC_URL", "https://from-the-shell.example.com")
        .env("AGMEN_POSTGRES_PASSWORD", "from-the-shell");
    let out = run(cmd).await;
    assert_exit(&out, 1);
    let log = docker_log(&home);
    assert!(
        log.contains(" up -d --wait"),
        "never reached compose up: {log}"
    );
    let seen = fs::read_to_string(home.path().join("calls/env.log")).unwrap();
    for line in seen.lines() {
        assert_eq!(line, "unset|unset|unset|unset|unset", "{seen}");
    }
    assert!(seen.lines().count() >= 4, "{seen}");
}

#[tokio::test]
async fn up_prints_hub_logs_without_terminal_escapes() {
    let home = Home::new();
    install_docker(&home);
    write_fake(&home, "fail-up", b"");
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    write_fake(
        &home,
        "hub.log",
        b"first\n\x1b]0;pwned\x07 title\n\x1b[2Jcleared \x1b[31mred\x1b[0m\r\nAGMEN_PUBLIC_URL is not a valid url\n",
    );
    let out = run(command(
        &home,
        &["host", "up", "--public-url", "https://hub.example.com"],
        false,
    ))
    .await;
    assert_exit(&out, 1);
    assert!(
        stderr(&out).contains("AGMEN_PUBLIC_URL is not a valid url"),
        "{}",
        stderr(&out)
    );
    assert!(
        out.stderr
            .iter()
            .all(|byte| *byte == b'\n' || !byte.is_ascii_control()),
        "control characters reached stderr: {:?}",
        stderr(&out)
    );
}

#[tokio::test]
async fn up_shows_the_end_of_a_hub_log_over_one_mebibyte() {
    let home = Home::new();
    install_docker(&home);
    write_fake(&home, "fail-up", b"");
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    let mut log = Vec::new();
    for _ in 0..6_000 {
        log.extend_from_slice(
            b"INFO request handled path=/v1/messages status=200 elapsed_ms=3 padding=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n",
        );
    }
    log.extend_from_slice(b"binary \xff\xfe junk\n");
    for _ in 0..6_000 {
        log.extend_from_slice(
            b"INFO request handled path=/v1/messages status=200 elapsed_ms=3 padding=yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy\n",
        );
    }
    log.extend_from_slice(b"FATAL AGMEN_PUBLIC_URL is not a valid url\n");
    assert!(log.len() > 1024 * 1024);
    write_fake(&home, "hub.log", &log);
    let out = run(command(
        &home,
        &["host", "up", "--public-url", "https://hub.example.com"],
        false,
    ))
    .await;
    assert_exit(&out, 1);
    assert!(
        stderr(&out).contains("FATAL AGMEN_PUBLIC_URL is not a valid url"),
        "{}",
        stderr(&out)
    );
}

#[tokio::test]
async fn up_again_goes_on_when_compose_ps_cannot_be_read() {
    let home = Home::new();
    install_docker(&home);
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    let stub = hub_stub().await;
    assert_exit(&up_with_stub(&home, &stub, &[]).await, 0);

    // A Compose that prints another shape, then one that fails outright (a
    // missing `.env` makes `ps` fail on its required variables). The stack is
    // already there, so a rerun must not stop on this.
    for ps in [&b"[{\"Service\":\"hub\"}]\n"[..], b"not json\n"] {
        write_fake(&home, "ps.json", ps);
        let out = up_with_stub(&home, &stub, &[]).await;
        assert_exit(&out, 0);
        assert!(
            stderr(&out).contains("skipping the port check"),
            "{}",
            stderr(&out)
        );
    }
    write_fake(&home, "fail-ps", b"");
    assert_exit(&up_with_stub(&home, &stub, &[]).await, 0);
}

#[tokio::test]
async fn up_does_not_fail_on_more_than_a_mebibyte_of_docker_output() {
    let home = Home::new();
    install_docker(&home);
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    // Image pulls print progress to stderr; `up` reads none of it.
    write_fake(&home, "up-noise", &vec![b'.'; 3 * 1024 * 1024]);
    let stub = hub_stub().await;
    let out = up_with_stub(&home, &stub, &[]).await;
    assert_exit(&out, 0);
}

/// `fake_program` supplies the shebang and argument log.
const TAILSCALE: &str = r#"
if [ -e "$HOME/fake/tailscale-missing" ]; then
  exit 127
fi
case "$*" in
  'status --json')
    /bin/cat "$HOME/fake/tailscale-status.json" || exit 1
    ;;
  'funnel status --json')
    if [ -e "$HOME/fake/funnel-seen" ] && [ -e "$HOME/fake/funnel-status-later.json" ]; then
      /bin/cat "$HOME/fake/funnel-status-later.json" || exit 1
    else
      : > "$HOME/fake/funnel-seen"
      /bin/cat "$HOME/fake/funnel-status.json" || exit 1
    fi
    ;;
  'funnel --https=443 off')
    if [ -e "$HOME/fake/funnel-off-fails" ]; then
      exit 1
    fi
    ;;
  *) ;;
esac
exit 0
"#;

fn install_tailscale(home: &Home, state: &str) {
    home.fake_program("tailscale", TAILSCALE);
    let status =
        serde_json::json!({"BackendState": state, "Self": {"DNSName": "box.tail1234.ts.net."}});
    write_fake(home, "tailscale-status.json", status.to_string().as_bytes());
    write_fake(home, "funnel-status.json", b"{}");
}

fn tunnel_command(home: &Home, port: u16, extra: &[&str]) -> Command {
    let port = port.to_string();
    let mut args = vec!["host", "up", "--port", &port, "--wait", "1"];
    args.extend_from_slice(extra);
    let mut cmd = command(home, &args, false);
    cmd.env_remove("NGROK_AUTHTOKEN");
    cmd
}

fn tailscale_log(home: &Home) -> String {
    fs::read_to_string(home.path().join("calls/tailscale.log")).unwrap()
}

async fn tailscale_up(home: &Home, stub: &HubStub) -> Output {
    install_docker(home);
    install_tailscale(home, "Running");
    write_fake(home, "ps.json", RUNNING_PS.as_bytes());
    run(tunnel_command(home, stub.port, &[])).await
}

#[tokio::test]
async fn tailscale_preferred_when_signed_in() {
    let home = Home::new();
    let stub = hub_stub().await;
    let out = tailscale_up(&home, &stub).await;
    assert_exit(&out, 1);
    let pairs = env_pairs(&host_dir(&home));
    assert_eq!(env_value(&pairs, "AGMEN_TUNNEL"), "tailscale");
    assert_eq!(
        env_value(&pairs, "AGMEN_PUBLIC_URL").trim_end_matches('/'),
        "https://box.tail1234.ts.net"
    );
    assert!(tailscale_log(&home).contains(&format!("funnel --bg --yes {}", stub.port)));
    assert!(
        stderr(&out).contains("up to 15 minutes"),
        "{}",
        stderr(&out)
    );
}

#[tokio::test]
async fn funnel_busy_is_refused() {
    for foreground in [false, true] {
        let home = Home::new();
        install_docker(&home);
        install_tailscale(&home, "Running");
        let web = serde_json::json!({"Web": {"box.tail1234.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:3000"}}}}});
        let status = if foreground {
            serde_json::json!({"Foreground": {"session": web}})
        } else {
            web
        };
        write_fake(&home, "funnel-status.json", status.to_string().as_bytes());
        let out = run(tunnel_command(&home, 7080, &[])).await;
        assert_exit(&out, 1);
        for hint in [
            "3000",
            "tailscale funnel reset",
            "agmend host mode",
            "agmend host init --tailscale-serve",
        ] {
            assert!(stderr(&out).contains(hint), "{}", stderr(&out));
        }
        assert!(!tailscale_log(&home).contains("funnel --bg"));
        assert!(!tailscale_log(&home).contains("funnel reset"));
    }
}

#[tokio::test]
async fn tailscale_stopped_is_not_used() {
    let home = Home::new();
    install_docker(&home);
    install_tailscale(&home, "Stopped");
    let out = run(tunnel_command(&home, 7080, &[])).await;
    assert_exit(&out, 2);
    assert!(!tailscale_log(&home).contains("funnel --bg"));
}

async fn ngrok_up(home: &Home, token: &str) -> Output {
    use axum::{Json, Router, routing::get};
    install_docker(home);
    install_tailscale(home, "Stopped");
    write_fake(home, "tailscale-missing", b"");
    write_fake(home, "ps.json", RUNNING_PS.as_bytes());
    let stub = hub_stub().await;
    let app = Router::new().route("/api/tunnels", get(|| async {
        Json(serde_json::json!({"tunnels": [{"public_url": "http://a"}, {"public_url": "https://able-otter-123.ngrok-free.app"}]}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    write_fake(home, "port", addr.to_string().as_bytes());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut cmd = tunnel_command(home, stub.port, &[]);
    cmd.env("NGROK_AUTHTOKEN", token);
    let out = run(cmd).await;
    server.abort();
    out
}

#[tokio::test]
async fn ngrok_when_no_tailscale() {
    let home = Home::new();
    let token = "FakeNgrokToken_".repeat(2);
    let out = ngrok_up(&home, &token).await;
    assert_exit(&out, 1);
    let pairs = env_pairs(&host_dir(&home));
    assert_eq!(env_value(&pairs, "AGMEN_TUNNEL"), "ngrok");
    assert!(env_value(&pairs, "AGMEN_NGROK_AUTHTOKEN") == token);
    assert_eq!(env_value(&pairs, "COMPOSE_PROFILES"), "ngrok");
    assert_eq!(
        env_value(&pairs, "AGMEN_PUBLIC_URL").trim_end_matches('/'),
        "https://able-otter-123.ngrok-free.app"
    );
    assert_eq!(mode(&host_dir(&home).join(".env")), 0o600);
    let log = docker_log(&home);
    let starts: Vec<_> = log
        .lines()
        .filter(|line| line.contains(" up -d --wait --wait-timeout 180"))
        .collect();
    assert_eq!(starts.len(), 2, "{log}");
    assert!(starts[0].ends_with("180 ngrok"), "{log}");
    assert!(starts[1].ends_with("180"), "{log}");
}

#[tokio::test]
async fn no_tunnel_lists_three_options() {
    let home = Home::new();
    install_docker(&home);
    install_tailscale(&home, "Stopped");
    write_fake(&home, "tailscale-missing", b"");
    let out = run(tunnel_command(&home, 7080, &[])).await;
    assert_exit(&out, 2);
    for option in ["Tailscale", "ngrok", "--public-url"] {
        assert!(stderr(&out).contains(option), "{}", stderr(&out));
    }
}

#[tokio::test]
async fn up_stops_when_public_url_unreachable() {
    let home = Home::new();
    install_docker(&home);
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    let stub = hub_stub().await;
    let mut cmd = command(
        &home,
        &[
            "host",
            "up",
            "--port",
            &stub.port.to_string(),
            "--public-url",
            "https://127.0.0.1:9",
            "--wait",
            "2",
        ],
        false,
    );
    cmd.env_remove("NGROK_AUTHTOKEN");
    let start = std::time::Instant::now();
    let out = run(cmd).await;
    assert_exit(&out, 1);
    assert!(start.elapsed() < Duration::from_secs(10));
    for hint in [
        "https://127.0.0.1:9",
        "agmen host up again",
        "including WebSocket upgrades on /mqtt",
        "nothing is lost",
    ] {
        assert!(stderr(&out).contains(hint), "{}", stderr(&out));
    }
    assert!(
        !docker_log(&home)
            .lines()
            .any(|line| line.ends_with(" stop") || line.ends_with(" down"))
    );
}

#[tokio::test]
async fn stop_turns_funnel_off() {
    let home = Home::new();
    let stub = hub_stub().await;
    assert_exit(&tailscale_up(&home, &stub).await, 1);
    write_fake(
        &home,
        "funnel-status.json",
        own_funnel(stub.port).as_bytes(),
    );
    let out = run(command(&home, &["host", "stop"], false)).await;
    assert_exit(&out, 0);
    assert!(tailscale_log(&home).contains("funnel --https=443 off"));
    write_fake(&home, "funnel-off-fails", b"");
    let out = run(command(&home, &["host", "stop"], false)).await;
    assert_exit(&out, 0);
    assert!(stderr(&out).contains("warning"), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("tailscale funnel reset"),
        "{}",
        stderr(&out)
    );
    assert!(!tailscale_log(&home).contains("funnel reset"));
}

#[tokio::test]
async fn ngrok_token_never_on_argv() {
    let home = Home::new();
    let token = "FakePrivateToken_".repeat(2);
    let out = ngrok_up(&home, &token).await;
    assert_exit(&out, 1);
    assert!(!docker_log(&home).contains(&token));
    assert!(!stderr(&out).contains(&token));
    assert!(!String::from_utf8_lossy(&out.stdout).contains(&token));
    assert!(host_dir(&home).join(".env").exists());
}

#[tokio::test]
async fn foreground_conflict_is_not_hidden_by_a_matching_background_proxy() {
    let home = Home::new();
    install_docker(&home);
    install_tailscale(&home, "Running");
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    let stub = hub_stub().await;
    let target = format!("http://127.0.0.1:{}", stub.port);
    let status = serde_json::json!({
        "Web": {"box.tail1234.ts.net:443": {"Handlers": {"/": {"Proxy": target}}}},
        "Foreground": {"session": {"Web": {"box.tail1234.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:3000"}}}}}}
    });
    write_fake(&home, "funnel-status.json", status.to_string().as_bytes());
    let out = run(tunnel_command(&home, stub.port, &[])).await;
    assert_exit(&out, 1);
    assert!(
        stderr(&out).contains("already serves http://127.0.0.1:3000"),
        "{}",
        stderr(&out)
    );
    assert!(!tailscale_log(&home).contains("funnel --bg"));
    assert!(!docker_log(&home).contains(" up -d"));
}

#[cfg(target_os = "macos")]
async fn terminal_token_input(input: &[u8]) -> (Home, Vec<u8>, i32) {
    use std::process::Stdio;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let home = Home::new();
    install_docker(&home);
    install_tailscale(&home, "Stopped");
    write_fake(&home, "tailscale-missing", b"");
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    write_fake(&home, "fail-up", b"");
    // script owns a fresh PTY, so the user's terminal is never changed.
    // macOS sets the PENDIN state bit on returning to canonical mode.
    let mut cmd = Command::new("/usr/bin/script");
    cmd.args([
        "-q",
        "/dev/null",
        "/bin/sh",
        "-c",
        r#"before=$(/bin/stty -g) || exit 99; "$@"; result=$?; after=$(/bin/stty -g) || exit 99; printf 'terminal-before=%s\nterminal-after=%s\nterminal-restored\n' "$before" "$after"; exit "$result""#,
        "sh",
        env!("CARGO_BIN_EXE_agmen"),
        "host",
        "up",
        "--tunnel",
        "ngrok",
    ]);
    home.apply_env(&mut cmd);
    cmd.current_dir(home.path()).env_remove("NGROK_AUTHTOKEN");
    let mut child = tokio::process::Command::from(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut bytes = Vec::new();
    let prompt = b"your-authtoken: ";
    tokio::time::timeout(Duration::from_secs(10), async {
        for _ in 0..4096 {
            bytes.push(stdout.read_u8().await.unwrap());
            if bytes.ends_with(prompt) {
                return;
            }
        }
        panic!("token prompt exceeds 4096 bytes");
    })
    .await
    .unwrap();
    stdin.write_all(input).await.unwrap();
    let mut stdout = stdout.take(65536);
    let (status, _) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(child.wait(), stdout.read_to_end(&mut bytes))
    })
    .await
    .unwrap()
    .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    let settings = |label: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(label))
            .expect("the shell must inspect the terminal after CLI exit")
            .split(':')
            .map(|field| {
                if let Some(flags) = field.strip_prefix("lflag=") {
                    // sys/termios.h defines PENDIN as transient state 0x20000000.
                    let flags = u64::from_str_radix(flags, 16).unwrap() & !0x20000000;
                    format!("lflag={flags:x}")
                } else {
                    field.to_owned()
                }
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(settings("terminal-before="), settings("terminal-after="));
    (home, bytes, status.code().unwrap())
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn terminal_ngrok_token_is_not_echoed() {
    let token = "FakePastedToken_".repeat(2);
    let (home, bytes, code) = terminal_token_input(format!("{token}\n").as_bytes()).await;
    assert_eq!(
        code, 1,
        "CLI must fail on fake Docker and restore the terminal"
    );
    assert!(
        !String::from_utf8_lossy(&bytes).contains(&token),
        "terminal echoed the token"
    );
    assert!(String::from_utf8_lossy(&bytes).contains("terminal-restored"));
    assert!(env_value(&env_pairs(&host_dir(&home)), "AGMEN_NGROK_AUTHTOKEN") == token);
    assert!(!docker_log(&home).contains(&token));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn cancelled_terminal_token_restores_settings() {
    let (home, bytes, code) = terminal_token_input(b"\x03").await;
    assert!(
        String::from_utf8_lossy(&bytes).contains("terminal-restored"),
        "cancellation skipped the terminal restoration check"
    );
    assert_eq!(
        code, 2,
        "Ctrl+C must cancel token entry and restore the terminal"
    );
    assert!(!host_dir(&home).join(".env").exists());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn invalid_terminal_token_restores_settings() {
    for input in [b"short\n".to_vec(), vec![b'a'; 256]] {
        let (home, bytes, code) = terminal_token_input(&input).await;
        assert_eq!(code, 2);
        assert!(String::from_utf8_lossy(&bytes).contains("terminal-restored"));
        assert!(!host_dir(&home).join(".env").exists());
    }
}

#[tokio::test]
async fn unusable_tailscale_status_falls_back() {
    for status in [
        b"not json".as_slice(),
        br#"{"BackendState":"Running","Self":{"DNSName":""}}"#,
        br#"{"BackendState":"NeedsLogin"}"#,
    ] {
        let home = Home::new();
        install_docker(&home);
        install_tailscale(&home, "Running");
        write_fake(&home, "tailscale-status.json", status);
        let out = run(tunnel_command(&home, 7080, &[])).await;
        assert_exit(&out, 2);
        assert!(stderr(&out).contains("ngrok"), "{}", stderr(&out));
        assert!(!tailscale_log(&home).contains("funnel"));
    }
}

#[tokio::test]
async fn stored_public_url_is_reused() {
    let home = Home::new();
    install_docker(&home);
    install_tailscale(&home, "Running");
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    let stub = hub_stub().await;
    assert_exit(&up_with_stub(&home, &stub, &["--tunnel", "ngrok"]).await, 0);
    let mut cmd = tunnel_command(&home, stub.port, &[]);
    cmd.env("AGMEN_HOST_ALLOW_HTTP", "1");
    assert_exit(&run(cmd).await, 0);
    assert!(!home.path().join("calls/tailscale.log").exists());
    assert_eq!(
        env_value(&env_pairs(&host_dir(&home)), "COMPOSE_PROFILES"),
        ""
    );
}

#[tokio::test]
async fn stored_ngrok_token_wins_over_environment() {
    let home = Home::new();
    let stored = "FakeStoredToken_".repeat(2);
    fs::create_dir_all(host_dir(&home)).unwrap();
    fs::write(
        host_dir(&home).join(".env"),
        format!("AGMEN_TUNNEL=ngrok\nAGMEN_NGROK_AUTHTOKEN={stored}\n"),
    )
    .unwrap();
    let out = ngrok_up(&home, &"FakeEnvironmentToken_".repeat(2)).await;
    assert_exit(&out, 1);
    assert!(env_value(&env_pairs(&host_dir(&home)), "AGMEN_NGROK_AUTHTOKEN") == stored);
    assert!(!docker_log(&home).contains(&stored));
    assert!(!stderr(&out).contains(&stored));
}

fn own_funnel(port: u16) -> String {
    serde_json::json!({"Web": {"box.tail1234.ts.net:443": {"Handlers": {"/": {"Proxy": format!("http://127.0.0.1:{port}")}}}}})
        .to_string()
}

#[tokio::test]
async fn other_443_entries_are_refused() {
    let busy = [
        serde_json::json!({"Web": {"box.tail1234.ts.net:443": {"Handlers": {"/docs": {"Proxy": "http://127.0.0.1:3000"}}}}}),
        serde_json::json!({"Web": {"box.tail1234.ts.net:443": {"Handlers": {"/": {"Path": "/srv/site"}}}}}),
        serde_json::json!({"Web": {"box.tail1234.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:7080"}, "/docs": {"Text": "hi"}}}}}),
        serde_json::json!({"Web": {"box.tail1234.ts.net:443": "odd"}}),
        serde_json::json!({"TCP": {"443": {"TCPForward": "127.0.0.1:2222"}}}),
        serde_json::json!({"Foreground": {"s": {"Web": {"box.tail1234.ts.net:443": {"Handlers": {"/docs": {"Text": "hi"}}}}}}}),
    ];
    for status in busy {
        let home = Home::new();
        install_docker(&home);
        install_tailscale(&home, "Running");
        write_fake(&home, "funnel-status.json", status.to_string().as_bytes());
        let out = run(tunnel_command(&home, 7080, &[])).await;
        assert_exit(&out, 1);
        assert!(
            stderr(&out).contains("already serves"),
            "{status}: {}",
            stderr(&out)
        );
        assert!(!tailscale_log(&home).contains("funnel --bg"), "{status}");
        assert!(!docker_log(&home).contains(" up -d"), "{status}");
    }
}

#[tokio::test]
async fn funnel_is_checked_again_before_it_is_enabled() {
    let home = Home::new();
    let stub = hub_stub().await;
    install_docker(&home);
    install_tailscale(&home, "Running");
    write_fake(&home, "ps.json", RUNNING_PS.as_bytes());
    let other = serde_json::json!({"Web": {"box.tail1234.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:3000"}}}}});
    write_fake(
        &home,
        "funnel-status-later.json",
        other.to_string().as_bytes(),
    );
    let out = run(tunnel_command(&home, stub.port, &[])).await;
    assert_exit(&out, 1);
    assert!(stderr(&out).contains("3000"), "{}", stderr(&out));
    assert!(!tailscale_log(&home).contains("funnel --bg"));
}

#[tokio::test]
async fn stop_leaves_a_different_443_mapping_alone() {
    let home = Home::new();
    let stub = hub_stub().await;
    assert_exit(&tailscale_up(&home, &stub).await, 1);
    let other = serde_json::json!({"Web": {"box.tail1234.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:8080"}}}}});
    for status in [other.to_string(), "{}".to_owned(), "not json".to_owned()] {
        write_fake(&home, "funnel-status.json", status.as_bytes());
        let out = run(command(&home, &["host", "stop"], false)).await;
        assert_exit(&out, 0);
        assert!(
            !tailscale_log(&home).contains("--https=443 off"),
            "{status}"
        );
    }
}
