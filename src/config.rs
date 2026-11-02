use std::env;

/// Settings the process needs before it can serve requests.
#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub mqtt_host: String,
    pub mqtt_port: u16,
}

impl Config {
    /// Load `.env` (if it exists) and read `DATABASE_URL`.
    ///
    /// This panics when the variable is missing. A server without a database
    /// URL cannot do its job, so failing at startup is clearer than failing
    /// on the first request.
    pub fn from_env() -> Self {
        // Ignore a missing file: the variable may already be set in the shell.
        // Do not override variables that are already set.
        dotenvy::dotenv().ok();

        let database_url = env::var("DATABASE_URL")
            .expect("DATABASE_URL is not set. Copy .env.example to .env or export the variable.");

        // MQTT is optional. Missing values mean the local Mosquitto from docker compose.
        let mqtt_host = env::var("MQTT_HOST").unwrap_or_else(|_| "localhost".to_string());
        let mqtt_port = env::var("MQTT_PORT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1883);

        Self {
            database_url,
            mqtt_host,
            mqtt_port,
        }
    }
}
