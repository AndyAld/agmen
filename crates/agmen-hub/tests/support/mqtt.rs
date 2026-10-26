//! A small MQTT 5 test client against the dev EMQX, after the S1 spike's.

use std::collections::VecDeque;
use std::time::Duration;

use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::mqttbytes::v5::{
    DisconnectReasonCode, Packet, PubAckReason, Publish, SubscribeReasonCode,
};
use rumqttc::v5::{AsyncClient, ConnectionError, Event, MqttOptions, StateError};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{timeout, timeout_at};

use super::env;

pub const ACK_WAIT: Duration = Duration::from_secs(10);

/// Host and port from `AGMEN_IT_MQTT` (`mqtt://host:port`).
fn mqtt_addr() -> (String, u16) {
    let url = env("AGMEN_IT_MQTT");
    let rest = url
        .strip_prefix("mqtt://")
        .unwrap_or_else(|| panic!("AGMEN_IT_MQTT must start with mqtt://, got {url}"));
    let (host, port) = rest
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("AGMEN_IT_MQTT must name a port, got {url}"));
    (host.to_owned(), port.parse().unwrap())
}

pub fn options(client_id: &str, username: &str, token: &str) -> MqttOptions {
    let (host, port) = mqtt_addr();
    let mut opts = MqttOptions::new(client_id, host, port);
    opts.set_credentials(username, token)
        .set_keep_alive(Duration::from_secs(30));
    opts
}

#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "a test channel item; boxing Packet would only add derefs to every match"
)]
enum Ev {
    Packet(Packet),
    Closed(ConnectionError),
}

/// One connected client whose event loop runs on its own task. The poller
/// stops at the first error, so it never reconnects behind a test's back.
#[derive(Debug)]
pub struct Client {
    client: AsyncClient,
    events: mpsc::Receiver<Ev>,
    pending: VecDeque<Publish>,
    poller: JoinHandle<()>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.poller.abort();
    }
}

/// Connects and waits for CONNACK, failing the test if the broker refuses.
pub async fn connect_ok(opts: MqttOptions) -> Client {
    let id = opts.client_id();
    let (client, mut eventloop) = AsyncClient::new(opts, 64);
    loop {
        let event = timeout(ACK_WAIT, eventloop.poll())
            .await
            .expect("no CONNACK within 10 s")
            .unwrap_or_else(|e| panic!("{id} should connect: {e:?}"));
        if let Event::Incoming(Packet::ConnAck(_)) = event {
            break;
        }
    }
    let (tx, events) = mpsc::channel(1024);
    let poller = tokio::spawn(async move {
        loop {
            let ev = match eventloop.poll().await {
                Ok(Event::Incoming(packet)) => Ev::Packet(packet),
                Ok(Event::Outgoing(_)) => continue,
                Err(e) => Ev::Closed(e),
            };
            let closed = matches!(ev, Ev::Closed(_));
            if tx.send(ev).await.is_err() || closed {
                return;
            }
        }
    });
    Client {
        client,
        events,
        pending: VecDeque::new(),
        poller,
    }
}

impl Client {
    async fn next_control(&mut self) -> Packet {
        loop {
            match timeout(ACK_WAIT, self.events.recv()).await.ok().flatten() {
                Some(Ev::Packet(Packet::Publish(p))) => self.pending.push_back(p),
                Some(Ev::Packet(packet)) => return packet,
                Some(Ev::Closed(e)) => panic!("connection closed while waiting for an ack: {e:?}"),
                None => panic!("no ack within 10 s"),
            }
        }
    }

    /// Publishes at QoS 1 and returns the PUBACK reason.
    pub async fn publish(&mut self, topic: &str, payload: &[u8]) -> PubAckReason {
        self.client
            .publish(topic, QoS::AtLeastOnce, false, payload.to_vec())
            .await
            .unwrap();
        loop {
            if let Packet::PubAck(ack) = self.next_control().await {
                return ack.reason;
            }
        }
    }

    /// Subscribes at QoS 1 and returns the SUBACK reason.
    pub async fn subscribe(&mut self, filter: &str) -> SubscribeReasonCode {
        self.client
            .subscribe(filter, QoS::AtLeastOnce)
            .await
            .unwrap();
        loop {
            if let Packet::SubAck(ack) = self.next_control().await {
                return ack.return_codes[0];
            }
        }
    }

    /// The next publish on `topic` within `within`; other topics are dropped.
    pub async fn recv_on(&mut self, topic: &str, within: Duration) -> Option<Publish> {
        if let Some(i) = self
            .pending
            .iter()
            .position(|p| p.topic == topic.as_bytes())
        {
            return self.pending.remove(i);
        }
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match timeout_at(deadline, self.events.recv()).await {
                Ok(Some(Ev::Packet(Packet::Publish(p)))) if p.topic == topic.as_bytes() => {
                    return Some(p);
                }
                Ok(Some(Ev::Packet(_))) => {}
                Ok(Some(Ev::Closed(e))) => {
                    panic!("connection closed while waiting on {topic}: {e:?}")
                }
                Ok(None) | Err(_) => return None,
            }
        }
    }

    /// Waits for the broker to end the connection and returns the
    /// DISCONNECT reason, if EMQX sent one.
    pub async fn closed_within(
        &mut self,
        within: Duration,
    ) -> Result<Option<DisconnectReasonCode>, &'static str> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match timeout_at(deadline, self.events.recv()).await {
                Ok(Some(Ev::Closed(ConnectionError::MqttState(
                    StateError::ServerDisconnect { reason_code, .. },
                )))) => return Ok(Some(reason_code)),
                Ok(Some(Ev::Packet(_))) => {}
                Ok(Some(Ev::Closed(_)) | None) => return Ok(None),
                Err(_) => return Err("still connected"),
            }
        }
    }
}
