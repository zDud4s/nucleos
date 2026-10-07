//! Which Ollama model embeds knowledge rows: the owner's choice, persisted, applied without a restart.
//!
//! The choice lives in a row of `schema_meta` (like `distill_model`) and, when present, wins over
//! `embedding_model` of `nucleos-models.yaml`, which stays the default of an untouched install.

use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use sqlx::SqlitePool;
use std::sync::Arc;

pub const SETTING_KEY: &str = "embedding.model";

/// The longest model name a request may carry; real Ollama names are far shorter.
const MAX_NAME: usize = 128;

/// PURE: the one spelling a request may use: a trimmed, non-empty Ollama model name with no
/// whitespace or control characters and no more than `MAX_NAME` bytes.
pub fn parse_choice(spelling: &str) -> Option<String> {
    let name = spelling.trim();
    let valid = !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.chars().any(|c| c.is_whitespace() || c.is_control());
    valid.then(|| name.to_string())
}

/// The stored choice, if the owner made one.
pub async fn read(pool: &SqlitePool) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM schema_meta WHERE key = ?")
        .bind(SETTING_KEY)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|stored| parse_choice(&stored))
}

pub async fn write(pool: &SqlitePool, model: &str) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO schema_meta (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(SETTING_KEY)
    .bind(model)
    .execute(pool)
    .await?;
    Ok(())
}

/// The model in force: the stored choice, else the file's, else the built-in default.
pub async fn effective(pool: &SqlitePool, file_model: &str) -> String {
    read(pool).await.unwrap_or_else(|| file_model.to_string())
}

/// PURE: whether `wanted` is among the models Ollama lists. A listing names an untagged model
/// `name:latest`, so `nomic-embed-text` matches `nomic-embed-text:latest`.
pub fn is_pulled(wanted: &str, listed: &[String]) -> bool {
    listed.iter().any(|name| {
        name == wanted || (!wanted.contains(':') && name.strip_suffix(":latest") == Some(wanted))
    })
}

/// `GET /config/embedding`: the model in force.
pub async fn get_embedding_config(State(state): State<AppState>) -> Json<serde_json::Value> {
    let model = match crate::embed::installed() {
        Some(embedder) => embedder.model().to_string(),
        None => effective(&state.pool, crate::config::DEFAULT_EMBEDDING_MODEL).await,
    };
    Json(serde_json::json!({ "model": model }))
}

/// The body of `POST /config/embedding`.
#[derive(serde::Deserialize)]
pub struct ModelBody {
    pub model: String,
}

/// `POST /config/embedding`: an unusable name is `422 {"error":"unknown_model"}` and never saved.
/// A saved name is installed at once and the backfill worker nudged, so rows embedded by another
/// model are re-embedded under this one.
pub async fn post_embedding_config(
    State(state): State<AppState>,
    Json(body): Json<ModelBody>,
) -> axum::response::Response {
    let Some(model) = parse_choice(&body.model) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "unknown_model" })),
        )
            .into_response();
    };
    if write(&state.pool, &model).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    crate::embed::install(Arc::new(crate::embed::OllamaEmbedder::new(
        crate::runner::OLLAMA_BASE_URL.to_string(),
        model.clone(),
    )));
    crate::embed::nudge();
    Json(serde_json::json!({ "model": model })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        crate::storage::MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    #[test]
    fn parse_choice_trims_and_refuses_unusable_names() {
        assert_eq!(
            parse_choice("  mxbai-embed-large "),
            Some("mxbai-embed-large".to_string())
        );
        assert_eq!(parse_choice(""), None);
        assert_eq!(parse_choice("   "), None);
        assert_eq!(parse_choice("two words"), None);
        assert_eq!(parse_choice("bad\nname"), None);
        assert_eq!(parse_choice(&"x".repeat(MAX_NAME + 1)), None);
    }

    #[test]
    fn is_pulled_matches_an_untagged_name_to_its_latest_tag() {
        let listed = vec!["nomic-embed-text:latest".to_string(), "m:7b".to_string()];
        assert!(is_pulled("nomic-embed-text", &listed));
        assert!(is_pulled("nomic-embed-text:latest", &listed));
        assert!(is_pulled("m:7b", &listed));
        assert!(!is_pulled("m", &listed));
        assert!(!is_pulled("other", &listed));
    }

    #[tokio::test]
    async fn the_stored_choice_wins_over_the_file_and_survives_a_rewrite() {
        let pool = test_pool().await;
        assert_eq!(effective(&pool, "from-file").await, "from-file");
        write(&pool, "first").await.unwrap();
        write(&pool, "second").await.unwrap();
        assert_eq!(effective(&pool, "from-file").await, "second");
    }
}
