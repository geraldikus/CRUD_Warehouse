use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Every failure a handler can return.
///
/// Each variant carries the data that variant needs:
/// a database error, nothing (not found), or a message.
/// This is stronger than a TypeScript union of strings, because the
/// compiler checks that every `match` handles every variant.
#[derive(Debug)]
pub enum AppError {
    Database(sqlx::Error),
    NotFound,
    Validation(String),
    /// Reserved for failures that are not database errors.
    /// Nothing constructs this yet; kept so new code has one obvious bucket.
    #[allow(dead_code)]
    Internal(String),
}

/// Lets `?` turn a SQLx error into `AppError` automatically.
///
/// `From` is a trait (a shared contract, like a TypeScript interface).
/// When a function returns `Result<T, AppError>` and you write `db_call().await?`,
/// Rust looks for `From<sqlx::Error> for AppError` and converts the error.
impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        AppError::Database(err)
    }
}

/// Turns `AppError` into an HTTP response.
///
/// Axum calls this when a handler returns `Err(...)`.
/// Handlers can therefore return `Result<Json<T>, AppError>`.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::Database(err) => {
                // Log the real error for us. Send a generic message to the client
                // so SQL details and connection strings never leak.
                tracing::error!(error = %err, "database error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database error".to_string(),
                )
            }
            AppError::NotFound => (StatusCode::NOT_FOUND, "item not found".to_string()),
            AppError::Validation(message) => (StatusCode::BAD_REQUEST, message),
            AppError::Internal(message) => {
                tracing::error!(error = %message, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                )
            }
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}
