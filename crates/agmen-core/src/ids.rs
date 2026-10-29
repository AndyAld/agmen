//! Hub identity, clock, message ids and canonical envelope hashes.

use agmen_proto::{ClientEnvelope, IdentityId, MessageId, Timestamp};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The hub's own identity id, stamped as `from` on what it posts. It holds a
/// `0`, which the id alphabet the hub mints from leaves out, so no minted
/// human or identity can ever have it.
pub const HUB_IDENTITY: &str = "agmenhub0";

/// [`HUB_IDENTITY`] as an id.
#[expect(clippy::expect_used, reason = "a constant checked by a unit test")]
#[must_use]
pub fn hub_identity() -> IdentityId {
    HUB_IDENTITY
        .parse()
        .expect("HUB_IDENTITY is a valid identity id")
}

/// The hub's clock, in whole milliseconds as the protocol shows `hub_at`.
#[must_use]
pub fn hub_now() -> Timestamp {
    let now = Timestamp::now();
    Timestamp::from_millisecond(now.as_millisecond()).unwrap_or(now)
}

/// The `id` of a raw publish, read on its own so even a publish that fails
/// to parse can be NACKed.
#[must_use]
pub fn message_id(raw: &[u8]) -> Option<MessageId> {
    #[derive(Deserialize)]
    struct IdOnly {
        id: MessageId,
    }
    serde_json::from_slice::<IdOnly>(raw)
        .ok()
        .map(|only| only.id)
}

/// SHA-256 of the envelope as the hub parsed it, so a resend that differs
/// only in key order or spacing still matches.
///
/// # Errors
///
/// Returns the serialization error if the parsed envelope cannot be encoded.
pub fn envelope_hash(env: &ClientEnvelope) -> Result<Vec<u8>, serde_json::Error> {
    Ok(Sha256::digest(serde_json::to_vec(env)?).to_vec())
}

/// A fresh ULID (26 Crockford base32 digits: 48 bits of milliseconds, then
/// 80 random bits) for a message the hub posts.
///
/// # Errors
///
/// Returns the operating system random-source error.
#[expect(
    clippy::expect_used,
    reason = "the generator emits exactly 26 canonical ULID digits"
)]
pub fn new_message_id(now: Timestamp) -> Result<MessageId, getrandom::Error> {
    const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let millis = u128::try_from(now.as_millisecond()).unwrap_or(0) & ((1 << 48) - 1);
    let mut random = [0_u8; 10];
    getrandom::fill(&mut random)?;
    let value = random
        .iter()
        .fold(millis, |acc, byte| (acc << 8) | u128::from(*byte));
    let text: String = (0..26_u32)
        .rev()
        .map(|digit| char::from(CROCKFORD[((value >> (5 * digit)) & 31) as usize]))
        .collect();
    Ok(text
        .parse()
        .expect("26 Crockford digits from a u128 form a valid ULID"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agmen_proto::ClientEnvelope;
    use std::time::Duration;

    #[test]
    fn hub_message_ids_cover_timestamp_boundaries() {
        for now in [Timestamp::MIN, Timestamp::UNIX_EPOCH, Timestamp::MAX] {
            let generated: Result<MessageId, getrandom::Error> = new_message_id(now);
            let id = generated.unwrap();
            assert_eq!(id.as_str().len(), 26);
            assert!(matches!(id.as_str().as_bytes()[0], b'0'..=b'7'));
            assert_eq!(id.as_str().parse::<MessageId>().unwrap(), id);
        }
    }

    fn envelope(verb: &str, data: &str, extra: &str) -> ClientEnvelope {
        let raw = format!(
            r#"{{"v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"{verb}","data":{data},"body":"hi","sent_at":"2026-10-25T16:04:11Z"{extra}}}"#
        );
        ClientEnvelope::from_json(raw.as_bytes()).unwrap()
    }

    #[test]
    fn hub_identity_can_never_be_minted() {
        let hub = hub_identity();
        assert_eq!(hub.as_str(), HUB_IDENTITY);
        // db::new_id mints from an alphabet without 0, 1, l and o.
        assert!(HUB_IDENTITY.contains('0'));
    }

    #[test]
    fn hub_message_ids_are_ulids_in_time_order() {
        let now: Timestamp = "2026-10-25T16:04:11.402Z".parse().unwrap();
        let first = new_message_id(now).unwrap();
        let second = new_message_id(now.checked_add(Duration::from_millis(1)).unwrap()).unwrap();
        assert_eq!(first.as_str().len(), 26);
        assert!(first < second, "{first} {second}");
        assert_ne!(new_message_id(now).unwrap(), first);
    }

    #[test]
    fn message_id_is_read_from_a_bad_envelope() {
        assert_eq!(
            message_id(br#"{"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"NOPE"}"#)
                .unwrap()
                .as_str(),
            "01JBX6Q4T7W3C2M8Y9K5N0P1RS"
        );
        assert_eq!(message_id(br#"{"verb":"FYI"}"#), None);
        assert_eq!(message_id(b"not json"), None);
    }

    #[test]
    fn envelope_hash_ignores_spacing() {
        let a = envelope("FYI", "{}", "");
        let spaced = ClientEnvelope::from_json(
            br#"{ "sent_at":"2026-10-25T16:04:11Z", "body":"hi", "data":{}, "verb":"FYI",
             "id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS", "v":1 }"#,
        )
        .unwrap();
        assert_eq!(envelope_hash(&a).unwrap(), envelope_hash(&spaced).unwrap());
        let other = envelope("FYI", "{}", r#","items":["B-1"]"#);
        assert_ne!(envelope_hash(&a).unwrap(), envelope_hash(&other).unwrap());
    }
}
