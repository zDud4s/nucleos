//! The run history a chat carries in its transfer package, and how it lands on the destination.
//!
//! The package always carries the chat's `assistant` runs, so the destination shows the same
//! conversation and, when the CLI transcript is missing, can replay it to a model that has no session
//! (`assistant::send` takes its rotation path when `assistant_sessions` has no row). Only the columns
//! of [`RunRow`] travel: ids, projects, jobs, relays, accounts, routing and gate columns mean nothing
//! on another machine and stay home.
//!
//! The history is capped ([`HISTORY_MAX_BYTES`], [`HISTORY_MAX_ROWS`]): the newest rows win and
//! `history_truncated` says older ones were left out. Imported rows get `imported_at`, which every
//! reader of spend or work done here filters on (`imported_at IS NULL`). Imported rows count as read
//! (`last_seen_turn_id` advances) and never as live (`completed_at` is set).
//!
//! Known gap: the `/compact` floor is not carried, so a sessionless arrival after a compaction at
//! the origin replays text the compaction had folded, with no handoff summary.
//! Distillation (`distill::chats`) deliberately keeps imported rows: it reads conversation content,
//! and the destination's memory has not seen it.

use sqlx::{SqliteConnection, SqlitePool};

/// The most text (prompt, reply, error and thought) the history may carry.
pub const HISTORY_MAX_BYTES: usize = 4 * 1024 * 1024;

/// The most rows the history may carry.
pub const HISTORY_MAX_ROWS: usize = 500;

/// A row that alone exceeds the byte cap is clipped to this many bytes per text column
/// (the tail kept, on a character boundary) rather than dropped, so the newest exchange always
/// travels. `tools_used` is JSON and cannot be cut mid-way, so an oversized one is dropped.
const CLIP_BYTES: usize = 64 * 1024;

/// One past exchange. See the module comment for what is left out and why.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct RunRow {
    pub prompt: String,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub status: String,
    pub exit_code: Option<i64>,
    pub cost_usd: Option<f64>,
    pub answered_by: Option<String>,
    pub session_id: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub model: Option<String>,
    pub cache_ttl: Option<String>,
    pub context_fill: Option<i64>,
    pub tools_used: Option<String>,
    pub thought: Option<String>,
    pub thought_tokens: Option<i64>,
    pub compacted: i64,
    pub origin: Option<String>,
    /// Kept so the untrusted-read barrier floors a replay on the destination as it did here.
    pub read_untrusted: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_creation_tokens: Option<i64>,
    pub num_turns: Option<i64>,
}

impl RunRow {
    fn text_bytes(&self) -> usize {
        self.prompt.len()
            + self.stdout.as_ref().map_or(0, String::len)
            + self.stderr.as_ref().map_or(0, String::len)
            + self.thought.as_ref().map_or(0, String::len)
            + self.tools_used.as_ref().map_or(0, String::len)
            + self.origin.as_ref().map_or(0, String::len)
            + self.answered_by.as_ref().map_or(0, String::len)
    }
}

/// What [`collect`] found: the rows oldest first, and whether older ones were left out.
#[derive(Debug, Default)]
pub struct History {
    pub rows: Vec<RunRow>,
    pub truncated: bool,
}

/// The chat's history for a package, with the production caps.
pub async fn collect_history(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<History> {
    collect(pool, chat_id, HISTORY_MAX_BYTES, HISTORY_MAX_ROWS).await
}

/// `assistant` runs above the chat's `/clear` floor, newest first until a cap, then flipped. A row
/// that alone exceeds `max_bytes` is clipped instead of dropped, so the newest exchange survives.
async fn collect(
    pool: &SqlitePool,
    chat_id: &str,
    max_bytes: usize,
    max_rows: usize,
) -> sqlx::Result<History> {
    // The floor's id means nothing on the destination, so nothing at or below it is exported.
    let sizes: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT id, length(CAST(prompt AS BLOB)) + length(CAST(COALESCE(stdout, '') AS BLOB))
                + length(CAST(COALESCE(stderr, '') AS BLOB))
                + length(CAST(COALESCE(thought, '') AS BLOB))
                + length(CAST(COALESCE(tools_used, '') AS BLOB))
                + length(CAST(COALESCE(origin, '') AS BLOB))
                + length(CAST(COALESCE(answered_by, '') AS BLOB))
           FROM runs
          WHERE chat_id = ? AND mode = 'assistant'
            AND id > COALESCE((SELECT cleared_after_run_id FROM chats WHERE chat_id = ?), 0)
          ORDER BY id DESC
          LIMIT ?",
    )
    .bind(chat_id)
    .bind(chat_id)
    .bind(i64::try_from(max_rows.saturating_add(1)).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await?;

    let mut used = 0usize;
    let mut oldest = None;
    let mut truncated = false;
    for (index, (id, size)) in sizes.iter().enumerate() {
        let size = usize::try_from(*size).unwrap_or(0);
        // An oversized row will be clipped to four text columns of `CLIP_BYTES` (`tools_used` is
        // dropped); the slack covers the small `origin` and `answered_by` columns.
        let counted = if size > max_bytes {
            size.min(4 * CLIP_BYTES + 1024)
        } else {
            size
        };
        if index >= max_rows || (index > 0 && used + counted > max_bytes) {
            truncated = true;
            break;
        }
        used += counted;
        oldest = Some(*id);
    }
    let Some(oldest) = oldest else {
        return Ok(History::default());
    };

    let mut rows: Vec<RunRow> = sqlx::query_as(
        "SELECT prompt, stdout, stderr, status, exit_code, cost_usd, answered_by, session_id,
                created_at, completed_at, model, cache_ttl, context_fill, tools_used, thought,
                thought_tokens, compacted, origin, read_untrusted, input_tokens, output_tokens,
                cache_read_tokens, cache_creation_tokens, num_turns
           FROM runs
          WHERE chat_id = ? AND mode = 'assistant' AND id >= ?
          ORDER BY id",
    )
    .bind(chat_id)
    .bind(oldest)
    .fetch_all(pool)
    .await?;
    for row in &mut rows {
        if row.text_bytes() > max_bytes {
            clip(row);
        }
    }
    Ok(History { rows, truncated })
}

/// The last `CLIP_BYTES` bytes of `text`, starting on a character boundary.
fn tail(text: &str) -> String {
    let mut start = text.len().saturating_sub(CLIP_BYTES);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

fn clip(row: &mut RunRow) {
    if row
        .tools_used
        .as_ref()
        .is_some_and(|t| t.len() > CLIP_BYTES)
    {
        row.tools_used = None;
    }
    row.prompt = tail(&row.prompt);
    for text in [&mut row.stdout, &mut row.stderr, &mut row.thought]
        .into_iter()
        .flatten()
    {
        *text = tail(text);
    }
}

/// A status that nothing here is running becomes the one startup reconciliation uses.
fn landed_status(status: &str) -> &str {
    match status {
        "running" | "awaiting_approval" => "interrupted",
        other => other,
    }
}

/// Inserts `rows` (oldest first) as the chat's runs on this machine, inside the import transaction.
/// New local ids grow with the order; project, job, relay and account columns stay NULL. A row the
/// chat already holds (same `created_at` and `prompt`, left by another transfer of the same chat)
/// is skipped. Only the newest [`HISTORY_MAX_ROWS`] rows are taken, whatever the package carries.
///
/// A row that lands `interrupted` gets a `completed_at` (its own, else the import time), so no reader
/// of "`completed_at IS NULL` means live" counts it. Returns the highest local id inserted, if any.
pub async fn insert_history(
    tx: &mut SqliteConnection,
    chat_id: &str,
    rows: &[RunRow],
) -> sqlx::Result<Option<i64>> {
    let imported_at = chrono::Utc::now().to_rfc3339();
    let rows = &rows[rows.len().saturating_sub(HISTORY_MAX_ROWS)..];
    let mut newest = None;
    for row in rows {
        let status = landed_status(&row.status);
        let completed_at = match (&row.completed_at, status) {
            (None, "interrupted") => Some(imported_at.clone()),
            (other, _) => other.clone(),
        };
        let done = sqlx::query(
            "INSERT INTO runs (chat_id, mode, prompt, stdout, stderr, status, exit_code, cost_usd,
                               answered_by, session_id, created_at, completed_at, model, cache_ttl,
                               context_fill, tools_used, thought, thought_tokens, compacted, origin,
                               read_untrusted, input_tokens, output_tokens, cache_read_tokens,
                               cache_creation_tokens, num_turns, imported_at)
             SELECT ?, 'assistant', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                    ?, ?, ?
              WHERE NOT EXISTS (SELECT 1 FROM runs
                                 WHERE chat_id = ? AND mode = 'assistant'
                                   AND created_at = ? AND prompt = ?)",
        )
        .bind(chat_id)
        .bind(&row.prompt)
        .bind(&row.stdout)
        .bind(&row.stderr)
        .bind(status)
        .bind(row.exit_code)
        .bind(row.cost_usd)
        .bind(&row.answered_by)
        .bind(&row.session_id)
        .bind(&row.created_at)
        .bind(&completed_at)
        .bind(&row.model)
        .bind(&row.cache_ttl)
        .bind(row.context_fill)
        .bind(&row.tools_used)
        .bind(&row.thought)
        .bind(row.thought_tokens)
        .bind(row.compacted)
        .bind(&row.origin)
        .bind(row.read_untrusted)
        .bind(row.input_tokens)
        .bind(row.output_tokens)
        .bind(row.cache_read_tokens)
        .bind(row.cache_creation_tokens)
        .bind(row.num_turns)
        .bind(&imported_at)
        .bind(chat_id)
        .bind(&row.created_at)
        .bind(&row.prompt)
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() > 0 {
            newest = Some(done.last_insert_rowid());
        }
    }
    Ok(newest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chats::transfer;
    use crate::chats::transfer_package::{Failure, Imported, Package, build, import};
    use std::path::Path;

    async fn a_chat(pool: &SqlitePool, chat_id: &str) {
        sqlx::query("INSERT INTO chats (chat_id, title, brain, created_at) VALUES (?, 'work', 'cloud', '2026-01-01T00:00:00Z')")
            .bind(chat_id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// An `assistant` run the way a turn here leaves it, with a project and a job it must not carry.
    async fn a_run(
        pool: &SqlitePool,
        chat_id: &str,
        n: u32,
        status: &str,
        stdout: &str,
        untrusted: bool,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO runs (chat_id, mode, prompt, stdout, status, cost_usd, input_tokens,
                               created_at, completed_at, read_untrusted, project_id, from_relay_id)
             VALUES (?, 'assistant', ?, ?, ?, 0.5, 10, ?, ?, ?, 'proj', 7)",
        )
        .bind(chat_id)
        .bind(format!("question {n}"))
        .bind(stdout)
        .bind(status)
        .bind(format!("2026-02-01T00:00:{n:02}.000Z"))
        .bind(format!("2026-02-01T00:00:{n:02}.500Z"))
        .bind(i64::from(untrusted))
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    fn bare(chat_id: &str, transfer_id: &str, history: Vec<RunRow>) -> Package {
        serde_json::from_value(serde_json::json!({
            "transfer_id": transfer_id,
            "chat_id": chat_id,
            "chat": {
                "title": "work", "brain": "cloud", "created_at": "2026-01-01T00:00:00Z",
                "model": null, "effort": null, "plan_only": false
            },
            "session": null, "code": null, "queue": [], "parked": [], "interrupted": false,
            "history": history,
        }))
        .unwrap()
    }

    async fn moved_package(origin: &SqlitePool, chat: &str, id: &str) -> Package {
        transfer::freeze_at(origin, chat, id, transfer::unix_now())
            .await
            .unwrap();
        build(origin, chat, id, Path::new("unused"), vec![])
            .await
            .unwrap()
    }

    async fn texts(pool: &SqlitePool, chat: &str) -> Vec<(i64, String, String)> {
        sqlx::query_as(
            "SELECT id, prompt, status FROM runs WHERE chat_id = ? AND mode = 'assistant' ORDER BY id",
        )
        .bind(chat)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn history_round_trips_with_local_ids_and_order_and_leaves_local_columns_null() {
        let origin = crate::testdb::fresh_pool().await;
        let destination = crate::testdb::fresh_pool().await;
        a_chat(&origin, "c1").await;
        for n in 1..=3 {
            a_run(&origin, "c1", n, "completed", &format!("answer {n}"), false).await;
        }
        // Rows on the destination first, so the imported ids cannot equal the origin's.
        a_chat(&destination, "other").await;
        for n in 1..=5 {
            a_run(&destination, "other", n, "completed", "x", false).await;
        }

        let package = moved_package(&origin, "c1", "t-1").await;
        assert_eq!(package.history.len(), 3);
        assert!(!package.history_truncated);
        let done = import(&destination, &package, Path::new("unused"))
            .await
            .unwrap();
        assert!(matches!(done, Imported::Done { .. }));

        let imported = texts(&destination, "c1").await;
        let prompts: Vec<&str> = imported.iter().map(|r| r.1.as_str()).collect();
        assert_eq!(prompts, ["question 1", "question 2", "question 3"]);
        assert!(imported.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(imported[0].0 > 5, "ids are local: {imported:?}");
        type RunExtras = (
            Option<String>,
            Option<i64>,
            Option<i64>,
            String,
            Option<String>,
            Option<f64>,
        );
        let (project, job, relay, mode, stamped, cost): RunExtras = sqlx::query_as(
            "SELECT project_id, job_id, from_relay_id, mode, imported_at, cost_usd
               FROM runs WHERE chat_id = 'c1' ORDER BY id LIMIT 1",
        )
        .fetch_one(&destination)
        .await
        .unwrap();
        assert_eq!((project, job, relay), (None, None, None));
        assert_eq!(mode, "assistant");
        assert!(stamped.is_some());
        assert_eq!(cost, Some(0.5));
    }

    #[tokio::test]
    async fn a_package_without_the_history_keys_still_imports() {
        let mut value = serde_json::to_value(bare("c1", "t-1", vec![])).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("history");
        object.remove("history_truncated");
        let old: Package = serde_json::from_value(value).unwrap();
        assert!(old.history.is_empty() && !old.history_truncated);

        let destination = crate::testdb::fresh_pool().await;
        let done = import(&destination, &old, Path::new("unused"))
            .await
            .unwrap();
        assert!(matches!(done, Imported::Done { .. }));
        assert!(texts(&destination, "c1").await.is_empty());
    }

    #[tokio::test]
    async fn the_cap_keeps_the_newest_rows_and_says_it_cut() {
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c1").await;
        for n in 1..=4 {
            a_run(&pool, "c1", n, "completed", &"y".repeat(100), false).await;
        }
        let by_rows = collect(&pool, "c1", usize::MAX, 2).await.unwrap();
        assert!(by_rows.truncated);
        let prompts: Vec<&str> = by_rows.rows.iter().map(|r| r.prompt.as_str()).collect();
        assert_eq!(prompts, ["question 3", "question 4"]);

        // Each row is about 110 bytes: two fit in 250, a third does not.
        let by_bytes = collect(&pool, "c1", 250, 500).await.unwrap();
        assert!(by_bytes.truncated);
        assert_eq!(by_bytes.rows.len(), 2);
        assert_eq!(by_bytes.rows[1].prompt, "question 4");

        let all = collect_history(&pool, "c1").await.unwrap();
        assert!(!all.truncated);
        assert_eq!(all.rows.len(), 4);
    }

    #[tokio::test]
    async fn one_oversized_row_is_clipped_not_dropped() {
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c1").await;
        let huge = format!("{}END", "z".repeat(300_000));
        a_run(&pool, "c1", 1, "completed", &huge, false).await;
        let history = collect(&pool, "c1", 100_000, 500).await.unwrap();
        assert_eq!(history.rows.len(), 1);
        assert!(!history.truncated);
        let stdout = history.rows[0].stdout.as_deref().unwrap();
        assert_eq!(stdout.len(), CLIP_BYTES);
        assert!(stdout.ends_with("END"), "the tail is kept");
    }

    #[tokio::test]
    async fn rows_below_the_clear_floor_do_not_travel() {
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c1").await;
        let first = a_run(&pool, "c1", 1, "completed", "a", false).await;
        a_run(&pool, "c1", 2, "completed", "b", false).await;
        sqlx::query("UPDATE chats SET cleared_after_run_id = ? WHERE chat_id = 'c1'")
            .bind(first)
            .execute(&pool)
            .await
            .unwrap();
        let history = collect_history(&pool, "c1").await.unwrap();
        assert_eq!(history.rows.len(), 1);
        assert_eq!(history.rows[0].prompt, "question 2");
    }

    #[tokio::test]
    async fn running_rows_land_interrupted_and_the_untrusted_barrier_survives() {
        let origin = crate::testdb::fresh_pool().await;
        let destination = crate::testdb::fresh_pool().await;
        a_chat(&origin, "c1").await;
        a_run(&origin, "c1", 1, "completed", "before", false).await;
        a_run(&origin, "c1", 2, "completed", "read mail", true).await;
        a_run(&origin, "c1", 3, "completed", "after", false).await;
        a_run(&origin, "c1", 4, "running", "", false).await;
        let package = moved_package(&origin, "c1", "t-1").await;
        import(&destination, &package, Path::new("unused"))
            .await
            .unwrap();

        let landed = texts(&destination, "c1").await;
        assert_eq!(landed[3].2, "interrupted");
        assert_eq!(landed[2].2, "completed");
        let floor: i64 = sqlx::query_scalar(
            "SELECT MAX(id) FROM runs WHERE chat_id = 'c1' AND read_untrusted = 1",
        )
        .fetch_one(&destination)
        .await
        .unwrap();
        assert_eq!(floor, landed[1].0, "the barrier sits on the same exchange");
    }

    #[tokio::test]
    async fn the_same_transfer_twice_and_a_second_transfer_of_the_chat_add_no_duplicates() {
        let origin = crate::testdb::fresh_pool().await;
        let destination = crate::testdb::fresh_pool().await;
        a_chat(&origin, "c1").await;
        a_run(&origin, "c1", 1, "completed", "a", false).await;
        a_run(&origin, "c1", 2, "completed", "b", false).await;
        let first = moved_package(&origin, "c1", "t-1").await;
        import(&destination, &first, Path::new("unused"))
            .await
            .unwrap();
        assert_eq!(
            import(&destination, &first, Path::new("unused"))
                .await
                .unwrap(),
            Imported::Already
        );
        assert_eq!(texts(&destination, "c1").await.len(), 2);

        // The chat goes back and forth: the destination ships it home, then the origin ships it
        // again with the same two rows plus a new one. Only the new one is added there.
        sqlx::query("UPDATE chats SET moved_to = 'elsewhere' WHERE chat_id = 'c1'")
            .execute(&destination)
            .await
            .unwrap();
        let mut second = first.clone();
        second.transfer_id = "t-2".into();
        let mut extra = second.history[1].clone();
        extra.prompt = "question 3".into();
        extra.created_at = "2026-02-01T00:00:03.000Z".into();
        second.history.push(extra);
        import(&destination, &second, Path::new("unused"))
            .await
            .unwrap();
        let landed = texts(&destination, "c1").await;
        let prompts: Vec<&str> = landed.iter().map(|r| r.1.as_str()).collect();
        assert_eq!(prompts, ["question 1", "question 2", "question 3"]);
    }

    #[tokio::test]
    async fn a_failure_while_inserting_leaves_no_rows_and_no_chat() {
        let destination = crate::testdb::fresh_pool().await;
        sqlx::query(
            "CREATE TRIGGER refuse_boom BEFORE INSERT ON runs WHEN NEW.prompt = 'boom'
             BEGIN SELECT RAISE(ABORT, 'boom'); END",
        )
        .execute(&destination)
        .await
        .unwrap();
        let row = |prompt: &str, second: u32| RunRow {
            prompt: prompt.into(),
            stdout: Some("x".into()),
            stderr: None,
            status: "completed".into(),
            exit_code: Some(0),
            cost_usd: None,
            answered_by: None,
            session_id: None,
            created_at: format!("2026-02-01T00:00:{second:02}.000Z"),
            completed_at: None,
            model: None,
            cache_ttl: None,
            context_fill: None,
            tools_used: None,
            thought: None,
            thought_tokens: None,
            compacted: 0,
            origin: None,
            read_untrusted: 0,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            num_turns: None,
        };
        let package = bare("c1", "t-1", vec![row("fine", 1), row("boom", 2)]);
        let failed = import(&destination, &package, Path::new("unused")).await;
        assert!(matches!(failed, Err(Failure::Refused(_))), "{failed:?}");
        assert!(texts(&destination, "c1").await.is_empty());
        let chats: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chats WHERE chat_id = 'c1'")
            .fetch_one(&destination)
            .await
            .unwrap();
        assert_eq!(chats, 0);
    }

    #[tokio::test]
    async fn an_imported_formerly_running_row_is_not_live_and_nothing_lands_unread() {
        let origin = crate::testdb::fresh_pool().await;
        let destination = crate::testdb::fresh_pool().await;
        a_chat(&origin, "c1").await;
        a_run(&origin, "c1", 1, "completed", "a", false).await;
        let live = a_run(&origin, "c1", 2, "running", "", false).await;
        sqlx::query("UPDATE runs SET completed_at = NULL WHERE id = ?")
            .bind(live)
            .execute(&origin)
            .await
            .unwrap();
        let package = moved_package(&origin, "c1", "t-1").await;
        import(&destination, &package, Path::new("unused"))
            .await
            .unwrap();

        // The analytics `in_flight` count is `completed_at IS NULL`.
        let live_here: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE completed_at IS NULL")
                .fetch_one(&destination)
                .await
                .unwrap();
        assert_eq!(live_here, 0);

        let waiting = |pool: SqlitePool| async move {
            crate::chats::list(&pool)
                .await
                .unwrap()
                .into_iter()
                .find(|c| c.chat_id == "c1")
                .unwrap()
                .waiting
        };
        assert_eq!(waiting(destination.clone()).await, 0);
        a_run(&destination, "c1", 9, "completed", "new here", false).await;
        assert_eq!(waiting(destination.clone()).await, 1);
    }

    #[tokio::test]
    async fn import_takes_only_the_newest_rows_up_to_the_cap() {
        let destination = crate::testdb::fresh_pool().await;
        let row = |n: usize| RunRow {
            prompt: format!("p{n}"),
            stdout: None,
            stderr: None,
            status: "completed".into(),
            exit_code: None,
            cost_usd: None,
            answered_by: None,
            session_id: None,
            created_at: format!("2026-02-01T00:00:00.{n:06}Z"),
            completed_at: None,
            model: None,
            cache_ttl: None,
            context_fill: None,
            tools_used: None,
            thought: None,
            thought_tokens: None,
            compacted: 0,
            origin: None,
            read_untrusted: 0,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            num_turns: None,
        };
        let rows: Vec<RunRow> = (0..HISTORY_MAX_ROWS + 3).map(row).collect();
        let package = bare("c1", "t-1", rows);
        import(&destination, &package, Path::new("unused"))
            .await
            .unwrap();
        let landed = texts(&destination, "c1").await;
        assert_eq!(landed.len(), HISTORY_MAX_ROWS);
        assert_eq!(landed[0].1, "p3");
    }

    #[tokio::test]
    async fn tools_used_counts_toward_the_cap_and_multibyte_clips_by_bytes() {
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c1").await;
        let first = a_run(&pool, "c1", 1, "completed", "a", false).await;
        a_run(&pool, "c1", 2, "completed", "b", false).await;
        sqlx::query("UPDATE runs SET tools_used = ? WHERE id = ?")
            .bind("t".repeat(400))
            .bind(first)
            .execute(&pool)
            .await
            .unwrap();
        // Each row is about 20 bytes of text; the 400-byte tools_used alone breaks a 200 budget.
        let history = collect(&pool, "c1", 200, 500).await.unwrap();
        assert!(history.truncated);
        assert_eq!(history.rows.len(), 1);

        let emoji = "\u{1F600}".repeat(100_000);
        let mut row = collect_history(&pool, "c1").await.unwrap().rows.remove(0);
        row.stdout = Some(emoji);
        clip(&mut row);
        let stdout = row.stdout.unwrap();
        assert!(stdout.len() <= CLIP_BYTES);
        assert!(stdout.chars().all(|c| c == '\u{1F600}'));
    }

    #[tokio::test]
    async fn a_missing_transcript_builds_a_sessionless_package_that_keeps_its_history() {
        let origin = crate::testdb::fresh_pool().await;
        a_chat(&origin, "c1").await;
        a_run(&origin, "c1", 1, "completed", "a", false).await;
        sqlx::query("INSERT INTO assistant_sessions (chat_id, session_id, updated_at) VALUES ('c1', 'gone', 'now')")
            .execute(&origin)
            .await
            .unwrap();
        let empty_store =
            crate::execution::git_exec::testkit::space_free_tempdir("nucleos-no-transcript-");
        transfer::freeze_at(&origin, "c1", "t-1", transfer::unix_now())
            .await
            .unwrap();
        let package = build(&origin, "c1", "t-1", empty_store.path(), vec![])
            .await
            .unwrap();
        assert!(package.session.is_none());
        assert_eq!(package.history.len(), 1);
    }
}
