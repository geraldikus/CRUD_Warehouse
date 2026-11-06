use std::time::Duration;

use rumqttc::{AsyncClient, MqttOptions, QoS};
use serde::Serialize;
use uuid::Uuid;

use warehouse_core::topics;

use crate::models::Item;

/// Publisher handle shared by request handlers.
///
/// `Live` talks to Mosquitto. `Disabled` does nothing: tests and a server
/// whose broker is down still answer HTTP. Cloning is cheap, same idea as `PgPool`.
#[derive(Clone)]
pub enum MqttPublisher {
    Live(AsyncClient),
    Disabled,
}

#[derive(Serialize)]
struct DeletedEvent {
    id: Uuid,
}

#[derive(Serialize)]
struct ExpiringEvent<'a> {
    item: &'a Item,
    hours_left: i64,
}

#[derive(Serialize)]
struct ExpiredEvent<'a> {
    item: &'a Item,
}

impl MqttPublisher {
    pub fn disabled() -> Self {
        Self::Disabled
    }

    /// Open a connection and keep the MQTT event loop running in the background.
    ///
    /// rumqttc does not send or receive bytes until something polls that loop.
    /// The first poll is the handshake. If the broker is silent, we return an error
    /// and the caller starts the API without events.
    pub async fn connect(host: &str, port: u16) -> Result<Self, String> {
        let mut options = MqttOptions::new("warehouse-api", host, port);
        options.set_keep_alive(Duration::from_secs(5));

        let (client, mut eventloop) = AsyncClient::new(options, 10);

        match tokio::time::timeout(Duration::from_secs(2), eventloop.poll()).await {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => return Err(err.to_string()),
            Err(_) => {
                return Err(format!(
                    "broker at {host}:{port} did not answer within 2 seconds"
                ));
            }
        }

        tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!(error = %err, "mqtt connection error, retrying");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        });

        tracing::info!(host, port, "mqtt connected");
        Ok(Self::Live(client))
    }

    pub async fn publish_created(&self, item: &Item) {
        self.publish(topics::CREATED, item).await;
    }

    pub async fn publish_updated(&self, item: &Item) {
        self.publish(topics::UPDATED, item).await;
    }

    pub async fn publish_deleted(&self, id: Uuid) {
        self.publish(topics::DELETED, &DeletedEvent { id }).await;
    }

    pub async fn publish_expiring(&self, item: &Item, hours_left: i64) {
        self.publish(topics::EXPIRING, &ExpiringEvent { item, hours_left })
            .await;
    }

    pub async fn publish_expired(&self, item: &Item) {
        self.publish(topics::EXPIRED, &ExpiredEvent { item }).await;
    }

    async fn publish<T: Serialize>(&self, topic: &str, payload: &T) {
        let Self::Live(client) = self else {
            return;
        };

        let bytes = match serde_json::to_vec(payload) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!(error = %err, "mqtt payload encode failed");
                return;
            }
        };

        // QoS 1: the broker should deliver the message at least once.
        // A timeout keeps a dead broker from holding the HTTP response forever.
        let send = client.publish(topic, QoS::AtLeastOnce, false, bytes);
        match tokio::time::timeout(Duration::from_secs(2), send).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::warn!(error = %err, topic, "mqtt publish failed"),
            Err(_) => tracing::warn!(topic, "mqtt publish timed out"),
        }
    }
}
