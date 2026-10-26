//! Exit codes and what the commands print.
//!
//! Exit codes (m0-m1 Task 14): 0 ok, 1 any other error, 2 usage (clap's own
//! errors too), 3 not signed in or not joined, 4 rejected (a final NACK, a
//! body over the limit, a DENY), 5 queued but not recorded within `--wait`.

use std::fmt;
use std::io::{self, Write as _};

use agmen_proto::{Payload, StampedEnvelope, Stream};

/// A failure with its own exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Code {
    /// 2: the arguments are wrong, or name a handle nobody has.
    Usage,
    /// 3: not signed in, or this worktree has not joined.
    NotLoggedIn,
    /// 4: the hub refused for good.
    Rejected,
    /// 5: queued, but the hub did not answer within `--wait`.
    Queued,
}

impl Code {
    /// The process exit code.
    pub(crate) const fn exit_code(self) -> u8 {
        match self {
            Self::Usage => 2,
            Self::NotLoggedIn => 3,
            Self::Rejected => 4,
            Self::Queued => 5,
        }
    }
}

/// An error that ends the command with `code`; `main` prints its message
/// to stderr as is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fail {
    pub(crate) code: Code,
    pub(crate) message: String,
}

impl Fail {
    pub(crate) fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn usage(message: impl Into<String>) -> Self {
        Self::new(Code::Usage, message)
    }

    pub(crate) fn not_signed_in() -> Self {
        Self::new(
            Code::NotLoggedIn,
            "not signed in: run agmen login --hub <url>",
        )
    }

    pub(crate) fn not_joined() -> Self {
        Self::new(
            Code::NotLoggedIn,
            "not joined: run agmen join <invite> in this worktree",
        )
    }

    pub(crate) fn rejected(reason: impl fmt::Display) -> Self {
        Self::new(Code::Rejected, format!("rejected: {reason}"))
    }
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Fail {}

/// `recorded #<seq> (<stream>)`: the hub has the message.
pub(crate) fn recorded(stream: &Stream, seq: u64) -> String {
    format!("recorded #{seq} ({stream})")
}

/// One message on one line for people: `#<seq> <stream> <VERB> from
/// <handle> [about <items>]: <body>`.
pub(crate) fn message_line(message: &StampedEnvelope) -> String {
    let client = &message.client;
    let verb = match &client.payload {
        Payload::Unknown { verb, .. } => verb.clone(),
        payload => payload
            .verb()
            .map_or_else(String::new, |verb| verb.to_string()),
    };
    let mut items: Vec<String> = client.items.iter().map(ToString::to_string).collect();
    match &client.payload {
        Payload::Claim { item }
        | Payload::Grant { item, .. }
        | Payload::Deny { item, .. }
        | Payload::Release { item, .. }
        | Payload::Assign { item, .. }
        | Payload::Done { item, .. } => items.push(item.to_string()),
        Payload::Item { key } => items.push(key.to_string()),
        _ => {}
    }
    items.dedup();
    let about = if items.is_empty() {
        String::new()
    } else {
        format!(" about {}", items.join(", "))
    };
    format!(
        "#{} {} {verb} from {}{about}: {}",
        message.seq,
        message.stream,
        printable(&message.from_handle),
        printable(client.body.as_str())
    )
}

/// `text` with control characters other than line breaks and tabs
/// replaced, so a message cannot send escape sequences to the terminal.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect()
}

/// Writes `line` and a newline to stdout and flushes, so a reader on a
/// pipe (the plugin) gets each line as it is written.
///
/// # Errors
///
/// When stdout is closed.
pub(crate) fn println_flush(line: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_lines_drop_escape_sequences() {
        let message: StampedEnvelope = serde_json::from_str(
            r#"{"v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"ASSIGN",
            "data":{"item":"B-4","to":"idn7k2q9"},"body":"take it\u001b[2J\nnow",
            "sent_at":"2026-10-25T16:04:11Z","stream":"feed","seq":3,"from":"adamcoord",
            "from_handle":"adam/coord","from_kind":"coordinator","hub_at":"2026-10-25T16:04:11Z"}"#,
        )
        .unwrap();
        assert_eq!(
            message_line(&message),
            "#3 feed ASSIGN from adam/coord about B-4: take it\u{fffd}[2J\nnow"
        );
    }
}
