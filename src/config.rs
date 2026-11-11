use std::env;

/// Settings the process needs before it can serve requests.
#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub mqtt_host: String,
    pub mqtt_port: u16,
    /// `None` when the variable is missing or blank. The HTTP server still
    /// starts; `POST /assistant` answers 503 until a key is set.
    pub groq_api_key: Option<String>,
    pub groq_model: String,
    pub groq_base_url: String,
    /// `None` when `VOICE_URL` is set and blank. Missing means the local
    /// sidecar from docker compose. Speech routes answer 503 until a URL is set.
    pub voice_url: Option<String>,
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

        // A blank value counts as unset, so `.env.example` can list the key
        // without turning the assistant on by accident.
        let groq_api_key = non_empty("GROQ_API_KEY");
        let groq_model =
            non_empty("GROQ_MODEL").unwrap_or_else(|| "openai/gpt-oss-20b".to_string());
        let groq_base_url = non_empty("GROQ_BASE_URL")
            .unwrap_or_else(|| "https://api.groq.com/openai/v1".to_string());
        // Missing variable: local sidecar. Present but blank: voice off.
        // `non_empty` cannot tell those apart, because a missing variable
        // and a blank one both come back as `None`.
        let voice_url = match env::var("VOICE_URL") {
            Ok(value) if value.trim().is_empty() => None,
            Ok(value) => Some(value.trim().to_string()),
            Err(_) => Some("http://localhost:8000".to_string()),
        };

        Self {
            database_url,
            mqtt_host,
            mqtt_port,
            groq_api_key,
            groq_model,
            groq_base_url,
            voice_url,
        }
    }
}

fn non_empty(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
