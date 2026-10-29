//! `agmen group seats` and `agmen group billing` against a hub each test owns.
//!
//! The default nextest profile skips this file. Run it with `--profile it`
//! against the dev stack.

#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_hub::{Plan, PlanRecord, PlanState, Role, SeededHuman, TenantRole, seed_tenant_member};
use agmen_proto::{TenantId, Timestamp};
use serde_json::{Value, json};
use support::{Ctx, Home, Ingest, assert_exit, run, run_hosted, run_hosted_paddle, stderr, stdout};

const REQUEST_LIMIT: Duration = Duration::from_secs(10);
const CLI_LIMIT: Duration = Duration::from_secs(45);
const DAY: Duration = Duration::from_secs(86_400);

async fn signed_in(ctx: &Ctx, role: Role) -> (Home, SeededHuman) {
    let human = ctx.hub.seed_human(&ctx.topics, role).await.unwrap();
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

async fn agmen(home: &Home, args: &[&str]) -> std::process::Output {
    tokio::time::timeout(CLI_LIMIT, home.run(home.path(), args))
        .await
        .expect("agmen timed out")
}

fn tenant_of(created: &Value) -> String {
    created["tenant"].as_str().unwrap().to_owned()
}

fn tenant_id(id: &str) -> TenantId {
    id.parse().unwrap()
}

async fn add_member(ctx: &Ctx, tenant: &str, role: TenantRole) -> SeededHuman {
    let human = ctx.hub.seed_human(&ctx.topics, Role::Member).await.unwrap();
    let mut conn = ctx.db.acquire().await.unwrap();
    seed_tenant_member(&mut conn, tenant, human.id.as_str(), role)
        .await
        .unwrap();
    human
}

fn active_team(extra_seats: u32) -> PlanRecord {
    PlanRecord {
        state: PlanState::Active,
        plan: Some(Plan::Team { extra_seats }),
        trial_started_at: None,
        past_due_since: None,
        read_only_cause: None,
        read_only_since: None,
        reminders_sent: 0,
        last_event_at: None,
    }
}

async fn store_paddle_ids(ctx: &Ctx, tenant: &str) {
    // Short stand-in ids. The fake only echoes the customer id into a link.
    sqlx::query(
        "UPDATE tenant_plans SET paddle_customer_id = $1, paddle_subscription_id = $2
         WHERE tenant_id = $3",
    )
    .bind("ctm_1")
    .bind("sub_1")
    .bind(tenant)
    .execute(&ctx.db)
    .await
    .unwrap();
}

#[tokio::test]
async fn it_group_seats_shows_use() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, owner) = signed_in(&ctx, Role::Member).await;
        let created = create_group(&ctx, &owner, "seats").await;
        let tenant = tenant_of(&created);
        add_member(&ctx, &tenant, TenantRole::Member).await;
        let out = agmen(&home, &["group", "seats", "--group", &tenant]).await;
        assert_exit(&out, 0);
        assert_eq!(
            stdout(&out),
            "seats: 2 used of 4 paid (trial, trial)\ntoday: 0 of 5000 messages\n"
        );
    })
    .await;
}

#[tokio::test]
async fn it_group_seats_add_in_trial() {
    run_hosted_paddle(Ingest::Off, |ctx| async move {
        let (home, owner) = signed_in(&ctx, Role::Member).await;
        let created = create_group(&ctx, &owner, "grow").await;
        let tenant = tenant_of(&created);
        let out = agmen(&home, &["group", "seats", "add", "1", "--group", &tenant]).await;
        assert_exit(&out, 0);
        let text = stdout(&out);
        assert!(
            text.contains("Finish checkout to add the seats: https://"),
            "{text}"
        );
    })
    .await;
}

#[tokio::test]
async fn it_group_seats_remove_below_used() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, owner) = signed_in(&ctx, Role::Member).await;
        let created = create_group(&ctx, &owner, "tight").await;
        let tenant = tenant_of(&created);
        add_member(&ctx, &tenant, TenantRole::Member).await;
        let out = agmen(
            &home,
            &["group", "seats", "remove", "3", "--group", &tenant],
        )
        .await;
        assert_exit(&out, 1);
        assert!(
            stderr(&out).contains("remove members first"),
            "{}",
            stderr(&out)
        );
    })
    .await;
}

#[tokio::test]
async fn it_group_seats_add_needs_plan() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, owner) = signed_in(&ctx, Role::Member).await;
        create_group(&ctx, &owner, "trial").await;
        let second = create_group(&ctx, &owner, "later").await;
        let tenant = tenant_of(&second);
        let out = agmen(&home, &["group", "seats", "add", "1", "--group", &tenant]).await;
        assert_exit(&out, 1);
        let err = stderr(&out);
        assert!(err.contains("no seats changed"), "{err}");
        assert!(err.contains("agmen group billing"), "{err}");
        assert!(!stdout(&out).contains("https://"), "{}", stdout(&out));
        assert!(!err.contains("https://"), "{err}");
    })
    .await;
}

#[tokio::test]
async fn it_group_billing_portal() {
    run_hosted_paddle(Ingest::Off, |ctx| async move {
        let (home, owner) = signed_in(&ctx, Role::Member).await;
        let created = create_group(&ctx, &owner, "paid").await;
        let tenant = tenant_of(&created);
        ctx.hub
            .set_plan(&tenant_id(&tenant), &active_team(1))
            .await
            .unwrap();
        store_paddle_ids(&ctx, &tenant).await;
        let out = agmen(&home, &["group", "billing", "--group", &tenant]).await;
        assert_exit(&out, 0);
        let text = stdout(&out);
        assert!(text.contains("billing portal: https://"), "{text}");
        assert!(text.contains("/portal/"), "{text}");
    })
    .await;
}

#[tokio::test]
async fn it_group_billing_read_only_checkout() {
    run_hosted_paddle(Ingest::Off, |ctx| async move {
        let (home, owner) = signed_in(&ctx, Role::Member).await;
        create_group(&ctx, &owner, "first").await;
        let second = create_group(&ctx, &owner, "owed").await;
        let tenant = tenant_of(&second);
        let out = agmen(&home, &["group", "billing", "--group", &tenant]).await;
        assert_exit(&out, 0);
        assert!(
            stdout(&out).contains("finish checkout: https://"),
            "{}",
            stdout(&out)
        );
    })
    .await;
}

#[tokio::test]
async fn it_group_billing_plan_and_seats() {
    run_hosted_paddle(Ingest::Off, |ctx| async move {
        let (home, owner) = signed_in(&ctx, Role::Member).await;
        let created = create_group(&ctx, &owner, "plans").await;
        let tenant = tenant_of(&created);
        // `--seats` reaches the hub: a Business plan for 10 seats starts a checkout.
        let args = [
            "group", "billing", "--plan", "business", "--seats", "10", "--group", &tenant,
        ];
        let out = agmen(&home, &args).await;
        assert_exit(&out, 0);
        assert!(
            stdout(&out).contains("finish checkout: https://"),
            "{}",
            stdout(&out)
        );
        // Past 50 seats the hub names sales, and the CLI prints its sentence.
        let args = [
            "group", "billing", "--plan", "team", "--seats", "51", "--group", &tenant,
        ];
        let out = agmen(&home, &args).await;
        assert_exit(&out, 1);
        assert!(
            stderr(&out).contains("write to sales@getagmen.ai"),
            "{}",
            stderr(&out)
        );
        assert!(!stdout(&out).contains("https://"), "{}", stdout(&out));
    })
    .await;
}

#[tokio::test]
async fn it_group_billing_admin_refused() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (_, owner) = signed_in(&ctx, Role::Member).await;
        let created = create_group(&ctx, &owner, "owned").await;
        let tenant = tenant_of(&created);
        let admin = add_member(&ctx, &tenant, TenantRole::Admin).await;
        let home = Home::new();
        home.sign_in(&ctx.base, &admin);
        let out = agmen(&home, &["group", "billing", "--group", &tenant]).await;
        assert_exit(&out, 1);
        assert!(stderr(&out).contains("owner"), "{}", stderr(&out));
    })
    .await;
}

#[tokio::test]
async fn it_whoami_prints_trial_notice() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, owner) = signed_in(&ctx, Role::Member).await;
        let created = create_group(&ctx, &owner, "notice").await;
        let started = Timestamp::now() - DAY - DAY - DAY - DAY;
        ctx.hub
            .set_plan(
                &tenant_id(&tenant_of(&created)),
                &PlanRecord::trial(started),
            )
            .await
            .unwrap();
        let out = agmen(&home, &["whoami"]).await;
        assert_exit(&out, 0);
        let err = stderr(&out);
        assert!(err.contains("agmen: "), "{err}");
        assert!(err.contains("agmen group billing"), "{err}");
    })
    .await;
}

#[tokio::test]
async fn it_invite_prints_trial_notice() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, owner) = signed_in(&ctx, Role::Member).await;
        let created = create_group(&ctx, &owner, "invite").await;
        let started = Timestamp::now() - DAY - DAY - DAY - DAY;
        ctx.hub
            .set_plan(
                &tenant_id(&tenant_of(&created)),
                &PlanRecord::trial(started),
            )
            .await
            .unwrap();
        let project = created["project"].as_str().unwrap();
        let out = agmen(&home, &["invite", "--project", project, "--role", "member"]).await;
        assert_exit(&out, 0);
        assert!(
            stderr(&out).contains("agmen group billing"),
            "{}",
            stderr(&out)
        );
    })
    .await;
}

#[tokio::test]
async fn it_group_seats_self_host() {
    run(Ingest::Off, |ctx| async move {
        let (home, _) = signed_in(&ctx, Role::Member).await;
        for args in [vec!["group", "seats"], vec!["group", "billing"]] {
            let out = agmen(&home, &args).await;
            assert_exit(&out, 2);
            assert!(
                stderr(&out).contains("self-hosted"),
                "{}: {}",
                args.join(" "),
                stderr(&out)
            );
        }
    })
    .await;
}

#[tokio::test]
async fn it_group_seats_no_plan_row() {
    run_hosted(Ingest::Off, |ctx| async move {
        let (home, _) = signed_in(&ctx, Role::Admin).await;
        // `--plan` must not skip the no-plan answer: it names no seats or
        // billing here either, and an admin sees that instead of an owner refusal.
        for args in [
            vec!["group", "seats"],
            vec!["group", "billing"],
            vec!["group", "billing", "--plan", "team"],
        ] {
            let out = agmen(&home, &args).await;
            assert_exit(&out, 2);
            assert!(
                stderr(&out).contains("this group has no paid plan, so it has no seats or billing"),
                "{}: {}",
                args.join(" "),
                stderr(&out)
            );
        }
    })
    .await;
}
