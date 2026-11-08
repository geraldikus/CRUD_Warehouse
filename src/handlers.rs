use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppState;
use crate::error::AppError;
use crate::models::{CreateItem, Item, UpdateItem};
use crate::repo;

/// `GET /`. The demo page is compiled into the binary.
///
/// `include_str!` reads the file at compile time. The running server
/// does not need the `static/` folder next to it.
pub async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

/// `GET /health`. Does not touch the database.
///
/// The body is a plain JSON value. A private struct in the return type
/// would leak into the public function signature, which Rust rejects.
pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// `GET /items`.
///
/// `State<AppState>` is an extractor: Axum pulls the shared pool and MQTT
/// publisher out of the router. Both are cheap to clone.
pub async fn get_all(State(state): State<AppState>) -> Result<Json<Vec<Item>>, AppError> {
    let items = repo::list_items(&state.pool).await?;
    Ok(Json(items))
}

/// `POST /items`. Returns 201 and the created row.
pub async fn create(
    State(state): State<AppState>,
    Json(input): Json<CreateItem>,
) -> Result<(StatusCode, Json<Item>), AppError> {
    let item = repo::create_item(&state.pool, &state.mqtt, input).await?;
    Ok((StatusCode::CREATED, Json(item)))
}

/// `GET /items/{id}`.
///
/// These handlers do not declare a lifetime (`'a`). Axum gives each request
/// its own task, and the extractors (`State`, `Path`, `Json`) own their data.
/// The function does not hold a reference to the raw request, so there is
/// nothing for a lifetime to describe.
pub async fn get_one(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Item>, AppError> {
    // `Option` is Rust's stand-in for "maybe missing" — there is no `null`.
    let item = repo::get_item(&state.pool, id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(item))
}

/// `PUT /items/{id}`. Replaces name and quantity.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateItem>,
) -> Result<Json<Item>, AppError> {
    let item = repo::update_item(&state.pool, &state.mqtt, id, input).await?;
    Ok(Json(item))
}

/// `DELETE /items/{id}`. 204 when a row was removed, 404 when it was not.
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    repo::delete_item(&state.pool, &state.mqtt, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
