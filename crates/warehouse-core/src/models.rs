use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One row of the `items` table, also the JSON on the wire.
///
/// `Serialize` writes this struct to JSON. `Deserialize` reads it back.
/// The desktop client needs both. Create and update bodies are separate
/// structs, so a client still cannot send `id` or timestamps as if they
/// were input fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: Uuid,
    pub name: String,
    pub quantity: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// `None` means the item does not expire. SQL stores that as NULL.
    pub expires_at: Option<DateTime<Utc>>,
}

/// JSON body for `POST /items`.
///
/// The server builds this with `Deserialize`. The desktop client builds it
/// in Rust and writes it with `Serialize`. Unknown extra fields are ignored
/// by serde's default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateItem {
    pub name: String,
    pub quantity: i32,
    /// Missing field or JSON `null` both become `None`.
    pub expires_at: Option<DateTime<Utc>>,
}

/// JSON body for `PUT /items/{id}`. Name and quantity are required.
/// `expires_at` is optional: omit it or send `null` to clear the date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateItem {
    pub name: String,
    pub quantity: i32,
    pub expires_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_json_round_trip() {
        let json = r#"{
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "name": "bolts",
            "quantity": 10,
            "created_at": "2026-01-02T03:04:05Z",
            "updated_at": "2026-01-02T04:05:06Z",
            "expires_at": "2026-02-03T23:59:59Z"
        }"#;

        let item: Item = serde_json::from_str(json).expect("sample item json");
        let again: Item = serde_json::from_str(&serde_json::to_string(&item).unwrap()).unwrap();

        assert_eq!(item, again);
        assert_eq!(item.name, "bolts");
        assert_eq!(item.quantity, 10);
        assert!(item.expires_at.is_some());
    }

    #[test]
    fn create_item_treats_null_expiry_as_none() {
        let input: CreateItem =
            serde_json::from_str(r#"{"name":"bolts","quantity":1,"expires_at":null}"#).unwrap();

        assert_eq!(input.expires_at, None);
    }
}
