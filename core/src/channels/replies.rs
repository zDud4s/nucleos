//! Turn cycle and queue notices (flow G, Anexo D D.11). P0 skeleton: A2 owns the `todo!()` bodies.
#![allow(dead_code, unused_variables)] // P0 skeleton: A2 removes this with the real body

use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{SqliteConnection, SqlitePool};

use crate::channels::bus::{Cadence, ChannelsBus};
use crate::channels::outbox::{self, Class, NewAction, Subject};
use crate::channels::routing::advance_turn_watermark;
use crate::channels::strings::{self, Text};
use crate::channels::{contract, registry};
use crate::state::AppState;

/// The ping deadlines, in memory: `(created_at + ping_after, run_id)` of every open channel turn
/// without `pinged_at`, ordered.
#[derive(Default)]
pub struct Pings(std::collections::BTreeMap<(tokio::time::Instant, i64), ()>);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Runs found by `discover` that had no `channel_turn_replies` row.
    pub discovered: u32,
    pub acked: u32,
    pub pinged: u32,
    pub replied: u32,
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The conversation of `chat_id` when it belongs to a registered channel that is live now. Anything
/// else (a shell chat, an old unprefixed Telegram id, a channel nobody is serving) is `None`, so the
/// callers do no I/O for it.
fn live_key(state: &AppState, chat_id: &str) -> Option<contract::ConversationKey> {
    contract::ConversationKey::parse(chat_id)
        .filter(|key| state.channels.presence.is_live(key.channel))
}

fn is_running(status: &str) -> bool {
    status == "running"
}

/// What a finished turn answers: its stdout when it completed, otherwise the status sentence.
pub(crate) fn final_text(run_id: i64, status: &str, stdout: Option<&str>) -> String {
    match stdout.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) if status == "completed" => text.to_owned(),
        _ => strings::render(&Text::TurnEnded {
            turn: run_id,
            status: status.to_owned(),
        }),
    }
}

async fn enqueue_text(
    conn: &mut SqliteConnection,
    key: &contract::ConversationKey,
    run_id: i64,
    text: String,
    now: DateTime<Utc>,
) -> sqlx::Result<i64> {
    outbox::enqueue(
        conn,
        key,
        NewAction::Send {
            reply_to: None,
            parts: vec![contract::OutPart::Text { text }],
            buttons: None,
            silent: false,
        },
        Class::Turn,
        Some(Subject::Run(run_id)),
        now,
    )
    .await
}

/// Registers the turn (`acked_at`) and, only if the row is new, enqueues the "working" ack inside
/// the caller's transaction. `true` when this call inserted the row.
async fn ack_row(
    conn: &mut SqliteConnection,
    key: &contract::ConversationKey,
    run_id: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let inserted = sqlx::query(
        "INSERT INTO channel_turn_replies (run_id, channel, conversation, acked_at) \
         VALUES (?, ?, ?, ?) ON CONFLICT (run_id) DO NOTHING",
    )
    .bind(run_id)
    .bind(key.channel)
    .bind(&key.reference)
    .bind(stamp(now))
    .execute(&mut *conn)
    .await?
    .rows_affected()
        == 1;
    if inserted {
        let text = strings::render(&Text::Working { turn: run_id });
        enqueue_text(conn, key, run_id, text, now).await?;
    }
    Ok(inserted)
}

/// Registers an already finished turn (`acked_at = replied_at = now`) and, only if the row is new,
/// enqueues just the reply: a "working" ack for a finished turn would be noise.
async fn reply_row(
    conn: &mut SqliteConnection,
    key: &contract::ConversationKey,
    run_id: i64,
    text: String,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let inserted = sqlx::query(
        "INSERT INTO channel_turn_replies \
         (run_id, channel, conversation, acked_at, replied_at) \
         VALUES (?, ?, ?, ?, ?) ON CONFLICT (run_id) DO NOTHING",
    )
    .bind(run_id)
    .bind(key.channel)
    .bind(&key.reference)
    .bind(stamp(now))
    .bind(stamp(now))
    .execute(&mut *conn)
    .await?
    .rows_affected()
        == 1;
    if inserted {
        enqueue_text(conn, key, run_id, text, now).await?;
    }
    Ok(inserted)
}

async fn ack_in_transaction(
    state: &AppState,
    key: &contract::ConversationKey,
    run_id: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let mut tx = state.pool.begin().await?;
    let inserted = ack_row(&mut tx, key, run_id, now).await?;
    tx.commit().await?;
    Ok(inserted)
}

/// After the commit: show "typing" and wake the channel's waiter.
fn announce(state: &AppState, key: &contract::ConversationKey) {
    outbox::typing(state, key);
    outbox::wake(state, key.channel);
}

/// Registers the turn and enqueues the "working" ack. A no-op without I/O unless `chat_id` belongs
/// to a registered channel that is live. Never fails the turn: an error is logged and the safety
/// sweep (`discover`) acks it later.
pub async fn turn_started(state: &AppState, run_id: i64, chat_id: &str) {
    let Some(key) = live_key(state, chat_id) else {
        return;
    };
    match ack_in_transaction(state, &key, run_id, Utc::now()).await {
        Ok(true) => announce(state, &key),
        Ok(false) => {}
        Err(error) => {
            tracing::warn!(%error, run_id, chat_id, "channels: could not acknowledge the turn")
        }
    }
}

/// Spawned once from `main.rs` when a channel instance exists. A2.
pub async fn run_replies_loop(state: AppState, bus: ChannelsBus, cadence: Cadence) {
    todo!()
}

impl SweepReport {
    fn add(&mut self, other: SweepReport) {
        self.discovered += other.discovered;
        self.acked += other.acked;
        self.pinged += other.pinged;
        self.replied += other.replied;
    }
}

type FoundRun = (i64, String, String, Option<String>);

/// Finds turns that have no `channel_turn_replies` row, for every live channel that has a routing
/// row (an unseeded channel would otherwise be scanned from run 0 and answer its whole history).
/// `hi` is read BEFORE the range query: reading it afterwards would miss a run inserted between
/// the two reads.
pub async fn discover(state: &AppState, now: DateTime<Utc>) -> sqlx::Result<SweepReport> {
    let mut report = SweepReport::default();
    for manifest in registry::all()
        .iter()
        .filter(|m| state.channels.presence.is_live(m.id))
    {
        let hi: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM runs")
            .fetch_one(&state.pool)
            .await?;
        report.add(discover_channel(state, manifest.id, hi, now).await?);
    }
    Ok(report)
}

/// One channel's slice of the discovery: the runs in `(turn_watermark, hi]` whose `chat_id` starts
/// with `<channel>:` and that are assistant turns (not task runs). The watermark moves to `hi` in
/// the same transaction as the rows it covers.
async fn discover_channel(
    state: &AppState,
    channel: &'static str,
    hi: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<SweepReport> {
    let mut report = SweepReport::default();
    let watermark: Option<i64> =
        sqlx::query_scalar("SELECT turn_watermark FROM channel_routing_state WHERE channel = ?")
            .bind(channel)
            .fetch_optional(&state.pool)
            .await?;
    let Some(watermark) = watermark.filter(|wm| *wm < hi) else {
        return Ok(report);
    };
    // `;` is the character after `:`, so the range is exactly the `<channel>:` prefix and, unlike
    // `LIKE`, it uses the index.
    let runs: Vec<FoundRun> = sqlx::query_as(
        "SELECT id, chat_id, status, stdout FROM runs \
         WHERE id > ? AND id <= ? AND mode = 'assistant' AND COALESCE(origin, '') <> 'task' \
         AND chat_id >= ? AND chat_id < ? \
         AND NOT EXISTS (SELECT 1 FROM channel_turn_replies t WHERE t.run_id = runs.id) \
         ORDER BY id",
    )
    .bind(watermark)
    .bind(hi)
    .bind(format!("{channel}:"))
    .bind(format!("{channel};"))
    .fetch_all(&state.pool)
    .await?;

    let mut tx = state.pool.begin().await?;
    let mut acked: Vec<contract::ConversationKey> = Vec::new();
    let mut answered = false;
    for (run_id, chat_id, status, stdout) in runs {
        let Some(key) = contract::ConversationKey::parse(&chat_id) else {
            continue;
        };
        if is_running(&status) {
            if ack_row(&mut tx, &key, run_id, now).await? {
                report.discovered += 1;
                report.acked += 1;
                acked.push(key);
            }
        } else {
            let text = final_text(run_id, &status, stdout.as_deref());
            if reply_row(&mut tx, &key, run_id, text, now).await? {
                report.discovered += 1;
                report.replied += 1;
                answered = true;
            }
        }
    }
    advance_turn_watermark(&mut tx, channel, hi).await?;
    tx.commit().await?;
    for key in &acked {
        outbox::typing(state, key);
    }
    if answered || !acked.is_empty() {
        outbox::wake(state, channel);
    }
    Ok(report)
}

/// Answers terminal turns and refreshes ping deadlines. A2.
pub async fn open_pass(
    state: &AppState,
    now: DateTime<Utc>,
    cadence: &Cadence,
    pings: &mut Pings,
) -> sqlx::Result<SweepReport> {
    todo!()
}

/// A ping that fired. A2.
pub async fn fire_ping(state: &AppState, run_id: i64, now: DateTime<Utc>) -> sqlx::Result<bool> {
    todo!()
}

/// `discover` + `open_pass`. A2.
pub async fn sweep_once(
    state: &AppState,
    now: DateTime<Utc>,
    cadence: &Cadence,
    pings: &mut Pings,
) -> sqlx::Result<SweepReport> {
    todo!()
}

/// `channel_turn_replies` rows more than 7 days past `replied_at`.
pub async fn prune(_pool: &SqlitePool, _now: DateTime<Utc>) -> sqlx::Result<u64> {
    Ok(0) // P0 stub: real DELETE lands with its chunk
}

/// Posts a system notice and mirrors `queue` notices to the channel. A2.
pub async fn post_notice(
    state: &AppState,
    chat_id: &str,
    kind: &str,
    from_run_id: i64,
    body: &str,
) -> sqlx::Result<i64> {
    todo!()
}

/// "Queue paused (N messages) - write to continue", or `None` when nothing waits. A2.
pub async fn queue_paused_text(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<String>> {
    todo!()
}

/// Announces "queue paused (N)" once per pause. A2.
pub async fn announce_queue_paused(state: &AppState, chat_id: &str) -> sqlx::Result<bool> {
    todo!()
}

/// Clears the pause mark when the owner writes again. Real no-op placeholder that A2 replaces.
pub async fn clear_queue_pause(_pool: &SqlitePool, _chat_id: &str) -> sqlx::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::channels::{routing, testkit};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap()
    }

    /// A state whose `telegram` channel was launched and has a routing row (watermark 0).
    async fn live_state() -> AppState {
        let state = testkit::state().await;
        seed_routing(&state).await;
        state.channels.presence.set_launched("telegram", now());
        state
    }

    // The connection is dropped before the helper returns: the test pool has a single connection.
    async fn seed_routing(state: &AppState) {
        let mut conn = state.pool.acquire().await.unwrap();
        routing::seed(&mut conn, "telegram").await.unwrap();
    }

    async fn insert_run(
        pool: &SqlitePool,
        chat_id: &str,
        status: &str,
        origin: Option<&str>,
        stdout: Option<&str>,
    ) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, origin, stdout, created_at) \
             VALUES ('p', ?, 'assistant', ?, ?, ?, '2026-10-20T00:00:00Z') RETURNING id",
        )
        .bind(status)
        .bind(chat_id)
        .bind(origin)
        .bind(stdout)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn reply_rows(pool: &SqlitePool) -> Vec<(i64, Option<String>, Option<String>)> {
        sqlx::query_as(
            "SELECT run_id, acked_at, replied_at FROM channel_turn_replies ORDER BY run_id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// `(action, class, subject, text)` of every outbox row, oldest first.
    async fn outbox_rows(pool: &SqlitePool) -> Vec<(String, String, Option<String>, String)> {
        let rows: Vec<(String, String, Option<String>, String)> = sqlx::query_as(
            "SELECT action, class, subject, payload FROM channel_outbox ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        rows.into_iter()
            .map(|(action, class, subject, payload)| {
                let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
                let text = payload["parts"][0]["text"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned();
                (action, class, subject, text)
            })
            .collect()
    }

    #[tokio::test]
    async fn turn_started_acks_once_and_enqueues_working_and_typing() {
        let state = live_state().await;
        let run = insert_run(&state.pool, "telegram:77", "running", None, None).await;

        turn_started(&state, run, "telegram:77").await;
        turn_started(&state, run, "telegram:77").await;

        let rows = reply_rows(&state.pool).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, run);
        assert!(rows[0].1.is_some() && rows[0].2.is_none());
        let sent = outbox_rows(&state.pool).await;
        assert_eq!(
            sent,
            vec![(
                "send".to_owned(),
                "turn".to_owned(),
                Some(format!("run:{run}")),
                strings::render(&Text::Working { turn: run }),
            )]
        );
    }

    #[tokio::test]
    async fn turn_started_is_noop_for_non_channel_chat() {
        let state = live_state().await;
        let run = insert_run(&state.pool, "8d3f-shell-chat", "running", None, None).await;
        turn_started(&state, run, "8d3f-shell-chat").await;
        // The Telegram v1 spelling carries no `telegram:` prefix.
        turn_started(&state, run, "-1001234").await;
        assert!(reply_rows(&state.pool).await.is_empty());
        assert!(outbox_rows(&state.pool).await.is_empty());
    }

    #[tokio::test]
    async fn turn_started_is_noop_for_a_channel_that_is_not_live() {
        let state = testkit::state().await;
        seed_routing(&state).await;
        let run = insert_run(&state.pool, "telegram:77", "running", None, None).await;
        turn_started(&state, run, "telegram:77").await;
        assert!(reply_rows(&state.pool).await.is_empty());
        assert!(outbox_rows(&state.pool).await.is_empty());
    }

    #[tokio::test]
    async fn turn_started_acks_when_the_adapter_said_hello_without_launch() {
        let state = testkit::state().await;
        seed_routing(&state).await;
        assert!(state.channels.presence.launched_at("telegram").is_none());
        let hello: contract::Hello = serde_json::from_value(serde_json::json!({
            "kind": registry::by_id("telegram").unwrap().kind,
            "version": "1.0.0",
            "capabilities": { "max_text": 4096 },
        }))
        .unwrap();
        registry::record_hello(&state, "telegram", &hello, now())
            .await
            .unwrap();

        let run = insert_run(&state.pool, "telegram:77", "running", None, None).await;
        turn_started(&state, run, "telegram:77").await;

        assert_eq!(reply_rows(&state.pool).await.len(), 1);
        assert_eq!(outbox_rows(&state.pool).await.len(), 1);
    }

    #[tokio::test]
    async fn discover_running_run_acks_like_turn_started() {
        let state = live_state().await;
        let run = insert_run(&state.pool, "telegram:77", "running", None, None).await;

        let report = discover(&state, now()).await.unwrap();

        assert_eq!(
            report,
            SweepReport {
                discovered: 1,
                acked: 1,
                ..SweepReport::default()
            }
        );
        let rows = reply_rows(&state.pool).await;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].1.is_some() && rows[0].2.is_none());
        let sent = outbox_rows(&state.pool).await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].3, strings::render(&Text::Working { turn: run }));
        assert_eq!(
            routing::turn_watermark(&state.pool, "telegram")
                .await
                .unwrap(),
            run
        );
        // A second pass finds nothing new.
        assert_eq!(
            discover(&state, now()).await.unwrap(),
            SweepReport::default()
        );
    }

    #[tokio::test]
    async fn discover_terminal_run_only_replies() {
        let state = live_state().await;
        let done = insert_run(
            &state.pool,
            "telegram:77",
            "completed",
            None,
            Some("All done."),
        )
        .await;
        let failed = insert_run(&state.pool, "telegram:77", "failed", None, None).await;

        let report = discover(&state, now()).await.unwrap();

        assert_eq!(report.replied, 2);
        assert_eq!(report.acked, 0);
        let rows = reply_rows(&state.pool).await;
        assert!(rows.iter().all(|r| r.1.is_some() && r.2.is_some()));
        let sent = outbox_rows(&state.pool).await;
        assert_eq!(sent.len(), 2, "no working ack for a finished turn");
        assert_eq!(sent[0].2, Some(format!("run:{done}")));
        assert_eq!(sent[0].3, "All done.");
        assert_eq!(sent[1].2, Some(format!("run:{failed}")));
        assert_eq!(
            sent[1].3,
            strings::render(&Text::TurnEnded {
                turn: failed,
                status: "failed".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn discover_reads_max_before_the_range() {
        let state = live_state().await;
        let first = insert_run(&state.pool, "telegram:77", "running", None, None).await;
        // Inserted after `hi` was read: the pass must neither answer it nor walk the mark past it.
        let late = insert_run(&state.pool, "telegram:77", "running", None, None).await;

        let report = discover_channel(&state, "telegram", first, now())
            .await
            .unwrap();

        assert_eq!(report.discovered, 1);
        let rows = reply_rows(&state.pool).await;
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), vec![first]);
        assert_eq!(
            routing::turn_watermark(&state.pool, "telegram")
                .await
                .unwrap(),
            first
        );
        // The next pass reads a newer max and picks the late run up.
        assert_eq!(discover(&state, now()).await.unwrap().discovered, 1);
        assert_eq!(reply_rows(&state.pool).await.len(), 2);
        assert_eq!(
            routing::turn_watermark(&state.pool, "telegram")
                .await
                .unwrap(),
            late
        );
    }

    #[tokio::test]
    async fn discover_leaves_task_runs_other_chats_and_dead_channels_alone() {
        let state = live_state().await;
        insert_run(&state.pool, "telegram:77", "running", Some("task"), None).await;
        insert_run(&state.pool, "8d3f-shell-chat", "running", None, None).await;
        insert_run(&state.pool, "-1001234", "running", None, None).await;

        assert_eq!(
            discover(&state, now()).await.unwrap(),
            SweepReport::default()
        );
        assert!(reply_rows(&state.pool).await.is_empty());

        // A channel that is not live keeps its watermark, so nothing is lost when it comes up.
        let cold = testkit::state().await;
        seed_routing(&cold).await;
        let run = insert_run(&cold.pool, "telegram:77", "running", None, None).await;
        assert_eq!(
            discover(&cold, now()).await.unwrap(),
            SweepReport::default()
        );
        assert_eq!(
            routing::turn_watermark(&cold.pool, "telegram")
                .await
                .unwrap(),
            0
        );
        cold.channels.presence.set_launched("telegram", now());
        assert_eq!(discover(&cold, now()).await.unwrap().discovered, 1);
        assert_eq!(reply_rows(&cold.pool).await[0].0, run);
    }

    #[tokio::test]
    async fn discover_racing_turn_started_writes_one_ack() {
        let state = live_state().await;
        let run = insert_run(&state.pool, "telegram:77", "running", None, None).await;

        tokio::join!(
            turn_started(&state, run, "telegram:77"),
            async {
                discover(&state, now()).await.unwrap();
            },
            turn_started(&state, run, "telegram:77"),
        );

        assert_eq!(reply_rows(&state.pool).await.len(), 1);
        assert_eq!(outbox_rows(&state.pool).await.len(), 1);
    }
}
