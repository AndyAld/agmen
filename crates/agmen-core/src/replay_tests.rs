use std::time::Duration;

use agmen_proto::{Payload, Stream, Topics};

use crate::{Core, HubPost, MAX_PAGE, MemStore, Store, StoreTx, post_in};

pub(crate) fn topics() -> Topics {
    Topics::new("tenant01".parse().unwrap(), "project01".parse().unwrap())
}

pub(crate) fn post() -> HubPost {
    HubPost {
        payload: Payload::Status {
            summary: "ready".into(),
            items: Vec::new(),
            spend_usd: None,
            hours: None,
            tokens: None,
        },
        body: "ready".try_into().unwrap(),
        reply_to: None,
        items: Vec::new(),
        lane: None,
        mentioned: Vec::new(),
        streams: vec![Stream::Feed],
    }
}

async fn filled(count: u32) -> Core<MemStore> {
    let core = Core::new(MemStore::default(), Duration::from_secs(60));
    let mut tx = core.store().begin().await.unwrap();
    for _ in 0..count {
        post_in(&mut tx, &topics(), post()).await.unwrap();
    }
    tx.commit().await.unwrap();
    core
}

#[tokio::test]
async fn page_is_the_next_run_of_seqs() {
    let core = filled(5).await;
    let page = core
        .page(topics().project(), &Stream::Feed, 2, 2)
        .await
        .unwrap();
    assert_eq!(
        page.messages.iter().map(|m| m.seq.0).collect::<Vec<_>>(),
        [3, 4]
    );
    assert_eq!(page.next_after, Some(4));
    assert_eq!(page.head, 5);
    assert_eq!(page.pruned_through, 0);
    let last = core
        .page(topics().project(), &Stream::Feed, 4, 2)
        .await
        .unwrap();
    assert_eq!(last.messages[0].seq.0, 5);
    assert_eq!(last.next_after, None);
}

#[tokio::test]
async fn page_limit_is_capped() {
    let core = filled(MAX_PAGE + 1).await;
    let page = core
        .page(topics().project(), &Stream::Feed, 0, u32::MAX)
        .await
        .unwrap();
    assert_eq!(MAX_PAGE, 500);
    assert_eq!(page.messages.len(), MAX_PAGE as usize);
    assert_eq!(page.next_after, Some(u64::from(MAX_PAGE)));
    assert_eq!(page.head, u64::from(MAX_PAGE + 1));
}

#[tokio::test]
async fn empty_pages_keep_the_head_and_project_and_stream_scope() {
    let core = filled(1).await;
    for (project, stream, after, head) in [
        (topics().project().clone(), Stream::Feed, 1, 1),
        (
            topics().project().clone(),
            Stream::Inbox("aaaaaaaa".parse().unwrap()),
            0,
            0,
        ),
        ("project02".parse().unwrap(), Stream::Feed, 0, 0),
    ] {
        let page = core.page(&project, &stream, after, MAX_PAGE).await.unwrap();
        assert!(page.messages.is_empty());
        assert_eq!(page.head, head);
        assert_eq!(page.next_after, None);
        assert_eq!(page.pruned_through, 0);
    }
    core.store().fail_next("page");
    assert!(
        core.page(topics().project(), &Stream::Feed, 0, 1)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn page_rejects_out_of_range_cursor() {
    let core = filled(1).await;
    assert!(
        core.page(
            topics().project(),
            &Stream::Feed,
            i64::MAX as u64 + 1,
            MAX_PAGE
        )
        .await
        .is_err()
    );
    assert!(
        core.page(topics().project(), &Stream::Feed, i64::MAX as u64, MAX_PAGE)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
}
