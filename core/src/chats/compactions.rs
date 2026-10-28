//! The `/compact` handoffs of a conversation (migration 0253): the `chat_compactions` SQL and
//! nothing else.
//!
//! A compaction is an ordinary turn whose answer is a handoff document. Its row is `pending` while
//! that turn runs and is settled once when it ends: `done` with the answer as `summary`, or
//! `failed` when there is no usable answer. `/clear` (`chats::clear_context`) turns every `done`
//! row `cleared`. Only a `done` row above the chat's `cleared_after_run_id` is ever read back
//! (`active`).
//!
//! **`settle` is idempotent and guarded on `status = 'pending'`.** It is called both when the turn
//! ends and right after the route inserts the row, because a fast engine can finish the turn before
//! the insert lands. The first caller that finds the run finished wins; every later one gets
//! `Already`. What a settled compaction does to the session, the replay and the notices is the
//! assistant's business, not this module's.

use sqlx::SqlitePool;

/// The `chat_notices.kind` a compaction posts.
pub const NOTICE_KIND: &str = "compaction";

/// Where a compaction is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    Done,
    Failed,
    Cleared,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Cleared => "cleared",
        }
    }

    pub fn from_wire(s: &str) -> Option<Status> {
        match s {
            "pending" => Some(Status::Pending),
            "done" => Some(Status::Done),
            "failed" => Some(Status::Failed),
            "cleared" => Some(Status::Cleared),
            _ => None,
        }
    }
}

/// Files a compaction for the turn `run_id`, before (or while) it runs. Returns the row id.
pub async fn insert_pending(
    pool: &SqlitePool,
    chat_id: &str,
    run_id: i64,
    prompt_version: &str,
    engine: &str,
    model: Option<&str>,
    focus: Option<&str>,
) -> sqlx::Result<i64> {
    Ok(sqlx::query(
        "INSERT INTO chat_compactions (chat_id, run_id, status, prompt_version, engine, model, focus)
         VALUES (?, ?, 'pending', ?, ?, ?, ?)",
    )
    .bind(chat_id)
    .bind(run_id)
    .bind(prompt_version)
    .bind(engine)
    .bind(model)
    .bind(focus)
    .execute(pool)
    .await?
    .last_insert_rowid())
}

/// Whether there is anything to compact: an answered assistant turn after the last `/clear`.
pub async fn has_exchanges_since_clear(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM runs
              WHERE chat_id = ? AND mode = 'assistant' AND status = 'completed'
                AND stdout IS NOT NULL AND trim(stdout) <> ''
                AND id > COALESCE((SELECT cleared_after_run_id FROM chats WHERE chat_id = ?), 0))",
    )
    .bind(chat_id)
    .bind(chat_id)
    .fetch_one(pool)
    .await
}

/// What `settle` found or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// The run has no compaction row.
    NotACompaction,
    /// The compaction's turn has not ended yet.
    StillRunning,
    /// This call turned the row `done`.
    Done { chat_id: String, summary_chars: i64 },
    /// This call turned the row `failed`.
    Failed { chat_id: String },
    /// Somebody settled it before (or it was cleared); `status` is what it is now.
    Already { chat_id: String, status: Status },
}

/// Settles the compaction of `run_id` if its turn has ended. Safe to call any number of times.
///
/// A turn that read third-party text (`runs.read_untrusted`, mail or the web through a tool) settles
/// `failed` even with an answer: its handoff would be replayed ahead of every later turn, past the
/// untrusted floor the history window keeps and the session drop such a turn gets.
pub async fn settle(pool: &SqlitePool, run_id: i64) -> sqlx::Result<Settled> {
    // chat_id, compaction status, run status, run stdout, run read_untrusted.
    type Row = (String, String, Option<String>, Option<String>, Option<i64>);
    let row: Option<Row> = sqlx::query_as(
        "SELECT cc.chat_id, cc.status, r.status, r.stdout, r.read_untrusted
           FROM chat_compactions cc
           LEFT JOIN runs r ON r.id = cc.run_id
          WHERE cc.run_id = ?
          ORDER BY cc.id DESC
          LIMIT 1",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    let Some((chat_id, cc_status, run_status, stdout, read_untrusted)) = row else {
        return Ok(Settled::NotACompaction);
    };
    if matches!(run_status.as_deref(), Some("running") | Some("pending")) {
        return Ok(Settled::StillRunning);
    }
    if cc_status != Status::Pending.as_str() {
        return Ok(already(chat_id, &cc_status));
    }
    let summary = stdout.as_deref().map(str::trim).unwrap_or("");
    let answered = run_status.as_deref() == Some("completed")
        && read_untrusted == Some(0)
        && !summary.is_empty();
    let affected = if answered {
        let chars = summary.chars().count() as i64;
        let n = sqlx::query(
            "UPDATE chat_compactions SET status = 'done', summary = ?, summary_chars = ?
              WHERE run_id = ? AND status = 'pending'",
        )
        .bind(summary)
        .bind(chars)
        .bind(run_id)
        .execute(pool)
        .await?
        .rows_affected();
        if n > 0 {
            return Ok(Settled::Done {
                chat_id,
                summary_chars: chars,
            });
        }
        n
    } else {
        let n = sqlx::query(
            "UPDATE chat_compactions SET status = 'failed' WHERE run_id = ? AND status = 'pending'",
        )
        .bind(run_id)
        .execute(pool)
        .await?
        .rows_affected();
        if n > 0 {
            return Ok(Settled::Failed { chat_id });
        }
        n
    };
    debug_assert_eq!(affected, 0);
    // Another caller settled it between the read and the update.
    let status: String = sqlx::query_scalar(
        "SELECT status FROM chat_compactions WHERE run_id = ? ORDER BY id DESC LIMIT 1",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await?;
    Ok(already(chat_id, &status))
}

/// Settles every `pending` compaction of `chat_id` whose turn is no longer running or pending (or
/// whose run row is gone), returning each run id with what [`settle`] made of it.
///
/// For the rows the turn's own settle never reached: a cancel, a `/stop` or a daemon restart can
/// abort the turn's task before its last line runs, and the row would otherwise stay `pending`.
pub async fn settle_stale(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Vec<(i64, Settled)>> {
    let stale: Vec<i64> = sqlx::query_scalar(
        "SELECT cc.run_id
           FROM chat_compactions cc
           LEFT JOIN runs r ON r.id = cc.run_id
          WHERE cc.chat_id = ? AND cc.status = 'pending'
            AND (r.status IS NULL OR r.status NOT IN ('running', 'pending'))
          ORDER BY cc.run_id",
    )
    .bind(chat_id)
    .fetch_all(pool)
    .await?;
    let mut settled = Vec::with_capacity(stale.len());
    for run_id in stale {
        settled.push((run_id, settle(pool, run_id).await?));
    }
    Ok(settled)
}

fn already(chat_id: String, status: &str) -> Settled {
    Settled::Already {
        chat_id,
        status: Status::from_wire(status).unwrap_or(Status::Failed),
    }
}

/// The compaction in force in a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    pub run_id: i64,
    pub summary: String,
    pub created_at: String,
}

/// The newest `done` compaction above the chat's `/clear` floor, if any.
pub async fn active(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<Active>> {
    let row: Option<(i64, Option<String>, String)> = sqlx::query_as(
        "SELECT run_id, summary, created_at FROM chat_compactions
          WHERE chat_id = ? AND status = 'done'
            AND run_id > COALESCE((SELECT cleared_after_run_id FROM chats WHERE chat_id = ?), 0)
          ORDER BY run_id DESC
          LIMIT 1",
    )
    .bind(chat_id)
    .bind(chat_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(run_id, summary, created_at)| Active {
        run_id,
        summary: summary.unwrap_or_default(),
        created_at,
    }))
}

/// `/clear`: every `done` compaction of the chat becomes `cleared`.
pub async fn clear(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE chat_compactions SET status = 'cleared' WHERE chat_id = ? AND status = 'done'",
    )
    .bind(chat_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Whether the run `run_id` was a compaction turn (any status).
pub async fn is_compaction(pool: &SqlitePool, run_id: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM chat_compactions WHERE run_id = ?)")
        .bind(run_id)
        .fetch_one(pool)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chats::{self, Brain};

    async fn chat(pool: &SqlitePool) -> String {
        chats::create(pool, Brain::Cloud, None).await.unwrap()
    }

    /// An assistant run of `chat`; returns its id.
    async fn seed_turn(pool: &SqlitePool, chat: &str, status: &str, stdout: Option<&str>) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, created_at, stdout)
             VALUES ('q', ?, 'assistant', ?, '2026-10-28T10:00:00+00:00', ?)
             RETURNING id",
        )
        .bind(status)
        .bind(chat)
        .bind(stdout)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn set_run(pool: &SqlitePool, run: i64, status: &str, stdout: Option<&str>) {
        sqlx::query("UPDATE runs SET status = ?, stdout = ? WHERE id = ?")
            .bind(status)
            .bind(stdout)
            .bind(run)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn pending(pool: &SqlitePool, chat: &str, run: i64) {
        insert_pending(
            pool,
            chat,
            run,
            "nucleos-compact v1",
            "claude",
            Some("sonnet"),
            None,
        )
        .await
        .unwrap();
    }

    async fn status_of(pool: &SqlitePool, run: i64) -> String {
        sqlx::query_scalar("SELECT status FROM chat_compactions WHERE run_id = ?")
            .bind(run)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn pending_then_completed_run_settles_done_with_summary() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let run = seed_turn(&pool, &c, "running", None).await;
        pending(&pool, &c, run).await;
        assert_eq!(status_of(&pool, run).await, "pending");
        assert!(is_compaction(&pool, run).await.unwrap());
        set_run(&pool, run, "completed", Some("  # Handoff — x\nbody  \n")).await;
        let s = settle(&pool, run).await.unwrap();
        let text = "# Handoff — x\nbody";
        assert_eq!(
            s,
            Settled::Done {
                chat_id: c.clone(),
                summary_chars: text.chars().count() as i64
            }
        );
        let (summary, chars): (String, i64) =
            sqlx::query_as("SELECT summary, summary_chars FROM chat_compactions WHERE run_id = ?")
                .bind(run)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(summary, text);
        assert_eq!(chars, text.chars().count() as i64);
        assert!(!is_compaction(&pool, run + 1000).await.unwrap());
        assert_eq!(
            settle(&pool, run + 1000).await.unwrap(),
            Settled::NotACompaction
        );
    }

    #[tokio::test]
    async fn failed_cancelled_or_empty_run_settles_failed() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        for (status, stdout) in [
            ("failed", Some("partial")),
            ("cancelled", None),
            ("timed_out", Some("x")),
            ("completed", Some("   \n")),
            ("completed", None),
        ] {
            let run = seed_turn(&pool, &c, status, stdout).await;
            pending(&pool, &c, run).await;
            assert_eq!(
                settle(&pool, run).await.unwrap(),
                Settled::Failed { chat_id: c.clone() },
                "{status} {stdout:?}"
            );
            assert_eq!(status_of(&pool, run).await, "failed");
        }
    }

    #[tokio::test]
    async fn an_answered_run_that_read_untrusted_text_settles_failed() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let run = seed_turn(&pool, &c, "completed", Some("# Handoff — from a mail")).await;
        sqlx::query("UPDATE runs SET read_untrusted = 1 WHERE id = ?")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        pending(&pool, &c, run).await;

        assert_eq!(
            settle(&pool, run).await.unwrap(),
            Settled::Failed { chat_id: c.clone() }
        );
        assert_eq!(status_of(&pool, run).await, "failed");
        assert_eq!(active(&pool, &c).await.unwrap(), None, "never replayed");
    }

    #[tokio::test]
    async fn settle_stale_settles_only_ended_pending_rows_of_the_chat() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let other = chat(&pool).await;
        // Aborted before its own settle ran: the run says cancelled, the row still pending.
        let cancelled = seed_turn(&pool, &c, "cancelled", None).await;
        pending(&pool, &c, cancelled).await;
        // Ended with an answer, but the daemon restarted before settling it.
        let answered = seed_turn(&pool, &c, "completed", Some("handoff")).await;
        pending(&pool, &c, answered).await;
        let running = seed_turn(&pool, &c, "running", None).await;
        pending(&pool, &c, running).await;
        let theirs = seed_turn(&pool, &other, "cancelled", None).await;
        pending(&pool, &other, theirs).await;

        let settled = settle_stale(&pool, &c).await.unwrap();

        assert_eq!(
            settled,
            vec![
                (cancelled, Settled::Failed { chat_id: c.clone() }),
                (
                    answered,
                    Settled::Done {
                        chat_id: c.clone(),
                        summary_chars: 7
                    }
                ),
            ]
        );
        assert_eq!(status_of(&pool, cancelled).await, "failed");
        assert_eq!(status_of(&pool, answered).await, "done");
        assert_eq!(status_of(&pool, running).await, "pending");
        assert_eq!(status_of(&pool, theirs).await, "pending");
        assert_eq!(
            settle_stale(&pool, &c).await.unwrap(),
            Vec::new(),
            "nothing left"
        );
    }

    #[tokio::test]
    async fn settle_while_running_is_still_running_and_twice_is_already() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let run = seed_turn(&pool, &c, "running", None).await;
        pending(&pool, &c, run).await;
        assert_eq!(settle(&pool, run).await.unwrap(), Settled::StillRunning);
        assert_eq!(status_of(&pool, run).await, "pending");
        set_run(&pool, run, "completed", Some("handoff")).await;
        assert!(matches!(
            settle(&pool, run).await.unwrap(),
            Settled::Done { .. }
        ));
        assert_eq!(
            settle(&pool, run).await.unwrap(),
            Settled::Already {
                chat_id: c.clone(),
                status: Status::Done
            }
        );
    }

    #[tokio::test]
    async fn active_is_newest_done_above_the_clear_floor() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        assert_eq!(active(&pool, &c).await.unwrap(), None);
        let old = seed_turn(&pool, &c, "completed", Some("old handoff")).await;
        pending(&pool, &c, old).await;
        settle(&pool, old).await.unwrap();
        let failed = seed_turn(&pool, &c, "failed", None).await;
        pending(&pool, &c, failed).await;
        settle(&pool, failed).await.unwrap();
        let new = seed_turn(&pool, &c, "completed", Some("new handoff")).await;
        pending(&pool, &c, new).await;
        settle(&pool, new).await.unwrap();
        let a = active(&pool, &c).await.unwrap().unwrap();
        assert_eq!(a.run_id, new);
        assert_eq!(a.summary, "new handoff");
        assert!(!a.created_at.is_empty());
        // A floor at or above the compaction run hides it even while it is still `done`.
        sqlx::query("UPDATE chats SET cleared_after_run_id = ? WHERE chat_id = ?")
            .bind(new)
            .bind(&c)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(active(&pool, &c).await.unwrap(), None);
    }

    #[tokio::test]
    async fn clear_context_marks_done_compactions_cleared() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let other = chat(&pool).await;
        let done = seed_turn(&pool, &c, "completed", Some("handoff")).await;
        pending(&pool, &c, done).await;
        settle(&pool, done).await.unwrap();
        let failed = seed_turn(&pool, &c, "failed", None).await;
        pending(&pool, &c, failed).await;
        settle(&pool, failed).await.unwrap();
        let theirs = seed_turn(&pool, &other, "completed", Some("handoff")).await;
        pending(&pool, &other, theirs).await;
        settle(&pool, theirs).await.unwrap();

        crate::chats::clear_context(&pool, &c).await.unwrap();

        assert_eq!(status_of(&pool, done).await, "cleared");
        assert_eq!(status_of(&pool, failed).await, "failed");
        assert_eq!(status_of(&pool, theirs).await, "done");
        assert_eq!(active(&pool, &c).await.unwrap(), None);
        assert!(active(&pool, &other).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn has_exchanges_since_clear_counts_only_answered_turns_after_the_floor() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        assert!(!has_exchanges_since_clear(&pool, &c).await.unwrap());
        seed_turn(&pool, &c, "failed", Some("x")).await;
        seed_turn(&pool, &c, "completed", Some("  ")).await;
        seed_turn(&pool, &c, "running", None).await;
        assert!(!has_exchanges_since_clear(&pool, &c).await.unwrap());
        seed_turn(&pool, &c, "completed", Some("an answer")).await;
        assert!(has_exchanges_since_clear(&pool, &c).await.unwrap());
        crate::chats::clear_context(&pool, &c).await.unwrap();
        assert!(!has_exchanges_since_clear(&pool, &c).await.unwrap());
        seed_turn(&pool, &c, "completed", Some("after the clear")).await;
        assert!(has_exchanges_since_clear(&pool, &c).await.unwrap());
    }

    #[tokio::test]
    async fn migration_check_rejects_an_unknown_status() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let run = seed_turn(&pool, &c, "completed", Some("x")).await;
        let err = sqlx::query(
            "INSERT INTO chat_compactions (chat_id, run_id, status, prompt_version, engine)
             VALUES (?, ?, 'archived', 'nucleos-compact v1', 'codex')",
        )
        .bind(&c)
        .bind(run)
        .execute(&pool)
        .await;
        assert!(err.is_err());
        for s in [
            Status::Pending,
            Status::Done,
            Status::Failed,
            Status::Cleared,
        ] {
            assert_eq!(Status::from_wire(s.as_str()), Some(s));
        }
        assert_eq!(Status::from_wire("archived"), None);
    }
}
