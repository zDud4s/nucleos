//! The distiller's per-source switches (chat and calendar), on by default, and the P8 history cut
//! of the automatic chat path. Spec 2026-10-22 B8, B14.

use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use sqlx::SqlitePool;

pub const CHAT_KEY: &str = "distiller.chat_sources";
pub const CALENDAR_KEY: &str = "distiller.calendar_sources";
pub const CHAT_FLOOR_KEY: &str = "distiller.chat_floor_run_id";

/// Absent key = on (owner decision P6); only a stored `0` turns a source off. A read that fails
/// reads as off: the owner's text must not leave the machine because of a fault.
pub async fn enabled(pool: &SqlitePool, key: &str) -> bool {
    match sqlx::query_scalar::<_, String>("SELECT value FROM schema_meta WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
    {
        Ok(None) => true,
        Ok(Some(stored)) => stored.trim() != "0",
        Err(_) => false,
    }
}

const UPSERT: &str = "INSERT INTO schema_meta (key, value) VALUES (?, ?) \
                      ON CONFLICT(key) DO UPDATE SET value = excluded.value";

pub async fn set(pool: &SqlitePool, key: &str, on: bool) -> sqlx::Result<()> {
    sqlx::query(UPSERT)
        .bind(key)
        .bind(if on { "1" } else { "0" })
        .execute(pool)
        .await?;
    Ok(())
}

/// P8: the automatic path only sees chat turns with `runs.id` above this cut. Absent or unreadable
/// reads as `i64::MAX` (fail closed: nothing old reaches the model by accident).
pub async fn chat_floor(pool: &SqlitePool) -> i64 {
    match sqlx::query_scalar::<_, String>("SELECT value FROM schema_meta WHERE key = ?")
        .bind(CHAT_FLOOR_KEY)
        .fetch_optional(pool)
        .await
    {
        Ok(Some(v)) => v.trim().parse().unwrap_or(i64::MAX),
        _ => i64::MAX,
    }
}

async fn both(pool: &SqlitePool) -> serde_json::Value {
    serde_json::json!({
        "chat": enabled(pool, CHAT_KEY).await,
        "calendar": enabled(pool, CALENDAR_KEY).await,
    })
}

/// `GET /config/distiller-sources`.
pub async fn get_sources(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(both(&state.pool).await)
}

/// The body of `POST /config/distiller-sources`: only the present switches are written.
#[derive(serde::Deserialize)]
pub struct SourcesBody {
    pub chat: Option<bool>,
    pub calendar: Option<bool>,
}

/// `POST /config/distiller-sources`. Switching chat from off to on moves the history cut to the
/// newest run in the same transaction (P8): whatever was said while it was off is history, and
/// reaches the model only through the owner's explicit ingestion.
pub async fn post_sources(
    State(state): State<AppState>,
    Json(body): Json<SourcesBody>,
) -> axum::response::Response {
    match write_sources(&state.pool, &body).await {
        Ok(()) => Json(both(&state.pool).await).into_response(),
        Err(error) => {
            tracing::warn!(%error, "setting the distiller sources failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn write_sources(pool: &SqlitePool, body: &SourcesBody) -> sqlx::Result<()> {
    let was_on = enabled(pool, CHAT_KEY).await;
    let mut tx = pool.begin().await?;
    if let Some(on) = body.chat {
        sqlx::query(UPSERT)
            .bind(CHAT_KEY)
            .bind(if on { "1" } else { "0" })
            .execute(&mut *tx)
            .await?;
        // Only an off -> on transition moves the cut; absent already means on.
        if on && !was_on {
            sqlx::query(
                "INSERT INTO schema_meta (key, value) \
                 SELECT ?, CAST(COALESCE(MAX(id), 0) AS TEXT) FROM runs WHERE true \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )
            .bind(CHAT_FLOOR_KEY)
            .execute(&mut *tx)
            .await?;
        }
    }
    if let Some(on) = body.calendar {
        sqlx::query(UPSERT)
            .bind(CALENDAR_KEY)
            .bind(if on { "1" } else { "0" })
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_absent_key_is_on_and_only_a_stored_zero_turns_a_source_off() {
        let pool = crate::testdb::fresh_pool().await;
        assert!(enabled(&pool, CHAT_KEY).await, "P6: absent means on");
        for (stored, on) in [
            ("0", false),
            (" 0 ", false),
            ("1", true),
            ("yes", true),
            ("", true),
        ] {
            sqlx::query(UPSERT)
                .bind(CHAT_KEY)
                .bind(stored)
                .execute(&pool)
                .await
                .unwrap();
            assert_eq!(enabled(&pool, CHAT_KEY).await, on, "stored {stored:?}");
        }
        assert!(
            enabled(&pool, CALENDAR_KEY).await,
            "each source on its own key"
        );
    }

    #[tokio::test]
    async fn a_failing_read_fails_closed() {
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query("DROP TABLE schema_meta")
            .execute(&pool)
            .await
            .unwrap();
        assert!(!enabled(&pool, CHAT_KEY).await);
    }
}
