use std::time::Duration;

use serde::Deserialize;
use uuid::Uuid;
use warehouse_core::models::{AssistantReply, AssistantRequest, CreateItem, Item, UpdateItem};

#[derive(Debug)]
pub(crate) enum ApiError {
    Transport(String),
    Message(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) | Self::Message(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ApiError {}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}

/// HTTP client for the warehouse API. One instance, shared by the worker task.
pub(crate) struct ApiClient {
    http: reqwest::Client,
    base: String,
}

impl ApiClient {
    pub(crate) fn new(base: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
        }
    }

    pub(crate) async fn list(&self) -> Result<Vec<Item>, ApiError> {
        let response = self
            .http
            .get(format!("{}/items", self.base))
            .send()
            .await
            .map_err(transport)?;
        read_json(response).await
    }

    pub(crate) async fn create(&self, input: &CreateItem) -> Result<Item, ApiError> {
        let response = self
            .http
            .post(format!("{}/items", self.base))
            .json(input)
            .send()
            .await
            .map_err(transport)?;
        read_json(response).await
    }

    pub(crate) async fn update(&self, id: Uuid, input: &UpdateItem) -> Result<Item, ApiError> {
        let response = self
            .http
            .put(format!("{}/items/{id}", self.base))
            .json(input)
            .send()
            .await
            .map_err(transport)?;
        read_json(response).await
    }

    /// The assistant may call Groq several times, so this request waits
    /// longer than a normal item write. The client timeout stays at 10s.
    pub(crate) async fn ask(&self, request: &AssistantRequest) -> Result<AssistantReply, ApiError> {
        let response = self
            .http
            .post(format!("{}/assistant", self.base))
            .json(request)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .map_err(transport)?;
        read_json(response).await
    }

    pub(crate) async fn delete(&self, id: Uuid) -> Result<(), ApiError> {
        let response = self
            .http
            .delete(format!("{}/items/{id}", self.base))
            .send()
            .await
            .map_err(transport)?;
        if response.status().is_success() {
            return Ok(());
        }
        Err(ApiError::Message(error_message(response).await))
    }
}

fn transport(err: reqwest::Error) -> ApiError {
    ApiError::Transport(err.to_string())
}

async fn read_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, ApiError> {
    if !response.status().is_success() {
        return Err(ApiError::Message(error_message(response).await));
    }
    response.json().await.map_err(transport)
}

async fn error_message(response: reqwest::Response) -> String {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    serde_json::from_str::<ErrorBody>(&text)
        .map(|body| body.error)
        .unwrap_or_else(|_| {
            if text.is_empty() {
                format!("HTTP {status}")
            } else {
                text
            }
        })
}
