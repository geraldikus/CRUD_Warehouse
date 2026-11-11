//! Library crate for the warehouse API.
//!
//! `main.rs` starts the HTTP server. Integration tests in `tests/` cannot
//! import a binary, so they call [`app`] and send requests in-process.

mod assistant;
pub mod config;
pub mod db;
pub mod error;
pub mod expiry;
pub mod handlers;
pub mod llm;
pub mod models;
pub mod mqtt;
mod repo;
pub mod routes;
pub mod validation;
pub mod voice;

use sqlx::PgPool;

use crate::llm::GroqClient;
use crate::mqtt::MqttPublisher;
use crate::voice::VoiceClient;

/// Everything a request handler needs.
///
/// Axum stores one state value on the router. The pool, the MQTT client,
/// and the optional Groq client therefore travel together.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub mqtt: MqttPublisher,
    /// `None` when no API key was configured. Item routes ignore this field.
    pub llm: Option<GroqClient>,
    /// `None` when `VOICE_URL` is blank. Item routes ignore this field.
    pub voice: Option<VoiceClient>,
}

/// Build the HTTP router with the database pool and the MQTT publisher.
pub fn app(state: AppState) -> axum::Router {
    routes::router(state)
}
