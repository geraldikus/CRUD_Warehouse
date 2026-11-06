//! Types shared by the HTTP API and the desktop client.
//!
//! The API owns the database and MQTT publishing. The client only speaks
//! JSON and topic names, so this crate stays free of axum, sqlx, and egui.

pub mod models;
pub mod topics;
pub mod validation;
