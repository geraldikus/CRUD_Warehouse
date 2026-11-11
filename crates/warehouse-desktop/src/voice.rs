//! Websocket client for `GET /voice/stream` on the warehouse API.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;

use crate::worker::Event;

#[derive(Deserialize)]
struct VoiceMessage {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    message: Option<String>,
}

/// Forward microphone chunks until `stop` flips, and turn sidecar JSON into events.
///
/// `Ok` means the user stopped. `Err` means the socket died on its own.
pub(crate) async fn run_session(
    url: String,
    mut audio: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    mut stop: tokio::sync::watch::Receiver<bool>,
    muted: Arc<AtomicBool>,
    events: &std::sync::mpsc::Sender<Event>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let (mut socket, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|err| err.to_string())?;

    loop {
        tokio::select! {
            biased;
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    let _ = socket.close(None).await;
                    return Ok(());
                }
            }
            chunk = audio.recv() => {
                let Some(chunk) = chunk else {
                    return Ok(());
                };
                // Chunks queued before mute flipped would still be the reply
                // we just played. Drop them instead of forwarding.
                if muted.load(Ordering::SeqCst) {
                    continue;
                }
                if socket
                    .send(tokio_tungstenite::tungstenite::Message::Binary(chunk.into()))
                    .await
                    .is_err()
                {
                    return Err("voice connection closed".to_string());
                }
            }
            incoming = socket.next() => {
                match incoming {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                        handle_text(&text, events, ctx);
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(payload))) => {
                        if socket
                            .send(tokio_tungstenite::tungstenite::Message::Pong(payload))
                            .await
                            .is_err()
                        {
                            return Err("voice connection closed".to_string());
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => {
                        return Err("voice connection closed".to_string());
                    }
                    Some(Err(err)) => return Err(err.to_string()),
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

fn handle_text(text: &str, events: &std::sync::mpsc::Sender<Event>, ctx: &egui::Context) {
    let Ok(message) = serde_json::from_str::<VoiceMessage>(text) else {
        return;
    };
    match message.kind.as_str() {
        "transcript" => {
            let text = message.text.unwrap_or_default();
            if text.trim().is_empty() {
                return;
            }
            let _ = events.send(Event::VoiceTranscript(text));
            ctx.request_repaint();
        }
        "error" => {
            let _ = events.send(Event::VoiceError(
                message
                    .message
                    .unwrap_or_else(|| "voice request failed".to_string()),
            ));
            ctx.request_repaint();
        }
        _ => {}
    }
}
