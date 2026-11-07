//! Local identity watches page the record after each stored notification.

use std::collections::VecDeque;

use agmen_client::{ClientEvent, Me};
use agmen_local::{ApiError, Response, WATCH_KEEPALIVE};
use agmen_proto::Stream;
use serde_json::json;
use tokio::io::AsyncReadExt as _;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use super::{
    AUTHORITY_RECHECK, COLLECTION_PAGE_SIZE, Caller, DEADLINE, Identities, ProjectParams,
    bad_request, forbidden, hub_error, still_managed, unauthorized, write_event, write_watch_frame,
};
use crate::origin::Origin;

impl Identities {
    pub(super) async fn watch_local(
        &self,
        caller: &Caller,
        id: u64,
        params: ProjectParams,
        me: &Me,
        mut reader: OwnedReadHalf,
        writer: &mut OwnedWriteHalf,
    ) -> Result<(), ApiError> {
        let streams = [Stream::Feed, Stream::Inbox(me.identity_id.clone())];
        let resume = params.after.is_some();
        let mut positions = params.after.unwrap_or_default();
        for (stream, seq) in &positions {
            if !streams.contains(stream) {
                return Err(forbidden());
            }
            i64::try_from(*seq).map_err(|_| bad_request("after exceeds the storage range"))?;
        }
        let local = self.origins.local();
        // Subscribe before reading heads or pages so commits during setup and
        // replay remain visible even if no later message wakes the watch.
        let mut stored = local.watch_stored(&params.project).await?;
        for stream in &streams {
            let after = if resume {
                positions.get(stream).copied().unwrap_or(0)
            } else {
                local.page(&params.project, stream, 0, 1).await?.head
            };
            positions.insert(stream.clone(), after);
        }
        write_event(writer, id, &ClientEvent::Me(me.clone())).await?;
        let mut pending = VecDeque::from(streams.clone());
        let mut authority = tokio::time::interval(AUTHORITY_RECHECK);
        let mut last_frame = tokio::time::Instant::now();
        let mut disconnected = [0; 1];
        loop {
            tokio::select! {
                _ = reader.read(&mut disconnected) => return Ok(()),
                changed = stored.changed() => {
                    changed.map_err(|_| hub_error("local record notification channel closed"))?;
                    for stream in &streams {
                        if !pending.contains(stream) {
                            pending.push_back(stream.clone());
                        }
                    }
                }
                _ = std::future::ready(()), if !pending.is_empty() => {
                    if let Some(stream) = pending.pop_front() {
                        let after = positions.get(&stream).copied().unwrap_or(0);
                        let page = local.page(&params.project, &stream, after, COLLECTION_PAGE_SIZE).await?;
                        for row in page.messages {
                            let last = positions.get(&stream).copied().unwrap_or(0);
                            if row.stream != stream || row.seq.0 != last.saturating_add(1) {
                                return Err(hub_error("noncontiguous local watch replay"));
                            }
                            let seq = row.seq.0;
                            write_event(writer, id, &ClientEvent::Message { message: Box::new(row) }).await?;
                            positions.insert(stream.clone(), seq);
                            last_frame = tokio::time::Instant::now();
                        }
                        if page.next_after.is_some() {
                            pending.push_back(stream);
                        }
                    }
                }
                _ = tokio::time::sleep_until(last_frame + WATCH_KEEPALIVE) => {
                    // Older readers already understand connection events and
                    // ignore the added field. Current readers skip this frame.
                    write_watch_frame(writer, &Response::ok(id, json!({
                        "kind":"connection", "connected":true, "keepalive":true
                    }))).await?;
                    last_frame = tokio::time::Instant::now();
                }
                _ = authority.tick() => {
                    let valid = tokio::time::timeout(DEADLINE, async {
                        match caller {
                            Caller::Identity(identity) => still_managed(&self.origins, self.store.clone(), identity).await,
                            Caller::Human => self.origins.person(Origin::Local).await.is_ok_and(|login| login.human_id == me.identity_id),
                        }
                    }).await;
                    if !matches!(valid, Ok(true)) {
                        return Err(unauthorized());
                    }
                }
            }
        }
    }
}
