//! Database writes shared by the HTTP handlers and the assistant.
//!
//! Both paths must trim the name, reject a bad quantity, reset
//! `expiry_notice` when the expiry date changes, and publish MQTT.
//! If those steps lived only in the handlers, a tool call could save a row
//! the REST API would have refused.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use crate::models::{CreateItem, Item, UpdateItem};
use crate::mqtt::MqttPublisher;
use crate::validation::validate_item;

pub async fn list_items(pool: &PgPool) -> Result<Vec<Item>, AppError> {
    let items = sqlx::query_as!(
        Item,
        r#"
        SELECT id, name, quantity, created_at, updated_at, expires_at
        FROM items
        ORDER BY created_at DESC
        "#
    )
    .fetch_all(pool)
    .await?;

    Ok(items)
}

pub async fn get_item(pool: &PgPool, id: Uuid) -> Result<Option<Item>, AppError> {
    let item = sqlx::query_as!(
        Item,
        r#"
        SELECT id, name, quantity, created_at, updated_at, expires_at
        FROM items
        WHERE id = $1
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;

    Ok(item)
}

/// Rows whose name matches after trim and case folding.
pub async fn find_items_by_name(pool: &PgPool, name: &str) -> Result<Vec<Item>, AppError> {
    let items = sqlx::query_as!(
        Item,
        r#"
        SELECT id, name, quantity, created_at, updated_at, expires_at
        FROM items
        WHERE lower(btrim(name)) = lower(btrim($1))
        ORDER BY created_at DESC
        "#,
        name
    )
    .fetch_all(pool)
    .await?;

    Ok(items)
}

/// Insert one row and announce it. The caller already parsed JSON.
pub async fn create_item(
    pool: &PgPool,
    mqtt: &MqttPublisher,
    input: CreateItem,
) -> Result<Item, AppError> {
    // `trim()` borrows `input.name`. The query copies the trimmed text into
    // the SQL message, so the borrow only has to last until `fetch_one` returns.
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
    .fetch_one(pool)
    .await?;

    // The row is already saved. A broker failure only logs a warning.
    mqtt.publish_created(&item).await;

    Ok(item)
}

pub async fn update_item(
    pool: &PgPool,
    mqtt: &MqttPublisher,
    id: Uuid,
    input: UpdateItem,
) -> Result<Item, AppError> {
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
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)?;

    mqtt.publish_updated(&item).await;

    Ok(item)
}

/// `NotFound` when the id is already gone, so the handler can answer 404.
pub async fn delete_item(pool: &PgPool, mqtt: &MqttPublisher, id: Uuid) -> Result<(), AppError> {
    let result = sqlx::query!(
        r#"
        DELETE FROM items
        WHERE id = $1
        "#,
        id
    )
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }

    mqtt.publish_deleted(id).await;

    Ok(())
}
