//! Field checks live in `warehouse-core`.
//!
//! `?` in a handler turns [`ValidationError`] into [`crate::error::AppError`]
//! through `From`. The HTTP status stays 400.

pub use warehouse_core::validation::{ValidationError, validate_item};
