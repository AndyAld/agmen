use super::*;
use agmen_client::{HumanTokens, Login, TokenStore, store_tokens};
use agmen_proto::{IdentityKind, Payload, Seq, Timestamp};
use tokio_stream::StreamExt as _;

const PENDING_LIMIT: usize = 10_000;
const WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);
const INITIAL_MESSAGES: usize = 3;
const FINAL_MESSAGES: usize = 6;
const HUB_EVENTS: usize = 16;
const ALERT_PROBE: Duration = Duration::from_millis(300);

type HubEvents = Arc<AsyncMutex<Option<tokio::sync::mpsc::Sender<axum::response::sse::Event>>>>;

struct Fixture {
    dir: tempfile::TempDir,
    daemon: crate::Agmend,
    full: ManagedIdentity,
    other: ManagedIdentity,
    hub_events: HubEvents,
    published: Arc<Mutex<Vec<ClientEnvelope>>>,
    publish_failure: Arc<AtomicBool>,
    publish_attempts: Arc<Mutex<Vec<agmen_proto::MessageId>>>,
    hub_task: tokio::task::JoinHandle<()>,
    supervisor: tokio::task::JoinHandle<()>,
    next_seq: u64,
}

impl Fixture {
    async fn new(local: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (hub_url, hub_events, hub_task) = fake_hub().await;
        let tokens = TokenStore::Dir(dir.path().join("tokens"));
        let login_file = dir.path().join("login.json");
        if !local {
            let login = Login {
                hub_url: hub_url.clone(),
                human_id: "owner001".parse().unwrap(),
                handle: "owner".into(),
            };
            tokio::fs::write(&login_file, serde_json::to_vec(&login).unwrap())
                .await
                .unwrap();
            store_tokens(
                &tokens,
                &login,
                &HumanTokens::new("fake-access".into(), "fake-refresh".into(), 3600),
            )
            .await
            .unwrap();
        }
        let mut daemon = crate::Agmend::bind(crate::Config::new(
            dir.path().join("agmend.sock"),
            login_file,
            tokens,
        ))
        .unwrap();
        let published = Arc::new(Mutex::new(Vec::new()));
        let captured = published.clone();
        let publish_failure = Arc::new(AtomicBool::new(false));
        let failed = publish_failure.clone();
        let publish_attempts = Arc::new(Mutex::new(Vec::new()));
        let attempts = publish_attempts.clone();
        Arc::get_mut(&mut Arc::get_mut(&mut daemon.core).unwrap().identities)
            .unwrap()
            .publisher = Some(Arc::new(move |_, envelope| {
            attempts.lock().unwrap().push(envelope.id.clone());
            if failed.load(Ordering::SeqCst) {
                return Box::pin(async { Err(hub_error("injected ALERT publishing failure")) });
            }
            captured.lock().unwrap().push(envelope);
            Box::pin(async { Ok(OutboxState::Pending { attempts: 0 }) })
        }));
        let mut rows = managed_rows(&daemon, local, &hub_url).await;
        rows.sort_by(|a, b| a.identity_id.cmp(&b.identity_id));
        let full = rows[0].clone();
        let other = rows[1].clone();
        store_work(daemon.core.identities.store.clone(), move |store| {
            for row in rows {
                store.register(&row)?;
            }
            Ok(())
        })
        .await
        .unwrap();
        let path = dir.path().join("agmend.db");
        let seeded = full.clone();
        tokio::task::spawn_blocking(move || {
            let message = test_message(&seeded, 1);
            rusqlite::Connection::open(path).unwrap().execute(
                "WITH RECURSIVE rows(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM rows WHERE n<?1)
                 INSERT INTO pending(origin,project,identity,sender,id,message)
                 SELECT ?2,?3,?4,'sender01',printf('%026d',n),json_set(?5,'$.id',printf('%026d',n)) FROM rows",
                rusqlite::params![PENDING_LIMIT as i64, agmen_client::origin_key(&seeded.hub_url), seeded.project.as_str(), seeded.identity_id.as_str(), serde_json::to_string(&message).unwrap()],
            ).unwrap();
        }).await.unwrap();
        let identities = daemon.core.identities.clone();
        let supervisor = tokio::spawn(async move { identities.supervise().await });
        if !local {
            tokio::time::timeout(WAIT, async {
                while hub_events.lock().await.is_none() {
                    tokio::time::sleep(POLL).await;
                }
            })
            .await
            .unwrap();
        }
        Self {
            dir,
            daemon,
            full,
            other,
            hub_events,
            published,
            publish_failure,
            publish_attempts,
            hub_task,
            supervisor,
            next_seq: 0,
        }
    }

    async fn send(&mut self) -> StampedEnvelope {
        self.next_seq += 1;
        let mut message = test_message(&self.full, self.next_seq);
        message.client.mentions.push(agmen_proto::Mention::Identity(
            self.other.identity_id.clone(),
        ));
        message.mentioned.push(self.other.identity_id.clone());
        if agmen_client::is_local(&self.full.hub_url) {
            let (_, seq) = self
                .daemon
                .core
                .origins
                .local()
                .post_as_person(&self.full.project, &message.client)
                .await
                .unwrap();
            message.seq = seq;
        } else {
            self.hub_events
                .lock()
                .await
                .as_ref()
                .unwrap()
                .send(
                    axum::response::sse::Event::default()
                        .data(serde_json::to_string(&message).unwrap()),
                )
                .await
                .unwrap();
        }
        let origin = agmen_client::origin_key(&self.full.hub_url);
        let owner = self.full.owner_id.clone();
        let project = self.full.project.clone();
        tokio::time::timeout(WAIT, async {
            loop {
                let (origin, owner, project) = (origin.clone(), owner.clone(), project.clone());
                let positions =
                    store_work(self.daemon.core.identities.store.clone(), move |store| {
                        store.positions_for_owner(&origin, &owner, &project)
                    })
                    .await
                    .unwrap();
                if positions
                    .get(&Stream::Feed)
                    .is_some_and(|seq| *seq >= message.seq.0)
                {
                    break;
                }
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .expect("full identity must not stop the collector's stream position");
        let delivered = self.drain(&self.other.identity_id, 500).await;
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].client.id, message.client.id);
        assert!(self.drain(&self.other.identity_id, 500).await.is_empty());
        message
    }

    async fn drain(&self, id: &IdentityId, limit: u32) -> Vec<StampedEnvelope> {
        let origin = agmen_client::origin_key(&self.full.hub_url);
        let id = id.clone();
        store_work(self.daemon.core.identities.store.clone(), move |store| {
            store.drain(&origin, &id, limit, usize::MAX)
        })
        .await
        .unwrap()
    }

    async fn alerts(&self) -> Vec<ClientEnvelope> {
        if agmen_client::is_local(&self.full.hub_url) {
            self.daemon
                .core
                .origins
                .local()
                .page(&self.full.project, &Stream::Feed, 0, COLLECTION_PAGE_SIZE)
                .await
                .unwrap()
                .messages
                .into_iter()
                .filter(|message| matches!(message.client.payload, Payload::Alert { .. }))
                .map(|message| message.client)
                .collect()
        } else {
            self.published.lock().unwrap().clone()
        }
    }

    async fn wait_alerts(&self, count: usize) {
        tokio::time::timeout(WAIT, async {
            loop {
                let alerts = self.alerts().await;
                assert!(alerts.len() <= count, "more than one ALERT per fill");
                if alerts.len() == count {
                    for alert in alerts {
                        let text = format!("{}'s queue is full ({PENDING_LIMIT} waiting); new messages to it are recorded in the project but not queued for it until it is started or removed.", self.full.handle);
                        assert_eq!(alert.body.as_str(), text);
                        assert!(matches!(alert.payload, Payload::Alert { need, needs: agmen_proto::Needs::Owner, .. } if need == text));
                    }
                    tokio::time::sleep(ALERT_PROBE).await;
                    assert_eq!(self.alerts().await.len(), count, "no repeated ALERT while full");
                    return;
                }
                tokio::time::sleep(POLL).await;
            }
        }).await.expect("owner must receive the full-queue ALERT");
    }

    async fn check(mut self) {
        for _ in 0..INITIAL_MESSAGES {
            self.send().await;
        }
        self.wait_alerts(1).await;
        assert_eq!(self.alerts().await.len(), 1);
        let path = self.dir.path().join("agmend.db");
        let id = self.full.identity_id.clone();
        let count = tokio::task::spawn_blocking(move || {
            rusqlite::Connection::open(path)
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM pending WHERE identity=?1",
                    [id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(count, PENDING_LIMIT as i64);
        assert_eq!(self.drain(&self.full.identity_id, 1).await.len(), 1);
        self.send().await;
        assert_eq!(
            self.alerts().await.len(),
            1,
            "draining alone must not alert"
        );
        for _ in INITIAL_MESSAGES + 1..FINAL_MESSAGES {
            self.send().await;
        }
        self.wait_alerts(2).await;
        assert_eq!(self.alerts().await.len(), 2);
        let collectors = self.daemon.core.identities.collectors.lock().await;
        assert_eq!(collectors.len(), 1);
        assert!(!collectors.values().next().unwrap().task.is_finished());
        drop(collectors);
        self.stop().await;
    }

    async fn stop(self) {
        self.supervisor.abort();
        assert!(self.supervisor.await.unwrap_err().is_cancelled());
        self.daemon.core.identities.shutdown().await;
        self.daemon.core.origins.local().shutdown().await;
        self.hub_task.abort();
        assert!(self.hub_task.await.unwrap_err().is_cancelled());
    }
}

async fn fake_hub() -> (Url, HubEvents, tokio::task::JoinHandle<()>) {
    let hub_events = Arc::new(AsyncMutex::new(None));
    let events = hub_events.clone();
    let app = axum::Router::new().route(
        "/v1/projects/project1/events",
        axum::routing::get(move || {
            let events = events.clone();
            async move {
                let (send, receive) = tokio::sync::mpsc::channel(HUB_EVENTS);
                *events.lock().await = Some(send);
                axum::response::Sse::new(
                    tokio_stream::wrappers::ReceiverStream::new(receive)
                        .map(Ok::<_, std::convert::Infallible>),
                )
                .keep_alive(axum::response::sse::KeepAlive::default())
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hub_url = format!("http://{}/", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    let hub_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (hub_url, hub_events, hub_task)
}

async fn managed_rows(daemon: &crate::Agmend, local: bool, hub_url: &Url) -> Vec<ManagedIdentity> {
    let mut rows = Vec::new();
    if local {
        let origin = daemon.core.origins.local();
        let topics = origin.create_project("queue-full", "owner").await.unwrap();
        let owner = origin.person().await.unwrap().unwrap().id;
        for name in ["first", "second"] {
            let minted = origin
                .mint(crate::record::admin::LocalMint {
                    project: topics.project().clone(),
                    name: name.into(),
                    kind: IdentityKind::Agent,
                    spec: None,
                    agent: None,
                    delegated_by: None,
                })
                .await
                .unwrap();
            rows.push(ManagedIdentity {
                hub_url: agmen_client::local_hub_url(),
                broker_url: agmen_client::LOCAL_BROKER_URL.parse().unwrap(),
                tenant: topics.tenant().clone(),
                project: topics.project().clone(),
                identity_id: minted.id,
                owner_id: owner.clone(),
                handle: minted.handle,
                kind: IdentityKind::Agent,
            });
        }
    } else {
        for id in ["agent001", "agent002"] {
            rows.push(ManagedIdentity {
                hub_url: hub_url.clone(),
                broker_url: "mqtt://127.0.0.1:9".parse().unwrap(),
                tenant: "tenant01".parse().unwrap(),
                project: "project1".parse().unwrap(),
                identity_id: id.parse().unwrap(),
                owner_id: "owner001".parse().unwrap(),
                handle: format!("owner/{id}"),
                kind: IdentityKind::Agent,
            });
        }
    }
    rows
}

fn test_message(who: &ManagedIdentity, seq: u64) -> StampedEnvelope {
    let mut message: StampedEnvelope = serde_json::from_value(json!({
        "v":1,"id":agmen_core::new_message_id(Timestamp::now()).unwrap(),"verb":"FYI","data":{},
        "body":"full queue test","mentioned":[who.identity_id],"sent_at":Timestamp::now(),
        "stream":"feed","seq":Seq(seq),"from":who.owner_id,"from_handle":"owner","from_kind":"human","hub_at":Timestamp::now()
    })).unwrap();
    message
        .client
        .mentions
        .push(agmen_proto::Mention::Identity(who.identity_id.clone()));
    message
}

#[tokio::test]
async fn local_collector_skips_full_queue_alerts_once_and_rearms_after_drain() {
    Fixture::new(true).await.check().await;
}

#[tokio::test]
async fn hub_collector_skips_full_queue_alerts_once_and_rearms_after_drain() {
    Fixture::new(false).await.check().await;
}

#[tokio::test]
async fn hub_queue_full_alert_recovers_after_post_failures_with_the_same_id() {
    let mut fixture = Fixture::new(false).await;
    fixture.publish_failure.store(true, Ordering::SeqCst);
    fixture.send().await;
    tokio::time::timeout(WAIT, async {
        while fixture.publish_attempts.lock().unwrap().len() < MAX_QUEUE_ALERT_ATTEMPTS {
            tokio::time::sleep(POLL).await;
        }
    })
    .await
    .unwrap();
    fixture.publish_failure.store(false, Ordering::SeqCst);
    fixture.wait_alerts(1).await;
    fixture.send().await;
    assert_eq!(fixture.alerts().await.len(), 1);
    {
        let attempts = fixture.publish_attempts.lock().unwrap();
        assert!(attempts.iter().all(|id| id == &attempts[0]));
    }
    fixture.stop().await;
}
