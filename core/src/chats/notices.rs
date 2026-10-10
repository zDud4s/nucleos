//! What a department said in a conversation, without the conversation answering.
//!
//! A department's only voice used to be its delivery: a folder the owner opens when the work is
//! over. Anything it wanted to say before then — "the source you gave me is dead", "half of this is
//! already done elsewhere" — waited for a document nobody had reason to open yet.
//!
//! **Not a relay, and the distinction is the design rather than an implementation detail.**
//! `send_to_chat` hands a message to a conversation and that conversation ANSWERS: a turn starts, a
//! model thinks, money is spent. Three things make that wrong here, and each of them is ours rather
//! than accidental:
//!
//! - `relay::admit` refuses when the owner is away. Correct for delegation — a message handed over
//!   should land where somebody is watching — and wrong for a report, which is precisely the thing
//!   that should survive the night.
//! - §13 gives a relayed turn `ToolPolicy::McpOnly`. The delegation that would justify the spend —
//!   "I found a bug, you have the tools, fix it" — arrives DISARMED, because we shut that door on
//!   purpose.
//! - `relay::chain_of` walks `runs.chat_id`, which a team node does not have. A team-sourced relay
//!   refuses itself at the chain brake today; widening it means rebuilding `chat_relays`.
//!
//! So the words appear and the owner decides. That keeps the decision to spend with a person, which
//! is where it belongs for a message nobody asked for.
//!
//! **This module owns the `chat_notices` SQL and nothing else**, the way `notes.rs` and
//! `team_notes.rs` own theirs. It decides nothing about who may write one — `team::report` holds
//! that — and nothing about how one is drawn.
//!
//! **What it deliberately does NOT do: reach the model.** A notice is not in
//! `assistant::recent_exchanges` and not in the CLI's resumed session, so a conversation's next turn
//! does not know a department spoke. That is a real limit and it is chosen: the alternative is
//! fabricating a `prompt`/`stdout` pair that no turn ever produced, and inserting it into the
//! history a model reasons from. An owner who wants the conversation to act on a report says so, in
//! a sentence, which is also the moment they decide it is worth a turn.

use sqlx::SqlitePool;

/// One thing a department said in a conversation.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct ChatNotice {
    pub id: i64,
    pub chat_id: String,
    pub team_run_id: String,
    pub from_agent_id: String,
    pub from_run_id: i64,
    pub body: String,
    /// `department` (the default), `restart` or `untrusted`.
    pub kind: String,
    pub created_at: String,
}

/// A chat turn the daemon's restart cut short.
pub const KIND_RESTART: &str = "restart";
/// A rooted turn that has just read third-party text and is read-only from here on.
pub const KIND_UNTRUSTED: &str = "untrusted";
/// What a `restart` notice says.
pub const RESTART_NOTICE: &str = "The daemon restarted during this turn; the session is intact.";
/// What an `untrusted` notice says.
pub const UNTRUSTED_NOTICE: &str = "This turn read third-party text. From now on only reads are allowed in it, and the session will be rotated when the turn ends.";

/// Named once so the readers below cannot drift into selecting different shapes of the same row.
const NOTICE_COLUMNS: &str =
    "id, chat_id, team_run_id, from_agent_id, from_run_id, body, kind, created_at";

/// Files a note from NucleOS itself (not a department) in a conversation.
///
/// `team_run_id` is empty, so `team.rs`, which counts by it, never counts one as a department's.
pub async fn post_system(
    pool: &SqlitePool,
    chat_id: &str,
    kind: &str,
    from_run_id: i64,
    body: &str,
) -> sqlx::Result<i64> {
    Ok(sqlx::query(
        "INSERT INTO chat_notices
             (chat_id, team_run_id, from_agent_id, from_run_id, body, kind, created_at)
         VALUES (?, '', 'nucleos', ?, ?, ?, ?)",
    )
    .bind(chat_id)
    .bind(from_run_id)
    .bind(body)
    .bind(kind)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?
    .last_insert_rowid())
}

/// Files the words in a conversation. Nothing is woken and nothing is spent.
///
/// Deliberately takes no "and now answer it" flag. A notice that could sometimes start a turn would
/// be two features sharing a table and one of them would be a relay wearing a different name — and
/// the relay path already exists, with brakes this one does not have and must not silently inherit.
pub async fn post(
    pool: &SqlitePool,
    chat_id: &str,
    team_run_id: &str,
    from_agent_id: &str,
    from_run_id: i64,
    body: &str,
) -> sqlx::Result<i64> {
    Ok(sqlx::query(
        "INSERT INTO chat_notices
             (chat_id, team_run_id, from_agent_id, from_run_id, body, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(chat_id)
    .bind(team_run_id)
    .bind(from_agent_id)
    .bind(from_run_id)
    .bind(body)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?
    .last_insert_rowid())
}

/// Everything this conversation was told, oldest first.
///
/// Unbounded on purpose, unlike the transcript beside it: that one takes the last
/// `ASSISTANT_TRANSCRIPT_LIMIT` turns because a long conversation is long in TURNS, and a
/// conversation with more than a handful of departmental reports in it does not exist. If one ever
/// does, the limit belongs here and not in the caller, so that the two cannot disagree about which
/// end of the list they kept.
pub async fn for_chat(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Vec<ChatNotice>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {NOTICE_COLUMNS} FROM chat_notices WHERE chat_id = ? ORDER BY id"
    )))
    .bind(chat_id)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    /// The unread count as the WINDOW reads it.
    ///
    /// Through `chats::get` and not through a query of this module's own, deliberately. The count
    /// lives in `chats::list`'s SELECT, beside `waiting` and `relayed_waiting`, because all three
    /// are read in one pass for every conversation in the sidebar — and a second copy here, however
    /// convenient to test with, would be a second answer to the same question, free to drift from
    /// the one the person actually sees.
    async fn unread(pool: &SqlitePool, chat_id: &str) -> i64 {
        crate::chats::get(pool, chat_id)
            .await
            .unwrap()
            .expect("the chat exists")
            .notices_waiting
    }

    async fn a_chat(pool: &SqlitePool, chat_id: &str) {
        sqlx::query("INSERT INTO chats (chat_id, brain, created_at) VALUES (?, 'cloud', ?)")
            .bind(chat_id)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(pool)
            .await
            .unwrap();
    }

    /// What was said reaches the conversation it was said in, and no other.
    #[tokio::test]
    async fn a_report_lands_in_the_conversation_it_was_addressed_to() {
        let pool = test_pool().await;
        a_chat(&pool, "c-1").await;
        a_chat(&pool, "c-2").await;

        post(
            &pool,
            "c-1",
            "tr-1",
            "director",
            4,
            "a fonte que deste está morta",
        )
        .await
        .unwrap();

        let told = for_chat(&pool, "c-1").await.unwrap();
        assert_eq!(told.len(), 1);
        assert_eq!(told[0].body, "a fonte que deste está morta");
        assert_eq!(told[0].from_agent_id, "director");
        assert!(for_chat(&pool, "c-2").await.unwrap().is_empty());
    }

    /// The unread count is its own, and opening the conversation clears it.
    ///
    /// The second half runs through `chats::mark_seen`, not through a write of its own: there has to
    /// be exactly one moment at which a conversation becomes read, or the two watermarks drift and a
    /// conversation reads as half-open.
    #[tokio::test]
    async fn a_report_waits_until_the_conversation_is_opened() {
        let pool = test_pool().await;
        a_chat(&pool, "c-1").await;

        post(&pool, "c-1", "tr-1", "director", 4, "primeiro")
            .await
            .unwrap();
        post(&pool, "c-1", "tr-1", "director", 4, "segundo")
            .await
            .unwrap();
        assert_eq!(unread(&pool, "c-1").await, 2);

        crate::chats::mark_seen(&pool, "c-1").await.unwrap();
        assert_eq!(unread(&pool, "c-1").await, 0);

        // And a report that lands afterwards is unread again, which is the whole point of a
        // watermark rather than a flag.
        post(&pool, "c-1", "tr-1", "director", 4, "terceiro")
            .await
            .unwrap();
        assert_eq!(unread(&pool, "c-1").await, 1);
    }

    /// A conversation nobody told anything is not a conversation with an unread report.
    ///
    /// Worth its own test because the count is a correlated subquery against a `COALESCE`d
    /// watermark: a chat with no notices and a chat whose watermark is unset are different shapes
    /// reaching the same zero, and only one of them is the state most conversations are in.
    #[tokio::test]
    async fn silence_counts_as_nothing_waiting() {
        let pool = test_pool().await;
        a_chat(&pool, "c-1").await;

        assert_eq!(unread(&pool, "c-1").await, 0);
        assert!(for_chat(&pool, "c-1").await.unwrap().is_empty());
    }

    /// A running turn row for a chat (mode `assistant`) or any other run; returns its id.
    async fn a_running_run(pool: &SqlitePool, mode: &str, chat_id: Option<&str>) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, chat_id, created_at)
             VALUES ('p', 'running', ?, ?, ?)",
        )
        .bind(mode)
        .bind(chat_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// A restart tells the chat which turn it cut, exactly once, and tells no chat about a run
    /// that never belonged to one.
    #[tokio::test]
    async fn a_restart_leaves_a_continue_notice_on_the_chat_turn_it_cut() {
        let pool = test_pool().await;
        a_chat(&pool, "c-1").await;
        let turn = a_running_run(&pool, "assistant", Some("c-1")).await;
        let other = a_running_run(&pool, "worktree", None).await;

        crate::runs::reconcile_orphaned_runs(&pool).await.unwrap();

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(turn)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "interrupted");

        let told = for_chat(&pool, "c-1").await.unwrap();
        assert_eq!(told.len(), 1);
        assert_eq!(told[0].kind, KIND_RESTART);
        assert_eq!(told[0].from_run_id, turn);
        assert_eq!(told[0].body, RESTART_NOTICE);

        let notices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_notices")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            notices, 1,
            "the non-chat run {other} must not be announced anywhere"
        );
    }

    /// The first read of third-party text in a rooted turn is announced; the second is not.
    #[tokio::test]
    async fn a_rooted_turn_reading_third_party_text_is_told_once() {
        let pool = test_pool().await;
        a_chat(&pool, "c-1").await;
        crate::chats::set_cwd(&pool, "c-1", "C:/x").await.unwrap();
        let turn = a_running_run(&pool, "assistant", Some("c-1")).await;

        crate::runs::mark_untrusted_context(&pool, turn)
            .await
            .unwrap();
        crate::runs::mark_untrusted_context(&pool, turn)
            .await
            .unwrap();

        let told = for_chat(&pool, "c-1").await.unwrap();
        assert_eq!(told.len(), 1, "only the first mark of a turn notifies");
        assert_eq!(told[0].kind, KIND_UNTRUSTED);
        assert_eq!(told[0].from_run_id, turn);
        assert_eq!(told[0].body, UNTRUSTED_NOTICE);
    }

    /// An unrooted conversation has no barrier to announce, so it gets no notice.
    #[tokio::test]
    async fn an_unrooted_turn_reading_third_party_text_gets_no_notice() {
        let pool = test_pool().await;
        a_chat(&pool, "c-1").await;
        let turn = a_running_run(&pool, "assistant", Some("c-1")).await;

        crate::runs::mark_untrusted_context(&pool, turn)
            .await
            .unwrap();

        assert!(for_chat(&pool, "c-1").await.unwrap().is_empty());
        let marked: i64 = sqlx::query_scalar("SELECT read_untrusted FROM runs WHERE id = ?")
            .bind(turn)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(marked, 1, "the mark itself is still written");
    }

    /// A department's notice keeps reading back as a department's, whatever kinds exist beside it.
    #[tokio::test]
    async fn a_departments_notice_reads_back_as_a_department() {
        let pool = test_pool().await;
        a_chat(&pool, "c-1").await;

        post(&pool, "c-1", "tr-1", "director", 4, "a word from the team")
            .await
            .unwrap();

        let told = for_chat(&pool, "c-1").await.unwrap();
        assert_eq!(told.len(), 1);
        assert_eq!(told[0].kind, "department");
    }
}
