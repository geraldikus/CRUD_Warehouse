use std::env;

/// Where the window sends HTTP and where it listens for MQTT.
///
/// Defaults match `cargo run` of the API on this machine. `.env` in the
/// current directory overrides them, same as the server.
#[derive(Debug, Clone)]
pub struct Config {
    pub api_url: String,
    pub mqtt_host: String,
    pub mqtt_port: u16,
}

impl Config {
    pub fn from_env() -> Self {
        dotenvy::dotenv().ok();

        let api_url =
            env::var("WAREHOUSE_API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
        let mqtt_host = env::var("MQTT_HOST").unwrap_or_else(|_| "localhost".to_string());
        let mqtt_port = env::var("MQTT_PORT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1883);

        Self {
            api_url,
            mqtt_host,
            mqtt_port,
        }
    }
}
