//! `GET /v1/projects/{p}/events`: a member's chat as server-sent events,
//! against an in-process hub and a database of the test's own on
//! `AGMEN_IT_PG`. The hub replays what the reader has not seen, then goes
//! live, with event ids `<stream>:<seq>` (protocol section 6).

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use agmen_hub::HubPost;
use agmen_proto::{Needs, Payload, Stream, Urgency};
use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{
    Ctx, Project, envelope, login, mint_ok, run, seed_human, seed_member, seed_project, topics,
    ulid,
};

const ANDY: u64 = 4242;
const CAROL: u64 = 77;

/// How long a test waits for an event it expects.
const EVENT_WAIT: Duration = Duration::from_secs(10);
/// How long a test watches for an event it does not expect.
const QUIET: Duration = Duration::from_millis(2500);

/// One server-sent event: its id and its data as JSON.
#[derive(Debug, Clone)]
struct Event {
    id: String,
    data: Value,
}

/// An open events stream and the bytes read from it that do not yet make a
/// whole event.
struct Events {
    resp: reqwest::Response,
    buf: String,
}

impl Events {
    /// The next event, skipping keep-alive comments; `None` when nothing
    /// came within `wait`.
    async fn next_within(&mut self, wait: Duration) -> Option<Event> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            while let Some(end) = self.buf.find("\n\n") {
                let block: String = self.buf.drain(..end + 2).collect();
                if let Some(event) = parse_block(&block) {
                    return Some(event);
                }
            }
            let chunk = tokio::time::timeout_at(deadline, self.resp.chunk()).await;
            match chunk {
                Err(_) => return None,
                Ok(chunk) => {
                    let chunk = chunk.unwrap().expect("the events stream ended");
                    self.buf.push_str(std::str::from_utf8(&chunk).unwrap());
                }
            }
        }
    }

    /// Whether the hub ends the stream within `wait`, ignoring anything
    /// sent before the end.
    async fn ends_within(&mut self, wait: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            match tokio::time::timeout_at(deadline, self.resp.chunk()).await {
                Err(_) => return false,
                Ok(Ok(None) | Err(_)) => return true,
                Ok(Ok(Some(_))) => {}
            }
        }
    }

    async fn next(&mut self) -> Event {
        self.next_within(EVENT_WAIT)
            .await
            .expect("no event within the wait")
    }

    /// Events until every id in `want` has been seen, with any others that
    /// came first.
    async fn until(&mut self, want: &BTreeSet<String>) -> Vec<Event> {
        let mut got = Vec::new();
        let mut missing = want.clone();
        while !missing.is_empty() {
            let event = self.next().await;
            missing.remove(&event.id);
            got.push(event);
        }
        got
    }

    /// The events that arrive within [`QUIET`].
    async fn quiet(&mut self) -> Vec<Event> {
        let mut got = Vec::new();
        let deadline = tokio::time::Instant::now() + QUIET;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match self.next_within(left).await {
                Some(event) => got.push(event),
                None => return got,
            }
        }
    }
}

/// An event block's id and data, or `None` for a comment-only block.
fn parse_block(block: &str) -> Option<Event> {
    let mut id = None;
    let mut data = String::new();
    for line in block.lines() {
        if let Some(value) = line.strip_prefix("id:") {
            id = Some(value.trim_start().to_owned());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    let id = id?;
    Some(Event {
        id,
        data: serde_json::from_str(&data).unwrap(),
    })
}

/// `GET /v1/projects/{project}/events` with `token`, an optional `after`
/// query and an optional `Last-Event-ID`.
async fn open(
    ctx: &Ctx,
    token: &str,
    project: &str,
    after: Option<&str>,
    last: Option<&str>,
) -> reqwest::Response {
    let mut url = ctx.hub.url(&format!("/v1/projects/{project}/events"));
    if let Some(after) = after {
        url.query_pairs_mut().append_pair("after", after);
    }
    let mut req = ctx.http.get(url).bearer_auth(token);
    if let Some(last) = last {
        req = req.header("last-event-id", last);
    }
    req.send().await.unwrap()
}

async fn events(
    ctx: &Ctx,
    token: &str,
    project: &str,
    after: Option<&str>,
    last: Option<&str>,
) -> Events {
    let resp = open(ctx, token, project, after, last).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp.headers()["content-type"].to_str().unwrap().to_owned();
    assert!(
        content_type.starts_with("text/event-stream"),
        "{content_type}"
    );
    Events {
        resp,
        buf: String::new(),
    }
}

/// Posts an FYI to the feed as the holder of `token`; returns its id.
async fn say(ctx: &Ctx, token: &str, project: &str, body: &str) -> String {
    let id = ulid();
    let resp = ctx
        .http
        .post(ctx.hub.url(&format!("/v1/projects/{project}/messages")))
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(envelope(&id, "FYI", json!({}), json!({ "body": body })))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    id
}

/// Posts an ALERT as the hub to the inbox of `to` only.
async fn to_inbox(ctx: &Ctx, project: &Project, to: &str) {
    ctx.hub
        .hub
        .post_as_hub(
            &topics(project),
            HubPost {
                payload: Payload::Alert {
                    need: "look".to_owned(),
                    urgency: Urgency::Normal,
                    needs: Needs::Anyone,
                    actions: Vec::new(),
                },
                body: "for you".try_into().unwrap(),
                reply_to: None,
                items: Vec::new(),
                lane: None,
                mentioned: Vec::new(),
                streams: vec![Stream::Inbox(to.parse().unwrap())],
            },
        )
        .await
        .unwrap();
}

/// A project where andy is a member and has a token.
async fn room(ctx: &Ctx) -> (Project, String, String) {
    let project = seed_project(&ctx.pool).await;
    let andy = seed_human(&ctx.pool, ANDY, "andy").await;
    seed_member(&ctx.pool, &andy, &project, "admin").await;
    let token = login(ctx, ANDY, "andy").await;
    (project, andy, token)
}

/// The seqs seen per stream, checking that each event's id matches its
/// data and that no `(stream, seq)` came twice.
fn by_stream(events: &[Event]) -> BTreeMap<String, Vec<u64>> {
    let mut seen = BTreeSet::new();
    let mut streams: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for event in events {
        let stream = event.data["stream"].as_str().unwrap().to_owned();
        let seq = event.data["seq"].as_u64().unwrap();
        assert_eq!(event.id, format!("{stream}:{seq}"), "{:?}", event.data);
        assert!(seen.insert((stream.clone(), seq)), "{stream}:{seq} twice");
        streams.entry(stream).or_default().push(seq);
    }
    streams
}

fn seqs(range: std::ops::RangeInclusive<u64>) -> Vec<u64> {
    range.collect()
}

#[tokio::test]
async fn it_events_stream_live_messages() {
    run(|ctx| async move {
        let (project, andy, token) = room(&ctx).await;
        let p = project.project.as_str();
        let first = say(&ctx, &token, p, "before anyone listens").await;

        let mut stream = events(&ctx, &token, p, None, None).await;
        // What was there before the connection is replayed first.
        let replayed = stream.next().await;
        assert_eq!(replayed.id, "feed:1");
        assert_eq!(replayed.data["id"], first);
        assert_eq!(replayed.data["from"], andy);
        assert_eq!(replayed.data["from_kind"], "human");

        // A post after the connection arrives live.
        let second = say(&ctx, &token, p, "live").await;
        let live = stream.next().await;
        assert_eq!(live.id, "feed:2");
        assert_eq!(live.data["id"], second);

        // Posts racing a fresh connection's replay: each arrives once, in
        // order, with no gap where the replay ends and live delivery starts.
        let poster = {
            let (http, url, token) = (
                ctx.http.clone(),
                ctx.hub.url(&format!("/v1/projects/{p}/messages")),
                token.clone(),
            );
            tokio::spawn(async move {
                for n in 0..20 {
                    let body =
                        envelope(&ulid(), "FYI", json!({}), json!({ "body": format!("{n}") }));
                    let resp = http
                        .post(url.clone())
                        .bearer_auth(&token)
                        .header("content-type", "application/json")
                        .body(body)
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(resp.status(), StatusCode::OK);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
        };
        let mut racing = events(&ctx, &token, p, None, None).await;
        let got = racing.until(&BTreeSet::from(["feed:22".to_owned()])).await;
        poster.await.unwrap();
        let mut got = got;
        got.extend(racing.quiet().await);
        assert_eq!(by_stream(&got)["feed"], seqs(1..=22));
    })
    .await;
}

#[tokio::test]
async fn it_events_need_a_member_with_an_access_token() {
    run(|ctx| async move {
        let (project, _, token) = room(&ctx).await;
        let p = project.project.as_str();

        let other = seed_project(&ctx.pool).await;
        let carol = seed_human(&ctx.pool, CAROL, "carol").await;
        seed_member(&ctx.pool, &carol, &other, "member").await;
        let carol_token = login(&ctx, CAROL, "carol").await;
        let resp = open(&ctx, &carol_token, p, None, None).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "not_a_member");

        let sol = mint_ok(&ctx, &token, "andy/sol", &project).await;
        let resp = open(&ctx, &sol.token, p, None, None).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "human_token_required");

        let resp = ctx
            .http
            .get(ctx.hub.url(&format!("/v1/projects/{p}/events")))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        for (after, last) in [
            (None, Some("feed")),
            (None, Some("feed:x")),
            (None, Some("dm:1")),
            (Some("feed:1;inbox:abc"), None),
            (Some("feed:-1"), None),
        ] {
            let resp = open(&ctx, &token, p, after, last).await;
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{after:?} {last:?}");
            let body: Value = resp.json().await.unwrap();
            assert_eq!(body["error"], "invalid_request");
        }
    })
    .await;
}

#[tokio::test]
async fn it_events_resume_after_last_event_id() {
    run(|ctx| async move {
        let (project, _, token) = room(&ctx).await;
        let p = project.project.as_str();
        let sol = mint_ok(&ctx, &token, "andy/sol", &project).await;
        let inbox = format!("inbox:{}", sol.id);
        for n in 0..3 {
            say(&ctx, &token, p, &format!("feed {n}")).await;
        }
        for _ in 0..2 {
            to_inbox(&ctx, &project, &sol.id).await;
        }

        let mut first = events(&ctx, &token, p, None, None).await;
        let want = BTreeSet::from(["feed:3".to_owned(), format!("{inbox}:2")]);
        let mut seen = first.until(&want).await;
        seen.extend(first.quiet().await);
        let streams = by_stream(&seen);
        assert_eq!(streams["feed"], seqs(1..=3));
        assert_eq!(streams[&inbox], seqs(1..=2));
        let last = seen.last().unwrap().id.clone();
        drop(first);

        // Messages on both streams while the reader is away.
        for n in 0..2 {
            say(&ctx, &token, p, &format!("away {n}")).await;
            to_inbox(&ctx, &project, &sol.id).await;
        }

        // The reader resumes with its position on every stream. Its
        // `after` is one behind on the stream of `Last-Event-ID`, as after
        // a browser reconnect that keeps the original URL: the later of the
        // two positions wins.
        let (feed_at, inbox_at) = if last.starts_with("feed:") {
            (2, 2)
        } else {
            (3, 1)
        };
        let after = format!("feed:{feed_at},{inbox}:{inbox_at}");
        let mut second = events(&ctx, &token, p, Some(&after), Some(&last)).await;
        let want = BTreeSet::from(["feed:5".to_owned(), format!("{inbox}:4")]);
        let mut resumed = second.until(&want).await;
        resumed.extend(second.quiet().await);
        let streams = by_stream(&resumed);
        assert_eq!(streams["feed"], seqs(4..=5), "{resumed:?}");
        assert_eq!(streams[&inbox], seqs(3..=4), "{resumed:?}");

        // Live again on both streams after the replay.
        say(&ctx, &token, p, "back").await;
        to_inbox(&ctx, &project, &sol.id).await;
        let want = BTreeSet::from(["feed:6".to_owned(), format!("{inbox}:5")]);
        let mut live = second.until(&want).await;
        live.extend(second.quiet().await);
        let streams = by_stream(&live);
        assert_eq!(streams["feed"], vec![6]);
        assert_eq!(streams[&inbox], vec![5]);

        // Across both connections, every stream came through whole, once.
        let mut all = seen;
        all.extend(resumed);
        all.extend(live);
        let streams = by_stream(&all);
        assert_eq!(streams["feed"], seqs(1..=6));
        assert_eq!(streams[&inbox], seqs(1..=5));
    })
    .await;
}

#[tokio::test]
async fn it_events_cover_owned_inboxes_only() {
    run(|ctx| async move {
        let (project, andy, token) = room(&ctx).await;
        let p = project.project.as_str();
        let sol = mint_ok(&ctx, &token, "andy/sol", &project).await;
        let carol = seed_human(&ctx.pool, CAROL, "carol").await;
        seed_member(&ctx.pool, &carol, &project, "member").await;
        let carol_token = login(&ctx, CAROL, "carol").await;
        let kit = mint_ok(&ctx, &carol_token, "carol/kit", &project).await;

        to_inbox(&ctx, &project, &kit.id).await;
        to_inbox(&ctx, &project, &carol).await;
        to_inbox(&ctx, &project, &andy).await;
        to_inbox(&ctx, &project, &sol.id).await;
        say(&ctx, &carol_token, p, "hello all").await;

        // Naming someone else's inbox in `after` does not open it.
        let after = format!("inbox:{}:0,inbox:{carol}:0", kit.id);
        let mut stream = events(&ctx, &token, p, Some(&after), None).await;
        let want = BTreeSet::from([
            "feed:1".to_owned(),
            format!("inbox:{andy}:1"),
            format!("inbox:{}:1", sol.id),
        ]);
        let mut got = stream.until(&want).await;
        got.extend(stream.quiet().await);
        let ids: BTreeSet<String> = got.iter().map(|event| event.id.clone()).collect();
        assert_eq!(ids, want);

        // An identity andy mints while connected is covered from then on;
        // carol's inboxes stay closed.
        let nova = mint_ok(&ctx, &token, "andy/nova", &project).await;
        to_inbox(&ctx, &project, &kit.id).await;
        to_inbox(&ctx, &project, &nova.id).await;
        let event = stream.next().await;
        assert_eq!(event.id, format!("inbox:{}:1", nova.id));
        assert!(stream.quiet().await.is_empty());
    })
    .await;
}

#[tokio::test]
async fn it_events_end_when_the_token_does() {
    run(|ctx| async move {
        let (project, _, deleted) = room(&ctx).await;
        let p = project.project.as_str();
        let expired = login(&ctx, ANDY, "andy").await;
        let kept = login(&ctx, ANDY, "andy").await;
        let mut gone = events(&ctx, &deleted, p, None, None).await;
        let mut lapsed = events(&ctx, &expired, p, None, None).await;
        let mut open_one = events(&ctx, &kept, p, None, None).await;

        // Logging out deletes the token row; an hour after login the
        // token expires. Either ends a stream opened with it.
        sqlx::query("DELETE FROM tokens WHERE hash = sha256(convert_to($1, 'UTF8'))")
            .bind(&deleted)
            .execute(&ctx.pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE tokens SET expires_at = now() - interval '1 second'
             WHERE hash = sha256(convert_to($1, 'UTF8'))",
        )
        .bind(&expired)
        .execute(&ctx.pool)
        .await
        .unwrap();
        assert!(
            gone.ends_within(EVENT_WAIT).await,
            "a deleted token's stream went on"
        );
        assert!(
            lapsed.ends_within(EVENT_WAIT).await,
            "an expired token's stream went on"
        );

        // A stream on a live token keeps delivering.
        say(&ctx, &kept, p, "still here").await;
        assert_eq!(open_one.next().await.id, "feed:1");
    })
    .await;
}

#[tokio::test]
async fn it_events_connections_are_capped() {
    run(|ctx| async move {
        let (project, _, token) = room(&ctx).await;
        let p = project.project.as_str();
        let carol = seed_human(&ctx.pool, CAROL, "carol").await;
        seed_member(&ctx.pool, &carol, &project, "member").await;
        let carol_token = login(&ctx, CAROL, "carol").await;
        ctx.hub.hub.set_events_limits(3, 2);

        let first = events(&ctx, &token, p, None, None).await;
        let _second = events(&ctx, &token, p, None, None).await;
        // A person over their own cap is told to slow down.
        let resp = open(&ctx, &token, p, None, None).await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "too_many_streams");

        // A full hub refuses everyone.
        let _carols = events(&ctx, &carol_token, p, None, None).await;
        let resp = open(&ctx, &carol_token, p, None, None).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "hub_busy");

        // Closing a stream frees its place once the hub notices.
        drop(first);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let resp = open(&ctx, &token, p, None, None).await;
            if resp.status() == StatusCode::OK {
                break;
            }
            assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
            assert!(
                tokio::time::Instant::now() < deadline,
                "the place was never freed"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
}
