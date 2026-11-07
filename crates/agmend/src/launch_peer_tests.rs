use super::*;
use crate::peer_tests as probe;

async fn escaped_batch(depth: usize) {
    let dir = tempfile::tempdir().unwrap();
    let runner = Arc::new(ClaudeRunner {
        program: probe::client(dir.path(), depth),
        shell: Arc::new(TestShell {
            calls: std::sync::atomic::AtomicUsize::new(0),
            output: format!("{LOGIN_PATH_MARKER}\n/usr/bin:/bin\n"),
        }),
        ..ClaudeRunner::default()
    });
    let mut config = crate::Config::new(
        dir.path().join("daemon.sock"),
        dir.path().join("login.json"),
        agmen_client::TokenStore::Dir(dir.path().join("tokens")),
    );
    config.runner = runner.clone();
    config.claude_home = Some(dir.path().join("claude-home"));
    let daemon = crate::Agmend::bind(config).unwrap();
    let project = daemon
        .core
        .origins
        .local()
        .create_project("ancestry", "owner")
        .await
        .unwrap()
        .project()
        .clone();
    probe::requests(dir.path(), &project);
    let listener = tokio::net::UnixListener::bind(dir.path().join("peer.sock")).unwrap();
    let (_stop, stopped) = tokio::sync::oneshot::channel();
    let running = tokio::spawn(runner.run(spec(dir.path()), stopped));
    let connection = probe::accept(&listener, daemon.core.clone()).await;
    let answers = probe::answers(dir.path()).await;
    connection.await.unwrap();
    probe::human_controls(daemon.core.clone(), dir.path(), &listener, &project).await;
    std::fs::write(dir.path().join("finish"), "done").unwrap();
    let _batch_result = tokio::time::timeout(probe::WAIT, running)
        .await
        .unwrap()
        .unwrap();
    probe::assert_refused(&answers);
}

#[tokio::test]
async fn batch_new_session_child_cannot_use_owner_methods_or_human_post() {
    escaped_batch(1).await;
}

#[tokio::test]
async fn batch_new_session_grandchild_cannot_use_owner_methods_or_human_post() {
    escaped_batch(2).await;
}

#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[tokio::test]
async fn unreadable_peer_ancestry_is_refused_and_logs_once_then_debug() {
    use agmen_local::{Outcome, Request, Response, frame};
    use serde_json::json;
    use tracing::instrument::WithSubscriber as _;

    let dir = tempfile::tempdir().unwrap();
    let runner = Arc::new(ClaudeRunner::default());
    runner.groups.lock().unwrap().insert(i32::MAX);
    let mut config = crate::Config::new(
        dir.path().join("daemon.sock"),
        dir.path().join("login.json"),
        agmen_client::TokenStore::Dir(dir.path().join("tokens")),
    );
    config.runner = runner;
    config.claude_home = Some(dir.path().join("claude-home"));
    let daemon = crate::Agmend::bind(config).unwrap();
    let logs = Logs::default();
    let subscriber = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(logs.clone())
            .finish(),
    );
    for _ in 0..2 {
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let task = tokio::spawn(
            crate::api::serve_with_peer(server, daemon.core.clone(), |_| Ok(i32::MAX - 1))
                .with_subscriber(subscriber.clone()),
        );
        for (id, method, params) in [
            (1, "hello", json!({"api":1})),
            (2, "agent_list", json!({})),
            (3, "post", json!({})),
        ] {
            frame::write_async(&mut client, &Request::new(id, method, params))
                .await
                .unwrap();
            let answer: Response =
                tokio::time::timeout(probe::WAIT, frame::read_async(&mut client))
                    .await
                    .unwrap()
                    .unwrap();
            if id == 1 {
                assert!(matches!(answer.outcome, Outcome::Ok { .. }));
            } else {
                let Outcome::Err { error } = answer.outcome else {
                    panic!("{answer:?}");
                };
                assert_eq!(error.code, agmen_local::ErrorCode::Forbidden);
            }
        }
        drop(client);
        task.await.unwrap();
    }
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    let failures: Vec<_> = logs
        .lines()
        .filter(|line| line.contains("peer ancestry lookup failed"))
        .collect();
    assert_eq!(
        failures.iter().filter(|line| line.contains("WARN")).count(),
        1,
        "{logs}"
    );
    assert_eq!(
        failures
            .iter()
            .filter(|line| line.contains("DEBUG"))
            .count(),
        1,
        "{logs}"
    );
}

/// Well under the 2 s the CLI gives connect and hello together.
const PROMPT: std::time::Duration = std::time::Duration::from_secs(1);

/// A daemon that sees an agent running, so every connection needs an ancestry walk, and whose
/// walks wait on the returned gate until the test adds a permit.
fn gated_daemon(dir: &std::path::Path) -> (crate::Agmend, Arc<tokio::sync::Semaphore>) {
    let runner = Arc::new(ClaudeRunner::default());
    runner.groups.lock().unwrap().insert(i32::MAX);
    let mut config = crate::Config::new(
        dir.join("daemon.sock"),
        dir.join("login.json"),
        agmen_client::TokenStore::Dir(dir.join("tokens")),
    );
    config.runner = runner;
    config.claude_home = Some(dir.join("claude-home"));
    let daemon = crate::Agmend::bind(config).unwrap();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *daemon.core.peers.gate.lock().unwrap() = Some(gate.clone());
    (daemon, gate)
}

/// A connection whose peer is this test process, which no agent contains.
fn connect(core: &Arc<crate::Core>) -> (tokio::net::UnixStream, tokio::task::JoinHandle<()>) {
    let pid = i32::try_from(std::process::id()).unwrap();
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    let task = tokio::spawn(crate::api::serve_with_peer(
        server,
        core.clone(),
        move |_| Ok(pid),
    ));
    (client, task)
}

/// One request and its one answer, which must come within `wait`.
async fn ask(
    client: &mut tokio::net::UnixStream,
    request: agmen_local::Request,
    wait: std::time::Duration,
) -> agmen_local::Outcome {
    let method = request.method.clone();
    agmen_local::frame::write_async(client, &request)
        .await
        .unwrap();
    let answer: agmen_local::Response =
        tokio::time::timeout(wait, agmen_local::frame::read_async(client))
            .await
            .unwrap_or_else(|_| panic!("{method} was not answered within {wait:?}"))
            .unwrap();
    answer.outcome
}

#[tokio::test]
async fn a_slow_ancestry_walk_delays_only_the_first_owner_only_call() {
    use agmen_local::{Outcome, Request, Response, frame};
    use serde_json::json;

    let dir = tempfile::tempdir().unwrap();
    let (daemon, gate) = gated_daemon(dir.path());
    let core = daemon.core.clone();
    let project = core
        .origins
        .local()
        .create_project("lazy", "owner")
        .await
        .unwrap()
        .project()
        .clone();
    let reader: agmen_client::IdentityFile = serde_json::from_value(
        core.identities
            .local_mint(&core.managed, json!({"project": project, "name": "reader"}))
            .await
            .unwrap(),
    )
    .unwrap();
    let token = core
        .identities
        .local_token(&reader.identity_id)
        .await
        .unwrap()
        .unwrap();

    let (mut client, task) = connect(&core);
    // The gate is shut, so a walk would hold hello past the CLI's budget.
    let hello = ask(
        &mut client,
        Request::new(1, "hello", json!({"api": 1})),
        PROMPT,
    )
    .await;
    assert!(matches!(hello, Outcome::Ok { .. }), "{hello:?}");
    // A call that carries its own identity does not wait either.
    let mut state = Request::new(
        2,
        "state",
        json!({"project": project, "key": "roster", "local_token": token.expose()}),
    );
    state.as_ = Some(reader.identity_id.clone());
    let state = ask(&mut client, state, PROMPT).await;
    assert!(matches!(state, Outcome::Ok { .. }), "{state:?}");

    // The first owner-only call waits for the walk...
    frame::write_async(&mut client, &Request::new(3, "agent_list", json!({})))
        .await
        .unwrap();
    let early = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        frame::read_async::<Response>(&mut client),
    )
    .await;
    assert!(early.is_err(), "answered before the walk ended: {early:?}");
    gate.add_permits(1);
    let answer: Response = tokio::time::timeout(probe::WAIT, frame::read_async(&mut client))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(answer.outcome, Outcome::Ok { .. }), "{answer:?}");
    // ...and the connection keeps the answer: the walk's one permit is spent.
    let again = ask(
        &mut client,
        Request::new(4, "agent_list", json!({})),
        PROMPT,
    )
    .await;
    assert!(matches!(again, Outcome::Ok { .. }), "{again:?}");
    drop(client);
    task.await.unwrap();
}

#[tokio::test]
async fn a_walk_that_never_ends_refuses_the_owner_only_call_after_its_bound() {
    use agmen_local::{ErrorCode, Outcome, Request};
    use serde_json::json;

    let dir = tempfile::tempdir().unwrap();
    // The gate stays shut, so the walk can only end at its 2 s bound.
    let (daemon, _gate) = gated_daemon(dir.path());
    let (mut client, task) = connect(&daemon.core);
    let started = std::time::Instant::now();
    let hello = ask(
        &mut client,
        Request::new(1, "hello", json!({"api": 1})),
        PROMPT,
    )
    .await;
    assert!(matches!(hello, Outcome::Ok { .. }), "{hello:?}");
    let refused = ask(
        &mut client,
        Request::new(2, "agent_list", json!({})),
        probe::WAIT,
    )
    .await;
    let Outcome::Err { error } = refused else {
        panic!("{refused:?}");
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(error.message, "agents cannot call owner-only methods");
    assert!(started.elapsed() >= std::time::Duration::from_secs(2));
    drop(client);
    task.await.unwrap();
}
