use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One row of the `items` table, also the JSON we send back.
///
/// `Serialize` writes this struct to JSON. We do not implement `Deserialize`
/// here: clients do not send `id` or timestamps when they create an item.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub id: Uuid,
    pub name: String,
    pub quantity: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// JSON body for `POST /items`.
///
/// `Deserialize` builds this from the request body. Unknown extra fields
/// are ignored by serde's default.
#[derive(Debug, Deserialize)]
pub struct CreateItem {
    pub name: String,
    pub quantity: i32,
}

/// JSON body for `PUT /items/{id}`. Both fields are required.
#[derive(Debug, Deserialize)]
pub struct UpdateItem {
    pub name: String,
    pub quantity: i32,
}
