use super::*;
use crate::peer_tests as probe;

#[tokio::test]
async fn listener_new_session_child_cannot_use_owner_methods_or_human_post() {
    let fixture = Fixture::new("IFS= read -r line").await;
    probe::client(fixture.dir.path(), 1);
    let local_project = fixture
        .daemon
        .core
        .origins
        .local()
        .create_project("ancestry", "owner")
        .await
        .unwrap()
        .project()
        .clone();
    probe::requests(fixture.dir.path(), &local_project);
    let socket = tokio::net::UnixListener::bind(fixture.dir.path().join("peer.sock")).unwrap();
    fixture.start().await;
    let connection = probe::accept(&socket, fixture.daemon.core.clone()).await;
    let answers = probe::answers(fixture.dir.path()).await;
    connection.await.unwrap();
    probe::human_controls(
        fixture.daemon.core.clone(),
        fixture.dir.path(),
        &socket,
        &local_project,
    )
    .await;
    std::fs::write(fixture.dir.path().join("finish"), "done").unwrap();
    fixture.listeners().shutdown().await;
    probe::assert_refused(&answers);
}
