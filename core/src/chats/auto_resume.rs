//! A chat turn a daemon restart interrupted resumes by itself, a limited number of times in a row.
//!
//! `runs::reconcile_orphaned_runs` calls [`resume_or_notice`] for each cut assistant turn. The
//! resume is an ordinary queued message: the startup drain (`assistant::drain_waiting_queues`,
//! spawned in `main.rs` after startup recovery) sends it through `send_message`, which resumes the
//! CLI session. A completed turn resets the count, so only restarts that keep killing the same
//! turn reach the limit.

use sqlx::SqlitePool;

/// Consecutive interruptions that are resumed; the next one stays interrupted.
pub const AUTO_RESUME_LIMIT: i64 = 3;

/// What the queued resume says to the model.
pub const AUTO_RESUME_TEXT: &str = "The daemon restarted and interrupted your previous turn. Continue where you left off. Subagents and background tasks of that process did not survive, so check what actually finished before relying on it.";

/// What the owner-visible notice says when a resume was queued.
pub const RESUMING_NOTICE: &str =
    "The daemon restarted during this turn; resuming it automatically.";

/// Queues the resume of a cut turn when the chat is below the limit, otherwise leaves it
/// interrupted. Returns the notice body for the conversation.
///
/// Nothing is queued when the chat already has queued messages: they start a turn on the same
/// session anyway, and a resume appended behind them would only be a stale extra turn. The count
/// is not spent in that case.
pub async fn resume_or_notice(
    pool: &SqlitePool,
    chat_id: &str,
    origin: Option<&str>,
) -> sqlx::Result<&'static str> {
    let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_queue WHERE chat_id = ?")
        .bind(chat_id)
        .fetch_one(pool)
        .await?;
    if queued > 0 {
        return Ok(super::notices::RESTART_NOTICE);
    }
    // One statement: the limit check and the increment cannot disagree. No row (a chat that no
    // longer exists) or a spent count both fall through to the plain notice.
    let spent: Option<i64> = sqlx::query_scalar(
        "UPDATE chats SET auto_resumes = auto_resumes + 1
          WHERE chat_id = ? AND auto_resumes < ? RETURNING auto_resumes",
    )
    .bind(chat_id)
    .bind(AUTO_RESUME_LIMIT)
    .fetch_optional(pool)
    .await?;
    if spent.is_none() {
        return Ok(super::notices::RESTART_NOTICE);
    }
    let origin = crate::assistant_types::Origin::from_wire(origin).as_wire();
    super::enqueue(pool, chat_id, AUTO_RESUME_TEXT, origin, "[]").await?;
    Ok(RESUMING_NOTICE)
}

/// A completed turn ends the streak of interruptions.
pub async fn reset(pool: &SqlitePool, chat_id: &str) {
    if let Err(error) =
        sqlx::query("UPDATE chats SET auto_resumes = 0 WHERE chat_id = ? AND auto_resumes <> 0")
            .bind(chat_id)
            .execute(pool)
            .await
    {
        tracing::warn!(%error, chat_id, "could not reset the automatic resume count");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chats::notices::{RESTART_NOTICE, for_chat};

    async fn a_chat(pool: &SqlitePool, chat_id: &str) {
        sqlx::query("INSERT INTO chats (chat_id, brain, created_at) VALUES (?, 'cloud', ?)")
            .bind(chat_id)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(pool)
            .await
            .unwrap();
    }

    async fn a_running_turn(pool: &SqlitePool, chat_id: &str, origin: &str) {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, chat_id, origin, created_at)
             VALUES ('p', 'running', 'assistant', ?, ?, ?)",
        )
        .bind(chat_id)
        .bind(origin)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn waiting(pool: &SqlitePool, chat_id: &str) -> Vec<(String, Option<String>)> {
        sqlx::query_as("SELECT text, origin FROM chat_queue WHERE chat_id = ? ORDER BY id")
            .bind(chat_id)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn notice_bodies(pool: &SqlitePool, chat_id: &str) -> Vec<String> {
        for_chat(pool, chat_id)
            .await
            .unwrap()
            .into_iter()
            .map(|n| n.body)
            .collect()
    }

    /// What the startup drain does with a resume: takes it off the queue.
    async fn drained(pool: &SqlitePool, chat_id: &str) {
        while crate::chats::take_queued(pool, chat_id)
            .await
            .unwrap()
            .is_some()
        {}
    }

    #[tokio::test]
    async fn an_interrupted_turn_queues_one_resume_with_its_origin() {
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c-1").await;
        a_running_turn(&pool, "c-1", "voice").await;

        crate::execution::runs::reconcile_orphaned_runs(&pool)
            .await
            .unwrap();

        assert_eq!(
            waiting(&pool, "c-1").await,
            vec![(AUTO_RESUME_TEXT.to_string(), Some("voice".to_string()))]
        );
        assert_eq!(notice_bodies(&pool, "c-1").await, vec![RESUMING_NOTICE]);
    }

    #[tokio::test]
    async fn the_fourth_interruption_stays_interrupted_and_says_so() {
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c-1").await;
        for _ in 0..=AUTO_RESUME_LIMIT {
            a_running_turn(&pool, "c-1", "shell").await;
            crate::execution::runs::reconcile_orphaned_runs(&pool)
                .await
                .unwrap();
            drained(&pool, "c-1").await;
        }
        let bodies = notice_bodies(&pool, "c-1").await;
        assert_eq!(bodies.len(), 4);
        assert_eq!(bodies[3], RESTART_NOTICE);
        assert!(
            bodies[..3].iter().all(|b| b == RESUMING_NOTICE),
            "{bodies:?}"
        );
        assert!(waiting(&pool, "c-1").await.is_empty());
    }

    #[tokio::test]
    async fn a_completed_turn_resets_the_count() {
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c-1").await;
        for _ in 0..2 {
            a_running_turn(&pool, "c-1", "shell").await;
            crate::execution::runs::reconcile_orphaned_runs(&pool)
                .await
                .unwrap();
            drained(&pool, "c-1").await;
        }
        reset(&pool, "c-1").await;
        let count: i64 = sqlx::query_scalar("SELECT auto_resumes FROM chats WHERE chat_id = 'c-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn a_turn_killed_while_parked_resumes() {
        // The ask registry is in memory and empty after a restart, so an open ask before it
        // changes nothing here: the CLI asks again when the resumed turn reaches the tool.
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c-parked").await;
        a_running_turn(&pool, "c-parked", "shell").await;
        crate::execution::runs::reconcile_orphaned_runs(&pool)
            .await
            .unwrap();
        assert_eq!(waiting(&pool, "c-parked").await.len(), 1);
    }

    #[tokio::test]
    async fn resume_after_restart_cannot_loop_on_restart() {
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c-1").await;
        let mut queued = 0;
        for _ in 0..4 {
            a_running_turn(&pool, "c-1", "shell").await;
            crate::execution::runs::reconcile_orphaned_runs(&pool)
                .await
                .unwrap();
            queued += waiting(&pool, "c-1").await.len();
            drained(&pool, "c-1").await;
        }
        assert_eq!(queued, 3);
    }

    #[tokio::test]
    async fn a_chat_with_queued_work_gets_no_resume() {
        let pool = crate::testdb::fresh_pool().await;
        a_chat(&pool, "c-1").await;
        crate::chats::enqueue(&pool, "c-1", "typed meanwhile", "shell", "[]")
            .await
            .unwrap();
        a_running_turn(&pool, "c-1", "shell").await;

        crate::execution::runs::reconcile_orphaned_runs(&pool)
            .await
            .unwrap();

        let texts: Vec<String> = waiting(&pool, "c-1")
            .await
            .into_iter()
            .map(|w| w.0)
            .collect();
        assert_eq!(texts, vec!["typed meanwhile".to_string()]);
        assert_eq!(notice_bodies(&pool, "c-1").await, vec![RESTART_NOTICE]);
    }
}
