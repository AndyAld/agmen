//! Frame codec and wire type tests.

use agmen_local::console::{ConsoleEvent, ConsoleSnapshot, Me, RosterEntry};
use agmen_local::frame::{self, FrameError};
use agmen_local::{API, ApiError, ErrorCode, MAX_FRAME, Outcome, Request, Response};
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn frame_round_trips() -> TestResult {
    let mut buf = Vec::new();
    frame::write(&mut buf, &json!({"a": 1}))?;
    assert_eq!(&buf[..4], &[0, 0, 0, 7]);
    let back: serde_json::Value = frame::read(&mut buf.as_slice())?;
    assert_eq!(back, json!({"a": 1}));
    Ok(())
}

#[test]
fn frame_over_cap_is_refused() {
    // Only the length prefix exists; a reader that touched the body would hit EOF instead.
    let bytes = (MAX_FRAME + 1).to_be_bytes();
    let err = frame::read::<serde_json::Value>(&mut bytes.as_slice()).unwrap_err();
    assert!(
        matches!(err, FrameError::TooLarge { len } if len == MAX_FRAME + 1),
        "{err}"
    );
}

#[test]
fn write_over_cap_is_refused() {
    let big = "x".repeat(MAX_FRAME as usize);
    let mut buf = Vec::new();
    let err = frame::write(&mut buf, &big).unwrap_err();
    assert!(matches!(err, FrameError::TooLarge { .. }), "{err}");
    assert!(buf.is_empty());
}

#[test]
fn truncated_frame_is_an_error() -> TestResult {
    let mut buf = Vec::new();
    frame::write(&mut buf, &json!({"a": 1}))?;
    buf.truncate(buf.len() - 1);
    let err = frame::read::<serde_json::Value>(&mut buf.as_slice()).unwrap_err();
    assert!(matches!(err, FrameError::Io(_)), "{err}");
    let err = frame::read::<serde_json::Value>(&mut [0u8, 0].as_slice()).unwrap_err();
    assert!(matches!(err, FrameError::Io(_)), "{err}");
    Ok(())
}

#[test]
fn bad_json_is_an_error() {
    let mut bytes = 2u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(b"{x");
    let err = frame::read::<serde_json::Value>(&mut bytes.as_slice()).unwrap_err();
    assert!(matches!(err, FrameError::Json(_)), "{err}");
}

#[test]
fn request_serializes_as_with_the_wire_name() -> TestResult {
    let mut req = Request::new(3, "console.snapshot", json!({"limit": 50}));
    assert_eq!(req.v, API);
    let plain = serde_json::to_value(&req)?;
    assert!(plain.get("as").is_none(), "{plain}");
    req.as_ = Some("idn7k2q9".parse()?);
    let value = serde_json::to_value(&req)?;
    assert_eq!(value["as"], "idn7k2q9");
    assert_eq!(value["v"], 1);
    assert_eq!(serde_json::from_value::<Request>(value)?, req);
    Ok(())
}

#[test]
fn null_result_is_a_success() -> TestResult {
    let wire = serde_json::to_value(Response::ok(4, serde_json::Value::Null))?;
    assert_eq!(wire, json!({"id": 4, "result": null}));
    let back: Response = serde_json::from_value(wire)?;
    assert_eq!(back, Response::ok(4, serde_json::Value::Null));
    Ok(())
}

#[test]
fn response_with_neither_key_is_an_error() {
    assert!(serde_json::from_value::<Response>(json!({"id": 5})).is_err());
}

#[test]
fn unknown_error_code_keeps_id_and_message() -> TestResult {
    let wire = json!({"id": 6, "error": {"code": "rate_limited", "message": "slow down"}});
    let back: Response = serde_json::from_value(wire)?;
    assert_eq!(back.id, 6);
    assert_eq!(back, Response::err(6, ErrorCode::Unknown, "slow down"));
    Ok(())
}

#[test]
fn response_shapes_and_error_codes() -> TestResult {
    let ok = serde_json::to_value(Response::ok(1, json!({"x": 2})))?;
    assert_eq!(ok, json!({"id": 1, "result": {"x": 2}}));
    let err = serde_json::to_value(Response::err(2, ErrorCode::NotFound, "no such project"))?;
    assert_eq!(
        err,
        json!({"id": 2, "error": {"code": "not_found", "message": "no such project"}})
    );
    for (code, text) in [
        (ErrorCode::Version, "version"),
        (ErrorCode::NotFound, "not_found"),
        (ErrorCode::Unauthorized, "unauthorized"),
        (ErrorCode::Hub, "hub"),
        (ErrorCode::BadRequest, "bad_request"),
    ] {
        assert_eq!(serde_json::to_value(code)?, json!(text));
    }
    let back: Response = serde_json::from_value(err)?;
    assert_eq!(
        back.outcome,
        Outcome::Err {
            error: ApiError {
                code: ErrorCode::NotFound,
                message: "no such project".into()
            }
        }
    );
    Ok(())
}

const ROSTER: &str = r#"[
  {"identity_id":"idn7k2q9","handle":"andy/sol","kind":"agent","status":"online","agent":"claude-code/2.1.310"},
  {"identity_id":"idn2m4w8","handle":"andy","kind":"human","status":"offline"}
]"#;

#[test]
fn roster_reads_the_hub_row() -> TestResult {
    let roster: Vec<RosterEntry> = serde_json::from_str(ROSTER)?;
    assert_eq!(roster.len(), 2);
    assert_eq!(roster[0].agent.as_deref(), Some("claude-code/2.1.310"));
    assert_eq!(roster[1].model, None);
    // Optional fields stay out of the output, as the hub leaves them out.
    assert_eq!(
        serde_json::to_value(&roster)?,
        serde_json::from_str::<serde_json::Value>(ROSTER)?
    );
    Ok(())
}

fn message() -> Result<agmen_proto::StampedEnvelope, serde_json::Error> {
    serde_json::from_value(json!({
        "v": 1, "id": "01JBX6Q4T7W3C2M8Y9K5N0P1RS", "verb": "DM", "data": {},
        "to": ["idn7k2q9"], "body": "hi", "sent_at": "2026-10-25T16:04:11Z",
        "stream": "feed", "seq": 1842, "from": "idn2m4w8", "from_handle": "andy",
        "from_kind": "human", "hub_at": "2026-10-25T16:04:11.402Z"
    }))
}

#[test]
fn console_snapshot_round_trips() -> TestResult {
    let snap = ConsoleSnapshot {
        project: "agmen".into(),
        me: Me {
            id: "idn2m4w8".parse()?,
            handle: "andy".into(),
        },
        roster: serde_json::from_str(ROSTER)?,
        messages: vec![message()?],
        older: true,
    };
    let back: ConsoleSnapshot = serde_json::from_value(serde_json::to_value(&snap)?)?;
    assert_eq!(back, snap);
    let ev = ConsoleEvent::Hub {
        connected: false,
        error: None,
    };
    assert_eq!(
        serde_json::to_value(&ev)?,
        json!({"hub": {"connected": false}})
    );
    assert_eq!(
        serde_json::from_value::<ConsoleEvent>(serde_json::to_value(&ev)?)?,
        ev
    );
    let ev = ConsoleEvent::Hub {
        connected: false,
        error: Some(ApiError {
            code: ErrorCode::Unauthorized,
            message: "not signed in".into(),
        }),
    };
    assert_eq!(
        serde_json::to_value(&ev)?,
        json!({"hub": {"connected": false,
                       "error": {"code": "unauthorized", "message": "not signed in"}}})
    );
    assert_eq!(
        serde_json::from_value::<ConsoleEvent>(serde_json::to_value(&ev)?)?,
        ev
    );
    let ev = ConsoleEvent::Message(message()?);
    assert_eq!(
        serde_json::from_value::<ConsoleEvent>(serde_json::to_value(&ev)?)?,
        ev
    );
    Ok(())
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn async_frames_round_trip_and_match_blocking() -> TestResult {
    let (mut a, mut b) = tokio::io::duplex(1024);
    frame::write_async(&mut a, &json!({"a": 1})).await?;
    let back: serde_json::Value = frame::read_async(&mut b).await?;
    assert_eq!(back, json!({"a": 1}));

    let mut blocking = Vec::new();
    frame::write(&mut blocking, &json!({"b": 2}))?;
    tokio::io::AsyncWriteExt::write_all(&mut a, &blocking).await?;
    assert_eq!(
        frame::read_async::<serde_json::Value>(&mut b).await?,
        json!({"b": 2})
    );

    let bytes = (MAX_FRAME + 1).to_be_bytes();
    tokio::io::AsyncWriteExt::write_all(&mut a, &bytes).await?;
    let err = frame::read_async::<serde_json::Value>(&mut b)
        .await
        .unwrap_err();
    assert!(matches!(err, FrameError::TooLarge { .. }), "{err}");
    Ok(())
}
