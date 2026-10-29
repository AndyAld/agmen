//! Stream-json listener batch encoding.

use agmen_proto::{IdentityKind, MessageId, StampedEnvelope, Verb};
use serde::Serialize;
use serde_json::{Value, json};

use super::queue::Batch;

pub(super) const MAX_BATCH_BYTES: usize = 64 * 1024;

#[derive(Debug, Serialize)]
pub(super) struct BatchLine {
    id: MessageId,
    seq: u64,
    from_handle: String,
    from_kind: IdentityKind,
    verb: Verb,
    body: String,
    reply_to: Option<MessageId>,
}

impl TryFrom<&StampedEnvelope> for BatchLine {
    type Error = BatchError;

    fn try_from(message: &StampedEnvelope) -> Result<Self, BatchError> {
        let client = &message.client;
        // A wire verb this build does not know cannot be shown as a protocol `Verb`.
        let verb = client.payload.verb().ok_or(BatchError::UnknownVerb)?;
        let body = if client.body.as_str().is_empty() {
            serde_json::to_string(&client.payload)?
        } else {
            client.body.as_str().to_owned()
        };
        Ok(Self {
            id: client.id.clone(),
            seq: message.seq.0,
            from_handle: message.from_handle.clone(),
            from_kind: message.from_kind,
            verb,
            body,
            reply_to: client.reply_to.clone(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum BatchError {
    #[error("encoded listener batch exceeds 64 KiB")]
    TooLarge,
    #[error("listener batch contains an unknown verb")]
    UnknownVerb,
    #[error("encoding listener batch: {0}")]
    Json(#[from] serde_json::Error),
}

pub(super) fn encode(batch: &Batch, context: &Value) -> Result<Vec<u8>, BatchError> {
    #[derive(Serialize)]
    struct Content<'a> {
        batch_id: u64,
        context: &'a Value,
        events: Vec<BatchLine>,
    }
    let events = batch
        .messages
        .iter()
        .map(BatchLine::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let content = serde_json::to_string(&Content {
        batch_id: batch.batch_id.get(),
        context,
        events,
    })?;
    let mut bytes = serde_json::to_vec(&json!({
        "type": "user", "message": {"role": "user", "content": content}
    }))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_BATCH_BYTES {
        return Err(BatchError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "batch_tests.rs"]
mod tests;
