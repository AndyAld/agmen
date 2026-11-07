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
