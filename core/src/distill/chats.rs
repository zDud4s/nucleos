//! Conversations as a distiller source: scope ids of a chat, the window of exchanges one queue row
//! reads, and the owner's explicit ingestion mark. Spec 2026-10-22 4.1, B8, B14.

/// Fewest exchanges a chat needs before a batch is queued.
pub const MIN_EXCHANGES: usize = 4;
/// Minutes without a new turn before a chat counts as idle.
pub const IDLE_MINUTES: i64 = 90;
/// Most exchanges one queue row reads.
pub const BATCH_EXCHANGES: i64 = 40;
/// Most characters kept of each side of an exchange.
pub const SIDE_CHARS: usize = 1_200;
/// Daily ceilings (spec B14); a guess to be tuned.
pub const MAX_CHAT_ROWS_PER_DAY: i64 = 6;
pub const MAX_CHAT_ROWS_PER_CHAT_DAY: i64 = 2;
pub const MAX_CHAT_CANDIDATES_PER_ROW: usize = 3;
pub const MAX_OBS_PER_ROW: usize = 8;
pub const MAX_OBS_PER_CHAT_DAY: i64 = 10;

pub fn chat_scope_id(project: &str, chat_key: i64) -> String {
    format!("{}:chat:{chat_key}", if project.is_empty() { "-" } else { project })
}

/// `<project|->:chat:<digits>`; `rsplit_once` because a project id may itself contain ':'.
pub fn parse_chat_scope(scope_id: &str) -> Option<(&str, i64)> {
    let (project, key) = scope_id.rsplit_once(":chat:")?;
    if project.is_empty() || key.is_empty() || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((project, key.parse().ok()?))
}

/// The instant the local day of `now` began, in UTC.
pub fn local_day_start<Tz: chrono::TimeZone>(
    now: chrono::DateTime<Tz>,
) -> chrono::DateTime<chrono::Utc> {
    let midnight = now.date_naive().and_hms_opt(0, 0, 0).expect("midnight exists");
    now.timezone()
        .from_local_datetime(&midnight)
        .earliest()
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_else(|| now.with_timezone(&chrono::Utc) - chrono::Duration::hours(24))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_scope_ids_are_read_back_by_the_last_marker_with_digits_only() {
        assert_eq!(chat_scope_id("alpha", 5), "alpha:chat:5");
        assert_eq!(chat_scope_id("", 5), "-:chat:5");
        assert_eq!(parse_chat_scope("-:chat:5"), Some(("-", 5)));
        // a project id may hold ':'; the LAST marker wins, so `a:chat:5` is the project here
        assert_eq!(parse_chat_scope("a:chat:5:chat:9"), Some(("a:chat:5", 9)));
        for bad in [
            "week:2026-W43",
            "alpha:chat:",
            "alpha:chat:5x",
            "alpha",
            ":chat:5",
            "alpha:chat:+1",
        ] {
            assert_eq!(parse_chat_scope(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_local_day_starts_at_local_midnight() {
        use chrono::{FixedOffset, TimeZone, Utc};
        let now = FixedOffset::east_opt(3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, 22, 0, 30, 0)
            .unwrap();
        assert_eq!(
            local_day_start(now),
            Utc.with_ymd_and_hms(2026, 10, 21, 23, 0, 0).unwrap()
        );
    }
}

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
