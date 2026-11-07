//! The check every request of an identity passes before any method runs,
//! `Identities::authorize`. It answers `unauthorized` only for a caller that is
//! unknown, revoked or holds the wrong credential. A read that fails, runs out
//! of time or meets a stopped origin says nothing against the caller: it is
//! refused with the answer the CLI retries, and nothing is served after it.

use std::path::{Path, PathBuf};
use std::time::Instant;

use agmen_local::Request;
use agmen_proto::IdentityId;
use serde_json::Value;

use super::*;

const BUSY: &str = "local.db is busy; try again";
const STOPPED: &str = "the local origin has stopped";
/// Hides the registry table, so reading it fails as any store failure does,
/// and shows it again.
const HIDE_REGISTRY: &str = "ALTER TABLE identities RENAME TO identities_hidden";
const SHOW_REGISTRY: &str = "ALTER TABLE identities_hidden RENAME TO identities";
/// Every method agmend dispatches after `authorize`: the four with arms of
/// their own, the calls an identity makes, and some it is refused.
const METHODS: [&str; 22] = [
    "watch",
    "inbox",
    "state",
    "pending",
    "post",
    "page",
    "lanes",
    "items",
    "check_path",
    "child_launch",
    "child_stop",
    "git_log",
    "git_show",
    "git_diff",
    "git_status",
    "settings_get",
    "settings_apply",
    "roster",
    "agent_list",
    "whoami",
    "hello",
    "no_such_method",
];

/// A pipe nobody writes to stops a reader. Opening it for reading and writing
/// never blocks, and closing it again gives the reader end of file.
struct Release(PathBuf);

impl Drop for Release {
    fn drop(&mut self) {
        drop(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.0),
        );
    }
}

fn mkfifo(path: &Path) {
    let made = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .unwrap();
    assert!(made.success(), "mkfifo {path:?}");
}

/// The one credential the daemon stored for the fixture's identity.
fn credential_path(f: &Fixture) -> PathBuf {
    let mut files = std::fs::read_dir(f.dir.path().join("tokens")).unwrap();
    let path = files.next().unwrap().unwrap().path();
    assert!(files.next().is_none(), "one credential is stored");
    path
}

async fn token(f: &Fixture) -> String {
    let token = f
        .daemon
        .core
        .identities
        .local_token(&f.who.identity_id)
        .await
        .unwrap()
        .unwrap();
    token.expose().to_owned()
}

async fn authorize(f: &Fixture, who: &IdentityId, token: &str) -> Result<Caller, ApiError> {
    f.daemon
        .core
        .identities
        .authorize(Some(who), Some(token))
        .await
}

/// The fixture's identity is let in, so a refusal in a test is the fault under test.
async fn assert_admitted(f: &Fixture, token: &str) {
    match authorize(f, &f.who.identity_id, token).await {
        Ok(Caller::Identity(row)) => assert_eq!(row, f.who),
        other => panic!("the identity was not admitted: {other:?}"),
    }
}

fn envelope(id: &str) -> Value {
    json!({"v": 1, "id": id, "verb": "FYI", "data": {}, "body": "hello", "sent_at": "2026-10-27T12:00:00Z"})
}

/// Params that would make `method` do its work, where it takes any.
fn params(f: &Fixture, method: &str) -> Value {
    match method {
        "watch" | "inbox" => json!({"project": f.who.project}),
        "state" => json!({"project": f.who.project, "key": "head"}),
        "page" => json!({"stream": "feed"}),
        "check_path" => json!({"paths": []}),
        "post" => json!({"envelope": envelope("01JBX6Q4T7W3C2M8Y9K5N0P1RT")}),
        // The dispatcher checks `hello`'s params before it authorizes.
        "hello" => json!({"api": 1}),
        _ => json!({}),
    }
}

/// One request and its one answer, as the CLI sends it: the identity in `as`
/// and its credential in the params.
async fn ask(
    client: &mut UnixStream,
    id: u64,
    who: Option<&IdentityId>,
    method: &str,
    mut params: Value,
    token: Option<&str>,
) -> Response {
    if let Some(token) = token {
        params["local_token"] = json!(token);
    }
    let mut request = Request::new(id, method, params);
    request.as_ = who.cloned();
    tokio::time::timeout(WAIT, async {
        frame::write_async(client, &request).await.unwrap();
        frame::read_async(client).await.unwrap()
    })
    .await
    .expect("agmend answers")
}

/// A connection to `api::serve` that has said `hello`.
async fn connect(f: &Fixture) -> (UnixStream, JoinHandle<()>) {
    let (mut client, server) = UnixStream::pair().unwrap();
    let serving = tokio::spawn(crate::api::serve(server, f.daemon.core.clone()));
    let hello = ask(&mut client, 1, None, "hello", json!({"api": 1}), None).await;
    assert!(matches!(hello.outcome, Outcome::Ok { .. }), "{hello:?}");
    (client, serving)
}

async fn close(client: UnixStream, serving: JoinHandle<()>) {
    drop(client);
    tokio::time::timeout(WAIT, serving)
        .await
        .expect("the connection ends")
        .unwrap();
}

fn refusal(response: Response, method: &str) -> ApiError {
    match response.outcome {
        Outcome::Err { error } => error,
        Outcome::Ok { result } => panic!("{method} was served: {result}"),
    }
}

/// Every method, over one connection, as `who` with `token`. Each is refused
/// with `expected` and nothing else, and the connection then reads and answers
/// the next request, so no watch took it over.
async fn refused_everywhere(f: &Fixture, who: &IdentityId, token: &str, expected: &ApiError) {
    let (mut client, serving) = connect(f).await;
    for (n, method) in METHODS.into_iter().enumerate() {
        let id = 100 + n as u64;
        let answer = ask(
            &mut client,
            id,
            Some(who),
            method,
            params(f, method),
            Some(token),
        )
        .await;
        assert_eq!(answer.id, id, "{method}");
        assert_eq!(&refusal(answer, method), expected, "{method}");
        let next = ask(
            &mut client,
            id + 1000,
            None,
            "hello",
            json!({"api": 1}),
            None,
        )
        .await;
        assert_eq!(next.id, id + 1000, "{method}: {next:?}");
        assert!(matches!(next.outcome, Outcome::Ok { .. }), "{method}");
    }
    close(client, serving).await;
}

async fn head(f: &Fixture) -> u64 {
    f.daemon
        .core
        .origins
        .local()
        .head(&f.who.project)
        .await
        .unwrap()
}

/// The record is as it was and the identity holds no presence: nothing was
/// posted and no watch opened.
async fn assert_untouched(f: &Fixture, head_before: u64) {
    assert_eq!(head(f).await, head_before, "a refused call was recorded");
    let roster = f
        .daemon
        .core
        .origins
        .local()
        .state(&f.who.project, "roster")
        .await
        .unwrap();
    let online = roster["value"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["identity_id"] == json!(f.who.identity_id) && row["status"] == "online");
    assert!(!online, "a refused watch held presence");
}

#[tokio::test]
async fn a_failed_store_read_at_connect_is_busy_and_nothing_is_served() {
    let f = Fixture::new().await;
    let token = token(&f).await;
    assert_admitted(&f, &token).await;
    // One message is recorded while the registry reads, so a post that got
    // past the refusal below would move the head.
    let (mut client, serving) = connect(&f).await;
    let posted = ask(
        &mut client,
        5,
        Some(&f.who.identity_id),
        "post",
        json!({"envelope": envelope("01JBX6Q4T7W3C2M8Y9K5N0P1RS")}),
        Some(&token),
    )
    .await;
    assert!(matches!(posted.outcome, Outcome::Ok { .. }), "{posted:?}");
    close(client, serving).await;
    let head_before = head(&f).await;
    assert_eq!(head_before, 1);

    f.state(HIDE_REGISTRY).await;
    assert_eq!(
        authorize(&f, &f.who.identity_id, &token).await.unwrap_err(),
        retried(BUSY)
    );
    refused_everywhere(&f, &f.who.identity_id, &token, &retried(BUSY)).await;
    assert_untouched(&f, head_before).await;
    // The person is not asked to read the registry.
    assert!(matches!(
        f.daemon.core.identities.authorize(None, None).await,
        Ok(Caller::Human)
    ));

    f.state(SHOW_REGISTRY).await;
    assert_admitted(&f, &token).await;
    f.stop().await;
}

#[tokio::test]
async fn an_unreadable_credential_at_connect_is_busy_and_nothing_is_served() {
    let f = Fixture::new().await;
    let token = token(&f).await;
    let head_before = head(&f).await;
    // A directory where the credential file was: the read fails, and the
    // credential is not shown to be wrong.
    let path = credential_path(&f);
    let kept = path.with_extension("kept");
    std::fs::rename(&path, &kept).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(
        authorize(&f, &f.who.identity_id, &token).await.unwrap_err(),
        retried(BUSY)
    );
    refused_everywhere(&f, &f.who.identity_id, &token, &retried(BUSY)).await;
    assert_untouched(&f, head_before).await;

    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&kept, &path).unwrap();
    assert_admitted(&f, &token).await;
    f.stop().await;
}

#[tokio::test]
async fn a_read_that_outlasts_the_deadline_at_connect_is_busy_and_nothing_is_served() {
    let f = Fixture::new().await;
    let token = token(&f).await;
    let head_before = head(&f).await;
    // The credential becomes a pipe nobody writes to, so reading it blocks and
    // only the 10 s deadline ends the call. This is the one real wait here:
    // agmend's tests have no paused clock, and no inner read gives up sooner.
    let path = credential_path(&f);
    std::fs::remove_file(&path).unwrap();
    mkfifo(&path);
    let release = Release(path);
    let (mut client, serving) = connect(&f).await;
    let started = Instant::now();
    let answer = ask(
        &mut client,
        7,
        Some(&f.who.identity_id),
        "watch",
        params(&f, "watch"),
        Some(&token),
    )
    .await;
    let waited = started.elapsed();
    assert!(
        waited >= DEADLINE,
        "refused after {waited:?}, before the deadline"
    );
    assert_eq!(refusal(answer, "watch"), retried(BUSY));
    let next = ask(&mut client, 8, None, "hello", json!({"api": 1}), None).await;
    assert!(matches!(next.outcome, Outcome::Ok { .. }), "{next:?}");
    assert_untouched(&f, head_before).await;
    close(client, serving).await;
    drop(release);
    f.stop().await;
}

#[tokio::test]
async fn a_stopped_origin_at_connect_is_stopped_and_nothing_is_served() {
    let f = Fixture::new().await;
    let token = token(&f).await;
    f.daemon.core.origins.local().shutdown().await;
    assert_eq!(
        authorize(&f, &f.who.identity_id, &token).await.unwrap_err(),
        retried(STOPPED)
    );
    refused_everywhere(&f, &f.who.identity_id, &token, &retried(STOPPED)).await;
    f.stop().await;
}

#[tokio::test]
async fn a_revoked_identity_at_connect_is_unauthorized_and_nothing_is_served() {
    let f = Fixture::new().await;
    let token = token(&f).await;
    // What `agent_remove` does: revoke the identity in the record, then forget
    // it. The caller still holds the credential it was given.
    let local = f.daemon.core.origins.local();
    local
        .revoke(&f.who.project, &f.who.identity_id)
        .await
        .unwrap();
    f.daemon.core.identities.forget(&f.who).await.unwrap();
    let head_before = head(&f).await;
    assert_eq!(
        authorize(&f, &f.who.identity_id, &token).await.unwrap_err(),
        unauthorized()
    );
    refused_everywhere(&f, &f.who.identity_id, &token, &unauthorized()).await;
    assert_untouched(&f, head_before).await;
    f.stop().await;
}

#[tokio::test]
async fn an_unknown_identity_or_a_wrong_credential_at_connect_is_unauthorized() {
    let f = Fixture::new().await;
    let token = token(&f).await;
    let head_before = head(&f).await;
    // An identity nobody registered, with a well formed credential, even a real one.
    let ghost: IdentityId = "ghost001".parse().unwrap();
    assert_eq!(
        authorize(&f, &ghost, &token).await.unwrap_err(),
        unauthorized()
    );
    refused_everywhere(&f, &ghost, &token, &unauthorized()).await;
    // A registered identity with another credential.
    let wrong = if token.starts_with('a') { "b" } else { "a" }.repeat(64);
    assert_eq!(
        authorize(&f, &f.who.identity_id, &wrong).await.unwrap_err(),
        unauthorized()
    );
    refused_everywhere(&f, &f.who.identity_id, &wrong, &unauthorized()).await;
    // A stored credential that reads but is not one.
    std::fs::write(credential_path(&f), "not json").unwrap();
    assert_eq!(
        authorize(&f, &f.who.identity_id, &token).await.unwrap_err(),
        unauthorized()
    );
    assert_untouched(&f, head_before).await;
    f.stop().await;
}
