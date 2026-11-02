use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Html;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppState;
use crate::error::AppError;
use crate::models::{CreateItem, Item, UpdateItem};
use crate::validation::validate_item;

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
    // `query_as!` checks this SQL against the real database at compile time
    // and maps each column onto a field of `Item`.
    let items = sqlx::query_as!(
        Item,
        r#"
        SELECT id, name, quantity, created_at, updated_at, expires_at
        FROM items
        ORDER BY created_at DESC
        "#
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(items))
}

/// `POST /items`. Returns 201 and the created row.
pub async fn create(
    State(state): State<AppState>,
    Json(input): Json<CreateItem>,
) -> Result<(StatusCode, Json<Item>), AppError> {
    // `trim()` borrows `input.name`. The query below copies the trimmed
    // text into the SQL message, so the borrow only has to last until
    // `fetch_one` is done.
    let name = input.name.trim();
    validate_item(name, input.quantity)?;

    let item = sqlx::query_as!(
        Item,
        r#"
        INSERT INTO items (name, quantity, expires_at)
        VALUES ($1, $2, $3)
        RETURNING id, name, quantity, created_at, updated_at, expires_at
        "#,
        name,
        input.quantity,
        input.expires_at
    )
    .fetch_one(&state.pool)
    .await?;

    // The row is already saved. A broker failure only logs a warning.
    state.mqtt.publish_created(&item).await;

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
    // `fetch_optional` returns `Result<Option<Item>, sqlx::Error>`.
    // `?` converts the database error. `ok_or` turns `None` into 404.
    // `Option` is Rust's stand-in for "maybe missing" — there is no `null`.
    let item = sqlx::query_as!(
        Item,
        r#"
        SELECT id, name, quantity, created_at, updated_at, expires_at
        FROM items
        WHERE id = $1
        "#,
        id
    )
    .fetch_optional(&state.pool)
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
    let name = input.name.trim();
    validate_item(name, input.quantity)?;

    let item = sqlx::query_as!(
        Item,
        r#"
        UPDATE items
        SET name = $1,
            quantity = $2,
            expires_at = $3,
            -- Same date keeps the notices we already sent.
            -- A new date, or clearing it, starts the ladder over.
            expiry_notice = CASE
                WHEN expires_at IS NOT DISTINCT FROM $3 THEN expiry_notice
                ELSE 'none'
            END,
            updated_at = now()
        WHERE id = $4
        RETURNING id, name, quantity, created_at, updated_at, expires_at
        "#,
        name,
        input.quantity,
        input.expires_at,
        id
    )
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    state.mqtt.publish_updated(&item).await;

    Ok(Json(item))
}

/// `DELETE /items/{id}`. 204 when a row was removed, 404 when it was not.
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    // `query!` is the compile-time-checked query that does not map rows
    // onto a struct. We only need to know whether a row disappeared.
    let result = sqlx::query!(
        r#"
        DELETE FROM items
        WHERE id = $1
        "#,
        id
    )
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }

    state.mqtt.publish_deleted(id).await;

    Ok(StatusCode::NO_CONTENT)
}
