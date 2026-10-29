//! The cluster probes, `GET /livez`, `GET /healthz` and `GET /statusz`, against an
//! in-process hub on a database of the test's own on `AGMEN_IT_PG`.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::time::Duration;

use agmen_hub::HubMode;
use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{run, run_in_mode};

#[tokio::test]
async fn it_health_probes_answer_ok() {
    run(|ctx| async move {
        for path in ["/livez", "/healthz"] {
            let resp = ctx.http.get(ctx.hub.url(path)).send().await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{path}");
            assert_eq!(resp.text().await.unwrap(), "ok", "{path}");
        }
    })
    .await;
}

#[tokio::test]
async fn it_statusz_reports_broker() {
    for mode in [HubMode::SelfHost, HubMode::Hosted] {
        run_in_mode(mode, |ctx| async move {
            tokio::time::timeout(Duration::from_secs(30), async move {
                let url = ctx.hub.url("/statusz");
                let resp = ctx.http.get(url.clone()).send().await.unwrap();
                assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(resp.headers()["cache-control"], "no-store");
                assert_eq!(
                    resp.json::<Value>().await.unwrap(),
                    json!({"database": "ok", "broker": "down"})
                );

                let messaging = ctx.hub.start_messaging().await;
                let resp = ctx.http.get(url.clone()).send().await.unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
                assert_eq!(resp.headers()["cache-control"], "no-store");
                assert_eq!(
                    resp.json::<Value>().await.unwrap(),
                    json!({"database": "ok", "broker": "ok"})
                );

                drop(messaging);
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let resp = ctx.http.get(url.clone()).send().await.unwrap();
                        let status = resp.status();
                        assert_eq!(resp.headers()["cache-control"], "no-store");
                        let body = resp.json::<Value>().await.unwrap();
                        if status == StatusCode::SERVICE_UNAVAILABLE {
                            assert_eq!(body, json!({"database": "ok", "broker": "down"}));
                            break;
                        }
                        assert_eq!(status, StatusCode::OK);
                        assert_eq!(body, json!({"database": "ok", "broker": "ok"}));
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("dropping Messaging did not clear the broker flag within 2 s");
            })
            .await
            .expect("status probes did not finish within 30 s");
        })
        .await;
    }
}
