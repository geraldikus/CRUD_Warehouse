use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Open a pool of Postgres connections.
///
/// `database_url` is borrowed (`&str`): this function only reads it.
/// The pool copies the URL into its own memory, so the caller can drop
/// the original `String` after this function returns.
///
/// A pool is not one connection. It is a small set of connections that
/// handlers share. Cloning a `PgPool` is cheap: the clone is another
/// handle to the same pool (an `Arc` internally), not a new TCP connection.
pub async fn create_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
}
