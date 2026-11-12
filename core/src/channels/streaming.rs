//! Progress streaming of a running channel turn (the optimisation phase of the channel hub).
//!
//! While a turn runs, its ack message ("Working on it (turn N).") is edited, throttled, with the
//! tail of the text the model has produced so far. When the turn ends the ack is edited once more
//! to "Turn N finished." and the answer goes out as a NEW send, because a new message notifies the
//! phone and an edit does not. Everything here is best-effort: the edits are `Class::Progress`
//! (never retried, never turned into a send, the newest supersedes the pending one).
//!
//! It only happens when the channel declares `edit` and the channel's yaml does not say
//! `stream_edits: false`, and never for a run whose origin is `task`.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;
use tokio::time::Instant;

use crate::channels::contract;
use crate::channels::outbox::{self, Class, NewAction, Subject};
use crate::channels::replies::key_of;
use crate::channels::strings::{self, Text};
use crate::state::AppState;

/// A turn stops getting progress edits this long after it was first seen running.
pub const STREAM_CAP: Duration = Duration::from_secs(10 * 60);

/// Longest tail of the live text offered as a snippet, in chars.
const SNIPPET_CHARS: usize = 600;

/// The last `SNIPPET_CHARS` chars of `text`, cut on a char boundary and trimmed.
pub fn tail_snippet(text: &str) -> String {
    let text = text.trim();
    let skip = text.chars().count().saturating_sub(SNIPPET_CHARS);
    let start = text.char_indices().nth(skip).map_or(0, |(at, _)| at);
    text[start..].trim().to_owned()
}

fn hash_of(snippet: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    snippet.hash(&mut hasher);
    hasher.finish()
}

/// Per running turn: when it was first seen, and what its last edit was.
struct Entry {
    started: Instant,
    last_edit: Option<Instant>,
    last_hash: Option<u64>,
}

/// Decides when a turn may be edited again. Pure: the caller hands in the clock, so the minimum gap
/// and the cap are testable without sleeping.
#[derive(Default)]
pub struct Throttle(BTreeMap<i64, Entry>);

impl Throttle {
    /// Forgets the runs that left `running`.
    fn retain_running(&mut self, running: &[i64]) {
        self.0.retain(|run_id, _| running.contains(run_id));
    }

    /// Whether `run_id` may be edited with a snippet of hash `hash` at `now`: inside the cap, a
    /// changed snippet, and at least `min_gap` after the previous edit.
    fn allows(&mut self, run_id: i64, hash: u64, now: Instant, min_gap: Duration) -> bool {
        let entry = self.0.entry(run_id).or_insert(Entry {
            started: now,
            last_edit: None,
            last_hash: None,
        });
        if now.saturating_duration_since(entry.started) >= STREAM_CAP
            || entry.last_hash == Some(hash)
        {
            return false;
        }
        entry
            .last_edit
            .is_none_or(|at| now.saturating_duration_since(at) >= min_gap)
    }

    /// Notes that an edit with this snippet was enqueued at `now`.
    fn record(&mut self, run_id: i64, hash: u64, now: Instant) {
        if let Some(entry) = self.0.get_mut(&run_id) {
            entry.last_edit = Some(now);
            entry.last_hash = Some(hash);
        }
    }
}

/// Whether the ack of a turn in this conversation may be edited: the channel declares `edit` and
/// its yaml does not turn `stream_edits` off.
pub fn enabled(state: &AppState, key: &contract::ConversationKey) -> bool {
    let declared = state
        .channels
        .presence
        .seen(key.channel)
        .is_some_and(|seen| seen.capabilities.edit);
    declared
        && state
            .channels
            .config(key.channel)
            .is_none_or(|config| config.stream_edits())
}

/// The text the model has produced so far in `run_id`, from the live stream tail.
fn live_text(state: &AppState, run_id: i64) -> Option<String> {
    let buffer = state.run_tails.lock().ok()?.get(&run_id).cloned()?;
    let raw = buffer.lock().ok()?.clone();
    Some(crate::execution::runner::live_from_stream(&raw).text)
}

/// The outbox id of the turn's ack: the oldest `send` about the run in this conversation.
async fn ack_id(
    conn: &mut SqliteConnection,
    key: &contract::ConversationKey,
    run_id: i64,
) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT id FROM channel_outbox WHERE channel = ? AND conversation = ? AND subject = ? \
         AND action = 'send' ORDER BY id LIMIT 1",
    )
    .bind(key.channel)
    .bind(&key.reference)
    .bind(Subject::Run(run_id).to_text())
    .fetch_optional(&mut *conn)
    .await
}

async fn enqueue_ack_edit(
    conn: &mut SqliteConnection,
    key: &contract::ConversationKey,
    run_id: i64,
    ack: i64,
    text: &Text,
    now: DateTime<Utc>,
) -> sqlx::Result<i64> {
    outbox::enqueue(
        conn,
        key,
        NewAction::Edit {
            target: ack,
            parts: vec![contract::OutPart::Text {
                text: strings::render(text),
            }],
            buttons: None,
        },
        Class::Progress,
        Some(Subject::Run(run_id)),
        now,
    )
    .await
}

/// Enqueues one progress edit of the ack, unless the turn was answered meanwhile or has no ack.
/// `true` when an edit was enqueued.
async fn enqueue_progress(
    state: &AppState,
    key: &contract::ConversationKey,
    run_id: i64,
    snippet: String,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let mut tx = state.pool.begin().await?;
    // Re-checked inside the transaction: an edit enqueued after the turn's "finished" edit would
    // overwrite it with a stale snippet.
    let open: Option<i64> = sqlx::query_scalar(
        "SELECT run_id FROM channel_turn_replies WHERE run_id = ? AND replied_at IS NULL",
    )
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await?;
    if open.is_none() {
        return Ok(false);
    }
    let Some(ack) = ack_id(&mut tx, key, run_id).await? else {
        return Ok(false);
    };
    let text = Text::Progress {
        turn: run_id,
        snippet,
    };
    enqueue_ack_edit(&mut tx, key, run_id, ack, &text, now).await?;
    tx.commit().await?;
    Ok(true)
}

/// One streaming tick: for every acknowledged, unanswered, `running` turn that is not a task run
/// and whose channel allows it, enqueues an edit of the ack with the tail of the live text when the
/// throttle permits. A failure is logged; the next tick repeats with newer text.
pub async fn stream_progress(
    state: &AppState,
    throttle: &mut Throttle,
    now: Instant,
    min_gap: Duration,
    wall: DateTime<Utc>,
) {
    let rows: sqlx::Result<Vec<(i64, String, String)>> = sqlx::query_as(
        "SELECT t.run_id, t.channel, t.conversation \
         FROM channel_turn_replies t JOIN runs r ON r.id = t.run_id \
         WHERE t.replied_at IS NULL AND r.status = 'running' \
         AND COALESCE(r.origin, '') <> 'task' ORDER BY t.run_id",
    )
    .fetch_all(&state.pool)
    .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "channels: could not read the running turns to stream");
            return;
        }
    };
    let ids: Vec<i64> = rows.iter().map(|(run_id, _, _)| *run_id).collect();
    throttle.retain_running(&ids);
    for (run_id, channel, conversation) in rows {
        let Some(key) = key_of(&channel, &conversation).filter(|key| enabled(state, key)) else {
            continue;
        };
        let snippet = live_text(state, run_id)
            .map(|text| tail_snippet(&text))
            .unwrap_or_default();
        if snippet.is_empty() {
            continue;
        }
        let hash = hash_of(&snippet);
        if !throttle.allows(run_id, hash, now, min_gap) {
            continue;
        }
        match enqueue_progress(state, &key, run_id, snippet, wall).await {
            Ok(true) => {
                throttle.record(run_id, hash, now);
                outbox::wake(state, key.channel);
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, run_id, "channels: could not enqueue a progress edit")
            }
        }
    }
}

/// At the end of a turn, inside the transaction that enqueues its final reply: drops the progress
/// edits still waiting and, when streaming applies, edits the ack to "Turn N finished.". Both
/// precede the final `send` on the ordered lane. Nothing is enqueued without an ack.
pub async fn finish_progress(
    conn: &mut SqliteConnection,
    state: &AppState,
    key: &contract::ConversationKey,
    run_id: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    outbox::cancel_pending_progress(conn, key, &Subject::Run(run_id), now).await?;
    if !enabled(state, key) {
        return Ok(());
    }
    if let Some(ack) = ack_id(conn, key, run_id).await? {
        let text = Text::Finished { turn: run_id };
        enqueue_ack_edit(conn, key, run_id, ack, &text, now).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::registry::ChannelConfig;
    use crate::channels::{replies, testkit};

    const GAP: Duration = Duration::from_secs(5);

    async fn insert_run(state: &AppState, origin: Option<&str>) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, origin, created_at) \
             VALUES ('p', 'running', 'assistant', 'telegram:77', ?, '2026-10-20T00:00:00Z') \
             RETURNING id",
        )
        .bind(origin)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    /// An acknowledged running turn of `telegram:77`.
    async fn running_turn(state: &AppState) -> i64 {
        let run = insert_run(state, None).await;
        replies::turn_started(state, run, "telegram:77").await;
        run
    }

    fn set_tail(state: &AppState, run_id: i64, text: &str) {
        let line = serde_json::json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": text }] },
        });
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(line.to_string()));
        state.run_tails.lock().unwrap().insert(run_id, buffer);
    }

    /// `(id, action, class, subject, state, fail_reason, payload)` of every outbox row.
    type Row = (
        i64,
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        serde_json::Value,
    );

    type RawRow = (
        i64,
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        String,
    );

    async fn rows(state: &AppState) -> Vec<Row> {
        let rows: Vec<RawRow> = sqlx::query_as(
            "SELECT id, action, class, subject, state, fail_reason, payload \
             FROM channel_outbox ORDER BY id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        rows.into_iter()
            .map(|(id, action, class, subject, st, reason, payload)| {
                let payload = serde_json::from_str(&payload).unwrap();
                (id, action, class, subject, st, reason, payload)
            })
            .collect()
    }

    fn edits(rows: &[Row]) -> Vec<&Row> {
        rows.iter().filter(|row| row.1 == "edit").collect()
    }

    async fn tick(state: &AppState, throttle: &mut Throttle, now: Instant) {
        stream_progress(state, throttle, now, GAP, Utc::now()).await;
    }

    async fn complete(state: &AppState, run_id: i64, stdout: &str) {
        sqlx::query("UPDATE runs SET status = 'completed', stdout = ? WHERE id = ?")
            .bind(stdout)
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        replies::reply_now(state, run_id).await;
    }

    #[test]
    fn the_snippet_is_the_tail_cut_on_a_char_boundary() {
        let text = format!("head {}", "é".repeat(SNIPPET_CHARS));
        let snippet = tail_snippet(&text);
        assert_eq!(snippet.chars().count(), SNIPPET_CHARS);
        assert!(snippet.chars().all(|c| c == 'é'));
        assert_eq!(tail_snippet("  short  "), "short");
        assert_eq!(tail_snippet("   "), "");
    }

    #[tokio::test]
    async fn no_edit_is_enqueued_without_the_edit_capability() {
        let state = testkit::hello_state(true, false).await;
        let run = running_turn(&state).await;
        set_tail(&state, run, "partial answer");
        tick(&state, &mut Throttle::default(), Instant::now()).await;
        assert!(edits(&rows(&state).await).is_empty());
    }

    #[tokio::test]
    async fn edits_are_throttled_to_the_minimum_interval() {
        let state = testkit::hello_state(true, true).await;
        let run = running_turn(&state).await;
        let mut throttle = Throttle::default();
        let t0 = Instant::now();
        set_tail(&state, run, "one");
        tick(&state, &mut throttle, t0).await;
        set_tail(&state, run, "two");
        tick(&state, &mut throttle, t0 + Duration::from_secs(4)).await;
        assert_eq!(edits(&rows(&state).await).len(), 1);
        tick(&state, &mut throttle, t0 + GAP).await;
        assert_eq!(edits(&rows(&state).await).len(), 2);
    }

    #[tokio::test]
    async fn an_unchanged_snippet_enqueues_nothing() {
        let state = testkit::hello_state(true, true).await;
        let run = running_turn(&state).await;
        let mut throttle = Throttle::default();
        let t0 = Instant::now();
        set_tail(&state, run, "same words");
        tick(&state, &mut throttle, t0).await;
        tick(&state, &mut throttle, t0 + Duration::from_secs(30)).await;
        assert_eq!(edits(&rows(&state).await).len(), 1);
    }

    #[tokio::test]
    async fn a_changed_snippet_after_the_interval_edits_the_ack_message() {
        let state = testkit::hello_state(true, true).await;
        let run = running_turn(&state).await;
        let mut throttle = Throttle::default();
        let t0 = Instant::now();
        set_tail(&state, run, "one");
        tick(&state, &mut throttle, t0).await;
        set_tail(&state, run, "two");
        tick(&state, &mut throttle, t0 + GAP).await;

        let all = rows(&state).await;
        let ack = all.iter().find(|row| row.1 == "send").unwrap().0;
        let sent = edits(&all);
        assert_eq!(sent.len(), 2);
        // The first one was superseded by the second, which is the pending one.
        assert_eq!(sent[0].4, "done");
        let last = sent[1];
        assert_eq!(last.2, "progress");
        assert_eq!(last.3.as_deref(), Some(format!("run:{run}").as_str()));
        assert_eq!(last.4, "pending");
        assert_eq!(last.6["target"], ack);
        assert_eq!(
            last.6["parts"][0]["text"],
            format!("Working on it (turn {run}): two")
        );
    }

    #[tokio::test]
    async fn streaming_stops_after_ten_minutes() {
        let state = testkit::hello_state(true, true).await;
        let run = running_turn(&state).await;
        let mut throttle = Throttle::default();
        let t0 = Instant::now();
        set_tail(&state, run, "one");
        tick(&state, &mut throttle, t0).await;
        set_tail(&state, run, "two");
        tick(
            &state,
            &mut throttle,
            t0 + STREAM_CAP - Duration::from_secs(1),
        )
        .await;
        assert_eq!(edits(&rows(&state).await).len(), 2);
        set_tail(&state, run, "three");
        tick(&state, &mut throttle, t0 + STREAM_CAP).await;
        tick(&state, &mut throttle, t0 + STREAM_CAP + GAP).await;
        assert_eq!(edits(&rows(&state).await).len(), 2);
    }

    #[tokio::test]
    async fn the_final_reply_is_a_new_send_and_cancels_the_pending_progress_edit() {
        let state = testkit::hello_state(true, true).await;
        let run = running_turn(&state).await;
        set_tail(&state, run, "halfway");
        tick(&state, &mut Throttle::default(), Instant::now()).await;
        complete(&state, run, "All done.").await;

        let all = rows(&state).await;
        let mentions = |row: &&Row| {
            row.1 == "edit"
                && row.6["parts"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("halfway"))
        };
        let progress = all.iter().find(mentions).unwrap();
        assert_eq!(progress.4, "done");
        assert_eq!(progress.5.as_deref(), Some("cancelled"));
        let last = all.last().unwrap();
        assert_eq!(last.1, "send");
        assert_eq!(last.2, "turn");
        assert_eq!(last.6["parts"][0]["text"], "All done.");
        assert!(last.0 > progress.0);
    }

    #[tokio::test]
    async fn the_ack_is_edited_to_finished_when_the_turn_ends() {
        let state = testkit::hello_state(true, true).await;
        let run = running_turn(&state).await;
        complete(&state, run, "All done.").await;

        let all = rows(&state).await;
        let actions: Vec<&str> = all.iter().map(|row| row.1.as_str()).collect();
        assert_eq!(actions, ["send", "edit", "send"]);
        assert_eq!(all[1].2, "progress");
        assert_eq!(all[1].6["target"], all[0].0);
        assert_eq!(
            all[1].6["parts"][0]["text"],
            format!("Turn {run} finished.")
        );
    }

    #[tokio::test]
    async fn a_disabled_setting_enqueues_nothing() {
        let state = testkit::hello_state(true, true).await;
        state.channels.configs.write().unwrap().insert(
            "telegram",
            ChannelConfig {
                stream_edits: Some(false),
                ..ChannelConfig::default()
            },
        );
        let run = running_turn(&state).await;
        set_tail(&state, run, "partial answer");
        tick(&state, &mut Throttle::default(), Instant::now()).await;
        complete(&state, run, "All done.").await;
        let all = rows(&state).await;
        assert!(edits(&all).is_empty(), "{all:?}");
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn task_origin_runs_are_never_streamed() {
        let state = testkit::hello_state(true, true).await;
        let run = insert_run(&state, Some("task")).await;
        replies::turn_started(&state, run, "telegram:77").await;
        set_tail(&state, run, "partial answer");
        tick(&state, &mut Throttle::default(), Instant::now()).await;
        assert!(edits(&rows(&state).await).is_empty());
    }
}
