//! Turn cycle and queue notices (flow G, Anexo D D.11).
#![allow(dead_code)] // `post_notice`, `announce_queue_paused` and `reply_now` wait for their callers (X10, Rp6)

use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::{SqliteConnection, SqlitePool};
use tokio::time::Instant;

use crate::channels::bus::{Cadence, ChannelsBus};
use crate::channels::outbox::{self, Class, NewAction, Subject};
use crate::channels::routing::advance_turn_watermark;
use crate::channels::strings::{self, Text};
use crate::channels::{contract, registry};
use crate::state::AppState;

/// The ping deadlines, in memory: `(created_at + ping_after, run_id)` of every open channel turn
/// without `pinged_at`, ordered. Rebuilt from the database on every `open_pass`, so a restart or a
/// missed signal loses nothing.
#[derive(Default)]
pub struct Pings(std::collections::BTreeMap<(Instant, i64), ()>);

/// `runs.created_at` as stored: RFC 3339, or SQLite's `YYYY-MM-DD HH:MM:SS`.
fn parse_stored(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .map(|at| at.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|naive| naive.and_utc())
        })
}

impl Pings {
    /// Replaces every deadline with the ones of the unpinged `running` turns. A turn already past its
    /// deadline (or with an unreadable `created_at`) is due immediately.
    fn rebuild(&mut self, running: &[RunningTurn], now: DateTime<Utc>, ping_after: Duration) {
        self.0.clear();
        let base = Instant::now();
        for turn in running.iter().filter(|turn| !turn.pinged) {
            // A `created_at` slightly in the future (clock step) has waited zero, not forever.
            let elapsed = parse_stored(&turn.created_at)
                .map(|created| (now - created).to_std().unwrap_or(Duration::ZERO))
                .unwrap_or(ping_after);
            let remaining = ping_after.saturating_sub(elapsed);
            self.0.insert((base + remaining, turn.run_id), ());
        }
    }

    /// The earliest deadline, if any.
    pub fn first(&self) -> Option<Instant> {
        self.0.keys().next().map(|(at, _)| *at)
    }

    /// Removes and returns the runs whose deadline is at or before `at`, earliest first.
    fn pop_due(&mut self, at: Instant) -> Vec<i64> {
        let mut due = Vec::new();
        while let Some(((deadline, run_id), ())) = self.0.pop_first() {
            if deadline > at {
                self.0.insert((deadline, run_id), ());
                break;
            }
            due.push(run_id);
        }
        due
    }
}

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

/// Resolves when `rx` changed. A closed topic never resolves, so the loop does not spin on it.
async fn runs_changed(rx: &mut tokio::sync::watch::Receiver<u64>) {
    if rx.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// One sweep, its failure only logged: the next signal or safety sweep repeats it.
async fn sweep_logged(state: &AppState, cadence: &Cadence, pings: &mut Pings) {
    if let Err(error) = sweep_once(state, Utc::now(), cadence, pings).await {
        tracing::warn!(%error, "a replies sweep failed");
    }
}

/// Telegram drops the "typing" indicator after about 5 s, so a running turn re-emits it this often.
const TYPING_EVERY: Duration = Duration::from_secs(4);

/// A turn stops getting typing refreshes this long after it was first seen running.
const TYPING_CAP: Duration = Duration::from_secs(15 * 60);

/// Per running turn: when it was first seen, and when its next refresh is due.
struct TypingEntry {
    started: Instant,
    next: Instant,
}

/// Which running turns are due a typing refresh. Pure: the caller hands in the running set and the
/// clock, so the cadence and the cap are testable without sleeping.
#[derive(Default)]
struct TypingRefresh(std::collections::BTreeMap<i64, TypingEntry>);

impl TypingRefresh {
    /// The runs of `running` due a refresh at `now`. A run that left `running` is forgotten (and
    /// starts a fresh cap if it ever comes back); a run past the cap stays tracked but silent.
    fn due(&mut self, running: &[i64], now: Instant) -> Vec<i64> {
        self.0.retain(|run_id, _| running.contains(run_id));
        let mut due = Vec::new();
        for run_id in running {
            let entry = self.0.entry(*run_id).or_insert(TypingEntry {
                started: now,
                next: now,
            });
            if now.saturating_duration_since(entry.started) >= TYPING_CAP || now < entry.next {
                continue;
            }
            entry.next = now + TYPING_EVERY;
            due.push(*run_id);
        }
        due
    }
}

/// Re-emits "typing" for every acknowledged, unanswered turn that is still `running`. The status is
/// read each time, so a turn that finished (or whose reply, and any pause notice after it, went out)
/// stops at once. `outbox::typing` is a no-op for a channel that does not declare the capability.
async fn refresh_typing(state: &AppState, tracker: &mut TypingRefresh, now: Instant) {
    let rows: sqlx::Result<Vec<(i64, String, String)>> = sqlx::query_as(
        "SELECT t.run_id, t.channel, t.conversation \
         FROM channel_turn_replies t JOIN runs r ON r.id = t.run_id \
         WHERE t.replied_at IS NULL AND r.status = 'running' ORDER BY t.run_id",
    )
    .fetch_all(&state.pool)
    .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "channels: could not read the running turns for typing");
            return;
        }
    };
    let ids: Vec<i64> = rows.iter().map(|(run_id, _, _)| *run_id).collect();
    let due = tracker.due(&ids, now);
    for (run_id, channel, conversation) in rows {
        if !due.contains(&run_id) {
            continue;
        }
        if let Some(key) = key_of(&channel, &conversation) {
            outbox::typing(state, &key);
        }
    }
}

/// Spawned once from `main.rs` when a channel instance exists.
///
/// Wakes on a `runs` signal (no settle: a late reply is cheap to repeat and idempotent), on the
/// earliest ping deadline, or on the safety sweep. The sweep's first tick is the startup sweep.
/// Every sweep rebuilds the ping deadlines from the database, so a missed signal loses nothing.
pub async fn run_replies_loop(state: AppState, mut bus: ChannelsBus, cadence: Cadence) {
    let mut pings = Pings::default();
    let mut sweep = tokio::time::interval(cadence.safety_sweep);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut typing_tick = tokio::time::interval(TYPING_EVERY);
    typing_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut typing = TypingRefresh::default();
    loop {
        let next_ping = pings.first();
        // Without a deadline the arm is disabled; the placeholder instant is never awaited.
        let deadline = next_ping.unwrap_or_else(Instant::now);
        tokio::select! {
            _ = sweep.tick() => sweep_logged(&state, &cadence, &mut pings).await,
            _ = runs_changed(&mut bus.runs) => sweep_logged(&state, &cadence, &mut pings).await,
            _ = typing_tick.tick() => refresh_typing(&state, &mut typing, Instant::now()).await,
            _ = tokio::time::sleep_until(deadline), if next_ping.is_some() => {
                for run_id in pings.pop_due(Instant::now()) {
                    if let Err(error) = fire_ping(&state, run_id, Utc::now()).await {
                        tracing::warn!(%error, run_id, "could not send the still-working ping");
                    }
                }
            }
        }
    }
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

/// A channel turn that is acknowledged, still running and not answered: what `open_pass` hands back
/// so the ping deadlines can be rebuilt from it.
#[derive(Debug, PartialEq, Eq)]
pub struct RunningTurn {
    pub run_id: i64,
    pub channel: &'static str,
    /// `runs.created_at`, as stored.
    pub created_at: String,
    /// Whether the "still working" ping already went out.
    pub pinged: bool,
}

/// The outcome of one `open_pass`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct OpenPass {
    pub report: SweepReport,
    pub running: Vec<RunningTurn>,
}

/// `(run_id, channel, conversation, status, stdout, created_at, pinged_at)` of an open turn.
type OpenRow = (
    i64,
    String,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
);

const OPEN_TURNS: &str = "SELECT t.run_id, t.channel, t.conversation, r.status, r.stdout, \
     r.created_at, t.pinged_at \
     FROM channel_turn_replies t JOIN runs r ON r.id = t.run_id \
     WHERE t.replied_at IS NULL";

fn key_of(channel: &str, conversation: &str) -> Option<contract::ConversationKey> {
    registry::by_id(channel).map(|m| contract::ConversationKey::new(m.id, conversation))
}

/// The final reply, once: the compare-and-set on `replied_at` and the enqueue share a transaction,
/// so the send exists if and only if this call won the race. `true` when it did.
async fn reply_once(
    state: &AppState,
    key: &contract::ConversationKey,
    run_id: i64,
    text: String,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let mut tx = state.pool.begin().await?;
    let won = sqlx::query(
        "UPDATE channel_turn_replies SET replied_at = ? WHERE run_id = ? AND replied_at IS NULL",
    )
    .bind(stamp(now))
    .bind(run_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if won {
        enqueue_text(&mut tx, key, run_id, text, now).await?;
    }
    tx.commit().await?;
    if won {
        outbox::wake(state, key.channel);
    }
    Ok(won)
}

/// Answers every acknowledged turn that finished and returns the ones still running. An interrupted
/// turn (the daemon died under it) is terminal too, so it is answered here with its status sentence.
/// Emits `send` only, never `edit` (Q8).
pub async fn open_pass(state: &AppState, now: DateTime<Utc>) -> sqlx::Result<OpenPass> {
    let rows: Vec<OpenRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{OPEN_TURNS} ORDER BY t.run_id"
    )))
    .fetch_all(&state.pool)
    .await?;
    let mut out = OpenPass::default();
    for (run_id, channel, conversation, status, stdout, created_at, pinged_at) in rows {
        let Some(key) = key_of(&channel, &conversation) else {
            continue;
        };
        if is_running(&status) {
            out.running.push(RunningTurn {
                run_id,
                channel: key.channel,
                created_at,
                pinged: pinged_at.is_some(),
            });
            continue;
        }
        let text = final_text(run_id, &status, stdout.as_deref());
        if reply_once(state, &key, run_id, text, now).await? {
            out.report.replied += 1;
        }
    }
    Ok(out)
}

/// Answers `run_id` now if it is finished and not yet answered, by the same compare-and-set as
/// `open_pass`, so calling both is safe. Used to put the final reply ahead of a pause notice. Never
/// fails the caller: an error is logged and the sweep answers the turn later.
pub(crate) async fn reply_now(state: &AppState, run_id: i64) {
    let row: sqlx::Result<Option<OpenRow>> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{OPEN_TURNS} AND t.run_id = ?"
    )))
    .bind(run_id)
    .fetch_optional(&state.pool)
    .await;
    let (_, channel, conversation, status, stdout, _, _) = match row {
        Ok(Some(row)) => row,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%error, run_id, "channels: could not read the turn to answer");
            return;
        }
    };
    let Some(key) = key_of(&channel, &conversation).filter(|_| !is_running(&status)) else {
        return;
    };
    let text = final_text(run_id, &status, stdout.as_deref());
    if let Err(error) = reply_once(state, &key, run_id, text, Utc::now()).await {
        tracing::warn!(%error, run_id, "channels: could not answer the turn");
    }
}

/// Sends the "still working" ping of `run_id`, once. The compare-and-set on `pinged_at` only wins
/// while the turn is unanswered and its run is still `running`, and the enqueue shares its
/// transaction. `true` when this call sent the ping.
pub async fn fire_ping(state: &AppState, run_id: i64, now: DateTime<Utc>) -> sqlx::Result<bool> {
    let mut tx = state.pool.begin().await?;
    let won: Option<(String, String)> = sqlx::query_as(
        "UPDATE channel_turn_replies SET pinged_at = ? \
         WHERE run_id = ? AND pinged_at IS NULL AND replied_at IS NULL \
         AND EXISTS (SELECT 1 FROM runs r \
                     WHERE r.id = channel_turn_replies.run_id AND r.status = 'running') \
         RETURNING channel, conversation",
    )
    .bind(stamp(now))
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((channel, conversation)) = won else {
        return Ok(false);
    };
    // A channel that left the registry: dropping `tx` rolls the mark back.
    let Some(key) = key_of(&channel, &conversation) else {
        return Ok(false);
    };
    enqueue_text(
        &mut tx,
        &key,
        run_id,
        strings::render(&Text::StillWorking),
        now,
    )
    .await?;
    tx.commit().await?;
    outbox::wake(state, key.channel);
    Ok(true)
}

/// One full pass: `discover` the unregistered turns, answer the finished ones, rebuild the ping
/// deadlines from the turns still running and fire the pings already due.
pub async fn sweep_once(
    state: &AppState,
    now: DateTime<Utc>,
    cadence: &Cadence,
    pings: &mut Pings,
) -> sqlx::Result<SweepReport> {
    let mut report = discover(state, now).await?;
    let pass = open_pass(state, now).await?;
    report.add(pass.report);
    pings.rebuild(&pass.running, now, cadence.ping_after);
    for run_id in pings.pop_due(Instant::now()) {
        if fire_ping(state, run_id, now).await? {
            report.pinged += 1;
        }
    }
    Ok(report)
}

/// How long an answered turn keeps its `channel_turn_replies` row.
const REPLIED_TTL_DAYS: i64 = 7;

/// Deletes the rows answered more than 7 days before `now`; unanswered rows stay whatever their
/// age. Returns how many were removed.
pub async fn prune(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<u64> {
    let cutoff = stamp(now - chrono::Duration::days(REPLIED_TTL_DAYS));
    let removed = sqlx::query(
        "DELETE FROM channel_turn_replies WHERE replied_at IS NOT NULL AND replied_at < ?",
    )
    .bind(cutoff)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(removed)
}

/// A free-standing line for the conversation (not tied to a turn), as a `Class::Command` send.
async fn enqueue_notice(
    conn: &mut SqliteConnection,
    key: &contract::ConversationKey,
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
        Class::Command,
        None,
        now,
    )
    .await
}

/// Posts a system notice. Only `kind == "queue"` in a conversation of a live channel is also
/// enqueued for that channel, in the same transaction as the note; everything else is exactly
/// `post_system`.
pub async fn post_notice(
    state: &AppState,
    chat_id: &str,
    kind: &str,
    from_run_id: i64,
    body: &str,
) -> sqlx::Result<i64> {
    let Some(key) = live_key(state, chat_id).filter(|_| kind == "queue") else {
        return crate::chats::notices::post_system(&state.pool, chat_id, kind, from_run_id, body)
            .await;
    };
    let mut tx = state.pool.begin().await?;
    let id =
        crate::chats::notices::post_system_in(&mut tx, chat_id, kind, from_run_id, body).await?;
    enqueue_notice(&mut tx, &key, body.to_owned(), Utc::now()).await?;
    tx.commit().await?;
    outbox::wake(state, key.channel);
    Ok(id)
}

/// "The queue is paused; N message(s) waiting.", or `None` when nothing waits.
pub async fn queue_paused_text(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<String>> {
    let waiting = crate::chats::queued(pool, chat_id).await?.len();
    Ok((waiting > 0).then(|| strings::render(&Text::QueuePaused { waiting })))
}

/// Announces "queue paused (N)" once per pause: the mark and the enqueue share a transaction, and
/// the text is enqueued only if this call inserted the mark. `true` when it announced. Does nothing
/// for a chat outside a live channel or with nothing waiting.
pub async fn announce_queue_paused(state: &AppState, chat_id: &str) -> sqlx::Result<bool> {
    let Some(key) = live_key(state, chat_id) else {
        return Ok(false);
    };
    // Read before the transaction opens: the test pool has one connection.
    let Some(text) = queue_paused_text(&state.pool, chat_id).await? else {
        return Ok(false);
    };
    let now = Utc::now();
    let mut tx = state.pool.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO channel_queue_pause_marks (channel, conversation, announced_at) \
         VALUES (?, ?, ?) ON CONFLICT (channel, conversation) DO NOTHING",
    )
    .bind(key.channel)
    .bind(&key.reference)
    .bind(stamp(now))
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if inserted {
        enqueue_notice(&mut tx, &key, text, now).await?;
    }
    tx.commit().await?;
    if inserted {
        outbox::wake(state, key.channel);
    }
    Ok(inserted)
}

/// Clears the pause mark when the owner writes again, so the next pause is announced again. Never
/// fails the send: an error is logged (a stale mark only costs one missed announcement).
pub async fn clear_queue_pause(state: &AppState, chat_id: &str) {
    let Some(key) = contract::ConversationKey::parse(chat_id) else {
        return;
    };
    if let Err(error) =
        sqlx::query("DELETE FROM channel_queue_pause_marks WHERE channel = ? AND conversation = ?")
            .bind(key.channel)
            .bind(&key.reference)
            .execute(&state.pool)
            .await
    {
        tracing::warn!(%error, chat_id, "channels: could not clear the queue pause mark");
    }
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

    fn loop_cadence(sweep_ms: u64, ping_after_ms: u64) -> Cadence {
        Cadence {
            safety_sweep: Duration::from_millis(sweep_ms),
            settle: Duration::from_millis(5),
            second_look: Duration::from_millis(150),
            budget_min_gap: Duration::ZERO,
            ping_after: Duration::from_millis(ping_after_ms),
        }
    }

    /// The senders stay with the test: a dropped sender closes its topic.
    fn private_bus() -> (Vec<tokio::sync::watch::Sender<u64>>, ChannelsBus) {
        let (tx, rx): (Vec<_>, Vec<_>) = (0..5).map(|_| tokio::sync::watch::channel(0u64)).unzip();
        let mut rx = rx.into_iter();
        let mut next = || rx.next().expect("five receivers");
        let bus = ChannelsBus::from_receivers(next(), next(), next(), next(), next());
        (tx, bus)
    }

    /// Polls until the outbox holds `n` rows (or 3 s pass) and returns what it holds.
    async fn wait_outbox(
        pool: &SqlitePool,
        n: usize,
    ) -> Vec<(String, String, Option<String>, String)> {
        for _ in 0..300 {
            let rows = outbox_rows(pool).await;
            if rows.len() >= n {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        outbox_rows(pool).await
    }

    #[tokio::test]
    async fn run_replies_loop_replies_on_runs_signal() {
        let state = live_state().await;
        let (tx, bus) = private_bus();
        let task = tokio::spawn(run_replies_loop(
            state.clone(),
            bus,
            loop_cadence(60_000, 600_000),
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        insert_run(
            &state.pool,
            "telegram:77",
            "completed",
            None,
            Some("All done."),
        )
        .await;
        tx[4].send_modify(|n| *n += 1);
        let rows = wait_outbox(&state.pool, 1).await;
        task.abort();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].3, "All done.");
    }

    #[tokio::test]
    async fn run_replies_loop_pings_at_the_deadline() {
        let state = live_state().await;
        let run = insert_run(&state.pool, "telegram:77", "running", None, None).await;
        sqlx::query("UPDATE runs SET created_at = ? WHERE id = ?")
            .bind(stamp(Utc::now()))
            .bind(run)
            .execute(&state.pool)
            .await
            .unwrap();
        turn_started(&state, run, "telegram:77").await;
        let (_tx, bus) = private_bus();
        // The deadline (between 0.2 and 1.2 s away) falls well before the 60 s safety sweep.
        let task = tokio::spawn(run_replies_loop(
            state.clone(),
            bus,
            loop_cadence(60_000, 1_200),
        ));
        let rows = wait_outbox(&state.pool, 2).await;
        task.abort();
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[1].3, strings::render(&Text::StillWorking));
    }

    #[tokio::test]
    async fn run_replies_loop_sweeps_without_any_signal() {
        let state = live_state().await;
        let (_tx, bus) = private_bus();
        let task = tokio::spawn(run_replies_loop(
            state.clone(),
            bus,
            loop_cadence(40, 600_000),
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        insert_run(
            &state.pool,
            "telegram:77",
            "completed",
            None,
            Some("Swept."),
        )
        .await;
        let rows = wait_outbox(&state.pool, 1).await;
        task.abort();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].3, "Swept.");
    }

    /// A state whose `telegram` said hello, with or without the `typing` capability.
    async fn hello_state(typing: bool) -> AppState {
        let state = testkit::state().await;
        seed_routing(&state).await;
        let hello: contract::Hello = serde_json::from_value(serde_json::json!({
            "kind": registry::by_id("telegram").unwrap().kind,
            "version": "1.0.0",
            "capabilities": { "max_text": 4096, "typing": typing },
        }))
        .unwrap();
        registry::record_hello(&state, "telegram", &hello, now())
            .await
            .unwrap();
        state
    }

    /// An acknowledged running turn of `telegram:77`, with the ack's typing already drained.
    async fn typing_turn(state: &AppState) -> i64 {
        let run = insert_run(&state.pool, "telegram:77", "running", None, None).await;
        turn_started(state, run, "telegram:77").await;
        state.channels.outbox.take_typing("telegram");
        run
    }

    #[tokio::test]
    async fn typing_is_refreshed_every_four_seconds_while_running() {
        let state = hello_state(true).await;
        typing_turn(&state).await;
        let mut tracker = TypingRefresh::default();
        let t0 = Instant::now();
        let mut emitted = Vec::new();
        for second in 0..=12u64 {
            refresh_typing(&state, &mut tracker, t0 + Duration::from_secs(second)).await;
            if !state.channels.outbox.take_typing("telegram").is_empty() {
                emitted.push(second);
            }
        }
        assert_eq!(emitted, vec![0, 4, 8, 12]);
    }

    #[tokio::test]
    async fn typing_stops_when_the_run_is_terminal() {
        let state = hello_state(true).await;
        let run = typing_turn(&state).await;
        let mut tracker = TypingRefresh::default();
        let t0 = Instant::now();
        refresh_typing(&state, &mut tracker, t0).await;
        assert_eq!(state.channels.outbox.take_typing("telegram"), ["77"]);

        sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
            .bind(run)
            .execute(&state.pool)
            .await
            .unwrap();
        refresh_typing(&state, &mut tracker, t0 + Duration::from_secs(4)).await;
        assert!(state.channels.outbox.take_typing("telegram").is_empty());
    }

    #[tokio::test]
    async fn typing_stops_after_a_pause_notice() {
        let state = hello_state(true).await;
        let run = typing_turn(&state).await;
        let mut tracker = TypingRefresh::default();
        let t0 = Instant::now();
        refresh_typing(&state, &mut tracker, t0).await;
        assert_eq!(state.channels.outbox.take_typing("telegram"), ["77"]);

        // The turn ends, its reply goes out ahead of the notice, then the pause is announced.
        sqlx::query("UPDATE runs SET status = 'completed', stdout = 'Done.' WHERE id = ?")
            .bind(run)
            .execute(&state.pool)
            .await
            .unwrap();
        reply_now(&state, run).await;
        state.channels.outbox.take_typing("telegram");
        refresh_typing(&state, &mut tracker, t0 + Duration::from_secs(4)).await;
        refresh_typing(&state, &mut tracker, t0 + Duration::from_secs(8)).await;
        assert!(state.channels.outbox.take_typing("telegram").is_empty());
    }

    #[tokio::test]
    async fn typing_is_not_refreshed_without_the_capability() {
        let state = hello_state(false).await;
        typing_turn(&state).await;
        let mut tracker = TypingRefresh::default();
        let t0 = Instant::now();
        for second in [0, 4, 8] {
            refresh_typing(&state, &mut tracker, t0 + Duration::from_secs(second)).await;
        }
        assert!(state.channels.outbox.take_typing("telegram").is_empty());
    }

    #[test]
    fn typing_stops_after_the_fifteen_minute_cap() {
        let mut tracker = TypingRefresh::default();
        let t0 = Instant::now();
        assert_eq!(tracker.due(&[1], t0), vec![1]);
        let before_cap = t0 + TYPING_CAP - Duration::from_secs(1);
        assert_eq!(tracker.due(&[1], before_cap), vec![1]);
        assert!(tracker.due(&[1], t0 + TYPING_CAP).is_empty());
        assert!(tracker.due(&[1], t0 + TYPING_CAP + TYPING_EVERY).is_empty());
        // A second run seen late gets its own cap.
        assert_eq!(
            tracker.due(&[1, 2], t0 + TYPING_CAP + TYPING_EVERY),
            vec![2]
        );
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

    /// An acknowledged turn whose run then moved to `status`.
    async fn acked_run(state: &AppState, status: &str, stdout: Option<&str>) -> i64 {
        let run = insert_run(&state.pool, "telegram:77", "running", None, None).await;
        turn_started(state, run, "telegram:77").await;
        sqlx::query("UPDATE runs SET status = ?, stdout = ? WHERE id = ?")
            .bind(status)
            .bind(stdout)
            .bind(run)
            .execute(&state.pool)
            .await
            .unwrap();
        run
    }

    #[tokio::test]
    async fn open_pass_terminal_run_replies_exactly_once() {
        let state = live_state().await;
        let run = acked_run(&state, "completed", Some("All done.")).await;

        let first = open_pass(&state, now()).await.unwrap();
        let second = open_pass(&state, now()).await.unwrap();

        assert_eq!(first.report.replied, 1);
        assert!(first.running.is_empty());
        assert_eq!(second, OpenPass::default());
        let rows = reply_rows(&state.pool).await;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].1.is_some() && rows[0].2.is_some());
        let sent = outbox_rows(&state.pool).await;
        assert_eq!(sent.len(), 2, "the working ack and one final reply");
        assert_eq!(sent[1].2, Some(format!("run:{run}")));
        assert_eq!(sent[1].3, "All done.");
    }

    #[tokio::test]
    async fn open_pass_failed_run_replies_with_turn_ended() {
        let state = live_state().await;
        let mut expected = Vec::new();
        for status in ["failed", "timeout", "cancelled", "interrupted"] {
            let run = acked_run(&state, status, Some("partial output")).await;
            expected.push(strings::render(&Text::TurnEnded {
                turn: run,
                status: status.to_owned(),
            }));
        }

        let pass = open_pass(&state, now()).await.unwrap();

        assert_eq!(pass.report.replied, 4);
        let sent = outbox_rows(&state.pool).await;
        let replies: Vec<&str> = sent.iter().map(|row| row.3.as_str()).collect();
        for text in &expected {
            assert!(replies.contains(&text.as_str()), "missing: {text}");
        }
        assert!(!sent.iter().any(|row| row.3 == "partial output"));
    }

    #[tokio::test]
    async fn open_pass_running_run_is_returned_not_answered() {
        let state = live_state().await;
        let run = insert_run(&state.pool, "telegram:77", "running", None, None).await;
        turn_started(&state, run, "telegram:77").await;

        let pass = open_pass(&state, now()).await.unwrap();

        assert_eq!(pass.report, SweepReport::default());
        assert_eq!(pass.running.len(), 1);
        assert_eq!(pass.running[0].run_id, run);
        assert_eq!(pass.running[0].channel, "telegram");
        assert!(!pass.running[0].pinged);
        let rows = reply_rows(&state.pool).await;
        assert!(rows[0].2.is_none());
        assert_eq!(
            outbox_rows(&state.pool).await.len(),
            1,
            "only the working ack"
        );
    }

    #[tokio::test]
    async fn reply_now_is_idempotent_with_open_pass() {
        let state = live_state().await;
        let run = acked_run(&state, "completed", Some("All done.")).await;

        reply_now(&state, run).await;
        reply_now(&state, run).await;
        let pass = open_pass(&state, now()).await.unwrap();

        assert_eq!(pass.report.replied, 0);
        let sent = outbox_rows(&state.pool).await;
        assert_eq!(sent.len(), 2, "the working ack and one final reply");
        assert_eq!(sent[1].3, "All done.");

        // And the other way round: the loop answers first, `reply_now` finds nothing to do.
        let other = acked_run(&state, "completed", Some("Again.")).await;
        assert_eq!(open_pass(&state, now()).await.unwrap().report.replied, 1);
        reply_now(&state, other).await;
        assert_eq!(outbox_rows(&state.pool).await.len(), 4);
    }

    #[tokio::test]
    async fn open_pass_running_run_refreshes_the_ping_deadline() {
        let state = live_state().await;
        let ping_after = Duration::from_secs(600);
        // Created 4 minutes before `now()`: 6 minutes left of the 10.
        let fresh = insert_run(&state.pool, "telegram:77", "running", None, None).await;
        sqlx::query("UPDATE runs SET created_at = ? WHERE id = ?")
            .bind(stamp(now() - chrono::Duration::minutes(4)))
            .bind(fresh)
            .execute(&state.pool)
            .await
            .unwrap();
        turn_started(&state, fresh, "telegram:77").await;
        // Created long ago: already due.
        let old = insert_run(&state.pool, "telegram:77", "running", None, None).await;
        turn_started(&state, old, "telegram:77").await;
        // Already pinged: no deadline.
        let pinged = insert_run(&state.pool, "telegram:77", "running", None, None).await;
        turn_started(&state, pinged, "telegram:77").await;
        assert!(fire_ping(&state, pinged, now()).await.unwrap());

        let mut pings = Pings::default();
        for _ in 0..2 {
            let pass = open_pass(&state, now()).await.unwrap();
            pings.rebuild(&pass.running, now(), ping_after);
        }

        assert_eq!(
            pings.0.len(),
            2,
            "rebuilt, not accumulated; the pinged turn is out"
        );
        let first = pings.first().unwrap();
        assert!(first <= Instant::now(), "the old turn is due at once");
        let keys: Vec<i64> = pings.0.keys().map(|(_, run)| *run).collect();
        assert_eq!(keys, vec![old, fresh]);
        let (late, _) = pings.0.keys().next_back().unwrap();
        let left = late.saturating_duration_since(Instant::now());
        assert!(
            left > Duration::from_secs(350) && left <= Duration::from_secs(360),
            "{left:?}"
        );
    }

    #[tokio::test]
    async fn fire_ping_only_if_still_running_and_not_pinged() {
        let state = live_state().await;
        let run = insert_run(&state.pool, "telegram:77", "running", None, None).await;
        turn_started(&state, run, "telegram:77").await;

        assert!(fire_ping(&state, run, now()).await.unwrap());
        assert!(!fire_ping(&state, run, now()).await.unwrap(), "not twice");
        let sent = outbox_rows(&state.pool).await;
        assert_eq!(sent.len(), 2, "the working ack and one ping");
        assert_eq!(sent[1].2, Some(format!("run:{run}")));
        assert_eq!(sent[1].3, strings::render(&Text::StillWorking));

        // The run finished meanwhile: no ping, and the mark stays unset.
        let done = acked_run(&state, "completed", Some("Done.")).await;
        assert!(!fire_ping(&state, done, now()).await.unwrap());
        let pinged_at: Option<String> =
            sqlx::query_scalar("SELECT pinged_at FROM channel_turn_replies WHERE run_id = ?")
                .bind(done)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(pinged_at.is_none());

        // Answered already, even though the run still says running: no ping either.
        let answered = insert_run(&state.pool, "telegram:77", "running", None, None).await;
        turn_started(&state, answered, "telegram:77").await;
        sqlx::query("UPDATE channel_turn_replies SET replied_at = ? WHERE run_id = ?")
            .bind(stamp(now()))
            .bind(answered)
            .execute(&state.pool)
            .await
            .unwrap();
        assert!(!fire_ping(&state, answered, now()).await.unwrap());
        // No row at all.
        assert!(!fire_ping(&state, 9999, now()).await.unwrap());
        // Run's ack and ping, plus one ack each for `done` and `answered`.
        assert_eq!(outbox_rows(&state.pool).await.len(), 4);
    }

    #[tokio::test]
    async fn sweep_once_answers_an_interrupted_turn() {
        let state = live_state().await;
        let run = acked_run(&state, "interrupted", None).await;
        let mut pings = Pings::default();

        let report = sweep_once(&state, now(), &Cadence::default(), &mut pings)
            .await
            .unwrap();

        assert_eq!(report.replied, 1);
        assert!(pings.first().is_none());
        let sent = outbox_rows(&state.pool).await;
        assert_eq!(sent.len(), 2, "the working ack and the reply");
        assert_eq!(
            sent[1].3,
            strings::render(&Text::TurnEnded {
                turn: run,
                status: "interrupted".to_owned(),
            })
        );
        let again = sweep_once(&state, now(), &Cadence::default(), &mut pings)
            .await
            .unwrap();
        assert_eq!(again, SweepReport::default());
    }

    /// A row answered at `replied_at` (or never, for `None`), for a fresh run.
    async fn reply_row_at(state: &AppState, replied_at: Option<DateTime<Utc>>) -> i64 {
        let run = insert_run(&state.pool, "telegram:77", "completed", None, None).await;
        sqlx::query(
            "INSERT INTO channel_turn_replies \
             (run_id, channel, conversation, acked_at, replied_at) VALUES (?, 'telegram', '77', ?, ?)",
        )
        .bind(run)
        .bind(stamp(now()))
        .bind(replied_at.map(stamp))
        .execute(&state.pool)
        .await
        .unwrap();
        run
    }

    #[tokio::test]
    async fn prune_removes_replied_older_than_7_days() {
        let state = live_state().await;
        let old = reply_row_at(&state, Some(now() - chrono::Duration::days(8))).await;
        let older = reply_row_at(&state, Some(now() - chrono::Duration::days(30))).await;
        let kept = reply_row_at(&state, Some(now() - chrono::Duration::days(6))).await;

        assert_eq!(prune(&state.pool, now()).await.unwrap(), 2);

        let left: Vec<i64> = reply_rows(&state.pool).await.iter().map(|r| r.0).collect();
        assert_eq!(left, vec![kept]);
        assert!(!left.contains(&old) && !left.contains(&older));
        assert_eq!(prune(&state.pool, now()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn prune_keeps_unreplied_and_recent_rows() {
        let state = live_state().await;
        // Unanswered, however old its ack.
        let open = reply_row_at(&state, None).await;
        sqlx::query("UPDATE channel_turn_replies SET acked_at = ? WHERE run_id = ?")
            .bind(stamp(now() - chrono::Duration::days(60)))
            .bind(open)
            .execute(&state.pool)
            .await
            .unwrap();
        let recent = reply_row_at(&state, Some(now() - chrono::Duration::hours(1))).await;
        let edge = reply_row_at(&state, Some(now() - chrono::Duration::days(7))).await;

        assert_eq!(prune(&state.pool, now()).await.unwrap(), 0);

        let left: Vec<i64> = reply_rows(&state.pool).await.iter().map(|r| r.0).collect();
        assert_eq!(left, vec![open, recent, edge]);
    }

    async fn wait_message(pool: &SqlitePool, chat_id: &str, text: &str) {
        crate::chats::enqueue(pool, chat_id, text, "channel", "[]")
            .await
            .unwrap();
    }

    async fn notice_bodies(pool: &SqlitePool, chat_id: &str) -> Vec<(String, String)> {
        crate::chats::notices::for_chat(pool, chat_id)
            .await
            .unwrap()
            .into_iter()
            .map(|notice| (notice.kind, notice.body))
            .collect()
    }

    async fn mark_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM channel_queue_pause_marks")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn post_notice_mirrors_queue_kind_in_one_transaction() {
        let state = live_state().await;

        let id = post_notice(&state, "telegram:77", "queue", 5, "Not sent: busy.")
            .await
            .unwrap();

        assert!(id > 0);
        assert_eq!(
            notice_bodies(&state.pool, "telegram:77").await,
            vec![("queue".to_owned(), "Not sent: busy.".to_owned())]
        );
        let sent = outbox_rows(&state.pool).await;
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "send");
        assert_eq!(sent[0].1, "command");
        assert_eq!(sent[0].3, "Not sent: busy.");

        // Both writes share one transaction: when the enqueue cannot happen, the note rolls back.
        sqlx::query("DROP TABLE channel_outbox")
            .execute(&state.pool)
            .await
            .unwrap();
        assert!(
            post_notice(&state, "telegram:77", "queue", 6, "again")
                .await
                .is_err()
        );
        assert_eq!(notice_bodies(&state.pool, "telegram:77").await.len(), 1);
    }

    #[tokio::test]
    async fn post_notice_is_post_system_for_other_chats_and_for_a_channel_not_live() {
        let state = live_state().await;
        post_notice(&state, "8d3f-shell-chat", "queue", 1, "shell")
            .await
            .unwrap();
        // Another kind in a live channel chat is a plain note too.
        post_notice(&state, "telegram:77", "restart", 2, "restarted")
            .await
            .unwrap();
        assert_eq!(notice_bodies(&state.pool, "8d3f-shell-chat").await.len(), 1);
        assert_eq!(
            notice_bodies(&state.pool, "telegram:77").await,
            vec![("restart".to_owned(), "restarted".to_owned())]
        );

        let cold = testkit::state().await;
        post_notice(&cold, "telegram:77", "queue", 3, "cold")
            .await
            .unwrap();
        assert_eq!(notice_bodies(&cold.pool, "telegram:77").await.len(), 1);

        assert!(outbox_rows(&state.pool).await.is_empty());
        assert!(outbox_rows(&cold.pool).await.is_empty());
    }

    #[tokio::test]
    async fn announce_queue_paused_once_per_pause() {
        let state = live_state().await;
        wait_message(&state.pool, "telegram:77", "one").await;
        wait_message(&state.pool, "telegram:77", "two").await;

        assert!(announce_queue_paused(&state, "telegram:77").await.unwrap());
        assert!(!announce_queue_paused(&state, "telegram:77").await.unwrap());

        let sent = outbox_rows(&state.pool).await;
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].1, "command");
        assert_eq!(
            sent[0].3,
            strings::render(&Text::QueuePaused { waiting: 2 })
        );
        assert_eq!(mark_count(&state.pool).await, 1);

        // No-ops: a shell chat, and a channel that is not live (nothing written, no mark).
        wait_message(&state.pool, "8d3f-shell-chat", "x").await;
        assert!(
            !announce_queue_paused(&state, "8d3f-shell-chat")
                .await
                .unwrap()
        );
        let cold = testkit::state().await;
        wait_message(&cold.pool, "telegram:77", "x").await;
        assert!(!announce_queue_paused(&cold, "telegram:77").await.unwrap());
        assert!(outbox_rows(&cold.pool).await.is_empty());
        assert_eq!(mark_count(&cold.pool).await, 0);
        assert_eq!(outbox_rows(&state.pool).await.len(), 1);
    }

    #[tokio::test]
    async fn clear_queue_pause_rearms_the_announcement() {
        let state = live_state().await;
        wait_message(&state.pool, "telegram:77", "one").await;
        assert!(announce_queue_paused(&state, "telegram:77").await.unwrap());

        clear_queue_pause(&state, "telegram:77").await;
        assert_eq!(mark_count(&state.pool).await, 0);
        // Clearing a chat that is not a channel's, or has no mark, is harmless.
        clear_queue_pause(&state, "8d3f-shell-chat").await;
        clear_queue_pause(&state, "telegram:77").await;

        assert!(announce_queue_paused(&state, "telegram:77").await.unwrap());
        assert_eq!(outbox_rows(&state.pool).await.len(), 2);
    }

    #[tokio::test]
    async fn queue_paused_text_is_none_when_nothing_waits() {
        let state = live_state().await;
        assert_eq!(
            queue_paused_text(&state.pool, "telegram:77").await.unwrap(),
            None
        );
        // Nothing waits, so nothing is announced and no mark is left behind.
        assert!(!announce_queue_paused(&state, "telegram:77").await.unwrap());
        assert_eq!(mark_count(&state.pool).await, 0);

        wait_message(&state.pool, "telegram:77", "one").await;
        assert_eq!(
            queue_paused_text(&state.pool, "telegram:77").await.unwrap(),
            Some(strings::render(&Text::QueuePaused { waiting: 1 }))
        );
    }
}
