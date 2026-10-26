//! agmen-client against the dev EMQX and an in-process hub.
//!
//! Only `it_gap_filled_by_replay` uses toxiproxy; `.config/nextest.toml`
//! runs it in the `toxiproxy` test group, one at a time with the other
//! tests that cut that shared link.

// Test code throughout, helpers included: this lets clippy's
// allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use agmen_client::{ClientError, ClientEvent, ClientKind, OutboxState, Session, TokenSource};
use agmen_hub::{BrokerTokens, Identity, SigningKey};
use agmen_proto::{Seq, Stream};
use support::{
    close, connection, delivered, dev_key, dm, fyi, messages_until, next_event, next_feed_seq,
    recorded_on_feed, run, set_proxy_enabled,
};

#[tokio::test(flavor = "multi_thread")]
async fn it_publish_recorded() {
    run(|ctx| async move {
        let who = ctx.identity().await;
        let (session, mut events) =
            Session::connect(ctx.config(ClientKind::Cli, &who, &ctx.broker))
                .await
                .unwrap();
        connection(&mut events, true).await;
        let env = fyi("hello");
        session.publish(env.clone()).await.unwrap();
        let sent = next_event(&mut events, |event| match event {
            ClientEvent::Delivery { id, state } if *id == env.id => Some(state.clone()),
            _ => None,
        })
        .await;
        assert_eq!(sent, OutboxState::Pending { attempts: 1 });
        let recorded = OutboxState::Recorded {
            stream: Stream::Feed,
            seq: Seq(1),
        };
        assert_eq!(delivered(&mut events, &env.id).await, recorded);
        assert_eq!(session.outbox().state(&env.id).unwrap(), Some(recorded));
        close(session).await;
    })
    .await;
}

/// The watcher's session does not persist (expiry 0), so while toxiproxy
/// cuts it off the broker drops what is published; feed seq 2 and the DM in
/// its inbox reach it only by replay (ruling R18).
#[tokio::test(flavor = "multi_thread")]
async fn it_gap_filled_by_replay() {
    run(|ctx| async move {
        // A failed run of a toxiproxy test can leave the link off.
        set_proxy_enabled(true).await;
        let (poster, watcher) = (ctx.identity().await, ctx.identity().await);
        let (post, mut posted) =
            Session::connect(ctx.config(ClientKind::Cli, &poster, &ctx.broker))
                .await
                .unwrap();
        let watch_config = ctx
            .config(ClientKind::Watch, &watcher, &ctx.proxied)
            .with_session_expiry(Duration::ZERO);
        let (watch, mut watched) = Session::connect(watch_config).await.unwrap();

        let first = fyi("one");
        post.publish(first.clone()).await.unwrap();
        assert_eq!(recorded_on_feed(&mut posted, &first.id).await, 1);
        assert_eq!(next_feed_seq(&mut watched).await, 1);

        set_proxy_enabled(false).await;
        connection(&mut watched, false).await;
        let second = fyi("two");
        post.publish(second.clone()).await.unwrap();
        assert_eq!(recorded_on_feed(&mut posted, &second.id).await, 2);
        // A DM goes to the watcher's inbox, which replays the same way.
        let note = dm(&watcher.identity.id, "psst");
        post.publish(note.clone()).await.unwrap();
        assert!(matches!(
            delivered(&mut posted, &note.id).await,
            OutboxState::Recorded { .. }
        ));
        set_proxy_enabled(true).await;

        connection(&mut watched, true).await;
        let inbox = Stream::Inbox(watcher.identity.id.clone());
        let caught_up = messages_until(&mut watched, |seen| {
            seen.iter().any(|m| m.stream == Stream::Feed) && seen.iter().any(|m| m.stream == inbox)
        })
        .await;
        let feed: Vec<_> = caught_up
            .iter()
            .filter(|m| m.stream == Stream::Feed)
            .collect();
        assert_eq!(feed.len(), 1);
        assert_eq!(feed[0].seq, Seq(2));
        assert_eq!(feed[0].client.id, second.id);
        let dms: Vec<_> = caught_up.iter().filter(|m| m.stream == inbox).collect();
        assert_eq!(dms.len(), 1);
        assert_eq!((dms[0].seq, &dms[0].client.id), (Seq(1), &note.id));
        let third = fyi("three");
        post.publish(third.clone()).await.unwrap();
        assert_eq!(recorded_on_feed(&mut posted, &third.id).await, 3);
        assert_eq!(next_feed_seq(&mut watched).await, 3);
        // Nothing comes twice.
        let late = tokio::time::timeout(Duration::from_secs(2), async {
            next_feed_seq(&mut watched).await
        })
        .await;
        assert!(late.is_err(), "a message was delivered twice: {late:?}");
        close(watch).await;
        close(post).await;
    })
    .await;
}

/// ADR-0003: with its MQTT session record lost or corrupt, a watcher whose
/// broker session still holds a queued message gets `SessionStateMismatch`,
/// connects once with a clean start, and replays the message instead.
#[tokio::test(flavor = "multi_thread")]
async fn it_lost_session_record_falls_back_to_replay() {
    run(|ctx| async move {
        let (poster, watcher) = (ctx.identity().await, ctx.identity().await);
        let (post, mut posted) =
            Session::connect(ctx.config(ClientKind::Cli, &poster, &ctx.broker))
                .await
                .unwrap();
        let record = ctx
            .state_dir(&watcher)
            .join(format!("{}-watch.mqtt", watcher.identity.id));
        let watch = || Session::connect(ctx.config(ClientKind::Watch, &watcher, &ctx.broker));

        let (mut session, mut watched) = watch().await.unwrap();
        let first = fyi("one");
        post.publish(first.clone()).await.unwrap();
        assert_eq!(recorded_on_feed(&mut posted, &first.id).await, 1);
        assert_eq!(next_feed_seq(&mut watched).await, 1);

        let corrupt: &[u8] = b"not a session record";
        for (seq, damage) in [(2, None), (3, Some(corrupt))] {
            // A clean DISCONNECT: the broker keeps the session (60 s here).
            close(session).await;
            assert!(record.exists(), "a persistent session keeps a record");
            match damage {
                None => std::fs::remove_file(&record).unwrap(),
                Some(bytes) => std::fs::write(&record, bytes).unwrap(),
            }
            // The broker queues this for the watcher's session.
            let queued = fyi("sent while the watcher was away");
            post.publish(queued.clone()).await.unwrap();
            assert_eq!(recorded_on_feed(&mut posted, &queued.id).await, seq);
            (session, watched) = watch().await.unwrap();
            assert_eq!(next_feed_seq(&mut watched).await, seq);
        }
        // The new session works live.
        let last = fyi("four");
        post.publish(last.clone()).await.unwrap();
        assert_eq!(recorded_on_feed(&mut posted, &last.id).await, 4);
        assert_eq!(next_feed_seq(&mut watched).await, 4);
        close(session).await;
        close(post).await;
    })
    .await;
}

/// Mints broker tokens that live 12 s, as the hub would with a short TTL,
/// and counts them.
#[derive(Debug, Clone)]
struct ShortTokens {
    tokens: Arc<BrokerTokens>,
    identity: Identity,
    minted: Arc<AtomicU32>,
}

impl TokenSource for ShortTokens {
    async fn token(&self) -> Result<String, ClientError> {
        self.minted.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .tokens
            .mint(&self.identity, Duration::from_secs(12))
            .unwrap())
    }
}

/// EMQX disconnects a client when its token expires (ADR-0002). The
/// session swaps the token halfway through its 12 s life and reconnects
/// with the fresh one first, so the link never drops.
#[tokio::test(flavor = "multi_thread")]
async fn it_token_refreshed_before_expiry() {
    run(|ctx| async move {
        let who = ctx.identity().await;
        let tokens =
            BrokerTokens::new(SigningKey::from_pem_file(dev_key()).unwrap(), Vec::new()).unwrap();
        let source = ShortTokens {
            tokens: Arc::new(tokens),
            identity: who.identity.clone(),
            minted: Arc::default(),
        };
        let config = ctx
            .config(ClientKind::Mcp, &who, &ctx.broker)
            .with_token_source(source.clone());
        let (session, mut events) = Session::connect(config).await.unwrap();
        connection(&mut events, true).await;
        // Past the first token's expiry, when EMQX would have dropped it.
        let until = tokio::time::Instant::now() + Duration::from_secs(16);
        while let Ok(event) = tokio::time::timeout_at(until, events.recv()).await {
            assert_ne!(
                event,
                Some(ClientEvent::Connection { connected: false }),
                "the link dropped"
            );
        }
        assert!(
            source.minted.load(Ordering::SeqCst) >= 3,
            "a token at connect, then one every 6 s"
        );
        let env = fyi("still here");
        session.publish(env.clone()).await.unwrap();
        assert_eq!(recorded_on_feed(&mut events, &env.id).await, 1);
        close(session).await;
    })
    .await;
}
