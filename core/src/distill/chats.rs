//! Conversations as a distiller source: scope ids of a chat, the window of exchanges one queue row
//! reads, and the owner's explicit ingestion mark. Spec 2026-10-22 4.1, B8, B14.

#[cfg(test)]
pub(crate) mod fixtures {
    use sqlx::SqlitePool;

    pub(crate) async fn chat_with(pool: &SqlitePool) -> String {
        crate::chats::create(pool, crate::chats::Brain::Local, None)
            .await
            .unwrap()
    }

    /// The chat's numeric key: its oldest run (spec 4.1, `chat_key`).
    pub(crate) async fn chat_key(pool: &SqlitePool, chat: &str) -> i64 {
        sqlx::query_scalar("SELECT MIN(id) FROM runs WHERE chat_id = ?")
            .bind(chat)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// One completed assistant turn of `chat`; returns the run id.
    pub(crate) async fn turn(
        pool: &SqlitePool,
        chat: &str,
        project: Option<&str>,
        asked: &str,
        answered: &str,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, chat_id, stdout, project_id, created_at)
             VALUES (?, 'completed', 'assistant', 's', ?, ?, ?, '2026-10-01T10:00:00+00:00')",
        )
        .bind(asked)
        .bind(chat)
        .bind(answered)
        .bind(project)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// Moves the P8 history cut to `run_id`.
    pub(crate) async fn set_floor(pool: &SqlitePool, run_id: i64) {
        sqlx::query(
            "INSERT INTO schema_meta (key, value) VALUES (?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(super::super::sources::CHAT_FLOOR_KEY)
        .bind(run_id.to_string())
        .execute(pool)
        .await
        .unwrap();
    }

    /// An `observed` observation created `days` days ago; returns its id.
    pub(crate) async fn stale_observation(pool: &SqlitePool, days: i64) -> i64 {
        let at = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('episodic', 'observation', 'alpha:chat:1', 'distiller', 'memory',
                     'stale', 'stale', 'observed', ?)",
        )
        .bind(at)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }
}
