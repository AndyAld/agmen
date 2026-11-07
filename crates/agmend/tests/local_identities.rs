//! Local identity credentials and authenticated record access without a hub.

#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_client::{
    IdentityFile, LOCAL_TOKEN_NAMESPACE, LocalCredential, TokenKey, read_local_credential,
};
use agmen_local::{ErrorCode, Request, Response};
use agmen_proto::{ClientEnvelope, IdentityId, Payload, Timestamp};
use agmend::Agmend;
use serde_json::{Value, json};
use support::{Client, Home, error, result};

const CALL_WAIT: Duration = Duration::from_secs(10);
const STOP_WAIT: Duration = Duration::from_secs(5);
const INVALID_LIMITS: [u32; 2] = [0, 501];
const IDENTITY_CAP: usize = agmen_local::safeguards::MAX_MANAGED;
const ROLE_CASES: [(&str, &str); 2] = [("builtin", "reviewer"), ("personal", "personal")];
const LOCAL_READS: [(&str, &str); 4] = [
    ("page", r#"{"stream":"feed"}"#),
    ("lanes", "{}"),
    ("items", "{}"),
    ("check_path", r#"{"paths":[]}"#),
];

struct Fixture {
    home: Home,
    client: Client,
    created: Value,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        let home = Home {
            dir: tempfile::Builder::new().prefix("lid").tempdir().unwrap(),
        };
        Self::start(home).await
    }

    async fn start(home: Home) -> Self {
        let daemon = Agmend::bind(home.config()).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(daemon.serve_until(async {
            stopped.await.unwrap();
        }));
        let mut client = Client::connect(&home.socket()).await;
        result(client.call("hello", json!({"api": 1})).await);
        let created = result(
            client
                .call("local_project_create", json!({"name": "identities"}))
                .await,
        );
        Self {
            home,
            client,
            created,
            stop,
            task,
        }
    }

    async fn mint(&mut self, name: &str) -> IdentityFile {
        let raw = result(
            self.client
                .call(
                    "local_mint",
                    json!({
                        "project": self.created["project"], "name": name
                    }),
                )
                .await,
        );
        assert!(raw.get("token").is_none());
        assert!(raw.get("local_token").is_none());
        serde_json::from_value(raw).unwrap()
    }

    fn credential(&self, who: &IdentityFile) -> LocalCredential {
        read_local_credential(&self.home.tokens(), &who.identity_id)
            .unwrap()
            .unwrap()
    }

    async fn call(&mut self, who: &IdentityFile, method: &str, mut raw: Value) -> Response {
        raw["local_token"] = json!(self.credential(who).local_token);
        self.call_with(who.identity_id.clone(), method, raw).await
    }

    async fn call_with(&mut self, id: IdentityId, method: &str, raw: Value) -> Response {
        let mut request = Request::new(900, method, raw);
        request.as_ = Some(id);
        tokio::time::timeout(CALL_WAIT, async {
            self.client.send(&request).await;
            self.client.read().await
        })
        .await
        .unwrap()
    }

    async fn stop(self) {
        self.stop.send(()).unwrap();
        tokio::time::timeout(STOP_WAIT, self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

fn envelope(body: &str) -> ClientEnvelope {
    let now = Timestamp::now();
    ClientEnvelope {
        v: 1,
        id: agmen_core::new_message_id(now).unwrap(),
        reply_to: None,
        to: Vec::new(),
        mentions: Vec::new(),
        items: Vec::new(),
        lane: None,
        body: body.try_into().unwrap(),
        valid_until: None,
        payload: Payload::Fyi {},
        sent_at: now,
    }
}

#[tokio::test]
async fn local_mint_registers_and_issues_a_token() {
    let mut f = Fixture::new().await;
    let who = f.mint("worker").await;
    assert!(who.is_local());
    assert_eq!(who.broker_url, agmen_client::local_broker());
    assert_eq!(json!(who.project), f.created["project"]);
    assert_eq!(json!(who.owner_id), f.created["person"]["id"]);
    let credential = f.credential(&who);
    assert_eq!(credential.origin, "local");
    assert_eq!(credential.owner_id, who.owner_id);
    assert_eq!(credential.local_token.len(), 64);
    assert!(
        credential
            .local_token
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    );
    assert!(json!(&who).get("token").is_none());
    let conn = rusqlite::Connection::open(f.home.database()).unwrap();
    let origin: String = conn
        .query_row(
            "SELECT origin FROM identities WHERE identity=?1",
            [who.identity_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(origin, "local");
    drop(conn);
    result(
        f.call(
            &who,
            "state",
            json!({"project": who.project, "key": "head"}),
        )
        .await,
    );
    assert!(!f.home.dir.path().join("identities").exists());
    f.stop().await;
}

#[tokio::test]
async fn local_post_is_stamped_with_the_caller() {
    let mut f = Fixture::new().await;
    let who = f.mint("worker").await;
    let sent = envelope("hello locally");
    let posted = result(f.call(&who, "post", json!({"envelope": sent})).await);
    assert_eq!(posted, json!({"stream": "feed", "seq": 1}));
    let page = result(f.call(&who, "page", json!({"stream": "feed"})).await);
    assert_eq!(page["messages"][0]["from"], json!(who.identity_id));
    assert_eq!(page["messages"][0]["from_handle"], who.handle);
    assert_eq!(page["messages"][0]["id"], json!(sent.id));
    let mut forged = json!(envelope("forged"));
    forged["from"] = json!(f.created["person"]["id"]);
    assert_eq!(
        error(f.call(&who, "post", json!({"envelope": forged})).await).code,
        ErrorCode::BadRequest
    );
    assert!(!f.home.dir.path().join("identities").exists());
    f.stop().await;
}

#[tokio::test]
async fn token_of_one_identity_cannot_post_as_another() {
    let mut f = Fixture::new().await;
    let a = f.mint("alpha").await;
    let b = f.mint("beta").await;
    let err = error(
        f.call_with(
            b.identity_id.clone(),
            "post",
            json!({
                "envelope": envelope("wrong caller"), "local_token": f.credential(&a).local_token
            }),
        )
        .await,
    );
    assert_eq!(err.code, ErrorCode::Unauthorized);
    let page = result(f.call(&b, "page", json!({"stream": "feed"})).await);
    assert_eq!(page["head"], 0);
    f.stop().await;
}

#[tokio::test]
async fn identity_page_reads_feed_and_own_inbox_only() {
    let mut f = Fixture::new().await;
    let a = f.mint("alpha").await;
    let b = f.mint("beta").await;
    let mut sent = envelope("private hello");
    sent.payload = Payload::Dm {};
    sent.to.push(a.identity_id.to_string().try_into().unwrap());
    result(f.call(&b, "post", json!({"envelope": sent})).await);
    let inbox = format!("inbox:{}", a.identity_id);
    let page = result(f.call(&a, "page", json!({"stream": inbox})).await);
    assert_eq!(page["messages"][0]["body"], "private hello");
    assert_eq!(page["pruned_through"], 0);
    let inbox_page = result(f.call(&a, "inbox", json!({"project": a.project})).await);
    assert_eq!(inbox_page["messages"], page["messages"]);
    let after = result(
        f.call(
            &a,
            "page",
            json!({"stream": inbox, "after": page["messages"][0]["seq"]}),
        )
        .await,
    );
    assert!(after["messages"].as_array().unwrap().is_empty());
    result(
        f.call(&a, "page", json!({"stream": "feed", "limit": 1}))
            .await,
    );
    let err = error(
        f.call(
            &a,
            "page",
            json!({"stream": format!("inbox:{}", b.identity_id)}),
        )
        .await,
    );
    assert_eq!(err.code, ErrorCode::Forbidden);
    for limit in INVALID_LIMITS {
        assert_eq!(
            error(
                f.call(&a, "page", json!({"stream": "feed", "limit": limit}))
                    .await
            )
            .code,
            ErrorCode::BadRequest
        );
    }
    assert_eq!(
        error(
            f.call(&a, "state", json!({"project": "other001", "key": "head"}))
                .await
        )
        .code,
        ErrorCode::Forbidden
    );
    f.stop().await;
}

#[tokio::test]
async fn identity_lanes_items_and_check_path() {
    let mut f = Fixture::new().await;
    let who = f.mint("worker").await;
    let lanes = result(
        f.client
            .call(
                "lanes_set",
                json!({"project": who.project,
                    "lanes": [{"name": "L-SRC", "globs": ["src/**"]}]
                }),
            )
            .await,
    );
    assert_eq!(result(f.call(&who, "lanes", json!({})).await), lanes);
    assert_eq!(
        result(f.call(&who, "items", json!({})).await),
        json!({"items": []})
    );
    let checks = result(
        f.call(
            &who,
            "check_path",
            json!({"paths": ["src/a.rs", "docs/a.md", "../escape"]}),
        )
        .await,
    );
    assert_eq!(
        checks,
        json!({"checks": [
            {"path": "src/a.rs", "lane": "L-SRC", "allowed": false, "reason": "not claimed"},
            {"path": "docs/a.md", "lane": null, "allowed": true, "reason": "unowned"},
            {"path": "../escape", "lane": null, "allowed": false, "reason": agmen_core::NOT_A_REPO_PATH}
        ]})
    );
    assert_eq!(
        result(f.call(&who, "check_path", json!({"paths": []})).await),
        json!({"checks": []})
    );
    let head = result(
        f.call(
            &who,
            "state",
            json!({"project": who.project, "key": "head"}),
        )
        .await,
    );
    assert_eq!(head["value"]["lanes_v"], 0);
    assert_eq!(
        error(
            f.call(&who, "lanes", json!({"from": who.identity_id}))
                .await
        )
        .code,
        ErrorCode::BadRequest
    );
    f.stop().await;
}

#[tokio::test]
async fn rejected_local_post_reports_the_reason() {
    let mut f = Fixture::new().await;
    let who = f.mint("worker").await;
    let body = format!("{}{}", "ghp_", "a".repeat(36));
    let err = error(
        f.call(&who, "post", json!({"envelope": envelope(&body)}))
            .await,
    );
    assert_eq!(err.code, ErrorCode::Hub);
    assert_eq!(
        err.message,
        "hub rejected identity message: secret_detected"
    );
    assert_eq!(
        result(f.call(&who, "page", json!({"stream": "feed"})).await)["head"],
        0
    );
    f.stop().await;
}

#[tokio::test]
async fn local_mint_reports_a_newer_record_without_writing() {
    let home = Home {
        dir: tempfile::tempdir().unwrap(),
    };
    let path = home.dir.path().join("local.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    let daemon = Agmend::bind(home.config()).unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(daemon.serve_until(async {
        stopped.await.unwrap();
    }));
    let mut client = Client::connect(&home.socket()).await;
    result(client.call("hello", json!({"api": 1})).await);
    let err = error(
        client
            .call(
                "local_mint",
                json!({"project": "future01", "name": "worker"}),
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::Version);
    assert!(err.message.contains("local.db"), "{err:?}");
    assert_eq!(err, error(client.call("local_projects", json!({})).await));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    stop.send(()).unwrap();
    tokio::time::timeout(STOP_WAIT, task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn local_mint_uses_a_known_project_role() {
    let mut f = Fixture::new().await;
    let owner = f.mint("checkout").await;
    let repo = f.home.dir.path().join("checkout");
    std::fs::create_dir_all(repo.join(".agmen/roles")).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(repo.join(".gitignore"), ".agmen/\n").unwrap();
    std::fs::write(
        repo.join(".agmen/identity.json"),
        serde_json::to_vec(&owner).unwrap(),
    )
    .unwrap();
    std::fs::write(repo.join(".agmen/roles/custom.xml"), "<role name=\"custom\" extends=\"reviewer\" version=\"1\"><purpose>Project review</purpose></role>").unwrap();
    result(
        f.client
            .call(
                "console_snapshot",
                json!({"project": owner.project, "root": repo}),
            )
            .await,
    );
    let catalog = agmen_roles::Layers {
        project: Some(repo),
        ..Default::default()
    }
    .load()
    .unwrap();
    let resolved = catalog.get("custom").unwrap();
    let minted = result(
        f.client
            .call(
                "local_mint",
                json!({"project": owner.project, "name": "review", "role": "custom"}),
            )
            .await,
    );
    assert_eq!(minted["role"], "custom");
    assert_eq!(minted["role_sha256"], resolved.sha256);
    assert_eq!(minted["verbs"], json!(resolved.verbs));
    f.stop().await;
}

#[tokio::test]
async fn local_mint_without_checkout_uses_builtin_and_personal_roles() {
    let mut f = Fixture::new().await;
    let unrelated = f.home.dir.path().join("unremembered");
    std::fs::create_dir_all(unrelated.join(".agmen/roles")).unwrap();
    std::fs::write(
        unrelated.join(".agmen/roles/project-only.xml"),
        "<role name=\"project-only\" extends=\"reviewer\" version=\"1\"/>",
    )
    .unwrap();
    std::fs::create_dir_all(f.home.personal_roles()).unwrap();
    std::fs::write(
        f.home.personal_roles().join("personal.xml"),
        "<role name=\"personal\" extends=\"reviewer\" version=\"1\"/>",
    )
    .unwrap();
    let catalog = agmen_roles::Layers {
        personal: Some(f.home.personal_roles()),
        ..Default::default()
    }
    .load()
    .unwrap();
    for (name, role) in ROLE_CASES {
        let minted = result(
            f.client
                .call(
                    "local_mint",
                    json!({"project": f.created["project"], "name": name, "role": role}),
                )
                .await,
        );
        assert_eq!(minted["role_sha256"], catalog.get(role).unwrap().sha256);
    }
    let err = error(
        f.client
            .call(
                "local_mint",
                json!({"project": f.created["project"], "name": "unknown", "role": "project-only"}),
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.message.contains("unknown role"), "{err:?}");
    f.stop().await;
}

#[tokio::test]
async fn hub_and_local_credentials_keep_their_own_authority() {
    let hub = support::fake_hub().await;
    let home = Home::signed_in(&hub.url).await;
    let mut f = Fixture::start(home).await;
    let local = f.mint("worker").await;
    let hub_id: IdentityId = "hubagent".parse().unwrap();
    f.home
        .seed_identity(&hub.url, hub_id.as_str(), &"a".repeat(64));
    for (method, raw) in LOCAL_READS {
        let mut raw: Value = serde_json::from_str(raw).unwrap();
        raw["local_token"] = json!("a".repeat(64));
        let err = error(f.call_with(hub_id.clone(), method, raw).await);
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert_eq!(err.message, "hub identities read this from their hub");
    }
    result(
        f.call(
            &local,
            "post",
            json!({"envelope": envelope("local beside hub")}),
        )
        .await,
    );
    assert!(hub.state.posts.lock().unwrap().is_empty());
    std::fs::remove_file(f.home.login_file()).unwrap();
    result(
        f.call(
            &local,
            "state",
            json!({"project": local.project, "key": "roster"}),
        )
        .await,
    );
    let err = error(
        f.call_with(
            hub_id,
            "state",
            json!({
                "project": support::PROJECT, "key": "head", "local_token": "a".repeat(64)
            }),
        )
        .await,
    );
    assert_eq!(err.code, ErrorCode::Unauthorized);
    f.stop().await;
}

#[tokio::test]
async fn local_mint_refusals_preserve_hub_codes_and_owner_scope() {
    let mut f = Fixture::new().await;
    let who = f.mint("worker").await;
    let err = error(
        f.client
            .call(
                "local_mint",
                json!({"project": who.project, "name": "worker"}),
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::Hub);
    assert_eq!(err.message, "the hub answered 409 handle_taken");
    let err = error(
        f.client
            .call(
                "local_mint",
                json!({"project": "unknown1", "name": "worker"}),
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::Unauthorized);
    assert_eq!(err.message, "the hub answered 403 not_a_member");
    assert_eq!(
        error(
            f.call(
                &who,
                "local_mint",
                json!({"project": who.project, "name": "other"})
            )
            .await
        )
        .code,
        ErrorCode::Forbidden
    );
    let err = error(
        f.client
            .call("local_mint", json!({"project": who.project, "name": "Bad"}))
            .await,
    );
    assert_eq!(err.code, ErrorCode::Hub);
    assert!(err.message.contains("422 bad_handle"), "{err:?}");
    f.stop().await;
}

fn put_credential(home: &Home, id: &IdentityId, origin: &str, owner: &str, token: &str) {
    let namespace: url::Url = LOCAL_TOKEN_NAMESPACE.parse().unwrap();
    home.tokens()
        .set(
            &TokenKey::new(&namespace, id).unwrap(),
            &json!({"origin": origin, "owner_id": owner, "local_token": token}).to_string(),
        )
        .unwrap();
}

#[tokio::test]
async fn local_listener_cannot_post_without_a_running_listener() {
    let mut f = Fixture::new().await;
    let raw = result(
        f.client
            .call(
                "local_mint",
                json!({"project": f.created["project"], "name": "listen", "kind": "listener"}),
            )
            .await,
    );
    let listener: IdentityFile = serde_json::from_value(raw).unwrap();
    let err = error(
        f.call(
            &listener,
            "post",
            json!({"envelope": envelope("listener chatter")}),
        )
        .await,
    );
    assert_eq!(err.code, ErrorCode::Forbidden);
    let head = result(
        f.client
            .call("state", json!({"project": listener.project, "key": "head"}))
            .await,
    );
    assert_eq!(head["as_of_seq"], 0);
    f.stop().await;
}

#[tokio::test]
async fn failed_registration_frees_the_minted_handle() {
    let mut f = Fixture::new().await;
    for n in 0..IDENTITY_CAP {
        f.home.seed_identity(
            "https://hub.example",
            &format!("seed{n:04}"),
            &"a".repeat(64),
        );
    }
    let handle = format!("{}/worker", f.created["person"]["handle"].as_str().unwrap());
    let err = error(
        f.client
            .call(
                "local_mint",
                json!({"project": f.created["project"], "name": "worker"}),
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::Hub);
    let roster = result(
        f.client
            .call(
                "state",
                json!({"project": f.created["project"], "key": "roster"}),
            )
            .await,
    );
    assert!(
        roster["value"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["handle"] != json!(handle)),
        "{roster}"
    );
    rusqlite::Connection::open(f.home.database())
        .unwrap()
        .execute("DELETE FROM identities WHERE identity = 'seed0000'", [])
        .unwrap();
    assert_eq!(f.mint("worker").await.handle, handle);
    f.stop().await;
}

#[tokio::test]
async fn a_credential_never_authorizes_the_other_origin() {
    let hub = support::fake_hub().await;
    let home = Home::signed_in(&hub.url).await;
    let mut f = Fixture::start(home).await;
    let local = f.mint("worker").await;
    let local_token = f.credential(&local).local_token;
    let hub_id: IdentityId = "hubagent".parse().unwrap();
    let hub_token = "a".repeat(64);
    f.home.seed_identity(&hub.url, hub_id.as_str(), &hub_token);
    let hub_origin = agmen_client::origin_key(&hub.url.parse().unwrap());
    let local_state = json!({"project": local.project, "key": "head"});
    let hub_inbox = json!({"project": support::PROJECT});
    let with_token = |raw: &Value, token: &str| {
        let mut raw = raw.clone();
        raw["local_token"] = json!(token);
        raw
    };
    result(
        f.call_with(
            local.identity_id.clone(),
            "state",
            with_token(&local_state, &local_token),
        )
        .await,
    );
    result(
        f.call_with(hub_id.clone(), "inbox", with_token(&hub_inbox, &hub_token))
            .await,
    );
    // Each identity's own token, presented for the other origin's identity.
    let err = error(
        f.call_with(
            hub_id.clone(),
            "inbox",
            with_token(&hub_inbox, &local_token),
        )
        .await,
    );
    assert_eq!(err.code, ErrorCode::Unauthorized);
    let err = error(
        f.call_with(
            local.identity_id.clone(),
            "state",
            with_token(&local_state, &hub_token),
        )
        .await,
    );
    assert_eq!(err.code, ErrorCode::Unauthorized);
    // The right token under the other origin's stamp.
    put_credential(
        &f.home,
        &local.identity_id,
        &hub_origin,
        local.owner_id.as_str(),
        &local_token,
    );
    let err = error(
        f.call_with(
            local.identity_id.clone(),
            "state",
            with_token(&local_state, &local_token),
        )
        .await,
    );
    assert_eq!(err.code, ErrorCode::Unauthorized);
    put_credential(&f.home, &hub_id, "local", support::HUMAN, &hub_token);
    let err = error(
        f.call_with(hub_id, "inbox", with_token(&hub_inbox, &hub_token))
            .await,
    );
    assert_eq!(err.code, ErrorCode::Unauthorized);
    f.stop().await;
}

#[tokio::test]
async fn the_person_reads_a_local_inbox_and_state() {
    let mut f = Fixture::new().await;
    let who = f.mint("worker").await;
    let mut dm = envelope("for the person");
    dm.payload = Payload::Dm {};
    dm.to.push(who.owner_id.clone());
    result(f.call(&who, "post", json!({"envelope": dm})).await);
    let inbox = result(
        f.client
            .call("inbox", json!({"project": who.project}))
            .await,
    );
    assert_eq!(inbox["messages"][0]["body"], "for the person");
    let head = result(
        f.client
            .call("state", json!({"project": who.project, "key": "head"}))
            .await,
    );
    assert_eq!(head["value"]["feed_seq"], head["as_of_seq"]);
    let roster = result(
        f.client
            .call("state", json!({"project": who.project, "key": "roster"}))
            .await,
    );
    assert!(
        roster["value"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["identity_id"] == json!(who.identity_id)),
        "{roster}"
    );
    f.stop().await;
}
