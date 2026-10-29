use super::stop::publisher;
use super::*;

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

/// Child roles come from `origin/<branch>` of the main checkout, so a
/// checkout with no branch has nothing to read from, and says so.
#[tokio::test]
async fn a_main_checkout_with_no_branch_is_refused_with_the_reason() {
    let (fixture, mints) = tree_fixture().await;
    root(&fixture).await;
    git(&fixture.dir.path().join("repo"), &["checkout", "--detach"]);
    let error = child(&fixture, "agent001", "k1").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(
        error.message.contains("no current branch") && error.message.contains("origin/"),
        "{}",
        error.message
    );
    assert_eq!(mints.lock().unwrap().len(), 1);
}
