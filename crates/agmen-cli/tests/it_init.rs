//! Hosted init against a hub and database owned by each test.

#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_hub::{Role, SeededHuman};
use agmen_proto::Timestamp;
use serde_json::{Value, json};
use support::{Ctx, Home, Ingest, Worktree, assert_exit, run, run_hosted, stderr, stdout};

const REQUEST_LIMIT: Duration = Duration::from_secs(10);
const CLI_LIMIT: Duration = Duration::from_secs(45);
const TRIAL_SECONDS: i64 = 7 * 24 * 60 * 60;

async fn signed_in(ctx: &Ctx) -> (Home, SeededHuman) {
    let human = ctx.hub.seed_human(&ctx.topics, Role::Member).await.unwrap();
    let home = Home::new();
    home.sign_in(&ctx.base, &human);
    (home, human)
}

async fn create_group(ctx: &Ctx, human: &SeededHuman, name: &str) -> Value {
    reqwest::Client::builder()
        .timeout(REQUEST_LIMIT)
        .build()
        .unwrap()
        .post(ctx.base.join("v1/groups").unwrap())
        .bearer_auth(&human.access_token)
        .json(&json!({"name": name, "project": "seed"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn me(ctx: &Ctx, human: &SeededHuman) -> Value {
    reqwest::Client::builder()
        .timeout(REQUEST_LIMIT)
        .build()
        .unwrap()
        .get(ctx.base.join("v1/me").unwrap())
        .bearer_auth(&human.access_token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn init(ctx: &Ctx, home: &Home, wt: &Worktree, extra: &[&str]) -> std::process::Output {
    let mut args = vec![
        "init",
        "--hosted",
        "--hub",
        ctx.base.as_str(),
        "--handle",
        "sol",
    ];
    args.extend_from_slice(extra);
    tokio::time::timeout(
        CLI_LIMIT,
        tokio::process::Command::from(home.agmen(&wt.path(), &args))
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap()
}

async fn owned_count(ctx: &Ctx, human: &SeededHuman) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM tenant_members WHERE human_id = $1 AND role = 'owner'")
        .bind(human.id.as_str())
        .fetch_one(&ctx.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn it_init_hosted_first_group() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, human) = signed_in(&ctx).await;
        // No memberships is a new person, so no confirmation is needed.
        sqlx::query("DELETE FROM memberships WHERE human_id = $1")
            .bind(human.id.as_str())
            .execute(&ctx.db)
            .await
            .unwrap();
        sqlx::query("DELETE FROM tenant_members WHERE human_id = $1")
            .bind(human.id.as_str())
            .execute(&ctx.db)
            .await
            .unwrap();
        let wt = Worktree::new(&home);
        let before = Timestamp::now();
        let out = init(&ctx, &home, &wt, &[]).await;
        assert_exit(&out, 0);
        assert!(stdout(&out).contains("/i#"));
        assert!(stdout(&out).contains("trial"));
        assert!(wt.path().join(".agmen/identity.json").is_file());
        let answer = me(&ctx, &human).await;
        let groups = answer["groups"].as_array().unwrap();
        let owned: Vec<_> = groups.iter().filter(|g| g["role"] == "owner").collect();
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0]["state"], "trial");
        let end: Timestamp = owned[0]["trial_ends_at"].as_str().unwrap().parse().unwrap();
        assert!((end.as_second() - before.as_second() - TRIAL_SECONDS).abs() <= 5);
    })
    .await;
}

#[tokio::test]
async fn it_init_hosted_second_group_checkout() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, human) = signed_in(&ctx).await;
        create_group(&ctx, &human, "acme").await;
        let wt = Worktree::new(&home);
        let out = init(&ctx, &home, &wt, &["--new-group"]).await;
        assert_exit(&out, 0);
        assert!(stdout(&out).contains("read-only"));
        assert!(stdout(&out).contains("agmen group billing"));
        assert!(!stdout(&out).contains("/i#"));
        assert!(wt.path().join(".agmen/identity.json").is_file());
    })
    .await;
}

#[tokio::test]
async fn it_init_adds_project_to_own_group() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, human) = signed_in(&ctx).await;
        let group = create_group(&ctx, &human, "acme").await;
        let wt = Worktree::new(&home);
        let out = init(&ctx, &home, &wt, &[]).await;
        assert_exit(&out, 0);
        assert_eq!(owned_count(&ctx, &human).await, 1);
        let projects: i64 =
            sqlx::query_scalar("SELECT count(*) FROM projects WHERE tenant_id = $1")
                .bind(group["tenant"].as_str().unwrap())
                .fetch_one(&ctx.db)
                .await
                .unwrap();
        assert_eq!(projects, 2);
        assert_eq!(
            wt.identity().tenant.as_str(),
            group["tenant"].as_str().unwrap()
        );
        assert!(stdout(&out).contains("trial ends "));
    })
    .await;
}

#[tokio::test]
async fn it_init_several_groups_needs_choice() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, human) = signed_in(&ctx).await;
        let acme = create_group(&ctx, &human, "acme").await;
        let beta = create_group(&ctx, &human, "beta").await;
        let wt = Worktree::new(&home);
        let out = init(&ctx, &home, &wt, &[]).await;
        assert_exit(&out, 2);
        assert!(stderr(&out).contains("name the group: --group <name or id>"));
        assert!(stderr(&out).contains(acme["group"].as_str().unwrap()));
        assert!(stderr(&out).contains(beta["group"].as_str().unwrap()));
        assert!(!wt.path().join(".agmen/identity.json").exists());
        assert_eq!(owned_count(&ctx, &human).await, 2);
    })
    .await;
}

#[tokio::test]
async fn it_init_group_by_name() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, human) = signed_in(&ctx).await;
        create_group(&ctx, &human, "acme").await;
        let beta = create_group(&ctx, &human, "beta").await;
        let tenant = beta["tenant"].as_str().unwrap();
        // A second group awaits payment; make this fixture writable.
        ctx.hub
            .set_plan(
                &tenant.parse().unwrap(),
                &agmen_hub::PlanRecord::trial(Timestamp::now()),
            )
            .await
            .unwrap();
        let name = format!("{}/beta", human.handle);
        for flag in [name.as_str(), tenant] {
            let wt = Worktree::new(&home);
            let out = init(&ctx, &home, &wt, &["--group", flag]).await;
            assert_exit(&out, 0);
            assert_eq!(wt.identity().tenant.as_str(), tenant);
            let project_tenant: String =
                sqlx::query_scalar("SELECT tenant_id FROM projects WHERE id = $1")
                    .bind(wt.identity().project.as_str())
                    .fetch_one(&ctx.db)
                    .await
                    .unwrap();
            assert_eq!(project_tenant, tenant);
        }
    })
    .await;
}

#[tokio::test]
async fn it_init_member_group_refused() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, _) = signed_in(&ctx).await;
        let wt = Worktree::new(&home);
        let out = init(&ctx, &home, &wt, &["--group", ctx.topics.tenant().as_str()]).await;
        assert_exit(&out, 1);
        assert!(stderr(&out).contains("owner or an admin"));
    })
    .await;
}

#[tokio::test]
async fn it_init_hosted_group_limit() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, human) = signed_in(&ctx).await;
        for name in ["acme", "beta", "gamma"] {
            create_group(&ctx, &human, name).await;
        }
        let wt = Worktree::new(&home);
        let out = init(&ctx, &home, &wt, &["--new-group"]).await;
        assert_exit(&out, 1);
        assert!(stderr(&out).contains("3 groups"));
        assert_eq!(owned_count(&ctx, &human).await, 3);
    })
    .await;
}

#[tokio::test]
async fn it_init_on_self_host_hub() {
    run(Ingest::Off, |ctx| async move {
        let (home, _) = signed_in(&ctx).await;
        let wt = Worktree::new(&home);
        let out = init(&ctx, &home, &wt, &["--new-group"]).await;
        assert_exit(&out, 1);
        assert!(stderr(&out).contains("not agmen hosted"));
    })
    .await;
}

#[tokio::test]
async fn it_init_member_without_new_group_requires_confirmation() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, human) = signed_in(&ctx).await;
        let wt = Worktree::new(&home);
        let out = init(&ctx, &home, &wt, &[]).await;
        assert_exit(&out, 2);
        assert!(stderr(&out).contains("--new-group"));
        assert_eq!(owned_count(&ctx, &human).await, 0);
        // The test database is this test's own, so any grant would be init's.
        let trials: i64 = sqlx::query_scalar("SELECT count(*) FROM trial_grants")
            .fetch_one(&ctx.db)
            .await
            .unwrap();
        assert_eq!(trials, 0);
        assert!(!wt.path().join(".agmen/identity.json").exists());
    })
    .await;
}

#[tokio::test]
async fn it_init_taken_folder_group_name_is_usage() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, human) = signed_in(&ctx).await;
        let wt = Worktree::new(&home);
        // Match the documented folder normalization without calling binary internals.
        let folder = wt
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        create_group(&ctx, &human, &folder).await;
        let out = init(&ctx, &home, &wt, &["--new-group"]).await;
        assert_exit(&out, 2);
        assert!(stderr(&out).contains("already taken"));
        assert!(stderr(&out).contains("--group <existing>"));
        assert_eq!(owned_count(&ctx, &human).await, 1);
    })
    .await;
}
