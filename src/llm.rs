//! HTTP client for Groq's OpenAI-compatible chat completions API.
//!
//! The API key stays in this process. Callers pass the conversation and the
//! tool declarations; this module does not know about warehouse items.

use std::time::Duration;

use serde_json::{Value, json};

/// One Groq client, shared by every request the way `PgPool` is shared.
///
/// Cloning is cheap: `reqwest::Client` is an `Arc` around the connection pool.
#[derive(Clone)]
pub struct GroqClient {
    http: reqwest::Client,
    key: String,
    model: String,
    base_url: String,
}

impl GroqClient {
    pub fn new(api_key: &str, model: &str, base_url: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client");
        Self {
            http,
            key: api_key.to_string(),
            model: model.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// `messages` is the OpenAI conversation, including the system prompt and
    /// any tool results from this same user message. `tools` is the JSON
    /// array Groq expects under the `tools` field.
    ///
    /// The key goes in the `Authorization` header, not the URL.
    pub async fn generate(&self, messages: &[Value], tools: &Value) -> Result<Value, String> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
            "temperature": 0.2
        });

        let response = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {}", self.key))
            .json(&body)
            .send()
            .await
            .map_err(|err| err.to_string())?;

        let status = response.status();
        let text = response.text().await.map_err(|err| err.to_string())?;
        if !status.is_success() {
            return Err(format!("groq HTTP {status}: {}", clip(&text)));
        }

        serde_json::from_str(&text).map_err(|err| format!("groq body was not json: {err}"))
    }
}

fn clip(body: &str) -> String {
    let mut clipped: String = body.chars().take(300).collect();
    if body.chars().count() > 300 {
        clipped.push_str("...");
    }
    clipped
}
