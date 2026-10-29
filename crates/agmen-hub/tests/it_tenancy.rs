//! One group can never read, post to or administer another's projects.
//!
//! `isolation_table_covers_every_route` reads every `.rs` file under `src`
//! except `test_github.rs` and `broker/shim.rs`. It compares `.route(` paths,
//! not methods, so a method added to a listed path needs its row by hand.
//! The `it_` tests run under `--profile it`.
#![cfg(test)]

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use agmen_hub::PlanRecord;
use agmen_proto::{DeliveryReply, IdentityId, TenantId, Timestamp, Topics};
use reqwest::{Method, StatusCode};
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::mqttbytes::v5::{PubAckReason, SubscribeReasonCode};
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, PgPool};
use support::mqtt::{self, connect_ok};
use support::{
    Ctx, Project, broker_token, envelope, login, member_with_token, mint_ok, run_hosted, ulid,
};

/// A code no invite was issued with. A does not hold B's invite code, so
/// accept is a lookup miss for a human token.
const UNKNOWN_INVITE_CODE: &str = "unknown-code";

const DAY_SECS: u64 = 86_400;

/// Paths with no tenant check, and why.
const NOT_SCOPED: &[(&str, &str)] = &[
    ("/livez", "no tenant data"),
    ("/healthz", "no tenant data"),
    ("/statusz", "no tenant data"),
    ("/v1/jwks", "public keys"),
    ("/i", "static invite page"),
    ("/v1/auth/device", "sign-in, no tenant"),
    ("/v1/auth/token", "sign-in, no tenant"),
    (
        "/v1/me",
        "the caller's own rows only; it_me_and_groups_stay_in_tenant",
    ),
    (
        "/v1/claim",
        "self-host only, one group; it_hosted_mode_has_no_claim_route (it_claim.rs)",
    ),
    (
        "/mqtt",
        "byte proxy; the broker ACL is tested in it_no_cross_tenant_broker",
    ),
    (
        "/v1/paddle/webhook",
        "signed by Paddle, no bearer token; it_unsigned_webhook_refused and \
         it_webhook_for_other_tenant_only_changes_it (it_billing.rs)",
    ),
    (
        "/v1/groups",
        "creates a new tenant; it_me_and_groups_stay_in_tenant",
    ),
    (
        "/slow",
        "test router in http_tests.rs, not a route of the hub",
    ),
];

/// How many directories the `src` walk may enter.
const MAX_SRC_DIRS: u32 = 64;

/// Message rows read from one tenant. One past the most a fresh pair of
/// groups posts, so a full read means the scan stopped early.
const MESSAGE_SCAN_LIMIT: i64 = 101;

#[derive(Clone, Copy)]
enum Kind {
    Empty,
    Invite,
    Lanes,
    CheckPath,
    Message,
    Project,
    Mint,
    Seats,
    Plan,
    Role,
    Transfer,
}

struct Attempt {
    method: Method,
    route: &'static str,
    kind: Kind,
}

#[derive(Clone, Copy)]
struct Logins {
    owner_sub: u64,
    owner: &'static str,
    admin_sub: u64,
    admin: &'static str,
    member_sub: u64,
    member: &'static str,
    group: &'static str,
}

const SIDE_A: Logins = Logins {
    owner_sub: 1101,
    owner: "ownera",
    admin_sub: 1102,
    admin: "admina",
    member_sub: 1103,
    member: "membera",
    group: "alpha",
};

const SIDE_B: Logins = Logins {
    owner_sub: 2201,
    owner: "ownerb",
    admin_sub: 2202,
    admin: "adminb",
    member_sub: 2203,
    member: "memberb",
    group: "beta",
};

struct Party {
    owner_token: String,
    admin_token: String,
    member_token: String,
    identity_token: String,
    identity_id: String,
    tenant: String,
    project: String,
    group: String,
    owner_handle: String,
    admin_handle: String,
    member_handle: String,
}

impl Party {
    fn tokens(&self) -> [(&str, &str); 4] {
        [
            ("owner", self.owner_token.as_str()),
            ("admin", self.admin_token.as_str()),
            ("member", self.member_token.as_str()),
            ("identity", self.identity_token.as_str()),
        ]
    }
}

fn attempts() -> Vec<Attempt> {
    let mut rows = identity_and_project_attempts();
    rows.extend(tenant_attempts());
    rows
}

fn identity_and_project_attempts() -> Vec<Attempt> {
    vec![
        Attempt {
            method: Method::DELETE,
            route: "/v1/identities/{id}",
            kind: Kind::Empty,
        },
        Attempt {
            method: Method::GET,
            route: "/v1/identities/{id}/broker-token",
            kind: Kind::Empty,
        },
        Attempt {
            method: Method::POST,
            route: "/v1/invites",
            kind: Kind::Invite,
        },
        Attempt {
            method: Method::POST,
            route: "/v1/invites/{code}/accept",
            kind: Kind::Empty,
        },
        Attempt {
            method: Method::GET,
            route: "/v1/projects/{p}/streams/{s}",
            kind: Kind::Empty,
        },
        Attempt {
            method: Method::GET,
            route: "/v1/projects/{project}/lanes",
            kind: Kind::Empty,
        },
        Attempt {
            method: Method::PUT,
            route: "/v1/projects/{project}/lanes",
            kind: Kind::Lanes,
        },
        Attempt {
            method: Method::POST,
            route: "/v1/projects/{project}/check-path",
            kind: Kind::CheckPath,
        },
        Attempt {
            method: Method::GET,
            route: "/v1/projects/{project}/items",
            kind: Kind::Empty,
        },
        Attempt {
            method: Method::GET,
            route: "/v1/projects/{project}/state",
            kind: Kind::Empty,
        },
        Attempt {
            method: Method::POST,
            route: "/v1/projects",
            kind: Kind::Project,
        },
    ]
}

fn tenant_attempts() -> Vec<Attempt> {
    vec![
        Attempt {
            method: Method::GET,
            route: "/v1/tenants/{tenant}/members",
            kind: Kind::Empty,
        },
        Attempt {
            method: Method::DELETE,
            route: "/v1/tenants/{tenant}/members/{handle}",
            kind: Kind::Empty,
        },
        Attempt {
            method: Method::PUT,
            route: "/v1/tenants/{tenant}/members/{handle}/role",
            kind: Kind::Role,
        },
        Attempt {
            method: Method::POST,
            route: "/v1/tenants/{tenant}/transfer",
            kind: Kind::Transfer,
        },
        Attempt {
            method: Method::GET,
            route: "/v1/tenants/{tenant}/seats",
            kind: Kind::Empty,
        },
        Attempt {
            method: Method::PUT,
            route: "/v1/tenants/{tenant}/seats",
            kind: Kind::Seats,
        },
        Attempt {
            method: Method::POST,
            route: "/v1/tenants/{tenant}/plan",
            kind: Kind::Plan,
        },
        Attempt {
            method: Method::POST,
            route: "/v1/tenants/{tenant}/portal",
            kind: Kind::Empty,
        },
        Attempt {
            method: Method::POST,
            route: "/v1/identities",
            kind: Kind::Mint,
        },
        Attempt {
            method: Method::POST,
            route: "/v1/projects/{project}/messages",
            kind: Kind::Message,
        },
        Attempt {
            method: Method::GET,
            route: "/v1/projects/{project}/events",
            kind: Kind::Empty,
        },
    ]
}

fn route_set() -> BTreeSet<&'static str> {
    attempts()
        .into_iter()
        .map(|attempt| attempt.route)
        .collect()
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut dirs = 0u32;
    while let Some(dir) = stack.pop() {
        dirs += 1;
        assert!(
            dirs <= MAX_SRC_DIRS,
            "src walk passed {MAX_SRC_DIRS} directories at {}",
            dir.display()
        );
        let mut children = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            children.push(entry.unwrap().path());
        }
        children.sort();
        for path in children {
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }
    files
}

fn skipped(rel: &Path) -> bool {
    rel == Path::new("test_github.rs") || rel == Path::new("broker").join("shim.rs")
}

/// Path literals of `.route(` calls. Whitespace, including a newline, may
/// sit between `.route(` and the quote.
fn route_literals(file: &Path, source: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find(".route(") {
        rest = &rest[at + ".route(".len()..];
        let trimmed = rest.trim_start();
        assert!(
            trimmed.starts_with('"'),
            "{} has a .route( call that is not a plain string literal",
            file.display()
        );
        let bytes = trimmed.as_bytes();
        let mut end = 1;
        while end < bytes.len() && bytes[end] != b'"' {
            assert!(
                bytes[end] != b'\\',
                "{} has an escaped quote in a route path",
                file.display()
            );
            end += 1;
        }
        assert!(
            end < bytes.len(),
            "{} has an unclosed route string",
            file.display()
        );
        let path = std::str::from_utf8(&bytes[1..end]).unwrap();
        found.push(path.to_owned());
        rest = &trimmed[end + 1..];
    }
    found
}

fn discovered() -> Vec<(String, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    for file in rust_files(&root) {
        let rel = file.strip_prefix(&root).unwrap();
        if skipped(rel) {
            continue;
        }
        let source = std::fs::read_to_string(&file).unwrap();
        for path in route_literals(&file, &source) {
            found.push((rel.display().to_string(), path));
        }
    }
    found
}

#[test]
fn isolation_table_covers_every_route() {
    let routes = route_set();
    let excused: BTreeSet<&str> = NOT_SCOPED.iter().map(|(path, _)| *path).collect();
    for path in &routes {
        assert!(
            !excused.contains(path),
            "{path} is both a tenant route and not scoped"
        );
    }
    let found = discovered();
    let mut missing = Vec::new();
    let mut seen = BTreeSet::new();
    for (file, path) in &found {
        seen.insert(path.as_str());
        if !routes.contains(path.as_str()) && !excused.contains(path.as_str()) {
            missing.push(format!("{file} {path}"));
        }
    }
    assert!(
        missing.is_empty(),
        "paths in neither ROUTES nor NOT_SCOPED:\n{}",
        missing.join("\n")
    );
    let mut absent = Vec::new();
    for path in routes.iter().copied().chain(excused) {
        if !seen.contains(path) {
            absent.push(path);
        }
    }
    assert!(
        absent.is_empty(),
        "listed paths that no .route( literal names:\n{}",
        absent.join("\n")
    );
}

fn fill(route: &str, party: &Party) -> String {
    // `{project}` before `{p}`: `{p}` is a prefix of `{project}`.
    route
        .replace("{project}", &party.project)
        .replace("{tenant}", &party.tenant)
        .replace("{handle}", &party.member_handle)
        .replace("{code}", UNKNOWN_INVITE_CODE)
        .replace("{id}", &party.identity_id)
        .replace("{p}", &party.project)
        .replace("{s}", "feed")
}

fn request_path(route: &str, party: &Party) -> String {
    let path = fill(route, party);
    assert!(
        !path.contains('{'),
        "{route} still has a placeholder: {path}"
    );
    if route.ends_with("/state") {
        format!("{path}?key=roster")
    } else {
        path
    }
}

fn body_bytes(kind: Kind, party: &Party) -> Option<Vec<u8>> {
    let value = match kind {
        Kind::Empty => return None,
        Kind::Invite => json!({"project": party.project, "role": "member"}),
        Kind::Lanes => json!({"lanes": []}),
        Kind::CheckPath => json!({"identity": party.identity_id, "paths": ["README.md"]}),
        Kind::Message => {
            return Some(envelope(&ulid(), "FYI", json!({}), json!({})));
        }
        Kind::Project => json!({"name": "extra", "tenant": party.tenant}),
        Kind::Mint => json!({
            "handle": format!("{}/nope", party.owner_handle),
            "project": party.project,
            "kind": "agent",
        }),
        Kind::Seats => json!({"paid": 4}),
        Kind::Plan => json!({"plan": "team"}),
        Kind::Role => json!({"role": "member"}),
        Kind::Transfer => json!({"to": party.admin_handle}),
    };
    Some(serde_json::to_vec(&value).unwrap())
}

async fn send(
    ctx: &Ctx,
    method: Method,
    path: &str,
    token: &str,
    body: Option<Vec<u8>>,
) -> StatusCode {
    let mut request = ctx
        .http
        .request(method, ctx.hub.url(path))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(body);
    }
    // Drop the body unread. `GET .../events` is a stream when it answers
    // 200, and reading it would wait until the stream ends.
    let response = request.send().await.unwrap();
    let status = response.status();
    drop(response);
    status
}

async fn create_group(ctx: &Ctx, token: &str, name: &str) -> Value {
    let response = ctx
        .http
        .post(ctx.hub.url("/v1/groups"))
        .bearer_auth(token)
        .json(&json!({"name": name, "project": "web"}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

async fn party(ctx: &Ctx, who: Logins) -> Party {
    let owner_token = login(ctx, who.owner_sub, who.owner).await;
    let created = create_group(ctx, &owner_token, who.group).await;
    let project = Project {
        tenant: created["tenant"].as_str().unwrap().to_owned(),
        project: created["project"].as_str().unwrap().to_owned(),
    };
    let admin_token = member_with_token(ctx, &project, who.admin_sub, who.admin, "admin").await;
    let member_token = member_with_token(ctx, &project, who.member_sub, who.member, "member").await;
    let minted = mint_ok(ctx, &owner_token, &format!("{}/sol", who.owner), &project).await;
    Party {
        owner_token,
        admin_token,
        member_token,
        identity_token: minted.token,
        identity_id: minted.id,
        tenant: project.tenant,
        project: project.project,
        group: created["group"].as_str().unwrap().to_owned(),
        owner_handle: who.owner.to_owned(),
        admin_handle: who.admin.to_owned(),
        member_handle: who.member.to_owned(),
    }
}

fn ident(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// The row count and an md5 of the rows' text for the rows of `table` that
/// `filter` keeps (`$1` is `ids`). The md5 makes an UPDATE show, which a
/// count alone would miss: revoking an identity or changing a role adds and
/// removes nothing.
async fn fingerprint(pool: &PgPool, table: &str, filter: &str, ids: &[String]) -> (i64, String) {
    assert!(ident(table), "{table}");
    let sql = format!(
        "SELECT count(*), coalesce(md5(string_agg(t::text, '|' ORDER BY t::text)), '')
         FROM \"{table}\" t WHERE {filter}"
    );
    sqlx::query_as(AssertSqlSafe(sql))
        .bind(ids)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// B's rows: every table with a `tenant_id` or `project_id` column, read from
/// the schema so a table added later is covered, plus the three tables keyed
/// another way.
async fn tenant_rows(pool: &PgPool, who: &Party) -> BTreeMap<String, (i64, String)> {
    let columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.table_name::text, c.column_name::text
         FROM information_schema.columns c
         JOIN information_schema.tables t
           ON t.table_schema = c.table_schema AND t.table_name = c.table_name
         WHERE c.table_schema = 'public' AND t.table_type = 'BASE TABLE'
           AND c.column_name IN ('tenant_id', 'project_id')
         ORDER BY 1, 2",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let tenant = [who.tenant.clone()];
    let project = [who.project.clone()];
    let identity = [who.identity_id.clone()];
    let mut found = BTreeMap::new();
    for (table, column) in columns {
        let (filter, ids) = if column == "tenant_id" {
            ("t.tenant_id = ANY($1)", &tenant)
        } else {
            ("t.project_id = ANY($1)", &project)
        };
        let rows = fingerprint(pool, &table, filter, ids).await;
        found.insert(format!("{table}.{column}"), rows);
    }
    for (table, filter, ids) in [
        ("tenants", "t.id = ANY($1)", &tenant),
        (
            "stream_heads",
            "split_part(t.stream, '/', 1) = ANY($1)",
            &project,
        ),
        ("tokens", "t.identity_id = ANY($1)", &identity),
    ] {
        found.insert(
            table.to_owned(),
            fingerprint(pool, table, filter, ids).await,
        );
    }
    found
}

/// Tables a fresh group always has rows in. If one were empty before the
/// attacks, "B's rows are unchanged" would prove nothing.
const ALWAYS_PRESENT: [&str; 6] = [
    "tenants",
    "projects.tenant_id",
    "tenant_members.tenant_id",
    "memberships.project_id",
    "identities.tenant_id",
    "tokens",
];

#[tokio::test]
async fn it_no_cross_tenant_http() {
    run_hosted(|ctx| async move {
        let a = party(&ctx, SIDE_A).await;
        let b = party(&ctx, SIDE_B).await;
        // One route of each kind answers B's own credentials, so A's refusals
        // below are about B and not about a route that refuses everyone.
        for (path, token) in [
            (format!("/v1/tenants/{}/members", b.tenant), &b.owner_token),
            (format!("/v1/projects/{}/items", b.project), &b.member_token),
            (
                format!("/v1/identities/{}/broker-token", b.identity_id),
                &b.identity_token,
            ),
        ] {
            let status = send(&ctx, Method::GET, &path, token, None).await;
            assert_eq!(status, StatusCode::OK, "B's own {path}");
        }
        let before = tenant_rows(&ctx.pool, &b).await;
        for key in ALWAYS_PRESENT {
            assert!(
                before.get(key).is_some_and(|(rows, _)| *rows > 0),
                "B has no {key} rows before the attacks: {before:?}"
            );
        }
        for attempt in attempts() {
            let path = request_path(attempt.route, &b);
            for (who, token) in a.tokens() {
                let status = send(
                    &ctx,
                    attempt.method.clone(),
                    &path,
                    token,
                    body_bytes(attempt.kind, &b),
                )
                .await;
                assert!(
                    status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
                    "{} {path} as A's {who} answered {status}",
                    attempt.method
                );
            }
        }
        let after = tenant_rows(&ctx.pool, &b).await;
        assert_eq!(after, before, "B's rows changed");
    })
    .await;
}

fn project_root(topics: &Topics) -> String {
    topics
        .feed()
        .strip_suffix("/feed")
        .expect("feed topic ends in /feed")
        .to_owned()
}

#[tokio::test]
async fn it_no_cross_tenant_broker() {
    run_hosted(|ctx| async move {
        let a = party(&ctx, SIDE_A).await;
        let b = party(&ctx, SIDE_B).await;
        let (status, body) =
            broker_token(&ctx.http, &ctx.hub, &a.identity_id, &a.identity_token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let jwt = body["token"].as_str().unwrap();
        // The support client decodes MQTT reason 0x87 (135) as NotAuthorized.
        let mut client = connect_ok(mqtt::options(
            &format!("{}-mcp", a.identity_id),
            &a.identity_id,
            jwt,
        ))
        .await;
        // A's own topics work on this connection, so the refusals below come
        // from B's topics and not from the token or the client id.
        let own = Topics::new(a.tenant.parse().unwrap(), a.project.parse().unwrap());
        let own_id: IdentityId = a.identity_id.parse().unwrap();
        let published = client.publish(&own.up(&own_id), b"own").await;
        assert!(
            matches!(
                published,
                PubAckReason::Success | PubAckReason::NoMatchingSubscribers
            ),
            "A's own up/ answered {published:?}"
        );
        assert_eq!(
            client.subscribe(&own.feed()).await,
            SubscribeReasonCode::Success(QoS::AtLeastOnce),
            "A's own feed"
        );
        let topics = Topics::new(b.tenant.parse().unwrap(), b.project.parse().unwrap());
        let other: IdentityId = b.identity_id.parse().unwrap();
        let root = project_root(&topics);
        let (tenant_root, _) = root.rsplit_once('/').expect("a project root has a tenant");
        assert_eq!(
            client.publish(&topics.up(&other), b"cross").await,
            PubAckReason::NotAuthorized
        );
        // The last two reach past B's project: all of B's tenant, and every topic.
        for filter in [
            topics.feed(),
            format!("{root}/state/#"),
            format!("{root}/presence/+"),
            format!("{tenant_root}/#"),
            "#".to_owned(),
        ] {
            assert_eq!(
                client.subscribe(&filter).await,
                SubscribeReasonCode::NotAuthorized,
                "{filter}"
            );
        }
        client.disconnect().await;
    })
    .await;
}

async fn invite(ctx: &Ctx, token: &str, project: &str) -> (StatusCode, Value) {
    let response = ctx
        .http
        .post(ctx.hub.url("/v1/invites"))
        .bearer_auth(token)
        .json(&json!({"project": project, "role": "member"}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn invite_count(ctx: &Ctx, tenant: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM invites WHERE tenant_id = $1")
        .bind(tenant)
        .fetch_one(&ctx.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn it_invite_cannot_cross_tenants() {
    run_hosted(|ctx| async move {
        let a = party(&ctx, SIDE_A).await;
        let b = party(&ctx, SIDE_B).await;
        // A's admin may invite into A's own project, so the refusal below is
        // about B's project and not about A's admin.
        let own_before = invite_count(&ctx, &a.tenant).await;
        let (status, body) = invite(&ctx, &a.admin_token, &a.project).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(invite_count(&ctx, &a.tenant).await, own_before + 1);
        let before = invite_count(&ctx, &b.tenant).await;
        let (status, body) = invite(&ctx, &a.admin_token, &b.project).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(invite_count(&ctx, &b.tenant).await, before);
    })
    .await;
}

fn whole_now() -> Timestamp {
    Timestamp::from_second(Timestamp::now().as_second()).unwrap()
}

fn ago(now: Timestamp, days: u64) -> Timestamp {
    now - Duration::from_secs(days * DAY_SECS + 60)
}

async fn message_text(ctx: &Ctx, tenant: &str) -> Vec<String> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT m.envelope::text FROM messages m
         JOIN projects p ON p.id = m.project_id
         WHERE p.tenant_id = $1
         ORDER BY m.project_id, m.stream, m.seq
         LIMIT $2",
    )
    .bind(tenant)
    .bind(MESSAGE_SCAN_LIMIT)
    .fetch_all(&ctx.pool)
    .await
    .unwrap();
    assert!(
        i64::try_from(rows.len()).unwrap() < MESSAGE_SCAN_LIMIT,
        "message scan hit its limit of {MESSAGE_SCAN_LIMIT}"
    );
    rows
}

#[tokio::test]
async fn it_hub_posts_stay_in_tenant() {
    run_hosted(|ctx| async move {
        let a = party(&ctx, SIDE_A).await;
        let b = party(&ctx, SIDE_B).await;
        let b_before = message_text(&ctx, &b.tenant).await;
        let tenant: TenantId = a.tenant.parse().unwrap();
        let topics = Topics::new(tenant.clone(), a.project.parse().unwrap());
        ctx.hub.hub.set_usage(&tenant, 0, 3_999).await.unwrap();
        let from: IdentityId = a.identity_id.parse().unwrap();
        let reply = ctx
            .hub
            .hub
            .ingest(
                &topics,
                &from,
                &envelope(&ulid(), "FYI", json!({}), json!({})),
            )
            .await
            .unwrap();
        assert!(matches!(reply, DeliveryReply::Ack { .. }), "{reply:?}");
        let now = whole_now();
        let started = ago(now, 4);
        ctx.hub
            .hub
            .set_plan(&tenant, &PlanRecord::trial(started))
            .await
            .unwrap();
        assert_eq!(ctx.hub.hub.tick_plans_at(now).await.unwrap(), 1);
        let name = &a.group;
        let quota = format!(
            "{name} used 80% of today's 5,000 messages; posts stop at 5,000 until 00:00 UTC."
        );
        let ends = started + Duration::from_secs(7 * DAY_SECS);
        let reminder = format!(
            "Your agmen hosted trial of {name} ends on {ends}. Run agmen group billing to keep going."
        );
        let a_text = message_text(&ctx, &a.tenant).await;
        let b_text = message_text(&ctx, &b.tenant).await;
        assert!(
            a_text.iter().any(|text| text.contains(&quota)),
            "quota alert missing from A: {a_text:?}"
        );
        assert!(
            a_text.iter().any(|text| text.contains(&reminder)),
            "trial reminder missing from A: {a_text:?}"
        );
        // Any new message in B is a leak, including one that names B's own
        // group, which a search for A's two texts would not find.
        assert_eq!(b_text, b_before, "A's hub posts reached B");
    })
    .await;
}

async fn me(ctx: &Ctx, token: &str) -> Value {
    let response = ctx
        .http
        .get(ctx.hub.url("/v1/me"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// Why `/v1/me` and `/v1/groups` are in `NOT_SCOPED`: `/v1/me` lists only the
/// caller's own groups and projects, and `/v1/groups` makes a tenant of its
/// own, whatever name it is given, without joining or changing another.
#[tokio::test]
async fn it_me_and_groups_stay_in_tenant() {
    run_hosted(|ctx| async move {
        let a = party(&ctx, SIDE_A).await;
        let b = party(&ctx, SIDE_B).await;
        let before = tenant_rows(&ctx.pool, &b).await;
        for token in [&a.owner_token, &a.admin_token, &a.member_token] {
            let body = me(&ctx, token).await;
            assert_eq!(body["groups"].as_array().map(Vec::len), Some(1), "{body}");
            assert_eq!(body["groups"][0]["tenant"], a.tenant.as_str(), "{body}");
            assert_eq!(body["projects"].as_array().map(Vec::len), Some(1), "{body}");
            assert_eq!(body["projects"][0]["project"], a.project.as_str(), "{body}");
            let text = body.to_string();
            for id in [&b.tenant, &b.project, &b.identity_id] {
                assert!(!text.contains(id.as_str()), "B's {id} is in A's /v1/me");
            }
        }
        // A body cannot name a tenant: the route only makes new ones.
        let response = ctx
            .http
            .post(ctx.hub.url("/v1/groups"))
            .bearer_auth(&a.owner_token)
            .json(&json!({"name": SIDE_B.group, "project": "web", "tenant": b.tenant}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        // A group named like B's group is a tenant of its own.
        let made = create_group(&ctx, &a.owner_token, SIDE_B.group).await;
        assert_eq!(
            made["group"],
            format!("{}/{}", a.owner_handle, SIDE_B.group)
        );
        let tenant = made["tenant"].as_str().unwrap();
        assert!(tenant != a.tenant && tenant != b.tenant, "{made}");
        assert_eq!(tenant_rows(&ctx.pool, &b).await, before, "B's rows changed");
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT m.tenant_id FROM tenant_members m JOIN humans h ON h.id = m.human_id
             WHERE h.handle = $1",
        )
        .bind(&a.owner_handle)
        .fetch_all(&ctx.pool)
        .await
        .unwrap();
        let joined: BTreeSet<String> = rows.into_iter().collect();
        assert_eq!(
            joined,
            BTreeSet::from([a.tenant.clone(), tenant.to_owned()]),
            "A's owner belongs to a group it did not make"
        );
    })
    .await;
}
