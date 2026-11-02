use std::error::Error;
use std::net::SocketAddr;

use warehouse::AppState;
use warehouse::config::Config;
use warehouse::db;
use warehouse::expiry;
use warehouse::mqtt::MqttPublisher;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env();
    let pool = db::create_pool(&config.database_url).await?;
    // A dead broker must not stop the HTTP server. Events are best-effort.
    let mqtt = match MqttPublisher::connect(&config.mqtt_host, config.mqtt_port).await {
        Ok(publisher) => publisher,
        Err(err) => {
            tracing::warn!(error = %err, "mqtt broker unavailable, continuing without events");
            MqttPublisher::disabled()
        }
    };

    let state = AppState { pool, mqtt };
    // Same process as the HTTP server, separate task. Ctrl+C stops both.
    tokio::spawn(expiry::run_loop(state.clone()));
    let app = warehouse::app(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("listening on {addr}");

    // `with_graceful_shutdown` stops accepting new connections when the
    // signal future finishes, then waits for in-flight requests to end.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    // Installing the handler fails only when the process cannot receive
    // signals at all. There is no useful fallback, so we stop here.
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install Ctrl+C handler");
    tracing::info!("shutdown signal received, stopping server");
}
