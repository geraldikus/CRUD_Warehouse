//! The assistant talks to a fake Groq on localhost. No test calls Groq.

use std::collections::VecDeque;
use std::env;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::routing::post;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgConnection, PgPool};
use tower::ServiceExt;
use warehouse::llm::GroqClient;
use warehouse::mqtt::MqttPublisher;
use warehouse::{AppState, app};

/// Same key as `tests/api.rs`.
const TEST_DB_LOCK: i64 = 424242;

static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Recorded {
    path: String,
    api_key: Option<String>,
    body: Value,
}

struct FakeGroq {
    responses: Mutex<VecDeque<Value>>,
    requests: Mutex<Vec<Recorded>>,
}

async fn setup() -> Option<(PgPool, PgConnection)> {
    dotenvy::dotenv().ok();

    let url = match env::var("TEST_DATABASE_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!(
                "skipping integration test: TEST_DATABASE_URL is not set. \
                 Copy .env.example to .env, run `docker compose up -d`, \
                 then `sqlx migrate run`."
            );
            return None;
        }
    };

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("failed to connect to TEST_DATABASE_URL");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("failed to run migrations on the test database");

    let mut lock = PgConnection::connect(&url)
        .await
        .expect("failed to open the test lock connection");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(TEST_DB_LOCK)
        .execute(&mut lock)
        .await
        .expect("failed to lock the test database");

    sqlx::query("TRUNCATE items")
        .execute(&pool)
        .await
        .expect("failed to empty the items table");

    Some((pool, lock))
}

async fn spawn_fake(script: Vec<Value>) -> (String, Arc<FakeGroq>) {
    let state = Arc::new(FakeGroq {
        responses: Mutex::new(VecDeque::from(script)),
        requests: Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .fallback(post(fake_generate))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake groq");
    let addr = listener.local_addr().expect("fake groq address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("fake groq server");
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("fake groq did not accept connections");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    (format!("http://{addr}/openai/v1"), state)
}

async fn fake_generate(State(state): State<Arc<FakeGroq>>, request: Request) -> Json<Value> {
    let path = request.uri().path().to_string();
    let api_key = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_string);
    let bytes = request
        .into_body()
        .collect()
        .await
        .expect("fake groq body")
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("fake groq json");

    state.requests.lock().expect("requests").push(Recorded {
        path,
        api_key,
        body,
    });
    let next = state
        .responses
        .lock()
        .expect("responses")
        .pop_front()
        .unwrap_or_else(|| text_response("no more scripted replies"));
    Json(next)
}

fn text_response(text: &str) -> Value {
    json!({
        "choices": [{
            "message": { "role": "assistant", "content": text }
        }]
    })
}

fn call_response(name: &str, args: Value) -> Value {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(&args).unwrap()
                    }
                }]
            }
        }]
    })
}

fn assistant_app(pool: PgPool, base_url: &str) -> axum::Router {
    app(AppState {
        pool,
        mqtt: MqttPublisher::disabled(),
        llm: Some(GroqClient::new("test-key", "openai/gpt-oss-20b", base_url)),
        voice: None,
    })
}

async fn send(app: &axum::Router, request: Request) -> (StatusCode, Option<Value>) {
    let response = app.clone().oneshot(request).await.expect("request failed");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("failed to read body")
        .to_bytes();
    if bytes.is_empty() {
        return (status, None);
    }
    let body = serde_json::from_slice(&bytes).expect("response was not json");
    (status, Some(body))
}

fn json_request(method: &str, uri: &str, body: Value) -> Request {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn empty_request(method: &str, uri: &str) -> Request {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn ask(text: &str) -> Request {
    json_request(
        "POST",
        "/assistant",
        json!({ "messages": [{ "role": "user", "text": text }] }),
    )
}

fn function_error(recorded: &Recorded) -> String {
    recorded
        .body
        .pointer("/messages")
        .and_then(|messages| messages.as_array())
        .into_iter()
        .flatten()
        .find(|message| message["role"] == "tool")
        .and_then(|message| message["content"].as_str())
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn create_tool_writes_a_row_and_echoes_the_model_content() {
    let _guard = DB_LOCK.lock().await;
    let Some((pool, _lock)) = setup().await else {
        return;
    };
    let (base_url, fake) = spawn_fake(vec![
        call_response("create_item", json!({"name": " bolts ", "quantity": 3})),
        text_response("Added 3 bolts."),
    ])
    .await;
    let app = assistant_app(pool, &base_url);

    let (status, body) = send(&app, ask("add 3 bolts")).await;
    assert_eq!(status, StatusCode::OK);
    let body = body.expect("reply");
    assert_eq!(body["reply"], "Added 3 bolts.");
    assert_eq!(body["changed"], true);
    assert!(body["pending_delete"].is_null());
    assert!(body["pending_add"].is_null());

    let (status, body) = send(&app, empty_request("GET", "/items")).await;
    assert_eq!(status, StatusCode::OK);
    let items = body.expect("items").as_array().unwrap().clone();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "bolts");
    assert_eq!(items[0]["quantity"], 3);

    let recorded = fake.requests.lock().expect("requests");
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].api_key.as_deref(), Some("test-key"));
    assert!(
        recorded[0].path.contains("/chat/completions"),
        "{}",
        recorded[0].path
    );
    let second = recorded[1].body.to_string();
    assert!(second.contains("call_1"), "{second}");
    assert!(second.contains("\"role\":\"tool\""), "{second}");
    assert!(second.contains("bolts"), "{second}");
}

#[tokio::test]
async fn invalid_quantity_is_returned_to_the_model() {
    let _guard = DB_LOCK.lock().await;
    let Some((pool, _lock)) = setup().await else {
        return;
    };
    let (base_url, fake) = spawn_fake(vec![
        call_response("create_item", json!({"name": "bolts", "quantity": -1})),
        text_response("That quantity is not allowed."),
    ])
    .await;
    let app = assistant_app(pool, &base_url);

    let (status, body) = send(&app, ask("add negative bolts")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.expect("reply")["changed"], false);

    let (status, body) = send(&app, empty_request("GET", "/items")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.expect("items").as_array().unwrap().is_empty());

    let recorded = fake.requests.lock().expect("requests");
    let error = function_error(&recorded[1]);
    assert!(error.contains("quantity"), "{error}");
}

#[tokio::test]
async fn delete_tool_waits_for_confirmation() {
    let _guard = DB_LOCK.lock().await;
    let Some((pool, _lock)) = setup().await else {
        return;
    };
    let app_for_create = app(AppState {
        pool: pool.clone(),
        mqtt: MqttPublisher::disabled(),
        llm: None,
        voice: None,
    });
    let (status, body) = send(
        &app_for_create,
        json_request("POST", "/items", json!({"name": "bolts", "quantity": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = body.expect("created")["id"].as_str().unwrap().to_string();

    let (base_url, _fake) = spawn_fake(vec![
        call_response("delete_item", json!({ "id": id })),
        text_response("Please confirm."),
    ])
    .await;
    let app = assistant_app(pool, &base_url);

    let (status, body) = send(&app, ask("delete bolts")).await;
    assert_eq!(status, StatusCode::OK);
    let body = body.expect("reply");
    assert_eq!(body["reply"], "Please confirm.");
    assert_eq!(body["changed"], false);
    assert_eq!(body["pending_delete"]["id"], id);
    assert_eq!(body["pending_delete"]["name"], "bolts");
    assert!(body["pending_add"].is_null());

    let (status, body) = send(&app, empty_request("GET", &format!("/items/{id}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.expect("item")["name"], "bolts");
}

#[tokio::test]
async fn create_tool_asks_when_the_name_already_exists() {
    let _guard = DB_LOCK.lock().await;
    let Some((pool, _lock)) = setup().await else {
        return;
    };
    let app_for_create = app(AppState {
        pool: pool.clone(),
        mqtt: MqttPublisher::disabled(),
        llm: None,
        voice: None,
    });
    let (status, body) = send(
        &app_for_create,
        json_request("POST", "/items", json!({"name": "Dish", "quantity": 10})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = body.expect("created")["id"].as_str().unwrap().to_string();

    let (base_url, _fake) = spawn_fake(vec![
        call_response("create_item", json!({"name": " dish ", "quantity": 2})),
        text_response("Dish is already on a row."),
    ])
    .await;
    let app = assistant_app(pool, &base_url);

    let (status, body) = send(&app, ask("add 2 Dish")).await;
    assert_eq!(status, StatusCode::OK);
    let body = body.expect("reply");
    assert_eq!(body["reply"], "Dish is already on a row.");
    assert_eq!(body["changed"], false);
    assert!(body["pending_delete"].is_null());
    assert_eq!(body["pending_add"]["name"], "dish");
    assert_eq!(body["pending_add"]["quantity"], 2);
    assert_eq!(body["pending_add"]["matches"][0]["id"], id);
    assert_eq!(body["pending_add"]["matches"][0]["quantity"], 10);

    let (status, body) = send(&app, empty_request("GET", "/items")).await;
    assert_eq!(status, StatusCode::OK);
    let items = body.expect("items").as_array().unwrap().clone();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["quantity"], 10);
}

#[tokio::test]
async fn add_fallback_when_model_only_replies() {
    let _guard = DB_LOCK.lock().await;
    let Some((pool, _lock)) = setup().await else {
        return;
    };
    let app_for_create = app(AppState {
        pool: pool.clone(),
        mqtt: MqttPublisher::disabled(),
        llm: None,
        voice: None,
    });
    let (status, body) = send(
        &app_for_create,
        json_request("POST", "/items", json!({"name": "cow", "quantity": 3})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = body.expect("created")["id"].as_str().unwrap().to_string();

    let (base_url, _fake) =
        spawn_fake(vec![text_response("Press Add to existing or Create new.")]).await;
    let app = assistant_app(pool, &base_url);

    let (status, body) = send(&app, ask("add 10 cow")).await;
    assert_eq!(status, StatusCode::OK);
    let body = body.expect("reply");
    assert_eq!(body["changed"], false);
    assert!(body["pending_delete"].is_null());
    assert_eq!(body["pending_add"]["name"], "cow");
    assert_eq!(body["pending_add"]["quantity"], 10);
    assert_eq!(body["pending_add"]["matches"][0]["id"], id);
    assert_eq!(body["pending_add"]["matches"][0]["quantity"], 3);

    let (status, body) = send(&app, empty_request("GET", &format!("/items/{id}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.expect("item")["quantity"], 3);
}

#[tokio::test]
async fn assistant_without_a_key_is_unavailable() {
    let _guard = DB_LOCK.lock().await;
    let Some((pool, _lock)) = setup().await else {
        return;
    };
    let app = app(AppState {
        pool,
        mqtt: MqttPublisher::disabled(),
        llm: None,
        voice: None,
    });

    let (status, body) = send(&app, ask("hello")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body.expect("error")["error"], "assistant is not configured");
}

#[tokio::test]
async fn empty_transcript_is_rejected_before_calling_the_model() {
    let _guard = DB_LOCK.lock().await;
    let Some((pool, _lock)) = setup().await else {
        return;
    };
    let (base_url, fake) = spawn_fake(vec![]).await;
    let app = assistant_app(pool, &base_url);

    let (status, body) = send(
        &app,
        json_request("POST", "/assistant", json!({ "messages": [] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.expect("error")["error"]
            .as_str()
            .unwrap()
            .contains("messages")
    );
    assert!(fake.requests.lock().expect("requests").is_empty());
}
