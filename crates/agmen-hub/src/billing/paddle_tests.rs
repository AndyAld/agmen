use serde_json::json;
use url::Url;

use super::*;

fn item(price_id: &str, quantity: u32) -> LineItem {
    LineItem {
        price_id: price_id.to_owned(),
        quantity,
    }
}

#[test]
fn price_bodies_match_the_catalog_amounts() {
    let expected = [
        ("team_base", "2500", None),
        ("team_seat", "1000", Some((1, 1000))),
        ("business_10", "10000", None),
        ("business_25", "20000", None),
        ("business_50", "35000", None),
    ];
    assert_eq!(PRICES.len(), expected.len());
    for (spec, (key, amount, quantity)) in PRICES.iter().zip(expected) {
        let body = price_body(spec, "pro_x");
        assert_eq!(body["custom_data"], json!({"agmen_key": key}));
        assert_eq!(body["product_id"], "pro_x");
        assert_eq!(
            body["unit_price"],
            json!({"amount": amount, "currency_code": "USD"}),
            "{key}"
        );
        assert_eq!(
            body["billing_cycle"],
            json!({"interval": "month", "frequency": 1}),
            "{key}"
        );
        match quantity {
            Some((minimum, maximum)) => {
                assert_eq!(
                    body["quantity"],
                    json!({"minimum": minimum, "maximum": maximum})
                );
            }
            None => assert!(body.get("quantity").is_none(), "{key}"),
        }
        assert!(body["description"].as_str().is_some_and(|d| d.len() >= 2));
    }
    let team: Vec<_> = PRICES
        .iter()
        .filter(|spec| spec.product == TEAM_PRODUCT)
        .map(|spec| spec.key)
        .collect();
    assert_eq!(team, ["team_base", "team_seat"]);
}

#[test]
fn checkout_body_shape() {
    let tenant: TenantId = "tenant01".parse().unwrap();
    let body = checkout_body(&tenant, &[item("pri_a", 1), item("pri_b", 3)]);
    assert_eq!(
        body,
        json!({
            "items": [
                {"price_id": "pri_a", "quantity": 1},
                {"price_id": "pri_b", "quantity": 3},
            ],
            "custom_data": {"agmen_tenant": "tenant01"},
        })
    );
}

#[test]
fn update_body_drops_zero_and_refuses_empty() {
    let body = update_body(&[item("pri_a", 1), item("pri_b", 0)]).unwrap();
    assert_eq!(
        body,
        json!({
            "items": [{"price_id": "pri_a", "quantity": 1}],
            "proration_billing_mode": "prorated_immediately",
        })
    );
    assert!(matches!(
        update_body(&[item("pri_b", 0)]),
        Err(PaddleError::Shape(_))
    ));
    assert!(matches!(update_body(&[]), Err(PaddleError::Shape(_))));
}

#[test]
fn response_items_parse_to_line_items() {
    // The shape of an update-subscription response and of a subscription
    // webhook: the price id is nested under `price.id`.
    let response = json!({
        "data": {
            "id": "sub_1",
            "status": "active",
            "items": [
                {
                    "status": "active",
                    "quantity": 1,
                    "recurring": true,
                    "price": {"id": "pri_a", "product_id": "pro_a", "type": "standard"},
                    "product": {"id": "pro_a", "name": "agmen Team"},
                },
                {"quantity": 4, "price": {"id": "pri_b"}},
                {"status": "inactive", "quantity": 2, "price": {"id": "pri_c"}},
            ],
        },
    });
    assert_eq!(
        items_of(&response).unwrap(),
        [item("pri_a", 1), item("pri_b", 4)]
    );
    for bad in [
        json!({"data": {}}),
        json!({"data": {"items": [{"quantity": 1, "price_id": "pri_a"}]}}),
        json!({"data": {"items": [{"quantity": -1, "price": {"id": "pri_a"}}]}}),
        json!({"data": {"items": [{"price": {"id": "pri_a"}}]}}),
    ] {
        assert!(
            matches!(items_of(&bad), Err(PaddleError::Shape(_))),
            "{bad}"
        );
    }
}

#[test]
fn unparsable_checkout_url_is_shape() {
    for bad in [
        json!({"data": {"checkout": {"url": "not a url"}}}),
        json!({"data": {"checkout": {"url": null}}}),
        json!({"data": {"checkout": null}}),
        json!({"data": {}}),
        json!({"data": {"checkout": {"url": 7}}}),
    ] {
        assert!(
            matches!(checkout_url(&bad), Err(PaddleError::Shape(_))),
            "{bad}"
        );
    }
    let ok = json!({"data": {"checkout": {"url": "https://pay.example.test/pay?_ptxn=txn_1"}}});
    assert_eq!(
        checkout_url(&ok).unwrap().as_str(),
        "https://pay.example.test/pay?_ptxn=txn_1"
    );
}

#[test]
fn endpoints_join_with_or_without_a_trailing_slash() {
    for base in ["https://api.example.test", "https://api.example.test/"] {
        let url = endpoint(
            &Url::parse(base).unwrap(),
            &["customers", "ctm_1", "portal-sessions"],
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.example.test/customers/ctm_1/portal-sessions"
        );
    }
    let nested = endpoint(
        &Url::parse("https://example.test/paddle/").unwrap(),
        &["prices"],
    )
    .unwrap();
    assert_eq!(nested.as_str(), "https://example.test/paddle/prices");
    // An id with a slash stays one path segment.
    let odd = endpoint(
        &Url::parse("https://api.example.test/").unwrap(),
        &["subscriptions", "a/b"],
    )
    .unwrap();
    assert_eq!(odd.as_str(), "https://api.example.test/subscriptions/a%2Fb");
}

#[test]
fn refusal_code_comes_from_the_error_body() {
    assert_eq!(
        refusal_code(br#"{"error":{"code":"bad_request","detail":"x"}}"#),
        "bad_request"
    );
    assert_eq!(refusal_code(b""), "");
    assert_eq!(refusal_code(b"<html>"), "");
    assert_eq!(refusal_code(br#"{"error":{"code":7}}"#), "");
}

#[test]
fn next_cursor_reads_the_after_parameter_only() {
    let page = json!({"meta": {"pagination": {
        "has_more": true,
        "next": "https://evil.example.test/products?per_page=200&after=pro_9",
    }}});
    assert_eq!(
        next_cursor(&page, "products").unwrap().as_deref(),
        Some("pro_9")
    );
    let last = json!({"meta": {"pagination": {"has_more": false, "next": "https://x.test/p?after=pro_9"}}});
    assert_eq!(next_cursor(&last, "products").unwrap(), None);
    let broken = json!({"meta": {"pagination": {"has_more": true, "next": "https://x.test/p"}}});
    assert!(matches!(
        next_cursor(&broken, "products"),
        Err(PaddleError::Shape(_))
    ));
}
