//! Probes for the cluster: `GET /livez` (the process answers),
//! `GET /healthz` (the database answers too, so the hub can serve), and
//! `GET /statusz` (the database and broker connection answer).

use std::time::Duration;

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use sqlx::PgPool;

use crate::http::Hub;

/// How long `/healthz` waits for the database. Shorter than a kubelet
/// probe's usual timeout, so a slow database reads as not ready instead of
/// as a probe timeout.
const DATABASE_TIMEOUT: Duration = Duration::from_millis(800);

pub(crate) fn routes() -> Router<Hub> {
    Router::new()
        .route("/livez", get(livez))
        .route("/healthz", get(healthz))
        .route("/statusz", get(statusz))
}

/// `GET /livez`: 200 `ok` while the process can answer at all.
async fn livez() -> &'static str {
    "ok"
}

/// `GET /healthz`: 200 `ok` when the database answers `SELECT 1` in time,
/// 503 otherwise.
async fn healthz(State(hub): State<Hub>) -> (StatusCode, &'static str) {
    if database_ready(hub.pool()).await {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
    }
}

/// `GET /statusz`: 200 when the database and broker connection are up,
/// 503 otherwise, with each part's status in the body.
async fn statusz(State(hub): State<Hub>) -> Response {
    let database = database_ready(hub.pool()).await;
    let broker = hub.broker_up();
    let status = if database && broker {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "database": if database { "ok" } else { "down" },
            "broker": if broker { "ok" } else { "down" },
        })),
    )
        .into_response()
}

async fn database_ready(pool: &PgPool) -> bool {
    let check = sqlx::query("SELECT 1").execute(pool);
    match tokio::time::timeout(DATABASE_TIMEOUT, check).await {
        Ok(Ok(_)) => true,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "readiness: database query failed");
            false
        }
        Err(_) => {
            tracing::warn!(timeout = ?DATABASE_TIMEOUT, "readiness: database timed out");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    #[tokio::test]
    async fn unreachable_database_is_not_ready() {
        // Nothing listens on port 1, so the check fails or times out.
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://agmen@127.0.0.1:1/agmen")
            .unwrap();
        assert!(!database_ready(&pool).await);
    }
}
