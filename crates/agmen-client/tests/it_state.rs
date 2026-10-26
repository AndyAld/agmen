//! `read_state` and `resolve_handle` against the dev EMQX and an
//! in-process hub (ruling R60). Each test's project is fresh, and the test
//! deletes the retained topics its hub published, even when it fails.

// Test code throughout, helpers included: this lets clippy's
// allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_client::{ClientError, ClientKind, Session, read_state, resolve_handle};
use agmen_proto::{ClientEnvelope, Timestamp};
use serde_json::{Value, json};
use support::{Ctx, close, delivered, env, new_message_id, run};
use url::Url;

/// How many times a test reads the roster before giving up on the hub
/// publishing it.
const ROSTER_READS: usize = 10;

/// A HELLO with a fresh id: the hub publishes the roster when it records
/// one.
fn hello() -> ClientEnvelope {
    let raw = json!({
        "v": 1, "id": new_message_id(), "verb": "HELLO",
        "data": { "client": "agmen-client-tests/0" },
        "body": "", "sent_at": Timestamp::now(),
    });
    ClientEnvelope::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap()
}

/// Deletes the retained message on each of `topics` through the EMQX REST
/// API. One that is already gone (404) is fine.
async fn clear_retained(topics: &[String]) {
    let http = reqwest::Client::new();
    for topic in topics {
        let mut url = Url::parse(&env("AGMEN_IT_EMQX_API")).unwrap();
        url.path_segments_mut()
            .unwrap()
            .pop_if_empty()
            .extend(["mqtt", "retainer", "message", topic]);
        let resp = http
            .delete(url)
            .basic_auth(
                env("AGMEN_IT_EMQX_API_KEY"),
                Some(env("AGMEN_IT_EMQX_API_SECRET")),
            )
            .send()
            .await
            .unwrap();
        assert!(
            matches!(resp.status().as_u16(), 200 | 204 | 404),
            "clearing {topic} answered {}",
            resp.status()
        );
    }
}

/// Runs `body`, then deletes the retained `state/roster` and `state/head`
/// the test's hub published for its project, also when `body` panics.
async fn clearing<F, Fut>(ctx: Ctx, body: F)
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let retained = vec![ctx.topics.state("roster"), ctx.topics.state_head()];
    let outcome = tokio::spawn(body(ctx)).await;
    clear_retained(&retained).await;
    if let Err(err) = outcome {
        std::panic::resume_unwind(err.into_panic());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn it_read_state_roster_and_head() {
    run(|ctx| {
        clearing(ctx, |ctx| async move {
            let who = ctx.identity().await;
            let file = ctx.identity_file(&who);
            let hub = ctx.hub_client(&who);

            // Nothing is retained in a fresh project.
            assert_eq!(read_state(&file, &hub, "roster").await.unwrap(), None);

            let (session, mut events) =
                Session::connect(ctx.config(ClientKind::Cli, &who, &ctx.broker))
                    .await
                    .unwrap();
            let greeting = hello();
            session.publish(greeting.clone()).await.unwrap();
            delivered(&mut events, &greeting.id).await;
            // read_state connects as the same `-cli` client id.
            close(session).await;

            let mut roster = Value::Null;
            for _ in 0..ROSTER_READS {
                if let Some(state) = read_state(&file, &hub, "roster").await.unwrap() {
                    roster = state;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            let entries = roster.as_array().expect("the hub published the roster");
            assert!(
                entries
                    .iter()
                    .any(|entry| entry["identity_id"] == who.identity.id.as_str()),
                "{roster}"
            );
            assert_eq!(
                resolve_handle(&roster, &who.identity.handle).unwrap(),
                Some(who.identity.id.clone())
            );
            assert_eq!(resolve_handle(&roster, "nobody/here").unwrap(), None);

            let head = read_state(&file, &hub, "head")
                .await
                .unwrap()
                .expect("the hub published state/head");
            assert_eq!(head["feed_seq"], 1, "{head}");
        })
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn it_read_state_refuses_a_bad_key() {
    run(|ctx| async move {
        let who = ctx.identity().await;
        let file = ctx.identity_file(&who);
        let hub = ctx.hub_client(&who);
        for bad in ["", "a/b", "+", "#", "head/#"] {
            assert!(
                matches!(
                    read_state(&file, &hub, bad).await,
                    Err(ClientError::StateKey(_))
                ),
                "{bad:?}"
            );
        }
    })
    .await;
}
