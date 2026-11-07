//! Real hub SSE coverage, following the isolated dev database and temporary
//! daemon pattern in tests/e2e/tests/it_roles_listener.rs. The hub binary must
//! be built alongside the workspace tests. Uses the dev Postgres container's
//! psql rather than adding a database dependency to agmend.

#![cfg(test)]

mod support;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use agmen_local::Request;
use serde_json::{Value, json};
use support::{ACCESS, HANDLE, HUMAN, Home, PROJECT, result};
use tokio::io::AsyncWriteExt as _;

const IDENTITIES: usize = agmen_local::safeguards::MAX_MANAGED - 1;
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);
const MAX_POLLS: usize = 300;
const POST_INTERVAL: Duration = Duration::from_millis(110);
const RECIPIENTS: usize = 8;
const DAEMON_PASSES: usize = 2;

fn env(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is unset; load deploy/dev/.secrets/dev.env"))
}

async fn output(mut command: tokio::process::Command) -> Vec<u8> {
    let answer = tokio::time::timeout(WAIT, command.kill_on_drop(true).output())
        .await
        .unwrap()
        .unwrap();
    // Child error output can contain connection credentials.
    assert!(answer.status.success(), "dev fixture command failed");
    answer.stdout
}

struct Database {
    container: String,
    user: String,
    admin: String,
    name: String,
    url: url::Url,
}

impl Database {
    async fn create() -> Self {
        let mut docker = tokio::process::Command::new("docker");
        docker.args([
            "ps",
            "--filter",
            "label=com.docker.compose.project=agmen-dev",
            "--filter",
            "label=com.docker.compose.service=postgres",
            "--format",
            "{{.ID}}",
        ]);
        let containers = String::from_utf8(output(docker).await).unwrap();
        let ids: Vec<_> = containers.lines().collect();
        assert_eq!(ids.len(), 1, "one dev Postgres container must be running");
        let mut url = url::Url::parse(&env("AGMEN_IT_PG")).unwrap();
        let admin = url.path().trim_start_matches('/').to_owned();
        let name = format!(
            "agmen_it_cap_{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let database = Self {
            container: ids[0].to_owned(),
            user: url.username().to_owned(),
            admin,
            name: name.clone(),
            url: url.clone(),
        };
        database
            .sql(&database.admin, &format!("CREATE DATABASE {name}"))
            .await;
        url.set_path(&name);
        Self { url, ..database }
    }

    async fn sql(&self, database: &str, sql: &str) {
        let mut child = tokio::process::Command::new("docker")
            .args([
                "exec",
                "-i",
                &self.container,
                "psql",
                "--no-psqlrc",
                "-U",
                &self.user,
                "-d",
                database,
                "--set",
                "ON_ERROR_STOP=1",
                "--quiet",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        tokio::time::timeout(WAIT, input.write_all(sql.as_bytes()))
            .await
            .unwrap()
            .unwrap();
        drop(input);
        assert!(
            tokio::time::timeout(WAIT, child.wait())
                .await
                .unwrap()
                .unwrap()
                .success(),
            "fixture SQL failed"
        );
    }

    async fn seed(&self) {
        // This is the same isolated seeding as the hub's integration harness.
        // sha256(bytea) is built into the dev stack's PostgreSQL 16.
        let sql = format!(
            r#"
            INSERT INTO tenants (id,name) VALUES ('tenant01','stream cap');
            INSERT INTO projects (id,tenant_id,name) VALUES ('{PROJECT}','tenant01','stream cap');
            INSERT INTO humans (id,provider,provider_sub,handle) VALUES ('{HUMAN}','github','4242','{HANDLE}');
            INSERT INTO memberships (human_id,project_id,role) VALUES ('{HUMAN}','{PROJECT}','admin');
            INSERT INTO tenant_members (tenant_id,human_id,role) VALUES ('tenant01','{HUMAN}','owner');
            INSERT INTO tokens (hash,kind,human_id,expires_at)
                VALUES (sha256(convert_to('{ACCESS}','UTF8')),'access','{HUMAN}',now()+interval '1 hour');
            INSERT INTO identities (id,tenant_id,project_id,kind,handle,owner_human_id)
                SELECT 'agent'||lpad(n::text,3,'0'),'tenant01','{PROJECT}','agent',
                    '{HANDLE}/agent'||lpad(n::text,3,'0'),'{HUMAN}' FROM generate_series(1,{IDENTITIES}) n;
        "#
        );
        self.sql(&self.name, &sql).await;
    }

    async fn remove(&self) {
        self.sql(
            &self.admin,
            &format!("DROP DATABASE {} WITH (FORCE)", self.name),
        )
        .await;
    }
}

async fn start_hub(database: &Database) -> (tokio::process::Child, String) {
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("agmen-hub");
    assert!(
        binary.is_file(),
        "build the workspace agmen-hub binary before the it profile"
    );
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let base = format!("http://{address}/");
    let instance = format!("cap{:x}", std::process::id());
    let child = tokio::process::Command::new(binary)
        .env("AGMEN_DATABASE_URL", database.url.as_str())
        .env("AGMEN_LISTEN", address.to_string())
        .env("AGMEN_PUBLIC_URL", &base)
        .env("AGMEN_HUB_MODE", "self-host")
        .env("AGMEN_GITHUB_CLIENT_ID", "stream-cap-fixture")
        .env("AGMEN_EMQX_API_URL", env("AGMEN_IT_EMQX_API"))
        .env("AGMEN_EMQX_API_KEY", env("AGMEN_IT_EMQX_API_KEY"))
        .env("AGMEN_EMQX_API_SECRET", env("AGMEN_IT_EMQX_API_SECRET"))
        .env("AGMEN_MQTT_URL", env("AGMEN_IT_MQTT"))
        .env("AGMEN_HUB_INSTANCE", &instance)
        .env("AGMEN_MQTT_SHARE_GROUP", &instance)
        .env(
            "AGMEN_JWT_SIGNING_KEY",
            PathBuf::from(env("AGMEN_IT_JWT_DIR")).join("agmen-dev-1.pem"),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let http = reqwest::Client::builder().timeout(WAIT).build().unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    for _ in 0..MAX_POLLS {
        match tokio::time::timeout_at(deadline, http.get(format!("{base}statusz")).send()).await {
            Ok(Ok(response)) if response.status().is_success() => return (child, base),
            Err(_) => break,
            _ => {}
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("real hub did not become ready within {WAIT:?}");
}

async fn wait_position(home: &Home, stream: String, wanted: i64) {
    let path = home.database();
    tokio::task::spawn_blocking(move || {
        let connection = rusqlite::Connection::open(path).unwrap();
        for _ in 0..MAX_POLLS {
            let saved: i64 = connection
                .query_row(
                    "SELECT coalesce(max(seq),0) FROM positions WHERE project=?1 AND stream=?2",
                    rusqlite::params![PROJECT, stream],
                    |r| r.get(0),
                )
                .unwrap();
            if saved >= wanted {
                return;
            }
            std::thread::sleep(POLL);
        }
        panic!("{stream}:{wanted} was never committed");
    })
    .await
    .unwrap();
}

async fn post(http: &reqwest::Client, base: &str, n: usize, recipients: &[String]) {
    let mut message = support::envelope(n as u32, "stream cap integration");
    message["sent_at"] = json!(jiff::Timestamp::now());
    if !recipients.is_empty() {
        message["verb"] = json!("DM");
        message["to"] = json!(recipients);
    }
    let response = http
        .post(format!("{base}v1/projects/{PROJECT}/messages"))
        .bearer_auth(ACCESS)
        .json(&message)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "posting fixture message failed: {}",
        response.status()
    );
    let answer: Value = response.json().await.unwrap();
    assert_eq!(answer["verb"], "ACK");
    tokio::time::sleep(POST_INTERVAL).await;
}

async fn exercise(base: String) {
    let home = Home::signed_in(&base).await;
    drop(agmend::Agmend::bind(home.config()).unwrap());
    let home = tokio::task::spawn_blocking(move || {
        for n in 1..=IDENTITIES {
            home.seed_identity(&base, &format!("agent{n:03}"), &"c".repeat(64));
        }
        (home, base)
    })
    .await
    .unwrap();
    let (home, base) = home;
    let http = reqwest::Client::builder().timeout(WAIT).build().unwrap();
    post(&http, &base, 1, &[]).await;
    let agents: Vec<_> = (1..=IDENTITIES).map(|n| format!("agent{n:03}")).collect();
    for (n, recipients) in agents.chunks(RECIPIENTS).enumerate() {
        post(&http, &base, n + 2, recipients).await;
    }
    // Drop and rebind the daemon after all 257 positions exist, so its next
    // connection has to resume through both real SSE requests.
    for pass in 0..DAEMON_PASSES {
        let daemon = agmend::Agmend::bind(home.config()).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(daemon.serve_until(async {
            let _stopped = stopped.await;
        }));
        wait_position(&home, "inbox:agent255".into(), 1).await;
        wait_position(
            &home,
            format!("inbox:{HUMAN}"),
            IDENTITIES.div_ceil(RECIPIENTS) as i64,
        )
        .await;
        let path = home.database();
        let count: i64 = tokio::task::spawn_blocking(move || {
            rusqlite::Connection::open(path)
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM positions WHERE project=?1",
                    [PROJECT],
                    |row| row.get(0),
                )
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(count, (IDENTITIES + 2) as i64);
        if pass == 1 {
            post(&http, &base, IDENTITIES + 2, &["agent255".into()]).await;
            wait_position(&home, "inbox:agent255".into(), 2).await;
            // The person's inbox sorts last, so only the second SSE request
            // delivers it. Every DM this person sends copies into it.
            wait_position(
                &home,
                format!("inbox:{HUMAN}"),
                IDENTITIES.div_ceil(RECIPIENTS) as i64 + 1,
            )
            .await;
            post(&http, &base, IDENTITIES + 3, &[]).await;
            wait_position(&home, "feed".into(), 2).await;
            let mut client = support::Client::connect(&home.socket()).await;
            result(client.call("hello", json!({"api":1})).await);
            let mut request = Request::new(99, "pending", json!({"local_token":"c".repeat(64)}));
            request.as_ = Some("agent255".parse().unwrap());
            client.send(&request).await;
            let rows = result(tokio::time::timeout(WAIT, client.read()).await.unwrap());
            let seqs: Vec<_> = rows
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["stream"] == "inbox:agent255")
                .map(|row| row["seq"].clone())
                .collect();
            assert_eq!(seqs, [json!(1), json!(2)]);
        }
        stop.send(()).unwrap();
        tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
    }
}

async fn clear_retained() {
    let http = reqwest::Client::builder().timeout(WAIT).build().unwrap();
    let topics = agmen_proto::Topics::new("tenant01".parse().unwrap(), PROJECT.parse().unwrap());
    for topic in [topics.state_head(), topics.state("roster")] {
        let mut url = url::Url::parse(&env("AGMEN_IT_EMQX_API")).unwrap();
        url.path_segments_mut()
            .unwrap()
            .pop_if_empty()
            .extend(["mqtt", "retainer", "message", &topic]);
        let response = http
            .delete(url)
            .basic_auth(
                env("AGMEN_IT_EMQX_API_KEY"),
                Some(env("AGMEN_IT_EMQX_API_SECRET")),
            )
            .send()
            .await
            .unwrap();
        assert!(matches!(response.status().as_u16(), 200 | 204 | 404));
    }
}

#[tokio::test]
async fn it_owner_with_255_identities_keeps_every_stream() {
    let database = Database::create().await;
    let (mut hub, base) = start_hub(&database).await;
    database.seed().await;
    // Joining catches assertion panics so the isolated database is removed.
    let outcome = tokio::spawn(exercise(base)).await;
    hub.kill().await.unwrap();
    clear_retained().await;
    database.remove().await;
    if let Err(error) = outcome {
        std::panic::resume_unwind(error.into_panic());
    }
}
