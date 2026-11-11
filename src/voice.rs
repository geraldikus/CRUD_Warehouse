//! Gateway to the local speech sidecar.
//!
//! The window never talks to Vosk or Piper directly. It opens a websocket
//! here, and this process opens a second one to the sidecar. Text-to-speech
//! is the same idea as an HTTP reverse proxy.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;

use crate::AppState;
use crate::error::AppError;

/// One shared client. Cloning is cheap: `reqwest::Client` is an `Arc`.
#[derive(Clone)]
pub struct VoiceClient {
    http: reqwest::Client,
    http_base: String,
    ws_base: String,
}

impl VoiceClient {
    pub fn new(base_url: &str) -> Self {
        let http_base = base_url.trim().trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client");
        Self {
            ws_base: to_ws_base(&http_base),
            http_base,
            http,
        }
    }

    /// `GET /health`. Startup uses this to log a warning. A failure does
    /// not disable the client: the sidecar may still be downloading models.
    pub async fn probe(&self) -> Result<(), String> {
        let response = self
            .http
            .get(format!("{}/health", self.http_base))
            .send()
            .await
            .map_err(|err| err.to_string())?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("voice health {}", response.status()))
        }
    }

    pub async fn synthesize(&self, text: &str) -> Result<Vec<u8>, String> {
        let response = self
            .http
            .post(format!("{}/tts", self.http_base))
            .json(&serde_json::json!({ "text": text }))
            .send()
            .await
            .map_err(|err| err.to_string())?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("voice HTTP {status}: {}", clip(&body)));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|err| err.to_string())
    }

    fn stream_url(&self) -> String {
        format!("{}/stream", self.ws_base)
    }
}

fn to_ws_base(http_base: &str) -> String {
    if let Some(rest) = http_base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = http_base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        http_base.to_string()
    }
}

fn clip(body: &str) -> String {
    let mut clipped: String = body.chars().take(300).collect();
    if body.chars().count() > 300 {
        clipped.push_str("...");
    }
    clipped
}

#[derive(Deserialize)]
pub(crate) struct TtsRequest {
    text: String,
}

/// Websocket upgrade. The sidecar URL is resolved before the upgrade so a
/// missing `VOICE_URL` is a normal 503 instead of a socket that opens and
/// immediately dies.
pub(crate) async fn stream(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let voice = state.voice.clone().ok_or(AppError::VoiceUnavailable)?;
    Ok(ws.on_upgrade(move |socket| proxy(socket, voice)))
}

async fn proxy(mut socket: WebSocket, voice: VoiceClient) {
    let upstream = match tokio_tungstenite::connect_async(voice.stream_url()).await {
        Ok((stream, _)) => stream,
        Err(err) => {
            tracing::warn!(error = %err, "voice sidecar unavailable");
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: 1011,
                    reason: "voice sidecar unavailable".into(),
                })))
                .await;
            return;
        }
    };

    let (mut client_tx, mut client_rx) = socket.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();

    loop {
        tokio::select! {
            incoming = client_rx.next() => {
                match incoming {
                    Some(Ok(Message::Binary(data))) => {
                        if upstream_tx
                            .send(tokio_tungstenite::tungstenite::Message::Binary(data))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if upstream_tx
                            .send(tokio_tungstenite::tungstenite::Message::Text(text.to_string().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        let _ = client_tx.send(Message::Pong(data)).await;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                }
            }
            incoming = upstream_rx.next() => {
                use tokio_tungstenite::tungstenite::Message as Upstream;
                match incoming {
                    Some(Ok(Upstream::Binary(data))) => {
                        if client_tx.send(Message::Binary(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Upstream::Text(text))) => {
                        if client_tx
                            .send(Message::Text(text.to_string().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Upstream::Ping(data))) => {
                        let _ = upstream_tx.send(Upstream::Pong(data)).await;
                    }
                    Some(Ok(Upstream::Pong(_))) => {}
                    Some(Ok(Upstream::Close(_))) | Some(Ok(Upstream::Frame(_))) | None | Some(Err(_)) => {
                        break;
                    }
                }
            }
        }
    }
}

pub(crate) async fn tts(
    State(state): State<AppState>,
    Json(request): Json<TtsRequest>,
) -> Result<Response, AppError> {
    let text = request.text.trim();
    if text.is_empty() {
        return Err(AppError::Validation("text must not be empty".to_string()));
    }
    if text.chars().count() > 2000 {
        return Err(AppError::Validation("text is too long".to_string()));
    }
    let voice = state.voice.as_ref().ok_or(AppError::VoiceUnavailable)?;
    let wav = voice.synthesize(text).await.map_err(AppError::Voice)?;
    Ok(([(axum::http::header::CONTENT_TYPE, "audio/wav")], wav).into_response())
}
