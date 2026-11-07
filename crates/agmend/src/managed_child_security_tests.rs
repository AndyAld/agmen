use super::stop::publisher;
use super::*;

#[path = "managed_personal_role_tests.rs"]
mod personal;

#[tokio::test]
async fn brief_requires_dm_before_minting() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    let before = git_state(&fixture);
    let error = child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"chain","brief":"review"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(
        error
            .message
            .contains("role chain does not grant the DM verb"),
        "{}",
        error.message
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
    assert_eq!(git_state(&fixture), before);
}

#[tokio::test]
async fn a_legacy_parent_without_recorded_verbs_cannot_skip_the_brief_precheck() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    let record = fixture
        .daemon
        .core
        .managed
        .agent_dir(&"agent001".parse().unwrap())
        .join("agent.json");
    let mut stored: Value = serde_json::from_str(&role_file(&record).unwrap()).unwrap();
    stored.as_object_mut().unwrap().remove("verbs");
    std::fs::write(record, serde_json::to_vec(&stored).unwrap()).unwrap();
    let restored = restarted(&fixture).await;
    let worker = worker_of(&restored, "agent001").await;
    let caller = {
        let mut held = worker.lock().await;
        held.batch_tier = Some(Tier::Owner);
        held.record.identity.clone()
    };
    let failure = restored
        .launch_child(
            &caller,
            json!({"name":"k1","role":"chain","brief":"review"}),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.code, ErrorCode::BadRequest);
    assert!(
        failure
            .message
            .contains("no record of owner/worker's verbs")
            && failure.message.contains("relaunch"),
        "{}",
        failure.message
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
    // Without a brief it launches as before: only the brief needs the verb.
    restored
        .launch_child(&caller, json!({"name":"k1","role":"chain"}))
        .await
        .unwrap();
}

#[tokio::test]
async fn refused_brief_rolls_back_fully() {
    let (mut fixture, _) = tree_fixture_with_verbs(json!(["FYI", "DM"])).await;
    publisher(
        &mut fixture,
        Arc::new(|_, envelope| {
            Box::pin(async move {
                Ok(if matches!(envelope.payload, agmen_proto::Payload::Dm {}) {
                    OutboxState::Rejected {
                        reason: agmen_proto::NackReason::ForbiddenVerb,
                    }
                } else {
                    OutboxState::Pending { attempts: 0 }
                })
            })
        }),
    )
    .await;
    root(&fixture).await;
    let before = git_state(&fixture);
    std::fs::create_dir_all(
        fixture
            .daemon
            .core
            .managed
            .state_dir
            .join("identities/kid00001"),
    )
    .unwrap();
    let error = child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"chain","brief":"review"}),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("brief was not delivered"));
    assert!(error.message.contains("forbidden_verb"));
    assert_discarded(&fixture, "kid00001", &before).await;
    assert!(
        !fixture
            .daemon
            .core
            .managed
            .state_dir
            .join("identities/kid00001")
            .exists()
    );
}

#[tokio::test]
async fn timed_out_brief_rolls_back_fully() {
    let (mut fixture, _) = tree_fixture_with_verbs(json!(["FYI", "DM"])).await;
    publisher(
        &mut fixture,
        Arc::new(|_, envelope| {
            Box::pin(async move {
                if matches!(envelope.payload, agmen_proto::Payload::Dm {}) {
                    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
                }
                Ok(OutboxState::Pending { attempts: 0 })
            })
        }),
    )
    .await;
    root(&fixture).await;
    let before = git_state(&fixture);
    let error = child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"chain","brief":"review"}),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("brief was not delivered"));
    assert_discarded(&fixture, "kid00001", &before).await;
}

/// The parent's launch reaches agmend with its own valid token. The new
/// child's stored credential is cut short, so the launch's read of it refuses
/// after the hub minted the identity, and everything made so far is taken back.
#[tokio::test]
async fn child_launch_with_an_invalid_stored_credential_is_refused_and_leaves_nothing() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    let parent_token = fixture
        .daemon
        .core
        .identities
        .local_token(&"agent001".parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    // register keeps a stored credential that parses, so this one reaches the launch.
    let cut_short = "a".repeat(63);
    let key = agmen_client::TokenKey::new(
        &"https://agmend.invalid".parse().unwrap(),
        &"kid00001".parse().unwrap(),
    )
    .unwrap();
    let stored = json!({
        "origin": fixture.hub_url.origin().ascii_serialization(),
        "owner_id": "owner001",
        "local_token": cut_short,
    });
    fixture.tokens.set(&key, &stored.to_string()).unwrap();
    let before = git_state(&fixture);
    let error = child(&fixture, "agent001", "k1").await.unwrap_err();
    assert_eq!(error, crate::identities::unauthorized());
    assert!(!error.message.contains(parent_token.expose()));
    assert!(!error.message.contains(&cut_short));
    assert_discarded(&fixture, "kid00001", &before).await;
}

/// The front door checks the parent's own stored credential. Here the stored
/// copy lost its last digit, so it agrees with the token the parent holds as
/// far as it goes. A launch made as `agmen launch` makes it is refused before
/// the hub is asked for anything.
#[tokio::test]
async fn child_launch_with_an_invalid_stored_parent_credential_is_refused_and_leaves_nothing() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    let parent: agmen_proto::IdentityId = "agent001".parse().unwrap();
    let held = fixture
        .daemon
        .core
        .identities
        .local_token(&parent)
        .await
        .unwrap()
        .unwrap()
        .expose()
        .to_owned();
    let cut_short = &held[..63];
    let key =
        agmen_client::TokenKey::new(&"https://agmend.invalid".parse().unwrap(), &parent).unwrap();
    let stored = json!({
        "origin": fixture.hub_url.origin().ascii_serialization(),
        "owner_id": "owner001",
        "local_token": cut_short,
    });
    fixture.tokens.set(&key, &stored.to_string()).unwrap();
    let before = git_state(&fixture);
    let error = framed_call(
        &fixture,
        "agent001",
        &held,
        "child_launch",
        json!({"name":"k1","role":"chain"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error, crate::identities::unauthorized());
    assert_eq!(error.message, "invalid local identity credential");
    assert!(!error.message.contains(&held));
    assert!(!error.message.contains(cut_short));
    // The hub never heard of a child, and nothing was made for one.
    let managed = &fixture.daemon.core.managed;
    let child: agmen_proto::IdentityId = "kid00001".parse().unwrap();
    assert_eq!(mints.lock().unwrap().len(), 1, "only the parent was minted");
    assert!(fixture.revoked.lock().unwrap().is_empty());
    let rows = fixture
        .daemon
        .core
        .identities
        .managed(
            fixture.hub_url.origin().ascii_serialization(),
            "owner001".parse().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "store row");
    assert!(!managed.agent_dir(&child).exists(), "record folder");
    assert!(
        !managed.state_dir.join("agents/kid00001").exists(),
        "role prompt folder"
    );
    assert_eq!(managed.workers.lock().await.len(), 1, "child worker");
    assert_eq!(managed.tree.lock().await.len(), 1, "tree link");
    assert_eq!(git_state(&fixture), before, "worktree and branch");
}

fn commit_roles(fixture: &Fixture) {
    let repo = fixture.dir.path().join("repo");
    git(&repo, &["add", "-f", ".agmen/roles"]);
    git(
        &repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "review child roles",
        ],
    );
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
}

#[tokio::test]
async fn pinned_role_idle_ticks_stop_spawning_git_after_first_resolution() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"chain"}),
    )
    .await
    .unwrap();
    let managed = &fixture.daemon.core.managed;
    let worker = worker_of(managed, "kid00001").await;
    assert!(worker.lock().await.record.role_commit.is_some());
    crate::git::INVOCATIONS
        .scope(std::cell::Cell::new(0), async {
            managed.tick().await;
            let first = crate::git::INVOCATIONS.with(std::cell::Cell::get);
            assert!(
                first > 0,
                "the first tick resolves the pinned role with git"
            );
            for _ in 0..3 {
                managed.tick().await;
                assert_eq!(crate::git::INVOCATIONS.with(std::cell::Cell::get), first);
                assert_eq!(worker.lock().await.state, "idle");
            }
        })
        .await;
    assert!(fixture.runner.runs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn pinned_role_records_at_different_commits_resolve_separately() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"chain"}),
    )
    .await
    .unwrap();
    let managed = &fixture.daemon.core.managed;
    let first = worker_of(managed, "kid00001")
        .await
        .lock()
        .await
        .record
        .clone();
    let chain = first.repo.join(".agmen/roles/chain.xml");
    std::fs::write(
        &chain,
        role_file(&chain)
            .unwrap()
            .replace("<summary>c</summary>", "<summary>later</summary>"),
    )
    .unwrap();
    commit_roles(&fixture);
    let mut second = first.clone();
    second.role_commit = Some(
        String::from_utf8(listing(&first.repo, &["rev-parse", "HEAD"]))
            .unwrap()
            .trim()
            .to_owned(),
    );
    assert_ne!(first.role_commit, second.role_commit);
    crate::git::INVOCATIONS
        .scope(std::cell::Cell::new(0), async {
            let earlier = managed.record_role(&first, "chain").await.unwrap();
            let after_first = crate::git::INVOCATIONS.with(std::cell::Cell::get);
            assert!(after_first > 0);
            let later = managed.record_role(&second, "chain").await.unwrap();
            let after_second = crate::git::INVOCATIONS.with(std::cell::Cell::get);
            assert!(
                after_second > after_first,
                "a different commit needs its own resolution"
            );
            assert_ne!(earlier.sha256, later.sha256);
            for (record, expected) in [(&first, &earlier), (&second, &later)] {
                assert_eq!(
                    &managed.record_role(record, "chain").await.unwrap(),
                    expected
                );
                assert_eq!(
                    crate::git::INVOCATIONS.with(std::cell::Cell::get),
                    after_second
                );
            }
        })
        .await;
}

#[tokio::test]
async fn pinned_role_resolution_errors_are_retried() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    let managed = &fixture.daemon.core.managed;
    let mut record = worker_of(managed, "agent001")
        .await
        .lock()
        .await
        .record
        .clone();
    record.role_commit = Some(record.reviewed_base.as_ref().unwrap().commit.clone());
    crate::git::INVOCATIONS
        .scope(std::cell::Cell::new(0), async {
            assert!(managed.record_role(&record, "chain").await.is_ok());
            for _ in 0..2 {
                let before = crate::git::INVOCATIONS.with(std::cell::Cell::get);
                assert!(managed.record_role(&record, "missing").await.is_err());
                assert!(crate::git::INVOCATIONS.with(std::cell::Cell::get) > before);
            }
            assert!(managed.record_role(&record, "chain").await.is_ok());
        })
        .await;
}

#[tokio::test]
async fn pinned_role_cache_hit_does_not_wait_for_another_cold_load() {
    use std::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    let managed = &fixture.daemon.core.managed;
    let mut record = worker_of(managed, "agent001")
        .await
        .lock()
        .await
        .record
        .clone();
    record.role_commit = Some(record.reviewed_base.as_ref().unwrap().commit.clone());
    assert!(managed.record_role(&record, "chain").await.is_ok());
    let mut cx = Context::from_waker(Waker::noop());
    // The first poll starts git and parks, so a held lock would still be held here.
    let mut cold = pin!(managed.record_role(&record, "missing"));
    assert!(cold.as_mut().poll(&mut cx).is_pending());
    let mut hit = pin!(managed.record_role(&record, "chain"));
    assert!(matches!(hit.as_mut().poll(&mut cx), Poll::Ready(Ok(_))));
}

#[tokio::test]
async fn pinned_role_cache_evicts_smallest_key_at_managed_bound() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    let managed = &fixture.daemon.core.managed;
    let mut record = worker_of(managed, "agent001")
        .await
        .lock()
        .await
        .record
        .clone();
    record.role_commit = Some(record.reviewed_base.as_ref().unwrap().commit.clone());
    let repo = record.repo.clone();
    let aliases = fixture.dir.path().join("cache");
    std::fs::create_dir(&aliases).unwrap();
    crate::git::INVOCATIONS
        .scope(std::cell::Cell::new(0), async {
            for index in (0..=agmen_local::safeguards::MAX_MANAGED).rev() {
                record.repo = aliases.join(format!("{index:03}"));
                std::os::unix::fs::symlink(&repo, &record.repo).unwrap();
                assert!(managed.record_role(&record, "chain").await.is_ok());
            }
            let full = crate::git::INVOCATIONS.with(std::cell::Cell::get);
            record.repo = aliases.join(format!("{:03}", agmen_local::safeguards::MAX_MANAGED));
            assert!(managed.record_role(&record, "chain").await.is_ok());
            assert_eq!(crate::git::INVOCATIONS.with(std::cell::Cell::get), full);
            // The smallest key before the last insertion was 001, not the oldest, 256.
            record.repo = aliases.join("001");
            assert!(managed.record_role(&record, "chain").await.is_ok());
            assert!(crate::git::INVOCATIONS.with(std::cell::Cell::get) > full);
        })
        .await;
}

#[tokio::test]
async fn unpinned_role_records_keep_reading_local_files() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    let managed = &fixture.daemon.core.managed;
    let record = worker_of(managed, "agent001")
        .await
        .lock()
        .await
        .record
        .clone();
    assert!(record.role_commit.is_none());
    let earlier = managed.record_role(&record, "chain").await.unwrap();
    let chain = record.repo.join(".agmen/roles/chain.xml");
    std::fs::write(
        &chain,
        role_file(&chain)
            .unwrap()
            .replace("<summary>c</summary>", "<summary>later</summary>"),
    )
    .unwrap();
    let later = managed.record_role(&record, "chain").await.unwrap();
    assert_ne!(earlier.sha256, later.sha256);
}

#[tokio::test]
async fn pinned_role_cached_resolution_still_checks_launch_hash_each_tick() {
    let (fixture, _) = tree_fixture().await;
    root(&fixture).await;
    child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"chain"}),
    )
    .await
    .unwrap();
    let managed = &fixture.daemon.core.managed;
    let worker = worker_of(managed, "kid00001").await;
    crate::git::INVOCATIONS
        .scope(std::cell::Cell::new(0), async {
            managed.tick().await;
            let first = crate::git::INVOCATIONS.with(std::cell::Cell::get);
            worker.lock().await.record.role_sha256 = Some("0".repeat(64).parse().unwrap());
            managed.tick().await;
            let held = worker.lock().await;
            assert_eq!(held.state, "offline");
            assert!(
                held.role_error
                    .as_ref()
                    .unwrap()
                    .contains("changed since launch")
            );
            assert_eq!(crate::git::INVOCATIONS.with(std::cell::Cell::get), first);
        })
        .await;
}

#[tokio::test]
async fn child_project_role_comes_from_the_base_commit_and_is_pinned() {
    let (fixture, mints) = tree_fixture().await;
    let repo = fixture.dir.path().join("repo");
    let chain = repo.join(".agmen/roles/chain.xml");
    std::fs::write(
        &chain,
        role_file(&chain)
            .unwrap()
            .replace("roles=\"chain\"", "roles=\"chain leaf\""),
    )
    .unwrap();
    let leaf = "<role name=\"leaf\" extends=\"reviewer\" version=\"1\"><rules><rule>reviewed prompt</rule></rules></role>";
    std::fs::write(repo.join(".agmen/roles/leaf.xml"), leaf).unwrap();
    commit_roles(&fixture);
    let expected = agmen_roles::Layers {
        project: Some(repo.clone()),
        ..Default::default()
    }
    .load()
    .unwrap()
    .get("leaf")
    .unwrap()
    .sha256
    .clone();
    let parent = root(&fixture).await;
    let parent_role =
        std::path::Path::new(parent["worktree"].as_str().unwrap()).join(".agmen/roles/leaf.xml");
    std::fs::write(
        &parent_role,
        leaf.replace("reviewed prompt", "unreviewed parent prompt"),
    )
    .unwrap();
    std::fs::write(
        repo.join(".agmen/roles/leaf.xml"),
        leaf.replace("reviewed prompt", "unreviewed main prompt"),
    )
    .unwrap();
    child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"leaf"}),
    )
    .await
    .unwrap();
    assert_eq!(mints.lock().unwrap()[1]["role_sha256"], expected);
    let prompt = fixture
        .daemon
        .core
        .managed
        .state_dir
        .join("agents/kid00001/role-prompt.md");
    assert!(role_file(&prompt).unwrap().contains("reviewed prompt"));
    assert!(!role_file(&prompt).unwrap().contains("unreviewed"));
    enqueue_from(&fixture, 1, "owner001", "kid00001").await;
    batch(&fixture.daemon.core.managed, "kid00001").await;
    assert_eq!(fixture.runner.runs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn child_role_missing_at_base_commit_is_refused() {
    let (fixture, mints) = tree_fixture().await;
    let repo = fixture.dir.path().join("repo");
    let chain = repo.join(".agmen/roles/chain.xml");
    std::fs::write(
        &chain,
        role_file(&chain)
            .unwrap()
            .replace("roles=\"chain\"", "roles=\"chain leaf\""),
    )
    .unwrap();
    commit_roles(&fixture);
    std::fs::write(
        repo.join(".agmen/roles/leaf.xml"),
        "<role name=\"leaf\" extends=\"reviewer\" version=\"1\"/>",
    )
    .unwrap();
    root(&fixture).await;
    let error = child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"leaf"}),
    )
    .await
    .unwrap_err();
    let commit = String::from_utf8(listing(&repo, &["rev-parse", "refs/remotes/origin/main"]))
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(commit.len(), 40);
    assert!(error.message.contains("leaf"), "{}", error.message);
    assert!(error.message.contains("origin/main"), "{}", error.message);
    assert!(error.message.contains(&commit), "{}", error.message);
    assert!(
        error.message.contains("relaunch the root"),
        "{}",
        error.message
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn child_project_role_cannot_shadow_a_builtin() {
    let (fixture, mints) = tree_fixture().await;
    let repo = fixture.dir.path().join("repo");
    let chain = repo.join(".agmen/roles/chain.xml");
    std::fs::write(
        &chain,
        role_file(&chain)
            .unwrap()
            .replace("roles=\"chain\"", "roles=\"chain reviewer\""),
    )
    .unwrap();
    std::fs::write(repo.join(".agmen/roles/reviewer.xml"), "<role name=\"reviewer\" extends=\"base\" kind=\"agent\" tier=\"standard\" version=\"1\"><summary>shadow</summary></role>").unwrap();
    commit_roles(&fixture);
    root(&fixture).await;
    let error = child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"reviewer"}),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .message
            .contains("project roles may not shadow built-in roles")
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
}

/// The reviewed role the child was launched with is the one it keeps: the
/// record holds the commit, not the name `origin/main`.
#[tokio::test]
async fn the_pinned_commit_outlives_origin_moving_and_a_restart() {
    let (fixture, _) = tree_fixture().await;
    settings(&fixture, "budgets = false");
    let repo = fixture.dir.path().join("repo");
    let chain = repo.join(".agmen/roles/chain.xml");
    std::fs::write(
        &chain,
        role_file(&chain)
            .unwrap()
            .replace("roles=\"chain\"", "roles=\"chain leaf\""),
    )
    .unwrap();
    let leaf = "<role name=\"leaf\" extends=\"reviewer\" version=\"1\"><rules><rule>reviewed prompt</rule></rules></role>";
    std::fs::write(repo.join(".agmen/roles/leaf.xml"), leaf).unwrap();
    commit_roles(&fixture);
    let reviewed = String::from_utf8(listing(&repo, &["rev-parse", "refs/remotes/origin/main"]))
        .unwrap()
        .trim()
        .to_owned();
    root(&fixture).await;
    child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"leaf"}),
    )
    .await
    .unwrap();
    let managed = &fixture.daemon.core.managed;
    let saved = |managed: &Managed| {
        let file = managed
            .agent_dir(&"kid00001".parse().unwrap())
            .join("agent.json");
        serde_json::from_str::<Value>(&role_file(file).unwrap()).unwrap()
    };
    assert_eq!(saved(managed)["role_commit"], reviewed);
    assert_eq!(saved(managed)["reviewed_base"]["commit"], reviewed);
    assert_eq!(saved(managed)["reviewed_base"]["reference"], "origin/main");
    // The remote's branch moves on and its role file changes.
    std::fs::write(
        repo.join(".agmen/roles/leaf.xml"),
        leaf.replace("reviewed prompt", "later prompt"),
    )
    .unwrap();
    commit_roles(&fixture);
    let prompt = managed.state_dir.join("agents/kid00001/role-prompt.md");
    enqueue_from(&fixture, 1, "owner001", "kid00001").await;
    batch(managed, "kid00001").await;
    assert_eq!(fixture.runner.runs.lock().unwrap().len(), 1);
    assert!(role_file(&prompt).unwrap().contains("reviewed prompt"));
    assert!(!role_file(&prompt).unwrap().contains("later prompt"));
    // The same after the daemon restarts and the agent is started again.
    let restored = restarted(&fixture).await;
    assert_eq!(saved(&restored)["role_commit"], reviewed);
    restored
        .start(json!({"identity":"kid00001"}))
        .await
        .unwrap();
    enqueue_from(&fixture, 2, "owner001", "kid00001").await;
    batch(&restored, "kid00001").await;
    assert_eq!(fixture.runner.runs.lock().unwrap().len(), 2);
    assert!(role_file(&prompt).unwrap().contains("reviewed prompt"));
    assert!(
        worker_of(&restored, "kid00001")
            .await
            .lock()
            .await
            .role_error
            .is_none()
    );
}

/// A parent with a shell can commit anything on its own branch. Only the
/// reviewed branch counts: its own commit does not make a role exist.
#[tokio::test]
async fn a_role_committed_only_on_the_parents_branch_is_not_read() {
    let (fixture, mints) = tree_fixture().await;
    let repo = fixture.dir.path().join("repo");
    let chain = repo.join(".agmen/roles/chain.xml");
    std::fs::write(
        &chain,
        role_file(&chain)
            .unwrap()
            .replace("roles=\"chain\"", "roles=\"chain leaf\""),
    )
    .unwrap();
    commit_roles(&fixture);
    let parent = root(&fixture).await;
    let worktree = std::path::PathBuf::from(parent["worktree"].as_str().unwrap());
    std::fs::write(
        worktree.join(".agmen/roles/leaf.xml"),
        "<role name=\"leaf\" extends=\"reviewer\" version=\"1\"/>",
    )
    .unwrap();
    git(&worktree, &["add", "-f", ".agmen/roles/leaf.xml"]);
    git(
        &worktree,
        &["-c", "commit.gpgsign=false", "commit", "-m", "own role"],
    );
    let error = child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"leaf"}),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("leaf"), "{}", error.message);
    assert!(error.message.contains("origin/main"), "{}", error.message);
    assert_eq!(mints.lock().unwrap().len(), 1);
}

/// The brief is refused, so the launch is undone, including the worktree. The
/// parent has committed work that the main checkout does not have, as a
/// working parent does, and the child's branch starts from it.
#[tokio::test]
async fn a_refused_brief_removes_the_worktree_when_the_parent_has_unmerged_commits() {
    let (mut fixture, _) = tree_fixture_with_verbs(json!(["FYI", "DM"])).await;
    publisher(
        &mut fixture,
        Arc::new(|_, envelope| {
            Box::pin(async move {
                Ok(if matches!(envelope.payload, agmen_proto::Payload::Dm {}) {
                    OutboxState::Rejected {
                        reason: agmen_proto::NackReason::ForbiddenVerb,
                    }
                } else {
                    OutboxState::Pending { attempts: 0 }
                })
            })
        }),
    )
    .await;
    let parent = root(&fixture).await;
    let worktree = std::path::PathBuf::from(parent["worktree"].as_str().unwrap());
    std::fs::write(worktree.join("work.txt"), "the parent's own work").unwrap();
    git(&worktree, &["add", "-f", "work.txt"]);
    git(
        &worktree,
        &["-c", "commit.gpgsign=false", "commit", "-m", "parent work"],
    );
    let before = git_state(&fixture);
    let error = child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"chain","brief":"review"}),
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("brief was not delivered"));
    assert!(!error.message.contains("retained"), "{}", error.message);
    assert_discarded(&fixture, "kid00001", &before).await;
    let repo = fixture.dir.path().join("repo");
    assert!(listing(&repo, &["for-each-ref", "refs/wip"]).is_empty());
}

const LEAF: &str = "<role name=\"leaf\" extends=\"reviewer\" version=\"1\"><rules><rule>reviewed prompt</rule></rules></role>";

/// `leaf` is a reviewed role that `chain` may launch, committed at
/// `origin/main`. Answers what the reviewed `leaf` hashes to.
fn review_leaf(fixture: &Fixture) -> String {
    let repo = fixture.dir.path().join("repo");
    let chain = repo.join(".agmen/roles/chain.xml");
    std::fs::write(
        &chain,
        role_file(&chain)
            .unwrap()
            .replace("roles=\"chain\"", "roles=\"chain leaf\""),
    )
    .unwrap();
    std::fs::write(repo.join(".agmen/roles/leaf.xml"), LEAF).unwrap();
    commit_roles(fixture);
    agmen_roles::Layers {
        project: Some(repo),
        ..Default::default()
    }
    .load()
    .unwrap()
    .get("leaf")
    .unwrap()
    .sha256
    .clone()
}

/// A parent with a shell commits a widened `leaf` on its own branch.
fn evil_leaf_commit(parent: &Value) -> (std::path::PathBuf, String) {
    let worktree = std::path::PathBuf::from(parent["worktree"].as_str().unwrap());
    std::fs::write(
        worktree.join(".agmen/roles/leaf.xml"),
        LEAF.replace("reviewed prompt", "EVIL prompt"),
    )
    .unwrap();
    git(&worktree, &["add", "-f", ".agmen/roles/leaf.xml"]);
    git(
        &worktree,
        &["-c", "commit.gpgsign=false", "commit", "-m", "evil"],
    );
    let oid = String::from_utf8(listing(&worktree, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .to_owned();
    (worktree, oid)
}

/// The child just launched got the reviewed `leaf`, whatever else the repository says.
async fn assert_reviewed(
    fixture: &Fixture,
    mints: &Arc<Mutex<Vec<Value>>>,
    child: &str,
    sha: &str,
) {
    assert_eq!(mints.lock().unwrap().last().unwrap()["role_sha256"], sha);
    let prompt = fixture
        .daemon
        .core
        .managed
        .state_dir
        .join("agents")
        .join(child)
        .join("role-prompt.md");
    let text = role_file(&prompt).unwrap();
    assert!(
        text.contains("reviewed prompt") && !text.contains("EVIL"),
        "{text}"
    );
}

async fn launch_leaf(fixture: &Fixture, parent: &str, name: &str) {
    child_call(
        fixture,
        parent,
        "child_launch",
        json!({"name":name,"role":"leaf"}),
    )
    .await
    .unwrap();
}

/// The commit is recorded when the person launches the root, in agmend's own
/// state. Moving `origin/<branch>` afterwards reaches no child.
#[tokio::test]
async fn a_parent_that_moves_origin_cannot_change_its_childs_role() {
    let (fixture, mints) = tree_fixture().await;
    settings(&fixture, "budgets = false");
    let sha = review_leaf(&fixture);
    let parent = root(&fixture).await;
    let (worktree, evil) = evil_leaf_commit(&parent);
    git(
        &worktree,
        &["update-ref", "refs/remotes/origin/main", &evil],
    );
    launch_leaf(&fixture, "agent001", "k1").await;
    assert_reviewed(&fixture, &mints, "kid00001", &sha).await;
}

/// Nor does a new base branch, which is read from the main checkout's HEAD.
#[tokio::test]
async fn a_parent_that_changes_the_base_branch_cannot_change_its_childs_role() {
    let (fixture, mints) = tree_fixture().await;
    settings(&fixture, "budgets = false");
    let sha = review_leaf(&fixture);
    let parent = root(&fixture).await;
    let (worktree, evil) = evil_leaf_commit(&parent);
    let repo = fixture.dir.path().join("repo");
    git(&repo, &["branch", "other"]);
    git(&repo, &["symbolic-ref", "HEAD", "refs/heads/other"]);
    git(
        &worktree,
        &["update-ref", "refs/remotes/origin/other", &evil],
    );
    launch_leaf(&fixture, "agent001", "k1").await;
    assert_reviewed(&fixture, &mints, "kid00001", &sha).await;
}

/// A replacement ref swaps an object's content while its id stays the same,
/// so the commit's id alone does not pin what is read.
#[tokio::test]
async fn a_parent_that_replaces_objects_cannot_change_its_childs_role() {
    let (fixture, mints) = tree_fixture().await;
    settings(&fixture, "budgets = false");
    let sha = review_leaf(&fixture);
    let parent = root(&fixture).await;
    let worktree = std::path::PathBuf::from(parent["worktree"].as_str().unwrap());
    let repo = fixture.dir.path().join("repo");
    let reviewed = String::from_utf8(listing(
        &repo,
        &["rev-parse", "origin/main:.agmen/roles/leaf.xml"],
    ))
    .unwrap()
    .trim()
    .to_owned();
    std::fs::write(
        worktree.join("evil.xml"),
        LEAF.replace("reviewed prompt", "EVIL prompt"),
    )
    .unwrap();
    let evil = String::from_utf8(listing(&worktree, &["hash-object", "-w", "evil.xml"]))
        .unwrap()
        .trim()
        .to_owned();
    git(&worktree, &["replace", &reviewed, &evil]);
    launch_leaf(&fixture, "agent001", "k1").await;
    assert_reviewed(&fixture, &mints, "kid00001", &sha).await;
}

/// The same goes for the recorded commit itself being replaced by another.
#[tokio::test]
async fn a_parent_that_replaces_the_commit_cannot_change_its_childs_role() {
    let (fixture, mints) = tree_fixture().await;
    settings(&fixture, "budgets = false");
    let sha = review_leaf(&fixture);
    let reviewed = String::from_utf8(listing(
        &fixture.dir.path().join("repo"),
        &["rev-parse", "refs/remotes/origin/main"],
    ))
    .unwrap()
    .trim()
    .to_owned();
    let parent = root(&fixture).await;
    let (worktree, evil) = evil_leaf_commit(&parent);
    git(&worktree, &["replace", &reviewed, &evil]);
    launch_leaf(&fixture, "agent001", "k1").await;
    assert_reviewed(&fixture, &mints, "kid00001", &sha).await;
}

/// Every descendant reads at the commit its tree's root recorded, whatever
/// `origin/<branch>` has moved to since, reviewed or not.
#[tokio::test]
async fn a_grandchild_reads_the_commit_its_tree_recorded() {
    let (fixture, mints) = tree_fixture().await;
    settings(&fixture, "budgets = false");
    let sha = review_leaf(&fixture);
    let reviewed = String::from_utf8(listing(
        &fixture.dir.path().join("repo"),
        &["rev-parse", "refs/remotes/origin/main"],
    ))
    .unwrap()
    .trim()
    .to_owned();
    root(&fixture).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    tier(&fixture, "kid00001", Some(Tier::Owner)).await;
    let repo = fixture.dir.path().join("repo");
    std::fs::write(
        repo.join(".agmen/roles/leaf.xml"),
        LEAF.replace("reviewed prompt", "later prompt"),
    )
    .unwrap();
    commit_roles(&fixture);
    launch_leaf(&fixture, "kid00001", "k2").await;
    assert_reviewed(&fixture, &mints, "kid00002", &sha).await;
    let saved = |id: &str| {
        let file = fixture
            .daemon
            .core
            .managed
            .agent_dir(&id.parse().unwrap())
            .join("agent.json");
        serde_json::from_str::<Value>(&role_file(file).unwrap()).unwrap()
    };
    for id in ["agent001", "kid00001", "kid00002"] {
        assert_eq!(saved(id)["reviewed_base"]["commit"], reviewed, "{id}");
    }
}

/// A root that agmend recorded no commit for, as one launched before this
/// was recorded, cannot launch children; the person relaunches it.
#[tokio::test]
async fn a_root_without_a_recorded_commit_cannot_launch_children() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    let record = fixture
        .daemon
        .core
        .managed
        .agent_dir(&"agent001".parse().unwrap())
        .join("agent.json");
    let mut stored: Value = serde_json::from_str(&role_file(&record).unwrap()).unwrap();
    assert!(
        stored
            .as_object_mut()
            .unwrap()
            .remove("reviewed_base")
            .is_some()
    );
    std::fs::write(record, serde_json::to_vec(&stored).unwrap()).unwrap();
    let restored = restarted(&fixture).await;
    let worker = worker_of(&restored, "agent001").await;
    let caller = {
        let mut held = worker.lock().await;
        held.batch_tier = Some(Tier::Owner);
        held.record.identity.clone()
    };
    let before = git_state(&fixture);
    let failure = restored
        .launch_child(&caller, json!({"name":"k1","role":"chain"}))
        .await
        .unwrap_err();
    assert_eq!(failure.code, ErrorCode::BadRequest);
    assert!(
        failure.message.contains("relaunch the root"),
        "{}",
        failure.message
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
    assert_eq!(git_state(&fixture), before);
}

/// A checkout with no `origin/<branch>` still launches its agents, as it did
/// before; only their children need the commit.
#[tokio::test]
async fn a_root_launched_with_no_base_still_launches_but_cannot_launch_children() {
    let (fixture, mints) = tree_fixture().await;
    let repo = fixture.dir.path().join("repo");
    git(&repo, &["checkout", "--detach"]);
    let launched = root(&fixture).await;
    assert_eq!(launched["state"], "idle");
    let error = child(&fixture, "agent001", "k1").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(
        error.message.contains("relaunch the root"),
        "{}",
        error.message
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
    // The reason agmend could not record one is plain.
    let failure = fixture
        .daemon
        .core
        .managed
        .resolve_base(&repo)
        .await
        .unwrap_err();
    assert_eq!(failure.code, ErrorCode::BadRequest);
    assert!(
        failure.message.contains("no current branch") && failure.message.contains("origin/"),
        "{}",
        failure.message
    );
}
