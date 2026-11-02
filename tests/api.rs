use std::env;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;
use warehouse::mqtt::MqttPublisher;
use warehouse::{AppState, app};

/// One lock so two tests never truncate the same table at the same time.
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Connect to the test database, apply migrations, and empty `items`.
///
/// Returns `None` when `TEST_DATABASE_URL` is missing. The test then
/// returns without failing, and prints how to enable it.
async fn setup() -> Option<PgPool> {
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

    sqlx::query("TRUNCATE items")
        .execute(&pool)
        .await
        .expect("failed to empty the items table");

    Some(pool)
}

/// HTTP tests do not need a broker. `Disabled` drops every publish.
fn test_app(pool: PgPool) -> axum::Router {
    app(AppState {
        pool,
        mqtt: MqttPublisher::disabled(),
    })
}

async fn send(app: &axum::Router, request: Request<Body>) -> (StatusCode, Option<Value>) {
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

fn json_request(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn empty_request(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn index_page_is_html() {
    let _guard = DB_LOCK.lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let response = test_app(pool)
        .oneshot(empty_request("GET", "/"))
        .await
        .expect("request failed");

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .expect("content-type")
        .to_str()
        .expect("content-type text");
    assert!(content_type.starts_with("text/html"));

    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("failed to read body")
        .to_bytes();
    let html = String::from_utf8(bytes.to_vec()).expect("html is utf-8");
    assert!(html.contains("<title>Warehouse</title>"));
}

#[tokio::test]
async fn health_and_crud_cycle() {
    let _guard = DB_LOCK.lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let app = test_app(pool);

    let (status, body) = send(&app, empty_request("GET", "/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.unwrap()["status"], "ok");

    let (status, body) = send(
        &app,
        json_request("POST", "/items", json!({"name": " bolts ", "quantity": 10})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let created = body.expect("create body");
    assert_eq!(created["name"], "bolts");
    assert_eq!(created["quantity"], 10);
    assert!(created["expires_at"].is_null());
    let id = created["id"].as_str().expect("id string").to_string();

    let (status, body) = send(&app, empty_request("GET", "/items")).await;
    assert_eq!(status, StatusCode::OK);
    let list = body.expect("list body");
    assert_eq!(list.as_array().expect("array").len(), 1);

    let (status, body) = send(&app, empty_request("GET", &format!("/items/{id}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.expect("get body")["name"], "bolts");

    let (status, body) = send(
        &app,
        json_request(
            "PUT",
            &format!("/items/{id}"),
            json!({"name": "nuts", "quantity": 4}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let updated = body.expect("update body");
    assert_eq!(updated["name"], "nuts");
    assert_eq!(updated["quantity"], 4);

    let (status, body) = send(&app, empty_request("DELETE", &format!("/items/{id}"))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_none());

    let (status, body) = send(&app, empty_request("GET", &format!("/items/{id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body.expect("404 body")["error"], "item not found");
}

#[tokio::test]
async fn rejects_invalid_items() {
    let _guard = DB_LOCK.lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let app = test_app(pool);

    let (status, body) = send(
        &app,
        json_request("POST", "/items", json!({"name": "   ", "quantity": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.expect("error body")["error"]
            .as_str()
            .unwrap()
            .contains("name")
    );

    let (status, body) = send(
        &app,
        json_request("POST", "/items", json!({"name": "bolts", "quantity": -5})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.expect("error body")["error"]
            .as_str()
            .unwrap()
            .contains("quantity")
    );

    let missing = "00000000-0000-0000-0000-000000000000";
    let (status, _) = send(
        &app,
        json_request(
            "PUT",
            &format!("/items/{missing}"),
            json!({"name": "bolts", "quantity": 1}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(&app, empty_request("DELETE", &format!("/items/{missing}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn optional_expiry_date() {
    let _guard = DB_LOCK.lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let app = test_app(pool);

    let (status, body) = send(
        &app,
        json_request(
            "POST",
            "/items",
            json!({"name": "milk", "quantity": 2, "expires_at": "2026-12-01T12:00:00Z"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let created = body.expect("create body");
    assert_eq!(created["expires_at"], "2026-12-01T12:00:00Z");
    let id = created["id"].as_str().expect("id").to_string();

    let (status, body) = send(
        &app,
        json_request(
            "PUT",
            &format!("/items/{id}"),
            json!({"name": "milk", "quantity": 2, "expires_at": null}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.expect("update body")["expires_at"].is_null());
}
