use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use agmen_core::{MemberRole, StoreError};
use agmen_proto::{EditScope, IdentityKind, RoleSpec, Timestamp, Topics};
use rusqlite::params;
use tempfile::TempDir;

use super::{LocalMint, MAX_LOCAL_PROJECTS, MintError, clean_person_handle, is_person_handle};
use crate::record::{Clock, SqliteStore, backend, micros, schema, test_clock, test_rows};

const NOW: &str = "2026-11-02T12:00:00Z";
const BAD_DELEGATOR: &str =
    "delegated_by must name a live identity of this owner and project for an agent child";
const PROJECT_FIXTURES: usize = MAX_LOCAL_PROJECTS + 1;

async fn store() -> (TempDir, SqliteStore) {
    store_with(Arc::new(|| NOW.parse().unwrap())).await
}

async fn store_with(clock: Clock) -> (TempDir, SqliteStore) {
    tokio::task::spawn_blocking(move || {
        let dir = tempfile::tempdir().unwrap();
        let store = schema::open(&dir.path().join(schema::RECORD_DB), clock).unwrap();
        (dir, store)
    })
    .await
    .unwrap()
}

async fn project() -> (TempDir, SqliteStore, Topics) {
    let (dir, store) = store().await;
    store.create_person("andy").await.unwrap();
    let topics = store.create_project("agmen").await.unwrap();
    (dir, store, topics)
}

fn request(topics: &Topics, name: &str, kind: IdentityKind) -> LocalMint {
    LocalMint {
        project: topics.project().clone(),
        name: name.into(),
        kind,
        spec: None,
        agent: None,
        delegated_by: None,
    }
}

fn spec(role: &str, edits: EditScope) -> RoleSpec {
    RoleSpec {
        role: role.parse().unwrap(),
        verbs: BTreeSet::from([
            "FYI".into(),
            "GRANT".into(),
            "ASSIGN".into(),
            "unknown".into(),
        ]),
        edits,
        role_sha256: "a".repeat(64).parse().unwrap(),
    }
}

fn error(error: MintError, code: &str, message: &str) {
    assert_eq!(error.code, code);
    assert_eq!(error.message, message);
}

fn valid_id(id: &str) {
    const ID_LEN: usize = 16;
    assert_eq!(id.len(), ID_LEN);
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    );
}

#[tokio::test]
async fn first_person_is_created_once() {
    let (dir, store) = store().await;
    assert_eq!(store.person().await.unwrap(), None);
    assert!(store.projects().await.unwrap().is_empty());
    assert!(store.create_project("before-person").await.is_err());
    let (first, second) = tokio::join!(store.create_person("andy"), store.create_person("andy"));
    let first = first.unwrap();
    assert_eq!(first, second.unwrap());
    assert_eq!(first.handle, "andy");
    valid_id(first.tenant.as_str());
    valid_id(first.id.as_str());
    assert_ne!(first.tenant.as_str(), first.id.as_str());
    assert_eq!(store.create_person("sean").await.unwrap(), first);
    assert_eq!(store.person().await.unwrap(), Some(first.clone()));
    store
        .read(|conn, _| {
            let rows: (i64, i64, i64, i64) = conn
                .query_row(
                    "SELECT (SELECT count(*) FROM tenants), (SELECT count(*) FROM humans),
                    (SELECT count(*) FROM local_origin), created_at FROM local_origin",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(backend)?;
            assert_eq!(rows, (1, 1, 1, micros(NOW.parse().unwrap())));
            Ok(())
        })
        .await
        .unwrap();
    drop(store);
    let path = dir.path().join(schema::RECORD_DB);
    let reopened =
        tokio::task::spawn_blocking(move || schema::open(&path, Arc::new(Timestamp::now)).unwrap())
            .await
            .unwrap();
    assert_eq!(reopened.create_person("changed").await.unwrap(), first);
}

#[tokio::test]
async fn mint_checks_the_handle() {
    let (_dir, store, topics) = project().await;
    let message = "the handle must be andy/<name>, where name is 1 to 32 of [a-z0-9-] starting with a letter or digit";
    for name in [
        "Bad_Name",
        "andy/Bad_Name",
        "sean/x",
        "",
        "-x",
        &"x".repeat(33),
    ] {
        error(
            store
                .mint(request(&topics, name, IdentityKind::Agent))
                .await
                .unwrap_err(),
            "bad_handle",
            message,
        );
    }
    let minted = store
        .mint(request(&topics, "x-2", IdentityKind::Agent))
        .await
        .unwrap();
    assert_eq!(minted.handle, "andy/x-2");
    assert_eq!(minted.topics, topics);
    assert_eq!(minted.verbs, None);
    valid_id(minted.id.as_str());
    let unknown = Topics::new(topics.tenant().clone(), "unknown1".parse().unwrap());
    error(
        store
            .mint(request(&unknown, "x", IdentityKind::Agent))
            .await
            .unwrap_err(),
        "not_a_member",
        "you are not a member of that project",
    );
    let foreign = store
        .write(|conn, _| Ok(test_rows::project(conn)))
        .await
        .unwrap();
    assert_eq!(store.topics(foreign.project()).await.unwrap(), None);
    error(
        store
            .mint(request(&foreign, "x", IdentityKind::Agent))
            .await
            .unwrap_err(),
        "not_a_member",
        "you are not a member of that project",
    );
    for kind in [IdentityKind::Human, IdentityKind::Hub] {
        assert!(store.mint(request(&topics, "human", kind)).await.is_err());
    }
    let mut too_long = request(&topics, "long-agent", IdentityKind::Agent);
    too_long.agent = Some("é".repeat(agmen_proto::MAX_HELLO_FIELD_BYTES / 2 + 1));
    error(
        store.mint(too_long).await.unwrap_err(),
        "bad_role",
        "agent is too long",
    );
    let mut boundary = request(&topics, "agent", IdentityKind::Agent);
    boundary.agent = Some("x".repeat(agmen_proto::MAX_HELLO_FIELD_BYTES));
    let minted = store.mint(boundary).await.unwrap();
    store
        .read(move |conn, _| {
            let (kind, owner, created, agent): (String, String, i64, String) = conn
                .query_row(
                    "SELECT kind, owner_human_id, created_at, agent FROM identities WHERE id=?1",
                    [minted.id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(backend)?;
            assert_eq!(kind, "agent");
            valid_id(&owner);
            assert_eq!(created, micros(NOW.parse().unwrap()));
            assert_eq!(agent.len(), agmen_proto::MAX_HELLO_FIELD_BYTES);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn live_handles_are_unique_until_revoked() {
    let (_dir, store, topics) = project().await;
    let req = request(&topics, "worker", IdentityKind::Agent);
    let (a, b) = tokio::join!(store.mint(req.clone()), store.mint(req.clone()));
    let (first, duplicate) = match (a, b) {
        (Ok(first), Err(duplicate)) | (Err(duplicate), Ok(first)) => (first, duplicate),
        other => panic!("expected one mint and one conflict: {other:?}"),
    };
    error(
        duplicate,
        "handle_taken",
        "a live identity in this project is already andy/worker",
    );
    let other = store.create_project("other").await.unwrap();
    store
        .mint(request(&other, "worker", IdentityKind::Agent))
        .await
        .unwrap();
    assert!(store.revoke(topics.project(), &first.id).await.unwrap());
    let second = store.mint(req).await.unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(first.handle, second.handle);
}

#[tokio::test]
async fn one_listener_per_person_per_project() {
    let (_dir, store, topics) = project().await;
    let first = store
        .mint(request(&topics, "listener", IdentityKind::Listener))
        .await
        .unwrap();
    error(
        store
            .mint(request(&topics, "other-listener", IdentityKind::Listener))
            .await
            .unwrap_err(),
        "listener_exists",
        "this owner already has a live listener in this project",
    );
    let other = store.create_project("other").await.unwrap();
    store
        .mint(request(&other, "listener", IdentityKind::Listener))
        .await
        .unwrap();
    assert!(store.revoke(topics.project(), &first.id).await.unwrap());
    store
        .mint(request(&topics, "other-listener", IdentityKind::Listener))
        .await
        .unwrap();
}

#[tokio::test]
async fn verbs_are_capped_by_kind() {
    let (_dir, store, topics) = project().await;
    let mut req = request(&topics, "engineer", IdentityKind::Agent);
    req.spec = Some(spec("engineer", EditScope::Lanes));
    let minted = store.mint(req).await.unwrap();
    assert_eq!(minted.verbs, Some(BTreeSet::from(["FYI".into()])));
    store
        .read(move |conn, _| {
            let (role, verbs, edits, sha): (String, String, String, String) = conn
                .query_row(
                    "SELECT role, verbs, edits, role_sha256 FROM identities WHERE id=?1",
                    [minted.id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(backend)?;
            assert_eq!(role, "engineer");
            assert_eq!(
                serde_json::from_str::<BTreeSet<String>>(&verbs).unwrap(),
                BTreeSet::from(["FYI".into()])
            );
            assert_eq!(
                serde_json::from_str::<EditScope>(&edits).unwrap(),
                EditScope::Lanes
            );
            assert_eq!(sha, "a".repeat(64));
            Ok(())
        })
        .await
        .unwrap();
    let mut listener = request(&topics, "listener", IdentityKind::Listener);
    listener.spec = Some(spec("listener", EditScope::Lanes));
    error(
        store.mint(listener.clone()).await.unwrap_err(),
        "bad_role",
        "a listener's edits must be none",
    );
    listener.spec = Some(spec("listener", EditScope::None));
    let minted = store.mint(listener).await.unwrap();
    assert!(!minted.verbs.unwrap().contains("ASSIGN"));
    store.revoke(topics.project(), &minted.id).await.unwrap();
    let mut listener = request(&topics, "listener", IdentityKind::Listener);
    listener.spec = Some(spec("coordinator", EditScope::None));
    assert!(
        store
            .mint(listener)
            .await
            .unwrap()
            .verbs
            .unwrap()
            .contains("ASSIGN")
    );
    let mut invalid_edits = request(&topics, "bad-edits", IdentityKind::Agent);
    invalid_edits.spec = Some(spec("engineer", EditScope::Paths { globs: vec![] }));
    error(
        store.mint(invalid_edits).await.unwrap_err(),
        "bad_edits",
        "expected 1 to 32 globs",
    );
    let mut coordinator = request(&topics, "coordinator", IdentityKind::Coordinator);
    coordinator.spec = Some(spec("coordinator", EditScope::Lanes));
    assert_eq!(
        store.mint(coordinator).await.unwrap().verbs,
        Some(BTreeSet::from(["ASSIGN".into(), "FYI".into()]))
    );
}

#[tokio::test]
async fn delegated_by_must_be_a_live_listener_of_the_same_person() {
    let (_dir, store, topics) = project().await;
    let person = store.person().await.unwrap().unwrap();
    let listener = store
        .mint(request(&topics, "listener", IdentityKind::Listener))
        .await
        .unwrap();
    let agent = store
        .mint(request(&topics, "agent", IdentityKind::Agent))
        .await
        .unwrap();
    let other = store.create_project("other").await.unwrap();
    let elsewhere = store
        .mint(request(&other, "listener", IdentityKind::Listener))
        .await
        .unwrap();
    let member = store.add_member(&topics, MemberRole::Member).await.unwrap();
    valid_id(member.as_str());
    let t = topics.clone();
    let foreign = store
        .write(move |conn, _| Ok(test_rows::identity(conn, &t, "listener", &member)))
        .await
        .unwrap();
    let mut req = request(&topics, "worker", IdentityKind::Agent);
    let mut agent_child = request(&topics, "agent-child", IdentityKind::Agent);
    agent_child.delegated_by = Some(agent.id);
    assert!(store.mint(agent_child).await.is_ok());
    for id in [elsewhere.id, foreign, "unknown1".parse().unwrap()] {
        req.delegated_by = Some(id);
        error(
            store.mint(req.clone()).await.unwrap_err(),
            "bad_role",
            BAD_DELEGATOR,
        );
    }
    req.delegated_by = Some(listener.id.clone());
    req.kind = IdentityKind::Coordinator;
    error(
        store.mint(req.clone()).await.unwrap_err(),
        "bad_role",
        BAD_DELEGATOR,
    );
    req.kind = IdentityKind::Agent;
    let worker = store.mint(req.clone()).await.unwrap();
    let t = topics.clone();
    let listener_id = listener.id.clone();
    store
        .read(move |conn, _| {
            super::check_delegator(conn, t.project(), &person.id, &listener_id).unwrap();
            let id: String = conn
                .query_row(
                    "SELECT delegated_by FROM identities WHERE id=?1",
                    [worker.id.as_str()],
                    |row| row.get(0),
                )
                .map_err(backend)?;
            assert_eq!(id, listener_id.as_str());
            Ok(())
        })
        .await
        .unwrap();
    store.revoke(topics.project(), &listener.id).await.unwrap();
    req.name = "after-revocation".into();
    error(
        store.mint(req).await.unwrap_err(),
        "bad_role",
        BAD_DELEGATOR,
    );
}

#[tokio::test]
async fn delegated_by_accepts_any_live_parent_of_owner() {
    let (_dir, store, topics) = project().await;
    for (name, kind) in [
        ("agent", IdentityKind::Agent),
        ("coordinator", IdentityKind::Coordinator),
        ("listener", IdentityKind::Listener),
    ] {
        let parent = store.mint(request(&topics, name, kind)).await.unwrap();
        let mut child = request(&topics, &format!("child-{name}"), IdentityKind::Agent);
        child.delegated_by = Some(parent.id);
        assert!(store.mint(child).await.is_ok());
    }
}

#[tokio::test]
async fn delegated_by_refuses_a_revoked_parent() {
    let (_dir, store, topics) = project().await;
    let parent = store
        .mint(request(&topics, "parent", IdentityKind::Agent))
        .await
        .unwrap();
    store.revoke(topics.project(), &parent.id).await.unwrap();
    let mut child = request(&topics, "child", IdentityKind::Agent);
    child.delegated_by = Some(parent.id);
    error(
        store.mint(child).await.unwrap_err(),
        "bad_role",
        BAD_DELEGATOR,
    );
}

#[tokio::test]
async fn delegated_by_refuses_another_owners_parent() {
    let (_dir, store, topics) = project().await;
    let other = store.add_member(&topics, MemberRole::Member).await.unwrap();
    let t = topics.clone();
    let parent = store
        .write(move |conn, _| Ok(test_rows::identity(conn, &t, "agent", &other)))
        .await
        .unwrap();
    let mut child = request(&topics, "child", IdentityKind::Agent);
    child.delegated_by = Some(parent);
    error(
        store.mint(child).await.unwrap_err(),
        "bad_role",
        BAD_DELEGATOR,
    );
}

#[tokio::test]
async fn delegated_by_refuses_a_parent_in_another_project() {
    let (_dir, store, topics) = project().await;
    let other = store.create_project("other").await.unwrap();
    let parent = store
        .mint(request(&other, "parent", IdentityKind::Agent))
        .await
        .unwrap();
    let mut child = request(&topics, "child", IdentityKind::Agent);
    child.delegated_by = Some(parent.id);
    error(
        store.mint(child).await.unwrap_err(),
        "bad_role",
        BAD_DELEGATOR,
    );
}

#[tokio::test]
async fn revoke_is_idempotent() {
    let (_dir, store, topics) = project().await;
    let minted = store
        .mint(request(&topics, "worker", IdentityKind::Agent))
        .await
        .unwrap();
    let other = store.create_project("other").await.unwrap();
    assert!(!store.revoke(other.project(), &minted.id).await.unwrap());
    assert!(
        !store
            .revoke(topics.project(), &"unknown1".parse().unwrap())
            .await
            .unwrap()
    );
    assert!(store.revoke(topics.project(), &minted.id).await.unwrap());
    assert!(!store.revoke(topics.project(), &minted.id).await.unwrap());
    store
        .read(move |conn, _| {
            let revoked: i64 = conn
                .query_row(
                    "SELECT revoked_at FROM identities WHERE id=?1",
                    [minted.id.as_str()],
                    |row| row.get(0),
                )
                .map_err(backend)?;
            assert_eq!(revoked, micros(NOW.parse().unwrap()));
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn project_names_are_unique() {
    let (_dir, store, topics) = project().await;
    assert!(matches!(
        store.create_project("agmen").await,
        Err(StoreError::Duplicate)
    ));
    let other = store.create_project("b-project").await.unwrap();
    valid_id(topics.project().as_str());
    valid_id(other.project().as_str());
    assert_ne!(topics.project(), other.project());
    assert_eq!(topics.tenant(), other.tenant());
    assert_eq!(
        store.topics(topics.project()).await.unwrap(),
        Some(topics.clone())
    );
    assert_eq!(
        store.topics(&"unknown1".parse().unwrap()).await.unwrap(),
        None
    );
    assert_eq!(
        store.projects().await.unwrap(),
        vec![
            (topics.project().clone(), "agmen".into()),
            (other.project().clone(), "b-project".into())
        ]
    );
    let t = topics.clone();
    store
        .read(move |conn, _| {
            let (role, at): (String, i64) = conn
                .query_row(
                    "SELECT role, created_at FROM memberships WHERE project_id=?1",
                    [t.project().as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(backend)?;
            assert_eq!(role, "admin");
            assert_eq!(at, micros(NOW.parse().unwrap()));
            Ok(())
        })
        .await
        .unwrap();
    store
        .write(move |conn, now| {
            for i in 0..PROJECT_FIXTURES {
                conn.execute(
                    "INSERT INTO projects(id, tenant_id, name, created_at) VALUES (?1, ?2, ?1, ?3)",
                    params![format!("project{i:09}"), topics.tenant().as_str(), now],
                )
                .map_err(backend)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let projects = store.projects().await.unwrap();
    assert_eq!(projects.len(), MAX_LOCAL_PROJECTS);
}

const HANDLE_RULE: &str = "the handle must be andy/<name>, where name is 1 to 32 of [a-z0-9-] \
                           starting with a letter or digit";

async fn set_role(store: &SqliteStore, role: &'static str) {
    store
        .write(move |conn, _| {
            conn.execute("UPDATE memberships SET role=?1", [role])
                .map_err(backend)?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn count(store: &SqliteStore, table: &'static str) -> i64 {
    store
        .read(move |conn, _| {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(backend)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn coordinators_need_the_coordinator_role() {
    const MESSAGE: &str = "minting a coordinator needs the coordinator role in the project";
    let (_dir, store, topics) = project().await;
    set_role(&store, "member").await;
    let coordinator = request(&topics, "coordinator", IdentityKind::Coordinator);
    let mut by_role = request(&topics, "listener", IdentityKind::Listener);
    by_role.spec = Some(spec("coordinator", EditScope::None));
    let mut invalid_edits = coordinator.clone();
    invalid_edits.spec = Some(spec("coordinator", EditScope::Paths { globs: vec![] }));
    let mut bad_name = coordinator.clone();
    bad_name.name = "Bad_Name".into();
    // The role check follows the handle check and precedes the permission checks.
    for refused in [coordinator.clone(), by_role.clone(), invalid_edits] {
        error(
            store.mint(refused).await.unwrap_err(),
            "coordinator_role_required",
            MESSAGE,
        );
    }
    error(
        store.mint(bad_name).await.unwrap_err(),
        "bad_handle",
        HANDLE_RULE,
    );
    assert_eq!(count(&store, "identities").await, 0);
    // A member still mints agents and a listener that is not the coordinator's.
    store
        .mint(request(&topics, "agent", IdentityKind::Agent))
        .await
        .unwrap();
    let mut plain = request(&topics, "listener", IdentityKind::Listener);
    plain.spec = Some(spec("listener", EditScope::None));
    let plain = store.mint(plain).await.unwrap();
    assert!(store.revoke(topics.project(), &plain.id).await.unwrap());
    for (role, name) in [("coordinator", "coord-a"), ("admin", "coord-b")] {
        set_role(&store, role).await;
        let mut named = coordinator.clone();
        named.name = name.into();
        store.mint(named).await.unwrap();
        let listener = store.mint(by_role.clone()).await.unwrap();
        assert!(listener.verbs.unwrap().contains("ASSIGN"));
        assert!(store.revoke(topics.project(), &listener.id).await.unwrap());
    }
}

#[tokio::test]
async fn an_unreadable_membership_role_refuses_every_mint() {
    let (_dir, store, topics) = project().await;
    store
        .write(|conn, _| {
            conn.execute_batch(
                "PRAGMA ignore_check_constraints=ON;
                 UPDATE memberships SET role='owner';
                 PRAGMA ignore_check_constraints=OFF;",
            )
            .map_err(backend)
        })
        .await
        .unwrap();
    for kind in [IdentityKind::Agent, IdentityKind::Coordinator] {
        error(
            store
                .mint(request(&topics, "worker", kind))
                .await
                .unwrap_err(),
            "server_error",
            "internal error",
        );
    }
    assert_eq!(count(&store, "identities").await, 0);
}

#[tokio::test]
async fn nobody_mints_before_the_person_exists() {
    let (_dir, store) = store().await;
    let unknown = Topics::new("tenant01".parse().unwrap(), "project1".parse().unwrap());
    error(
        store
            .mint(request(&unknown, "x", IdentityKind::Agent))
            .await
            .unwrap_err(),
        "not_a_member",
        "you are not a member of that project",
    );
}

#[tokio::test]
async fn internal_failures_do_not_reach_the_caller() {
    let (_dir, store, topics) = project().await;
    store
        .write(|conn, _| {
            conn.execute_batch("DROP TABLE memberships")
                .map_err(backend)
        })
        .await
        .unwrap();
    error(
        store
            .mint(request(&topics, "worker", IdentityKind::Agent))
            .await
            .unwrap_err(),
        "server_error",
        "internal error",
    );
}

#[tokio::test]
async fn a_second_listener_is_refused_as_a_listener_even_with_the_same_handle() {
    // Both unique keys conflict here. The listener runtime reads
    // `listener_exists` as "already running" (listener/runtime.rs).
    let (_dir, store, topics) = project().await;
    store
        .mint(request(&topics, "listener", IdentityKind::Listener))
        .await
        .unwrap();
    error(
        store
            .mint(request(&topics, "listener", IdentityKind::Listener))
            .await
            .unwrap_err(),
        "listener_exists",
        "this owner already has a live listener in this project",
    );
}

#[tokio::test]
async fn only_an_agent_worker_can_be_delegated() {
    let (_dir, store, topics) = project().await;
    let listener = store
        .mint(request(&topics, "listener", IdentityKind::Listener))
        .await
        .unwrap();
    for kind in [IdentityKind::Coordinator, IdentityKind::Listener] {
        let mut req = request(&topics, "worker", kind);
        req.delegated_by = Some(listener.id.clone());
        error(
            store.mint(req).await.unwrap_err(),
            "bad_role",
            BAD_DELEGATOR,
        );
    }
    assert_eq!(count(&store, "identities").await, 1);
}

#[tokio::test]
async fn revoking_again_keeps_the_first_revocation_time() {
    let at: Timestamp = NOW.parse().unwrap();
    let (clock, time) = test_clock(at);
    let (_dir, store) = store_with(clock).await;
    store.create_person("andy").await.unwrap();
    let topics = store.create_project("agmen").await.unwrap();
    let minted = store
        .mint(request(&topics, "worker", IdentityKind::Agent))
        .await
        .unwrap();
    let first = at + Duration::from_secs(5);
    *time.lock().unwrap() = first;
    assert!(store.revoke(topics.project(), &minted.id).await.unwrap());
    *time.lock().unwrap() = at + Duration::from_secs(60);
    assert!(!store.revoke(topics.project(), &minted.id).await.unwrap());
    let revoked: i64 = store
        .read(move |conn, _| {
            conn.query_row(
                "SELECT revoked_at FROM identities WHERE id=?1",
                [minted.id.as_str()],
                |row| row.get(0),
            )
            .map_err(backend)
        })
        .await
        .unwrap();
    assert_eq!(revoked, micros(first));
}

#[tokio::test]
async fn revoke_reaches_only_the_local_tenant() {
    let (_dir, store, _topics) = project().await;
    let (foreign, id) = store
        .write(|conn, _| {
            let topics = test_rows::project(conn);
            let owner = test_rows::human(conn, &topics, "admin");
            let id = test_rows::identity(conn, &topics, "agent", &owner);
            Ok((topics, id))
        })
        .await
        .unwrap();
    assert!(!store.revoke(foreign.project(), &id).await.unwrap());
    let live: bool = store
        .read(move |conn, _| {
            conn.query_row(
                "SELECT revoked_at IS NULL FROM identities WHERE id=?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .map_err(backend)
        })
        .await
        .unwrap();
    assert!(live);
}

#[tokio::test]
async fn the_record_has_no_place_for_a_token() {
    let (_dir, store) = store().await;
    let (tables, columns) = store
        .read(|conn, _| {
            let names = |sql: &str| -> Result<Vec<String>, StoreError> {
                conn.prepare(sql)
                    .map_err(backend)?
                    .query_map([], |row| row.get(0))
                    .map_err(backend)?
                    .map(|name| name.map_err(backend))
                    .collect()
            };
            Ok((
                names("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?,
                names("SELECT name FROM pragma_table_info('identities') ORDER BY cid")?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(
        tables,
        [
            "claims",
            "humans",
            "identities",
            "item_counters",
            "items",
            "lane_lends",
            "lanes",
            "local_origin",
            "memberships",
            "messages",
            "projects",
            "stream_heads",
            "tenant_usage",
            "tenants"
        ]
    );
    assert_eq!(
        columns,
        [
            "id",
            "tenant_id",
            "project_id",
            "kind",
            "handle",
            "owner_human_id",
            "created_at",
            "revoked_at",
            "role",
            "verbs",
            "edits",
            "role_sha256",
            "agent",
            "delegated_by"
        ]
    );
}

#[test]
fn person_handles_follow_the_hubs_rule() {
    let longest = "x".repeat(39);
    for good in ["a", "0", "me", "andy", "a-b", "a-", "0-9", &longest] {
        assert!(is_person_handle(good), "{good:?}");
    }
    let too_long = "x".repeat(40);
    for bad in [
        "", "-", "-x", "Andy", "a/b", "../x", "a b", "a.b", "a_b", "a\n", "a\0b", "\u{e9}",
        "\u{ff41}", "\u{212a}", &too_long,
    ] {
        assert!(!is_person_handle(bad), "{bad:?}");
    }
}

#[tokio::test]
async fn an_invalid_person_handle_creates_nothing() {
    let (_dir, store) = store().await;
    let too_long = "x".repeat(40);
    for bad in ["", "Andy", "a/b", "-x", "a b", "a\0b", "\u{e9}", &too_long] {
        let err = store.create_person(bad).await.unwrap_err();
        assert!(matches!(err, StoreError::Stored(_)), "{bad:?}: {err:?}");
        // A hostile handle is not echoed back.
        assert!(err.to_string().len() < 200, "{bad:?}");
    }
    assert_eq!(store.person().await.unwrap(), None);
    for table in ["tenants", "humans", "local_origin"] {
        assert_eq!(count(&store, table).await, 0, "{table}");
    }
    let longest = "x".repeat(39);
    assert_eq!(store.create_person(&longest).await.unwrap().handle, longest);
}

#[test]
fn user_names_clean_to_valid_handles() {
    let long = "a".repeat(100);
    let dashes = format!("{}a", "-".repeat(50));
    let cut = format!("{}-y", "x".repeat(38));
    let cases: [(&str, &str); 18] = [
        ("andy", "andy"),
        ("Andy.Aldrich", "andyaldrich"),
        ("Andy Aldrich", "andyaldrich"),
        ("a_b-c", "ab-c"),
        ("../../etc/passwd", "etcpasswd"),
        ("a/b\\c", "abc"),
        ("-rf", "rf"),
        ("_-x", "x"),
        (&dashes, "a"),
        ("--", "me"),
        ("", "me"),
        ("\t\r\n\0\u{1b}[31m", "31m"),
        // Not ASCII, so dropped and never folded onto an ASCII letter.
        ("\u{ff41}\u{ff4e}dy", "dy"),
        ("\u{212a}elvin", "elvin"),
        ("\u{130}smail", "smail"),
        ("\u{440}aul", "aul"),
        (&long, &long[..39]),
        (&cut, &cut[..39]),
    ];
    for (name, expected) in cases {
        let handle = clean_person_handle(name);
        assert_eq!(handle, expected, "{name:?}");
        assert!(is_person_handle(&handle), "{name:?}");
    }
}

#[test]
fn any_user_name_cleans_to_a_valid_handle() {
    // Every code point below U+3000 and a few beyond it, alone and beside ASCII.
    let beyond = ['\u{feff}', '\u{202e}', '\u{1f600}', '\u{10ffff}'];
    let chars = (0..0x3000_u32).filter_map(char::from_u32).chain(beyond);
    for c in chars {
        for name in [
            c.to_string(),
            format!("{c}x"),
            format!("x{c}"),
            format!("-{c}"),
        ] {
            let handle = clean_person_handle(&name);
            assert!(is_person_handle(&handle), "{name:?} gave {handle:?}");
            assert_eq!(clean_person_handle(&handle), handle, "{name:?}");
        }
    }
    let longest = "x".repeat(39);
    for handle in ["andy", "a-b", "0", &longest] {
        assert_eq!(clean_person_handle(handle), handle);
    }
}

#[tokio::test]
async fn a_person_without_a_membership_cannot_mint() {
    let (_dir, store, topics) = project().await;
    store
        .write(|conn, _| {
            conn.execute("DELETE FROM memberships", [])
                .map(drop)
                .map_err(backend)
        })
        .await
        .unwrap();
    error(
        store
            .mint(request(&topics, "worker", IdentityKind::Agent))
            .await
            .unwrap_err(),
        "not_a_member",
        "you are not a member of that project",
    );
    assert_eq!(count(&store, "identities").await, 0);
}

#[tokio::test]
async fn revoking_a_listener_does_not_revoke_its_workers() {
    // As on the hub, where revoking is per identity (identities.rs `revoke`).
    let (_dir, store, topics) = project().await;
    let listener = store
        .mint(request(&topics, "listener", IdentityKind::Listener))
        .await
        .unwrap();
    let mut worker = request(&topics, "worker", IdentityKind::Agent);
    worker.delegated_by = Some(listener.id.clone());
    let worker = store.mint(worker).await.unwrap();
    assert!(store.revoke(topics.project(), &listener.id).await.unwrap());
    let live: bool = store
        .read(move |conn, _| {
            conn.query_row(
                "SELECT revoked_at IS NULL FROM identities WHERE id=?1",
                [worker.id.as_str()],
                |row| row.get(0),
            )
            .map_err(backend)
        })
        .await
        .unwrap();
    assert!(live);
}
