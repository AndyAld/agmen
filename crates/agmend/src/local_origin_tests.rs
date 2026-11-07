use super::*;

const TEST_LEASE: Duration = Duration::from_secs(60);
const SWEEP_WAIT: Duration = Duration::from_secs(2);
const SWEEP_POLLS: usize = 100;
const POLL_EVERY: Duration = Duration::from_millis(20);

fn envelope(payload: agmen_proto::Payload) -> ClientEnvelope {
    let now = Timestamp::now();
    ClientEnvelope {
        v: 1,
        id: agmen_core::new_message_id(now).unwrap(),
        reply_to: None,
        to: Vec::new(),
        mentions: Vec::new(),
        items: Vec::new(),
        lane: None,
        body: "local test".try_into().unwrap(),
        valid_until: Some(now + TEST_LEASE),
        sent_at: now,
        payload,
    }
}

fn agent(project: &ProjectId) -> LocalMint {
    LocalMint {
        project: project.clone(),
        name: "worker".into(),
        kind: IdentityKind::Agent,
        spec: None,
        agent: None,
        delegated_by: None,
    }
}

async fn found_and_claim(
    local: &LocalOrigin,
    project: &ProjectId,
    id: &IdentityId,
    lane: Option<agmen_proto::LaneName>,
) {
    let found = envelope(agmen_proto::Payload::Found {
        title: "item".into(),
        lane,
        severity: None,
    });
    assert!(matches!(
        local.ingest(project, id, &found).await.unwrap(),
        DeliveryReply::Ack { .. }
    ));
    let page = local
        .page(project, &Stream::Feed, 0, MAX_PAGE)
        .await
        .unwrap();
    let key = page
        .messages
        .iter()
        .find_map(|message| match &message.client.payload {
            agmen_proto::Payload::Item { key }
                if message.client.reply_to.as_ref() == Some(&found.id) =>
            {
                Some(key.clone())
            }
            _ => None,
        })
        .unwrap();
    assert!(matches!(
        local
            .ingest(
                project,
                id,
                &envelope(agmen_proto::Payload::Claim { item: key })
            )
            .await
            .unwrap(),
        DeliveryReply::Ack { .. }
    ));
}

#[tokio::test]
async fn existing_origin_starts_its_sweeper_without_a_read() {
    let dir = tempfile::tempdir().unwrap();
    let local = LocalOrigin::new(dir.path(), TEST_LEASE);
    let topics = local.create_project("sweeper", "andy").await.unwrap();
    let project = topics.project();
    let worker = local.mint(agent(project)).await.unwrap();
    found_and_claim(&local, project, &worker.id, None).await;
    let record = local.open(None).await.unwrap();
    local.shutdown().await;
    let conn = rusqlite::Connection::open(dir.path().join(record::schema::RECORD_DB)).unwrap();
    conn.execute("UPDATE claims SET granted_at=0, lease_until=0", [])
        .unwrap();
    drop(conn);
    let next = LocalOrigin::new(dir.path(), TEST_LEASE);
    next.start().await;
    tokio::time::timeout(SWEEP_WAIT, async {
        for _ in 0..SWEEP_POLLS {
            // Read through the original core, so reading cannot start the new origin.
            let page = record.core.page(project, &Stream::Feed, 0, MAX_PAGE).await.unwrap();
            if page.messages.iter().any(|message| matches!(&message.client.payload, agmen_proto::Payload::Release { reason, .. } if reason.as_deref() == Some("lease_expired"))) {
                return;
            }
            tokio::time::sleep(POLL_EVERY).await;
        }
        panic!("the reopened origin did not sweep");
    }).await.unwrap();
    next.shutdown().await;
}

#[tokio::test]
async fn reads_of_an_absent_origin_leave_the_file_absent() {
    let dir = tempfile::tempdir().unwrap();
    let local = LocalOrigin::new(dir.path(), TEST_LEASE);
    let project = "unknown1".parse().unwrap();
    let id = "unknown2".parse().unwrap();
    assert!(!local.has_project(&project).await);
    assert_eq!(local.person().await.unwrap(), None);
    assert!(local.login().await.unwrap().is_none());
    assert!(local.projects().await.is_err());
    assert!(local.topics(&project).await.is_err());
    assert!(local.head(&project).await.is_err());
    assert!(
        local
            .page(&project, &Stream::Feed, 0, MAX_PAGE)
            .await
            .is_err()
    );
    assert!(local.roster(&project).await.is_err());
    assert!(local.state(&project, "head").await.is_err());
    let call = envelope(agmen_proto::Payload::Rollcall {});
    assert!(local.ingest(&project, &id, &call).await.is_err());
    assert!(local.post_as_person(&project, &call).await.is_err());
    assert!(local.mint(agent(&project)).await.is_err());
    assert!(local.revoke(&project, &id).await.is_err());
    assert!(local.lanes(&project).await.is_err());
    assert!(local.replace_lanes(&project, Vec::new()).await.is_err());
    assert!(local.check_path(&project, &id, Vec::new()).await.is_err());
    assert!(local.items(&project).await.is_err());
    assert!(local.watch_stored(&project).await.is_err());
    assert!(local.presence(&project, &id).await.is_err());
    assert!(!dir.path().join(record::schema::RECORD_DB).exists());
    local.shutdown().await;
}

#[tokio::test]
async fn local_reads_and_limits_match_the_core() {
    let dir = tempfile::tempdir().unwrap();
    let local = LocalOrigin::new(dir.path(), TEST_LEASE);
    let topics = local
        .create_project(&"é".repeat(MAX_NAME_CHARS), "--A N.D_Y")
        .await
        .unwrap();
    let project = topics.project();
    assert_eq!(local.topics(project).await.unwrap(), topics);
    let person = local.person().await.unwrap().unwrap();
    assert_eq!(person.handle, "andy");
    local.create_project("second", "another").await.unwrap();
    assert_eq!(local.person().await.unwrap().unwrap(), person);
    assert!(local.create_project("second", "another").await.is_err());
    let identity = local.mint(agent(project)).await.unwrap();
    let login = local.login().await.unwrap().unwrap();
    assert_eq!(login.hub_url, local_hub_url());
    assert_eq!(login.human_id, person.id);
    assert_eq!(
        local.state(project, "head").await.unwrap()["value"]["feed_seq"],
        0
    );
    assert!(local.state(project, "roster").await.unwrap()["value"].is_array());
    assert!(local.state(project, "unknown").await.is_err());
    for limit in [0, MAX_PAGE + 1] {
        assert_eq!(
            local
                .page(project, &Stream::Feed, 0, limit)
                .await
                .unwrap_err()
                .code,
            ErrorCode::BadRequest
        );
    }
    assert_eq!(
        local
            .page(project, &Stream::Feed, u64::MAX, 1)
            .await
            .unwrap_err()
            .code,
        ErrorCode::BadRequest
    );
    assert!(local.items(project).await.unwrap().items.is_empty());
    let specs: Vec<LaneSpec> = serde_json::from_value(
        json!([{"name": "L-UI", "globs": ["ui/**"], "owner": identity.handle}]),
    )
    .unwrap();
    let list = serde_json::to_value(local.replace_lanes(project, specs).await.unwrap()).unwrap();
    assert_eq!(list["lanes"][0]["name"], "L-UI");
    assert_eq!(
        serde_json::to_value(local.lanes(project).await.unwrap()).unwrap(),
        list
    );
    found_and_claim(&local, project, &identity.id, Some("L-UI".parse().unwrap())).await;
    let checks = local
        .check_path(project, &identity.id, vec!["ui/app.rs".into()])
        .await
        .unwrap();
    assert!(checks[0].allowed);
    assert_eq!(checks[0].lane.as_ref().unwrap().as_str(), "L-UI");
    assert_eq!(
        local
            .check_path(
                project,
                &identity.id,
                vec!["file".into(); MAX_CHECK_PATHS + 1]
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::BadRequest
    );
    local.shutdown().await;
    let next = LocalOrigin::new(dir.path(), TEST_LEASE);
    assert_eq!(next.person().await.unwrap().unwrap(), person);
    assert!(next.has_project(project).await);
    next.shutdown().await;
}

#[test]
fn mint_errors_match_the_hub_status_and_code() {
    for (code, status, expected_code) in [
        ("not_a_member", 403, "not_a_member"),
        (
            "coordinator_role_required",
            403,
            "coordinator_role_required",
        ),
        ("bad_handle", 422, "bad_handle"),
        ("bad_role", 422, "bad_role"),
        ("bad_edits", 422, "bad_edits"),
        ("listener_exists", 409, "listener_exists"),
        ("handle_taken", 409, "handle_taken"),
        ("server_error", 500, "server_error"),
        ("future_error", 500, "server_error"),
    ] {
        let HubError::Refused {
            status: actual,
            code: actual_code,
            description,
        } = mint_error(MintError {
            code,
            message: "reason".into(),
        })
        else {
            panic!("expected a refusal");
        };
        assert_eq!(
            (actual, actual_code.as_str(), description.as_str()),
            (status, expected_code, "reason")
        );
    }
}

#[tokio::test]
async fn roster_and_watch_reflect_mint_revoke_and_presence() {
    let dir = tempfile::tempdir().unwrap();
    let local = LocalOrigin::new(dir.path(), TEST_LEASE);
    let topics = local.create_project("roster", "--A N.D_Y").await.unwrap();
    let project = topics.project();
    assert_eq!(local.person().await.unwrap().unwrap().handle, "andy");
    let mut watch = local.watch_stored(project).await.unwrap();
    let identity = local
        .mint(LocalMint {
            project: project.clone(),
            name: "worker".into(),
            kind: IdentityKind::Agent,
            spec: None,
            agent: None,
            delegated_by: None,
        })
        .await
        .unwrap();
    assert!(watch.has_changed().unwrap());
    watch.borrow_and_update();
    let entry = local
        .roster(project)
        .await
        .unwrap()
        .into_iter()
        .find(|row| row.identity_id == identity.id)
        .unwrap();
    assert_eq!(entry.status, agmen_local::console::Status::Offline);
    let present = local.presence(project, &identity.id).await.unwrap();
    assert!(watch.has_changed().unwrap());
    watch.borrow_and_update();
    assert_eq!(
        local
            .roster(project)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.identity_id == identity.id)
            .unwrap()
            .status,
        agmen_local::console::Status::Online
    );
    drop(present);
    assert!(watch.has_changed().unwrap());
    watch.borrow_and_update();
    assert_eq!(
        local
            .roster(project)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.identity_id == identity.id)
            .unwrap()
            .status,
        agmen_local::console::Status::Offline
    );
    assert!(local.revoke(project, &identity.id).await.unwrap());
    assert!(watch.has_changed().unwrap());
    assert!(
        !local
            .roster(project)
            .await
            .unwrap()
            .iter()
            .any(|row| row.identity_id == identity.id)
    );
    local.shutdown().await;
}
