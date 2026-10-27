//! Conversations as a distiller source: scope ids of a chat, the window of exchanges one queue row
//! reads, and the owner's explicit ingestion mark. Spec 2026-10-22 4.1, B8, B14.

use sqlx::SqlitePool;

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

/// One exchange of a chat: what the owner wrote and the answer, each clipped to [`SIDE_CHARS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub run_id: i64,
    pub asked: String,
    pub answered: String,
}

/// The oldest [`BATCH_EXCHANGES`] exchanges of `chat_id` up to run `up_to`, after the latest of: the
/// last turn that read untrusted text, the owner's `/clear`, the previous batch of this chat and
/// (P8) the history cut, unless the owner seeded this chat. It is `history_window`'s floor
/// (`send.rs`) plus relays out: a relayed turn is not something the owner wrote.
pub(super) async fn window(pool: &SqlitePool, chat_id: &str, up_to: i64) -> sqlx::Result<Vec<Turn>> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id, prompt, stdout FROM runs
          WHERE chat_id = ?1 AND mode = 'assistant' AND status = 'completed'
            AND from_relay_id IS NULL AND stdout IS NOT NULL AND stdout <> '' AND id <= ?2
            AND id > max(
                  (SELECT COALESCE(MAX(id), 0) FROM runs WHERE chat_id = ?1 AND read_untrusted = 1),
                  COALESCE((SELECT cleared_after_run_id FROM chats WHERE chat_id = ?1), 0),
                  (SELECT COALESCE(MAX(run_id), 0) FROM distill_queue
                    WHERE cause = 'chat_exchanges' AND chat_id = ?1 AND run_id < ?2),
                  -- P8: the history cut, unless the owner seeded this chat (then its window starts at the floor above)
                  CASE WHEN EXISTS (SELECT 1 FROM distill_queue WHERE cause = 'chat_exchanges'
                                      AND chat_id = ?1 AND error = 'ingest') THEN 0 ELSE ?4 END)
          ORDER BY id LIMIT ?3",
    )
    .bind(chat_id)
    .bind(up_to)
    .bind(BATCH_EXCHANGES)
    .bind(super::sources::chat_floor(pool).await)
    .fetch_all(pool)
    .await?;
    let clip = |t: String| t.chars().take(SIDE_CHARS).collect::<String>();
    Ok(rows
        .into_iter()
        .map(|(run_id, asked, answered)| Turn {
            run_id,
            asked: clip(asked),
            answered: clip(answered),
        })
        .collect())
}

/// Marks `chat_id` as owner-ingested (P8): one `chat_exchanges` row, `done`, `error = 'ingest'`,
/// written once. `true` when written, `false` when the chat was already seeded (or has no runs).
/// Never calls the model. The row's `run_id` is the chat key negated: unique per chat under the
/// queue's once-index, and no watermark (a watermark is the largest `run_id` and this is below
/// zero); the daily caps skip `error = 'ingest'`.
pub(super) async fn seed(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<bool> {
    let key: Option<i64> = sqlx::query_scalar("SELECT MIN(id) FROM runs WHERE chat_id = ?")
        .bind(chat_id)
        .fetch_one(pool)
        .await?;
    let Some(key) = key else {
        return Ok(false);
    };
    let now = chrono::Utc::now().to_rfc3339();
    let written = sqlx::query(
        "INSERT INTO distill_queue
           (cause, project_id, run_id, chat_id, status, attempts, error, created_at, finished_at)
         SELECT 'chat_exchanges', '', ?2, ?1, 'done', 0, 'ingest', ?3, ?3
          WHERE NOT EXISTS (SELECT 1 FROM distill_queue
                             WHERE cause = 'chat_exchanges' AND chat_id = ?1 AND error = 'ingest')",
    )
    .bind(chat_id)
    .bind(-key)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(written.rows_affected() > 0)
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

    use super::fixtures::*;

    #[tokio::test]
    async fn the_window_is_the_forty_oldest_exchanges_clipped_per_side() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        let mut ids = Vec::new();
        for i in 0..45 {
            ids.push(turn(&pool, &chat, None, &format!("q{i}{}", "x".repeat(2_000)), "a").await);
        }
        let got = window(&pool, &chat, ids[39]).await.unwrap();
        assert_eq!(
            got.iter().map(|t| t.run_id).collect::<Vec<_>>(),
            ids[..40].to_vec()
        );
        assert!(got.iter().all(|t| t.asked.chars().count() <= SIDE_CHARS));
    }

    #[tokio::test]
    async fn the_window_starts_after_a_tainted_turn_a_clear_and_the_previous_batch() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        let mut ids = Vec::new();
        for _ in 0..12 {
            ids.push(turn(&pool, &chat, None, "q", "a").await);
        }
        async fn first(pool: &SqlitePool, chat: &str, last: i64) -> i64 {
            window(pool, chat, last).await.unwrap().first().unwrap().run_id
        }
        sqlx::query("UPDATE runs SET read_untrusted = 1 WHERE id = ?")
            .bind(ids[2])
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(first(&pool, &chat, ids[11]).await, ids[3], "the last tainted turn is the floor");
        sqlx::query("UPDATE chats SET cleared_after_run_id = ? WHERE chat_id = ?")
            .bind(ids[5])
            .bind(&chat)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(first(&pool, &chat, ids[11]).await, ids[6], "a clear is a floor too");
        sqlx::query(
            "INSERT INTO distill_queue (cause, project_id, run_id, chat_id, status, attempts, created_at)
             VALUES ('chat_exchanges', '', ?, ?, 'done', 0, '2026-10-02T00:00:00Z')",
        )
        .bind(ids[8])
        .bind(&chat)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(first(&pool, &chat, ids[11]).await, ids[9], "the previous batch is the watermark");
    }

    #[tokio::test]
    async fn relayed_failed_and_empty_turns_are_not_exchanges() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        let good = turn(&pool, &chat, None, "q", "a").await;
        let relayed = turn(&pool, &chat, None, "q", "a").await;
        sqlx::query("UPDATE runs SET from_relay_id = 1 WHERE id = ?")
            .bind(relayed)
            .execute(&pool)
            .await
            .unwrap();
        let failed = turn(&pool, &chat, None, "q", "a").await;
        sqlx::query("UPDATE runs SET status = 'failed' WHERE id = ?")
            .bind(failed)
            .execute(&pool)
            .await
            .unwrap();
        let last = turn(&pool, &chat, None, "q", "").await;
        let got = window(&pool, &chat, last).await.unwrap();
        assert_eq!(got.iter().map(|t| t.run_id).collect::<Vec<_>>(), vec![good]);
    }

    #[tokio::test]
    async fn the_history_cut_hides_old_turns_until_the_chat_is_seeded() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        let mut ids = Vec::new();
        for _ in 0..6 {
            ids.push(turn(&pool, &chat, None, "q", "a").await);
        }
        set_floor(&pool, ids[3]).await;
        let first = |w: Vec<Turn>| w.first().unwrap().run_id;
        assert_eq!(
            first(window(&pool, &chat, ids[5]).await.unwrap()),
            ids[4],
            "the cut hides turns up to ids[3]"
        );
        assert!(seed(&pool, &chat).await.unwrap());
        assert!(!seed(&pool, &chat).await.unwrap(), "seeding twice writes nothing");
        assert_eq!(
            first(window(&pool, &chat, ids[5]).await.unwrap()),
            ids[0],
            "a seeded chat ignores the cut"
        );
    }

    #[tokio::test]
    async fn two_chats_can_both_be_seeded() {
        let pool = crate::testdb::fresh_pool().await;
        let one = chat_with(&pool).await;
        let two = chat_with(&pool).await;
        turn(&pool, &one, None, "q", "a").await;
        turn(&pool, &two, None, "q", "a").await;
        assert!(seed(&pool, &one).await.unwrap());
        assert!(seed(&pool, &two).await.unwrap());
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
