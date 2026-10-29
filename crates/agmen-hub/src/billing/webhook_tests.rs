use aws_lc_rs::hmac::{self, HMAC_SHA256};
use serde_json::json;

use super::*;
use crate::billing::{BusinessStep, Plan};

const TEST_HEADER_LIMIT: usize = 1_024;
const TEST_SIGNATURE_LIMIT: usize = 8;
const TEST_TAG_BYTES: usize = 32;

fn now() -> Timestamp {
    "2026-10-29T12:00:00Z".parse().unwrap()
}

fn signature(body: &[u8], ts: i64) -> String {
    let key = hmac::Key::new(HMAC_SHA256, b"pdl_ntfset_test");
    let mut message = format!("{ts}:").into_bytes();
    message.extend_from_slice(body);
    let tag = hmac::sign(&key, &message);
    format!("ts={ts};h1={}", crate::db::hex(tag.as_ref()))
}

fn verify(header: &str, body: &[u8]) -> Result<(), SignatureError> {
    verify_signature(header, body, &Secret::new("pdl_ntfset_test"), now())
}

fn catalog() -> Catalog {
    Catalog {
        team_base: "pri_team_base".to_owned(),
        team_seat: "pri_team_seat".to_owned(),
        business_10: "pri_business_10".to_owned(),
        business_25: "pri_business_25".to_owned(),
        business_50: "pri_business_50".to_owned(),
    }
}

fn event(kind: &str, data: serde_json::Value) -> PaddleEvent {
    serde_json::from_value(json!({
        "event_id": "evt_1",
        "event_type": kind,
        "occurred_at": now(),
        "notification_id": "ntf_ignored",
        "data": data,
    }))
    .unwrap()
}

fn subscription(status: &str) -> serde_json::Value {
    json!({
        "id": "sub_1",
        "customer_id": "ctm_1",
        "custom_data": {"agmen_tenant": "tenant01"},
        "status": status,
        "items": [
            {"status": "active", "price": {"id": "pri_team_base"}, "quantity": 1},
            {"status": "active", "price": {"id": "pri_team_seat"}, "quantity": 1},
        ],
    })
}

#[test]
fn signature_accepts_valid() {
    let body = br#"{"event_id":"evt_1"}"#;
    assert_eq!(verify(&signature(body, now().as_second()), body), Ok(()));
}

#[test]
fn signature_rejects_tampered_body() {
    let body = b"body";
    assert_eq!(
        verify(&signature(body, now().as_second()), b"Body"),
        Err(SignatureError::Mismatch)
    );
}

#[test]
fn signature_rejects_stale() {
    let body = b"body";
    let cases = [
        (-301, Err(SignatureError::Stale)),
        (-300, Ok(())),
        (-299, Ok(())),
        (300, Ok(())),
        (301, Err(SignatureError::Stale)),
    ];
    for (offset, expected) in cases {
        assert_eq!(
            verify(&signature(body, now().as_second() + offset), body),
            expected
        );
    }
}

#[test]
fn signature_rejects_malformed() {
    let valid = signature(b"body", now().as_second());
    let wrong = crate::db::hex(&[0; TEST_TAG_BYTES]);
    let cases = [
        String::new(),
        "ts=1".to_owned(),
        "h1=ab".to_owned(),
        "ts=x;h1=zz".to_owned(),
        "x".repeat(TEST_HEADER_LIMIT + 1),
        format!(
            "{valid}{}",
            format!(";h1={wrong}").repeat(TEST_SIGNATURE_LIMIT)
        ),
        format!("{valid};ts={}", now().as_second()),
        format!("{valid};h1=zz"),
        format!("ts=-1;h1={wrong}"),
        format!("ts=+1;h1={wrong}"),
        format!("ts=999999999999999999999;h1={wrong}"),
        format!("{valid};unknown=value"),
        format!("{valid};"),
        format!("ts={};h1={wrong}00", now().as_second()),
    ];
    for header in cases {
        assert_eq!(verify(&header, b"body"), Err(SignatureError::Malformed));
    }
}

#[test]
fn signature_refuses_an_empty_secret() {
    // An empty key is a key anyone can sign with, so it must never verify.
    let body = b"body";
    let key = hmac::Key::new(HMAC_SHA256, b"");
    let mut message = format!("{}:", now().as_second()).into_bytes();
    message.extend_from_slice(body);
    let header = format!(
        "ts={};h1={}",
        now().as_second(),
        crate::db::hex(hmac::sign(&key, &message).as_ref())
    );
    assert_eq!(
        verify_signature(&header, body, &Secret::new(""), now()),
        Err(SignatureError::Mismatch)
    );
}

#[test]
fn signature_accepts_any_h1() {
    let valid = signature(b"body", now().as_second());
    let (ts, right) = valid.split_once(';').unwrap();
    let wrong = crate::db::hex(&[0; TEST_TAG_BYTES]);
    assert_eq!(verify(&format!("{ts};h1={wrong};{right}"), b"body"), Ok(()));
    assert_eq!(verify(&format!("{valid};h1={wrong}"), b"body"), Ok(()));
    let at_limit = format!(
        "{ts}{}{right}",
        format!(";h1={wrong}").repeat(TEST_SIGNATURE_LIMIT - 1) + ";"
    );
    assert_eq!(verify(&at_limit, b"body"), Ok(()));
}

#[test]
fn events_map_to_billing() {
    let cases = [
        (
            "subscription.activated",
            "active",
            BillingEvent::Active(Plan::Team { extra_seats: 1 }),
        ),
        ("subscription.updated", "past_due", BillingEvent::PastDue),
        ("subscription.canceled", "canceled", BillingEvent::Canceled),
        (
            "subscription.created",
            "active",
            BillingEvent::Active(Plan::Team { extra_seats: 1 }),
        ),
    ];
    for (kind, status, expected) in cases {
        let mapped = billing_event(&event(kind, subscription(status)), &catalog())
            .unwrap()
            .unwrap();
        assert_eq!(mapped.event, expected);
        assert_eq!(mapped.tenant, Some("tenant01".parse().unwrap()));
        assert_eq!(mapped.ids.customer_id, "ctm_1");
        assert_eq!(mapped.ids.subscription_id, "sub_1");
    }
    let mut data = subscription("completed");
    data["id"] = json!("txn_1");
    data["subscription_id"] = json!("sub_transaction");
    let paid = billing_event(&event("transaction.completed", data.clone()), &catalog())
        .unwrap()
        .unwrap();
    assert_eq!(paid.event, BillingEvent::Paid);
    assert_eq!(paid.ids.customer_id, "ctm_1");
    assert_eq!(paid.ids.subscription_id, "sub_transaction");
    data["subscription_id"] = serde_json::Value::Null;
    assert!(
        billing_event(&event("transaction.completed", data), &catalog())
            .unwrap()
            .is_none()
    );
    assert!(
        billing_event(&event("customer.updated", json!({})), &catalog())
            .unwrap()
            .is_none()
    );
}

#[test]
fn inactive_items_are_ignored() {
    let mut data = subscription("active");
    data["items"] = json!([
        {"status": "active", "price": {"id": "pri_business_25"}, "quantity": 1},
        {"status": "inactive", "price": {"id": "pri_team_base"}, "quantity": 1},
    ]);
    let mapped = billing_event(&event("subscription.activated", data), &catalog())
        .unwrap()
        .unwrap();
    assert_eq!(
        mapped.event,
        BillingEvent::Active(Plan::Business(BusinessStep::Seats25))
    );
}

#[test]
fn subscription_event_without_custom_data_has_no_tenant() {
    let mut data = subscription("past_due");
    data["custom_data"] = serde_json::Value::Null;
    let mapped = billing_event(&event("subscription.past_due", data), &catalog())
        .unwrap()
        .unwrap();
    assert_eq!(mapped.tenant, None);
    assert_eq!(mapped.ids.subscription_id, "sub_1");
}

#[test]
fn trialing_and_paused_are_ignored() {
    for status in ["trialing", "paused"] {
        assert!(
            billing_event(
                &event("subscription.updated", subscription(status)),
                &catalog()
            )
            .unwrap()
            .is_none()
        );
    }
}

#[test]
fn unpersistable_seat_quantity_is_shape() {
    let mut data = subscription("active");
    data["items"][1]["quantity"] = json!(u64::from(u32::try_from(i32::MAX).unwrap()) + 1);
    assert!(matches!(
        billing_event(&event("subscription.activated", data), &catalog()),
        Err(WebhookError::Shape("items"))
    ));
}

#[test]
fn event_errors() {
    let mut data = subscription("active");
    data["custom_data"] = serde_json::Value::Null;
    data["id"] = serde_json::Value::Null;
    assert!(matches!(
        billing_event(&event("subscription.activated", data), &catalog()),
        Err(WebhookError::NoTenant)
    ));
    let mut data = subscription("active");
    data["custom_data"]["agmen_tenant"] = json!("Bad Id");
    assert!(
        matches!(billing_event(&event("subscription.activated", data), &catalog()), Err(WebhookError::BadTenant(value)) if value == "Bad Id")
    );
    let mut data = subscription("active");
    data["items"][0]["price"]["id"] = json!("pri_x");
    assert!(
        matches!(billing_event(&event("subscription.activated", data), &catalog()), Err(WebhookError::UnknownPrice(value)) if value == "pri_x")
    );
    let mut data = subscription("active");
    data["items"] = json!([
        {"price": {"id": "pri_business_10"}, "quantity": 1},
        {"price": {"id": "pri_business_25"}, "quantity": 1},
    ]);
    assert!(matches!(
        billing_event(&event("subscription.activated", data), &catalog()),
        Err(WebhookError::Shape("items"))
    ));
    let mut data = subscription("past_due");
    data["customer_id"] = serde_json::Value::Null;
    assert!(matches!(
        billing_event(&event("subscription.past_due", data), &catalog()),
        Err(WebhookError::Shape("customer_id"))
    ));
    let mut data = subscription("active");
    data["id"] = serde_json::Value::Null;
    assert!(matches!(
        billing_event(&event("subscription.activated", data), &catalog()),
        Err(WebhookError::Shape("id"))
    ));
    let mut data = subscription("active");
    data["status"] = json!("unexpected");
    assert!(matches!(
        billing_event(&event("subscription.updated", data), &catalog()),
        Err(WebhookError::Shape("status"))
    ));
    let body = serde_json::from_slice::<PaddleEvent>(b"{").map_err(WebhookError::Body);
    assert!(matches!(body, Err(WebhookError::Body(_))));
}
