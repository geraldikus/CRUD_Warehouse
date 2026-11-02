use std::env;

/// Settings the process needs before it can serve requests.
#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
}

impl Config {
    /// Load `.env` (if it exists) and read `DATABASE_URL`.
    ///
    /// This panics when the variable is missing. A server without a database
    /// URL cannot do its job, so failing at startup is clearer than failing
    /// on the first request.
    pub fn from_env() -> Self {
        // Ignore a missing file: the variable may already be set in the shell.
        // Do not override variables that are already set.
        dotenvy::dotenv().ok();

        let database_url = env::var("DATABASE_URL")
            .expect("DATABASE_URL is not set. Copy .env.example to .env or export the variable.");

        Self { database_url }
    }
}
