//! Speech routes talk to a fake sidecar. No test loads Vosk or Piper.

use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use warehouse::mqtt::MqttPublisher;
use warehouse::voice::VoiceClient;
use warehouse::{AppState, app};

fn lazy_pool() -> PgPool {
    PgPoolOptions::new()
        .connect_lazy("postgres://warehouse:warehouse@127.0.0.1:1/warehouse")
        .expect("lazy pool")
}

fn warehouse(voice: Option<VoiceClient>) -> axum::Router {
    app(AppState {
        pool: lazy_pool(),
        mqtt: MqttPublisher::disabled(),
        llm: None,
        voice,
    })
}

async fn spawn(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("address");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("server did not accept connections");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    format!("http://{addr}")
}

async fn fake_stream(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(read_one_chunk)
}

async fn read_one_chunk(mut socket: WebSocket) {
    while let Some(Ok(message)) = socket.recv().await {
        if let Message::Binary(_) = message {
            let _ = socket
                .send(Message::Text(
                    r#"{"type":"transcript","text":"add three bolts"}"#.into(),
                ))
                .await;
        }
    }
}

async fn fake_tts(Json(body): Json<Value>) -> impl IntoResponse {
    assert!(
        !body["text"].as_str().unwrap_or("").is_empty(),
        "sidecar received an empty phrase"
    );
    (
        [(axum::http::header::CONTENT_TYPE, "audio/wav")],
        b"RIFF-fake".to_vec(),
    )
}

async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = app.clone().oneshot(request).await.expect("request failed");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("failed to read body")
        .to_bytes();
    (status, bytes.to_vec())
}

#[tokio::test]
async fn transcript_and_tts_are_proxied() {
    let sidecar = Router::new()
        .route("/stream", get(fake_stream))
        .route("/tts", post(fake_tts));
    let sidecar_url = spawn(sidecar).await;
    let api_url = spawn(warehouse(Some(VoiceClient::new(&sidecar_url)))).await;

    let (mut socket, _) = tokio_tungstenite::connect_async(format!(
        "{}/voice/stream",
        api_url.replacen("http://", "ws://", 1)
    ))
    .await
    .expect("websocket");
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            vec![1, 2, 3].into(),
        ))
        .await
        .expect("send audio");
    let incoming = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("timed out waiting for a transcript")
        .expect("socket closed")
        .expect("socket error");
    let text = match incoming {
        tokio_tungstenite::tungstenite::Message::Text(text) => text.to_string(),
        other => panic!("expected a text frame, got {other:?}"),
    };
    assert!(text.contains("add three bolts"), "{text}");

    let response = reqwest::Client::new()
        .post(format!("{api_url}/voice/tts"))
        .json(&json!({ "text": "Added three bolts." }))
        .send()
        .await
        .expect("tts request");
    assert_eq!(response.status(), StatusCode::OK);
    let wav = response.bytes().await.expect("wav");
    assert_eq!(&wav[..], b"RIFF-fake");
}

#[tokio::test]
async fn voice_without_a_url_is_unavailable() {
    let router = warehouse(None);
    let request = Request::builder()
        .method("POST")
        .uri("/voice/tts")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"hello"}"#))
        .unwrap();
    let (status, body) = send(&router, request).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let json: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"], "voice is not configured");

    let api_url = spawn(warehouse(None)).await;
    let error = tokio_tungstenite::connect_async(format!(
        "{}/voice/stream",
        api_url.replacen("http://", "ws://", 1)
    ))
    .await
    .expect_err("upgrade should be refused");
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
        other => panic!("expected HTTP 503, got {other}"),
    }
}

#[tokio::test]
async fn dead_sidecar_is_a_bad_gateway_and_closes_the_socket() {
    let api_url = spawn(warehouse(Some(VoiceClient::new("http://127.0.0.1:1")))).await;

    let response = reqwest::Client::new()
        .post(format!("{api_url}/voice/tts"))
        .json(&json!({ "text": "hello" }))
        .send()
        .await
        .expect("tts request");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let json: Value = response.json().await.expect("json");
    assert_eq!(json["error"], "voice request failed");

    let (mut socket, _) = tokio_tungstenite::connect_async(format!(
        "{}/voice/stream",
        api_url.replacen("http://", "ws://", 1)
    ))
    .await
    .expect("upgrade");
    let incoming = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("timed out waiting for close");
    match incoming {
        None => {}
        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(frame))) => {
            let frame = frame.expect("close frame");
            assert_eq!(u16::from(frame.code), 1011);
            assert!(frame.reason.contains("unavailable"), "{}", frame.reason);
        }
        Some(other) => panic!("expected the socket to close, got {other:?}"),
    }
}

#[tokio::test]
async fn empty_tts_text_is_rejected_before_the_sidecar() {
    let router = warehouse(Some(VoiceClient::new("http://127.0.0.1:1")));
    let request = Request::builder()
        .method("POST")
        .uri("/voice/tts")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"text":"   "}"#))
        .unwrap();
    let (status, body) = send(&router, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let json: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["error"], "text must not be empty");
}
