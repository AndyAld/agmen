use super::*;

#[test]
fn identity_id_rejects_wildcards() {
    for bad in [
        "abc+defg", "abc#defg", "abc/defg", "Abcdefgh", "abcdefg", "",
    ] {
        assert!(bad.parse::<IdentityId>().is_err(), "{bad:?} parsed");
    }
    assert!("a".repeat(33).parse::<IdentityId>().is_err());
    let ok: IdentityId = "abcdefgh".parse().unwrap();
    assert_eq!(ok.as_str(), "abcdefgh");
    assert!("a".repeat(32).parse::<IdentityId>().is_ok());
}

#[test]
fn body_limit_is_bytes_not_chars() {
    assert!(Body::try_from("a".repeat(32_768)).is_ok());
    let two_byte = "\u{e9}".repeat(16_385);
    assert_eq!(two_byte.len(), 32_770);
    assert!(matches!(
        Body::try_from(two_byte),
        Err(ProtoError::BodyTooLarge { len: 32_770 })
    ));
}

#[test]
fn serde_rejects_bad_ids() {
    assert!(serde_json::from_str::<IdentityId>(r#""abc/defg""#).is_err());
    let id: IdentityId = serde_json::from_str(r#""abcdefgh""#).unwrap();
    assert_eq!(serde_json::to_string(&id).unwrap(), r#""abcdefgh""#);
}

#[test]
fn message_id_is_canonical_ulid() {
    assert!("01JBX6Q4T7W3C2M8Y9K5N0P1RS".parse::<MessageId>().is_ok());
    for bad in [
        "01jbx6q4t7w3c2m8y9k5n0p1rs", // lowercase is not the canonical form
        "01JBX6Q4T7W3C2M8Y9K5N0P1R",  // 25 characters
        "81JBX6Q4T7W3C2M8Y9K5N0P1RS", // first character over 7 overflows 128 bits
        "01JBX6Q4T7W3C2M8Y9K5N0P1RU", // U is not in the Crockford alphabet
    ] {
        assert!(bad.parse::<MessageId>().is_err(), "{bad:?} parsed");
    }
}

#[test]
fn item_lane_role_and_sha_rules() {
    for ok in ["B-1", "ABCD-123456"] {
        assert!(ok.parse::<ItemKey>().is_ok(), "{ok:?}");
    }
    for bad in ["b-1", "ABCDE-1", "B-1234567", "B-", "B1"] {
        assert!(bad.parse::<ItemKey>().is_err(), "{bad:?}");
    }
    for ok in ["L-UI", "L-API-2"] {
        assert!(ok.parse::<LaneName>().is_ok(), "{ok:?}");
    }
    assert!(format!("L-{}", "A".repeat(40)).parse::<LaneName>().is_ok());
    for bad in ["L-", "L-ui", "UI", "L-A/B"] {
        assert!(bad.parse::<LaneName>().is_err(), "{bad:?}");
    }
    assert!(format!("L-{}", "A".repeat(41)).parse::<LaneName>().is_err());
    for ok in ["reviewer", "security-reviewer", "qa"] {
        assert!(ok.parse::<RoleName>().is_ok(), "{ok:?}");
    }
    for bad in ["Reviewer", "r", "1reviewer", "role:x"] {
        assert!(bad.parse::<RoleName>().is_err(), "{bad:?}");
    }
    assert!("abc1234".parse::<GitSha>().is_ok());
    assert!("a".repeat(40).parse::<GitSha>().is_ok());
    for bad in ["", "abc123", "ABC1234", "g".repeat(40).as_str()] {
        assert!(bad.parse::<GitSha>().is_err(), "{bad:?}");
    }
}

#[test]
fn mention_strings_round_trip() {
    let cases = [
        ("all", Mention::All),
        ("agents", Mention::Agents),
        ("humans", Mention::Humans),
        ("L-UI", Mention::Lane("L-UI".parse().unwrap())),
        ("role:reviewer", Mention::Role("reviewer".parse().unwrap())),
        ("abcdefgh", Mention::Identity("abcdefgh".parse().unwrap())),
    ];
    for (text, mention) in cases {
        assert_eq!(text.parse::<Mention>().unwrap(), mention, "{text}");
        assert_eq!(mention.to_string(), text);
        let json = serde_json::to_string(&mention).unwrap();
        assert_eq!(json, format!("\"{text}\""));
        assert_eq!(serde_json::from_str::<Mention>(&json).unwrap(), mention);
    }
    for bad in ["andy/sol", "@all", "L-", "role:", "role:Reviewer", "All"] {
        assert!(bad.parse::<Mention>().is_err(), "{bad:?} parsed");
    }
}

#[test]
fn stream_strings_round_trip() {
    assert_eq!("feed".parse::<Stream>().unwrap(), Stream::Feed);
    let inbox: Stream = "inbox:abcdefgh".parse().unwrap();
    assert_eq!(inbox, Stream::Inbox("abcdefgh".parse().unwrap()));
    assert_eq!(inbox.to_string(), "inbox:abcdefgh");
    assert_eq!(serde_json::to_string(&Stream::Feed).unwrap(), r#""feed""#);
    for bad in ["Feed", "inbox:", "inbox:ab/cdefg", "inbox"] {
        assert!(bad.parse::<Stream>().is_err(), "{bad:?} parsed");
    }
}
