//! The Paddle client against `FakePaddle`. No database and no dev stack:
//! these run under the default nextest profile.
#![cfg(test)]

mod support;

use agmen_hub::{Catalog, LineItem, PaddleClient, PaddleConfig, PaddleError, Secret};
use agmen_proto::TenantId;
use reqwest::Method;
use serde_json::{Value, json};
use support::fake_paddle::FakePaddle;

fn config(fake: &FakePaddle) -> PaddleConfig {
    PaddleConfig {
        api_url: fake.base().clone(),
        api_key: Secret::new(fake.api_key()),
        webhook_secret: Secret::new(fake.webhook_secret()),
    }
}

async fn start() -> (FakePaddle, PaddleClient) {
    let fake = FakePaddle::start().await.unwrap();
    let client = PaddleClient::new(&config(&fake), reqwest::Client::new()).unwrap();
    (fake, client)
}

fn item(price_id: &str, quantity: u32) -> LineItem {
    LineItem {
        price_id: price_id.to_owned(),
        quantity,
    }
}

fn tenant() -> TenantId {
    "tenant01".parse().unwrap()
}

fn count(fake: &FakePaddle, method: &Method, path: &str) -> usize {
    fake.requests()
        .iter()
        .filter(|(m, p, _)| m == method && p == path)
        .count()
}

fn ids(catalog: &Catalog) -> [&str; 5] {
    [
        &catalog.team_base,
        &catalog.team_seat,
        &catalog.business_10,
        &catalog.business_25,
        &catalog.business_50,
    ]
}

#[tokio::test]
async fn checkout_sends_items_and_tenant() {
    let (fake, client) = start().await;
    let url = client
        .create_checkout(&tenant(), &[item("pri_a", 1), item("pri_b", 2)])
        .await
        .unwrap();
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    let (method, path, body) = &requests[0];
    assert_eq!((method, path.as_str()), (&Method::POST, "/transactions"));
    assert_eq!(
        body["items"],
        json!([
            {"price_id": "pri_a", "quantity": 1},
            {"price_id": "pri_b", "quantity": 2},
        ])
    );
    assert_eq!(body["custom_data"], json!({"agmen_tenant": "tenant01"}));
    assert_eq!(url.as_str(), format!("{}checkout/txn_1", fake.base()));
}

#[tokio::test]
async fn checkout_without_a_url_is_a_shape_error() {
    let (fake, client) = start().await;
    fake.fail_next(201, json!({"data": {"id": "txn_x", "checkout": null}}));
    let err = client
        .create_checkout(&tenant(), &[item("pri_a", 1)])
        .await
        .unwrap_err();
    match err {
        PaddleError::Shape(message) => {
            assert!(
                message.to_lowercase().contains("default payment link"),
                "{message}"
            );
        }
        other => panic!("expected a shape error, got {other:?}"),
    }
}

#[tokio::test]
async fn update_drops_zero_quantity() {
    let (fake, client) = start().await;
    client
        .update_items("sub_1", &[item("pri_a", 1), item("pri_b", 0)])
        .await
        .unwrap();
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    let (method, path, body) = &requests[0];
    assert_eq!(
        (method, path.as_str()),
        (&Method::PATCH, "/subscriptions/sub_1")
    );
    assert_eq!(body["items"], json!([{"price_id": "pri_a", "quantity": 1}]));
    assert_eq!(body["proration_billing_mode"], "prorated_immediately");
}

#[tokio::test]
async fn update_returns_the_subscription_items() {
    let (_fake, client) = start().await;
    let items = client
        .update_items("sub_1", &[item("pri_a", 1), item("pri_b", 2)])
        .await
        .unwrap();
    assert_eq!(items, vec![item("pri_a", 1), item("pri_b", 2)]);
}

#[tokio::test]
async fn update_with_no_items_sends_nothing() {
    let (fake, client) = start().await;
    let err = client
        .update_items("sub_1", &[item("pri_b", 0)])
        .await
        .unwrap_err();
    assert!(matches!(err, PaddleError::Shape(_)), "{err:?}");
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn portal_url_from_session() {
    let (fake, client) = start().await;
    let url = client.portal_url("ctm_1", "sub_1").await.unwrap();
    assert_eq!(url.as_str(), format!("{}portal/ctm_1", fake.base()));
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    let (method, path, body) = &requests[0];
    assert_eq!(
        (method, path.as_str()),
        (&Method::POST, "/customers/ctm_1/portal-sessions")
    );
    assert_eq!(body["subscription_ids"], json!(["sub_1"]));
}

#[tokio::test]
async fn refusal_carries_status_and_code() {
    let (fake, client) = start().await;
    fake.fail_next(400, json!({"error": {"code": "bad_request"}}));
    let err = client
        .create_checkout(&tenant(), &[item("pri_a", 1)])
        .await
        .unwrap_err();
    assert!(
        matches!(&err, PaddleError::Refused { status: 400, code } if code == "bad_request"),
        "{err:?}"
    );
}

#[tokio::test]
async fn refusal_without_an_error_body() {
    let (fake, client) = start().await;
    fake.fail_next(502, Value::Null);
    let err = client
        .create_checkout(&tenant(), &[item("pri_a", 1)])
        .await
        .unwrap_err();
    assert!(
        matches!(&err, PaddleError::Refused { status: 502, code } if code.is_empty()),
        "{err:?}"
    );
}

#[tokio::test]
async fn ensure_catalog_is_idempotent() {
    let (fake, client) = start().await;
    let first = client.ensure_catalog().await.unwrap();
    assert_eq!(count(&fake, &Method::POST, "/products"), 2);
    assert_eq!(count(&fake, &Method::POST, "/prices"), 5);
    let mut distinct = ids(&first).to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 5);
    assert!(ids(&first).iter().all(|id| id.starts_with("pri_")));

    let bodies = |path: &str| -> Vec<Value> {
        fake.requests()
            .into_iter()
            .filter(|(m, p, _)| *m == Method::POST && p == path)
            .map(|(_, _, body)| body)
            .collect()
    };
    let products = bodies("/products");
    let mut names: Vec<&str> = products
        .iter()
        .map(|body| body["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["agmen Business", "agmen Team"]);
    let prices = bodies("/prices");
    let seat = prices
        .iter()
        .find(|body| body["custom_data"]["agmen_key"] == "team_seat")
        .unwrap();
    assert_eq!(seat["quantity"], json!({"minimum": 1, "maximum": 1000}));

    let writes = fake.requests().len();
    let second = client.ensure_catalog().await.unwrap();
    assert_eq!(second, first);
    assert_eq!(count(&fake, &Method::POST, "/products"), 2);
    assert_eq!(count(&fake, &Method::POST, "/prices"), 5);
    assert!(fake.requests().len() > writes, "the second run still reads");
}

#[tokio::test]
async fn ensure_catalog_pages_through_lists() {
    let (fake, client) = start().await;
    let first = client.ensure_catalog().await.unwrap();
    fake.set_page_size(1);
    let reads = count(&fake, &Method::GET, "/prices");
    let second = client.ensure_catalog().await.unwrap();
    assert_eq!(second, first);
    assert_eq!(count(&fake, &Method::POST, "/products"), 2);
    assert_eq!(count(&fake, &Method::POST, "/prices"), 5);
    assert!(
        count(&fake, &Method::GET, "/prices") >= reads + 5,
        "five prices at one per page take at least five reads"
    );
}

#[tokio::test]
async fn ensure_catalog_ignores_foreign_prices() {
    let (fake, client) = start().await;
    let http = reqwest::Client::new();
    let product = fake.base().join("products").unwrap();
    let price = fake.base().join("prices").unwrap();
    let mut foreign = Vec::new();
    for body in [
        json!({"description": "other", "custom_data": {"agmen_key": "someone_else"}}),
        json!({"description": "plain"}),
        json!({"description": "wrong type", "custom_data": {"agmen_key": 7}}),
    ] {
        let created: Value = http
            .post(price.clone())
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        foreign.push(created["data"]["id"].as_str().unwrap().to_owned());
    }
    http.post(product)
        .json(&json!({"name": "agmen Team extras", "tax_category": "standard"}))
        .send()
        .await
        .unwrap();

    let catalog = client.ensure_catalog().await.unwrap();
    for id in ids(&catalog) {
        assert!(!foreign.iter().any(|f| f == id), "{id} is a foreign price");
    }
    // Two products ("agmen Team" and "agmen Business") were still created: the
    // foreign one's name is not an exact match.
    assert_eq!(count(&fake, &Method::POST, "/products"), 1 + 2);
    assert_eq!(count(&fake, &Method::POST, "/prices"), 3 + 5);
    assert_eq!(count(&fake, &Method::PATCH, "/prices"), 0);
}

#[tokio::test]
async fn ensure_catalog_stops_at_the_page_bound() {
    let (fake, client) = start().await;
    fake.set_endless_pages(true);
    let err = client.ensure_catalog().await.unwrap_err();
    match err {
        PaddleError::Shape(message) => assert!(message.contains("/products"), "{message}"),
        other => panic!("expected a shape error, got {other:?}"),
    }
    let reads = count(&fake, &Method::GET, "/products");
    assert_eq!(reads, 20, "the loop reads exactly 20 pages, then stops");
    assert_eq!(count(&fake, &Method::POST, "/products"), 0);
}

#[tokio::test]
async fn api_key_sent_as_bearer() {
    let (fake, client) = start().await;
    client.ensure_catalog().await.unwrap();
    client
        .create_checkout(&tenant(), &[item("pri_a", 1)])
        .await
        .unwrap();
    client
        .update_items("sub_1", &[item("pri_a", 1)])
        .await
        .unwrap();
    client.portal_url("ctm_1", "sub_1").await.unwrap();
    let expected = Some(format!("Bearer {}", fake.api_key()));
    let seen = fake.authorizations();
    assert_eq!(seen.len(), fake.requests().len());
    assert!(seen.len() > 4);
    assert!(seen.iter().all(|header| *header == expected), "{seen:?}");
}

#[tokio::test]
async fn paddle_version_header_is_sent() {
    let (fake, client) = start().await;
    client.portal_url("ctm_1", "sub_1").await.unwrap();
    assert_eq!(fake.paddle_versions(), [Some("1".to_owned())]);
}

#[tokio::test]
async fn debug_and_errors_never_print_the_key() {
    let (fake, client) = start().await;
    let key = fake.api_key().to_owned();
    assert!(!format!("{client:?}").contains(&key));
    fake.fail_next(400, json!({"error": {"code": "bad_request"}}));
    let err = client
        .create_checkout(&tenant(), &[item("pri_a", 1)])
        .await
        .unwrap_err();
    assert!(!format!("{err}").contains(&key));
    assert!(!format!("{err:?}").contains(&key));
}
