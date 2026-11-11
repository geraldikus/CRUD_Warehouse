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
    /// `GROQ_API_KEY` is missing. The rest of the API still works.
    AssistantUnavailable,
    /// Groq refused the call, timed out, or returned a body we cannot use.
    /// The string is for the log. The HTTP body stays generic.
    Llm(String),
    /// `VOICE_URL` is empty. Item routes and the text assistant still work.
    VoiceUnavailable,
    /// The speech sidecar refused the call or could not be reached.
    /// The string is for the log. The HTTP body stays generic.
    Voice(String),
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

/// Lets `?` turn a shared validation failure into HTTP 400.
///
/// The desktop client imports the same `ValidationError` and shows it
/// before the request. The handler still runs the check, because a client
/// can skip its own UI and post JSON directly.
impl From<warehouse_core::validation::ValidationError> for AppError {
    fn from(err: warehouse_core::validation::ValidationError) -> Self {
        AppError::Validation(err.to_string())
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
            AppError::AssistantUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "assistant is not configured".to_string(),
            ),
            AppError::Llm(message) => {
                tracing::error!(error = %message, "assistant request failed");
                (
                    StatusCode::BAD_GATEWAY,
                    "assistant request failed".to_string(),
                )
            }
            AppError::VoiceUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "voice is not configured".to_string(),
            ),
            AppError::Voice(message) => {
                tracing::error!(error = %message, "voice request failed");
                (StatusCode::BAD_GATEWAY, "voice request failed".to_string())
            }
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
