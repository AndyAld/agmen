//! The hub's first broker connect retries until the broker answers, against
//! an in-process hub, a database of the test's own on `AGMEN_IT_PG` and the
//! dev EMQX (`AGMEN_IT_MQTT`). In the cluster EMQX reads the hub's own JWKS,
//! so after a cold start the hub's first attempts fail.

// Integration tests are test code throughout, helpers included: this lets
// clippy's allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::time::Duration;

use support::{mqtt, run};
use tokio::net::{TcpListener, TcpStream};

/// Forwards every connection on `listener` to the dev EMQX.
async fn forward_to_emqx(listener: TcpListener) {
    let (host, port) = mqtt::mqtt_addr();
    loop {
        let (mut inbound, _) = listener.accept().await.unwrap();
        let mut outbound = TcpStream::connect((host.as_str(), port)).await.unwrap();
        tokio::spawn(async move {
            // The link ends when either side closes, which is not a failure.
            if let Err(err) = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await {
                eprintln!("forwarded link closed: {err}");
            }
        });
    }
}

#[tokio::test]
async fn it_first_broker_connect_retries_until_the_broker_answers() {
    run(|ctx| async move {
        // A free port with nothing listening yet, so the first attempts are
        // refused.
        let port = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut config = ctx.hub.mqtt.clone();
        config.host = "127.0.0.1".to_owned();
        config.port = port;

        let hub = ctx.hub.hub.clone();
        let started = tokio::spawn(async move { hub.start_messaging(&config).await });
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!started.is_finished(), "start_messaging gave up at once");

        let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let forwarder = tokio::spawn(forward_to_emqx(listener));
        let messaging = tokio::time::timeout(Duration::from_secs(30), started)
            .await
            .expect("start_messaging did not finish within 30 s")
            .unwrap();
        assert!(messaging.is_ok(), "{:?}", messaging.err());
        forwarder.abort();
    })
    .await;
}
