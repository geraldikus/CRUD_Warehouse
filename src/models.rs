//! Row types live in `warehouse-core` so the desktop client and the API
//! share one JSON shape. Handlers still name them as `crate::models::Item`.
//!
//! `query_as!` fills the public fields of that struct. It does not need
//! sqlx to be a dependency of the core crate.

pub use warehouse_core::models::{
    AssistantReply, AssistantRequest, ChatMessage, ChatRole, CreateItem, Item, PendingDelete,
    UpdateItem,
};
