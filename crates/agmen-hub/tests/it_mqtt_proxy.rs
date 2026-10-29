//! MQTT 5 over the hub's WebSocket proxy against `AGMEN_IT_MQTT_WS`.
#![cfg(test)]

mod support;

use std::time::Duration;

use rumqttc_next::{
    AsyncClient, Broker, ConnectReturnCode, Event, MqttOptions, Packet, PubAckReason,
    PublishOptions,
};
use support::{broker_token, env, member_with_token, mint_ok, run, seed_project};
use tokio::time::timeout;
use url::Url;

#[tokio::test]
async fn it_mqtt_over_hub_proxy() {
    let upstream = Url::parse(&env("AGMEN_IT_MQTT_WS")).unwrap();
    assert_eq!(upstream.scheme(), "ws");
    run(|ctx| async move {
        timeout(Duration::from_secs(90), async {
            let _messaging = timeout(Duration::from_secs(30), ctx.hub.start_messaging())
                .await
                .unwrap();
            let project = seed_project(&ctx.pool).await;
            let human = member_with_token(&ctx, &project, 4242, "andy", "member").await;
            let own = mint_ok(&ctx, &human, "andy/proxy", &project).await;
            let other = mint_ok(&ctx, &human, "andy/other", &project).await;
            let (status, body) = broker_token(&ctx.http, &ctx.hub, &own.id, &own.token).await;
            assert_eq!(status, 200);
            let token = body["token"].as_str().unwrap().to_owned();
            let mut endpoint = ctx.hub.url("/mqtt");
            endpoint.set_scheme("ws").unwrap();
            let mut options = MqttOptions::new(
                format!("{}-mcp", own.id),
                Broker::websocket(endpoint.to_string()).unwrap(),
            );
            options
                .set_credentials(own.id.clone(), token)
                .set_keep_alive(30);
            let (client, mut events) = AsyncClient::builder(options).capacity(8).build();
            timeout(Duration::from_secs(11), async {
                for _ in 0..32 {
                    if let Event::Incoming(Packet::ConnAck(ack)) = events.poll().await.unwrap() {
                        assert_eq!(ack.code, ConnectReturnCode::Success);
                        return;
                    }
                }
                panic!("no CONNACK within 32 events");
            })
            .await
            .unwrap();
            for (identity, expected) in [
                (&own.id, PubAckReason::Success),
                (&other.id, PubAckReason::NotAuthorized),
            ] {
                let topic = format!(
                    "agmen/v1/{}/{}/up/{identity}",
                    project.tenant, project.project
                );
                timeout(Duration::from_secs(11), async {
                    client
                        .publish(
                            topic,
                            b"proxy-test".as_slice(),
                            PublishOptions::at_least_once(),
                        )
                        .await
                        .unwrap();
                    for _ in 0..32 {
                        if let Event::Incoming(Packet::PubAck(ack)) = events.poll().await.unwrap() {
                            assert_eq!(ack.reason, expected);
                            if expected == PubAckReason::NotAuthorized {
                                assert_eq!(ack.reason as u8, 0x87);
                            }
                            return;
                        }
                    }
                    panic!("no PUBACK within 32 events");
                })
                .await
                .unwrap();
            }
        })
        .await
        .unwrap();
    })
    .await;
}
