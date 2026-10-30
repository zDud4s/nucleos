//! `/compact` on the assistant's side (spec §7): starting the compaction turn, settling it when it
//! ends, and carrying a done handoff into the turns that follow it.
//!
//! The rows themselves are `chats::compactions`. This module adds what a settled compaction does:
//! a `done` one forgets the session, consumes any older pending root rotation and posts a notice;
//! a `failed` one only posts a notice. After a `done` compaction both turn paths read the history
//! floored at its run and lead with the handoff, until the next compaction or `/clear`.
//!
//! **`settle` runs from two places** — the end of both turn paths and right after `start` files
//! the `pending` row — because a fast engine can end the turn before the row exists. The store
//! guards the transition, so exactly one caller sees `Done`/`Failed`; a caller that finds the row
//! already `done` forgets the session again, since the turn's own session upsert may land after
//! the route's forget. A turn aborted before its own settle (a cancel, `/stop`, a daemon restart)
//! leaves its row `pending`; `settle_stale` settles those, from `start` and from `/stop`.

use sqlx::SqlitePool;

use crate::chats::compactions::{self, NOTICE_KIND, Settled, Status};
use crate::state::AppState;

use super::Origin;

/// Why `/compact` did not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// A turn is in flight for the chat (409 `busy`).
    Busy,
    /// No answered turn since the last `/clear` (400 `nothing_to_compact`).
    NothingToCompact,
    /// The turn could not be sent.
    Send(String),
    /// A database read or write failed.
    Store(String),
}

/// What `runs.prompt` keeps for a compaction the daemon started on its own.
pub const AUTO_TYPED: &str = "/compact (auto)";

/// What a failed compaction says in the chat.
pub const FAILED_NOTICE: &str = "The compaction failed; the session stays as it was.";

/// Starts the compaction turn of `chat_id`: `typed` is what `runs.prompt` keeps (`/compact
/// [focus]`), the model reads the versioned compaction prompt. Returns the turn's run id.
pub async fn start(
    state: &AppState,
    chat_id: &str,
    typed: &str,
    focus: Option<&str>,
) -> Result<i64, StartError> {
    start_as(state, chat_id, typed, focus, false).await
}

/// [`start`] for a compaction the daemon began itself (`auto`) or the user typed.
pub async fn start_as(
    state: &AppState,
    chat_id: &str,
    typed: &str,
    focus: Option<&str>,
    auto: bool,
) -> Result<i64, StartError> {
    if super::is_busy(chat_id) {
        return Err(StartError::Busy);
    }
    // An earlier compaction whose turn was aborted before it settled; its notice belongs before
    // this one's.
    settle_stale(&state.pool, chat_id).await;
    let store = |e: sqlx::Error| StartError::Store(e.to_string());
    if !compactions::has_exchanges_since_clear(&state.pool, chat_id)
        .await
        .map_err(store)?
    {
        return Err(StartError::NothingToCompact);
    }
    // Read before the turn starts, so the row records the engine the turn was sent to. A chat with
    // no row (only reachable from a caller that skipped the 404) is a cloud chat on the default.
    let chat = crate::chats::get(&state.pool, chat_id)
        .await
        .map_err(store)?;
    let (brain, model) = match &chat {
        Some(c) => (crate::chats::Brain::from_wire(&c.brain), c.model.clone()),
        None => (crate::chats::Brain::Cloud, None),
    };
    let engine = super::slash::capabilities::engine_of(
        &crate::config::models_config_now(),
        brain,
        model.as_deref(),
    );
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let prompt = super::slash::compact_prompt::build(focus, &date);
    let id = super::send::send_message_inner(
        state,
        chat_id,
        typed,
        Some(&prompt),
        &[],
        Origin::Shell,
        None,
    )
    .await
    .map_err(|e| {
        if e == super::TURN_IN_PROGRESS {
            StartError::Busy
        } else {
            StartError::Send(e)
        }
    })?;
    let focus = focus.map(str::trim).filter(|f| !f.is_empty());
    compactions::insert_pending_as(
        &state.pool,
        chat_id,
        id,
        super::slash::compact_prompt::COMPACT_PROMPT_VERSION,
        engine.as_str(),
        model.as_deref(),
        focus,
        auto,
    )
    .await
    .map_err(store)?;
    // The turn may already have ended, and its own settle found no row to settle.
    settle(&state.pool, id).await;
    Ok(id)
}

/// Completed turns that must pass after a failed automatic compaction before another is tried.
const AUTO_BACKOFF_TURNS: i64 = 3;

/// Files an automatic compaction after a completed turn when the guards allow it. Never fails.
///
/// Reads the guards cheapest first and stops at the first that settles the question. `run_id` is
/// the turn that just ended and `fill` its context fill: measured on the CLI path, estimated on the
/// local one.
pub(super) async fn maybe_auto_compact(
    state: &AppState,
    chat_id: &str,
    run_id: i64,
    fill: Option<i64>,
) {
    let pool = &state.pool;
    if !crate::chats::auto_compact(pool, chat_id).await {
        return;
    }
    let window = match crate::chats::answering(pool, chat_id).await {
        Ok(answering) => answering.context_window,
        Err(error) => {
            tracing::warn!(%error, chat_id, "could not read the chat's context window");
            return;
        }
    };
    let soft = super::rotation::soft_fill();
    // A fill that cannot fire needs none of the reads below.
    if !should_auto_compact(fill, soft, window, AutoCompactGuards::default()) {
        return;
    }
    let is_compaction = is_compaction(pool, run_id).await;
    let pending = compactions::has_pending(pool, chat_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, chat_id, "could not read pending compactions");
            true
        });
    let busy = super::is_busy(chat_id);
    let in_backoff = match compactions::latest_auto(pool, chat_id).await {
        Ok(latest) => {
            let turns_since = match latest {
                Some((since, _)) => sqlx::query_scalar(
                    "SELECT COUNT(*) FROM runs
                      WHERE chat_id = ? AND mode = 'assistant' AND status = 'completed' AND id > ?",
                )
                .bind(chat_id)
                .bind(since)
                .fetch_one(pool)
                .await
                .unwrap_or(0),
                None => 0,
            };
            in_backoff(latest, turns_since)
        }
        Err(error) => {
            tracing::warn!(%error, chat_id, "could not read the last automatic compaction");
            true
        }
    };
    let nodes = super::rotation::root_nodes(pool);
    let child_waiting = match nodes.active_root_for_chat(chat_id).await {
        Some(root) => nodes.has_waiting_child(&root).await,
        None => false,
    };
    let nothing_to_compact = !compactions::has_exchanges_since_clear(pool, chat_id)
        .await
        .unwrap_or(false);
    let guards = AutoCompactGuards {
        busy,
        pending,
        is_compaction,
        nothing_to_compact,
        in_backoff,
        child_waiting,
    };
    if !should_auto_compact(fill, soft, window, guards) {
        return;
    }
    match start_as(state, chat_id, AUTO_TYPED, None, true).await {
        Ok(id) => tracing::info!(chat_id, run_id = id, "automatic compaction started"),
        Err(StartError::Busy | StartError::NothingToCompact) => {
            tracing::debug!(chat_id, "automatic compaction not started");
        }
        Err(error) => tracing::warn!(?error, chat_id, "automatic compaction failed to start"),
    }
}

/// Whether the latest automatic compaction failed too recently to try another: it is `failed` and
/// fewer than the backoff window of completed turns have passed since it.
fn in_backoff(latest: Option<(i64, Status)>, turns_since: i64) -> bool {
    matches!(latest, Some((_, Status::Failed))) && turns_since < AUTO_BACKOFF_TURNS
}

/// What a failed compaction says in the chat, worded for who started it.
fn failed_notice(auto: bool) -> &'static str {
    if auto {
        "The automatic compaction failed; the session stays as it was."
    } else {
        FAILED_NOTICE
    }
}

/// The notice a done compaction posts: its size in tokens, estimated as characters over four.
fn done_notice(summary_chars: i64, auto: bool) -> String {
    let tokens = (summary_chars.max(0) as u64).div_ceil(4);
    if auto {
        format!(
            "Compacted automatically: the next turn starts from the handoff (~{tokens} tokens)."
        )
    } else {
        format!("Compacted: the next turn starts from the handoff (~{tokens} tokens).")
    }
}

/// Settles the compaction of `run_id`, if it is one and its turn has ended, and acts on it. Never
/// fails: every error is a warning, because the turn it runs after is already over.
pub(super) async fn settle(pool: &SqlitePool, run_id: i64) {
    match compactions::settle(pool, run_id).await {
        Ok(settled) => act(pool, run_id, settled).await,
        Err(error) => tracing::warn!(%error, run_id, "could not settle the compaction"),
    }
}

/// Settles, and acts on, every compaction of `chat_id` left `pending` by a turn that ended without
/// reaching its own [`settle`] (cancelled, `/stop`ped, or cut by a daemon restart). Never fails:
/// an error is a warning, and the rows are tried again on the next call.
pub async fn settle_stale(pool: &SqlitePool, chat_id: &str) {
    match compactions::settle_stale(pool, chat_id).await {
        Ok(settled) => {
            for (run_id, settled) in settled {
                act(pool, run_id, settled).await;
            }
        }
        Err(error) => tracing::warn!(%error, chat_id, "could not settle stale compactions"),
    }
}

/// Whether `run_id` is a compaction turn; `false` on a read error (warned), so the turn is treated
/// as an ordinary one.
pub(super) async fn is_compaction(pool: &SqlitePool, run_id: i64) -> bool {
    compactions::is_compaction(pool, run_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, run_id, "could not tell whether the turn is a compaction");
            false
        })
}

/// What a settled compaction does to the session, the older rotations and the notices.
async fn act(pool: &SqlitePool, run_id: i64, settled: Settled) {
    match settled {
        Settled::Done {
            chat_id,
            summary_chars,
        } => {
            if let Err(error) = super::rotation::consume_older(pool, &chat_id, run_id).await {
                tracing::warn!(%error, run_id, "the compaction could not consume older rotations");
            }
            forget(pool, &chat_id).await;
            let auto = is_auto(pool, run_id).await;
            notice(pool, &chat_id, run_id, &done_notice(summary_chars, auto)).await;
        }
        Settled::Failed { chat_id } => {
            let auto = is_auto(pool, run_id).await;
            notice(pool, &chat_id, run_id, failed_notice(auto)).await;
        }
        Settled::Already {
            chat_id,
            status: Status::Done,
        } => forget(pool, &chat_id).await,
        Settled::Already { .. } | Settled::NotACompaction | Settled::StillRunning => {}
    }
}

/// Whether the daemon started the compaction of `run_id`; `false` on a read error (warned), which
/// words the notice as a manual one.
async fn is_auto(pool: &SqlitePool, run_id: i64) -> bool {
    compactions::is_auto(pool, run_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, run_id, "could not tell whether the compaction was automatic");
            false
        })
}

async fn forget(pool: &SqlitePool, chat_id: &str) {
    if let Err(error) = super::forget_session(pool, chat_id).await {
        tracing::warn!(%error, chat_id, "the compaction could not forget the session");
    }
    super::evict_live(chat_id);
}

async fn notice(pool: &SqlitePool, chat_id: &str, run_id: i64, body: &str) {
    if let Err(error) =
        crate::chats::notices::post_system(pool, chat_id, NOTICE_KIND, run_id, body).await
    {
        tracing::warn!(%error, run_id, "the compaction notice was not posted");
    }
}

/// The done compaction in force, `None` on a read error (warned): a turn answers without the
/// handoff rather than not at all.
async fn active(pool: &SqlitePool, chat_id: &str) -> Option<compactions::Active> {
    compactions::active(pool, chat_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, chat_id, "could not read the compaction in force");
            None
        })
}

/// A sessionless CLI turn's replay: `(handoff, older, tail)`, the two lists floored at the done
/// compaction's run, so nothing it summarised is replayed or rolled up again.
#[allow(clippy::type_complexity)]
pub(super) async fn replay_window(
    pool: &SqlitePool,
    chat_id: &str,
) -> sqlx::Result<(Option<String>, Vec<(String, String)>, Vec<(String, String)>)> {
    let active = active(pool, chat_id).await;
    let floor = active.as_ref().map_or(0, |a| a.run_id);
    let (older, tail) = super::send::history_window_after(pool, chat_id, floor).await?;
    Ok((active.map(|a| a.summary), older, tail))
}

/// The handoff, then a blank line, then the rolled-up summary of what came after it.
pub(super) fn joined(handoff: Option<String>, rolled: Option<String>) -> Option<String> {
    match (handoff, rolled) {
        (Some(h), Some(r)) => Some(format!("{h}\n\n{r}")),
        (Some(h), None) => Some(h),
        (None, r) => r,
    }
}

/// A local or OpenRouter turn's history `(tail, handoff)`, floored like [`replay_window`]. A read
/// error is an empty history (warned): a bot that answers without remembering is worse than one
/// that remembers, and better than one that refuses to answer.
pub(super) async fn local_history(
    pool: &SqlitePool,
    chat_id: &str,
) -> (Vec<(String, String)>, Option<String>) {
    let active = active(pool, chat_id).await;
    let floor = active.as_ref().map_or(0, |a| a.run_id);
    let tail = match super::send::history_window_after(pool, chat_id, floor).await {
        Ok((_, tail)) => tail,
        Err(error) => {
            tracing::warn!(%error, "could not read chat history; answering without it");
            Vec::new()
        }
    };
    (tail, active.map(|a| a.summary))
}

/// The prompt led by the handoff in force, or unchanged without one.
pub(super) fn with_handoff(handoff: Option<&str>, prompt: String) -> String {
    match handoff {
        None => prompt,
        Some(handoff) => format!(
            "Earlier in this conversation, before the exchanges you can see, in summary (what you and the other person had already settled):\n{handoff}\n\nThe new message follows.\n\n{prompt}"
        ),
    }
}

/// Estimated context of a local or OpenRouter turn, for `runs.context_fill`.
///
/// Characters over four of what the NEXT request will resend: the replayed history, this turn's
/// prompt (handoff included) and its answer. Same convention as `Conversation::context_estimate`;
/// these engines report no usage we read, so an estimate beats a NULL that would exempt them from
/// auto-compaction.
pub(super) fn local_fill_estimate(history: &[(String, String)], prompt: &str, answer: &str) -> i64 {
    let chars: usize = history
        .iter()
        .map(|(asked, answered)| asked.chars().count() + answered.chars().count())
        .sum::<usize>()
        + prompt.chars().count()
        + answer.chars().count();
    (chars / 4) as i64
}

/// What the caller knows about the chat when a turn has just completed. Every field is a fact read
/// elsewhere; the decision below does no I/O.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AutoCompactGuards {
    /// A turn is in flight for the chat (`is_busy`).
    pub busy: bool,
    /// The chat already has a `pending` compaction.
    pub pending: bool,
    /// The turn that just ended was itself a compaction.
    pub is_compaction: bool,
    /// No answered exchange since the last `/clear` (`NothingToCompact`).
    pub nothing_to_compact: bool,
    /// The previous automatic compaction failed within the backoff window.
    pub in_backoff: bool,
    /// A child is waiting synchronously on this chat.
    pub child_waiting: bool,
}

/// Whether a just-completed turn should trigger an automatic compaction. Pure: no I/O.
///
/// Unknown fill never fires. The limit is `soft`, lowered to the chat's `window` when that is
/// smaller; fires only strictly above it. Any guard held back wins over the threshold.
pub fn should_auto_compact(
    fill: Option<i64>,
    soft: i64,
    window: Option<i64>,
    guards: AutoCompactGuards,
) -> bool {
    // Destructured without `..` so a new guard field is a compile error here, not silently ignored.
    let AutoCompactGuards {
        busy,
        pending,
        is_compaction,
        nothing_to_compact,
        in_backoff,
        child_waiting,
    } = guards;
    if busy || pending || is_compaction || nothing_to_compact || in_backoff || child_waiting {
        return false;
    }
    let Some(fill) = fill else {
        return false;
    };
    // A window of zero or less is a bad row, not a tiny window: ignoring it keeps one corrupt value
    // from compacting the chat on every turn.
    let limit = match window {
        Some(w) if w > 0 && w < soft => w,
        _ => soft,
    };
    fill > limit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chats::{self, Brain};
    use crate::execution::runner::{FakeCommandRunner, RunOutcome};
    use std::sync::Arc;
    use std::time::Duration;

    async fn chat(pool: &SqlitePool) -> String {
        chats::create(pool, Brain::Cloud, None).await.unwrap()
    }

    /// A completed exchange of `chat`; returns its run id.
    async fn exchange(pool: &SqlitePool, chat: &str, asked: &str, answered: &str) -> i64 {
        run(pool, chat, asked, "completed", Some(answered)).await
    }

    async fn run(
        pool: &SqlitePool,
        chat: &str,
        asked: &str,
        status: &str,
        out: Option<&str>,
    ) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, stdout, created_at)
             VALUES (?, ?, 'assistant', ?, ?, '2026-10-28T10:00:00+00:00')
             RETURNING id",
        )
        .bind(asked)
        .bind(status)
        .bind(chat)
        .bind(out)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// A compaction turn of `chat` with a `pending` row; settle it to make it `done`.
    async fn compaction(pool: &SqlitePool, chat: &str, handoff: &str) -> i64 {
        let id = exchange(pool, chat, "/compact", handoff).await;
        compactions::insert_pending(pool, chat, id, "nucleos-compact v1", "claude", None, None)
            .await
            .unwrap();
        id
    }

    async fn has_session(pool: &SqlitePool, chat: &str) -> bool {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assistant_sessions WHERE chat_id = ?)")
            .bind(chat)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn notices(pool: &SqlitePool, chat: &str) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT body FROM chat_notices WHERE chat_id = ? AND kind = ? ORDER BY id",
        )
        .bind(chat)
        .bind(NOTICE_KIND)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn session(pool: &SqlitePool, chat: &str) {
        super::super::upsert_session(pool, chat, "s-1", "2026-10-28T10:00:00+00:00")
            .await
            .unwrap();
    }

    fn pairs(prefix: &str, range: std::ops::RangeInclusive<usize>) -> Vec<(String, String)> {
        range
            .map(|n| (format!("{prefix}-q{n}"), format!("{prefix}-a{n}")))
            .collect()
    }

    async fn seed(pool: &SqlitePool, chat: &str, list: &[(String, String)]) {
        for (q, a) in list {
            exchange(pool, chat, q, a).await;
        }
    }

    #[tokio::test]
    async fn replay_uses_the_handoff_and_only_the_tail_after_it() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        seed(&pool, &c, &pairs("pre", 1..=2)).await;
        let id = compaction(&pool, &c, "HANDOFF").await;
        settle(&pool, id).await;
        seed(&pool, &c, &pairs("post", 1..=2)).await;

        let (handoff, older, tail) = replay_window(&pool, &c).await.unwrap();

        assert_eq!(handoff.as_deref(), Some("HANDOFF"));
        assert!(older.is_empty(), "{older:?}");
        assert_eq!(tail, pairs("post", 1..=2));
    }

    #[tokio::test]
    async fn clear_invalidates_the_compaction() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        seed(&pool, &c, &pairs("pre", 1..=1)).await;
        let id = compaction(&pool, &c, "HANDOFF").await;
        settle(&pool, id).await;
        chats::clear_context(&pool, &c).await.unwrap();

        let (handoff, older, tail) = replay_window(&pool, &c).await.unwrap();

        assert_eq!(handoff, None);
        assert!(older.is_empty() && tail.is_empty());
    }

    #[tokio::test]
    async fn a_compaction_below_the_clear_floor_is_ignored() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let id = compaction(&pool, &c, "HANDOFF").await;
        settle(&pool, id).await;
        // Left `done`, but under a floor set without `clear_context`.
        sqlx::query("UPDATE chats SET cleared_after_run_id = ? WHERE chat_id = ?")
            .bind(id)
            .bind(&c)
            .execute(&pool)
            .await
            .unwrap();
        seed(&pool, &c, &pairs("post", 1..=1)).await;

        let (handoff, _, tail) = replay_window(&pool, &c).await.unwrap();

        assert_eq!(handoff, None);
        assert_eq!(tail, pairs("post", 1..=1));
    }

    #[tokio::test]
    async fn done_consumes_the_older_pending_rotation_and_keeps_a_newer_one() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let before = exchange(&pool, &c, "q", "a").await;
        let id = compaction(&pool, &c, "HANDOFF").await;
        for from in [before, id, id + 50] {
            sqlx::query(
                "INSERT INTO root_rotations (chat_id, root_id, from_run_id, prefix, tainted, created_at)
                 VALUES (?, 'r', ?, ?, 0, '2026-10-28T10:00:00+00:00')",
            )
            .bind(&c)
            .bind(from)
            .bind(format!("P{from}"))
            .execute(&pool)
            .await
            .unwrap();
        }

        settle(&pool, id).await;

        let left: Vec<String> = sqlx::query_scalar(
            "SELECT prefix FROM root_rotations WHERE chat_id = ? AND delivered_run_id IS NULL",
        )
        .bind(&c)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(left, vec![format!("P{}", id + 50)]);
    }

    #[tokio::test]
    async fn failed_does_not_forget_the_session() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let id = run(&pool, &c, "/compact", "failed", Some("partial")).await;
        compactions::insert_pending(&pool, &c, id, "nucleos-compact v1", "codex", None, None)
            .await
            .unwrap();
        session(&pool, &c).await;

        settle(&pool, id).await;

        assert!(has_session(&pool, &c).await);
        assert_eq!(notices(&pool, &c).await, vec![FAILED_NOTICE.to_string()]);
    }

    #[tokio::test]
    async fn done_forgets_the_session_and_posts_the_notice() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        // Nine characters: ceil(9 / 4) = 3 tokens.
        let id = compaction(&pool, &c, "123456789").await;
        session(&pool, &c).await;

        settle(&pool, id).await;

        assert!(!has_session(&pool, &c).await);
        assert_eq!(
            notices(&pool, &c).await,
            vec!["Compacted: the next turn starts from the handoff (~3 tokens).".to_string()]
        );
    }

    #[tokio::test]
    async fn an_untrusted_compaction_keeps_the_session_and_posts_the_failed_notice() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let id = compaction(&pool, &c, "HANDOFF from a mail").await;
        sqlx::query("UPDATE runs SET read_untrusted = 1 WHERE id = ?")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        session(&pool, &c).await;

        settle(&pool, id).await;

        assert!(has_session(&pool, &c).await);
        assert_eq!(notices(&pool, &c).await, vec![FAILED_NOTICE.to_string()]);
        assert_eq!(replay_window(&pool, &c).await.unwrap().0, None);
    }

    #[tokio::test]
    async fn settle_stale_acts_on_an_aborted_compaction_once() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        // The turn's task was aborted: the run is cancelled, its own settle never ran.
        let id = run(&pool, &c, "/compact", "cancelled", None).await;
        compactions::insert_pending(&pool, &c, id, "nucleos-compact v1", "claude", None, None)
            .await
            .unwrap();
        session(&pool, &c).await;

        settle_stale(&pool, &c).await;
        settle_stale(&pool, &c).await;

        assert_eq!(compaction_status(&pool, id).await, "failed");
        assert!(has_session(&pool, &c).await);
        assert_eq!(notices(&pool, &c).await, vec![FAILED_NOTICE.to_string()]);
    }

    #[tokio::test]
    async fn start_settles_a_stale_compaction_before_refusing() {
        let state = crate::channels::testkit::state().await;
        let c = chat(&state.pool).await;
        let pool = &state.pool;
        let id = run(pool, &c, "/compact", "cancelled", None).await;
        compactions::insert_pending(pool, &c, id, "nucleos-compact v1", "claude", None, None)
            .await
            .unwrap();

        assert_eq!(
            start(&state, &c, "/compact", None).await,
            Err(StartError::NothingToCompact)
        );

        assert_eq!(compaction_status(&state.pool, id).await, "failed");
        assert_eq!(
            notices(&state.pool, &c).await,
            vec![FAILED_NOTICE.to_string()]
        );
    }

    #[tokio::test]
    async fn is_compaction_tells_a_compaction_turn_from_an_ordinary_one() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let plain = exchange(&pool, &c, "q", "a").await;
        let id = compaction(&pool, &c, "HANDOFF").await;

        assert!(is_compaction(&pool, id).await);
        assert!(!is_compaction(&pool, plain).await);
    }

    #[tokio::test]
    async fn settle_on_an_already_done_row_forgets_again_without_a_second_notice() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        let id = compaction(&pool, &c, "HANDOFF").await;
        settle(&pool, id).await;
        // The turn's own session upsert landing after the first forget.
        session(&pool, &c).await;

        settle(&pool, id).await;

        assert!(!has_session(&pool, &c).await);
        assert_eq!(notices(&pool, &c).await.len(), 1);
    }

    #[tokio::test]
    async fn rolled_up_never_sees_exchanges_before_the_compaction() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        seed(&pool, &c, &pairs("pre", 1..=10)).await;
        let id = compaction(&pool, &c, "HANDOFF").await;
        settle(&pool, id).await;
        // More than HISTORY_TURNS, so some post-compaction exchanges are `older`.
        seed(&pool, &c, &pairs("post", 1..=8)).await;

        let (_, older, tail) = replay_window(&pool, &c).await.unwrap();

        assert_eq!(older, pairs("post", 1..=2));
        assert_eq!(tail, pairs("post", 3..=8));
    }

    #[tokio::test]
    async fn local_history_carries_the_handoff_until_cleared() {
        let pool = crate::testdb::fresh_pool().await;
        let c = chat(&pool).await;
        seed(&pool, &c, &pairs("pre", 1..=2)).await;
        let id = compaction(&pool, &c, "HANDOFF").await;
        settle(&pool, id).await;
        seed(&pool, &c, &pairs("post", 1..=1)).await;

        assert_eq!(
            local_history(&pool, &c).await,
            (pairs("post", 1..=1), Some("HANDOFF".to_string()))
        );
        seed(&pool, &c, &pairs("later", 1..=1)).await;
        let (tail, handoff) = local_history(&pool, &c).await;
        assert_eq!(
            handoff.as_deref(),
            Some("HANDOFF"),
            "every later turn keeps it"
        );
        assert_eq!(tail.len(), 2);

        chats::clear_context(&pool, &c).await.unwrap();
        assert_eq!(local_history(&pool, &c).await, (Vec::new(), None));
    }

    #[test]
    fn joined_and_with_handoff_shapes() {
        let s = |x: &str| Some(x.to_string());
        assert_eq!(joined(s("H"), s("R")), s("H\n\nR"));
        assert_eq!(joined(s("H"), None), s("H"));
        assert_eq!(joined(None, s("R")), s("R"));
        assert_eq!(joined(None, None), None);

        assert_eq!(with_handoff(None, "hi".into()), "hi");
        let out = with_handoff(Some("HANDOFF"), "hi".into());
        assert!(out.starts_with("Earlier in this conversation, before the exchanges you can see"));
        let h = out
            .find("\nHANDOFF\n\nThe new message follows.\n\nhi")
            .expect(&out);
        assert!(h > 0 && out.ends_with("hi"));
    }

    #[tokio::test]
    async fn start_refuses_busy_and_nothing_to_compact() {
        let state = crate::channels::testkit::state().await;
        let c = chat(&state.pool).await;
        assert_eq!(
            start(&state, &c, "/compact", None).await,
            Err(StartError::NothingToCompact)
        );
        exchange(&state.pool, &c, "q", "a").await;
        let held = super::super::take_the_slot_for_testing(&c);
        assert_eq!(
            start(&state, &c, "/compact", None).await,
            Err(StartError::Busy)
        );
        drop(held);
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE chat_id = ?")
            .bind(&c)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 1, "a refused /compact starts no turn");
    }

    fn answer(text: &str) -> RunOutcome {
        RunOutcome {
            exit_code: 0,
            stdout: format!(r#"{{"type":"result","subtype":"success","result":"{text}"}}"#),
            stderr: String::new(),
            session_id: Some("s-compact".into()),
            cost_usd: Some(0.0),
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            num_turns: None,
            compacted: false,
            model_requested: false,
        }
    }

    async fn compaction_status(pool: &SqlitePool, run_id: i64) -> String {
        sqlx::query_scalar("SELECT status FROM chat_compactions WHERE run_id = ?")
            .bind(run_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_cli_turn_after_a_done_compaction_is_prompted_with_the_handoff() {
        let fake = Arc::new(FakeCommandRunner::default());
        let mut state = crate::channels::testkit::state().await;
        state.runner = fake.clone();
        let c = chat(&state.pool).await;
        exchange(&state.pool, &c, "old question", "OLD-ANSWER").await;
        *fake.canned.lock().unwrap() = Some(answer("HANDOFF-XYZ"));

        let id = start(&state, &c, "/compact keep the plan", Some("keep the plan"))
            .await
            .unwrap();

        // The compaction turn was told the compaction prompt; runs.prompt keeps what was typed.
        let mut status = String::new();
        for _ in 0..500 {
            status = compaction_status(&state.pool, id).await;
            if status != "pending" && !super::super::is_busy(&c) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(status, "done");
        let typed: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(typed, "/compact keep the plan");
        let told = fake.last_prompt.lock().unwrap().clone().unwrap_or_default();
        assert!(
            told.contains("Additional focus from the user: keep the plan"),
            "{told}"
        );
        assert!(
            !has_session(&state.pool, &c).await,
            "the session is forgotten"
        );

        *fake.canned.lock().unwrap() = Some(answer("fine"));
        let next = super::super::send_message(&state, &c, "what now?", Origin::Shell)
            .await
            .unwrap();
        let mut handed = String::new();
        for _ in 0..500 {
            handed = fake.last_prompt.lock().unwrap().clone().unwrap_or_default();
            if handed.contains("what now?") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(handed.contains("HANDOFF-XYZ"), "{handed}");
        assert!(!handed.contains("OLD-ANSWER"), "{handed}");
        assert!(!handed.contains("Additional focus"), "{handed}");
        assert!(next > id);
    }

    #[test]
    fn local_fill_estimate_is_chars_over_four_of_what_the_next_request_resends() {
        assert_eq!(
            local_fill_estimate(&[("abcd".into(), "efgh".into())], "ijkl", "mnop"),
            4
        );
    }

    #[test]
    fn local_fill_estimate_counts_characters_not_bytes() {
        assert_eq!(
            local_fill_estimate(&[("ééééé".into(), String::new())], "", ""),
            1
        );
    }

    #[test]
    fn local_fill_estimate_of_nothing_is_zero() {
        assert_eq!(local_fill_estimate(&[], "", ""), 0);
    }

    #[test]
    fn local_fill_estimate_includes_the_handoff_through_the_prompt() {
        let prompt = with_handoff(Some(&"h".repeat(400)), "q".into());
        assert!(local_fill_estimate(&[], &prompt, "") >= 100);
    }

    fn open() -> AutoCompactGuards {
        AutoCompactGuards::default()
    }

    #[test]
    fn auto_compact_fires_strictly_above_soft() {
        assert!(should_auto_compact(Some(120_001), 120_000, None, open()));
    }

    #[test]
    fn auto_compact_does_not_fire_at_exactly_soft() {
        assert!(!should_auto_compact(Some(120_000), 120_000, None, open()));
        assert!(!should_auto_compact(Some(119_999), 120_000, None, open()));
    }

    #[test]
    fn auto_compact_does_not_fire_without_a_measurement() {
        assert!(!should_auto_compact(None, 120_000, None, open()));
    }

    #[test]
    fn auto_compact_is_held_back_by_each_guard() {
        let rows = [
            (
                "busy",
                AutoCompactGuards {
                    busy: true,
                    ..open()
                },
            ),
            (
                "pending",
                AutoCompactGuards {
                    pending: true,
                    ..open()
                },
            ),
            (
                "is_compaction",
                AutoCompactGuards {
                    is_compaction: true,
                    ..open()
                },
            ),
            (
                "nothing_to_compact",
                AutoCompactGuards {
                    nothing_to_compact: true,
                    ..open()
                },
            ),
            (
                "in_backoff",
                AutoCompactGuards {
                    in_backoff: true,
                    ..open()
                },
            ),
            (
                "child_waiting",
                AutoCompactGuards {
                    child_waiting: true,
                    ..open()
                },
            ),
        ];
        // Control: with every guard clear the same fill does fire.
        assert!(should_auto_compact(Some(500_000), 120_000, None, open()));
        for (name, guards) in rows {
            assert!(
                !should_auto_compact(Some(500_000), 120_000, None, guards),
                "{name} must hold the compaction back"
            );
        }
    }

    #[test]
    fn auto_compact_uses_a_smaller_context_window_as_the_limit() {
        assert!(should_auto_compact(
            Some(50_001),
            120_000,
            Some(50_000),
            open()
        ));
        assert!(!should_auto_compact(
            Some(50_000),
            120_000,
            Some(50_000),
            open()
        ));
    }

    #[test]
    fn auto_compact_ignores_a_window_larger_than_soft() {
        assert!(should_auto_compact(
            Some(120_001),
            120_000,
            Some(200_000),
            open()
        ));
        assert!(!should_auto_compact(
            Some(120_000),
            120_000,
            Some(200_000),
            open()
        ));
    }

    #[test]
    fn auto_compact_with_a_window_equal_to_soft_behaves_like_soft() {
        assert!(should_auto_compact(
            Some(120_001),
            120_000,
            Some(120_000),
            open()
        ));
        assert!(!should_auto_compact(
            Some(120_000),
            120_000,
            Some(120_000),
            open()
        ));
    }

    #[test]
    fn auto_compact_with_a_nonpositive_window_falls_back_to_soft() {
        for window in [Some(0), Some(-5)] {
            assert!(!should_auto_compact(Some(120_000), 120_000, window, open()));
            assert!(should_auto_compact(Some(120_001), 120_000, window, open()));
        }
    }

    #[test]
    fn done_notice_says_automatic_for_an_auto_compaction() {
        let auto = done_notice(400, true);
        assert!(auto.contains("automatically"), "{auto}");
        assert!(auto.contains("~100 tokens"), "{auto}");
        assert_eq!(
            done_notice(400, false),
            "Compacted: the next turn starts from the handoff (~100 tokens)."
        );
    }

    #[test]
    fn failed_notice_says_automatic_for_an_auto_compaction() {
        let auto = failed_notice(true);
        assert!(auto.contains("automatic"), "{auto}");
        assert_ne!(auto, FAILED_NOTICE);
        assert_eq!(failed_notice(false), FAILED_NOTICE);
    }

    #[tokio::test]
    async fn settling_an_auto_compaction_posts_the_automatic_notice() {
        let pool = crate::testdb::fresh_pool().await;
        let auto_chat = chat(&pool).await;
        let manual_chat = chat(&pool).await;
        // Nine characters: ceil(9 / 4) = 3 tokens.
        let auto_run = exchange(&pool, &auto_chat, "/compact (auto)", "123456789").await;
        compactions::insert_pending_as(
            &pool,
            &auto_chat,
            auto_run,
            "nucleos-compact v1",
            "claude",
            None,
            None,
            true,
        )
        .await
        .unwrap();
        let manual_run = compaction(&pool, &manual_chat, "123456789").await;

        settle(&pool, auto_run).await;
        settle(&pool, manual_run).await;

        let auto_notices = notices(&pool, &auto_chat).await;
        assert_eq!(auto_notices.len(), 1, "{auto_notices:?}");
        assert!(
            auto_notices[0].contains("automatically"),
            "{auto_notices:?}"
        );
        assert_eq!(
            notices(&pool, &manual_chat).await,
            vec!["Compacted: the next turn starts from the handoff (~3 tokens).".to_string()]
        );
    }

    #[tokio::test]
    async fn a_manual_compact_row_is_not_auto_after_start() {
        let fake = Arc::new(FakeCommandRunner::default());
        let mut state = crate::channels::testkit::state().await;
        state.runner = fake.clone();
        let c = chat(&state.pool).await;
        exchange(&state.pool, &c, "q", "a").await;
        *fake.canned.lock().unwrap() = Some(answer("HANDOFF"));

        let id = start(&state, &c, "/compact", None).await.unwrap();

        let auto: i64 = sqlx::query_scalar("SELECT auto FROM chat_compactions WHERE run_id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(auto, 0);
    }

    #[tokio::test]
    async fn maybe_auto_compact_does_nothing_when_the_flag_is_off() {
        let state = crate::channels::testkit::state().await;
        let c = chat(&state.pool).await;
        let run = exchange(&state.pool, &c, "q", "a").await;

        maybe_auto_compact(
            &state,
            &c,
            run,
            Some(super::super::rotation::DEFAULT_SOFT_FILL + 1),
        )
        .await;

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_compactions")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    async fn compaction_rows(pool: &SqlitePool, chat: &str, auto: Option<i64>) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM chat_compactions WHERE chat_id = ? AND (? IS NULL OR auto = ?)",
        )
        .bind(chat)
        .bind(auto)
        .bind(auto)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// A state whose summarising engine answers, and a chat with auto-compact on and one exchange.
    async fn auto_chat(on: bool) -> (crate::state::AppState, String, i64) {
        let fake = Arc::new(FakeCommandRunner::default());
        let mut state = crate::channels::testkit::state().await;
        state.runner = fake.clone();
        let c = chat(&state.pool).await;
        chats::set_auto_compact(&state.pool, &c, on).await.unwrap();
        let run = exchange(&state.pool, &c, "q", "a").await;
        *fake.canned.lock().unwrap() = Some(answer("HANDOFF"));
        (state, c, run)
    }

    #[tokio::test]
    async fn a_local_turn_over_soft_with_auto_compact_on_files_an_automatic_compaction() {
        let (state, c, run) = auto_chat(true).await;

        super::super::turn::after_local_turn(
            &state,
            &c,
            run,
            Some(super::super::rotation::DEFAULT_SOFT_FILL + 1),
        )
        .await;

        assert_eq!(compaction_rows(&state.pool, &c, Some(1)).await, 1);
    }

    #[tokio::test]
    async fn a_local_turn_with_auto_compact_off_files_no_compaction() {
        let (state, c, run) = auto_chat(false).await;

        super::super::turn::after_local_turn(
            &state,
            &c,
            run,
            Some(super::super::rotation::DEFAULT_SOFT_FILL + 1),
        )
        .await;

        assert_eq!(compaction_rows(&state.pool, &c, None).await, 0);
    }

    #[tokio::test]
    async fn a_local_compaction_turn_triggers_no_further_compaction() {
        let (state, c, _) = auto_chat(true).await;
        let compact_run = compaction(&state.pool, &c, "HANDOFF").await;
        let before = compaction_rows(&state.pool, &c, None).await;

        super::super::turn::after_local_turn(
            &state,
            &c,
            compact_run,
            Some(super::super::rotation::DEFAULT_SOFT_FILL + 1),
        )
        .await;

        assert_eq!(compaction_rows(&state.pool, &c, None).await, before);
    }

    #[tokio::test]
    async fn a_local_turn_without_a_measured_fill_never_compacts() {
        let (state, c, run) = auto_chat(true).await;

        super::super::turn::after_local_turn(&state, &c, run, None).await;

        assert_eq!(compaction_rows(&state.pool, &c, None).await, 0);
    }

    #[test]
    fn in_backoff_holds_for_three_turns_after_a_failed_automatic_compaction() {
        assert!(!in_backoff(None, 0));
        assert!(!in_backoff(None, 5));
        assert!(!in_backoff(Some((1, Status::Done)), 0));
        assert!(!in_backoff(Some((1, Status::Pending)), 0));
        for turns in 0..3 {
            assert!(in_backoff(Some((1, Status::Failed)), turns), "{turns}");
        }
        assert!(!in_backoff(Some((1, Status::Failed)), 3));
        assert!(!in_backoff(Some((1, Status::Failed)), 10));
    }
}
