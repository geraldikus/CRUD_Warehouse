use std::time::Duration;

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde::Deserialize;
use uuid::Uuid;
use warehouse_core::models::Item;
use warehouse_core::topics;

use crate::config::Config;

/// What a broker message means for the window.
///
/// `Changed` is "the table may be stale": the worker follows it with a
/// `GET /items`. The two expiry variants carry the text for the notice list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteEvent {
    Changed,
    Expiring { name: String, hours_left: i64 },
    Expired { name: String },
}

#[derive(Debug)]
pub(crate) enum MqttUpdate {
    Online(bool),
    Message(RemoteEvent),
}

#[derive(Deserialize)]
struct NamedItem {
    item: Item,
}

#[derive(Deserialize)]
struct ExpiringBody {
    item: Item,
    hours_left: i64,
}

/// Subscribe until the process exits. A dead broker is not fatal: the
/// callback hears `Online(false)` and HTTP keeps working.
pub(crate) async fn run(config: Config, mut on_update: impl FnMut(MqttUpdate) + Send) {
    let mut announced_offline = false;
    loop {
        match connect_and_listen(&config, &mut on_update, &mut announced_offline).await {
            Ok(()) => {}
            Err(err) => {
                tracing::warn!(error = %err, "mqtt unavailable");
                if !announced_offline {
                    announced_offline = true;
                    on_update(MqttUpdate::Online(false));
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

async fn connect_and_listen(
    config: &Config,
    on_update: &mut (impl FnMut(MqttUpdate) + Send),
    announced_offline: &mut bool,
) -> Result<(), String> {
    // A fixed client id would kick the previous window off the broker
    // as soon as a second one connects. The id is unique per connection.
    let client_id = format!("warehouse-desktop-{}", Uuid::new_v4());
    let mut options = MqttOptions::new(&client_id, config.mqtt_host.clone(), config.mqtt_port);
    options.set_keep_alive(Duration::from_secs(5));

    let (client, mut eventloop) = AsyncClient::new(options, 10);
    match tokio::time::timeout(Duration::from_secs(2), eventloop.poll()).await {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => return Err(err.to_string()),
        Err(_) => {
            return Err(format!(
                "broker at {}:{} did not answer within 2 seconds",
                config.mqtt_host, config.mqtt_port
            ));
        }
    }

    client
        .subscribe(topics::ALL, QoS::AtLeastOnce)
        .await
        .map_err(|err| err.to_string())?;

    tracing::info!(%client_id, "mqtt subscribed");
    *announced_offline = false;
    on_update(MqttUpdate::Online(true));

    // Dropping `client` closes the connection. Hold it for the listen loop.
    let _client = client;
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                let payload = String::from_utf8_lossy(&publish.payload);
                if let Some(remote) = parse_mqtt(&publish.topic, &payload) {
                    on_update(MqttUpdate::Message(remote));
                }
            }
            Ok(_) => {}
            Err(err) => return Err(err.to_string()),
        }
    }
}

pub(crate) fn parse_mqtt(topic: &str, payload: &str) -> Option<RemoteEvent> {
    match topic {
        topics::CREATED | topics::UPDATED | topics::DELETED => Some(RemoteEvent::Changed),
        topics::EXPIRING => {
            let body: ExpiringBody = serde_json::from_str(payload).ok()?;
            Some(RemoteEvent::Expiring {
                name: body.item.name,
                hours_left: body.hours_left,
            })
        }
        topics::EXPIRED => {
            let body: NamedItem = serde_json::from_str(payload).ok()?;
            Some(RemoteEvent::Expired {
                name: body.item.name,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_json() -> &'static str {
        r#"{"id":"550e8400-e29b-41d4-a716-446655440000","name":"bolts","quantity":2,"created_at":"2026-01-02T03:04:05Z","updated_at":"2026-01-02T03:04:05Z","expires_at":null}"#
    }

    #[test]
    fn created_is_a_change_even_without_a_body() {
        assert_eq!(parse_mqtt(topics::CREATED, ""), Some(RemoteEvent::Changed));
        assert_eq!(
            parse_mqtt(
                topics::DELETED,
                r#"{"id":"550e8400-e29b-41d4-a716-446655440000"}"#
            ),
            Some(RemoteEvent::Changed)
        );
    }

    #[test]
    fn expiring_carries_the_name_and_step() {
        let payload = format!(r#"{{"item":{},"hours_left":24}}"#, item_json());
        assert_eq!(
            parse_mqtt(topics::EXPIRING, &payload),
            Some(RemoteEvent::Expiring {
                name: "bolts".to_string(),
                hours_left: 24,
            })
        );
    }

    #[test]
    fn expired_carries_the_name() {
        let payload = format!(r#"{{"item":{}}}"#, item_json());
        assert_eq!(
            parse_mqtt(topics::EXPIRED, &payload),
            Some(RemoteEvent::Expired {
                name: "bolts".to_string(),
            })
        );
    }

    #[test]
    fn bad_expiry_payload_is_ignored() {
        assert_eq!(parse_mqtt(topics::EXPIRING, "not-json"), None);
        assert_eq!(parse_mqtt("warehouse/items/other", item_json()), None);
    }
}
