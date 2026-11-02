use axum::Router;
use axum::http::Request;
use axum::routing::get;
use tower_http::trace::TraceLayer;
use tracing::Span;

use crate::AppState;
use crate::handlers;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(handlers::index))
        .route("/health", get(handlers::health))
        .route("/items", get(handlers::get_all).post(handlers::create))
        .route(
            "/items/{id}",
            get(handlers::get_one)
                .put(handlers::update)
                .delete(handlers::delete),
        )
        // The trace layer runs around every handler and logs method + path.
        .layer(
            TraceLayer::new_for_http().on_request(|request: &Request<_>, _span: &Span| {
                tracing::info!(
                    method = %request.method(),
                    path = %request.uri().path(),
                    "incoming request"
                );
            }),
        )
        .with_state(state)
}
