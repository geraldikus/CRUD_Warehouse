//! Prints warehouse events from the MQTT broker.
//!
//! Run this in a second terminal while `cargo run` serves the API:
//! `cargo run --bin mqtt_listener`

use std::env;
use std::time::Duration;

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    dotenvy::dotenv().ok();
    let host = env::var("MQTT_HOST").unwrap_or_else(|_| "localhost".to_string());
    let port = env::var("MQTT_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1883);

    let mut options = MqttOptions::new("warehouse-listener", host.clone(), port);
    options.set_keep_alive(Duration::from_secs(5));
    let (client, mut eventloop) = AsyncClient::new(options, 10);
    client
        .subscribe("warehouse/items/#", QoS::AtLeastOnce)
        .await?;
    tracing::info!(%host, port, "listening for warehouse/items/#");

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::Publish(publish))) => {
                let body = String::from_utf8_lossy(&publish.payload);
                tracing::info!(topic = %publish.topic, %body, "event");
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(error = %err, "mqtt connection error, retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}
