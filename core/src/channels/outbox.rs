//! The outbound queue with leases (flow F, Anexo D D.10) and the adapter routes that read and
//! acknowledge it (RB).

mod ack;
mod degrade;
mod lease;

pub use ack::{AckOutcome, ActionState, RenewError, Renewed, ack, renew};
pub use lease::lease;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use sqlx::{SqliteConnection, SqlitePool};
use tokio::sync::Notify;

use super::ingest::{contract_error, own_channel};
use super::{contract, registry};
use crate::auth::Scope;
use crate::state::AppState;

/// A notification not delivered within this long fails with reason `expired` (section 7.4).
pub const NOTIFICATION_TTL: Duration = Duration::seconds(24 * 60 * 60);
/// An `answer_action` is only useful while the platform still accepts the callback answer.
const ANSWER_ACTION_TTL: Duration = Duration::seconds(60);
/// A progress edit is stale within minutes: undelivered past this it fails `expired` instead of
/// lingering for hours.
const PROGRESS_TTL: Duration = Duration::seconds(2 * 60);
/// Retention of settled rows (`done`/`failed`), and the window of the `failed` count.
const RETENTION: Duration = Duration::seconds(7 * 24 * 60 * 60);

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Why a row ended `failed` (`channel_outbox.fail_reason`). The stored spelling is the snake_case
/// name, except a free adapter reason (`Adapter`), which is stored as sent in the ack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailReason {
    /// Undelivered past `expires_at`.
    Expired,
    /// The sender/conversation was removed before delivery; see `revoke_unallowed`.
    Revoked,
    /// Ten deliveries spent.
    AttemptsExhausted,
    /// The adapter's own non-retryable reason (`thread_not_found`, ...).
    Adapter(String),
}

impl FailReason {
    pub fn to_text(&self) -> String {
        match self {
            Self::Expired => "expired".to_string(),
            Self::Revoked => "revoked".to_string(),
            Self::AttemptsExhausted => "attempts_exhausted".to_string(),
            Self::Adapter(reason) => reason.clone(),
        }
    }

    pub fn from_text(text: &str) -> Self {
        match text {
            "expired" => Self::Expired,
            "revoked" => Self::Revoked,
            "attempts_exhausted" => Self::AttemptsExhausted,
            other => Self::Adapter(other.to_string()),
        }
    }
}

/// Why an action exists: decides its TTL and what health counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Turn ack / ping / answer: never expires by TTL.
    Turn,
    /// Approval request with buttons: never expires by TTL.
    Approval,
    /// Feed, kill, budget: expires after `NOTIFICATION_TTL` undelivered.
    Notification,
    /// Answer to a command or a queue notice.
    Command,
    /// Best-effort progress of a live turn (edits of its progress message): expires after
    /// `PROGRESS_TTL`, is never retried, never becomes a send, and a newer pending progress edit of
    /// the same run supersedes it.
    Progress,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Approval => "approval",
            Self::Notification => "notification",
            Self::Command => "command",
            Self::Progress => "progress",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "turn" => Some(Self::Turn),
            "approval" => Some(Self::Approval),
            "notification" => Some(Self::Notification),
            "command" => Some(Self::Command),
            "progress" => Some(Self::Progress),
            _ => None,
        }
    }
}

/// What a row is about, so a later reply or edit can find it. Stored as `capture:<job>`,
/// `proposal:<id>` or `run:<id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Capture(i64),
    Proposal(i64),
    Run(i64),
}

impl Subject {
    pub fn to_text(&self) -> String {
        match self {
            Self::Capture(id) => format!("capture:{id}"),
            Self::Proposal(id) => format!("proposal:{id}"),
            Self::Run(id) => format!("run:{id}"),
        }
    }

    pub fn from_text(text: &str) -> Option<Self> {
        let (kind, id) = text.split_once(':')?;
        let id: i64 = id.parse().ok()?;
        match kind {
            "capture" => Some(Self::Capture(id)),
            "proposal" => Some(Self::Proposal(id)),
            "run" => Some(Self::Run(id)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum NewAction {
    Send {
        reply_to: Option<i64>,
        parts: Vec<contract::OutPart>,
        buttons: Option<Vec<Vec<contract::Button>>>,
        silent: bool,
    },
    Edit {
        target: i64,
        parts: Vec<contract::OutPart>,
        buttons: Option<Vec<Vec<contract::Button>>>,
    },
    /// Immediate lane, expires in 60 s.
    AnswerAction { action_ref: String, text: String },
}

/// Inserts a `pending` row, due now, inside the caller's transaction (the action and the mark that
/// justifies it fall together). Does not wake the channel: the caller calls `wake` after commit.
pub async fn enqueue(
    conn: &mut SqliteConnection,
    key: &contract::ConversationKey,
    action: NewAction,
    class: Class,
    subject: Option<Subject>,
    now: DateTime<Utc>,
) -> sqlx::Result<i64> {
    let (name, lane, payload, ttl) = match &action {
        NewAction::Send {
            reply_to,
            parts,
            buttons,
            silent,
        } => (
            "send",
            "ordered",
            serde_json::json!({
                "parts": parts,
                "buttons": buttons,
                "silent": silent,
                "reply_to": reply_to,
            }),
            None,
        ),
        NewAction::Edit {
            target,
            parts,
            buttons,
        } => (
            "edit",
            "ordered",
            serde_json::json!({
                "parts": parts,
                "buttons": buttons,
                "target": target,
            }),
            None,
        ),
        NewAction::AnswerAction { action_ref, text } => (
            "answer_action",
            "immediate",
            serde_json::json!({ "action_ref": action_ref, "text": text }),
            Some(ANSWER_ACTION_TTL),
        ),
    };
    let ttl: Option<Duration> = ttl.or(match class {
        Class::Notification => Some(NOTIFICATION_TTL),
        Class::Progress => Some(PROGRESS_TTL),
        _ => None,
    });
    let expires_at: Option<String> = ttl.map(|ttl| stamp(now + ttl));
    let subject: Option<String> = subject.map(|s| s.to_text());
    if class == Class::Progress
        && name == "edit"
        && let Some(subject) = subject.as_deref()
    {
        // Only the newest progress edit of a run is worth delivering.
        settle_pending_progress(conn, key, subject, "superseded", now).await?;
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class, \
         subject, state, due_at, expires_at, attempts, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?, 0, ?) RETURNING id",
    )
    .bind(key.channel)
    .bind(&key.reference)
    .bind(lane)
    .bind(name)
    .bind(payload.to_string())
    .bind(class.as_str())
    .bind(subject)
    .bind(stamp(now))
    .bind(expires_at)
    .bind(stamp(now))
    .fetch_one(&mut *conn)
    .await?;
    Ok(id)
}

/// Settles every still-`pending` (never leased) progress edit of this conversation and subject as
/// `done` without a delivery, annotated with `reason`. Other classes, other subjects and leased
/// rows are left alone. Returns how many rows it settled.
async fn settle_pending_progress(
    conn: &mut SqliteConnection,
    key: &contract::ConversationKey,
    subject: &str,
    reason: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<u64> {
    let settled = sqlx::query(
        "UPDATE channel_outbox SET state = 'done', fail_reason = ?, lease_until = NULL,          settled_at = ? WHERE channel = ? AND conversation = ? AND subject = ?          AND class = 'progress' AND action = 'edit' AND state = 'pending'",
    )
    .bind(reason)
    .bind(stamp(now))
    .bind(key.channel)
    .bind(&key.reference)
    .bind(subject)
    .execute(&mut *conn)
    .await?;
    Ok(settled.rows_affected())
}

/// Drops the pending (not leased) progress edits of a run's progress message, e.g. at turn end,
/// when the final reply makes them moot. Returns how many were dropped.
pub async fn cancel_pending_progress(
    conn: &mut SqliteConnection,
    key: &contract::ConversationKey,
    subject: &Subject,
    now: DateTime<Utc>,
) -> sqlx::Result<u64> {
    settle_pending_progress(conn, key, &subject.to_text(), "cancelled", now).await
}

/// After the caller's commit. Cheap; a no-op for a channel nobody is polling. `lease` arms its
/// `Notified` before it reads the database, so a wake between the read and the sleep is not lost.
pub fn wake(state: &AppState, channel: &str) {
    state.channels.outbox.waiter(channel).notify_waiters();
}

/// Inside the caller's transaction (`access::forget`): every `pending` or `leased` action of
/// `channel` addressed to a conversation that is no longer allowed becomes `failed` with reason
/// `revoked`. Allowed = a remaining `channel_identities.conversation_ref` or one of
/// `project_conversations`. Conservative: the outbox row does not know its parent, so the caller
/// lists the threads still covered by a surviving parent in `project_conversations`.
pub async fn revoke_unallowed(
    conn: &mut SqliteConnection,
    channel: &str,
    project_conversations: &[&str],
    now: DateTime<Utc>,
) -> sqlx::Result<u64> {
    let projects = serde_json::to_string(project_conversations)
        .map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    let revoked = sqlx::query(
        "UPDATE channel_outbox SET state = 'failed', fail_reason = 'revoked', \
         lease_until = NULL, settled_at = ? \
         WHERE channel = ? AND state IN ('pending', 'leased') \
         AND conversation NOT IN \
             (SELECT conversation_ref FROM channel_identities WHERE channel = ?) \
         AND conversation NOT IN (SELECT value FROM json_each(?))",
    )
    .bind(stamp(now))
    .bind(channel)
    .bind(channel)
    .bind(projects)
    .execute(&mut *conn)
    .await?;
    Ok(revoked.rows_affected())
}

/// `typing`: memory only, no id, no ack. Dropped unless the channel declared `typing`.
pub fn typing(state: &AppState, key: &contract::ConversationKey) {
    let declared = state
        .channels
        .presence
        .seen(key.channel)
        .is_some_and(|seen| seen.capabilities.typing);
    if !declared {
        return;
    }
    state
        .channels
        .outbox
        .put_typing(key.channel, &key.reference);
    wake(state, key.channel);
}

/// A row already delivered, found by the platform's own message id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentRef {
    pub id: i64,
    pub subject: Option<Subject>,
}

/// A platform message id seen in an ack, looked up within ONE conversation (Telegram message ids
/// are per chat). Only `done` rows carry refs that count.
pub async fn find_by_platform_ref(
    pool: &SqlitePool,
    key: &contract::ConversationKey,
    platform_ref: &str,
) -> sqlx::Result<Option<SentRef>> {
    let row: Option<(i64, Option<String>)> = sqlx::query_as(
        "SELECT o.id, o.subject FROM channel_outbox o, json_each(o.platform_refs) j \
         WHERE o.channel = ? AND o.conversation = ? AND o.state = 'done' AND j.value = ? \
         ORDER BY o.id DESC LIMIT 1",
    )
    .bind(key.channel)
    .bind(&key.reference)
    .bind(platform_ref)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(id, subject)| SentRef {
        id,
        subject: subject.as_deref().and_then(Subject::from_text),
    }))
}

/// The counts `GET /channels` shows. `pending`/`leased` read the partial index
/// `channel_outbox_due`; `failed` counts rows settled inside the retention window through
/// `channel_outbox_settled`. `oldest_due_at` is the `MIN(due_at)` of the `pending` rows.
pub async fn counts(pool: &SqlitePool, channel: &str) -> sqlx::Result<registry::OutboxCounts> {
    let open: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT state, COUNT(*), MIN(CASE WHEN state = 'pending' THEN due_at END) \
         FROM channel_outbox WHERE channel = ? AND state IN ('pending', 'leased') \
         GROUP BY state",
    )
    .bind(channel)
    .fetch_all(pool)
    .await?;
    let failed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM channel_outbox \
         WHERE channel = ? AND state = 'failed' AND settled_at >= ?",
    )
    .bind(channel)
    .bind(stamp(Utc::now() - RETENTION))
    .fetch_one(pool)
    .await?;
    let mut counts = registry::OutboxCounts {
        failed: failed as u32,
        ..registry::OutboxCounts::default()
    };
    for (state, n, oldest) in open {
        match state.as_str() {
            "pending" => {
                counts.pending = n as u32;
                counts.oldest_due_at = oldest.as_deref().and_then(parse_time);
            }
            "leased" => counts.leased = n as u32,
            _ => {}
        }
    }
    Ok(counts)
}

/// The longest an adapter may park one poll (section 6.1).
const MAX_WAIT_SECS: u64 = 30;

/// Routes of D.10.2, literal paths (no `nest`) so `map::seam` can read them.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/channels/{channel}/outbox", get(get_outbox))
        .route("/channels/{channel}/outbox/{id}/ack", post(post_ack))
        .route("/channels/{channel}/outbox/{id}/renew", post(post_renew))
}

/// The 4xx/5xx bodies that carry only a name (`not_leased`, `not_found`): `{error[, detail]}`.
fn named_error(status: StatusCode, error: &str, detail: Option<&str>) -> Response {
    let body = contract::ErrorResponse {
        error: error.to_string(),
        at: None,
        detail: detail.map(str::to_string),
        contract_version: None,
    };
    (status, Json(body)).into_response()
}

fn not_found() -> Response {
    named_error(StatusCode::NOT_FOUND, "not_found", Some("no such action"))
}

fn database_failure(error: &sqlx::Error, channel: &str, what: &str) -> Response {
    tracing::warn!(%error, channel, "outbox {what} failed");
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

#[derive(serde::Deserialize)]
struct PollQuery {
    /// Seconds to park when nothing is due; absent = 0, above 30 = 30. A value that is not an
    /// unsigned integer is refused by the `Query` extractor with its default 400.
    #[serde(default)]
    wait: Option<u64>,
}

/// `GET /channels/{channel}/outbox?wait=` (section 6.1).
async fn get_outbox(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Path(named): Path<String>,
    Query(query): Query<PollQuery>,
) -> Response {
    let channel = match own_channel(&scope, &named) {
        Ok(channel) => channel,
        Err(refusal) => return refusal,
    };
    let wait = std::time::Duration::from_secs(query.wait.unwrap_or(0).min(MAX_WAIT_SECS));
    match lease(&state, channel, wait, Utc::now()).await {
        Ok(poll) => Json(poll).into_response(),
        Err(error) => database_failure(&error, channel, "poll"),
    }
}

/// The strictly decoded wire ack as the domain type `outbox::ack` takes. `None` only for a body
/// that carries neither `ok` nor `failed`, which `AckShape` has already refused.
fn ack_of(body: contract::AckBody) -> Option<contract::Ack> {
    match (body.ok, body.failed) {
        (Some(ok), None) => Some(contract::Ack::Ok {
            ok,
            platform_refs: body.platform_refs,
        }),
        (None, Some(failed)) => Some(contract::Ack::Failed {
            failed: contract::Failure {
                retryable: failed.retryable,
                retry_after_ms: failed.retry_after_ms,
                reason: failed.reason,
            },
        }),
        _ => None,
    }
}

// A non-numeric `{id}` never reaches the handlers below: the `Path<(String, i64)>` extractor
// refuses it with axum's default 400 (and an id that parses but does not exist is a 404).

/// `POST /channels/{channel}/outbox/{id}/ack` (section 6.2). An `ok` without `platform_refs` is a
/// success like any other (owner decision Q5): the row is `done`, nothing is retried.
async fn post_ack(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Path((named, id)): Path<(String, i64)>,
    body: Bytes,
) -> Response {
    let channel = match own_channel(&scope, &named) {
        Ok(channel) => channel,
        Err(refusal) => return refusal,
    };
    let decoded = match contract::decode::<contract::AckShape>(&body) {
        Ok(decoded) => decoded,
        Err(error) => return contract_error(error),
    };
    let Some(verdict) = ack_of(decoded) else {
        return contract_error(contract::ContractError::new(contract::ErrorCode::OneOf, ""));
    };
    match ack(&state, channel, id, verdict, Utc::now()).await {
        Ok(Some(outcome)) => Json(outcome).into_response(),
        Ok(None) => not_found(),
        Err(error) => database_failure(&error, channel, "ack"),
    }
}

/// `POST /channels/{channel}/outbox/{id}/renew` (section 6.1). The body, if any, is ignored.
async fn post_renew(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Path((named, id)): Path<(String, i64)>,
) -> Response {
    let channel = match own_channel(&scope, &named) {
        Ok(channel) => channel,
        Err(refusal) => return refusal,
    };
    match renew(&state.pool, channel, id, Utc::now()).await {
        Ok(renewed) => Json(contract::RenewAnswer {
            // Second precision, like the stored value and the contract's `date-time`.
            lease_until: stamp(renewed.lease_until),
        })
        .into_response(),
        Err(RenewError::NotLeased) => named_error(StatusCode::CONFLICT, "not_leased", None),
        Err(RenewError::NotFound) => not_found(),
        Err(RenewError::Db(error)) => database_failure(&error, channel, "renew"),
    }
}

/// Per-channel long-poll wakers and the ephemeral set (`typing`, memory only).
pub struct Waiters {
    wakers: HashMap<&'static str, Arc<Notify>>,
    typing: Mutex<HashSet<(String, String)>>,
}

impl Default for Waiters {
    fn default() -> Self {
        Self {
            wakers: registry::all()
                .iter()
                .map(|m| (m.id, Arc::new(Notify::new())))
                .collect(),
            typing: Mutex::default(),
        }
    }
}

impl Waiters {
    /// The channel's waker. A channel outside the manifest table gets a detached `Notify` that
    /// nothing ever wakes.
    pub fn waiter(&self, channel: &str) -> Arc<Notify> {
        self.wakers.get(channel).cloned().unwrap_or_default()
    }

    fn put_typing(&self, channel: &str, conversation: &str) {
        self.typing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((channel.to_string(), conversation.to_string()));
    }

    /// Removes and returns the conversations the channel should show as typing.
    pub fn take_typing(&self, channel: &str) -> Vec<String> {
        let mut set = self.typing.lock().unwrap_or_else(|e| e.into_inner());
        let taken: Vec<String> = set
            .iter()
            .filter(|(c, _)| c == channel)
            .map(|(_, conv)| conv.clone())
            .collect();
        set.retain(|(c, _)| c != channel);
        taken
    }
}

/// `hello` (section 5.1): every `leased` row of the channel goes back to `pending`. `attempts`
/// is left alone: it counts deliveries and was already incremented when the row was leased.
pub async fn release_all(pool: &SqlitePool, channel: &str) -> sqlx::Result<u64> {
    let released = sqlx::query(
        "UPDATE channel_outbox SET state = 'pending', lease_until = NULL \
         WHERE channel = ? AND state = 'leased'",
    )
    .bind(channel)
    .execute(pool)
    .await?;
    Ok(released.rows_affected())
}

/// done/failed rows settled more than 7 days ago. Open rows are never touched.
pub async fn prune(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<u64> {
    let pruned = sqlx::query(
        "DELETE FROM channel_outbox WHERE state IN ('done', 'failed') AND settled_at < ?",
    )
    .bind(stamp(now - RETENTION))
    .execute(pool)
    .await?;
    Ok(pruned.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn insert(pool: &SqlitePool, channel: &str, state: &str, attempts: i64) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class, \
             state, due_at, lease_until, attempts, created_at) \
             VALUES (?, 'conv', 'ordered', 'send', '{}', 'turn', ?, 't', \
             CASE WHEN ? = 'leased' THEN 'later' END, ?, 't') RETURNING id",
        )
        .bind(channel)
        .bind(state)
        .bind(state)
        .bind(attempts)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn row(pool: &SqlitePool, id: i64) -> (String, Option<String>, i64) {
        sqlx::query_as("SELECT state, lease_until, attempts FROM channel_outbox WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn release_all_returns_leased_rows_to_pending_without_touching_attempts() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let leased = insert(pool, "a", "leased", 2).await;
        let pending = insert(pool, "a", "pending", 1).await;
        let done = insert(pool, "a", "done", 3).await;
        let other_channel = insert(pool, "b", "leased", 1).await;

        assert_eq!(release_all(pool, "a").await.unwrap(), 1);

        assert_eq!(row(pool, leased).await, ("pending".to_string(), None, 2));
        assert_eq!(row(pool, pending).await, ("pending".to_string(), None, 1));
        assert_eq!(row(pool, done).await, ("done".to_string(), None, 3));
        let (st, lease, attempts) = row(pool, other_channel).await;
        assert_eq!((st.as_str(), attempts), ("leased", 1));
        assert!(lease.is_some());
    }

    #[tokio::test]
    async fn release_all_on_an_empty_queue_releases_nothing() {
        let state = crate::channels::testkit::state().await;
        assert_eq!(release_all(&state.pool, "a").await.unwrap(), 0);
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    fn key(reference: &str) -> contract::ConversationKey {
        contract::ConversationKey::new("fake", reference)
    }

    fn text_part(text: &str) -> contract::OutPart {
        contract::OutPart::Text {
            text: text.to_string(),
        }
    }

    fn plain_send() -> NewAction {
        NewAction::Send {
            reply_to: None,
            parts: vec![text_part("hi")],
            buttons: None,
            silent: false,
        }
    }

    /// Everything a test wants to set on a hand-made row.
    #[allow(clippy::too_many_arguments)]
    async fn insert_row(
        pool: &SqlitePool,
        channel: &str,
        conversation: &str,
        state: &str,
        due_at: &str,
        settled_at: Option<&str>,
        platform_refs: Option<&str>,
        subject: Option<&str>,
    ) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class, \
             subject, state, due_at, lease_until, platform_refs, attempts, created_at, \
             settled_at) \
             VALUES (?, ?, 'ordered', 'send', '{}', 'turn', ?, ?, ?, \
             CASE WHEN ? = 'leased' THEN 'later' END, ?, 0, 't', ?) RETURNING id",
        )
        .bind(channel)
        .bind(conversation)
        .bind(subject)
        .bind(state)
        .bind(due_at)
        .bind(state)
        .bind(platform_refs)
        .bind(settled_at)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[allow(clippy::type_complexity)]
    async fn stored(
        pool: &SqlitePool,
        id: i64,
    ) -> (
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        String,
        Option<String>,
        String,
    ) {
        sqlx::query_as(
            "SELECT conversation, lane, action, class, subject, state, due_at, expires_at, \
             payload FROM channel_outbox WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn enqueue_sets_lane_ttl_and_payload() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let now = at(0);
        let buttons = vec![vec![contract::Button {
            label: "Yes".to_string(),
            token: "tk_1".to_string(),
        }]];
        let mut conn = pool.acquire().await.unwrap();
        let id = enqueue(
            &mut conn,
            &key("c1"),
            NewAction::Send {
                reply_to: Some(7),
                parts: vec![text_part("hi")],
                buttons: Some(buttons),
                silent: true,
            },
            Class::Notification,
            Some(Subject::Proposal(12)),
            now,
        )
        .await
        .unwrap();
        let plain = enqueue(&mut conn, &key("c1"), plain_send(), Class::Turn, None, now)
            .await
            .unwrap();
        drop(conn);

        let (conversation, lane, action, class, subject, st, due_at, expires_at, payload) =
            stored(pool, id).await;
        assert_eq!(conversation, "c1");
        assert_eq!((lane.as_str(), action.as_str()), ("ordered", "send"));
        assert_eq!(class, "notification");
        assert_eq!(subject.as_deref(), Some("proposal:12"));
        assert_eq!(st, "pending");
        assert_eq!(due_at, stamp(now));
        assert_eq!(expires_at, Some(stamp(now + NOTIFICATION_TTL)));
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(
            payload,
            serde_json::json!({
                "parts": [{"type": "text", "text": "hi"}],
                "buttons": [[{"label": "Yes", "token": "tk_1"}]],
                "silent": true,
                "reply_to": 7,
            })
        );

        let (_, _, _, class, subject, _, _, expires_at, _) = stored(pool, plain).await;
        assert_eq!(class, "turn");
        assert_eq!(subject, None);
        assert_eq!(expires_at, None);
    }

    fn progress_edit() -> NewAction {
        NewAction::Edit {
            target: 1,
            parts: vec![text_part("working")],
            buttons: None,
        }
    }

    async fn progress(conn: &mut SqliteConnection, conversation: &str, run: i64, secs: i64) -> i64 {
        enqueue(
            conn,
            &key(conversation),
            progress_edit(),
            Class::Progress,
            Some(Subject::Run(run)),
            at(secs),
        )
        .await
        .unwrap()
    }

    async fn state_and_reason(pool: &SqlitePool, id: i64) -> (String, Option<String>) {
        sqlx::query_as("SELECT state, fail_reason FROM channel_outbox WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_new_progress_edit_supersedes_the_pending_one() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let mut conn = pool.acquire().await.unwrap();
        let first = progress(&mut conn, "c1", 7, 0).await;
        let second = progress(&mut conn, "c1", 7, 1).await;
        let third = progress(&mut conn, "c1", 7, 2).await;
        drop(conn);

        for old in [first, second] {
            let (st, reason) = state_and_reason(pool, old).await;
            assert_eq!(st, "done");
            assert_eq!(reason.as_deref(), Some("superseded"));
        }
        assert_eq!(state_and_reason(pool, third).await.0, "pending");
        let (class, expires): (String, Option<String>) =
            sqlx::query_as("SELECT class, expires_at FROM channel_outbox WHERE id = ?")
                .bind(third)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(class, "progress");
        assert_eq!(expires, Some(stamp(at(2) + PROGRESS_TTL)));
    }

    #[tokio::test]
    async fn a_leased_progress_edit_is_not_superseded() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let mut conn = pool.acquire().await.unwrap();
        let first = progress(&mut conn, "c1", 7, 0).await;
        sqlx::query(
            "UPDATE channel_outbox SET state = 'leased', lease_until = 'later' WHERE id = ?",
        )
        .bind(first)
        .execute(&mut *conn)
        .await
        .unwrap();
        let second = progress(&mut conn, "c1", 7, 1).await;
        drop(conn);

        assert_eq!(state_and_reason(pool, first).await.0, "leased");
        assert_eq!(state_and_reason(pool, second).await.0, "pending");
    }

    #[tokio::test]
    async fn superseding_cancels_only_unleased_rows_of_the_same_run() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let mut conn = pool.acquire().await.unwrap();
        let other_run = progress(&mut conn, "c1", 8, 0).await;
        let other_conversation = progress(&mut conn, "c2", 7, 0).await;
        let turn_edit = enqueue(
            &mut conn,
            &key("c1"),
            progress_edit(),
            Class::Turn,
            Some(Subject::Run(7)),
            at(0),
        )
        .await
        .unwrap();
        let progress_send = enqueue(
            &mut conn,
            &key("c1"),
            plain_send(),
            Class::Progress,
            Some(Subject::Run(7)),
            at(0),
        )
        .await
        .unwrap();
        let newest = progress(&mut conn, "c1", 7, 1).await;
        drop(conn);

        for untouched in [
            other_run,
            other_conversation,
            turn_edit,
            progress_send,
            newest,
        ] {
            assert_eq!(state_and_reason(pool, untouched).await.0, "pending");
        }
    }

    #[tokio::test]
    async fn cancel_pending_progress_drops_only_the_pending_progress_edits_of_the_run() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let mut conn = pool.acquire().await.unwrap();
        let pending = progress(&mut conn, "c1", 7, 0).await;
        let leased = progress(&mut conn, "c1", 8, 0).await;
        sqlx::query(
            "UPDATE channel_outbox SET state = 'leased', lease_until = 'later' WHERE id = ?",
        )
        .bind(leased)
        .execute(&mut *conn)
        .await
        .unwrap();
        let leased_same_run = progress(&mut conn, "c1", 7, 1).await;
        sqlx::query(
            "UPDATE channel_outbox SET state = 'leased', lease_until = 'later' WHERE id = ?",
        )
        .bind(leased_same_run)
        .execute(&mut *conn)
        .await
        .unwrap();
        let waiting = progress(&mut conn, "c1", 7, 2).await;
        let other_run = progress(&mut conn, "c1", 9, 2).await;
        let reply = enqueue(
            &mut conn,
            &key("c1"),
            plain_send(),
            Class::Turn,
            None,
            at(2),
        )
        .await
        .unwrap();

        let dropped = cancel_pending_progress(&mut conn, &key("c1"), &Subject::Run(7), at(3))
            .await
            .unwrap();
        drop(conn);

        assert_eq!(dropped, 1);
        let (st, reason) = state_and_reason(pool, waiting).await;
        assert_eq!(
            (st.as_str(), reason.as_deref()),
            ("done", Some("cancelled"))
        );
        assert_eq!(
            state_and_reason(pool, pending).await.1.as_deref(),
            Some("superseded")
        );
        for untouched in [leased, leased_same_run] {
            assert_eq!(state_and_reason(pool, untouched).await.0, "leased");
        }
        for untouched in [other_run, reply] {
            assert_eq!(state_and_reason(pool, untouched).await.0, "pending");
        }
    }

    #[tokio::test]
    async fn answer_action_is_immediate_and_expires_in_a_minute() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let now = at(0);
        let mut conn = pool.acquire().await.unwrap();
        let id = enqueue(
            &mut conn,
            &key("c1"),
            NewAction::AnswerAction {
                action_ref: "cb-9".to_string(),
                text: "Done".to_string(),
            },
            Class::Command,
            None,
            now,
        )
        .await
        .unwrap();
        drop(conn);

        let (_, lane, action, _, _, _, _, expires_at, payload) = stored(pool, id).await;
        assert_eq!(
            (lane.as_str(), action.as_str()),
            ("immediate", "answer_action")
        );
        assert_eq!(expires_at, Some(stamp(now + Duration::seconds(60))));
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(
            payload,
            serde_json::json!({"action_ref": "cb-9", "text": "Done"})
        );
    }

    #[tokio::test]
    async fn enqueue_rolls_back_with_the_callers_transaction() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let mut tx = pool.begin().await.unwrap();
        enqueue(&mut tx, &key("c1"), plain_send(), Class::Turn, None, at(0))
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channel_outbox")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn revoke_unallowed_fails_only_the_removed_conversations() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        sqlx::query(
            "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
             conversation_kind, created_at) VALUES ('fake', '42', 'keep', 'private', 't')",
        )
        .execute(pool)
        .await
        .unwrap();
        let keep = insert_row(pool, "fake", "keep", "pending", "t", None, None, None).await;
        let gone = insert_row(pool, "fake", "gone", "pending", "t", None, None, None).await;
        let gone_leased = insert_row(pool, "fake", "gone", "leased", "t", None, None, None).await;
        let project = insert_row(pool, "fake", "proj", "pending", "t", None, None, None).await;
        let gone_done = insert_row(pool, "fake", "gone", "done", "t", Some("s"), None, None).await;
        let other = insert_row(pool, "telegram", "gone", "pending", "t", None, None, None).await;

        let now = at(5);
        let mut conn = pool.acquire().await.unwrap();
        let revoked = revoke_unallowed(&mut conn, "fake", &["proj"], now)
            .await
            .unwrap();
        drop(conn);
        assert_eq!(revoked, 2);

        for id in [gone, gone_leased] {
            let (st, reason, settled, lease): (
                String,
                Option<String>,
                Option<String>,
                Option<String>,
            ) = sqlx::query_as(
                "SELECT state, fail_reason, settled_at, lease_until FROM channel_outbox \
                     WHERE id = ?",
            )
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
            assert_eq!(st, "failed");
            assert_eq!(reason.as_deref(), Some("revoked"));
            assert_eq!(settled, Some(stamp(now)));
            assert_eq!(lease, None);
        }
        for (id, expected) in [
            (keep, "pending"),
            (project, "pending"),
            (gone_done, "done"),
            (other, "pending"),
        ] {
            assert_eq!(row(pool, id).await.0, expected);
        }
    }

    #[tokio::test]
    async fn find_by_platform_ref_is_scoped_to_the_conversation() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let in_a = insert_row(
            pool,
            "fake",
            "a",
            "done",
            "t",
            Some("s"),
            Some(r#"["p1","p2"]"#),
            Some("capture:5"),
        )
        .await;
        let in_b = insert_row(
            pool,
            "fake",
            "b",
            "done",
            "t",
            Some("s"),
            Some(r#"["p1"]"#),
            Some("proposal:9"),
        )
        .await;
        insert_row(
            pool,
            "fake",
            "a",
            "pending",
            "t",
            None,
            Some(r#"["p3"]"#),
            None,
        )
        .await;

        assert_eq!(
            find_by_platform_ref(pool, &key("a"), "p2").await.unwrap(),
            Some(SentRef {
                id: in_a,
                subject: Some(Subject::Capture(5)),
            })
        );
        assert_eq!(
            find_by_platform_ref(pool, &key("b"), "p1").await.unwrap(),
            Some(SentRef {
                id: in_b,
                subject: Some(Subject::Proposal(9)),
            })
        );
        assert_eq!(
            find_by_platform_ref(pool, &key("b"), "p2").await.unwrap(),
            None
        );
        assert_eq!(
            find_by_platform_ref(pool, &key("a"), "p3").await.unwrap(),
            None
        );
        assert_eq!(
            find_by_platform_ref(pool, &key("c"), "p1").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn counts_match_the_rows_and_report_oldest_due_at() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let empty = counts(pool, "fake").await.unwrap();
        assert_eq!((empty.pending, empty.leased, empty.failed), (0, 0, 0));
        assert_eq!(empty.oldest_due_at, None);

        let recent = stamp(Utc::now() - Duration::days(1));
        let old = stamp(Utc::now() - Duration::days(8));
        let nine = "2026-10-19T09:00:00Z";
        let eight = "2026-10-19T08:00:00Z";
        insert_row(pool, "fake", "c", "pending", nine, None, None, None).await;
        insert_row(pool, "fake", "c", "pending", eight, None, None, None).await;
        insert_row(
            pool,
            "fake",
            "c",
            "leased",
            "2026-10-19T07:00:00Z",
            None,
            None,
            None,
        )
        .await;
        insert_row(pool, "fake", "c", "failed", "t", Some(&recent), None, None).await;
        insert_row(pool, "fake", "c", "failed", "t", Some(&old), None, None).await;
        insert_row(pool, "fake", "c", "done", "t", Some(&recent), None, None).await;
        insert_row(
            pool,
            "telegram",
            "c",
            "pending",
            "2026-10-19T01:00:00Z",
            None,
            None,
            None,
        )
        .await;

        let got = counts(pool, "fake").await.unwrap();
        assert_eq!((got.pending, got.leased, got.failed), (2, 1, 1));
        assert_eq!(got.oldest_due_at, parse_time(eight));
    }

    #[tokio::test]
    async fn prune_keeps_recent_and_open_rows() {
        let state = crate::channels::testkit::state().await;
        let pool = &state.pool;
        let now = at(0);
        let eight_days = stamp(now - Duration::days(8));
        let six_days = stamp(now - Duration::days(6));
        insert_row(
            pool,
            "fake",
            "c",
            "done",
            "t",
            Some(&eight_days),
            None,
            None,
        )
        .await;
        insert_row(
            pool,
            "fake",
            "c",
            "failed",
            "t",
            Some(&eight_days),
            None,
            None,
        )
        .await;
        let recent_done =
            insert_row(pool, "fake", "c", "done", "t", Some(&six_days), None, None).await;
        let pending = insert_row(pool, "fake", "c", "pending", "t", None, None, None).await;
        let leased = insert_row(pool, "fake", "c", "leased", "t", None, None, None).await;

        assert_eq!(prune(pool, now).await.unwrap(), 2);

        let left: Vec<i64> = sqlx::query_scalar("SELECT id FROM channel_outbox ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
        assert_eq!(left, vec![recent_done, pending, leased]);
    }

    fn hello(typing: bool) -> contract::Hello {
        contract::Hello {
            kind: "fake".to_string(),
            version: "1.0.0".to_string(),
            capabilities: contract::Capabilities {
                typing,
                ..contract::Capabilities::default()
            },
        }
    }

    #[tokio::test]
    async fn typing_is_dropped_without_the_capability_and_deduplicated() {
        // Never said hello: nothing declared.
        let state = crate::channels::testkit::state().await;
        typing(&state, &key("c1"));
        assert!(state.channels.outbox.take_typing("fake").is_empty());

        // Said hello without `typing`.
        registry::record_hello(&state, "fake", &hello(false), at(0))
            .await
            .unwrap();
        typing(&state, &key("c1"));
        assert!(state.channels.outbox.take_typing("fake").is_empty());

        // Declared: repeated calls collapse into one entry, drained per channel.
        let state = crate::channels::testkit::state().await;
        registry::record_hello(&state, "fake", &hello(true), at(0))
            .await
            .unwrap();
        typing(&state, &key("c1"));
        typing(&state, &key("c1"));
        typing(&state, &key("c2"));
        let mut taken = state.channels.outbox.take_typing("fake");
        taken.sort();
        assert_eq!(taken, vec!["c1".to_string(), "c2".to_string()]);
        assert!(state.channels.outbox.take_typing("fake").is_empty());
    }

    #[tokio::test]
    async fn wake_reaches_an_armed_waiter() {
        let state = crate::channels::testkit::state().await;
        let notify = state.channels.outbox.waiter("fake");
        let mut notified = std::pin::pin!(notify.notified());
        notified.as_mut().enable();
        wake(&state, "fake");
        let woke = tokio::time::timeout(std::time::Duration::from_secs(5), notified).await;
        assert!(woke.is_ok());
    }

    // ---- the routes (RB) ------------------------------------------------------------------

    /// The channel routes behind the real token middleware.
    fn app(st: &AppState) -> Router {
        crate::channels::routes()
            .layer(axum::middleware::from_fn_with_state(
                st.clone(),
                crate::auth::require_token,
            ))
            .with_state(st.clone())
    }

    async fn fake_key(st: &AppState) -> String {
        crate::auth::mint_service_token(&st.pool, crate::auth::Service::Channel("fake"))
            .await
            .unwrap()
    }

    async fn call(
        st: &AppState,
        key: &str,
        method: &str,
        path: &str,
        body: &str,
    ) -> (StatusCode, serde_json::Value) {
        use tower::ServiceExt;
        let request = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("Authorization", format!("Bearer {key}"))
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        let response = app(st).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    /// Queues a plain `send` for `channel` and returns its id.
    async fn queue(st: &AppState, channel: &'static str, conversation: &str) -> i64 {
        let mut conn = st.pool.acquire().await.unwrap();
        enqueue(
            &mut conn,
            &contract::ConversationKey::new(channel, conversation),
            plain_send(),
            Class::Turn,
            None,
            Utc::now(),
        )
        .await
        .unwrap()
    }

    /// Queues a `send` for `fake` and leases it through the route, as an adapter would.
    async fn leased(st: &AppState, key: &str) -> i64 {
        let id = queue(st, "fake", "c1").await;
        let (status, body) = call(st, key, "GET", "/channels/fake/outbox", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["actions"][0]["id"], id);
        id
    }

    async fn state_of(pool: &SqlitePool, id: i64) -> (String, Option<String>) {
        sqlx::query_as("SELECT state, platform_refs FROM channel_outbox WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn route_get_outbox_returns_actions_and_ephemeral_shapes_validating_against_the_contract()
    {
        let st = crate::channels::testkit::state().await;
        let token = fake_key(&st).await;
        registry::record_hello(&st, "fake", &hello(true), Utc::now())
            .await
            .unwrap();
        let id = queue(&st, "fake", "c1").await;
        typing(&st, &key("c2"));

        let (status, body) = call(&st, &token, "GET", "/channels/fake/outbox?wait=0", "").await;

        assert_eq!(status, StatusCode::OK);
        let bytes = serde_json::to_vec(&body).unwrap();
        contract::decode::<contract::OutboxResponseShape>(&bytes).expect("outbox_response shape");
        assert_eq!(body["actions"].as_array().unwrap().len(), 1);
        assert_eq!(body["actions"][0]["id"], id);
        assert_eq!(body["actions"][0]["type"], "send");
        assert_eq!(
            body["ephemeral"],
            serde_json::json!([{"type": "typing", "conversation": "c2"}])
        );
    }

    #[tokio::test]
    async fn route_an_empty_poll_with_wait_zero_returns_at_once() {
        let st = crate::channels::testkit::state().await;
        let token = fake_key(&st).await;
        for path in ["/channels/fake/outbox", "/channels/fake/outbox?wait=0"] {
            let reply = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                call(&st, &token, "GET", path, ""),
            )
            .await
            .expect("an empty poll with wait 0 must not park");
            assert_eq!(reply.0, StatusCode::OK);
            assert_eq!(reply.1, serde_json::json!({"actions": [], "ephemeral": []}));
        }
    }

    #[tokio::test]
    async fn route_a_wait_that_is_not_a_number_is_refused_by_the_extractor() {
        let st = crate::channels::testkit::state().await;
        let token = fake_key(&st).await;
        let (status, _) = call(&st, &token, "GET", "/channels/fake/outbox?wait=soon", "").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn route_ack_ok_returns_state_done() {
        let st = crate::channels::testkit::state().await;
        let token = fake_key(&st).await;
        let id = leased(&st, &token).await;

        let (status, body) = call(
            &st,
            &token,
            "POST",
            &format!("/channels/fake/outbox/{id}/ack"),
            r#"{"ok":true,"platform_refs":["p1"]}"#,
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"state": "done"}));
        let bytes = serde_json::to_vec(&body).unwrap();
        contract::decode::<contract::AckResponseShape>(&bytes).expect("ack_response shape");
        assert_eq!(
            state_of(&st.pool, id).await,
            ("done".to_string(), Some(r#"["p1"]"#.to_string()))
        );
    }

    #[tokio::test]
    async fn route_ack_ok_without_platform_refs_is_done() {
        // Q5: a `send` acknowledged `ok` with no refs is a success; nothing is retried.
        let st = crate::channels::testkit::state().await;
        let token = fake_key(&st).await;
        let id = leased(&st, &token).await;

        let (status, body) = call(
            &st,
            &token,
            "POST",
            &format!("/channels/fake/outbox/{id}/ack"),
            r#"{"ok":true}"#,
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"state": "done"}));
        assert_eq!(
            state_of(&st.pool, id).await,
            ("done".to_string(), Some("[]".to_string()))
        );
        let (_, polled) = call(&st, &token, "GET", "/channels/fake/outbox", "").await;
        assert_eq!(polled["actions"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn route_a_bad_ack_is_a_400_with_the_pointer() {
        let st = crate::channels::testkit::state().await;
        let token = fake_key(&st).await;
        let id = leased(&st, &token).await;
        let path = format!("/channels/fake/outbox/{id}/ack");

        let (status, body) = call(&st, &token, "POST", &path, r#"{"ok":false}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, serde_json::json!({"error": "value", "at": "/ok"}));

        let (status, body) = call(&st, &token, "POST", &path, "").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, serde_json::json!({"error": "decode"}));

        // Nothing was settled by the refused bodies.
        assert_eq!(state_of(&st.pool, id).await.0, "leased");
    }

    #[tokio::test]
    async fn route_a_retryable_failure_puts_the_row_back_to_pending() {
        let st = crate::channels::testkit::state().await;
        let token = fake_key(&st).await;
        let id = leased(&st, &token).await;
        let (status, body) = call(
            &st,
            &token,
            "POST",
            &format!("/channels/fake/outbox/{id}/ack"),
            r#"{"failed":{"retryable":true,"reason":"network"}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"state": "pending"}));
    }

    #[tokio::test]
    async fn route_renew_extends_a_leased_row() {
        let st = crate::channels::testkit::state().await;
        let token = fake_key(&st).await;
        let id = leased(&st, &token).await;

        let (status, body) = call(
            &st,
            &token,
            "POST",
            &format!("/channels/fake/outbox/{id}/renew"),
            "",
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let bytes = serde_json::to_vec(&body).unwrap();
        contract::decode::<contract::RenewResponseShape>(&bytes).expect("renew_response shape");
    }

    #[tokio::test]
    async fn route_renew_after_done_is_409_not_leased() {
        let st = crate::channels::testkit::state().await;
        let token = fake_key(&st).await;
        let id = leased(&st, &token).await;
        let ack_path = format!("/channels/fake/outbox/{id}/ack");
        call(&st, &token, "POST", &ack_path, r#"{"ok":true}"#).await;

        let (status, body) = call(
            &st,
            &token,
            "POST",
            &format!("/channels/fake/outbox/{id}/renew"),
            "",
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body, serde_json::json!({"error": "not_leased"}));
    }

    #[tokio::test]
    async fn route_ack_of_another_channels_row_is_404() {
        let st = crate::channels::testkit::state().await;
        let token = fake_key(&st).await;
        let foreign = queue(&st, "telegram", "c1").await;

        let (status, body) = call(
            &st,
            &token,
            "POST",
            &format!("/channels/fake/outbox/{foreign}/ack"),
            r#"{"ok":true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "not_found");

        let (status, _) = call(
            &st,
            &token,
            "POST",
            &format!("/channels/fake/outbox/{foreign}/renew"),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(state_of(&st.pool, foreign).await.0, "pending");
    }

    #[tokio::test]
    async fn route_a_non_numeric_id_is_refused_by_the_extractor() {
        let st = crate::channels::testkit::state().await;
        let token = fake_key(&st).await;
        let (status, _) = call(&st, &token, "POST", "/channels/fake/outbox/abc/ack", "{}").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn route_the_control_key_is_refused_on_every_outbox_route() {
        let st = crate::channels::testkit::state().await;
        for (method, path) in [
            ("GET", "/channels/fake/outbox"),
            ("POST", "/channels/fake/outbox/1/ack"),
            ("POST", "/channels/fake/outbox/1/renew"),
        ] {
            let (status, _) = call(&st, "test-token", method, path, "").await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        }
    }
}
