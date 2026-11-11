use std::error::Error;
use std::net::SocketAddr;

use warehouse::AppState;
use warehouse::config::Config;
use warehouse::db;
use warehouse::expiry;
use warehouse::llm::GroqClient;
use warehouse::mqtt::MqttPublisher;
use warehouse::voice::VoiceClient;

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

    let llm = config
        .groq_api_key
        .as_ref()
        .map(|key| GroqClient::new(key, &config.groq_model, &config.groq_base_url));
    if llm.is_none() {
        tracing::warn!("GROQ_API_KEY is not set, POST /assistant will return 503");
    }

    let voice = config.voice_url.as_ref().map(|url| VoiceClient::new(url));
    match &voice {
        Some(client) => {
            if let Err(err) = client.probe().await {
                tracing::warn!(
                    error = %err,
                    "voice sidecar unavailable, speech routes will fail until it is up"
                );
            } else {
                tracing::info!("voice sidecar ready");
            }
        }
        None => tracing::info!("VOICE_URL is empty, speech routes will return 503"),
    }

    let state = AppState {
        pool,
        mqtt,
        llm,
        voice,
    };
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
