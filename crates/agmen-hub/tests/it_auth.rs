//! Hub login (RFC 8628 device flow with GitHub sign-in on the approve page)
//! and invites, against an in-process hub, a fake GitHub and a database of
//! the test's own on `AGMEN_IT_PG`.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{
    Ctx, approve_in_browser, count, login, member_with_token, open_confirm_page, poll_device_token,
    post_approve, run, seed_human, seed_member, seed_project, start_device_login,
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
        assert_eq!(start.body["interval"], 5);
        assert_eq!(start.body["expires_in"], 600);
        assert!(
            start.verification_uri_complete.contains(&start.user_code),
            "{}",
            start.verification_uri_complete
        );
        let form = ctx.http.get(&start.verification_uri).send().await.unwrap();
        assert_eq!(form.status(), StatusCode::OK);
        assert!(form.text().await.unwrap().contains("name=\"user_code\""));

        let (status, body) = poll_device_token(&ctx, &start.device_code).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "authorization_pending");

        // GitHub reports a different login case; the seeded human matches by
        // account id, not by login.
        ctx.github.sign_in_as(ANDY, "Andy");
        let page = approve_in_browser(&ctx, &start).await;
        assert!(page.contains(&start.user_code), "confirm page: {page}");

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
async fn it_login_pages_refuse_framing() {
    run(|ctx| async move {
        ctx.github.sign_in_as(ANDY, "andy");
        let start = start_device_login(&ctx).await;
        let form = ctx.http.get(&start.verification_uri).send().await.unwrap();
        assert_unframeable(&form, false);

        let confirm = open_confirm_page(&ctx, &start).await;
        assert_unframeable(&confirm, true);
        let approved = post_approve(&ctx, &confirm.text().await.unwrap()).await;
        assert_eq!(approved.status(), StatusCode::OK);
        assert!(
            approved
                .headers()
                .get("content-type")
                .is_some_and(|value| value.to_str().unwrap().starts_with("text/html")),
            "the approve answer is a page"
        );
        assert_unframeable(&approved, true);

        // The layer is router wide: JSON answers carry the headers too.
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
        ctx.github.sign_in_as(ANDY, "andy");
        let start = start_device_login(&ctx).await;
        approve_in_browser(&ctx, &start).await;
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
        .env("AGMEN_GITHUB_API_URL", ctx.github.base.as_str());
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
