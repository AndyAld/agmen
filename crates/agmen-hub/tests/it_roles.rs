//! Role permissions, references and edit scopes through mint, ingest and HTTP.
#![cfg(test)]
mod support;

use agmen_proto::{DeliveryReply, NackReason};
use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{
    Ctx, Project, envelope, member_with_token, mint, mint_ok, run, seed_project, topics, ulid,
};

#[tokio::test]
async fn it_fix2_paths_scope_respects_other_holders_and_allows_unowned() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let owner = mint_ok(&ctx, &t, "andy/owner", &p).await;
        lanes(&ctx, &p, &t, &owner.id).await;
        let key = item(&ctx, &p, &owner.id).await;
        claim(&ctx, &p, &owner.id, &key).await;
        let worker = role(
            &ctx,
            &p,
            &t,
            "worker",
            "agent",
            "engineer",
            &["DONE"],
            json!({"scope":"paths","globs":["**"]}),
        )
        .await;
        let id = worker["id"].as_str().unwrap();
        let result = checks(&ctx, &p, &t, id, &["docs/a.md", "src/main.rs"]).await;
        assert_eq!(result[0]["allowed"], true, "unowned path remains editable");
        assert_eq!(
            result[1]["allowed"], false,
            "paths must not bypass another holder"
        );
        assert_eq!(result[1]["reason"], "lane L-UI held by andy/owner");
        ack(post(
            &ctx,
            &p,
            id,
            "DONE",
            json!({"item":"B-999","branch":"fix/scope",
            "commit":"a".repeat(40),"base_sha":"b".repeat(40),"checks":[],
            "changes":[{"status":"M","path":"src/main.rs"}]}),
            json!({}),
        )
        .await);
        let alerts: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM messages WHERE project_id=$1 AND envelope->>'verb'='ALERT'",
        )
        .bind(&p.project)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(alerts, 1);
    })
    .await;
}

#[tokio::test]
async fn it_fix2_lend_stops_after_granter_revocation() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let owner = mint_ok(&ctx, &t, "andy/owner", &p).await;
        let recipient = mint_ok(&ctx, &t, "andy/recipient", &p).await;
        let coordinator = support::mint_kind(&ctx, &t, "andy/coord", &p, "coordinator").await;
        lanes(&ctx, &p, &t, &owner.id).await;
        for granter in [&owner.id, &coordinator.id] {
            ack(post(&ctx, &p, granter, "LEND", json!({"lane":"L-UI","paths":["src/tests/**"],
                "to":recipient.id,"until":(jiff::Timestamp::now()+std::time::Duration::from_secs(3600)).to_string()}), json!({})).await);
            assert_eq!(checks(&ctx, &p, &t, &recipient.id, &["src/tests/a.rs"]).await[0]["allowed"], true);
            sqlx::query("UPDATE identities SET revoked_at=now() WHERE id=$1")
                .bind(granter).execute(&ctx.pool).await.unwrap();
            assert_eq!(checks(&ctx, &p, &t, &recipient.id, &["src/tests/a.rs"]).await[0]["allowed"], false,
                "revoked granter {granter} must not authorize a lend");
        }
    }).await;
}

#[tokio::test]
async fn it_fix2_lend_stops_after_ownership_moves() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let owner = mint_ok(&ctx, &t, "andy/owner", &p).await;
        let recipient = mint_ok(&ctx, &t, "andy/recipient", &p).await;
        let replacement = mint_ok(&ctx, &t, "andy/replacement", &p).await;
        let coordinator = support::mint_kind(&ctx, &t, "andy/coord", &p, "coordinator").await;
        lanes(&ctx, &p, &t, &owner.id).await;
        ack(post(&ctx, &p, &owner.id, "LEND", json!({"lane":"L-UI","paths":["src/tests/**"],
            "to":recipient.id,"until":(jiff::Timestamp::now()+std::time::Duration::from_secs(3600)).to_string()}), json!({})).await);
        ack(post(&ctx, &p, &coordinator.id, "LEND", json!({"lane":"L-UI","paths":["src/coord/**"],
            "to":recipient.id,"until":(jiff::Timestamp::now()+std::time::Duration::from_secs(3600)).to_string()}), json!({})).await);
        assert_eq!(checks(&ctx, &p, &t, &recipient.id, &["src/tests/a.rs"]).await[0]["allowed"], true);
        // Change ownership without replacing lanes, which deletes every grant.
        sqlx::query("UPDATE lanes SET owner_identity_id=$1 WHERE project_id=$2 AND name='L-UI'")
            .bind(&replacement.id).bind(&p.project).execute(&ctx.pool).await.unwrap();
        let result = checks(&ctx, &p, &t, &recipient.id, &["src/coord/a.rs", "src/tests/a.rs"]).await;
        assert_eq!(result[0]["allowed"], true, "live coordinator can lend any lane");
        assert_eq!(result[1]["allowed"], false, "former owner must not authorize a lend");
    }).await;
}

#[tokio::test]
async fn it_fix2_lend_refuses_until_past_seven_days() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let owner = mint_ok(&ctx, &t, "andy/owner", &p).await;
        let recipient = mint_ok(&ctx, &t, "andy/recipient", &p).await;
        lanes(&ctx, &p, &t, &owner.id).await;
        let data = |seconds| {
            json!({"lane":"L-UI","paths":["src/**"],"to":recipient.id,
            "until":(jiff::Timestamp::now()+std::time::Duration::from_secs(seconds)).to_string()})
        };
        ack(post(&ctx, &p, &owner.id, "LEND", data(7 * 24 * 3600), json!({})).await);
        nack(
            post(
                &ctx,
                &p,
                &owner.id,
                "LEND",
                data(7 * 24 * 3600 + 60),
                json!({}),
            )
            .await,
            NackReason::Schema,
        );
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lane_lends WHERE project_id=$1")
            .bind(&p.project)
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    })
    .await;
}

#[tokio::test]
async fn it_fix1_lend_cap_serializes_concurrent_grants() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let a = mint_ok(&ctx, &t, "andy/owner", &p).await;
        let b = mint_ok(&ctx, &t, "andy/other", &p).await;
        lanes(&ctx, &p, &t, &a.id).await;
        let data = json!({"lane":"L-UI","paths":["src/**"],"to":b.id,
            "until":(jiff::Timestamp::now()+std::time::Duration::from_secs(3600)).to_string()});
        for _ in 0..15 {
            ack(post(&ctx, &p, &a.id, "LEND", data.clone(), json!({})).await);
        }
        let (one, two) = tokio::join!(
            post(&ctx, &p, &a.id, "LEND", data.clone(), json!({})),
            post(&ctx, &p, &a.id, "LEND", data.clone(), json!({}))
        );
        let replies = [one, two];
        assert_eq!(
            replies
                .iter()
                .filter(|r| matches!(r, DeliveryReply::Ack { .. }))
                .count(),
            1
        );
        nack(
            replies
                .into_iter()
                .find(|r| matches!(r, DeliveryReply::Nack { .. }))
                .unwrap(),
            NackReason::Schema,
        );
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM lane_lends WHERE project_id=$1")
            .bind(&p.project)
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        assert_eq!(count, 16);
        sqlx::query("UPDATE lane_lends SET until_at=now()-interval '1 second' WHERE project_id=$1")
            .bind(&p.project)
            .execute(&ctx.pool)
            .await
            .unwrap();
        ack(post(&ctx, &p, &a.id, "LEND", data, json!({})).await);
    })
    .await;
}

#[tokio::test]
async fn it_fix1_delegation_waits_for_listener_revocation() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let listener = support::mint_kind(&ctx, &t, "andy/listen", &p, "listener").await;
        let mut tx = ctx.pool.begin().await.unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *tx).await.unwrap();
        sqlx::query("UPDATE identities SET revoked_at=now() WHERE id=$1")
            .bind(&listener.id).execute(&mut *tx).await.unwrap();
        let request = ctx.http.post(ctx.hub.url("/v1/identities")).bearer_auth(&t)
            .json(&json!({"handle":"andy/worker","project":p.project,"delegated_by":listener.id}));
        let task = tokio::spawn(async move { request.send().await.unwrap() });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))")
                    .bind(pid).fetch_one(&ctx.pool).await.unwrap();
                if blocked || task.is_finished() { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(task.await.unwrap().status(), StatusCode::UNPROCESSABLE_ENTITY);
        let workers: i64 = sqlx::query_scalar("SELECT count(*) FROM identities WHERE delegated_by=$1")
            .bind(&listener.id).fetch_one(&ctx.pool).await.unwrap();
        assert_eq!(workers, 0);
    }).await;
}

#[tokio::test]
async fn it_fix1_refs_bound_with_many_project_items() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let e = mint_ok(&ctx, &t, "andy/sol", &p).await;
        sqlx::query("INSERT INTO items (project_id,key,title,created_by) SELECT $1,'B-' || n,'work',$2 FROM generate_series(1,10000) n")
            .bind(&p.project).bind(&e.id).execute(&ctx.pool).await.unwrap();
        for body in ["#1 ".repeat(10_922), "B-10000 ".repeat(4096), "B-10000 B-999999".into()] {
            let id = ulid();
            ack(ctx.hub.hub.ingest(&topics(&p), &e.id.parse().unwrap(),
                &envelope(&id, "FYI", json!({}), json!({"body":body}))).await.unwrap());
            let stored: Value = sqlx::query_scalar("SELECT envelope FROM messages WHERE project_id=$1 AND id=$2 LIMIT 1")
                .bind(&p.project).bind(&id).fetch_one(&ctx.pool).await.unwrap();
            let refs = stored["refs"].as_array().unwrap();
            assert!(refs.len() <= 128, "{} refs", refs.len());
            assert!(serde_json::to_vec(&stored).unwrap().len() <= 80 * 1024);
            if body.ends_with("B-999999") {
                assert_eq!(refs.len(), 1);
                assert_eq!(refs[0]["target"], "B-10000");
            }
        }
    }).await;
}

async fn room(ctx: &Ctx) -> (Project, String) {
    let project = seed_project(&ctx.pool).await;
    let token = member_with_token(ctx, &project, 4242, "andy", "admin").await;
    (project, token)
}

async fn seed_ref_recipients(ctx: &Ctx, p: &Project, from: &str, count: i32) {
    sqlx::query("INSERT INTO identities (id,tenant_id,project_id,kind,handle,owner_human_id)
        SELECT substr(md5(n::text),1,16),tenant_id,project_id,'agent','andy/bulk' || n,owner_human_id
        FROM identities CROSS JOIN generate_series(1,$2) n WHERE id=$1")
        .bind(from).bind(count).execute(&ctx.pool).await.unwrap();
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM identities WHERE project_id=$1")
        .bind(&p.project)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
    assert_eq!(stored, i64::from(count) + 1);
}

#[tokio::test]
async fn it_fix1_envelope_budget_drops_refs_first() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let e = mint_ok(&ctx, &t, "andy/sol", &p).await;
        seed_ref_recipients(&ctx, &p, &e.id, 800).await;
        let human: String = sqlx::query_scalar("SELECT owner_human_id FROM identities WHERE id=$1")
            .bind(&e.id)
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        let id = ulid();
        let body = format!("{}{}", "#1 ".repeat(128), "\n".repeat(32_000));
        let raw = envelope(
            &id,
            "FYI",
            json!({}),
            json!({"body":body,"mentions":["all"]}),
        );
        assert!(raw.len() <= 65_536);
        ack(ctx
            .hub
            .hub
            .ingest(&topics(&p), &human.parse().unwrap(), &raw)
            .await
            .unwrap());
        let stored: Value = sqlx::query_scalar(
            "SELECT envelope FROM messages WHERE project_id=$1 AND id=$2 LIMIT 1",
        )
        .bind(&p.project)
        .bind(&id)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(stored["refs"], json!([]));
        assert_eq!(stored["mentioned"].as_array().unwrap().len(), 801);
        assert!(serde_json::to_vec(&stored).unwrap().len() <= 80 * 1024);
        let max: i32 = sqlx::query_scalar(
            "SELECT max(octet_length(payload)) FROM publish_outbox WHERE project_id=$1",
        )
        .bind(&p.project)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert!(max <= 80 * 1024);
    })
    .await;
}

#[tokio::test]
async fn it_fix1_envelope_budget_refuses_without_storing() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let e = mint_ok(&ctx, &t, "andy/sol", &p).await;
        seed_ref_recipients(&ctx, &p, &e.id, 1500).await;
        let human: String = sqlx::query_scalar("SELECT owner_human_id FROM identities WHERE id=$1")
            .bind(&e.id)
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        let id = ulid();
        let raw = envelope(
            &id,
            "FYI",
            json!({}),
            json!({"body":"\n".repeat(32_000),"mentions":["all"]}),
        );
        nack(
            ctx.hub
                .hub
                .ingest(&topics(&p), &human.parse().unwrap(), &raw)
                .await
                .unwrap(),
            NackReason::Schema,
        );
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM messages WHERE project_id=$1 AND id=$2")
                .bind(&p.project)
                .bind(&id)
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM publish_outbox WHERE project_id=$1")
                .bind(&p.project)
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
    })
    .await;
}

#[expect(
    clippy::too_many_arguments,
    reason = "mint fixtures spell out each permission boundary"
)]
async fn role(
    ctx: &Ctx,
    p: &Project,
    token: &str,
    name: &str,
    kind: &str,
    role: &str,
    verbs: &[&str],
    edits: Value,
) -> Value {
    let (status, body) = mint(
        ctx,
        token,
        &json!({"handle": format!("andy/{name}"), "project": p.project,
        "kind": kind, "role": role, "verbs": verbs, "edits": edits,
        "role_sha256": "a".repeat(64), "agent": "codex"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

async fn post(
    ctx: &Ctx,
    p: &Project,
    from: &str,
    verb: &str,
    data: Value,
    extra: Value,
) -> DeliveryReply {
    ctx.hub
        .hub
        .ingest(
            &topics(p),
            &from.parse().unwrap(),
            &envelope(&ulid(), verb, data, extra),
        )
        .await
        .unwrap()
}
fn ack(reply: DeliveryReply) {
    assert!(matches!(reply, DeliveryReply::Ack { .. }), "{reply:?}");
}
fn nack(reply: DeliveryReply, reason: NackReason) {
    assert!(
        matches!(reply, DeliveryReply::Nack { reason: r, retryable: false, .. } if r == reason),
        "{reply:?}"
    );
}
async fn roster(ctx: &Ctx, p: &Project, token: &str) -> Value {
    ctx.http
        .get(
            ctx.hub
                .url(&format!("/v1/projects/{}/state?key=roster", p.project)),
        )
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["value"]
        .clone()
}
async fn lanes(ctx: &Ctx, p: &Project, token: &str, owner: &str) {
    let response = ctx
        .http
        .put(ctx.hub.url(&format!("/v1/projects/{}/lanes", p.project)))
        .bearer_auth(token)
        .json(&json!({"lanes":[{"name":"L-UI", "globs":["src/**"],"owner":owner}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
}
async fn checks(ctx: &Ctx, p: &Project, token: &str, id: &str, paths: &[&str]) -> Value {
    ctx.http
        .post(
            ctx.hub
                .url(&format!("/v1/projects/{}/check-path", p.project)),
        )
        .bearer_auth(token)
        .json(&json!({"identity":id,"paths":paths}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}
async fn item(ctx: &Ctx, p: &Project, from: &str) -> String {
    ack(post(
        ctx,
        p,
        from,
        "FOUND",
        json!({"title":"work","lane":"L-UI"}),
        json!({}),
    )
    .await);
    sqlx::query_scalar("SELECT key FROM items WHERE project_id = $1 ORDER BY key DESC LIMIT 1")
        .bind(&p.project)
        .fetch_one(&ctx.pool)
        .await
        .unwrap()
}
async fn claim(ctx: &Ctx, p: &Project, from: &str, item: &str) {
    ack(post(ctx,p,from,"CLAIM",json!({"item":item}),json!({"valid_until":(jiff::Timestamp::now()+std::time::Duration::from_secs(60)).to_string()})).await);
}

#[tokio::test]
async fn it_mint_stores_role() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let r = role(
            &ctx,
            &p,
            &t,
            "sol",
            "agent",
            "engineer",
            &["HELLO", "CLAIM", "DONE", "LEND"],
            json!({"scope":"lanes"}),
        )
        .await;
        ack(post(
            &ctx,
            &p,
            r["id"].as_str().unwrap(),
            "HELLO",
            json!({"client":"test","agent":"other"}),
            json!({}),
        )
        .await);
        let rows = roster(&ctx, &p, &t).await;
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["identity_id"] == r["id"])
            .unwrap();
        assert_eq!(row["role"], "engineer");
        assert_eq!(row["agent"], "codex");
        let stored: (String, String, String) =
            sqlx::query_as("SELECT role, role_sha256, edits->>'scope' FROM identities WHERE id=$1")
                .bind(r["id"].as_str().unwrap())
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
        assert_eq!(stored, ("engineer".into(), "a".repeat(64), "lanes".into()));
    })
    .await;
}

#[tokio::test]
async fn it_mint_caps_verbs_at_kind() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let r = role(
            &ctx,
            &p,
            &t,
            "sol",
            "agent",
            "engineer",
            &["FYI", "RULING", "ASSIGN", "ROLLCALL", "STATE", "unknown"],
            json!({"scope":"none"}),
        )
        .await;
        assert_eq!(r["verbs"], json!(["FYI"]));
        nack(
            post(
                &ctx,
                &p,
                r["id"].as_str().unwrap(),
                "RULING",
                json!({"subject":"x"}),
                json!({}),
            )
            .await,
            NackReason::ForbiddenVerb,
        );
    })
    .await;
}

#[tokio::test]
async fn it_one_listener_per_person_project() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let r = role(
            &ctx,
            &p,
            &t,
            "listen",
            "listener",
            "listener",
            &["FYI", "DELEGATED", "CLAIM", "DONE", "LEND", "ASSIGN"],
            json!({"scope":"none"}),
        )
        .await;
        assert_eq!(r["verbs"], json!(["DELEGATED", "FYI"]));
        let (status, _) = mint(
            &ctx,
            &t,
            &json!({"handle":"andy/listen2","project":p.project,"kind":"listener"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        sqlx::query("UPDATE identities SET revoked_at=now() WHERE id=$1")
            .bind(r["id"].as_str().unwrap())
            .execute(&ctx.pool)
            .await
            .unwrap();
        role(
            &ctx,
            &p,
            &t,
            "listen2",
            "listener",
            "listener",
            &["FYI"],
            json!({"scope":"none"}),
        )
        .await;
    })
    .await;
}

#[tokio::test]
async fn it_role_verbs_enforced_on_ingest() {
    run(|ctx| async move {
    let (p,t)=room(&ctx).await;
    let r=role(&ctx,&p,&t,"res","agent","researcher", &["ANSWER"],json!({"scope":"none"})).await;
    let id=r["id"].as_str().unwrap();
    nack(post(&ctx,&p,id,"CLAIM",json!({"item":"B-1"}),json!({"valid_until":(jiff::Timestamp::now()+std::time::Duration::from_secs(60)).to_string()})).await,NackReason::ForbiddenVerb);
    ack(post(&ctx,&p,id,"ANSWER",json!({}),json!({"reply_to":ulid()})).await);
    let legacy=mint_ok(&ctx,&t,"andy/legacy",&p).await;
    ack(post(&ctx,&p,&legacy.id,"FYI",json!({}),json!({})).await);
    nack(post(&ctx,&p,&legacy.id,"PLAN",json!({"version":1,"path":"docs/plan.md","sha":"a".repeat(40)}),json!({})).await,NackReason::ForbiddenVerb);
    let planner=role(&ctx,&p,&t,"plan","agent","planner", &["PLAN"],json!({"scope":"paths","globs":["docs/**"]})).await;
    ack(post(&ctx,&p,planner["id"].as_str().unwrap(),"PLAN",json!({"version":1,"path":"docs/plan.md","sha":"a".repeat(40)}),json!({})).await);
}).await;
}

#[tokio::test]
async fn it_lend_only_from_owner() {
    run(|ctx| async move {
    let (p,t)=room(&ctx).await;
    let a=mint_ok(&ctx,&t,"andy/owner",&p).await; let b=mint_ok(&ctx,&t,"andy/other",&p).await;
    lanes(&ctx,&p,&t,&a.id).await;
    let data=json!({"lane":"L-UI","paths":["src/tests/**"],"to":b.id,"until":(jiff::Timestamp::now()+std::time::Duration::from_secs(60)).to_string()});
    nack(post(&ctx,&p,&b.id,"LEND",data.clone(),json!({})).await,NackReason::NotLaneOwner);
    let human=support::seed_human(&ctx.pool,5151,"adam").await;
    support::seed_member(&ctx.pool,&human,&p,"coordinator").await;
    nack(post(&ctx,&p,&human,"LEND",data.clone(),json!({})).await,NackReason::NotLaneOwner);
    ack(post(&ctx,&p,&a.id,"LEND",data.clone(),json!({})).await);
    let coord=support::mint_kind(&ctx,&t,"andy/coord",&p,"coordinator").await;
    ack(post(&ctx,&p,&coord.id,"LEND",data,json!({})).await);
    let result=checks(&ctx,&p,&t,&b.id,&["src/tests/a.rs","src/main.rs"]).await;
    assert_eq!(result[0]["allowed"],true); assert_eq!(result[1]["allowed"],false);
    sqlx::query("UPDATE lane_lends SET until_at=now()-interval '1 second' WHERE project_id=$1").bind(&p.project).execute(&ctx.pool).await.unwrap();
    assert_eq!(checks(&ctx,&p,&t,&b.id,&["src/tests/a.rs"]).await[0]["allowed"],false);
}).await;
}

#[tokio::test]
async fn it_review_mentions_done_author() {
    run(|ctx| async move {
    let (p,t)=room(&ctx).await; let e=mint_ok(&ctx,&t,"andy/sol",&p).await;
    let r=role(&ctx,&p,&t,"review","agent","reviewer", &["REVIEW"],json!({"scope":"none"})).await;
    let done=ulid();
    let raw=envelope(&done,"DONE",json!({"item":"B-1","branch":"fix/b-1","commit":"a".repeat(40),"base_sha":"b".repeat(40),"checks":[],"changes":[]}),json!({}));
    ack(ctx.hub.hub.ingest(&topics(&p),&e.id.parse().unwrap(),&raw).await.unwrap());
    ack(post(&ctx,&p,r["id"].as_str().unwrap(),"REVIEW",json!({"verdict":"pass","findings":[]}),json!({"reply_to":done})).await);
    let mentioned: Value=sqlx::query_scalar("SELECT envelope->'mentioned' FROM messages WHERE project_id=$1 AND envelope->>'verb'='REVIEW'").bind(&p.project).fetch_one(&ctx.pool).await.unwrap();
    assert_eq!(mentioned,json!([e.id]));
    let fyi=ulid(); ack(ctx.hub.hub.ingest(&topics(&p),&e.id.parse().unwrap(),&envelope(&fyi,"FYI",json!({}),json!({}))).await.unwrap());
    for parent in [fyi,ulid()] { nack(post(&ctx,&p,r["id"].as_str().unwrap(),"REVIEW",json!({"verdict":"pass","findings":[]}),json!({"reply_to":parent})).await,NackReason::Schema); }
}).await;
}

#[tokio::test]
async fn it_role_mention_expands() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let a = role(
            &ctx,
            &p,
            &t,
            "r1",
            "agent",
            "reviewer",
            &["FYI"],
            json!({"scope":"none"}),
        )
        .await;
        let b = role(
            &ctx,
            &p,
            &t,
            "r2",
            "agent",
            "reviewer",
            &["FYI"],
            json!({"scope":"none"}),
        )
        .await;
        let dead = role(
            &ctx,
            &p,
            &t,
            "old",
            "agent",
            "reviewer",
            &["FYI"],
            json!({"scope":"none"}),
        )
        .await;
        sqlx::query("UPDATE identities SET revoked_at=now() WHERE id=$1")
            .bind(dead["id"].as_str().unwrap())
            .execute(&ctx.pool)
            .await
            .unwrap();
        let human: String = sqlx::query_scalar("SELECT id FROM humans WHERE handle='andy'")
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
        ack(post(
            &ctx,
            &p,
            &human,
            "FYI",
            json!({}),
            json!({"mentions":["role:reviewer"]}),
        )
        .await);
        let got: Value = sqlx::query_scalar(
            "SELECT envelope->'mentioned' FROM messages WHERE project_id=$1 AND from_id=$2",
        )
        .bind(&p.project)
        .bind(&human)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        let mut expected = vec![a["id"].as_str().unwrap(), b["id"].as_str().unwrap()];
        expected.sort_unstable();
        assert_eq!(got, json!(expected));
        nack(
            post(
                &ctx,
                &p,
                a["id"].as_str().unwrap(),
                "FYI",
                json!({}),
                json!({"mentions":["role:reviewer"]}),
            )
            .await,
            NackReason::ForbiddenMention,
        );
    })
    .await;
}

#[tokio::test]
async fn it_refs_stamped() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let e = mint_ok(&ctx, &t, "andy/sol", &p).await;
        lanes(&ctx, &p, &t, &e.id).await;
        let key = item(&ctx, &p, &e.id).await;
        let r = role(
            &ctx,
            &p,
            &t,
            "review",
            "agent",
            "reviewer",
            &["FYI"],
            json!({"scope":"none"}),
        )
        .await;
        let body = format!("é andy/sol broke L-UI in {key}, see #10 @reviewers L-NOPE B-999");
        ack(post(
            &ctx,
            &p,
            r["id"].as_str().unwrap(),
            "FYI",
            json!({}),
            json!({"body":body}),
        )
        .await);
        let refs: Value = sqlx::query_scalar(
            "SELECT envelope->'refs' FROM messages WHERE project_id=$1 AND envelope->>'verb'='FYI'",
        )
        .bind(&p.project)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        let expected = [
            ("identity", "andy/sol", e.id.as_str()),
            ("lane", "L-UI", "L-UI"),
            ("item", key.as_str(), key.as_str()),
            ("pr", "#10", "10"),
            ("role", "@reviewers", "reviewer"),
        ];
        assert_eq!(refs.as_array().unwrap().len(), expected.len());
        for (got, (kind, text, target)) in refs.as_array().unwrap().iter().zip(expected) {
            let start = body.find(text).unwrap();
            assert_eq!(
                *got,
                json!({"kind":kind,"start":start,"end":start+text.len(),"target":target})
            );
        }
    })
    .await;
}

#[tokio::test]
async fn it_edit_scopes_and_done_enforce_both_lane_and_globs() {
    run(|ctx| async move {
    let (p,t)=room(&ctx).await;
    let r=role(&ctx,&p,&t,"tester","agent","tester", &["FOUND","CLAIM","DONE"],json!({"scope":"lanes_within","globs":["src/tests/**"]})).await;
    let id=r["id"].as_str().unwrap(); lanes(&ctx,&p,&t,id).await;
    assert_eq!(checks(&ctx,&p,&t,id,&["src/tests/a.rs"]).await[0]["allowed"],false);
    let key=item(&ctx,&p,id).await; claim(&ctx,&p,id,&key).await;
    let result=checks(&ctx,&p,&t,id,&["src/tests/a.rs","src/main.rs","docs/a.md"]).await;
    assert_eq!(result[0]["allowed"],true); assert_eq!(result[1]["allowed"],false); assert_eq!(result[2]["allowed"],false);
    ack(post(&ctx,&p,id,"DONE",json!({"item":key,"branch":"fix/tests","commit":"a".repeat(40),"base_sha":"b".repeat(40),"checks":[],"changes":[{"status":"M","path":"src/main.rs"}]}),json!({})).await);
    let alerts: i64=sqlx::query_scalar("SELECT count(*) FROM messages WHERE project_id=$1 AND envelope->>'verb'='ALERT'").bind(&p.project).fetch_one(&ctx.pool).await.unwrap(); assert!(alerts>0);
    let none=role(&ctx,&p,&t,"res","agent","researcher", &["FYI"],json!({"scope":"none"})).await;
    assert_eq!(checks(&ctx,&p,&t,none["id"].as_str().unwrap(),&["docs/a.md"]).await[0]["allowed"],false);
    let paths=role(&ctx,&p,&t,"plan","agent","planner", &["PLAN"],json!({"scope":"paths","globs":["docs/**"]})).await;
    let result=checks(&ctx,&p,&t,paths["id"].as_str().unwrap(),&["docs/a.md","src/main.rs","../docs/a.md"]).await;
    assert_eq!(result[0]["allowed"],true); assert_eq!(result[1]["allowed"],false); assert_eq!(result[2]["allowed"],false);
}).await;
}

#[tokio::test]
async fn it_delegated_by_validated_and_rostered() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let listener = role(
            &ctx,
            &p,
            &t,
            "listen",
            "listener",
            "listener",
            &["DELEGATED"],
            json!({"scope":"none"}),
        )
        .await;
        let (status, worker) = mint(
            &ctx,
            &t,
            &json!({"handle":"andy/worker","project":p.project,"delegated_by":listener["id"]}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let rows = roster(&ctx, &p, &t).await;
        assert!(
            rows.as_array()
                .unwrap()
                .iter()
                .any(|row| row["identity_id"] == worker["id"]
                    && row["delegated_by"] == listener["id"])
        );
        let (status, _) = mint(
            &ctx,
            &t,
            &json!({"handle":"andy/bad","project":p.project,"delegated_by":worker["id"]}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    })
    .await;
}

#[tokio::test]
async fn it_mint_rejects_partial_roles_and_invalid_globs() {
    run(|ctx| async move {
    let (p,t)=room(&ctx).await;
    for extra in [json!({"role":"engineer"}),json!({"verbs":["FYI"]}),json!({"edits":{"scope":"none"}}),
        json!({"role":"engineer","verbs":["FYI"],"role_sha256":"a".repeat(64),"edits":{"scope":"paths","globs":["!"]}})] {
        let mut body=json!({"handle":"andy/bad","project":p.project});
        for (key,value) in extra.as_object().unwrap() { body[key]=value.clone(); }
        let (status,_) = mint(&ctx,&t,&body).await;
        assert!(status.is_client_error());
    }
}).await;
}

#[tokio::test]
async fn it_coordinator_listener_assign_cap_and_delegated_worker_check() {
    run(|ctx| async move {
    let (p,t)=room(&ctx).await;
    let listener=role(&ctx,&p,&t,"listen","listener","coordinator", &["ASSIGN","DELEGATED","CLAIM","DONE","LEND","RULING","PUSH_OK"],json!({"scope":"none"})).await;
    assert_eq!(listener["verbs"],json!(["ASSIGN","DELEGATED"]));
    let other=mint_ok(&ctx,&t,"andy/other",&p).await;
    let (status,worker)=mint(&ctx,&t,&json!({"handle":"andy/worker","project":p.project,"kind":"agent","delegated_by":listener["id"]})).await;
    assert_eq!(status,StatusCode::CREATED);
    let data=|id: &Value| json!({"worker":id,"agent":"codex","role":"engineer","model":"test","tier":"standard","budget_usd":1.0});
    ack(post(&ctx,&p,listener["id"].as_str().unwrap(),"DELEGATED",data(&worker["id"]),json!({"reply_to":ulid()})).await);
    nack(post(&ctx,&p,listener["id"].as_str().unwrap(),"DELEGATED",data(&json!(other.id)),json!({"reply_to":ulid()})).await,NackReason::Schema);
    let outsider=member_with_token(&ctx,&p,5151,"adam","member").await;
    let (status,_)=mint(&ctx,&outsider,&json!({"handle":"adam/worker","project":p.project,"delegated_by":listener["id"]})).await;
    assert_eq!(status,StatusCode::UNPROCESSABLE_ENTITY);
}).await;
}

#[tokio::test]
async fn it_refs_do_not_match_unknown_names_or_substrings() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let e = mint_ok(&ctx, &t, "andy/sol", &p).await;
        lanes(&ctx, &p, &t, &e.id).await;
        ack(post(
            &ctx,
            &p,
            &e.id,
            "FYI",
            json!({}),
            json!({"body":"xandy/sol andy/solar L-UIX B-999 @unknowns x#12 #12x"}),
        )
        .await);
        let refs: Value = sqlx::query_scalar(
            "SELECT envelope->'refs' FROM messages WHERE project_id=$1 AND envelope->>'verb'='FYI'",
        )
        .bind(&p.project)
        .fetch_one(&ctx.pool)
        .await
        .unwrap();
        assert_eq!(refs, json!([]));
    })
    .await;
}

#[tokio::test]
async fn it_hub_alert_refs_are_stamped() {
    run(|ctx| async move {
        let (p,t)=room(&ctx).await;
        let e=mint_ok(&ctx,&t,"andy/sol",&p).await;
        lanes(&ctx,&p,&t,&e.id).await;
        let key=item(&ctx,&p,&e.id).await;
        ctx.hub.hub.post_as_hub(&topics(&p), agmen_hub::HubPost {
            payload: agmen_proto::Payload::Alert { need: "check".into(), urgency: agmen_proto::Urgency::Normal, needs: agmen_proto::Needs::Anyone, actions: vec![] },
            body: format!("andy/sol {key} L-UI #10").try_into().unwrap(), reply_to:None, items:vec![], lane:None, mentioned:vec![], streams:vec![agmen_proto::Stream::Feed],
        }).await.unwrap();
        let refs: Value=sqlx::query_scalar("SELECT envelope->'refs' FROM messages WHERE project_id=$1 AND envelope->>'verb'='ALERT'").bind(&p.project).fetch_one(&ctx.pool).await.unwrap();
        assert_eq!(refs.as_array().unwrap().len(),4);
    }).await;
}

#[tokio::test]
async fn it_listener_legacy_mint_still_cannot_edit() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let listener = support::mint_kind(&ctx, &t, "andy/listen", &p, "listener").await;
        let result = checks(&ctx, &p, &t, &listener.id, &["docs/a.md"]).await;
        assert_eq!(result[0]["allowed"], false);
    })
    .await;
}

#[tokio::test]
async fn it_coordinator_listener_requires_coordinator_membership() {
    run(|ctx| async move {
        let (p,_)=room(&ctx).await;
        let token=member_with_token(&ctx,&p,5151,"adam","member").await;
        let (status,body)=mint(&ctx,&token,&json!({"handle":"adam/listen","project":p.project,"kind":"listener","role":"coordinator","verbs":["ASSIGN"],"edits":{"scope":"none"},"role_sha256":"a".repeat(64)})).await;
        assert_eq!(status,StatusCode::FORBIDDEN);
        assert_eq!(body["error"],"coordinator_role_required");
    }).await;
}

#[tokio::test]
async fn it_refs_include_at_prefixed_handles() {
    run(|ctx| async move {
        let (p, t) = room(&ctx).await;
        let e = mint_ok(&ctx, &t, "andy/sol", &p).await;
        ack(post(
            &ctx,
            &p,
            &e.id,
            "FYI",
            json!({}),
            json!({"body":"@andy/sol @andy"}),
        )
        .await);
        let refs: Value =
            sqlx::query_scalar("SELECT envelope->'refs' FROM messages WHERE project_id=$1")
                .bind(&p.project)
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
        assert_eq!(refs.as_array().unwrap().len(), 2);
        assert_eq!(
            refs[0],
            json!({"kind":"identity","start":0,"end":9,"target":e.id})
        );
        assert_eq!(refs[1]["start"], 10);
        assert_eq!(refs[1]["end"], 15);
    })
    .await;
}

#[tokio::test]
async fn it_done_scope_violation_reports_valid_path() {
    run(|ctx| async move {
        let (p,t)=room(&ctx).await;
        let r=role(&ctx,&p,&t,"sol","agent","engineer", &["DONE"],json!({"scope":"paths","globs":["docs/**"]})).await;
        ack(post(&ctx,&p,r["id"].as_str().unwrap(),"DONE",json!({"item":"B-1","branch":"fix/tests","commit":"a".repeat(40),"base_sha":"b".repeat(40),"checks":[],"changes":[{"status":"M","path":"README.md"}]}),json!({})).await);
        let body: String=sqlx::query_scalar("SELECT envelope->>'body' FROM messages WHERE project_id=$1 AND envelope->>'verb'='ALERT'").bind(&p.project).fetch_one(&ctx.pool).await.unwrap();
        assert!(body.contains("outside edit scope"),"{body}");
        assert!(!body.contains("not a repository path"),"{body}");
    }).await;
}
