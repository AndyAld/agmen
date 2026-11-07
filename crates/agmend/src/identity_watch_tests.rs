//! A local watch's authority recheck answers `unauthorized` only for an
//! identity that is revoked or gone. A read that fails, runs out of time or
//! meets a stopped origin gets the answer the CLI retries.

use std::time::Duration;

use agmen_client::TokenStore;
use agmen_local::{ErrorCode, Outcome, frame};
use agmen_proto::IdentityKind;
use tokio::net::UnixStream;
use tokio::task::JoinHandle;

use super::*;
use crate::record::admin::LocalMint;
use crate::state::ManagedIdentity;

const WAIT: Duration = Duration::from_secs(20);
/// Long enough for a watch's opening replay to page the record.
const REPLAY: Duration = Duration::from_millis(300);

/// An answer from the CLI's retry table (agmen-cli `local_link.rs`), word for word.
fn retried(message: &str) -> ApiError {
    ApiError {
        code: ErrorCode::Hub,
        message: message.into(),
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    daemon: crate::Agmend,
    who: ManagedIdentity,
}

impl Fixture {
    /// A local project with one registered identity.
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let daemon = crate::Agmend::bind(crate::Config::new(
            dir.path().join("agmend.sock"),
            dir.path().join("login.json"),
            TokenStore::Dir(dir.path().join("tokens")),
        ))
        .unwrap();
        let local = daemon.core.origins.local();
        let topics = local.create_project("recheck", "andy").await.unwrap();
        let person = local.person().await.unwrap().unwrap();
        let minted = local
            .mint(LocalMint {
                project: topics.project().clone(),
                name: "sol".into(),
                kind: IdentityKind::Agent,
                spec: None,
                agent: None,
                delegated_by: None,
            })
            .await
            .unwrap();
        let who = ManagedIdentity {
            hub_url: agmen_client::local_hub_url(),
            broker_url: agmen_client::LOCAL_BROKER_URL.parse().unwrap(),
            tenant: topics.tenant().clone(),
            project: topics.project().clone(),
            identity_id: minted.id,
            owner_id: person.id,
            handle: minted.handle,
            kind: IdentityKind::Agent,
        };
        daemon.core.identities.register(who.clone()).await.unwrap();
        Self { dir, daemon, who }
    }

    /// Opens the identity's watch and returns once its `self` event is out. The
    /// stream is the client's end, which must stay open for the watch to run.
    async fn watch(&self) -> (JoinHandle<Result<(), ApiError>>, UnixStream) {
        let (mut client, server) = UnixStream::pair().unwrap();
        let (reader, mut writer) = server.into_split();
        let identities = self.daemon.core.identities.clone();
        let who = self.who.clone();
        let task = tokio::spawn(async move {
            let raw = json!({"project": who.project});
            identities
                .watch(&Caller::Identity(who), 7, raw, reader, &mut writer)
                .await
        });
        let first: Response = frame::read_async(&mut client).await.unwrap();
        assert!(matches!(first.outcome, Outcome::Ok { .. }), "{first:?}");
        (task, client)
    }

    /// Runs `sql` on the daemon's own database from another connection.
    async fn state(&self, sql: &'static str) {
        let path = self.dir.path().join("agmend.db");
        tokio::task::spawn_blocking(move || {
            rusqlite::Connection::open(path)
                .unwrap()
                .execute_batch(sql)
                .unwrap();
        })
        .await
        .unwrap();
    }

    async fn stop(self) {
        self.daemon.core.identities.shutdown().await;
        self.daemon.core.origins.local().shutdown().await;
    }
}

/// The error a watch ended with, which `api::serve` writes as its last frame.
async fn ended(watch: JoinHandle<Result<(), ApiError>>) -> ApiError {
    tokio::time::timeout(WAIT, watch)
        .await
        .expect("the recheck must end the watch")
        .unwrap()
        .expect_err("the watch ended without an error")
}

#[tokio::test]
async fn a_failed_registry_read_ends_the_watch_with_the_retryable_error() {
    let f = Fixture::new().await;
    let (watch, _client) = f.watch().await;
    f.state("UPDATE identities SET metadata = 'not json'").await;
    assert_eq!(ended(watch).await, retried("local.db is busy; try again"));
    f.stop().await;
}

#[tokio::test]
async fn a_timed_out_registry_read_ends_the_watch_with_the_retryable_error() {
    let f = Fixture::new().await;
    let (watch, _client) = f.watch().await;
    // The registry stays locked past the 5 s the recheck's read waits for it.
    let store = f.daemon.core.identities.store.store.clone();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let (locked, ready) = tokio::sync::oneshot::channel();
    let holder = tokio::task::spawn_blocking(move || {
        let _held = store.lock().unwrap();
        locked.send(()).unwrap();
        let _released = released.recv_timeout(WAIT);
    });
    ready.await.unwrap();
    let error = ended(watch).await;
    release.send(()).unwrap();
    holder.await.unwrap();
    assert_eq!(error, retried("local.db is busy; try again"));
    f.stop().await;
}

#[tokio::test]
async fn a_stopped_origin_ends_the_watch_with_the_stopped_answer() {
    let f = Fixture::new().await;
    let (watch, _client) = f.watch().await;
    // A replay page that met the stop would end the watch before the recheck does.
    tokio::time::sleep(REPLAY).await;
    f.daemon.core.origins.local().shutdown().await;
    assert_eq!(ended(watch).await, retried("the local origin has stopped"));
    f.stop().await;
}

#[tokio::test]
async fn a_revoked_identity_still_ends_the_watch_with_unauthorized() {
    let f = Fixture::new().await;
    let (watch, _client) = f.watch().await;
    // What `agent_remove` does: revoke the identity in the record, then forget it.
    f.daemon
        .core
        .origins
        .local()
        .revoke(&f.who.project, &f.who.identity_id)
        .await
        .unwrap();
    f.daemon.core.identities.forget(&f.who).await.unwrap();
    assert_eq!(ended(watch).await, unauthorized());
    f.stop().await;
}
