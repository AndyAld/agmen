//! Hub login (RFC 8628 device flow run by the hub against GitHub)
//! and invites, against an in-process hub, a fake GitHub and a database of
//! the test's own on `AGMEN_IT_PG`.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use jiff::Timestamp;
use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{
    Ctx, count, login, member_with_token, poll_device_token, run, seed_human, seed_member,
    seed_project, start_device_login,
};

const ANDY: u64 = 4242;

async fn refresh(ctx: &Ctx, refresh_token: &str) -> (StatusCode, Value) {
    let resp = ctx
        .http
        .post(ctx.hub.url("/v1/auth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .unwrap();
    (resp.status(), resp.json().await.unwrap())
}

#[tokio::test]
async fn it_device_flow_issues_human_token() {
    run(|ctx| async move {
        let project = seed_project(&ctx.pool).await;
        let andy = seed_human(&ctx.pool, ANDY, "andy").await;
        seed_member(&ctx.pool, &andy, &project, "member").await;

        let start = start_device_login(&ctx).await;
        assert_eq!(start.user_code.len(), 9, "user code {}", start.user_code);
        assert_eq!(start.body["interval"], 1);
        assert_eq!(start.body["expires_in"], 900);
        assert!(start.verification_uri.ends_with("/login/device"));
        assert!(start.body.get("verification_uri_complete").is_none());

        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "authorization_pending");

        // GitHub reports a different login case; the seeded human matches by
        // account id, not by login.
        ctx.github.approve(&start.user_code, ANDY, "Andy");
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["token_type"], "Bearer");
        assert_eq!(body["expires_in"], 3600);
        let access = body["access_token"].as_str().unwrap();
        let refresh_token = body["refresh_token"].as_str().unwrap();
        assert_ne!(access, refresh_token);
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM humans WHERE provider_sub = $1",
                "4242"
            )
            .await,
            1,
            "sign-in made a second human for the seeded account"
        );

        // The access token acts as the seeded human: it mints andy/ children.
        let (status, minted) = support::mint(
            &ctx,
            access,
            &json!({ "handle": "andy/sol", "project": project.project }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{minted}");

        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM device_authorizations")
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);

        // A device code is redeemed once.
        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");
    })
    .await;
}

/// Asserts the anti-framing headers every hub page sends (ruling R38), and
/// `Cache-Control: no-store` when `no_store` is set.
fn assert_unframeable(resp: &reqwest::Response, no_store: bool) {
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .map(|value| value.to_str().unwrap().to_owned())
    };
    let what = resp.url().path().to_owned();
    assert_eq!(header("x-frame-options").as_deref(), Some("DENY"), "{what}");
    assert_eq!(
        header("content-security-policy").as_deref(),
        Some("frame-ancestors 'none'"),
        "{what}"
    );
    if no_store {
        assert_eq!(
            header("cache-control").as_deref(),
            Some("no-store"),
            "{what}"
        );
    }
}

#[tokio::test]
async fn it_responses_refuse_framing() {
    run(|ctx| async move {
        // Move the whole context: capturing only `ctx.http` drops the fake GitHub.
        let ctx = ctx;
        let start = ctx
            .http
            .post(ctx.hub.url("/v1/auth/device"))
            .send()
            .await
            .unwrap();
        assert_eq!(start.status(), StatusCode::OK);
        assert_unframeable(&start, false);
        let jwks = ctx.http.get(ctx.hub.url("/v1/jwks")).send().await.unwrap();
        assert_unframeable(&jwks, false);
    })
    .await;
}

#[tokio::test]
async fn it_device_flow_new_human_gets_lowercase_handle() {
    run(|ctx| async move {
        login(&ctx, 777, "Adam-K").await;
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM humans WHERE handle = $1",
                "adam-k"
            )
            .await,
            1
        );
    })
    .await;
}

#[tokio::test]
async fn it_refresh_token_rotates() {
    run(|ctx| async move {
        seed_human(&ctx.pool, ANDY, "andy").await;
        let start = start_device_login(&ctx).await;
        ctx.github.approve(&start.user_code, ANDY, "andy");
        let (_, first) = poll_device_token(&ctx, &start.device_code).await;
        let old = first["refresh_token"].as_str().unwrap();

        let (status, second) = refresh(&ctx, old).await;
        assert_eq!(status, StatusCode::OK, "{second}");
        assert_ne!(second["refresh_token"], first["refresh_token"]);
        assert_ne!(second["access_token"], first["access_token"]);

        let (status, body) = refresh(&ctx, old).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "old refresh token reused");
        assert_eq!(body["error"], "invalid_grant");

        // An access token is not a refresh token.
        let (status, _) = refresh(&ctx, second["access_token"].as_str().unwrap()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    })
    .await;
}

#[tokio::test]
async fn it_invite_once() {
    run(|ctx| async move {
        let project = seed_project(&ctx.pool).await;
        let andy = member_with_token(&ctx, &project, ANDY, "andy", "admin").await;
        let adam = login(&ctx, 2, "adam").await;
        let eve = login(&ctx, 3, "eve").await;

        // Only project admins invite.
        let refused = ctx
            .http
            .post(ctx.hub.url("/v1/invites"))
            .bearer_auth(&adam)
            .json(&json!({ "project": project.project, "role": "member" }))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);

        let created = ctx
            .http
            .post(ctx.hub.url("/v1/invites"))
            .bearer_auth(&andy)
            .json(&json!({ "project": project.project, "role": "member" }))
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let invite: Value = created.json().await.unwrap();
        let code = invite["code"].as_str().unwrap();
        assert!(invite["expires_at"].is_string(), "{invite}");

        let accept = |token: String| {
            let req = ctx
                .http
                .post(ctx.hub.url(&format!("/v1/invites/{code}/accept")))
                .bearer_auth(token);
            async move { req.send().await.unwrap() }
        };
        let first = accept(adam.clone()).await;
        assert_eq!(first.status(), StatusCode::OK);
        let joined: Value = first.json().await.unwrap();
        assert_eq!(joined["project"], project.project.as_str());
        assert_eq!(joined["tenant"], project.tenant.as_str());
        assert_eq!(joined["role"], "member");

        assert_eq!(accept(eve).await.status(), StatusCode::CONFLICT);
        assert_eq!(accept(adam).await.status(), StatusCode::CONFLICT);
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM memberships WHERE project_id = $1",
                &project.project
            )
            .await,
            2,
            "andy and adam are members, eve is not"
        );

        let unknown = ctx
            .http
            .post(ctx.hub.url("/v1/invites/nosuchcode/accept"))
            .bearer_auth(&andy)
            .send()
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    })
    .await;
}

/// `POST /v1/invites` with `token` and `body`: the status and the JSON
/// answer (`null` when the answer is not JSON).
async fn invite(ctx: &Ctx, token: &str, body: &Value) -> (StatusCode, Value) {
    let resp = ctx
        .http
        .post(ctx.hub.url("/v1/invites"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// Admins invite any role; coordinators invite members only (ruling R58).
#[tokio::test]
async fn it_invite_roles_by_inviter() {
    run(|ctx| async move {
        let project = seed_project(&ctx.pool).await;
        let admin = member_with_token(&ctx, &project, ANDY, "andy", "admin").await;
        let coord = member_with_token(&ctx, &project, 2, "adam", "coordinator").await;
        let member = member_with_token(&ctx, &project, 3, "mel", "member").await;
        let ask = |role: &str| json!({ "project": project.project, "role": role });

        for role in ["member", "coordinator", "admin"] {
            let (status, body) = invite(&ctx, &admin, &ask(role)).await;
            assert_eq!(status, StatusCode::CREATED, "admin inviting {role}: {body}");
            assert_eq!(body["role"], role);
        }

        let (status, body) = invite(&ctx, &coord, &ask("member")).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "coordinator inviting member: {body}"
        );
        for role in ["coordinator", "admin"] {
            let (status, body) = invite(&ctx, &coord, &ask(role)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "coordinator inviting {role}");
            assert_eq!(body["error"], "admin_required", "{body}");
        }

        let (status, body) = invite(&ctx, &member, &ask("member")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "member inviting");
        assert_eq!(body["error"], "coordinator_required", "{body}");

        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM invites WHERE project_id = $1",
                &project.project
            )
            .await,
            4,
            "only the allowed invites were stored"
        );
    })
    .await;
}

/// `expires_in_secs` is optional, 3600 to 2592000, default 604800 (ruling
/// R58); the request still refuses unknown fields.
#[tokio::test]
async fn it_invite_expiry_bounds() {
    run(|ctx| async move {
        let project = seed_project(&ctx.pool).await;
        let admin = member_with_token(&ctx, &project, ANDY, "andy", "admin").await;
        let ask = |extra: Value| {
            let mut body = json!({ "project": project.project, "role": "member" });
            for (key, value) in extra.as_object().unwrap() {
                body[key] = value.clone();
            }
            body
        };
        // Seconds from now to the invite's expires_at.
        let lifetime = |body: &Value| {
            let at: Timestamp = body["expires_at"].as_str().unwrap().parse().unwrap();
            at.as_second() - Timestamp::now().as_second()
        };

        for (extra, want) in [
            (json!({}), 604_800),
            (json!({ "expires_in_secs": 3600 }), 3600),
            (json!({ "expires_in_secs": 2_592_000 }), 2_592_000),
        ] {
            let (status, body) = invite(&ctx, &admin, &ask(extra.clone())).await;
            assert_eq!(status, StatusCode::CREATED, "{extra}: {body}");
            let got = lifetime(&body);
            assert!(
                (want - 60..=want).contains(&got),
                "{extra}: expires in {got} s, want {want}"
            );
        }

        for secs in [0, 3599, 2_592_001, -1] {
            let (status, body) =
                invite(&ctx, &admin, &ask(json!({ "expires_in_secs": secs }))).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{secs}: {body}");
            assert_eq!(body["error"], "invalid_expiry", "{body}");
        }

        let (status, _) = invite(&ctx, &admin, &ask(json!({ "expires": "1d" }))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "an unknown field");

        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM invites WHERE project_id = $1",
                &project.project
            )
            .await,
            3,
            "refused requests stored nothing"
        );
    })
    .await;
}

/// Runs `agmen-hub bootstrap --tenant <tenant> --project <project> <admin>`
/// against the test's database and the fake GitHub.
async fn run_bootstrap(
    ctx: &Ctx,
    tenant: &str,
    project: &str,
    admin: &[&str],
) -> std::process::Output {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_agmen-hub"));
    command
        .args(["bootstrap", "--tenant", tenant, "--project", project])
        .args(admin)
        .env("AGMEN_DATABASE_URL", &ctx.db_url)
        .env("AGMEN_GITHUB_API_URL", ctx.github.base().as_str());
    tokio::task::spawn_blocking(move || command.output())
        .await
        .unwrap()
        .unwrap()
}

/// [`run_bootstrap`], asserting success, and the JSON it printed.
async fn bootstrap(ctx: &Ctx, tenant: &str, project: &str, admin: &[&str]) -> Value {
    let out = run_bootstrap(ctx, tenant, project, admin).await;
    assert!(
        out.status.success(),
        "bootstrap failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn is_hub_id(value: &Value) -> bool {
    value.as_str().is_some_and(|id| {
        (8..=32).contains(&id.len())
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

async fn role_in(ctx: &Ctx, handle: &str, project: &Value) -> Option<String> {
    sqlx::query_scalar(
        "SELECT m.role FROM memberships m JOIN humans h ON h.id = m.human_id
         WHERE h.handle = $1 AND m.project_id = $2",
    )
    .bind(handle)
    .bind(project.as_str().unwrap())
    .fetch_optional(&ctx.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn it_bootstrap_creates_project_and_first_admin() {
    run(|ctx| async move {
        ctx.github.register(30, "Root-Admin");
        let made = bootstrap(&ctx, "Acme Co", "agmen", &["--admin-github", "root-ADMIN"]).await;
        assert!(is_hub_id(&made["tenant"]), "{made}");
        assert!(is_hub_id(&made["project"]), "{made}");
        // The login resolved to GitHub's numeric id; the login is shown as
        // GitHub spells it, lowercased.
        assert_eq!(made["admin_github_id"], 30, "{made}");
        assert_eq!(made["admin_github"], "root-admin");
        let tenant = made["tenant"].as_str().unwrap();
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM projects WHERE tenant_id = $1 AND name = 'agmen'",
                tenant
            )
            .await,
            1
        );

        // Idempotent: the same names give the same ids and no new rows.
        assert_eq!(
            bootstrap(&ctx, "Acme Co", "agmen", &["--admin-github", "root-admin"]).await,
            made
        );
        for table in ["tenants", "projects", "bootstrap_admins"] {
            let rows: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM {table}"
            )))
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
            assert_eq!(rows, 1, "{table}");
        }

        // The first sign-in of account 30, under any login, makes the admin.
        let other = login(&ctx, 31, "bob").await;
        let admin = login(&ctx, 30, "ROOT-admin").await;
        assert_eq!(
            role_in(&ctx, "root-admin", &made["project"]).await.as_deref(),
            Some("admin")
        );
        assert_eq!(role_in(&ctx, "bob", &made["project"]).await, None);
        let invite = |token: &str| {
            ctx.http
                .post(ctx.hub.url("/v1/invites"))
                .bearer_auth(token)
                .json(&json!({ "project": made["project"], "role": "member" }))
                .send()
        };
        assert_eq!(invite(&admin).await.unwrap().status(), StatusCode::CREATED);
        assert_eq!(invite(&other).await.unwrap().status(), StatusCode::FORBIDDEN);

        // A re-run after the claim changes nothing: the row stays claimed.
        assert_eq!(
            bootstrap(&ctx, "Acme Co", "agmen", &["--admin-github", "root-admin"]).await,
            made
        );
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM bootstrap_admins WHERE claimed_by IS NULL AND project_id = $1",
                made["project"].as_str().unwrap()
            )
            .await,
            0
        );

        // A second project in the tenant, named by id with no lookup, whose
        // admin signed in before the bootstrap: the membership comes at
        // their next sign-in.
        let second = bootstrap(&ctx, "Acme Co", "docs", &["--admin-github-id", "31"]).await;
        assert_eq!(second["tenant"], made["tenant"]);
        assert_ne!(second["project"], made["project"]);
        assert_eq!(second["admin_github_id"], 31, "{second}");
        assert_eq!(second["admin_github"], Value::Null, "{second}");
        assert_eq!(role_in(&ctx, "bob", &second["project"]).await, None);
        login(&ctx, 31, "bob").await;
        assert_eq!(
            role_in(&ctx, "bob", &second["project"]).await.as_deref(),
            Some("admin")
        );
    })
    .await;
}

#[tokio::test]
async fn it_bootstrap_admin_is_bound_to_the_github_id() {
    run(|ctx| async move {
        // Ruling R39: account 30 holds the login at bootstrap time, then
        // renames, and account 99 takes the freed login.
        ctx.github.register(30, "root-admin");
        let made = bootstrap(&ctx, "Acme Co", "agmen", &["--admin-github", "root-admin"]).await;
        assert_eq!(made["admin_github_id"], 30, "{made}");
        let squatter = login(&ctx, 99, "Root-Admin").await;
        assert_eq!(role_in(&ctx, "root-admin", &made["project"]).await, None);
        let invite = ctx
            .http
            .post(ctx.hub.url("/v1/invites"))
            .bearer_auth(&squatter)
            .json(&json!({ "project": made["project"], "role": "member" }))
            .send()
            .await
            .unwrap();
        assert_eq!(invite.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM bootstrap_admins WHERE claimed_by IS NULL AND project_id = $1",
                made["project"].as_str().unwrap()
            )
            .await,
            1,
            "the squatter's sign-in must not claim the row"
        );

        // The real account signs in under its new login and gets the role.
        login(&ctx, 30, "root-renamed").await;
        assert_eq!(
            role_in(&ctx, "root-renamed", &made["project"]).await.as_deref(),
            Some("admin")
        );

        // A login GitHub does not know fails the bootstrap and writes nothing.
        let out = run_bootstrap(&ctx, "Acme Co", "other", &["--admin-github", "nobody-here"]).await;
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("nobody-here"), "{stderr}");
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM projects WHERE name = $1",
                "other"
            )
            .await,
            0
        );
    })
    .await;
}

#[tokio::test]
async fn it_device_poll_follows_github_interval() {
    run(|ctx| async move {
        ctx.github.set_interval(2);
        let start = start_device_login(&ctx).await;
        for _ in 0..2 {
            let (status, body) = poll_device_token(&ctx, &start.device_code).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["error"], "authorization_pending");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(ctx.github.token_polls(), 1);
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let (_, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(body["error"], "authorization_pending");
        assert_eq!(ctx.github.token_polls(), 2);
    })
    .await;
}

#[tokio::test]
async fn it_device_flow_slow_down_is_passed_on() {
    run(|ctx| async move {
        let start = start_device_login(&ctx).await;
        ctx.github.slow_down_next(&start.user_code);
        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "slow_down");
        let interval: i32 = sqlx::query_scalar(
            "SELECT interval_secs FROM device_authorizations WHERE user_code = $1",
        )
        .bind(&start.user_code)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(interval, 6);
    })
    .await;
}

#[tokio::test]
async fn it_device_flow_access_denied() {
    run(|ctx| async move {
        let start = start_device_login(&ctx).await;
        ctx.github.deny(&start.user_code);
        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "access_denied");
        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");
    })
    .await;
}

#[tokio::test]
async fn it_device_flow_expired() {
    run(|ctx| async move {
        let start = start_device_login(&ctx).await;
        ctx.github.expire(&start.user_code);
        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "expired_token");
        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");
    })
    .await;
}

#[tokio::test]
async fn it_device_start_github_down_is_502() {
    run(|ctx| async move {
        ctx.github.fail_device_code(503);
        let resp = ctx
            .http
            .post(ctx.hub.url("/v1/auth/device"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "upstream_unavailable");
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM device_authorizations")
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    })
    .await;
}

#[tokio::test]
async fn it_open_device_logins_are_capped() {
    run(|ctx| async move {
        sqlx::query(
            "INSERT INTO device_authorizations
             (device_code_hash, user_code, expires_at, github_device_code, interval_secs, next_poll_at)
             SELECT sha256(n::text::bytea), n::text, now() + interval '15 minutes', n::text, 1, now()
             FROM generate_series(1, 1000) n",
        )
        .execute(&ctx.pool)
        .await
        .unwrap();
        let resp = ctx.http.post(ctx.hub.url("/v1/auth/device")).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "temporarily_unavailable");
        assert!(ctx.github.last_device_form().is_none());
    }).await;
}

#[tokio::test]
async fn it_web_oauth_routes_are_gone() {
    run(|ctx| async move {
        for (path, expected) in [
            ("/v1/auth/device", StatusCode::METHOD_NOT_ALLOWED),
            (
                "/v1/auth/oauth/github/start?user_code=BCDF-GHJK",
                StatusCode::NOT_FOUND,
            ),
            (
                "/v1/auth/oauth/github/callback?state=x",
                StatusCode::NOT_FOUND,
            ),
        ] {
            assert_eq!(
                ctx.http
                    .get(ctx.hub.url(path))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                expected
            );
        }
        assert_eq!(
            ctx.http
                .post(ctx.hub.url("/v1/auth/device/approve"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    })
    .await;
}

#[tokio::test]
async fn it_hub_refuses_github_tokens() {
    run(|ctx| async move {
        let token = format!("gho_{}", "a".repeat(36));
        let resp = ctx
            .http
            .post(ctx.hub.url("/v1/auth/token"))
            .form(&[
                (
                    "grant_type",
                    "urn:ietf:params:oauth:grant-type:token-exchange",
                ),
                ("subject_token", token.as_str()),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "unsupported_grant_type");
        assert_eq!(
            ctx.http
                .get(ctx.hub.url("/v1/me"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    })
    .await;
}

#[tokio::test]
async fn it_device_poll_upstream_failure_keeps_interval() {
    run(|ctx| async move {
        ctx.github.set_interval(2);
        let start = start_device_login(&ctx).await;
        ctx.github.refuse_next_poll("device_flow_disabled");
        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"], "upstream_unavailable");
        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "authorization_pending");
        assert_eq!(ctx.github.token_polls(), 1);
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let (_, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(body["error"], "authorization_pending");
        assert_eq!(ctx.github.token_polls(), 2);
    })
    .await;
}

#[tokio::test]
async fn it_device_start_does_not_lock_out_poll_writes() {
    run(|ctx| async move {
        let start = start_device_login(&ctx).await;
        sqlx::query(
            "UPDATE device_authorizations SET expires_at = now() - interval '1 second'
                     WHERE user_code = $1",
        )
        .bind(&start.user_code)
        .execute(&ctx.pool)
        .await
        .unwrap();
        let mut holder = ctx.pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout = '500ms'")
            .execute(&mut *holder)
            .await
            .unwrap();
        sqlx::query(
            "SELECT device_code_hash FROM device_authorizations WHERE user_code = $1 FOR UPDATE",
        )
        .bind(&start.user_code)
        .execute(&mut *holder)
        .await
        .unwrap();
        let request = ctx
            .http
            .post(ctx.hub.url("/v1/auth/device"))
            .timeout(std::time::Duration::from_secs(5));
        let starting = tokio::spawn(async move { request.send().await });
        let mut waiting = false;
        for _ in 0..100 {
            waiting = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database()
                 AND query LIKE 'DELETE FROM device_authorizations WHERE expires_at%'
                 AND wait_event_type = 'Lock')",
            )
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let deleted = sqlx::query("DELETE FROM device_authorizations WHERE user_code = $1")
            .bind(&start.user_code)
            .execute(&mut *holder)
            .await;
        holder.rollback().await.unwrap();
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), starting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(waiting, "the new start must wait for the held row");
        assert!(
            deleted.is_ok(),
            "a start must not prevent a poll from deleting its row: {deleted:?}"
        );
        assert_eq!(response.status(), StatusCode::OK);
    })
    .await;
}

#[tokio::test]
async fn it_device_upsert_refusal_keeps_interval() {
    run(|ctx| async move {
        ctx.github.set_interval(2);
        let start = start_device_login(&ctx).await;
        ctx.github.approve(&start.user_code, ANDY, "-invalid");
        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "bad_handle");
        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "authorization_pending");
        assert_eq!(ctx.github.token_polls(), 1);
    })
    .await;
}

/// `GET /v1/me` must answer quickly while the hub waits on a slow GitHub:
/// it needs a pool connection, so it fails if sign-in holds them all.
async fn me_answers_fast(ctx: &Ctx, token: &str) {
    let started = std::time::Instant::now();
    let resp = ctx
        .http
        .get(ctx.hub.url("/v1/me"))
        .bearer_auth(token)
        .timeout(std::time::Duration::from_secs(1))
        .send()
        .await
        .expect("GET /v1/me must not wait on sign-in's GitHub calls");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

async fn rows(ctx: &Ctx, table: &'static str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
        .fetch_one(&ctx.pool)
        .await
        .unwrap()
}

/// More sign-ins than the hub's 10 pooled connections.
const MANY: usize = 14;
const SLOW_GITHUB: std::time::Duration = std::time::Duration::from_secs(3);

#[tokio::test]
async fn it_concurrent_device_starts_do_not_starve_the_pool() {
    run(|ctx| async move {
        let token = login(&ctx, ANDY, "Andy").await;
        ctx.github.set_delay(SLOW_GITHUB);
        let starts: Vec<_> = (0..MANY)
            .map(|_| {
                let request = ctx.http.post(ctx.hub.url("/v1/auth/device"));
                tokio::spawn(async move { request.send().await.map(|resp| resp.status()) })
            })
            .collect();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        me_answers_fast(&ctx, &token).await;
        for start in starts {
            let status = tokio::time::timeout(std::time::Duration::from_secs(15), start)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(status, StatusCode::OK);
        }
        assert_eq!(rows(&ctx, "device_authorizations").await, MANY as i64);
    })
    .await;
}

#[tokio::test]
async fn it_concurrent_device_approvals_do_not_starve_the_pool() {
    run(|ctx| async move {
        let token = login(&ctx, ANDY, "Andy").await;
        let mut codes = Vec::new();
        for n in 0..MANY {
            let start = start_device_login(&ctx).await;
            ctx.github
                .approve(&start.user_code, 9000 + n as u64, &format!("person-{n}"));
            codes.push(start.device_code);
        }
        ctx.github.set_delay(SLOW_GITHUB);
        let polls: Vec<_> = codes
            .iter()
            .map(|code| {
                let request = ctx.http.post(ctx.hub.url("/v1/auth/token")).form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("device_code", code.as_str()),
                ]);
                tokio::spawn(async move { request.send().await.map(|resp| resp.status()) })
            })
            .collect();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        me_answers_fast(&ctx, &token).await;
        for poll in polls {
            let status = tokio::time::timeout(std::time::Duration::from_secs(20), poll)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(status, StatusCode::OK);
        }
        assert_eq!(rows(&ctx, "device_authorizations").await, 0);
        assert_eq!(
            rows(&ctx, "humans").await,
            MANY as i64 + 1,
            "one human per approved account"
        );
    })
    .await;
}

#[tokio::test]
async fn it_second_poll_during_a_slow_github_call_answers_pending() {
    run(|ctx| async move {
        let start = start_device_login(&ctx).await;
        ctx.github.set_delay(std::time::Duration::from_secs(2));
        let first = {
            let request = ctx.http.post(ctx.hub.url("/v1/auth/token")).form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", start.device_code.as_str()),
            ]);
            tokio::spawn(async move { request.send().await.map(|resp| resp.status()) })
        };
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let started = std::time::Instant::now();
        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "authorization_pending");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "a second poll must not wait for the first poll's GitHub call"
        );
        assert_eq!(ctx.github.token_polls(), 1);
        assert_eq!(first.await.unwrap().unwrap(), StatusCode::BAD_REQUEST);
    })
    .await;
}
