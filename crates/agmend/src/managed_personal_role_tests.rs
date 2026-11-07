use super::*;

const NARROW: &str = r#"<role name="chain" extends="base" kind="agent" tier="standard" version="1"><summary>personal</summary><tools allow="read"/><edits scope="none"/><launches roles="chain late"/></role>"#;
const BROAD: &str = r#"<role name="chain" extends="base" kind="agent" tier="standard" version="1"><summary>personal</summary><tools allow="read edit shell"/><edits scope="lanes"/><launches roles="chain late"/></role>"#;

async fn personal_fixture() -> (Fixture, Arc<Mutex<Vec<Value>>>) {
    let (mut fixture, mints) = tree_fixture().await;
    let roles = fixture.dir.path().join("personal-roles");
    std::fs::create_dir(&roles).unwrap();
    std::fs::write(roles.join("chain.xml"), NARROW).unwrap();
    Arc::get_mut(&mut Arc::get_mut(&mut fixture.daemon.core).unwrap().managed)
        .unwrap()
        .personal_roles = Some(roles);
    settings(&fixture, "budgets = false");
    (fixture, mints)
}

async fn resolved_record(managed: &Managed, id: &str) -> agmen_roles::Resolved {
    let record = worker_of(managed, id).await.lock().await.record.clone();
    managed.record_role(&record, "chain").await.unwrap()
}

async fn snapshot_dir(managed: &Managed, id: &str) -> std::path::PathBuf {
    let record = managed.agent_dir(&id.parse().unwrap()).join("agent.json");
    let stored: Value = serde_json::from_str(&role_file(&record).unwrap()).unwrap();
    managed
        .state_dir
        .join("role-snapshots")
        .join(stored["personal_snapshot"].as_str().unwrap())
}

#[tokio::test]
async fn personal_role_replacement_keeps_child_tools_and_edits_pinned() {
    let (fixture, mints) = personal_fixture().await;
    root(&fixture).await;
    let managed = &fixture.daemon.core.managed;
    let expected = resolved_record(managed, "agent001").await;
    std::fs::write(fixture.dir.path().join("personal-roles/chain.xml"), BROAD).unwrap();
    child(&fixture, "agent001", "k1").await.unwrap();
    let actual = resolved_record(managed, "kid00001").await;
    assert_eq!(actual.tools, expected.tools);
    assert_eq!(actual.edits, expected.edits);
    assert_eq!(actual.sha256, expected.sha256);
    assert_eq!(mints.lock().unwrap()[1]["role_sha256"], expected.sha256);
    assert_eq!(mints.lock().unwrap()[1]["edits"], json!({"scope":"none"}));
    let restored = restarted(&fixture).await;
    assert_eq!(resolved_record(&restored, "kid00001").await, expected);
    let worker = worker_of(&restored, "kid00001").await;
    let caller = {
        let mut held = worker.lock().await;
        held.batch_tier = Some(Tier::Owner);
        held.record.identity.clone()
    };
    restored
        .launch_child(&caller, json!({"name":"k2","role":"chain"}))
        .await
        .unwrap();
    assert_eq!(resolved_record(&restored, "kid00002").await, expected);
}

#[tokio::test]
async fn personal_role_added_after_root_is_unavailable_to_children() {
    let (fixture, mints) = personal_fixture().await;
    root(&fixture).await;
    std::fs::write(
        fixture.dir.path().join("personal-roles/late.xml"),
        r#"<role name="late" extends="reviewer" version="1"/>"#,
    )
    .unwrap();
    let before = git_state(&fixture);
    let failure = child_call(
        &fixture,
        "agent001",
        "child_launch",
        json!({"name":"k1","role":"late"}),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, ErrorCode::BadRequest);
    assert!(failure.message.contains("unknown role \"late\""));
    assert_eq!(mints.lock().unwrap().len(), 1);
    assert_eq!(git_state(&fixture), before);
}

#[tokio::test]
async fn fresh_root_uses_current_personal_roles_without_changing_older_trees() {
    let (fixture, _) = personal_fixture().await;
    root(&fixture).await;
    child(&fixture, "agent001", "k1").await.unwrap();
    let managed = &fixture.daemon.core.managed;
    let earlier = resolved_record(managed, "kid00001").await;
    std::fs::write(fixture.dir.path().join("personal-roles/chain.xml"), BROAD).unwrap();
    worker_call(&fixture, "agent_launch", json!({"project":"project1","name":"fresh","root":fixture.dir.path().join("repo"),"role":"chain"})).await.unwrap();
    tier(&fixture, "kid00002", Some(Tier::Owner)).await;
    child(&fixture, "kid00002", "k3").await.unwrap();
    let current = resolved_record(managed, "kid00003").await;
    assert!(current.tools.contains(&agmen_roles::ToolClass::Shell));
    assert_eq!(current.edits, agmen_roles::EditScope::Lanes);
    assert_ne!(current.sha256, earlier.sha256);
    assert_eq!(resolved_record(managed, "kid00001").await, earlier);
}

#[tokio::test]
async fn legacy_record_without_personal_snapshot_refuses_children() {
    let (fixture, mints) = personal_fixture().await;
    root(&fixture).await;
    let file = fixture
        .daemon
        .core
        .managed
        .agent_dir(&"agent001".parse().unwrap())
        .join("agent.json");
    let mut stored: Value = serde_json::from_str(&role_file(&file).unwrap()).unwrap();
    stored.as_object_mut().unwrap().remove("personal_snapshot");
    std::fs::write(file, serde_json::to_vec(&stored).unwrap()).unwrap();
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
    assert!(failure.message.contains("personal role snapshot"));
    assert!(failure.message.contains("relaunch the root agent"));
    assert_eq!(mints.lock().unwrap().len(), 1);
    assert_eq!(git_state(&fixture), before);
    // Only children are refused: the agent still resolves the role it was minted with.
    let record = worker.lock().await.record.clone();
    let role = restored.record_role(&record, "chain").await.unwrap();
    assert_eq!(record.role_sha256.unwrap().as_str(), role.sha256);
}

/// Makes `chain` a project-layer root that may launch `leaf`, and answers the
/// personal `leaf` as the person's launch of that root records it.
async fn project_root_for_leaf(fixture: &Fixture) -> agmen_roles::Resolved {
    std::fs::remove_file(fixture.dir.path().join("personal-roles/chain.xml")).unwrap();
    let repo = fixture.dir.path().join("repo");
    let chain = repo.join(".agmen/roles/chain.xml");
    std::fs::write(
        &chain,
        role_file(&chain)
            .unwrap()
            .replace("roles=\"chain\"", "roles=\"chain leaf\""),
    )
    .unwrap();
    commit_roles(fixture);
    fixture
        .daemon
        .core
        .managed
        .resolve_any_role(&repo, "leaf")
        .await
        .unwrap()
}

fn leaf_of(xml: &str) -> String {
    xml.replace("name=\"chain\"", "name=\"leaf\"")
}

/// The finding: the root's own role is untouched while the person's file for a
/// role it may launch is widened after the root launched. The child's batch
/// must still get the tools of the role as the root recorded it.
#[tokio::test]
async fn widened_personal_file_does_not_reach_the_childs_batch() {
    let (fixture, mints) = personal_fixture().await;
    let leaf = fixture.dir.path().join("personal-roles/leaf.xml");
    std::fs::write(&leaf, leaf_of(NARROW)).unwrap();
    let expected = project_root_for_leaf(&fixture).await;
    root(&fixture).await;
    std::fs::write(&leaf, leaf_of(BROAD)).unwrap();
    launch_leaf(&fixture, "agent001", "k1").await;
    assert_eq!(mints.lock().unwrap()[1]["role_sha256"], expected.sha256);
    assert_eq!(mints.lock().unwrap()[1]["edits"], json!({"scope":"none"}));
    enqueue_from(&fixture, 1, "owner001", "kid00001").await;
    batch(&fixture.daemon.core.managed, "kid00001").await;
    let runs = fixture.runner.runs.lock().unwrap();
    assert_eq!(runs.len(), 1);
    let tools = &runs[0].role.as_ref().unwrap().tools;
    assert_eq!(*tools, agmen_roles::claude_tools(&expected).join(","));
    assert!(
        !tools.contains("Bash") && !tools.contains("Edit"),
        "{tools}"
    );
}

#[tokio::test]
async fn project_parent_uses_copied_personal_child_sources() {
    let (fixture, mints) = personal_fixture().await;
    let roles = fixture.dir.path().join("personal-roles");
    let source = roles.join("source.txt");
    std::fs::write(&source, leaf_of(NARROW)).unwrap();
    std::os::unix::fs::symlink(&source, roles.join("leaf.xml")).unwrap();
    let expected = project_root_for_leaf(&fixture).await;
    let managed = &fixture.daemon.core.managed;
    root(&fixture).await;
    std::fs::write(&source, leaf_of(BROAD)).unwrap();
    launch_leaf(&fixture, "agent001", "k1").await;
    let record = worker_of(managed, "kid00001")
        .await
        .lock()
        .await
        .record
        .clone();
    let actual = managed.record_role(&record, "leaf").await.unwrap();
    assert_eq!(actual.tools, expected.tools);
    assert_eq!(actual.edits, expected.edits);
    assert_eq!(actual.sha256, expected.sha256);
    assert_eq!(mints.lock().unwrap()[1]["role_sha256"], expected.sha256);
}

#[tokio::test]
async fn empty_personal_snapshot_ignores_a_directory_created_later() {
    let (fixture, mints) = personal_fixture().await;
    let roles = fixture.dir.path().join("personal-roles");
    std::fs::remove_dir_all(&roles).unwrap();
    root(&fixture).await;
    std::fs::create_dir(&roles).unwrap();
    std::fs::write(roles.join("chain.xml"), BROAD).unwrap();
    child(&fixture, "agent001", "k1").await.unwrap();
    let role = resolved_record(&fixture.daemon.core.managed, "kid00001").await;
    assert_eq!(role.layer, agmen_roles::Layer::Project);
    assert_eq!(mints.lock().unwrap()[1]["role_sha256"], role.sha256);
}

#[tokio::test]
async fn personal_snapshot_obeys_file_and_byte_limits() {
    let (fixture, _) = personal_fixture().await;
    let roles = fixture.dir.path().join("personal-roles");
    std::fs::remove_file(roles.join("chain.xml")).unwrap();
    std::fs::write(roles.join("0000.xml"), "x".repeat(64 * 1024 + 1)).unwrap();
    std::fs::write(roles.join("0001.xml"), [0xff]).unwrap();
    for index in 2..64 {
        let name = if index == 2 {
            "chain".into()
        } else {
            format!("bounded{index}")
        };
        let mut xml = NARROW.replace("name=\"chain\"", &format!("name=\"{name}\""));
        xml.push_str(&" ".repeat(64 * 1024 - xml.len()));
        std::fs::write(roles.join(format!("{index:04}.xml")), xml).unwrap();
    }
    // The sixty-fifth XML file must never be read, even though two were skipped.
    std::fs::write(roles.join("0064.xml"), "invalid XML").unwrap();
    root(&fixture).await;
    let managed = &fixture.daemon.core.managed;
    let dir = snapshot_dir(managed, "agent001").await;
    assert!(dir.starts_with(&managed.state_dir));
    assert!(!dir.starts_with(fixture.dir.path().join("repo")));
    let files = std::fs::read_dir(&dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(files.len(), 62);
    assert!(
        files
            .iter()
            .all(|file| file.metadata().unwrap().len() == 64 * 1024)
    );
    // Private to the user: nobody else can read or replace what a tree is pinned to.
    let mode = |path: &std::path::Path| {
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(path).unwrap().permissions())
            & 0o777
    };
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(dir.parent().unwrap()), 0o700);
    assert!(files.iter().all(|file| mode(&file.path()) == 0o600));
    let restored = restarted(&fixture).await;
    assert_eq!(
        resolved_record(&restored, "agent001").await.layer,
        agmen_roles::Layer::Personal
    );
}

#[tokio::test]
async fn missing_personal_snapshot_refuses_children_before_minting() {
    let (fixture, mints) = personal_fixture().await;
    root(&fixture).await;
    let managed = &fixture.daemon.core.managed;
    std::fs::remove_dir_all(snapshot_dir(managed, "agent001").await).unwrap();
    let before = git_state(&fixture);
    let failure = child(&fixture, "agent001", "k1").await.unwrap_err();
    assert_eq!(failure.code, ErrorCode::BadRequest);
    assert!(failure.message.contains("personal role snapshot"));
    assert!(failure.message.contains("relaunch the root agent"));
    assert_eq!(mints.lock().unwrap().len(), 1);
    assert_eq!(git_state(&fixture), before);
}

/// A root resolves its personal role from the snapshot too, so a later edit to
/// the person's file neither widens its next batch nor takes it offline.
#[tokio::test]
async fn personal_edit_after_launch_neither_widens_nor_stops_the_roots_batch() {
    let (fixture, _) = personal_fixture().await;
    root(&fixture).await;
    let managed = &fixture.daemon.core.managed;
    let expected = resolved_record(managed, "agent001").await;
    std::fs::write(fixture.dir.path().join("personal-roles/chain.xml"), BROAD).unwrap();
    enqueue_from(&fixture, 1, "owner001", "agent001").await;
    batch(managed, "agent001").await;
    let held = worker_of(managed, "agent001").await;
    assert_eq!(held.lock().await.role_error, None);
    let runs = fixture.runner.runs.lock().unwrap();
    assert_eq!(runs.len(), 1);
    let tools = &runs[0].role.as_ref().unwrap().tools;
    assert_eq!(*tools, agmen_roles::claude_tools(&expected).join(","));
}

/// agent.json is a file a worker can edit: a snapshot ID that is not a session
/// ID never reaches a path, because the record does not load.
#[tokio::test]
async fn record_with_a_path_for_a_snapshot_id_does_not_load() {
    let (fixture, _) = personal_fixture().await;
    root(&fixture).await;
    let file = fixture
        .daemon
        .core
        .managed
        .agent_dir(&"agent001".parse().unwrap())
        .join("agent.json");
    let mut stored: Value = serde_json::from_str(&role_file(&file).unwrap()).unwrap();
    stored["personal_snapshot"] = json!("../../../etc");
    std::fs::write(file, serde_json::to_vec(&stored).unwrap()).unwrap();
    let restored = restarted(&fixture).await;
    assert!(restored.workers.lock().await.is_empty());
}
