//! Library crate for the warehouse API.
//!
//! `main.rs` starts the HTTP server. Integration tests in `tests/` cannot
//! import a binary, so they call [`app`] and send requests in-process.

pub mod config;
pub mod db;
pub mod error;
pub mod handlers;
pub mod models;
pub mod routes;
pub mod validation;

use sqlx::PgPool;

/// Build the HTTP router with a Postgres connection pool.
pub fn app(pool: PgPool) -> axum::Router {
    routes::router(pool)
}
