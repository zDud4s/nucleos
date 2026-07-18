use crate::auth::Token;
use crate::runner::CommandRunner;
use sqlx::SqlitePool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub token: Token,
    pub pool: SqlitePool,
    pub runner: Arc<dyn CommandRunner>,
}
