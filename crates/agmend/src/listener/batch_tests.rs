use super::super::queue::{Incoming, Queue};
use super::*;
use agmen_proto::{IdentityId, Payload};
use serde_json::json;
use std::time::Duration;

fn message(n: u64, verb: Verb, body: &str) -> StampedEnvelope {
    serde_json::from_value(json!({
        "v": 1, "id": format!("{n:026}"), "reply_to": if n == 1 { Some(format!("{:026}", 9)) } else { None },
        "to": [], "mentions": [], "items": [], "lane": null, "body": body,
        "valid_until": null, "sent_at": "2026-10-27T12:00:00Z", "verb": verb, "data": {},
        "stream": "feed", "seq": n, "from": "person01", "from_handle": "andy",
        "from_kind": "human", "mentioned": [], "refs": [], "hub_at": "2026-10-27T12:00:00Z"
    })).unwrap()
}

fn batch(messages: Vec<StampedEnvelope>) -> Batch {
    let person: IdentityId = "person01".parse().unwrap();
    let (tx, mut queue) = Queue::new("listener1".parse().unwrap(), person);
    for message in messages {
        tx.try_send(Incoming {
            thread: message.client.id.clone(),
            message,
        })
        .unwrap();
    }
    queue.poll(Duration::ZERO, &[]).unwrap()
}

#[test]
fn batch_message_shape() {
    let batch = batch(vec![
        message(1, Verb::Fyi, "hello\nworld"),
        message(2, Verb::Ask, "why?"),
    ]);
    let bytes = encode(&batch, &json!({"workers": []})).unwrap();
    assert_eq!(bytes.iter().filter(|b| **b == b'\n').count(), 1);
    assert_eq!(bytes.last(), Some(&b'\n'));
    let outer: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let content: serde_json::Value =
        serde_json::from_str(outer["message"]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        outer,
        json!({"type": "user", "message": {"role": "user", "content": outer["message"]["content"]}})
    );
    assert_eq!(
        content,
        json!({"batch_id": batch.batch_id.get(), "events": [
        {"seq": 1, "from_handle": "andy", "from_kind": "human", "verb": "FYI", "body": "hello\nworld", "reply_to": format!("{:026}", 9)},
        {"seq": 2, "from_handle": "andy", "from_kind": "human", "verb": "ASK", "body": "why?", "reply_to": null}
    ], "context": {"workers": []}})
    );
}

#[test]
fn batch_over_64k_is_refused() {
    let batch = batch(vec![message(1, Verb::Fyi, "hello")]);
    assert!(matches!(
        encode(&batch, &json!({"text": "x".repeat(MAX_BATCH_BYTES)})),
        Err(BatchError::TooLarge)
    ));
    // Measure the full outer encoding, including escaping and its newline.
    let initial = encode(&batch, &json!("")).unwrap().len();
    assert_eq!(
        encode(&batch, &json!("x".repeat(MAX_BATCH_BYTES - initial)))
            .unwrap()
            .len(),
        MAX_BATCH_BYTES
    );
    assert!(matches!(
        encode(&batch, &json!("x".repeat(MAX_BATCH_BYTES - initial + 1))),
        Err(BatchError::TooLarge)
    ));
}

#[test]
fn batch_without_human_text_uses_payload_json() {
    let mut msg = message(1, Verb::Fyi, "");
    msg.client.payload = Payload::Status {
        summary: "working".into(),
        items: vec![],
        spend_usd: None,
        hours: None,
        tokens: None,
    };
    let line = BatchLine::try_from(&msg).unwrap();
    let serialized = serde_json::to_value(line).unwrap();
    let body: serde_json::Value =
        serde_json::from_str(serialized["body"].as_str().unwrap()).unwrap();
    assert_eq!(body, serde_json::to_value(msg.client.payload).unwrap());
}

#[test]
fn batch_unknown_verb_is_an_error() {
    let mut batch = batch(vec![message(1, Verb::Fyi, "hello")]);
    let mut msg = message(1, Verb::Fyi, "hello");
    msg.client.payload = Payload::Unknown {
        verb: "FUTURE".into(),
        data: json!({}),
    };
    batch.messages = vec![msg];
    assert!(matches!(
        encode(&batch, &json!({})),
        Err(BatchError::UnknownVerb)
    ));
    assert!(matches!(
        BatchLine::try_from(&batch.messages[0]),
        Err(BatchError::UnknownVerb)
    ));
}
