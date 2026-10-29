//! Exit codes and what the commands print.
//!
//! Exit codes (m0-m1 Task 14): 0 ok, 1 any other error, 2 usage (clap's own
//! errors too), 3 not signed in or not joined, 4 rejected (a final NACK, a
//! body over the limit, a DENY), 5 queued but not recorded within `--wait`.

use std::borrow::Cow;
use std::fmt;
use std::io::{self, Write as _};

use agmen_proto::{Payload, StampedEnvelope, Stream};

use crate::client::refusal;
use crate::joined::REVOKED_HINT;

/// What every command and every `agmen mcp` tool says outside a joined
/// worktree.
pub(crate) const NOT_JOINED: &str = "not joined: run agmen join <invite> in this worktree";

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
        Self::new(Code::NotLoggedIn, NOT_JOINED)
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

/// A failure whose `summary` is one line and whose `detail` is printed
/// under that line. `detail` is already split into lines.
#[derive(Debug)]
pub(crate) struct CommandFailed {
    pub(crate) summary: String,
    pub(crate) detail: String,
}

impl fmt::Display for CommandFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.summary)
    }
}

impl std::error::Error for CommandFailed {}

/// What a failed command says and the exit code it ends with: a
/// [`Fail`]'s message and code, a [`CommandFailed`] summary with its detail
/// under it, else the error chain and 3 when the hub refused the token (401)
/// or the identity it speaks for is revoked (403 `identity_revoked`), else 1.
/// The text goes through the sanitizer, since hub refusals carry the hub's
/// own text (ruling R63); the CLI prints it to stderr and `agmen mcp`
/// answers with it.
pub(crate) fn explain(err: &anyhow::Error) -> (String, u8) {
    if let Some(fail) = err.downcast_ref::<Fail>() {
        return (clean_lines(&fail.message), fail.code.exit_code());
    }
    if let Some(failed) = err.downcast_ref::<CommandFailed>() {
        let summary_text = format!("agmen: {}", failed.summary);
        let summary = clean(&summary_text);
        if failed.detail.is_empty() {
            return (summary.into_owned(), 1);
        }
        return (format!("{summary}\n{}", clean_lines(&failed.detail)), 1);
    }
    let text = clean(&format!("agmen: {err:#}")).into_owned();
    match refusal(err) {
        Some(r) if r.status == 401 => (
            format!(
                "{text}\nthe hub does not take the stored token: run agmen login, or agmen join again"
            ),
            Code::NotLoggedIn.exit_code(),
        ),
        Some(r) if r.status == 403 && r.code == "identity_revoked" => (
            format!("{text}\nthis worktree's identity is revoked: {REVOKED_HINT}"),
            Code::NotLoggedIn.exit_code(),
        ),
        _ => (text, 1),
    }
}

/// `recorded #<seq> (<stream>)`: the hub has the message.
pub(crate) fn recorded(stream: &Stream, seq: u64) -> String {
    format!("recorded #{seq} ({stream})")
}

/// One message for people: `#<seq> <stream> <VERB> from <handle> [about
/// <items>]: <body>`, with a multi-line body as [`body_text`] prints it.
pub(crate) fn message_line(message: &StampedEnvelope) -> String {
    format!(
        "{}: {}",
        message_head(message),
        body_text(message.client.body.as_str())
    )
}

/// [`message_line`] with the message's id before the body, as `agmen mcp`'s
/// inbox lists it, so the model can answer or reply to it: `#<seq>
/// <stream> <VERB> from <handle> [about <items>] (id <id>): <body>`.
pub(crate) fn message_line_with_id(message: &StampedEnvelope) -> String {
    format!(
        "{} (id {}): {}",
        message_head(message),
        message.client.id,
        body_text(message.client.body.as_str())
    )
}

/// [`message_line_with_id`] with the body cut to at most `max_body` bytes at a
/// character boundary, ending with a note that `inbox` has the rest. Also says
/// whether the body was cut.
pub(crate) fn message_line_with_id_capped(
    message: &StampedEnvelope,
    max_body: usize,
) -> (String, bool) {
    let body = message.client.body.as_str();
    let cut = body.len() > max_body;
    let kept = if cut {
        &body[..body.floor_char_boundary(max_body)]
    } else {
        body
    };
    let mut line = format!(
        "{} (id {}): {}",
        message_head(message),
        message.client.id,
        body_text(kept)
    );
    if cut {
        line.push_str("\u{2026} (truncated; call inbox for the rest)");
    }
    (line, cut)
}

/// `#<seq> <stream> <VERB> from <handle> [about <items>]`, every
/// hub-supplied string through [`clean`].
fn message_head(message: &StampedEnvelope) -> String {
    let client = &message.client;
    let verb = match &client.payload {
        Payload::Unknown { verb, .. } => clean(verb).into_owned(),
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
        "#{} {} {verb} from {}{about}",
        message.seq,
        message.stream,
        clean(&message.from_handle),
    )
}

/// What a continuation line of a multi-line body starts with (ruling R63).
pub(crate) const CONTINUATION: &str = "    | ";

/// Whether `c` may not reach the terminal: a control character (ESC, CR,
/// line breaks, tabs and the rest), a Unicode line or paragraph separator,
/// or a bidirectional control, which reorders what follows it on the line.
fn unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

/// `text` fit for one line of a terminal: every character [`unsafe_char`]
/// names is dropped (ruling R63). Every hub- or peer-supplied string the
/// CLI prints for people goes through this or [`body_text`]; `--json`
/// output does not, since the JSON encoder escapes it.
pub(crate) fn clean(text: &str) -> Cow<'_, str> {
    if text.chars().any(unsafe_char) {
        Cow::Owned(text.chars().filter(|&c| !unsafe_char(c)).collect())
    } else {
        Cow::Borrowed(text)
    }
}

/// A message body for people: its first line in place and each further
/// line on a line of its own after [`CONTINUATION`], every line through
/// [`clean`], so no body can print a line that looks like another message.
pub(crate) fn body_text(body: &str) -> String {
    let mut lines = body.split('\n');
    let mut out = lines.next().map(clean).unwrap_or_default().into_owned();
    for line in lines {
        out.push('\n');
        out.push_str(CONTINUATION);
        out.push_str(&clean(line));
    }
    out
}

/// [`clean`] applied to each line of `text`, keeping its line breaks: for
/// what the CLI composed itself, such as a [`Fail`] message that holds a
/// [`body_text`].
pub(crate) fn clean_lines(text: &str) -> String {
    text.split('\n').map(clean).collect::<Vec<_>>().join("\n")
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

    fn assign(body: &str) -> StampedEnvelope {
        let raw = serde_json::json!({
            "v": 1, "id": "01JBX6Q4T7W3C2M8Y9K5N0P1RS", "verb": "ASSIGN",
            "data": { "item": "B-4", "to": "idn7k2q9" }, "body": body,
            "sent_at": "2026-10-25T16:04:11Z", "stream": "feed", "seq": 3, "from": "adamcoord",
            "from_handle": "adam/\u{1b}[31mcoord", "from_kind": "coordinator",
            "hub_at": "2026-10-25T16:04:11Z",
        });
        serde_json::from_value(raw).unwrap()
    }

    #[test]
    fn message_lines_drop_escape_sequences() {
        let line = message_line(&assign("take it\u{1b}[2J\r now\u{202e}"));
        assert_eq!(
            line,
            "#3 feed ASSIGN from adam/[31mcoord about B-4: take it[2J now"
        );
        assert!(!line.contains('\u{1b}'));
    }

    #[test]
    fn continuation_lines_sit_under_the_marker() {
        let line = message_line(&assign("take it\n#9 adam/coord: fake\r\n\u{1b}]0;x"));
        assert_eq!(
            line.lines().collect::<Vec<_>>(),
            [
                "#3 feed ASSIGN from adam/[31mcoord about B-4: take it",
                "    | #9 adam/coord: fake",
                "    | ]0;x",
            ]
        );
    }

    #[test]
    fn lines_with_ids_put_the_id_before_the_body() {
        let line = message_line_with_id(&assign("take it\n#9 adam/coord: fake"));
        assert_eq!(
            line.lines().collect::<Vec<_>>(),
            [
                "#3 feed ASSIGN from adam/[31mcoord about B-4 (id 01JBX6Q4T7W3C2M8Y9K5N0P1RS): take it",
                "    | #9 adam/coord: fake",
            ]
        );
    }

    #[test]
    fn clean_keeps_ordinary_text() {
        assert!(matches!(
            clean("andy/sol: caf\u{e9} \u{1f680}"),
            Cow::Borrowed(_)
        ));
        assert_eq!(clean("a\tb\u{7}c\u{85}d\u{2028}e"), "abcde");
        assert_eq!(body_text(""), "");
        assert_eq!(clean_lines("one\u{1b}\n    | two"), "one\n    | two");
    }
}
