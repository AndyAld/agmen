//! The cluster probes, `GET /livez` and `GET /healthz`, against an
//! in-process hub on a database of the test's own on `AGMEN_IT_PG`.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use reqwest::StatusCode;
use support::run;

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
