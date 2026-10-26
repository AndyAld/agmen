//! Identities, broker tokens, the JWKS and revocation, against an
//! in-process hub, a database of the test's own on `AGMEN_IT_PG`, and the
//! dev EMQX (`AGMEN_IT_MQTT`, `AGMEN_IT_EMQX_API`).
//!
//! The dev EMQX verifies tokens against the shared static JWKS on port 8090,
//! which lists dev keys 1 and 2 (ruling R2); the hub signs with the same key
//! files from `AGMEN_IT_JWT_DIR`, so EMQX accepts what the hub mints.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agmen_hub::{BrokerTokens, SigningKey};
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use reqwest::StatusCode;
use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::mqttbytes::v5::{
    ConnectReturnCode, DisconnectReasonCode, PubAckReason, SubscribeReasonCode,
};
use serde_json::{Value, json};
use support::mqtt::{self, Client, connect_ok, connect_refused};
use support::{
    Ctx, DEV_KEY_1, DEV_KEY_2, Minted, Project, TestHub, broker_token, count, env, hub_jwks, login,
    member_with_token, mint, mint_ok, new_id, run, seed_project, unban_username,
};

const ANDY: u64 = 4242;
const ADAM: u64 = 2;

fn now_secs() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

fn base(project: &Project) -> String {
    format!("agmen/v1/{}/{}", project.tenant, project.project)
}

/// Andy, a member of a fresh project, with his token.
async fn andy(ctx: &Ctx) -> (Project, String) {
    let project = seed_project(&ctx.pool).await;
    let token = member_with_token(ctx, &project, ANDY, "andy", "member").await;
    (project, token)
}

/// A broker token for `minted` from `hub`, asserting the hub gave one.
async fn broker_token_ok(ctx: &Ctx, hub: &TestHub, minted: &Minted) -> String {
    let (status, body) = broker_token(&ctx.http, hub, &minted.id, &minted.token).await;
    assert_eq!(status, StatusCode::OK, "broker-token answered {body}");
    body["token"].as_str().unwrap().to_owned()
}

/// Verifies `token` against the JWKS `hub` serves and returns its kid and
/// claims.
async fn verify(ctx: &Ctx, hub: &TestHub, token: &str) -> (String, Value) {
    let kid = jsonwebtoken::decode_header(token).unwrap().kid.unwrap();
    let jwks = hub_jwks(&ctx.http, hub).await;
    let jwk: Jwk = serde_json::from_value(jwks[&kid].clone()).unwrap();
    let mut validation = Validation::new(Algorithm::ES256);
    validation.validate_aud = false;
    validation.set_required_spec_claims(&["exp", "sub"]);
    let data =
        jsonwebtoken::decode::<Value>(token, &DecodingKey::from_jwk(&jwk).unwrap(), &validation)
            .unwrap_or_else(|e| panic!("token with kid {kid} does not verify: {e}"));
    (kid, data.claims)
}

/// A subscriber with the hub's own broker token in a share group of its
/// own, so parallel tests each see every `up/` message.
async fn hub_subscriber() -> Client {
    let key = PathBuf::from(env("AGMEN_IT_JWT_DIR")).join(format!("{DEV_KEY_1}.pem"));
    let tokens = BrokerTokens::new(SigningKey::from_pem_file(key).unwrap(), Vec::new()).unwrap();
    let token = tokens.mint_hub(Duration::from_secs(600)).unwrap();
    let client_id = format!("agmen-hub-{}", new_id());
    let mut hub = connect_ok(mqtt::options(&client_id, "agmen-hub", &token)).await;
    let filter = format!("$share/hub{}/agmen/v1/+/+/up/+", new_id());
    assert_eq!(
        hub.subscribe(&filter).await,
        SubscribeReasonCode::Success(QoS::AtLeastOnce)
    );
    hub
}

#[tokio::test]
async fn it_mint_child_identity() {
    run(|ctx| async move {
        let (project, token) = andy(&ctx).await;
        let minted = mint_ok(&ctx, &token, "andy/sol", &project).await;

        let id = minted.id.as_str();
        assert!(
            (8..=32).contains(&id.len())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
            "id {id:?} is not [a-z0-9]{{8,32}}"
        );
        assert_eq!(minted.body["handle"], "andy/sol");
        assert_eq!(minted.body["kind"], "agent");
        assert_eq!(minted.body["tenant"], project.tenant.as_str());
        assert_eq!(minted.body["project"], project.project.as_str());
        assert_ne!(minted.token, token, "the child got the human's token");
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM identities i JOIN humans h ON h.id = i.owner_human_id
                 WHERE i.id = $1 AND h.handle = 'andy' AND i.revoked_at IS NULL",
                id
            )
            .await,
            1,
            "identity row owned by andy"
        );

        // The broker knows it by that id: its broker token's username is it.
        let broker = broker_token_ok(&ctx, &ctx.hub, &minted).await;
        let (_, claims) = verify(&ctx, &ctx.hub, &broker).await;
        assert_eq!(claims["username"], id);

        // Kind defaults to agent; coordinator needs the coordinator role.
        let (status, _) = mint(
            &ctx,
            &token,
            &json!({ "handle": "andy/coord", "project": project.project, "kind": "coordinator" }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    })
    .await;
}

#[tokio::test]
async fn it_child_handle_prefixed_by_owner() {
    run(|ctx| async move {
        let (project, token) = andy(&ctx).await;
        for handle in [
            "adam/x",
            "andy",
            "andy/",
            "andy/a/b",
            "andy/Sol",
            "andyx/sol",
            "/sol",
        ] {
            let (status, body) = mint(
                &ctx,
                &token,
                &json!({ "handle": handle, "project": project.project }),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{handle:?} answered {body}"
            );
        }
        mint_ok(&ctx, &token, "andy/sol", &project).await;
        let (status, _) = mint(
            &ctx,
            &token,
            &json!({ "handle": "andy/sol", "project": project.project }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "second live andy/sol");

        let other = seed_project(&ctx.pool).await;
        let (status, _) = mint(
            &ctx,
            &token,
            &json!({ "handle": "andy/sol", "project": other.project }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "not a member of that project"
        );
    })
    .await;
}

#[tokio::test]
async fn it_identity_token_cannot_mint_for_others() {
    run(|ctx| async move {
        let (project, token) = andy(&ctx).await;
        let sol = mint_ok(&ctx, &token, "andy/sol", &project).await;
        let adam = member_with_token(&ctx, &project, ADAM, "adam", "member").await;
        let adams = mint_ok(&ctx, &adam, "adam/coder", &project).await;

        for handle in ["adam/x", "andy/y"] {
            let (status, body) = mint(
                &ctx,
                &sol.token,
                &json!({ "handle": handle, "project": project.project }),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{handle:?} answered {body}");
        }
        let invite = ctx
            .http
            .post(ctx.hub.url("/v1/invites"))
            .bearer_auth(&sol.token)
            .json(&json!({ "project": project.project, "role": "member" }))
            .send()
            .await
            .unwrap();
        assert_eq!(invite.status(), StatusCode::FORBIDDEN);

        // Broker tokens only for the token's own identity, never with a
        // human token.
        let (status, _) = broker_token(&ctx.http, &ctx.hub, &adams.id, &sol.token).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = broker_token(&ctx.http, &ctx.hub, &sol.id, &token).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = broker_token(&ctx.http, &ctx.hub, &sol.id, "not-a-token").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM identities WHERE project_id = $1",
                &project.project
            )
            .await,
            2
        );
    })
    .await;
}

#[tokio::test]
async fn it_broker_token_claims_are_the_adr_0002_set() {
    run(|ctx| async move {
        let (project, token) = andy(&ctx).await;
        let sol = mint_ok(&ctx, &token, "andy/sol", &project).await;
        let before = now_secs();
        let (status, body) = broker_token(&ctx.http, &ctx.hub, &sol.id, &sol.token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let broker = body["token"].as_str().unwrap();
        let (kid, claims) = verify(&ctx, &ctx.hub, broker).await;
        assert_eq!(kid, DEV_KEY_1);

        let keys: BTreeSet<&str> = claims
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from(["acl", "exp", "sub", "username"]),
            "never is_superuser, clientid_override or client_attrs"
        );
        assert_eq!(claims["sub"], sol.id.as_str());
        assert_eq!(claims["username"], sol.id.as_str());
        let exp = claims["exp"].as_i64().unwrap();
        assert!(
            (before + 3600..=now_secs() + 3600).contains(&exp),
            "exp {exp} is not one hour out"
        );
        let expires_at: jiff::Timestamp = body["expires_at"].as_str().unwrap().parse().unwrap();
        assert!(
            expires_at.as_second() <= exp && expires_at.as_second() >= exp - 1,
            "expires_at {expires_at} does not match exp {exp}"
        );

        let (b, id) = (base(&project), sol.id.as_str());
        assert_eq!(
            claims["acl"],
            json!([
                { "permission": "allow", "action": "publish",   "topic": format!("eq {b}/up/{id}"), "qos": [1] },
                { "permission": "allow", "action": "publish",   "topic": format!("eq {b}/presence/{id}"), "retain": true },
                { "permission": "allow", "action": "subscribe", "topic": format!("eq {b}/feed") },
                { "permission": "allow", "action": "subscribe", "topic": format!("eq {b}/inbox/{id}") },
                { "permission": "allow", "action": "subscribe", "topic": format!("eq {b}/ack/{id}") },
                { "permission": "allow", "action": "subscribe", "topic": format!("{b}/state/#") },
                { "permission": "allow", "action": "subscribe", "topic": format!("{b}/presence/+") },
                { "permission": "deny",  "action": "all",       "topic": "#" }
            ])
        );
    })
    .await;
}

#[tokio::test]
async fn it_broker_token_connects() {
    run(|ctx| async move {
        let (project, token) = andy(&ctx).await;
        let sol = mint_ok(&ctx, &token, "andy/sol", &project).await;
        let other = mint_ok(&ctx, &token, "andy/other", &project).await;
        let broker = broker_token_ok(&ctx, &ctx.hub, &sol).await;
        let mut hub = hub_subscriber().await;

        let mut client =
            connect_ok(mqtt::options(&format!("{}-mcp", sol.id), &sol.id, &broker)).await;
        let b = base(&project);
        let own_up = format!("{b}/up/{}", sol.id);
        assert_eq!(
            client.publish(&own_up, b"hello").await,
            PubAckReason::Success
        );
        let got = hub
            .recv_on(&own_up, Duration::from_secs(5))
            .await
            .expect("the hub got nothing on the identity's up/");
        assert_eq!(got.payload.as_ref(), b"hello");

        for topic in [format!("{b}/up/{}", other.id), format!("{b}/feed")] {
            assert_eq!(
                client.publish(&topic, b"spoof").await,
                PubAckReason::NotAuthorized,
                "publish to {topic}"
            );
        }
        assert_eq!(
            client.subscribe(&format!("{b}/feed")).await,
            SubscribeReasonCode::Success(QoS::AtLeastOnce)
        );
        assert_eq!(
            client.subscribe(&format!("{b}/inbox/{}", other.id)).await,
            SubscribeReasonCode::NotAuthorized
        );
    })
    .await;
}

#[tokio::test]
async fn it_jwks_rotation() {
    run(|ctx| async move {
        let (project, token) = andy(&ctx).await;
        let sol = mint_ok(&ctx, &token, "andy/sol", &project).await;
        let old = broker_token_ok(&ctx, &ctx.hub, &sol).await;
        let before: HashMap<String, Value> = hub_jwks(&ctx.http, &ctx.hub).await;
        assert_eq!(
            before.keys().collect::<Vec<_>>(),
            [DEV_KEY_1],
            "before rotation the hub publishes only its signing key"
        );

        // Rotate: a hub on the same database now signs with key 2 and keeps
        // publishing key 1 until the last token it signed has expired.
        let rotated = TestHub::start(&ctx.db_url, &ctx.github, DEV_KEY_2, &[DEV_KEY_1]).await;
        let new = broker_token_ok(&ctx, &rotated, &sol).await;
        let after = hub_jwks(&ctx.http, &rotated).await;
        assert_eq!(
            after.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            BTreeSet::from([DEV_KEY_1, DEV_KEY_2])
        );
        assert_eq!(after[DEV_KEY_1], before[DEV_KEY_1], "key 1 changed");
        let (old_kid, _) = verify(&ctx, &rotated, &old).await;
        let (new_kid, _) = verify(&ctx, &rotated, &new).await;
        assert_eq!(old_kid, DEV_KEY_1);
        assert_eq!(new_kid, DEV_KEY_2);

        // EMQX accepts both at once, on two of the identity's client ids,
        // and both publishes reach the hub.
        let mut hub = hub_subscriber().await;
        let up = format!("{}/up/{}", base(&project), sol.id);
        let mut with_old =
            connect_ok(mqtt::options(&format!("{}-mcp", sol.id), &sol.id, &old)).await;
        let mut with_new =
            connect_ok(mqtt::options(&format!("{}-watch", sol.id), &sol.id, &new)).await;
        for (client, payload) in [(&mut with_old, b"old"), (&mut with_new, b"new")] {
            assert_eq!(client.publish(&up, payload).await, PubAckReason::Success);
            let got = hub
                .recv_on(&up, Duration::from_secs(5))
                .await
                .expect("the hub got nothing");
            assert_eq!(got.payload.as_ref(), payload);
        }
    })
    .await;
}

#[tokio::test]
async fn it_revoked_identity_token_refused_on_every_route() {
    run(|ctx| async move {
        let (project, token) = andy(&ctx).await;
        let a = mint_ok(&ctx, &token, "andy/a", &project).await;
        let b = mint_ok(&ctx, &token, "andy/b", &project).await;
        // Revoked in the database directly: this test is about the hub
        // token check (ruling R40), not about EMQX.
        sqlx::query("UPDATE identities SET revoked_at = now() WHERE id = $1")
            .bind(&a.id)
            .execute(&ctx.pool)
            .await
            .unwrap();
        // Every route that takes a bearer token.
        let routes = [
            ("GET", format!("/v1/identities/{}/broker-token", a.id), None),
            ("GET", format!("/v1/identities/{}/broker-token", b.id), None),
            (
                "POST",
                "/v1/identities".to_owned(),
                Some(json!({ "handle": "andy/c", "project": project.project })),
            ),
            ("DELETE", format!("/v1/identities/{}", a.id), None),
            ("DELETE", format!("/v1/identities/{}", b.id), None),
            (
                "POST",
                "/v1/invites".to_owned(),
                Some(json!({ "project": project.project, "role": "member" })),
            ),
            // The token is checked before the code is looked up.
            ("POST", "/v1/invites/anycode/accept".to_owned(), None),
        ];
        for (method, path, body) in &routes {
            let mut req = ctx
                .http
                .request(method.parse().unwrap(), ctx.hub.url(path))
                .bearer_auth(&a.token);
            if let Some(body) = body {
                req = req.json(body);
            }
            let resp = req.send().await.unwrap();
            let status = resp.status();
            let answer: Value = resp.json().await.unwrap_or(Value::Null);
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {answer}");
            assert_eq!(answer["error"], "identity_revoked", "{method} {path}");
        }

        // The owner's other identity is untouched.
        let (status, body) = broker_token(&ctx.http, &ctx.hub, &b.id, &b.token).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    })
    .await;
}

/// Once revoked_at is set, the ban and the kick happen even when a later
/// step fails. The failure is injected through the test's own database:
/// with its foreign key dropped, the identity's stored tenant id no longer
/// parses, so the hub cannot name the roster to publish.
#[tokio::test]
async fn it_revoke_bans_and_kicks_when_the_roster_publish_fails() {
    run(|ctx| async move {
        let (project, token) = andy(&ctx).await;
        let a = mint_ok(&ctx, &token, "andy/a", &project).await;
        let broker = broker_token_ok(&ctx, &ctx.hub, &a).await;
        let client_id = format!("{}-watch", a.id);
        let mut client = connect_ok(mqtt::options(&client_id, &a.id, &broker)).await;
        sqlx::query("ALTER TABLE identities DROP CONSTRAINT identities_project_id_tenant_id_fkey")
            .execute(&ctx.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE identities SET tenant_id = 'Not A Tenant' WHERE id = $1")
            .bind(&a.id)
            .execute(&ctx.pool)
            .await
            .unwrap();

        let revoked = ctx
            .http
            .delete(ctx.hub.url(&format!("/v1/identities/{}", a.id)))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
        let reason = client
            .closed_within(Duration::from_secs(5))
            .await
            .unwrap_or_else(|e| panic!("{client_id}: {e} 5 s after revocation"));
        assert_eq!(reason, Some(DisconnectReasonCode::AdministrativeAction));
        assert_eq!(
            connect_refused(mqtt::options(&client_id, &a.id, &broker)).await,
            ConnectReturnCode::Banned
        );
        unban_username(&ctx.http, &a.id).await;
    })
    .await;
}

/// Mint parses what it needs before it commits, so a stored id that does
/// not parse fails the request with nothing stored, instead of storing an
/// identity whose token, shown once, never reaches the caller. Injected
/// through the test's own database as above.
#[tokio::test]
async fn it_mint_fails_before_commit_on_a_bad_stored_tenant() {
    run(|ctx| async move {
        let (project, token) = andy(&ctx).await;
        sqlx::query("ALTER TABLE projects DROP CONSTRAINT projects_tenant_id_fkey")
            .execute(&ctx.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE projects SET tenant_id = 'Not A Tenant' WHERE id = $1")
            .bind(&project.project)
            .execute(&ctx.pool)
            .await
            .unwrap();
        let (status, body) = mint(
            &ctx,
            &token,
            &json!({ "handle": "andy/a", "project": project.project }),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert_eq!(
            count(
                &ctx.pool,
                "SELECT count(*) FROM identities WHERE project_id = $1",
                &project.project
            )
            .await,
            0,
            "nothing was stored"
        );
    })
    .await;
}

#[tokio::test]
async fn it_revoke_kicks_and_refuses_refresh() {
    run(|ctx| async move {
        let (project, token) = andy(&ctx).await;
        let a = mint_ok(&ctx, &token, "andy/a", &project).await;
        let b = mint_ok(&ctx, &token, "andy/b", &project).await;
        let broker = broker_token_ok(&ctx, &ctx.hub, &a).await;
        let mut clients = Vec::new();
        for suffix in ["mcp", "watch", "cli"] {
            let client_id = format!("{}-{suffix}", a.id);
            clients.push((
                client_id.clone(),
                connect_ok(mqtt::options(&client_id, &a.id, &broker)).await,
            ));
        }
        let revoke_url = ctx.hub.url(&format!("/v1/identities/{}", a.id));

        // Only the owner revokes; to anyone else the identity does not exist.
        let adam = login(&ctx, ADAM, "adam").await;
        let refused = ctx
            .http
            .delete(revoke_url.clone())
            .bearer_auth(&adam)
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::NOT_FOUND);
        let by_identity = ctx
            .http
            .delete(revoke_url.clone())
            .bearer_auth(&a.token)
            .send()
            .await
            .unwrap();
        assert_eq!(by_identity.status(), StatusCode::FORBIDDEN);

        let revoked = ctx
            .http
            .delete(revoke_url.clone())
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
        for (client_id, client) in &mut clients {
            let reason = client
                .closed_within(Duration::from_secs(5))
                .await
                .unwrap_or_else(|e| panic!("{client_id}: {e} 5 s after revocation"));
            assert_eq!(
                reason,
                Some(DisconnectReasonCode::AdministrativeAction),
                "{client_id}"
            );
        }

        // The old broker token has not expired, but EMQX now refuses the
        // banned username on every client id (ruling R36).
        for suffix in ["mcp", "watch", "cli"] {
            let client_id = format!("{}-{suffix}", a.id);
            assert_eq!(
                connect_refused(mqtt::options(&client_id, &a.id, &broker)).await,
                ConnectReturnCode::Banned,
                "{client_id} reconnected with the old token"
            );
        }

        let (status, body) = broker_token(&ctx.http, &ctx.hub, &a.id, &a.token).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "identity_revoked");
        // Revoking again bans and kicks again and still succeeds (the ban
        // already exists), so a failed call can be retried.
        let again = ctx
            .http
            .delete(revoke_url)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(again.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            connect_refused(mqtt::options(&format!("{}-mcp", a.id), &a.id, &broker)).await,
            ConnectReturnCode::Banned
        );

        // Andy's other identity is untouched: the ban names A's username only.
        let b_broker = broker_token_ok(&ctx, &ctx.hub, &b).await;
        connect_ok(mqtt::options(&format!("{}-mcp", b.id), &b.id, &b_broker)).await;

        unban_username(&ctx.http, &a.id).await;
    })
    .await;
}
