//! A server-sent events parser over the bytes of one response (the WHATWG
//! event stream format, the parts the hub uses): `id:` and `data:` fields,
//! `:` comments (the hub's keep-alive), events ended by a blank line. Other
//! fields (`event:`, `retry:`) are ignored.
//!
//! Bounds: a line longer than [`MAX_LINE`] or an event whose data passes
//! [`MAX_EVENT`] is an error, and the caller drops the connection. The hub
//! caps an envelope at 64 KiB, so a stamped one fits well inside both.

/// The longest line kept, in bytes.
pub(crate) const MAX_LINE: usize = 256 * 1024;
/// The most data one event may carry, in bytes.
pub(crate) const MAX_EVENT: usize = 256 * 1024;

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    /// The `id:` field, when the event had one.
    pub(crate) id: Option<String>,
    /// The `data:` lines, joined by `\n`.
    pub(crate) data: String,
}

/// The stream broke a bound or is not UTF-8.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SseError {
    /// A line passed [`MAX_LINE`].
    #[error("an event stream line is over {MAX_LINE} bytes")]
    LineTooLong,
    /// An event's data passed [`MAX_EVENT`].
    #[error("an event is over {MAX_EVENT} bytes")]
    EventTooLarge,
    /// A line is not UTF-8.
    #[error("an event stream line is not UTF-8")]
    NotUtf8,
}

/// Parser state between chunks.
#[derive(Debug, Default)]
pub(crate) struct SseParser {
    line: Vec<u8>,
    /// The last byte was `\r`, so a `\n` next ends nothing new.
    after_cr: bool,
    id: Option<String>,
    data: String,
    has_data: bool,
}

impl SseParser {
    /// Feeds `chunk` and returns the events it completes, in order.
    ///
    /// # Errors
    ///
    /// [`SseError`] when a bound is broken; the parser is then unusable.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        let mut events = Vec::new();
        for &byte in chunk {
            let after_cr = std::mem::replace(&mut self.after_cr, false);
            match byte {
                b'\n' if after_cr => {}
                b'\n' | b'\r' => {
                    self.after_cr = byte == b'\r';
                    if let Some(event) = self.end_line()? {
                        events.push(event);
                    }
                }
                _ => {
                    if self.line.len() >= MAX_LINE {
                        return Err(SseError::LineTooLong);
                    }
                    self.line.push(byte);
                }
            }
        }
        Ok(events)
    }

    /// Handles the line in `self.line`; a blank one dispatches the event.
    fn end_line(&mut self) -> Result<Option<SseEvent>, SseError> {
        let raw = std::mem::take(&mut self.line);
        if raw.is_empty() {
            let data = std::mem::take(&mut self.data);
            let id = self.id.take();
            if !std::mem::take(&mut self.has_data) {
                return Ok(None);
            }
            return Ok(Some(SseEvent { id, data }));
        }
        let line = std::str::from_utf8(&raw).map_err(|_| SseError::NotUtf8)?;
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                if self.data.len() + value.len() + 1 > MAX_EVENT {
                    return Err(SseError::EventTooLarge);
                }
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "id" => self.id = Some(value.to_owned()),
            _ => {}
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, data: &str) -> SseEvent {
        SseEvent {
            id: Some(id.to_owned()),
            data: data.to_owned(),
        }
    }

    #[test]
    fn parses_events_split_across_chunks() {
        let mut parser = SseParser::default();
        assert_eq!(parser.push(b"id: feed:1\nda").unwrap(), vec![]);
        assert_eq!(
            parser.push(b"ta: {\"a\":1}\n\n:keep-alive\n\n").unwrap(),
            vec![event("feed:1", "{\"a\":1}")]
        );
        assert_eq!(
            parser
                .push(b"id:inbox:idn7k2q9:4\r\ndata:x\r\ndata: y\r\n\r\n")
                .unwrap(),
            vec![event("inbox:idn7k2q9:4", "x\ny")]
        );
        // A bare `\r` ends a line too.
        assert_eq!(
            parser.push(b"event: other\rdata: z\r\r").unwrap(),
            vec![SseEvent {
                id: None,
                data: "z".to_owned()
            }]
        );
    }

    #[test]
    fn comments_and_blank_lines_dispatch_nothing() {
        let mut parser = SseParser::default();
        assert_eq!(parser.push(b": hi\n\n\n\nid: 3\n\n").unwrap(), vec![]);
    }

    #[test]
    fn bounds_are_enforced() {
        let mut parser = SseParser::default();
        let long = vec![b'a'; MAX_LINE + 1];
        assert_eq!(parser.push(&long), Err(SseError::LineTooLong));

        let mut parser = SseParser::default();
        let half = "d".repeat(MAX_EVENT / 2 + 1);
        let chunk = format!("data: {half}\ndata: {half}\n");
        assert_eq!(parser.push(chunk.as_bytes()), Err(SseError::EventTooLarge));

        let mut parser = SseParser::default();
        assert_eq!(parser.push(b"data: \xff\n"), Err(SseError::NotUtf8));
    }
}
