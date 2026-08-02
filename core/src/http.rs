use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tower_http::cors::{Any, CorsLayer};

use crate::attention::{self, AttentionScope};
use crate::auth::{ApiTokenLevel, mint_api_token, require_token};
use crate::autopilot::{self, ActivationError, Mode, ProjectSummary, ScopedKill};
use crate::backup;
use crate::budget;
use crate::feed::{self, FeedEntry};
use crate::health;
use crate::hooks::pretooluse_decision;
use crate::inspect;
use crate::presets;
use crate::runs::{self, AwaitingRun, CreateRunError, cancel_run, create_run, get_run};
use crate::shadow::{self, ClassTally, ShadowDecision};
use crate::state::AppState;
use crate::worktree::{self, ReleaseOutcome};

pub fn build_router(state: AppState) -> Router {
    // The production WebView2 origin is the only one a shipped build ever uses.
    let mut origins = vec!["https://tauri.localhost".parse().unwrap()];
    // The Vite dev server was compiled into release builds too. Binding localhost:1420 needs no
    // privilege on Windows, so any local process could serve a page whose cross-origin reads of
    // this API the browser would then approve — leaving only the bearer token in the way, and the
    // shell hands that to its own webview. A dev convenience does not belong in a shipped binary.
    #[cfg(debug_assertions)]
    origins.push("http://localhost:1420".parse().unwrap());

    let cors = CorsLayer::new()
        .allow_origin(origins)
        .allow_methods(Any)
        .allow_headers(Any);

    let protected = Router::new()
        .route("/status", get(status))
        .route("/health/readout", get(health_readout))
        .route("/sidecars", get(get_sidecars))
        .route("/config/email", get(get_email_config))
        .route("/backup", post(post_backup))
        .route("/backups", get(get_backups))
        .route("/backups/{name}/restore", post(post_backup_restore))
        .route(
            "/autopilot/state",
            get(get_autopilot_state).post(post_autopilot_state),
        )
        .route(
            "/autopilot/kill",
            get(get_autopilot_kill).post(post_autopilot_kill),
        )
        .route(
            "/autopilot/kill/scoped",
            get(get_autopilot_kill_scoped).post(post_autopilot_kill_scoped),
        )
        .route(
            "/autopilot/budget",
            get(get_autopilot_budget).post(post_autopilot_budget),
        )
        .route("/autopilot/attention", post(post_attention_heartbeat))
        .route("/projects", get(get_projects))
        .route("/projects/{id}/rules", get(get_project_rules))
        .route("/projects/{id}/wip-limit", post(post_project_wip_limit))
        .route("/projects/{id}/ls", get(get_project_ls))
        .route("/projects/{id}/cat", get(get_project_cat))
        .route("/projects/{id}/grep", get(get_project_grep))
        .route("/projects/{id}/diff", get(get_project_diff))
        .route("/feed", get(get_feed))
        .route("/runs", get(get_runs).post(create_run))
        .route(
            "/webhooks/push",
            post(post_webhook_push)
                .layer(DefaultBodyLimit::max(crate::webhook::WEBHOOK_BODY_LIMIT)),
        )
        .route("/presets", get(list_presets).post(create_preset))
        .route(
            "/presets/{id}",
            get(get_preset).put(update_preset).delete(delete_preset),
        )
        .route("/presets/{id}/run", post(run_preset))
        // The literal path coexists with `/runs/{id}`; static segments win in matchit.
        .route("/runs/awaiting-approval", get(list_awaiting_approval_runs))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route(
            "/runs/{id}/message",
            post(post_run_message).delete(delete_run_message),
        )
        .route("/jobs", get(get_jobs))
        .route("/jobs/{id}", get(get_job))
        // Distinct from `/runs/{id}/cancel`, which stops one node. Both end the job — a stopped
        // node leaves the tree holding edits no gate measured — but only this one reaches a job
        // that has no node in flight: parked for budget, waiting for the slot, or between nodes.
        .route("/jobs/{id}/cancel", post(cancel_job))
        .route("/assistant/message", post(post_assistant_message))
        // Static segment ahead of `{turn_id}`; matchit prefers the literal, so a chat named like a
        // number cannot shadow a turn id.
        .route("/assistant/chats/{chat_id}", get(get_assistant_chat))
        .route("/assistant/{turn_id}", get(get_run))
        .route("/proposals", get(get_proposals))
        .route("/proposals/{id}/approve", post(post_proposal_approve))
        .route("/proposals/{id}/reject", post(post_proposal_reject))
        .route("/worktrees/{run_id}/release", post(post_worktree_release))
        .route("/shadow-decisions", get(get_unreviewed_shadow_decisions))
        .route("/shadow-decisions/{id}/verdict", post(post_shadow_verdict))
        .route("/scoreboard", get(get_scoreboard))
        .route("/email/cursor", get(get_email_cursor))
        .route("/email/triage", post(post_email_triage))
        .route("/email/queue", get(get_email_queue))
        // Admin-only by construction: absent from BOTH scope tables in `auth.rs`, which is where
        // the reason is written down.
        .route("/email/send", post(crate::mailsend::post_email_send))
        // The one route whose legitimate payload outgrows axum's 2 MB default. A full batch is
        // 200 messages of up to 32 KiB of body each (the sidecar's own `MaxPerBatch` and
        // `MaxBodyBytes`), so ~6.4 MB of text before subjects, headers and JSON escaping — the
        // ordinary shape of a first sync, not an attack. Rejecting it at the transport was
        // self-inflicted deadlock rather than a limit: the 413 stopped the cursor advancing, so
        // the sidecar re-sent the identical batch every five minutes for as long as the mailbox
        // stayed that busy, and `MAX_MESSAGES_PER_BATCH` never ran because the body never reached
        // the handler. Sized with headroom over the sidecar's ceiling and applied only here, so
        // every other route keeps the tighter default.
        .route(
            "/email/incoming",
            post(post_email_incoming).layer(DefaultBodyLimit::max(EMAIL_BATCH_BODY_LIMIT)),
        )
        // Static segments win over `{id}` in matchit, so the three routes above stay reachable.
        .route("/email/{id}", get(get_email))
        .route(
            "/email/{id}/attachments/{position}",
            get(get_email_attachment),
        )
        // Static `/attachments` and the parameterised `/attachments/{position}` coexist; matchit
        // prefers the literal, so the bulk routes never shadow a single one.
        .route("/email/{id}/attachments", get(get_email_attachments))
        .route(
            "/email/{id}/attachments/save-all",
            post(post_email_attachments_save_all),
        )
        .route(
            "/email/{id}/attachments/{position}/save",
            post(post_email_attachment_save),
        )
        .route("/email/{id}/requeue", post(post_email_requeue))
        // The address travels in the body rather than the path: an email address is not a safe path
        // segment, and encoding one into a route only to decode it again buys nothing here.
        .route("/contacts", get(get_contacts))
        .route("/contacts/verdict", post(post_sender_verdict))
        .route("/contacts/unmerge", post(post_contact_unmerge))
        .route("/contacts/merges", get(get_contact_merges))
        // A twenty-minute memo is ~38 MB of 16 kHz PCM, and every route not given its own ceiling
        // inherits axum's 2 MB default — which would reject precisely the long recordings that are
        // least repeatable, and reject them the same way every time. The ceiling is derived from
        // `voice::MAX_CAPTURE_SECONDS` rather than written out, so the two cannot drift apart.
        .route(
            "/voice/capture",
            post(crate::voice::post_capture)
                .layer(DefaultBodyLimit::max(crate::voice::max_body_bytes())),
        )
        .route("/voice/config", get(crate::voice::get_config))
        .route("/voice/memos", get(crate::voice::list_memos))
        // Read by hand for prompt tuning, not by the shell — see voice.rs's `list_dictations`.
        .route("/voice/dictations", get(crate::voice::list_dictations))
        .route(
            "/voice/memos/{id}",
            get(crate::voice::get_memo).delete(crate::voice::delete_memo),
        )
        // The literal `/calendar/busy` and `/calendar/config` coexist with no `{id}` sibling at
        // that depth, so no shadowing question arises here — unlike `/runs/awaiting-approval`.
        .route(
            "/calendar/events",
            get(crate::calendar::list_events).post(crate::calendar::create_event),
        )
        .route(
            "/calendar/events/{id}",
            axum::routing::delete(crate::calendar::delete_event),
        )
        .route(
            "/calendar/events/{id}/cancel",
            post(crate::calendar::cancel_occurrence),
        )
        .route(
            "/calendar/events/{id}/move",
            post(crate::calendar::move_occurrence),
        )
        .route("/calendar/busy", get(crate::calendar::get_busy))
        .route("/calendar/config", get(crate::calendar::get_config))
        .route("/notifications/pending", get(crate::notify::list_pending))
        .route("/files", get(get_files).delete(delete_file))
        .route("/files/folder", post(post_files_folder))
        .route("/files/download", get(get_file_download))
        .route("/files/search", get(get_files_search))
        .route("/files/move", post(post_files_move))
        // Whole-body limit rather than the 2 MB default, for the same reason `/voice/capture` has
        // one: the request this route exists for is bigger than the default and would be rejected
        // identically every time. The ceiling is a memory ceiling too — see `files::MAX_UPLOAD_BYTES`.
        .route(
            "/files/upload",
            post(post_file_upload).layer(DefaultBodyLimit::max(crate::files::MAX_UPLOAD_BYTES)),
        )
        .route("/hooks/pretooluse-decision", post(pretooluse_decision))
        .route("/api-tokens", get(list_api_tokens).post(create_api_token))
        .route(
            "/api-tokens/{name}",
            axum::routing::delete(revoke_api_token),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_token,
        ))
        .with_state(state);

    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .layer(cors)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn health_readout(State(state): State<AppState>) -> Json<health::HealthReadout> {
    Json(health::readout(state).await)
}

async fn post_webhook_push(
    State(state): State<AppState>,
    Json(delivery): Json<crate::webhook::Delivery>,
) -> Result<(StatusCode, Json<crate::webhook::DeliveryOutcome>), StatusCode> {
    let outcome = crate::webhook::deliver(&state, delivery, chrono::Utc::now())
        .await
        .map_err(|error| match error {
            crate::webhook::DeliveryError::Invalid => StatusCode::BAD_REQUEST,
            crate::webhook::DeliveryError::Unconfigured => StatusCode::NOT_FOUND,
            crate::webhook::DeliveryError::Storage(error) => {
                tracing::warn!(%error, "webhook delivery storage failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
            crate::webhook::DeliveryError::Config(error) => {
                tracing::warn!(%error, "webhook project configuration could not be read");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        })?;
    let status = match outcome {
        crate::webhook::DeliveryOutcome::Fired { .. } => StatusCode::ACCEPTED,
        crate::webhook::DeliveryOutcome::Duplicate => StatusCode::OK,
        crate::webhook::DeliveryOutcome::Deferred { .. } => StatusCode::SERVICE_UNAVAILABLE,
    };
    Ok((status, Json(outcome)))
}

async fn status() -> impl IntoResponse {
    (StatusCode::OK, "daemon running")
}

#[derive(Deserialize)]
struct CreateApiTokenRequest {
    name: String,
    level: ApiTokenLevel,
}

#[derive(Serialize)]
struct CreatedApiToken {
    name: String,
    level: ApiTokenLevel,
    created_at: String,
    /// The complete bearer credential. It is returned only by creation, never by listing.
    token: String,
}

#[derive(Serialize)]
struct ApiTokenSummary {
    name: String,
    level: ApiTokenLevel,
    created_at: String,
}

#[derive(sqlx::FromRow)]
struct ApiTokenRow {
    name: String,
    access_level: String,
    created_at: String,
}

fn valid_api_token_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

async fn create_api_token(
    State(state): State<AppState>,
    Json(body): Json<CreateApiTokenRequest>,
) -> Result<(StatusCode, Json<CreatedApiToken>), StatusCode> {
    if !valid_api_token_name(&body.name) {
        return Err(StatusCode::BAD_REQUEST);
    }

    let created_at = chrono::Utc::now().to_rfc3339();
    let (token, secret) = mint_api_token(&body.name);
    let result = sqlx::query(
        "INSERT INTO api_tokens (name, token, access_level, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&body.name)
    .bind(secret)
    .bind(body.level.as_str())
    .bind(&created_at)
    .execute(&state.pool)
    .await;

    if let Err(error) = result {
        if error
            .as_database_error()
            .is_some_and(|database| database.is_unique_violation())
        {
            return Err(StatusCode::CONFLICT);
        }
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    Ok((
        StatusCode::CREATED,
        Json(CreatedApiToken {
            name: body.name,
            level: body.level,
            created_at,
            token,
        }),
    ))
}

async fn list_api_tokens(
    State(state): State<AppState>,
) -> Result<Json<Vec<ApiTokenSummary>>, StatusCode> {
    let rows = sqlx::query_as::<_, ApiTokenRow>(
        "SELECT name, access_level, created_at FROM api_tokens ORDER BY created_at, name",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    rows.into_iter()
        .map(|row| {
            Ok(ApiTokenSummary {
                name: row.name,
                level: ApiTokenLevel::from_str(&row.access_level)
                    .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?,
                created_at: row.created_at,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Json)
}

async fn revoke_api_token(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let result = sqlx::query("DELETE FROM api_tokens WHERE name = ?")
        .bind(name)
        .execute(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if result.rows_affected() == 0 {
        Err(StatusCode::NOT_FOUND)
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}

async fn post_backup(
    State(state): State<AppState>,
) -> Result<Json<backup::BackupInfo>, StatusCode> {
    backup::take_backup(&state.pool, backup::DEFAULT_RETENTION)
        .await
        .map(Json)
        .map_err(|error| backup_status(&error))
}

async fn get_backups(
    State(state): State<AppState>,
) -> Result<Json<Vec<backup::BackupInfo>>, StatusCode> {
    backup::list_backups(&state.pool)
        .await
        .map(Json)
        .map_err(|error| backup_status(&error))
}

async fn post_backup_restore(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<backup::StagedRestore>, StatusCode> {
    if !plain_filename(&name) {
        return Err(StatusCode::BAD_REQUEST);
    }

    backup::stage_restore(&state.pool, &name)
        .await
        .map(Json)
        .map_err(|error| backup_status(&error))
}

fn plain_filename(name: &str) -> bool {
    if name.is_empty() || name.contains("..") || name.contains('/') || name.contains('\\') {
        return false;
    }

    let mut components = std::path::Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    )
}

fn backup_status(error: &backup::BackupError) -> StatusCode {
    match error {
        backup::BackupError::InvalidName | backup::BackupError::InvalidRetention => {
            StatusCode::BAD_REQUEST
        }
        backup::BackupError::NotFound => StatusCode::NOT_FOUND,
        backup::BackupError::ExistingTarget(_) | backup::BackupError::PendingRestoreExists => {
            StatusCode::CONFLICT
        }
        backup::BackupError::Verification(_) => StatusCode::UNPROCESSABLE_ENTITY,
        backup::BackupError::Database(_) | backup::BackupError::Io(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[derive(Deserialize)]
struct ProjectQuery {
    project_id: String,
}

#[derive(Deserialize)]
struct FeedQuery {
    project_id: Option<String>,
    scope: Option<String>,
    q: Option<String>,
    kind: Option<String>,
    since: Option<String>,
    until: Option<String>,
    limit: Option<String>,
}

#[derive(Deserialize)]
struct RunsQuery {
    project_id: Option<String>,
    status: Option<String>,
    mode: Option<String>,
    q: Option<String>,
    since: Option<String>,
    until: Option<String>,
    limit: Option<String>,
}

#[derive(Deserialize)]
struct PathQuery {
    path: Option<String>,
}

#[derive(Deserialize)]
struct GrepQuery {
    q: Option<String>,
    path: Option<String>,
}

#[derive(Deserialize)]
struct AssistantMessageRequest {
    chat_id: String,
    text: String,
}

#[derive(Deserialize)]
struct VerdictRequest {
    verdict: String,
}

#[derive(Deserialize)]
struct AutopilotStateRequest {
    project_id: String,
    mode: String,
    project_root: Option<String>,
}

#[derive(serde::Serialize)]
struct AutopilotStateResponse {
    project_id: String,
    mode: Mode,
}

#[derive(Deserialize)]
struct AutopilotKillRequest {
    engaged: bool,
}

#[derive(Deserialize)]
struct ScopedKillRequest {
    scope_type: String,
    scope_id: String,
    engaged: bool,
}

#[derive(serde::Serialize)]
struct AutopilotKillResponse {
    engaged: bool,
}

#[derive(serde::Serialize)]
struct BudgetResponse {
    limit_usd: Option<f64>,
    period: String,
    hourly_limit_usd: Option<f64>,
    per_run_reserve_usd: f64,
    time_cost_per_hour_usd: f64,
    window_spend_usd: f64,
    hourly_spend_usd: f64,
    paused: bool,
    reason: Option<String>,
}

#[derive(Deserialize)]
struct BudgetRequest {
    limit_usd: Option<f64>,
    period: String,
    hourly_limit_usd: Option<f64>,
    per_run_reserve_usd: f64,
    time_cost_per_hour_usd: f64,
}

#[derive(Deserialize)]
struct AttentionHeartbeatRequest {
    project_id: Option<String>,
}

#[derive(Deserialize)]
struct MailboxQuery {
    mailbox: String,
}

/// Reads an absent list, a `null` list and an empty list as the same thing: nothing.
///
/// `#[serde(default)]` alone covers only the ABSENT case, and Go's `encoding/json` writes a nil
/// slice as `null` rather than omitting it. That gap ate a real inbox: the sidecar read the mail,
/// the núcleo answered 422, and the batch replayed into the same wall every five minutes. It
/// survived the tests because the fixtures omitted the field, which is a shape the sidecar never
/// sends. There is no message these three encodings could carry that differs.
fn absent_or_null_is_empty<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

/// The sidecar's delivery envelope (spec §4.3).
#[derive(Deserialize)]
struct EmailIncomingRequest {
    mailbox: String,
    uidvalidity: i64,
    /// The highest uid the sidecar LOOKED AT, which is what lets the cursor move past a message it
    /// could not read. Not the same as the highest uid delivered.
    max_uid_examined: i64,
    /// Absent, `null` and `""` all mean inbound — see `absent_or_null_is_empty` above for why the
    /// empty case has to be spelled out: Go writes an unset string field as `""` rather than
    /// omitting it, and `daemon.Batch.Direction` has no `omitempty`.
    #[serde(default)]
    direction: Option<String>,
    #[serde(default, deserialize_with = "absent_or_null_is_empty")]
    skipped: Vec<crate::email::SkippedMessage>,
    #[serde(default, deserialize_with = "absent_or_null_is_empty")]
    messages: Vec<crate::email::IncomingMessage>,
}

/// Paging belongs to the sidecar; a batch this large means it stopped doing its job, and the
/// núcleo should say so rather than quietly ingest whatever arrives.
const MAX_MESSAGES_PER_BATCH: usize = 200;

/// Body ceiling for `/email/incoming`, in bytes.
///
/// `MAX_MESSAGES_PER_BATCH` x the sidecar's 32 KiB per-body cap is ~6.4 MB of text; doubling it
/// covers subjects, addresses, headers and JSON escaping without turning the route into an
/// unbounded sink. It is a backstop, not the real limit — the count check in the handler is.
const EMAIL_BATCH_BODY_LIMIT: usize = 16 * 1024 * 1024;

/// How much of the mailbox the queue hands back. Generous because a first sync pulls a week at
/// once, and a list that silently stops at its limit is indistinguishable from mail that never
/// arrived — which is the exact confusion this pillar has already cost once.
const EMAIL_QUEUE_LIMIT: i64 = 200;

async fn get_email_cursor(
    State(state): State<AppState>,
    Query(query): Query<MailboxQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let cursor = crate::email::get_cursor(&state.pool, &query.mailbox)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(match cursor {
        Some(cursor) => serde_json::json!({
            "uidvalidity": cursor.uidvalidity,
            "last_uid": cursor.last_uid,
        }),
        None => serde_json::Value::Null,
    }))
}

async fn post_email_incoming(
    State(state): State<AppState>,
    Json(body): Json<EmailIncomingRequest>,
) -> Result<Json<crate::email::IngestOutcome>, StatusCode> {
    if body.messages.len() > MAX_MESSAGES_PER_BATCH {
        return Err(StatusCode::BAD_REQUEST);
    }
    let direction = match body.direction.as_deref().map(str::trim) {
        None | Some("") => crate::contacts::MessageDirection::Inbound,
        Some(value) if value.eq_ignore_ascii_case("inbound") => {
            crate::contacts::MessageDirection::Inbound
        }
        Some(value) if value.eq_ignore_ascii_case("outbound") => {
            crate::contacts::MessageDirection::Outbound
        }
        // Defaulting would store the user's sent bodies as inbound; refusal keeps the cursor in
        // place so the retry makes the mailbox complain instead of quietly mis-filing them.
        Some(_) => return Err(StatusCode::BAD_REQUEST),
    };
    // Ingestion is one transaction over untrusted content and it moves the cursor. A client that
    // disconnects mid-request must not be able to leave that half-done.
    let pool = state.pool.clone();
    let retain_bodies_days = state.email.retain_bodies_days;
    uncancellable(async move {
        crate::email::ingest_batch(
            &pool,
            direction,
            &body.mailbox,
            body.uidvalidity,
            body.max_uid_examined,
            &body.skipped,
            &body.messages,
            retain_bodies_days,
            chrono::Utc::now(),
        )
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    })
    .await?
}

/// Triage what is waiting, now.
///
/// Reading the mailbox happens on its own because it is free; spending a run does not. This is the
/// request — so it launches a batch immediately rather than raising a flag and waiting up to a
/// minute for the loop's own tick to notice.
///
/// It answers with what it started, not with verdicts: a run takes minutes, and holding an HTTP
/// request open for it would only make the caller's timeout the deadline for the mail.
async fn post_email_triage(
    State(state): State<AppState>,
) -> Result<Json<crate::triage::TriageOutcome>, StatusCode> {
    if !state.email.armed.load(std::sync::atomic::Ordering::Relaxed) {
        // Not an error the caller can fix by retrying: the pillar is off, or its barrier failed
        // verification at startup and it is deliberately staying off.
        return Ok(Json(crate::triage::TriageOutcome {
            queued: 0,
            run_id: None,
            reason: Some("the email pillar is not armed".to_string()),
        }));
    }

    // Launching writes a claim across the batch's rows; a client that disconnects must not leave
    // that half-written.
    let state = state.clone();
    uncancellable(async move {
        let mut loop_state = crate::triage::LoopState::default();
        crate::triage::triage_now(&state, &mut loop_state, chrono::Utc::now()).await
    })
    .await
    .map(Json)
}

#[derive(serde::Serialize, sqlx::FromRow)]
struct QueuedEmail {
    id: i64,
    from_addr: String,
    from_name: Option<String>,
    subject: Option<String>,
    received_at: String,
    triage_class: Option<String>,
    triage_summary: Option<String>,
    triaged_at: Option<String>,
    has_attachments: i64,
    /// The standing human decision about this sender — `pin`, `mute`, or none.
    ///
    /// Carried on the row rather than fetched per sender, because it is what the button in the list
    /// has to be drawn from: without it every row would have to ask separately, and a list of forty
    /// messages would open forty requests to render forty small pieces of state.
    sender_verdict: Option<String>,
}

/// How many correspondents the roster returns. Enough to find anyone; short enough to draw.
const CONTACTS_LIMIT: i64 = 200;

/// Who writes to you, busiest first, with each one's standing decision.
///
/// `contacts.rs` has recorded this since it existed — every inbound message updates a count and a
/// last-seen, and `priority.rs` reads it to tell a stranger from someone you know. None of it was
/// readable: the module still carries `#[allow(dead_code)]` on three fields "consumed by the later
/// contact display surface", and this is that surface for the part that is actually finished.
///
/// Deliberately NOT the merged view. `propose_merges` and `merge` exist and nothing in production
/// calls either, so two addresses belonging to one person are still two rows — and reporting them
/// as one would be reporting a judgement nobody has made.
async fn get_contacts(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::contacts::Correspondent>>, StatusCode> {
    crate::contacts::roster(&state.pool, CONTACTS_LIMIT)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "reading the contact roster failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// Suggestions that two addresses are one person, waiting on an answer.
///
/// Its own route rather than a slice of `/proposals`, because the two kinds share a table and
/// nothing else — see `contacts::pending_merges`. It also puts the question where the context is:
/// deciding whether two addresses are the same person is a thing you do while looking at your
/// correspondents, not while looking at a queue of paused runs.
async fn get_contact_merges(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::contacts::MergeSuggestion>>, StatusCode> {
    crate::contacts::pending_merges(&state.pool)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "reading pending contact merges failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[derive(Deserialize)]
struct UnmergeRequest {
    address: String,
}

/// Splits one address back out into a person of its own.
///
/// The undo for an approved merge, and the reason approving one is a safe thing to offer: the join
/// is a pointer move, so undoing it restores exactly what was there — counters included, since a
/// merge never moved them. Without this route the merge was still exact and still undoable in
/// principle, and unreachable in practice.
///
/// Answers 204 whether or not the address was merged with anything. Splitting an address that is
/// already alone is the state the caller asked for, and a caller that had to tell those apart would
/// be handling somebody else's bookkeeping.
async fn post_contact_unmerge(
    State(state): State<AppState>,
    Json(body): Json<UnmergeRequest>,
) -> Result<StatusCode, StatusCode> {
    let known: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM contact_addresses WHERE address = ?")
            .bind(crate::contacts::normalize_address(&body.address))
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if known.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }

    crate::contacts::unmerge(&state.pool, &body.address)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| {
            tracing::warn!(%error, "splitting a contact failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[derive(Deserialize)]
struct SenderVerdictRequest {
    address: String,
    /// `pin`, `mute`, or `null` to withdraw the decision entirely.
    verdict: Option<String>,
}

/// Records what a person has decided about a sender, once, for all their future mail.
///
/// This is the only writer of `contact_overrides` outside a test. The table has been read by
/// `priority::adjust` since it existed — a pin outranks the model and a mute outranks it the other
/// way — so until now the highest-authority rule in triage was one nothing could set.
///
/// An unknown verdict is refused rather than stored. `priority::adjust` falls through for anything
/// it does not recognise, which means a typo would be accepted, saved, and then do nothing at all
/// for as long as it sat there; a 400 now is the only moment that mistake is visible.
///
/// An address nobody has written from is a 404 for a related reason: a contact exists because mail
/// arrived, and inventing one here would let a mistyped address become a permanent row that never
/// matches anything and never explains why.
async fn post_sender_verdict(
    State(state): State<AppState>,
    Json(body): Json<SenderVerdictRequest>,
) -> Result<StatusCode, StatusCode> {
    if let Some(verdict) = body.verdict.as_deref()
        && !crate::priority::is_known_verdict(verdict)
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    match crate::contacts::set_verdict(&state.pool, &body.address, body.verdict.as_deref()).await {
        Ok(crate::contacts::VerdictOutcome::Applied) => Ok(StatusCode::NO_CONTENT),
        Ok(crate::contacts::VerdictOutcome::UnknownAddress) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, "recording a sender verdict failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// One attachment, described. The bytes are not here and are not stored (migration 0021).
#[derive(serde::Serialize, sqlx::FromRow)]
struct EmailAttachment {
    position: i64,
    filename: Option<String>,
    mime_type: Option<String>,
    size_bytes: i64,
}

/// One message in full.
///
/// Separate from the queue's row because the body is the expensive and sensitive half: a mailbox
/// list has no business carrying a mailbox's worth of third-party text, and this way opening a
/// message is the moment that text is read out of the database, not a side effect of drawing a list.
#[derive(serde::Serialize, sqlx::FromRow)]
struct EmailDetail {
    id: i64,
    from_addr: String,
    from_name: Option<String>,
    subject: Option<String>,
    received_at: String,
    triage_class: Option<String>,
    triage_summary: Option<String>,
    triaged_at: Option<String>,
    /// What the model answered; NULL when the row was never triaged.
    model_class: Option<String>,
    /// Which rule decided the stored class; NULL when none fired or the row was never triaged.
    priority_rule: Option<String>,
    /// NULL once retention has pruned it (§7.2), which is a state the reader must show rather than
    /// mistake for an empty message.
    body_text: Option<String>,
    has_attachments: i64,
}

#[derive(serde::Serialize)]
struct EmailDetailResponse {
    #[serde(flatten)]
    message: EmailDetail,
    attachments: Vec<EmailAttachment>,
}

/// One message, body included — what "open the mail" reads.
async fn get_email(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<EmailDetailResponse>, StatusCode> {
    let message: EmailDetail = sqlx::query_as(
        "SELECT id, from_addr, from_name, subject, received_at, triage_class, triage_summary,
                triaged_at, model_class, priority_rule, body_text, has_attachments
           FROM emails WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let attachments: Vec<EmailAttachment> = sqlx::query_as(
        "SELECT position, filename, mime_type, size_bytes
           FROM email_attachments WHERE email_id = ? ORDER BY position",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(EmailDetailResponse {
        message,
        attachments,
    }))
}

/// How long the núcleo waits for the sidecar to go and get a file. Generous because the sidecar
/// opens a fresh TLS connection and may be fetching 25 MB over someone's home connection.
const ATTACHMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// PURE: builds a `Content-Disposition` for a filename chosen by a stranger.
///
/// Always `attachment`, never `inline`: the bytes came from outside and nothing renders them in
/// place. The name goes out twice — a conservative ASCII form for old clients and the RFC 6266
/// `filename*` form for everything else — and both are built from `safe_filename`'s output, so a
/// name carrying a carriage return cannot end this header and begin one of the sender's choosing.
fn content_disposition(filename: &str) -> String {
    let safe = crate::email::safe_filename(filename);
    // The quoted form cannot carry a quote or a backslash without escaping them, and a filename is
    // not worth the escaping rules — anything outside a plain set becomes an underscore, and the
    // faithful version travels in `filename*` alongside it.
    let ascii: String = safe
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::with_capacity(safe.len());
    for byte in safe.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// One attachment's bytes, fetched from the mailbox at the moment they are asked for.
///
/// Nothing is cached on the way through. The file exists in exactly one place — the mailbox it
/// arrived in — and keeping a copy would mean a stranger's executable sitting on disk because
/// someone once clicked a filename.
async fn get_email_attachment(
    State(state): State<AppState>,
    Path((id, position)): Path<(i64, i64)>,
) -> Result<axum::response::Response, StatusCode> {
    let (bytes, filename) = fetch_attachment(&state, id, position).await?;

    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/octet-stream".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                content_disposition(&filename),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// One attachment as the sidecar hands it over in bulk.
#[derive(serde::Deserialize, serde::Serialize)]
struct BulkAttachment {
    position: i64,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
    size_bytes: i64,
    content_base64: String,
}

/// Every attachment of one message, read in a single pass over the mailbox.
///
/// One route rather than the caller looping, because each single fetch downloads the WHOLE message:
/// a loop over eight attachments pulled the same eight files eight times, over eight connections.
async fn get_email_attachments(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<BulkAttachment>>, StatusCode> {
    fetch_all_attachments(&state, id).await.map(Json)
}

async fn fetch_all_attachments(
    state: &AppState,
    id: i64,
) -> Result<Vec<BulkAttachment>, StatusCode> {
    let uid: Option<i64> = sqlx::query_scalar("SELECT uid FROM emails WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let uid = uid.ok_or(StatusCode::NOT_FOUND)?;

    let client = reqwest::Client::builder()
        .timeout(ATTACHMENT_TIMEOUT)
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let response = client
        .get(format!(
            "http://{}/attachments?uid={uid}",
            crate::sidecar::EMAIL_FETCH_ADDR
        ))
        .bearer_auth(&state.token.0)
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    if !response.status().is_success() {
        return Err(StatusCode::BAD_GATEWAY);
    }
    response
        .json::<Vec<BulkAttachment>>()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)
}

#[derive(serde::Serialize)]
struct SaveAllOutcome {
    folder: String,
    /// The names actually stored, in the order the message carries them.
    filenames: Vec<String>,
}

/// Files every attachment of one message into a folder.
///
/// Partial success is not a state this reports: the write happens after all the bytes are in hand,
/// so the failure that matters — the mailbox being unreachable — happens before anything lands.
/// What can still fail per file is the disk, and a folder holding three of eight files with no word
/// about the other five is worse than a refusal.
async fn post_email_attachments_save_all(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<SaveAttachmentRequest>,
) -> Result<Json<SaveAllOutcome>, StatusCode> {
    let root = files_root(&state)?.to_path_buf();
    let attachments = fetch_all_attachments(&state, id).await?;

    let decoded: Vec<(String, Vec<u8>)> = attachments
        .into_iter()
        .map(|attachment| {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&attachment.content_base64)
                .map_err(|_| StatusCode::BAD_GATEWAY)?;
            Ok((attachment.filename.unwrap_or_default(), bytes))
        })
        .collect::<Result<_, StatusCode>>()?;

    let saved = uncancellable(async move {
        tokio::task::spawn_blocking(move || {
            let mut filenames = Vec::with_capacity(decoded.len());
            for (filename, bytes) in &decoded {
                filenames.push(crate::files::write_file(
                    &root,
                    &body.folder,
                    filename,
                    bytes,
                )?);
            }
            Ok::<_, crate::files::PathError>(SaveAllOutcome {
                folder: body.folder,
                filenames,
            })
        })
        .await
    })
    .await?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    saved.map(Json).map_err(folder_status)
}

/// Asks the sidecar for one attachment's bytes, and reports the name it was described under.
///
/// Shared by the route that hands a file to a person and the one that files it into a folder,
/// because those must never diverge on WHICH file they mean.
async fn fetch_attachment(
    state: &AppState,
    id: i64,
    position: i64,
) -> Result<(Vec<u8>, String), StatusCode> {
    let row: Option<(i64, Option<String>)> = sqlx::query_as(
        "SELECT emails.uid, email_attachments.filename
           FROM email_attachments
           JOIN emails ON emails.id = email_attachments.email_id
          WHERE email_attachments.email_id = ? AND email_attachments.position = ?",
    )
    .bind(id)
    .bind(position)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (uid, filename) = row.ok_or(StatusCode::NOT_FOUND)?;

    let client = reqwest::Client::builder()
        .timeout(ATTACHMENT_TIMEOUT)
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let response = client
        .get(format!(
            "http://{}/attachment?uid={uid}&position={position}",
            crate::sidecar::EMAIL_FETCH_ADDR
        ))
        .bearer_auth(&state.token.0)
        .send()
        .await
        // The sidecar not answering is not the same as the file not existing, and telling a person
        // "not found" when the truth is "nothing went to look" sends them hunting in the mailbox.
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    if !response.status().is_success() {
        return Err(if response.status() == reqwest::StatusCode::NOT_FOUND {
            // The stored description and the live message disagree: the mail was deleted or
            // replaced since it was read.
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_GATEWAY
        });
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    Ok((bytes.to_vec(), filename.unwrap_or_default()))
}

/// Turns a folder refusal into a status. `Escapes` and `Unsafe` are both 400: the request named
/// something it may not name, and which of the two rules caught it is not the caller's business —
/// a distinction here would be a probe for how the guard is built.
///
/// The three 409s are not one answer either, and they do not need to be: a caller knows which of
/// the four operations it just asked for, so "conflict" reads as one sentence per route — this is
/// not a folder, this name is taken, this folder still has things in it.
fn folder_status(error: crate::files::PathError) -> StatusCode {
    use crate::files::PathError;
    match error {
        PathError::Escapes | PathError::Unsafe => StatusCode::BAD_REQUEST,
        PathError::NotFound => StatusCode::NOT_FOUND,
        PathError::NotADirectory | PathError::NotEmpty | PathError::Exists => StatusCode::CONFLICT,
        PathError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The folder root, or a refusal when startup could not create it.
fn files_root(state: &AppState) -> Result<&std::path::Path, StatusCode> {
    if state.email.files_root.as_os_str().is_empty() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(&state.email.files_root)
}

#[derive(Deserialize)]
struct FolderQuery {
    /// Relative to the root. Absent means the root itself.
    #[serde(default)]
    path: String,
}

async fn get_files(
    State(state): State<AppState>,
    Query(query): Query<FolderQuery>,
) -> Result<Json<Vec<crate::files::Entry>>, StatusCode> {
    let root = files_root(&state)?;
    crate::files::list(root, &query.path)
        .map(Json)
        .map_err(folder_status)
}

#[derive(Deserialize)]
struct CreateFolderRequest {
    path: String,
}

async fn post_files_folder(
    State(state): State<AppState>,
    Json(body): Json<CreateFolderRequest>,
) -> Result<StatusCode, StatusCode> {
    let root = files_root(&state)?;
    crate::files::create_folder(root, &body.path)
        .map(|()| StatusCode::CREATED)
        .map_err(folder_status)
}

#[derive(Deserialize)]
struct SearchQuery {
    /// Where the walk starts. Absent means the root itself.
    #[serde(default)]
    path: String,
    q: String,
}

/// Finds entries by name, in one folder and everything under it.
///
/// A folder this size is searched by walking it — there is no index, and building one would be a
/// second copy of the truth to keep honest. The ceilings live in `files::search`, and the answer
/// says when one of them cut in rather than passing a partial result off as the whole.
async fn get_files_search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<crate::files::Found>, StatusCode> {
    let root = files_root(&state)?.to_path_buf();

    // Off the async runtime: a deep tree is a long blocking walk, and holding a runtime thread for
    // it would stall every other request on that thread.
    let found =
        tokio::task::spawn_blocking(move || crate::files::search(&root, &query.path, &query.q))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    found.map(Json).map_err(folder_status)
}

/// Hands back one file's bytes, streamed rather than gathered.
///
/// `attachment`, never `inline`, and `application/octet-stream` whatever the extension says — the
/// same rule the attachment route follows, and for a stronger reason: half of what is in this
/// folder arrived as mail from a stranger, and a webview asked to render it in place would be
/// executing a file this system exists to keep at arm's length.
async fn get_file_download(
    State(state): State<AppState>,
    Query(query): Query<FolderQuery>,
) -> Result<axum::response::Response, StatusCode> {
    let root = files_root(&state)?;
    let target = crate::files::resolve_file(root, &query.path).map_err(folder_status)?;

    // The name comes off the resolved path, not the query string: `resolve_within` has already
    // decided the last component is a name a filesystem carries, and `content_disposition` makes it
    // safe for a header again.
    let filename = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file")
        .to_string();

    let file = tokio::fs::File::open(&target)
        .await
        // Gone between resolving and opening: rare, and it is the same answer as never having been
        // there.
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let length = file.metadata().await.ok().map(|metadata| metadata.len());

    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        axum::http::header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_str(&content_disposition(&filename))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    if let Some(length) = length {
        headers.insert(axum::http::header::CONTENT_LENGTH, length.into());
    }

    let stream = tokio_util::io::ReaderStream::new(file);
    Ok((headers, axum::body::Body::from_stream(stream)).into_response())
}

#[derive(Deserialize)]
struct UploadQuery {
    /// Which folder under the root. Absent means the root itself.
    #[serde(default)]
    folder: String,
    /// What the browser called the file. Made safe on the way to disk, like a sender's name is.
    filename: String,
}

/// The other way bytes enter this folder: a person picking a file of their own.
///
/// It goes through the same `write_file` as filing an attachment — same name rule, same numbered
/// collisions — because the folder's guarantee is about the folder, not about who is writing. The
/// stored name is reported back for the same reason it is there: it can differ from what was sent.
async fn post_file_upload(
    State(state): State<AppState>,
    Query(query): Query<UploadQuery>,
    bytes: axum::body::Bytes,
) -> Result<Json<SavedFile>, StatusCode> {
    let root = files_root(&state)?.to_path_buf();

    // Blocking file I/O off the async runtime, and uncancellable for the same reason as filing: a
    // client that disconnects mid-write must not leave half a file under a name that says it is
    // whole.
    let saved = uncancellable(async move {
        tokio::task::spawn_blocking(move || {
            crate::files::write_file(&root, &query.folder, &query.filename, &bytes).map(|stored| {
                SavedFile {
                    filename: stored,
                    folder: query.folder,
                }
            })
        })
        .await
    })
    .await?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    saved.map(Json).map_err(folder_status)
}

#[derive(Deserialize)]
struct MoveRequest {
    from: String,
    to: String,
}

/// Renames or moves one entry. Both ends are resolved against the root, so neither can name a
/// destination outside it.
async fn post_files_move(
    State(state): State<AppState>,
    Json(body): Json<MoveRequest>,
) -> Result<StatusCode, StatusCode> {
    let root = files_root(&state)?.to_path_buf();

    let moved = uncancellable(async move {
        tokio::task::spawn_blocking(move || crate::files::move_entry(&root, &body.from, &body.to))
            .await
    })
    .await?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    moved
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(folder_status)
}

#[derive(Deserialize)]
struct DeleteQuery {
    path: String,
    /// Required to remove a folder that still has something in it — see `files::delete`.
    #[serde(default)]
    recursive: bool,
}

async fn delete_file(
    State(state): State<AppState>,
    Query(query): Query<DeleteQuery>,
) -> Result<StatusCode, StatusCode> {
    let root = files_root(&state)?.to_path_buf();

    let deleted = uncancellable(async move {
        tokio::task::spawn_blocking(move || {
            crate::files::delete(&root, &query.path, query.recursive)
        })
        .await
    })
    .await?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    deleted
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(folder_status)
}

#[derive(Deserialize)]
struct SaveAttachmentRequest {
    /// Which folder under the root. Empty means the root itself.
    #[serde(default)]
    folder: String,
}

#[derive(serde::Serialize)]
struct SavedFile {
    /// The name it was ACTUALLY stored under, which can differ from the one it arrived with twice
    /// over: once because the name was made safe, once because it collided. True of a sender's
    /// attachment and of a file the user picked themselves — this is the answer to "where did it
    /// go", and guessing it is how a caller ends up naming a file that is not there.
    filename: String,
    folder: String,
}

/// Fetches an attachment and files it into the folder — the one path where a stranger's bytes are
/// written to this disk, and it happens because a person asked for it by name.
async fn post_email_attachment_save(
    State(state): State<AppState>,
    Path((id, position)): Path<(i64, i64)>,
    Json(body): Json<SaveAttachmentRequest>,
) -> Result<Json<SavedFile>, StatusCode> {
    let root = files_root(&state)?.to_path_buf();
    let (bytes, filename) = fetch_attachment(&state, id, position).await?;

    // Blocking file I/O off the async runtime, and uncancellable: a client that disconnects
    // mid-write must not leave half a file behind under a name that says it is whole.
    let saved = uncancellable(async move {
        tokio::task::spawn_blocking(move || {
            crate::files::write_file(&root, &body.folder, &filename, &bytes).map(|stored| {
                SavedFile {
                    filename: stored,
                    folder: body.folder,
                }
            })
        })
        .await
    })
    .await?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    saved.map(Json).map_err(folder_status)
}

/// The mail the pillar knows about: what is waiting, and what it most recently said.
async fn get_email_queue(
    State(state): State<AppState>,
) -> Result<Json<Vec<QueuedEmail>>, StatusCode> {
    // Newest arrival first — the order a mailbox is read in. Deliberately NOT by triage time: a
    // verdict landing now would otherwise drag a week-old message to the top, and a list that
    // reorders itself while you read it is one you lose your place in. Waiting mail is marked
    // rather than floated for the same reason; the count and the button live above the list.
    //
    // This list is the mail that came in. The user's own sent mail is held for what it says about a
    // correspondent, not read back to them; filtering it also stops sent mail consuming
    // `EMAIL_QUEUE_LIMIT` slots.
    //
    // Sorting `received_at` as text is a chronological sort because the sidecar normalises the
    // server's INTERNALDATE to UTC (`...Z`), so every value shares one offset. `id` breaks ties
    // within a second, which a bulk delivery produces routinely.
    //
    // `failed` sorts with the rest rather than being hidden: it is the class most likely to be
    // requeued, so it is the one that must stay findable.
    let mut queue = sqlx::query_as::<_, QueuedEmail>(
        "SELECT id, from_addr, from_name, subject, received_at, triage_class, triage_summary,
                triaged_at, has_attachments, NULL AS sender_verdict
           FROM emails
          WHERE direction = 'inbound'
          ORDER BY received_at DESC, id DESC
          LIMIT ?",
    )
    .bind(EMAIL_QUEUE_LIMIT)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Filled in afterwards rather than joined in SQL, because matching `emails.from_addr` to a
    // contact means applying `contacts::normalize_address` — which strips a display name's angle
    // brackets as well as lowercasing. Writing that as `LOWER(TRIM(...))` in the query would be a
    // second, subtly different definition of the same rule, and it would disagree exactly for the
    // senders whose header carries a name. One definition, applied here.
    //
    // The set is every address whose contact carries a standing verdict — only what a human pinned
    // or muted, so it is small regardless of how much mail there is.
    let overrides: Vec<(String, String)> = sqlx::query_as(
        "SELECT addresses.address, overrides.verdict
           FROM contact_overrides AS overrides
           JOIN contact_addresses AS addresses
             ON addresses.contact_id = overrides.contact_id",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let by_address: std::collections::HashMap<String, String> = overrides.into_iter().collect();
    for message in &mut queue {
        message.sender_verdict = by_address
            .get(&crate::contacts::normalize_address(&message.from_addr))
            .cloned();
    }

    Ok(Json(queue))
}

async fn post_email_requeue(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    use crate::email::RequeueError;
    let pool = state.pool.clone();
    uncancellable(async move { crate::email::requeue(&pool, id).await })
        .await?
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| match error {
            RequeueError::UnknownEmail => StatusCode::NOT_FOUND,
            RequeueError::BodyPurged | RequeueError::ClaimedByRun(_) => StatusCode::CONFLICT,
        })
}

async fn post_assistant_message(
    State(state): State<AppState>,
    Json(body): Json<AssistantMessageRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Uncancellable for the same reason `create_run` is: `send_message` writes the turn's `running`
    // row and only then spawns the task that will finish it. A client that disconnects mid-request
    // drops this future exactly the way `abort()` drops a run's, and a drop landing between those
    // two leaves a `running` assistant row with no task and no abort handle — `/assistant/{id}`
    // reports it running forever and `/cancel` answers 404. The Telegram sidecar is the caller, and
    // it gives up on a turn after a timeout, so the disconnect is routine rather than theoretical.
    let outcome = uncancellable(async move {
        crate::assistant::send_message(&state, &body.chat_id, &body.text).await
    })
    .await?;

    match outcome {
        Ok(turn_id) => Ok(Json(serde_json::json!({ "turn_id": turn_id }))),
        Err(msg) if msg.contains("already in progress") => Err(StatusCode::CONFLICT),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn get_autopilot_state(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<AutopilotStateResponse>, StatusCode> {
    let mode = autopilot::project_mode(&state.pool, &query.project_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(AutopilotStateResponse {
        project_id: query.project_id,
        mode,
    }))
}

async fn post_autopilot_state(
    State(state): State<AppState>,
    Json(body): Json<AutopilotStateRequest>,
) -> Result<Json<AutopilotStateResponse>, StatusCode> {
    let mode = Mode::from_db_str(&body.mode).ok_or(StatusCode::BAD_REQUEST)?;
    let project_root = body.project_root.as_deref().map(std::path::Path::new);
    autopilot::set_project_mode(&state.pool, &body.project_id, mode, project_root)
        .await
        .map_err(activation_status)?;
    let mode = autopilot::project_mode(&state.pool, &body.project_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(AutopilotStateResponse {
        project_id: body.project_id,
        mode,
    }))
}

async fn get_autopilot_kill(
    State(state): State<AppState>,
) -> Result<Json<AutopilotKillResponse>, StatusCode> {
    let engaged = autopilot::kill_switch_engaged(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(AutopilotKillResponse { engaged }))
}

async fn post_autopilot_kill(
    State(state): State<AppState>,
    Json(body): Json<AutopilotKillRequest>,
) -> Result<StatusCode, StatusCode> {
    autopilot::set_kill_switch(&state.pool, body.engaged)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get_autopilot_kill_scoped(
    State(state): State<AppState>,
) -> Result<Json<Vec<ScopedKill>>, StatusCode> {
    autopilot::list_scoped_kills(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn post_autopilot_kill_scoped(
    State(state): State<AppState>,
    Json(body): Json<ScopedKillRequest>,
) -> Result<StatusCode, StatusCode> {
    if body.scope_type != "project" && body.scope_type != "trigger" {
        return Err(StatusCode::BAD_REQUEST);
    }
    autopilot::set_scoped_kill(&state.pool, &body.scope_type, &body.scope_id, body.engaged)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get_autopilot_budget(
    State(state): State<AppState>,
) -> Result<Json<BudgetResponse>, StatusCode> {
    budget_response(&state).await.map(Json)
}

async fn post_autopilot_budget(
    State(state): State<AppState>,
    Json(body): Json<BudgetRequest>,
) -> Result<Json<BudgetResponse>, StatusCode> {
    let period = budget::BudgetPeriod::from_db_str(&body.period).ok_or(StatusCode::BAD_REQUEST)?;
    let config = budget::BudgetConfig {
        limit_usd: body.limit_usd,
        period,
        hourly_limit_usd: body.hourly_limit_usd,
        per_run_reserve_usd: body.per_run_reserve_usd,
        time_cost_per_hour_usd: body.time_cost_per_hour_usd,
    };
    budget::set_budget_config(&state.pool, &config)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    budget_response(&state).await.map(Json)
}

async fn post_attention_heartbeat(
    State(state): State<AppState>,
    Json(body): Json<AttentionHeartbeatRequest>,
) -> Result<StatusCode, StatusCode> {
    let scope = match body.project_id {
        None => AttentionScope::Global,
        Some(project_id) if project_id.trim().is_empty() => return Err(StatusCode::BAD_REQUEST),
        Some(project_id) => AttentionScope::Project(project_id),
    };
    attention::record_heartbeat(&state.pool, &scope, chrono::Utc::now())
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn budget_response(state: &AppState) -> Result<BudgetResponse, StatusCode> {
    let now = chrono::Utc::now();
    let config = budget::load_budget_config(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let window_spend_usd = budget::window_spend(&state.pool, now)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let hourly_spend_usd = budget::hourly_spend(&state.pool, now)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (paused, reason) = match budget::budget_permits_new_run(&state.pool, now).await {
        budget::BudgetDecision::Allow => (false, None),
        budget::BudgetDecision::Pause { reason, .. } => (true, Some(reason)),
    };
    Ok(BudgetResponse {
        limit_usd: config.limit_usd,
        period: config.period.as_db_str().to_string(),
        hourly_limit_usd: config.hourly_limit_usd,
        per_run_reserve_usd: config.per_run_reserve_usd,
        time_cost_per_hour_usd: config.time_cost_per_hour_usd,
        window_spend_usd,
        hourly_spend_usd,
        paused,
        reason,
    })
}

fn activation_status(error: ActivationError) -> StatusCode {
    match error {
        ActivationError::NotAGitRepo
        | ActivationError::ProjectRootRequired
        | ActivationError::NotOnboarded
        | ActivationError::HookNotRegistered => StatusCode::UNPROCESSABLE_ENTITY,
        ActivationError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn inspect_status(error: inspect::InspectError) -> StatusCode {
    match error {
        inspect::InspectError::NotFound => StatusCode::NOT_FOUND,
        inspect::InspectError::UnsafePath => StatusCode::BAD_REQUEST,
        // The only one the caller cannot diagnose from the status code alone, so it is the only one
        // worth a line in the log.
        inspect::InspectError::Io(error) => {
            tracing::warn!(%error, "project inspection failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn resolve_project_root(state: &AppState, id: &str) -> Result<PathBuf, StatusCode> {
    match inspect::project_root(&state.pool, id).await {
        Ok(Some(root)) => Ok(PathBuf::from(root)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// One rule's recorded scheduler state, as the tick writes it.
///
/// Named rather than left as a five-element tuple because the fields are read by position in three
/// places here, and `fires_date` and `fires_today` are the pair whose meaning depends entirely on
/// being read together — a count carrying yesterday's date is a count of nothing.
#[derive(sqlx::FromRow)]
struct RuleRunState {
    rule_name: String,
    last_fired_at: String,
    last_head_sha: Option<String>,
    fires_date: Option<String>,
    fires_today: i64,
}

/// One scheduled rule, with what the daemon knows about it having run.
#[derive(serde::Serialize)]
struct ScheduleView {
    name: String,
    cron: String,
    prompt: String,
    cwd: Option<String>,
    timezone: Option<String>,
    /// When this fires next, counted from the last time it did — the same anchor the tick uses.
    next_fire_at: Option<String>,
    /// Why it will never fire, when that is the answer instead.
    ///
    /// An unparseable cron or an unknown timezone makes the tick skip the rule and log at debug,
    /// 2,880 times a day. The rule simply never runs and nothing says so; this is where that stops
    /// being invisible.
    problem: Option<String>,
    last_fired_at: Option<String>,
    /// How many times it has fired today, against the daemon's own per-rule daily cap.
    fires_today: i64,
    daily_cap: u32,
}

/// One repo trigger, with the commit it last saw.
#[derive(serde::Serialize)]
struct RepoTriggerView {
    name: String,
    branch: String,
    prompt: String,
    /// The SHA recorded the last time this trigger was evaluated. `null` means it is armed and has
    /// not yet seen a first commit to compare against — which fires nothing, by design.
    last_sha: Option<String>,
}

/// Everything a project will do without being asked, and everything currently holding it back.
#[derive(serde::Serialize)]
struct ProjectRules {
    project_id: String,
    project_root: Option<String>,
    /// `present`, `absent`, or `unreadable` — the three states `.ai/autopilot.yaml` can be in.
    rules_file: &'static str,
    /// Why the file could not be read, when it could not be.
    ///
    /// `config.rs` uses `deny_unknown_fields` precisely so a typo is an error rather than a silently
    /// empty ruleset — but that error only reached a log line, so writing `schedule:` for
    /// `schedules:` stopped all autonomy for the project and looked like nothing had happened.
    rules_error: Option<String>,
    gate_command: Option<String>,
    schedules: Vec<ScheduleView>,
    repo_triggers: Vec<RepoTriggerView>,
    /// The effective open-proposal ceiling: the project's own, else the global default. `null` means
    /// the brake is off.
    wip_limit: Option<i64>,
    open_proposals: i64,
    /// Whether that ceiling is currently refusing new autonomous work.
    queue_full: bool,
}

/// What a project does on its own.
///
/// The rules live in `.ai/autopilot.yaml` under the project root and were readable only by opening
/// the file; the WIP ceiling lives in the database and was readable only through the roster's
/// summary. Both decide whether autonomous work happens at all, so they answer one question and are
/// served together.
async fn get_project_rules(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectRules>, StatusCode> {
    let project_root: Option<String> =
        sqlx::query_scalar("SELECT project_root FROM autopilot_state WHERE project_id = ?")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .flatten();

    let (rules_file, rules_error, loaded) = match project_root.as_deref() {
        // A project with no root has no file to read, which is not a failure — it is what an `off`
        // project looks like, and reporting it as unreadable would name a fault where there is none.
        None => ("absent", None, crate::config::AutopilotRules::default()),
        Some(root) => match crate::config::load_schedule_rules(std::path::Path::new(root)) {
            Ok(rules) => {
                let path = std::path::Path::new(root)
                    .join(".ai")
                    .join("autopilot.yaml");
                let present = tokio::task::spawn_blocking(move || path.exists())
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                (if present { "present" } else { "absent" }, None, rules)
            }
            Err(error) => (
                "unreadable",
                Some(error.to_string()),
                crate::config::AutopilotRules::default(),
            ),
        },
    };

    let state_rows: Vec<RuleRunState> = sqlx::query_as(
        "SELECT rule_name, last_fired_at, last_head_sha, fires_date, fires_today
           FROM scheduler_state
          WHERE project_id = ?",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let now = chrono::Utc::now();
    let today = now.date_naive().to_string();
    // One table holds both kinds of rule, keyed by name, so schedules and repo triggers read their
    // recorded state out of the same map.
    let by_rule: std::collections::HashMap<&str, &RuleRunState> = state_rows
        .iter()
        .map(|row| (row.rule_name.as_str(), row))
        .collect();

    let schedules = loaded
        .schedules
        .iter()
        .map(|rule| {
            let recorded = by_rule.get(rule.name.as_str());
            let last_fired_at = recorded.map(|row| row.last_fired_at.clone());
            // Anchored on the last fire when there is one, exactly as the tick anchors it. Counting
            // from now instead would quietly skip a window that is already overdue, and show the run
            // due tomorrow when it is due this minute.
            let since = last_fired_at
                .as_deref()
                .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
                .map(|stamp| stamp.with_timezone(&chrono::Utc))
                .unwrap_or(now);
            let (next_fire_at, problem) = match crate::scheduler::next_fire(rule, since) {
                Ok(next) => (Some(next.to_rfc3339()), None),
                Err(problem) => (None, Some(problem)),
            };
            // A count carrying another day's date is a count of nothing — the daemon resets by
            // comparing rather than by sweeping at midnight, so this reads it the same way.
            let fires_today = recorded
                .filter(|row| row.fires_date.as_deref() == Some(today.as_str()))
                .map_or(0, |row| row.fires_today);
            ScheduleView {
                name: rule.name.clone(),
                cron: rule.cron.clone(),
                prompt: rule.prompt.clone(),
                cwd: rule.cwd.clone(),
                timezone: rule.timezone.clone(),
                next_fire_at,
                problem,
                last_fired_at,
                fires_today,
                daily_cap: crate::scheduler::DAILY_CAP,
            }
        })
        .collect();

    let repo_triggers = loaded
        .repo_triggers
        .iter()
        .map(|trigger| RepoTriggerView {
            name: trigger.name.clone(),
            branch: trigger.branch.clone(),
            prompt: trigger.prompt.clone(),
            last_sha: by_rule
                .get(trigger.name.as_str())
                .and_then(|row| row.last_head_sha.clone()),
        })
        .collect();

    let wip_limit = crate::wip::wip_limit(&state.pool, &id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let open_proposals = crate::wip::open_proposals(&state.pool, &id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ProjectRules {
        project_id: id,
        project_root,
        rules_file,
        rules_error,
        gate_command: loaded.gate_command.clone(),
        schedules,
        repo_triggers,
        wip_limit,
        open_proposals,
        queue_full: crate::wip::queue_full(open_proposals, wip_limit),
    }))
}

#[derive(Deserialize)]
struct WipLimitRequest {
    /// `null` switches the brake off for this project.
    limit: Option<i64>,
}

/// Sets one project's open-proposal ceiling.
///
/// The brake it controls is self-clearing — it releases the moment you review something — so the
/// number is the answer to "how much unreviewed work am I willing to be holding", and until now it
/// could only be changed with sqlite3. A negative ceiling is refused rather than stored: `queue_is_full`
/// compares `open >= limit`, so a negative one would mean "never start anything again" while reading
/// like a number somebody chose.
async fn post_project_wip_limit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<WipLimitRequest>,
) -> Result<StatusCode, StatusCode> {
    if body.limit.is_some_and(|limit| limit < 0) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let updated = sqlx::query("UPDATE autopilot_state SET wip_limit = ? WHERE project_id = ?")
        .bind(body.limit)
        .bind(&id)
        .execute(&state.pool)
        .await
        .map_err(|error| {
            tracing::warn!(project_id = %id, %error, "setting a project WIP limit failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    if updated.rows_affected() == 0 {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Every supervised sidecar and what has happened to it.
///
/// A sidecar that keeps failing to start was previously invisible: `sidecar.rs` restarts it with a
/// backoff and writes one warning per attempt to a log nobody reads while using the app. For the
/// email poller in particular that meant the Mail tab looked like a quiet mailbox — which is what an
/// empty inbox looks like too.
async fn get_sidecars() -> Json<Vec<crate::sidecar::SidecarState>> {
    Json(crate::sidecar::states())
}

/// The email pillar's settings, minus everything secret.
///
/// No password: it comes from Credential Manager, is handed to the sidecar process, and does not
/// pass through here. The host and account are named because "which mailbox is this" is the question
/// the rest of the panel's numbers are about.
#[derive(serde::Serialize)]
struct EmailConfigView {
    enabled: bool,
    /// True only once the hook barrier has been PROVEN at startup. Enabled but unarmed is a real
    /// state — the pillar owns retention either way — and it is why triage can be stopped while
    /// mail keeps arriving.
    armed: bool,
    host: String,
    username: String,
    mailbox: String,
    sent_mailbox: Option<String>,
    poll_interval_secs: u64,
    notify_classes: Vec<String>,
    digest_hour_utc: u8,
    retain_bodies_days: u8,
    /// Why local triage is unavailable when a local model was configured but could not be trusted.
    local_triage_disabled: Option<String>,
}

async fn get_email_config(State(state): State<AppState>) -> Json<EmailConfigView> {
    Json(EmailConfigView {
        enabled: state.email.enabled,
        armed: state.email.armed.load(std::sync::atomic::Ordering::Relaxed),
        host: state.email.host.clone(),
        username: state.email.username.clone(),
        mailbox: state.email.mailbox.clone(),
        sent_mailbox: state.email.sent_mailbox.clone(),
        poll_interval_secs: state.email.poll_interval_secs,
        notify_classes: state.email.notify_classes.clone(),
        digest_hour_utc: state.email.digest_hour_utc,
        retain_bodies_days: state.email.retain_bodies_days,
        local_triage_disabled: state.local_triage_disabled.clone(),
    })
}

async fn get_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProjectSummary>>, StatusCode> {
    autopilot::project_roster(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get_project_ls(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<Json<Vec<inspect::Entry>>, StatusCode> {
    let root = resolve_project_root(&state, &id).await?;
    let rel = query.path.unwrap_or_default();
    tokio::task::spawn_blocking(move || inspect::ls(&root, &rel))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(inspect_status)
}

async fn get_project_cat(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<String, StatusCode> {
    let root = resolve_project_root(&state, &id).await?;
    let rel = query.path.unwrap_or_default();
    tokio::task::spawn_blocking(move || inspect::cat(&root, &rel))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(inspect_status)
}

async fn get_project_grep(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<GrepQuery>,
) -> Result<Json<Vec<inspect::Match>>, StatusCode> {
    let root = resolve_project_root(&state, &id).await?;
    let q = query.q.unwrap_or_default();
    let rel = query.path.unwrap_or_default();
    tokio::task::spawn_blocking(move || inspect::grep(&root, &q, &rel))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(inspect_status)
}

async fn get_project_diff(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<String, StatusCode> {
    let root = resolve_project_root(&state, &id).await?;
    tokio::task::spawn_blocking(move || inspect::diff(&root))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map_err(inspect_status)
}

/// Runs `work` in its own task so a request that goes away cannot abandon it half-done.
///
/// A client that disconnects cancels the request it was making, and the handler's future is dropped
/// — the same mechanism `abort()` uses on a run's task, with the same consequence: everything
/// sequenced after the drop point is silently never done. That is only a missing reply when the
/// handler reads; when it mutates durable state across awaits, it strands the half it had finished,
/// and the half-states here (a `running` or `awaiting_approval` worktree run) block their whole
/// project through `one_open_worktree_run_per_project` (migration 0009).
///
/// Awaiting the JoinHandle leaves the response exactly as it was; dropping a JoinHandle only
/// detaches its task, so the work still runs to the end. A panicking task becomes a 500 — the task
/// is gone, so there is no result left to return.
pub(crate) async fn uncancellable<T, F>(work: F) -> Result<T, StatusCode>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    tokio::spawn(work)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(crate) fn create_run_status(error: &CreateRunError) -> StatusCode {
    match error {
        CreateRunError::Invalid(_) => StatusCode::BAD_REQUEST,
        CreateRunError::Busy => StatusCode::CONFLICT,
        CreateRunError::Worktree(_) | CreateRunError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The search endpoints never return more than this many rows, even when a caller requests more.
const SEARCH_LIMIT_MAX: i64 = 200;

fn parse_time_bound(
    value: Option<String>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, StatusCode> {
    value
        .map(|value| {
            chrono::DateTime::parse_from_rfc3339(&value)
                .map(|time| time.with_timezone(&chrono::Utc))
                .map_err(|_| StatusCode::BAD_REQUEST)
        })
        .transpose()
}

fn parse_search_limit(value: Option<String>) -> Result<i64, StatusCode> {
    match value {
        Some(value) => value
            .parse::<i64>()
            .map(|limit| limit.clamp(1, SEARCH_LIMIT_MAX))
            .map_err(|_| StatusCode::BAD_REQUEST),
        None => Ok(50),
    }
}

async fn get_feed(
    State(state): State<AppState>,
    Query(query): Query<FeedQuery>,
) -> Result<Json<Vec<FeedEntry>>, StatusCode> {
    let has_search_filters = query.q.is_some()
        || query.kind.is_some()
        || query.since.is_some()
        || query.until.is_some()
        || query.limit.is_some();
    if !has_search_filters {
        let entries = if query.scope.as_deref() == Some("all") {
            feed::list_all(&state.pool, 50).await
        } else {
            feed::list_feed(&state.pool, query.project_id.as_deref(), 50).await
        };
        return entries
            .map(Json)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
    }

    let scope = if query.scope.as_deref() == Some("all") {
        feed::FeedScope::All
    } else if let Some(project_id) = query.project_id {
        feed::FeedScope::Project(project_id)
    } else {
        feed::FeedScope::Global
    };
    let entries = feed::search(
        &state.pool,
        &feed::SearchFilter {
            scope,
            q: query.q,
            kind: query.kind,
            since: parse_time_bound(query.since)?,
            until: parse_time_bound(query.until)?,
            limit: parse_search_limit(query.limit)?,
        },
    )
    .await;
    entries
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get_runs(
    State(state): State<AppState>,
    Query(query): Query<RunsQuery>,
) -> Result<Json<Vec<runs::RunSearchResult>>, StatusCode> {
    runs::search(
        &state.pool,
        &runs::SearchFilter {
            project_id: query.project_id,
            status: query.status,
            mode: query.mode,
            q: query.q,
            since: parse_time_bound(query.since)?,
            until: parse_time_bound(query.until)?,
            limit: parse_search_limit(query.limit)?,
        },
    )
    .await
    .map(Json)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn preset_status(error: &presets::PresetError) -> StatusCode {
    match error {
        presets::PresetError::DuplicateName => StatusCode::CONFLICT,
        presets::PresetError::Invalid(_) => StatusCode::BAD_REQUEST,
        presets::PresetError::NotFound => StatusCode::NOT_FOUND,
        presets::PresetError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn list_presets(
    State(state): State<AppState>,
) -> Result<Json<Vec<presets::Preset>>, StatusCode> {
    presets::list(&state.pool).await.map(Json).map_err(|error| {
        tracing::warn!(%error, "listing presets failed");
        preset_status(&error)
    })
}

async fn create_preset(
    State(state): State<AppState>,
    Json(request): Json<presets::PresetRequest>,
) -> Result<Json<presets::Preset>, StatusCode> {
    presets::create(&state.pool, &request.name, request.run)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "creating preset failed");
            preset_status(&error)
        })
}

async fn get_preset(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<presets::Preset>, StatusCode> {
    presets::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(preset_id = id, %error, "reading preset failed");
            preset_status(&error)
        })?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn update_preset(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(request): Json<presets::PresetRequest>,
) -> Result<Json<presets::Preset>, StatusCode> {
    presets::update(&state.pool, id, &request.name, request.run)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(preset_id = id, %error, "updating preset failed");
            preset_status(&error)
        })
}

async fn delete_preset(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    presets::delete(&state.pool, id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| {
            tracing::warn!(preset_id = id, %error, "deleting preset failed");
            preset_status(&error)
        })
}

async fn run_preset(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<runs::CreateRunResponse>, StatusCode> {
    let preset = presets::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(preset_id = id, %error, "reading preset to run failed");
            preset_status(&error)
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Delegate to the sole person-initiated run front door. It owns the fail-closed global kill
    // switch, uncancellable launch window, and conversion from run-domain errors to HTTP status.
    runs::create_run(
        State(state),
        Json(runs::CreateRunRequest {
            prompt: preset.prompt,
            project_id: preset.project_id,
            cwd: preset.cwd,
            mode: preset.mode,
            // A preset records what to run, not who may speak into it afterwards, and there is no
            // column here that could say otherwise. Every preset already stored was written before
            // steering existed, so `false` is the answer each of them was saved with — a preset must
            // not become a way to obtain a listening run that its author never asked for.
            steerable: false,
        }),
    )
    .await
}

async fn list_awaiting_approval_runs(
    State(state): State<AppState>,
) -> Result<Json<Vec<AwaitingRun>>, StatusCode> {
    runs::list_awaiting_approval(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Deserialize)]
struct RunMessageRequest {
    message: String,
}

/// Delivers one mid-run turn to a live run that asked to be steerable.
///
/// Every condition is read from the run's own recorded facts, never from `run_messages` holding a
/// sender for it. A channel there says a process is listening; it does not say this run was ever
/// meant to be spoken to, and treating the two as the same thing would make the barrier below
/// depend on cleanup timing rather than on a decision anyone made.
///
/// The message is queued, not delivered: the run reads it when it next reads stdin, which is why this
/// answers 202 rather than 200. What is guaranteed by the time it returns is that the text reached
/// the run's own channel and nothing else's.
async fn post_run_message(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<RunMessageRequest>,
) -> Result<StatusCode, StatusCode> {
    let run = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT status, mode, steerable FROM runs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|error| {
        tracing::warn!(run_id = id, %error, "reading a run to steer failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let Some((status, mode, steerable)) = run else {
        return Err(StatusCode::NOT_FOUND);
    };

    // Opting in is what gives the CLI a stdin at all. A run without it has no channel to reach, so
    // the answer is no rather than text buffered for a process that will never read it.
    if steerable == 0 {
        return Err(StatusCode::CONFLICT);
    }
    // A finished run has no process left to tell. Accepting anyway would record an instruction
    // against a transcript that ended before it arrived, which reads afterwards as something the run
    // was told and ignored.
    if status != "running" {
        return Err(StatusCode::CONFLICT);
    }
    // Spec §5.5 from the other side. The pillar's premise is that text a stranger wrote never meets a
    // tool; steering adds a second author to a live session, and the one session that must never gain
    // an author is the one already holding a stranger's words. Kept as two questions — where the run
    // came from, and what it may touch — because they coincide only while there is one toolless mode.
    if mode == crate::email::TRIAGE_MODE {
        return Err(StatusCode::FORBIDDEN);
    }
    if runs::tool_policy_for_mode(&mode) == crate::runner::ToolPolicy::None {
        return Err(StatusCode::FORBIDDEN);
    }

    let sender = state.run_messages.lock().unwrap().get(&id).cloned();
    let Some(sender) = sender else {
        return Err(StatusCode::CONFLICT);
    };
    // Raw text: framing a turn as a `stream-json` line is `runner.rs`'s job, because knowing the
    // CLI's wire format is what that module is for. A second copy of that shape here would drift the
    // day the format does.
    sender
        .send(body.message)
        .map_err(|_| StatusCode::CONFLICT)?;
    Ok(StatusCode::ACCEPTED)
}

/// Says that the turn just sent was the last one, and is what lets a steerable run end at all.
///
/// A run launched with `--input-format stream-json` reads turns until stdin closes, and the daemon
/// holds that stdin open for as long as it holds the run's sender. Without a way to let go of it,
/// such a run could only stop by going silent long enough to trip its progress deadline — and be
/// recorded `timed_out`, a failure status, for having been left listening. Closing the channel gives
/// the CLI its EOF, so the turn ends the way an ordinary run's does and the run is recorded on what
/// it actually did.
///
/// DELETE, and idempotent with it: a channel that is already closed is the state the caller asked
/// for, not a conflict. It answers 204 for any run that exists, whether or not that run was ever
/// listening, because "no such channel" is also what a run that finished a moment ago looks like —
/// and a caller that had to tell those two apart would be handling a race instead of ending a
/// conversation.
///
/// No steering barrier here, deliberately, and it does not narrow the one on `post_run_message`.
/// That barrier exists because steering ADDS an author to a live session; this takes nothing from
/// the caller and puts nothing in the run's context. The only thing it can do is end a conversation,
/// and there is no run for which ending one is the unsafe direction.
async fn delete_run_message(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    let known = sqlx::query_scalar::<_, i64>("SELECT 1 FROM runs WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|error| {
            tracing::warn!(run_id = id, %error, "reading a run to close its turns failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    if known.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    runs::close_steering_channel(&state, id);
    Ok(StatusCode::NO_CONTENT)
}

/// One exchange in a chat, as it is read back.
///
/// The reply is the run's stdout and the failure its stderr, which is what the shell was already
/// pulling out of the run detail — carried here so reopening a conversation does not mean one
/// request per turn.
#[derive(serde::Serialize, sqlx::FromRow)]
struct AssistantTurn {
    id: i64,
    asked: String,
    answer: Option<String>,
    error: Option<String>,
    status: String,
    cost_usd: Option<f64>,
    created_at: String,
}

/// How many turns of a conversation are read back. A chat is read from its recent end.
const ASSISTANT_TRANSCRIPT_LIMIT: i64 = 100;

/// A chat's turns, oldest first.
///
/// The daemon has always kept these — a turn is a run — but nothing on the row said which chat it
/// belonged to, so the shell's transcript could only live in the window that made it and died with
/// a reload. Reconstructing it from `/runs?mode=assistant` was never an option: that is every
/// chat's turns at once, the Telegram sidecar's included.
///
/// Ordered by id rather than by `created_at`: two turns of the same conversation can share a
/// timestamp to the second, and a transcript that reorders itself is one you lose your place in.
/// The limit takes the LAST turns and then puts them back in order, so a long conversation opens on
/// its recent end rather than on its beginning.
async fn get_assistant_chat(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<Json<Vec<AssistantTurn>>, StatusCode> {
    let mut turns = sqlx::query_as::<_, AssistantTurn>(
        "SELECT id, prompt AS asked, stdout AS answer, stderr AS error, status, cost_usd,
                created_at
           FROM runs
          WHERE chat_id = ? AND mode = 'assistant'
          ORDER BY id DESC
          LIMIT ?",
    )
    .bind(&chat_id)
    .bind(ASSISTANT_TRANSCRIPT_LIMIT)
    .fetch_all(&state.pool)
    .await
    .map_err(|error| {
        tracing::warn!(%error, "reading an assistant chat failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    turns.reverse();
    Ok(Json(turns))
}

async fn get_proposals(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::proposals::Proposal>>, StatusCode> {
    crate::proposals::list_pending(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Turns a merge decision's outcome into the answer the caller gets.
///
/// The conflict is a 409 with a body, not a bare status: it is the one refusal here that names
/// something the person can go and change, and a status code cannot say which two instructions
/// disagree.
fn merge_decision_response(
    outcome: Result<crate::contacts::MergeOutcome, crate::contacts::DecisionError>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match outcome {
        Ok(crate::contacts::MergeOutcome::Merged) => {
            Ok(Json(serde_json::json!({ "merged": true })))
        }
        Ok(crate::contacts::MergeOutcome::RefusedConflictingVerdicts { keep, absorb }) => {
            tracing::info!(%keep, %absorb, "refused a contact merge with conflicting verdicts");
            Err(StatusCode::CONFLICT)
        }
        Err(crate::contacts::DecisionError::NotPending) => Err(StatusCode::CONFLICT),
        Err(crate::contacts::DecisionError::Db(error)) => {
            tracing::warn!(%error, "deciding a contact merge failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn post_proposal_approve(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Two kinds of proposal share this table and this door, and they are decided by entirely
    // different machinery: an action approval resumes a paused run, a contact merge joins two
    // people and touches no run at all. Reading the kind first is only a dispatch — the kind never
    // changes, and both paths below are compare-and-set on `status = 'pending'`, so a second
    // decision racing this one still loses there rather than here.
    let kind = crate::proposals::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "reading a proposal to approve failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?
        .kind;
    if kind == "contact-merge" {
        // Uncancellable for the same reason the resume below is: the decision commits, and a
        // request dropped mid-flight must not leave the record disagreeing with what happened.
        let state = state.clone();
        let outcome =
            uncancellable(async move { crate::contacts::approve_merge(&state.pool, id).await })
                .await?;
        return merge_decision_response(outcome);
    }
    if kind == "calendar-event" {
        // Third kind through this door, and the second that starts no run: approving writes the
        // event and the decision in one transaction. Uncancellable for the same reason as the
        // other two — a dropped request must not leave the proposal and the calendar disagreeing.
        let state = state.clone();
        let created =
            uncancellable(
                async move { crate::calendar::approve_proposed_event(&state.pool, id).await },
            )
            .await?;
        return match created {
            Ok(event_id) => Ok(Json(serde_json::json!({ "event_id": event_id }))),
            Err(crate::calendar::DecisionError::NotFound) => Err(StatusCode::NOT_FOUND),
            Err(crate::calendar::DecisionError::NotPending) => Err(StatusCode::CONFLICT),
            Err(crate::calendar::DecisionError::Malformed) => {
                tracing::warn!(
                    proposal_id = id,
                    "a calendar proposal carried no usable event"
                );
                Err(StatusCode::UNPROCESSABLE_ENTITY)
            }
            Err(crate::calendar::DecisionError::Db(error)) => {
                tracing::warn!(proposal_id = id, %error, "approving a calendar proposal failed");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        };
    }

    // Uncancellable: the approval commits a transaction and only then spawns the resumed run, so a
    // request dropped in between leaves a `running` run nothing will ever drive.
    match uncancellable(async move { crate::runs::resume_approved_run(&state, id).await }).await? {
        Ok(resume_id) => Ok(Json(serde_json::json!({ "resume_run_id": resume_id }))),
        Err(crate::runs::ResumeError::ProposalNotFound) => Err(StatusCode::NOT_FOUND),
        Err(crate::runs::ResumeError::ProposalNotPending) => Err(StatusCode::CONFLICT),
        // A 409 alone cannot say which precondition failed, and these are the ones a human has to
        // act on — an approval that will not resume looks identical to one nobody clicked.
        Err(crate::runs::ResumeError::NotResumable(reason)) => {
            tracing::warn!(
                proposal_id = id,
                reason,
                "approved proposal is not resumable"
            );
            Err(StatusCode::CONFLICT)
        }
        Err(crate::runs::ResumeError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "approving a proposal failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn post_proposal_reject(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    let kind = crate::proposals::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "reading a proposal to reject failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?
        .kind;
    if kind == "contact-merge" {
        // `reject_merge` records the refused pair in the same transaction as the status, which is
        // what stops the heuristic asking the identical question forever. Wiring the approval
        // without this would have been worse than wiring neither: the suggestion you refused would
        // come back on every sweep, and a suggestion that ignores your answer is not a suggestion.
        let state = state.clone();
        let rejected =
            uncancellable(async move { crate::contacts::reject_merge(&state.pool, id).await })
                .await?;
        return match rejected {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            // The guarded SELECT and the compare-and-set both report a proposal that is no longer
            // pending this way; either means another decision got there first.
            Err(sqlx::Error::RowNotFound) => Err(StatusCode::CONFLICT),
            Err(error) => {
                tracing::warn!(proposal_id = id, %error, "rejecting a contact merge failed");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        };
    }

    if kind == "calendar-event" {
        // Refusing a suggested block leaves nothing behind: it is a suggestion declined, not a
        // meeting cancelled, so the calendar never learns it was offered.
        let state = state.clone();
        let rejected =
            uncancellable(
                async move { crate::calendar::reject_proposed_event(&state.pool, id).await },
            )
            .await?;
        return match rejected {
            Ok(()) => Ok(StatusCode::NO_CONTENT),
            Err(crate::calendar::DecisionError::NotFound) => Err(StatusCode::NOT_FOUND),
            Err(crate::calendar::DecisionError::NotPending) => Err(StatusCode::CONFLICT),
            Err(crate::calendar::DecisionError::Malformed) => Err(StatusCode::UNPROCESSABLE_ENTITY),
            Err(crate::calendar::DecisionError::Db(error)) => {
                tracing::warn!(proposal_id = id, %error, "rejecting a calendar proposal failed");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        };
    }

    // Uncancellable: rejecting flips the proposal first and discards the paused run second, and the
    // first half cannot be replayed — a retry finds the proposal no longer `pending` and answers 409.
    match uncancellable(async move { crate::proposals::reject_proposal(&state.pool, id).await })
        .await?
    {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(crate::proposals::RejectError::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(crate::proposals::RejectError::NotPending) => Err(StatusCode::CONFLICT),
        Err(crate::proposals::RejectError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "rejecting a proposal failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// How many jobs one listing returns.
///
/// Finished jobs are included, so this is a window rather than a queue: a job that stopped for the
/// budget or ran out of clock is exactly the one the user needs to see, and filtering to live ones
/// would make it vanish at the moment it started mattering.
const JOB_LIST_LIMIT: i64 = 20;

#[derive(serde::Deserialize)]
struct OptionalProjectQuery {
    project_id: Option<String>,
}

async fn get_jobs(
    State(state): State<AppState>,
    Query(query): Query<OptionalProjectQuery>,
) -> Result<Json<Vec<crate::job::JobSummary>>, StatusCode> {
    crate::job::list(&state.pool, query.project_id.as_deref(), JOB_LIST_LIMIT)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<crate::job::JobDetail>, StatusCode> {
    match crate::job::detail(&state.pool, id).await {
        Ok(Some(detail)) => Ok(Json(detail)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn cancel_job(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    // Uncancellable: this terminates the node in flight and only then retires the job. A request
    // dropped in between would leave a job nothing drives with a node still running inside it.
    match uncancellable(async move { crate::job::cancel(&state, id).await }).await? {
        Ok(crate::job::CancelOutcome::Cancelled) => Ok(StatusCode::NO_CONTENT),
        // Already over. Not success: "I stopped it" and "it had already finished" are different
        // answers to the question the user just asked.
        Ok(crate::job::CancelOutcome::NotLive) => Err(StatusCode::CONFLICT),
        Ok(crate::job::CancelOutcome::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn post_worktree_release(
    State(state): State<AppState>,
    Path(run_id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    // Uncancellable, and the widest window of the three: `git worktree remove` retries on a backoff
    // that can run for half a minute before the run is finally marked `cancelled`.
    match uncancellable(async move { worktree::release(&state.pool, run_id).await }).await? {
        Ok(ReleaseOutcome::Released) => Ok(StatusCode::NO_CONTENT),
        Ok(ReleaseOutcome::NotAwaitingApproval) => Err(StatusCode::CONFLICT),
        Ok(ReleaseOutcome::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn get_unreviewed_shadow_decisions(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<Vec<ShadowDecision>>, StatusCode> {
    shadow::list_unreviewed(&state.pool, &query.project_id)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn post_shadow_verdict(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<VerdictRequest>,
) -> Result<StatusCode, StatusCode> {
    if !matches!(body.verdict.as_str(), "approve" | "reject") {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Readiness only ever moves when a human reviews a decision, so this is the one place a project
    // can cross the promotion bar — sample it either side of the verdict to catch the crossing.
    let project = shadow::project_of_decision(&state.pool, id)
        .await
        .unwrap_or(None);
    let was_promotable = match &project {
        Some(project_id) => shadow::project_readiness(&state.pool, project_id)
            .await
            .map(|(ready, total, withheld)| shadow::promotable(ready, total, withheld))
            .unwrap_or(false),
        None => false,
    };

    // Uncancellable: the verdict lands first and the crossing is announced second, and a verdict is
    // recorded once — replaying it answers 404, so an announcement dropped in between is lost for
    // good, and the promotion bar is the one thing this endpoint exists to surface.
    let pool = state.pool.clone();
    let verdict = body.verdict.clone();
    uncancellable(async move {
        match shadow::set_verdict(&pool, id, &verdict).await {
            Ok(true) => {
                if let Some(project_id) = project {
                    announce_promotable(&pool, &project_id, was_promotable).await;
                }
                Ok(StatusCode::NO_CONTENT)
            }
            Ok(false) => Err(StatusCode::NOT_FOUND),
            Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        }
    })
    .await?
}

/// The §8.2 promotion nudge: a feed entry the moment a project's last outstanding action class
/// clears the bar. Without it the gate solves promotion-by-impatience but leaves the opposite
/// failure — a project that quietly became promotable and nobody noticed.
///
/// Best-effort and idempotent by construction: it fires only on the false→true crossing, so a
/// project already promotable before the verdict stays silent. Feed failures never fail the verdict.
async fn announce_promotable(pool: &sqlx::SqlitePool, project_id: &str, was_promotable: bool) {
    if was_promotable {
        return;
    }
    let Ok((ready, total, withheld)) = shadow::project_readiness(pool, project_id).await else {
        return;
    };
    if !shadow::promotable(ready, total, withheld) {
        return;
    }

    let summary = format!(
        "{project_id} is ready for promotion — all {total} reviewed action classes clear the bar \
         ({}+ reviews, {}%+ agreement). Promotion is still yours to make.",
        shadow::READINESS_MIN_REVIEWED,
        shadow::READINESS_MIN_AGREE_PERCENT,
    );
    let _ = feed::append(pool, Some(project_id), "promotion_ready", &summary, None).await;
}

async fn get_scoreboard(
    State(state): State<AppState>,
    Query(query): Query<ProjectQuery>,
) -> Result<Json<Vec<ClassTally>>, StatusCode> {
    shadow::scoreboard(&state.pool, &query.project_id)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Token;
    use crate::proposals;
    use crate::runner::FakeCommandRunner;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn file_test_state() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open(&dir.path().join("nucleos.db"))
            .await
            .unwrap();
        (
            AppState {
                token: Token("test-token".into()),
                pool,
                runner: Arc::new(FakeCommandRunner::default()),
                triage_runner: None,
                local_triage_disabled: None,
                run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
                voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
                calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
                run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
                progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            },
            dir,
        )
    }

    async fn backup_request(
        state: AppState,
        method: &str,
        uri: &str,
        token: Option<&str>,
    ) -> axum::response::Response {
        let mut request = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        build_router(state)
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn every_backup_route_requires_the_bearer_token() {
        for (method, uri) in [
            ("POST", "/backup"),
            ("GET", "/backups"),
            (
                "POST",
                "/backups/nucleos-20260729T010203.000000000Z-0000.db/restore",
            ),
        ] {
            let response = backup_request(test_state().await, method, uri, None).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
        }
    }

    #[tokio::test]
    async fn restore_route_rejects_traversal_and_path_separators() {
        for name in ["bad..name.db", "bad%5Cname.db"] {
            let response = backup_request(
                test_state().await,
                "POST",
                &format!("/backups/{name}/restore"),
                Some("test-token"),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
        }
    }

    #[tokio::test]
    async fn backups_route_lists_newest_first() {
        let (state, dir) = file_test_state().await;
        let backup_dir = dir.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let older = "nucleos-20260729T010203.000000000Z-0000.db";
        let newer = "nucleos-20260729T020203.000000000Z-0000.db";
        std::fs::write(backup_dir.join(older), b"old").unwrap();
        std::fs::write(backup_dir.join(newer), b"new").unwrap();

        let response = backup_request(state.clone(), "GET", "/backups", Some("test-token")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let listed: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(listed[0]["name"], newer);
        assert_eq!(listed[1]["name"], older);

        state.pool.close().await;
        drop(dir);
    }

    async fn test_state() -> AppState {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        AppState {
            token: Token("test-token".into()),
            pool,
            runner: Arc::new(FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    #[tokio::test]
    async fn oversized_webhook_body_is_refused() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/push")
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .body(Body::from(vec![
                        b'x';
                        crate::webhook::WEBHOOK_BODY_LIMIT + 1
                    ]))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    async fn api_token_request(
        state: AppState,
        method: &str,
        uri: &str,
        token: &str,
        body: Option<serde_json::Value>,
    ) -> axum::response::Response {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", format!("Bearer {token}"));
        let body = match body {
            Some(body) => {
                request = request.header("Content-Type", "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        build_router(state)
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn admin_api_token_routes_create_list_once_and_revoke() {
        let state = test_state().await;
        let response = api_token_request(
            state.clone(),
            "POST",
            "/api-tokens",
            "test-token",
            Some(serde_json::json!({
                "name": "administrator",
                "level": "admin"
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let admin_token = created["token"].as_str().unwrap().to_owned();
        assert_eq!(created["name"], "administrator");
        assert_eq!(created["level"], "admin");

        let stored_secret: String =
            sqlx::query_scalar("SELECT token FROM api_tokens WHERE name = 'administrator'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(admin_token.split_once('.').unwrap().1, stored_secret);

        let response = api_token_request(
            state.clone(),
            "POST",
            "/api-tokens",
            &admin_token,
            Some(serde_json::json!({
                "name": "reader",
                "level": "read-only"
            })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let reader: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let reader_token = reader["token"].as_str().unwrap().to_owned();

        let response =
            api_token_request(state.clone(), "GET", "/api-tokens", &reader_token, None).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response =
            api_token_request(state.clone(), "GET", "/api-tokens", &admin_token, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let listed: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(listed.len(), 2);
        assert!(
            listed.iter().all(|entry| entry.get("token").is_none()),
            "listing existing keys must never return their secrets"
        );
        assert!(
            listed
                .iter()
                .any(|entry| entry["name"] == "reader" && entry["level"] == "read-only")
        );

        let response = api_token_request(
            state.clone(),
            "DELETE",
            "/api-tokens/reader",
            &admin_token,
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let response = api_token_request(state, "GET", "/status", &reader_token, None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    async fn store_api_token_at_level(
        state: &AppState,
        name: &str,
        level: ApiTokenLevel,
    ) -> String {
        let (token, secret) = mint_api_token(name);
        sqlx::query(
            "INSERT INTO api_tokens (name, token, access_level, created_at)
             VALUES (?, ?, ?, '2026-07-29T12:00:00Z')",
        )
        .bind(name)
        .bind(secret)
        .bind(level.as_str())
        .execute(&state.pool)
        .await
        .unwrap();
        token
    }

    #[tokio::test]
    async fn attention_heartbeat_requires_a_bearer_and_records_the_requested_scope() {
        let state = test_state().await;
        let response = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/autopilot/attention")
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = api_token_request(
            state.clone(),
            "POST",
            "/autopilot/attention",
            "test-token",
            Some(serde_json::json!({ "project_id": "project-a" })),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let stored: (String, String) = sqlx::query_as(
            "SELECT scope, project_id FROM attention_heartbeats WHERE project_id = 'project-a'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(stored, ("project".to_string(), "project-a".to_string()));
    }

    #[tokio::test]
    async fn only_control_and_admin_tokens_may_post_attention_heartbeats() {
        let state = test_state().await;
        let read_only = store_api_token_at_level(&state, "reader", ApiTokenLevel::ReadOnly).await;
        let run_creating =
            store_api_token_at_level(&state, "launcher", ApiTokenLevel::RunCreating).await;
        let admin = store_api_token_at_level(&state, "administrator", ApiTokenLevel::Admin).await;

        for token in [&read_only, &run_creating] {
            let response = api_token_request(
                state.clone(),
                "POST",
                "/autopilot/attention",
                token,
                Some(serde_json::json!({})),
            )
            .await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }

        let response = api_token_request(
            state,
            "POST",
            "/autopilot/attention",
            &admin,
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    fn email_batch(messages: serde_json::Value) -> Body {
        Body::from(
            serde_json::json!({
                "mailbox": "INBOX",
                "uidvalidity": 1,
                "max_uid_examined": 10,
                "messages": messages,
            })
            .to_string(),
        )
    }

    fn email_batch_directed(direction: serde_json::Value, messages: serde_json::Value) -> Body {
        Body::from(
            serde_json::json!({
                "mailbox": "INBOX",
                "uidvalidity": 1,
                "max_uid_examined": 10,
                "direction": direction,
                "messages": messages,
            })
            .to_string(),
        )
    }

    fn email_batch_from(
        mailbox: &str,
        direction: serde_json::Value,
        messages: serde_json::Value,
    ) -> Body {
        Body::from(
            serde_json::json!({
                "mailbox": mailbox,
                "uidvalidity": 1,
                "max_uid_examined": 10,
                "direction": direction,
                "messages": messages,
            })
            .to_string(),
        )
    }

    fn one_message() -> serde_json::Value {
        serde_json::json!([{
            "message_id": "<a@b>",
            "uid": 10,
            "from_addr": "ana@company.com",
            "received_at": "2026-07-28T11:00:00+00:00",
            "body_text": "hello",
        }])
    }

    async fn post_email(state: AppState, token: Option<&str>, body: Body) -> StatusCode {
        let mut request = Request::builder()
            .method("POST")
            .uri("/email/incoming")
            .header("Content-Type", "application/json");
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        build_router(state)
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn email_ingestion_requires_the_bearer_token() {
        let state = test_state().await;
        assert_eq!(
            post_email(state, None, email_batch(one_message())).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn a_malformed_email_batch_is_rejected() {
        let state = test_state().await;
        assert_eq!(
            post_email(
                state,
                Some("test-token"),
                Body::from(r#"{"mailbox":"INBOX"}"#)
            )
            .await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    /// The shape the sidecar actually sends, not the one the fixtures invented. Go writes an empty
    /// `[]Skipped` as `null`, and the first real inbox met a 422 that every earlier test had
    /// missed because `email_batch` omits the field entirely — a nil slice and an absent key look
    /// alike in Rust and are different bytes on the wire.
    #[tokio::test]
    async fn a_batch_with_null_lists_is_accepted() {
        let state = test_state().await;
        let body = Body::from(
            serde_json::json!({
                "mailbox": "INBOX",
                "uidvalidity": 1,
                "max_uid_examined": 10,
                "skipped": serde_json::Value::Null,
                "messages": one_message(),
            })
            .to_string(),
        );
        assert_eq!(
            post_email(state, Some("test-token"), body).await,
            StatusCode::OK
        );
    }

    /// A poll that read nothing but examined uids still has to land, or the cursor never moves past
    /// mail the sidecar decided about.
    #[tokio::test]
    async fn a_batch_with_no_messages_at_all_is_accepted() {
        let state = test_state().await;
        let body = Body::from(
            serde_json::json!({
                "mailbox": "INBOX",
                "uidvalidity": 1,
                "max_uid_examined": 10,
                "skipped": serde_json::Value::Null,
                "messages": serde_json::Value::Null,
            })
            .to_string(),
        );
        assert_eq!(
            post_email(state, Some("test-token"), body).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn um_lote_marcado_de_saida_e_gravado_como_saida() {
        let state = test_state().await;
        let pool = state.pool.clone();

        assert_eq!(
            post_email(
                state,
                Some("test-token"),
                email_batch_directed(serde_json::json!("outbound"), one_message())
            )
            .await,
            StatusCode::OK
        );

        let stored = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT direction, body_text FROM emails",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, ("outbound".to_owned(), None));
    }

    #[tokio::test]
    async fn um_lote_sem_direccao_continua_a_ser_entrada() {
        let state = test_state().await;
        let pool = state.pool.clone();

        assert_eq!(
            post_email(state, Some("test-token"), email_batch(one_message())).await,
            StatusCode::OK
        );

        let stored = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT direction, body_text FROM emails",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, ("inbound".to_owned(), Some("hello".to_owned())));
    }

    #[tokio::test]
    async fn uma_direccao_vazia_e_lida_como_entrada() {
        let state = test_state().await;
        let pool = state.pool.clone();

        assert_eq!(
            post_email(
                state,
                Some("test-token"),
                email_batch_directed(serde_json::json!(""), one_message())
            )
            .await,
            StatusCode::OK
        );

        let stored = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT direction, body_text FROM emails",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, ("inbound".to_owned(), Some("hello".to_owned())));
    }

    #[tokio::test]
    async fn uma_direccao_nula_e_lida_como_entrada() {
        let state = test_state().await;
        let pool = state.pool.clone();

        assert_eq!(
            post_email(
                state,
                Some("test-token"),
                email_batch_directed(serde_json::Value::Null, one_message())
            )
            .await,
            StatusCode::OK
        );

        let stored = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT direction, body_text FROM emails",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, ("inbound".to_owned(), Some("hello".to_owned())));
    }

    #[tokio::test]
    async fn uma_direccao_desconhecida_e_recusada_sem_gravar() {
        let state = test_state().await;
        let pool = state.pool.clone();

        assert_eq!(
            post_email(
                state,
                Some("test-token"),
                email_batch_directed(serde_json::json!("sideways"), one_message())
            )
            .await,
            StatusCode::BAD_REQUEST
        );

        let stored = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM emails")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, 0);
    }

    /// Paging is the sidecar's job (C7T3). A batch past the ceiling means it stopped doing it, and
    /// the núcleo says so instead of ingesting whatever arrives.
    #[tokio::test]
    async fn an_oversized_email_batch_is_rejected() {
        let state = test_state().await;
        let messages: Vec<serde_json::Value> = (0..=MAX_MESSAGES_PER_BATCH)
            .map(|i| {
                serde_json::json!({
                    "message_id": format!("<m{i}@x>"),
                    "uid": i,
                    "from_addr": "ana@company.com",
                    "received_at": "2026-07-28T11:00:00+00:00",
                })
            })
            .collect();
        assert_eq!(
            post_email(
                state,
                Some("test-token"),
                email_batch(serde_json::Value::Array(messages))
            )
            .await,
            StatusCode::BAD_REQUEST
        );
    }

    /// A FULL batch is legitimate — a first sync of a busy mailbox is exactly this shape — and it
    /// has to survive the transport, not just the handler. At the sidecar's own ceilings (200
    /// messages of up to 32 KiB) the JSON runs to megabytes, well past axum's 2 MB default; the
    /// 413 that produced stopped the cursor from advancing, so the identical oversized batch came
    /// back every five minutes, forever. `MAX_MESSAGES_PER_BATCH` never even ran: the body was
    /// rejected before the handler saw it.
    #[tokio::test]
    async fn a_full_batch_of_large_messages_is_accepted() {
        let state = test_state().await;
        // 200 x ~32 KiB of body, i.e. the largest batch the sidecar is allowed to send.
        let body_text = "x".repeat(32 * 1024);
        let messages: Vec<serde_json::Value> = (0..MAX_MESSAGES_PER_BATCH)
            .map(|i| {
                serde_json::json!({
                    "message_id": format!("<big{i}@x>"),
                    "uid": i,
                    "from_addr": "ana@company.com",
                    "received_at": "2026-07-28T11:00:00+00:00",
                    "body_text": body_text,
                })
            })
            .collect();

        assert_eq!(
            post_email(
                state,
                Some("test-token"),
                email_batch(serde_json::Value::Array(messages))
            )
            .await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn a_valid_email_batch_reports_what_it_did() {
        let state = test_state().await;
        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/email/incoming")
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .body(email_batch(one_message()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["ingested"], 1);
        assert_eq!(body["duplicates"], 0);
        assert_eq!(body["cursor"], 10);
    }

    async fn get_queue(state: AppState) -> Vec<serde_json::Value> {
        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/email/queue")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn a_caixa_nao_mostra_o_que_o_utilizador_escreveu() {
        let state = test_state().await;
        let outbound = serde_json::json!([{
            "message_id": "<sent@user>",
            "uid": 10,
            "from_addr": "utilizador@example.com",
            "received_at": "2026-07-28T10:00:00+00:00",
            "body_text": "Resposta enviada",
            "headers": {"to": "destinatario@example.com"},
        }]);
        let inbound = serde_json::json!([{
            "message_id": "<received@contact>",
            "uid": 9,
            "from_addr": "remetente@example.com",
            "received_at": "2026-07-28T11:00:00+00:00",
            "body_text": "Pedido recebido",
        }]);

        assert_eq!(
            post_email(
                state.clone(),
                Some("test-token"),
                email_batch_from("Sent", serde_json::json!("outbound"), outbound)
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            post_email(
                state.clone(),
                Some("test-token"),
                email_batch_directed(serde_json::json!("inbound"), inbound)
            )
            .await,
            StatusCode::OK
        );

        let queue = get_queue(state).await;
        let senders: Vec<&str> = queue
            .iter()
            .map(|mail| mail["from_addr"].as_str().unwrap())
            .collect();
        assert_eq!(senders, vec!["remetente@example.com"]);
    }

    /// A mailbox reads newest-arrival-first, and a verdict does not move a message.
    ///
    /// The previous ordering floated waiting mail to the top, which meant a message classified for
    /// free a second ago sank below one that arrived an hour earlier — the list rearranged itself
    /// while you were reading it. Ordering by arrival is the only order that holds still.
    #[tokio::test]
    async fn the_queue_is_ordered_by_arrival_newest_first() {
        let state = test_state().await;
        let now = chrono::Utc::now();
        let stamp = |minutes: i64| (now - chrono::Duration::minutes(minutes)).to_rfc3339();

        // Delivered oldest-first, the way a mailbox hands them over, so a correct result cannot
        // come from insertion order by accident.
        let body = Body::from(
            serde_json::json!({
                "mailbox": "INBOX",
                "uidvalidity": 1,
                "max_uid_examined": 3,
                "messages": [
                    {"message_id": "<old@x>", "uid": 1, "from_addr": "ana@company.com",
                     "received_at": stamp(120), "body_text": "oldest"},
                    {"message_id": "<mid@x>", "uid": 2, "from_addr": "bea@company.com",
                     "received_at": stamp(60), "body_text": "still waiting"},
                    // Newest AND classified on arrival — the case the old ordering got backwards.
                    {"message_id": "<new@x>", "uid": 3, "from_addr": "news@list.com",
                     "received_at": stamp(1), "body_text": "newest",
                     "headers": {"list-unsubscribe": "<https://list.com/u>"}},
                ],
            })
            .to_string(),
        );
        assert_eq!(
            post_email(state.clone(), Some("test-token"), body).await,
            StatusCode::OK
        );

        let queue = get_queue(state).await;
        let senders: Vec<&str> = queue
            .iter()
            .map(|mail| mail["from_addr"].as_str().unwrap())
            .collect();
        assert_eq!(
            senders,
            vec!["news@list.com", "bea@company.com", "ana@company.com"]
        );
        assert_eq!(queue[0]["triage_class"], "noise");
        assert!(
            queue[1]["triage_class"].is_null(),
            "the hour-old message should still be waiting, and still second"
        );
    }

    fn with_files_root(state: AppState, root: std::path::PathBuf) -> AppState {
        let mut email = (*state.email).clone();
        email.files_root = root;
        AppState {
            email: std::sync::Arc::new(email),
            ..state
        }
    }

    async fn call(
        state: AppState,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", "Bearer test-token");
        let body = match body {
            Some(value) => {
                request = request.header("Content-Type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = build_router(state)
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    /// A root that startup could not create means the routes refuse, rather than quietly writing
    /// somewhere else on disk.
    #[tokio::test]
    async fn the_folder_routes_refuse_when_there_is_no_root() {
        let state = test_state().await;
        assert_eq!(
            call(state.clone(), "GET", "/files", None).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        // The four that write, or hand bytes back, refuse for the same reason and must not be
        // reachable in a state where nobody knows where they would be writing.
        assert_eq!(
            call(state.clone(), "GET", "/files/download?path=x", None)
                .await
                .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            call(
                state.clone(),
                "POST",
                "/files/move",
                Some(serde_json::json!({"from": "a", "to": "b"}))
            )
            .await
            .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            call(state.clone(), "DELETE", "/files?path=x", None).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            call(state.clone(), "GET", "/files/search?q=x", None)
                .await
                .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            upload(state, "", "guia.docx", b"x").await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    /// Filing checks where it would write BEFORE it fetches anything. The order is the test: no
    /// sidecar is running here, so reaching the mailbox first would answer 502 and, worse, would
    /// mean the mailbox is read for a write that was never going to happen.
    #[tokio::test]
    async fn filing_checks_the_root_before_it_touches_the_mailbox() {
        let state = test_state().await;
        for uri in [
            "/email/1/attachments/0/save",
            "/email/1/attachments/save-all",
        ] {
            assert_eq!(
                call(
                    state.clone(),
                    "POST",
                    uri,
                    Some(serde_json::json!({"folder": ""}))
                )
                .await
                .0,
                StatusCode::SERVICE_UNAVAILABLE,
                "{uri}"
            );
        }
    }

    /// The literal `save-all` must keep winning over `{position}`, or filing everything starts
    /// trying to file an attachment numbered "save-all".
    #[tokio::test]
    async fn the_bulk_route_is_not_shadowed_by_the_position_route() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::files::ensure_root(temp.path()).unwrap();
        let state = with_files_root(test_state().await, root);

        // No such message, so this stops at the database — which is proof enough that it reached
        // the bulk handler rather than being parsed as a position.
        assert_eq!(
            call(
                state,
                "POST",
                "/email/999/attachments/save-all",
                Some(serde_json::json!({"folder": ""}))
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_folder_can_be_created_and_listed_over_http() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::files::ensure_root(temp.path()).unwrap();
        let state = with_files_root(test_state().await, root);

        assert_eq!(
            call(
                state.clone(),
                "POST",
                "/files/folder",
                Some(serde_json::json!({"path": "BACMAT/2026"}))
            )
            .await
            .0,
            StatusCode::CREATED
        );

        let (status, entries) = call(state, "GET", "/files", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(entries[0]["name"], "BACMAT");
        assert_eq!(entries[0]["is_dir"], true);
    }

    /// The round trip the folder was missing: a file goes in, and the same bytes come back out.
    #[tokio::test]
    async fn a_file_can_be_uploaded_and_downloaded_again() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::files::ensure_root(temp.path()).unwrap();
        let state = with_files_root(test_state().await, root);

        assert_eq!(
            call(
                state.clone(),
                "POST",
                "/files/folder",
                Some(serde_json::json!({"path": "BACMAT"}))
            )
            .await
            .0,
            StatusCode::CREATED
        );

        let (status, saved) = upload(state.clone(), "BACMAT", "guia.docx", b"conteudo").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(saved["filename"], "guia.docx");

        let response = raw(
            state,
            "GET",
            "/files/download?path=BACMAT%2Fguia.docx",
            Body::empty(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        // Never `inline`, and never the type the extension suggests: this folder holds files that
        // arrived as mail from strangers, and a webview must not be invited to render one.
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "application/octet-stream"
        );
        assert!(
            response.headers()[axum::http::header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .starts_with("attachment;"),
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(&bytes[..], b"conteudo");
    }

    /// Search is the one route that answers about a folder it was not pointed at, so the path it
    /// reports has to be usable by every other route.
    #[tokio::test]
    async fn a_search_reaches_below_the_folder_and_reports_paths_that_work() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::files::ensure_root(temp.path()).unwrap();
        let state = with_files_root(test_state().await, root);

        call(
            state.clone(),
            "POST",
            "/files/folder",
            Some(serde_json::json!({"path": "BACMAT/2026"})),
        )
        .await;
        upload(state.clone(), "BACMAT/2026", "guia.docx", b"conteudo").await;

        let (status, found) = call(state.clone(), "GET", "/files/search?q=GUIA", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(found["truncated"], false);
        assert_eq!(found["hits"][0]["path"], "BACMAT/2026/guia.docx");
        assert_eq!(found["hits"][0]["name"], "guia.docx");

        // The path it reported is the one the download route takes, which is the point of reporting
        // it relative to the root rather than to the folder searched.
        let response = raw(
            state,
            "GET",
            "/files/download?path=BACMAT%2F2026%2Fguia.docx",
            Body::empty(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// An upload lands under the same rules as a filed attachment: the name is made safe, and a
    /// collision is numbered rather than allowed to erase what is already there.
    #[tokio::test]
    async fn an_upload_cannot_write_outside_the_root_and_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::files::ensure_root(temp.path()).unwrap();
        let state = with_files_root(test_state().await, root.clone());

        let (_, first) = upload(state.clone(), "", "guia.docx", b"primeiro").await;
        assert_eq!(first["filename"], "guia.docx");
        let (_, second) = upload(state.clone(), "", "guia.docx", b"segundo").await;
        assert_eq!(second["filename"], "guia (2).docx");
        assert_eq!(std::fs::read(root.join("guia.docx")).unwrap(), b"primeiro");

        let (_, escaped) = upload(state, "", "../../.ssh/authorized_keys", b"x").await;
        assert_eq!(escaped["filename"], "authorized_keys");
        assert!(root.join("authorized_keys").exists());
    }

    /// Deleting a folder with things in it takes a second word. The 409 is the whole point: it is
    /// the answer that lets the shell say what is about to be lost before it asks again.
    #[tokio::test]
    async fn a_full_folder_is_not_deleted_by_a_single_request() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::files::ensure_root(temp.path()).unwrap();
        let state = with_files_root(test_state().await, root.clone());

        call(
            state.clone(),
            "POST",
            "/files/folder",
            Some(serde_json::json!({"path": "BACMAT"})),
        )
        .await;
        upload(state.clone(), "BACMAT", "guia.docx", b"x").await;

        assert_eq!(
            call(state.clone(), "DELETE", "/files?path=BACMAT", None)
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert!(root.join("BACMAT").join("guia.docx").exists());

        assert_eq!(
            call(
                state.clone(),
                "DELETE",
                "/files?path=BACMAT&recursive=true",
                None
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert!(!root.join("BACMAT").exists());
    }

    #[tokio::test]
    async fn a_rename_moves_the_file_and_refuses_a_taken_name() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::files::ensure_root(temp.path()).unwrap();
        let state = with_files_root(test_state().await, root.clone());

        call(
            state.clone(),
            "POST",
            "/files/folder",
            Some(serde_json::json!({"path": "BACMAT"})),
        )
        .await;
        upload(state.clone(), "", "guia.docx", b"conteudo").await;
        upload(state.clone(), "", "outro.docx", b"outro").await;

        assert_eq!(
            call(
                state.clone(),
                "POST",
                "/files/move",
                Some(serde_json::json!({"from": "guia.docx", "to": "BACMAT/guia final.docx"}))
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            std::fs::read(root.join("BACMAT").join("guia final.docx")).unwrap(),
            b"conteudo"
        );

        assert_eq!(
            call(
                state,
                "POST",
                "/files/move",
                Some(serde_json::json!({"from": "outro.docx", "to": "BACMAT"}))
            )
            .await
            .0,
            StatusCode::CONFLICT,
            "a taken destination must be refused, not numbered and not overwritten"
        );
    }

    /// The one guard this whole surface rests on, checked through the routes rather than only in
    /// the module — a handler that forgets to call it is exactly the mistake worth catching, and
    /// there are now six handlers to forget it in.
    #[tokio::test]
    async fn a_path_that_leaves_the_root_is_refused_by_every_route() {
        let temp = tempfile::tempdir().unwrap();
        let root = crate::files::ensure_root(temp.path()).unwrap();
        let state = with_files_root(test_state().await, root);

        for escape in ["..", "../outside", "/etc", "C:\\Windows"] {
            let listed = call(
                state.clone(),
                "GET",
                &format!("/files?path={}", urlencode(escape)),
                None,
            )
            .await;
            assert_eq!(listed.0, StatusCode::BAD_REQUEST, "listed {escape:?}");

            let created = call(
                state.clone(),
                "POST",
                "/files/folder",
                Some(serde_json::json!({ "path": escape })),
            )
            .await;
            assert_eq!(created.0, StatusCode::BAD_REQUEST, "created {escape:?}");

            let downloaded = call(
                state.clone(),
                "GET",
                &format!("/files/download?path={}", urlencode(escape)),
                None,
            )
            .await;
            assert_eq!(
                downloaded.0,
                StatusCode::BAD_REQUEST,
                "downloaded {escape:?}"
            );

            let searched = call(
                state.clone(),
                "GET",
                &format!("/files/search?path={}&q=x", urlencode(escape)),
                None,
            )
            .await;
            assert_eq!(searched.0, StatusCode::BAD_REQUEST, "searched {escape:?}");

            let deleted = call(
                state.clone(),
                "DELETE",
                &format!("/files?path={}&recursive=true", urlencode(escape)),
                None,
            )
            .await;
            assert_eq!(deleted.0, StatusCode::BAD_REQUEST, "deleted {escape:?}");

            for pair in [
                serde_json::json!({ "from": escape, "to": "destino.docx" }),
                serde_json::json!({ "from": "origem.docx", "to": escape }),
            ] {
                let moved = call(state.clone(), "POST", "/files/move", Some(pair)).await;
                assert_eq!(moved.0, StatusCode::BAD_REQUEST, "moved {escape:?}");
            }

            // The upload names its folder, and that name is resolved the same way.
            let uploaded = upload(state.clone(), escape, "guia.docx", b"x").await;
            assert_eq!(
                uploaded.0,
                StatusCode::BAD_REQUEST,
                "uploaded into {escape:?}"
            );
        }
    }

    /// Sends bytes the way the shell does: the name and folder ride in the query string, the file
    /// is the whole body.
    async fn upload(
        state: AppState,
        folder: &str,
        filename: &str,
        bytes: &[u8],
    ) -> (StatusCode, serde_json::Value) {
        let response = raw(
            state,
            "POST",
            &format!(
                "/files/upload?folder={}&filename={}",
                urlencode(folder),
                urlencode(filename)
            ),
            Body::from(bytes.to_vec()),
        )
        .await;
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
        )
    }

    /// The response itself, for the two tests that care about headers or raw bytes rather than a
    /// JSON body.
    async fn raw(state: AppState, method: &str, uri: &str, body: Body) -> axum::response::Response {
        build_router(state)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("Authorization", "Bearer test-token")
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn urlencode(value: &str) -> String {
        value
            .bytes()
            .map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (byte as char).to_string()
                }
                other => format!("%{other:02X}"),
            })
            .collect()
    }

    async fn get_email_detail(state: AppState, id: i64) -> (StatusCode, serde_json::Value) {
        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/email/{id}"))
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    /// Opening a message reads the body, which the list deliberately does not carry.
    #[tokio::test]
    async fn opening_a_message_returns_its_body_and_attachments() {
        let state = test_state().await;
        let body = Body::from(
            serde_json::json!({
                "mailbox": "INBOX",
                "uidvalidity": 1,
                "max_uid_examined": 10,
                "messages": [{
                    "message_id": "<a@b>", "uid": 10, "from_addr": "ana@company.com",
                    "received_at": chrono::Utc::now().to_rfc3339(),
                    "body_text": "o texto que interessa",
                    "has_attachments": true,
                    "attachments": [
                        {"position": 0, "filename": "cotacao.pdf",
                         "mime_type": "application/pdf", "size_bytes": 4096},
                    ],
                }],
            })
            .to_string(),
        );
        assert_eq!(
            post_email(state.clone(), Some("test-token"), body).await,
            StatusCode::OK
        );

        let id = get_queue(state.clone()).await[0]["id"].as_i64().unwrap();
        let (status, detail) = get_email_detail(state, id).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["body_text"], "o texto que interessa");
        assert_eq!(detail["attachments"][0]["filename"], "cotacao.pdf");
        assert_eq!(detail["attachments"][0]["size_bytes"], 4096);
    }

    #[tokio::test]
    async fn abrir_a_mensagem_mostra_a_regra_que_decidiu() {
        let state = test_state().await;
        let id = sqlx::query(
            "INSERT INTO emails (message_id, mailbox, uidvalidity, uid, from_addr, body_text,
                                 received_at, ingested_at, triage_class, model_class, priority_rule)
             VALUES ('<priority-audit@x>', 'INBOX', 1, 77, 'sender@example.com', 'body',
                     '2026-07-30T10:00:00+00:00', '2026-07-30T10:00:00+00:00',
                     'action', 'urgent', 'first-contact')",
        )
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let (status, message) = get_email_detail(state, id).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(message["model_class"], "urgent");
        assert_eq!(message["priority_rule"], "first-contact");
    }

    /// `/email/queue` must keep winning over `/email/{id}`, or listing the mailbox starts trying to
    /// open a message called "queue".
    #[tokio::test]
    async fn the_static_email_routes_still_win_over_the_id_route() {
        let state = test_state().await;
        assert!(get_queue(state).await.is_empty());
    }

    /// The header a stranger's filename ends up in.
    #[test]
    fn a_content_disposition_cannot_be_ended_by_a_filename() {
        let header = content_disposition("relatorio\r\nSet-Cookie: session=stolen.docx");
        assert!(
            !header.contains('\r') && !header.contains('\n'),
            "the header carries a line break: {header}"
        );
        // Always a download, never something rendered where it landed.
        assert!(header.starts_with("attachment; "));
    }

    #[test]
    fn a_content_disposition_carries_the_name_in_both_forms() {
        // The ASCII form stays plain for old clients; `filename*` carries the accents faithfully.
        assert_eq!(
            content_disposition("MÉDIAS.docx"),
            "attachment; filename=\"M_DIAS.docx\"; filename*=UTF-8''M%C3%89DIAS.docx"
        );
        // A name with nothing usable in it still produces a valid header.
        assert_eq!(
            content_disposition(""),
            "attachment; filename=\"attachment.bin\"; filename*=UTF-8''attachment.bin"
        );
    }

    #[tokio::test]
    async fn asking_for_an_attachment_that_was_never_described_is_a_404() {
        let state = test_state().await;
        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/email/1/attachments/0")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Answered from the database, so no sidecar is contacted and none needs to be running.
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn opening_a_message_that_does_not_exist_is_a_404() {
        let state = test_state().await;
        assert_eq!(get_email_detail(state, 9999).await.0, StatusCode::NOT_FOUND);
    }

    async fn get_cursor_body(state: AppState, mailbox: &str) -> serde_json::Value {
        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/email/cursor?mailbox={mailbox}"))
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn an_unsynchronised_mailbox_reports_a_null_cursor() {
        let state = test_state().await;
        assert_eq!(
            get_cursor_body(state, "INBOX").await,
            serde_json::Value::Null
        );
    }

    /// Two mailboxes have two positions; answering with the wrong one would resynchronise a
    /// mailbox from the other's uid.
    #[tokio::test]
    async fn the_cursor_route_answers_per_mailbox() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO email_cursor (mailbox, uidvalidity, last_uid, updated_at) VALUES
                 ('INBOX', 1, 10, '2026-07-28T10:00:00+00:00'),
                 ('Archive', 2, 20, '2026-07-28T10:00:00+00:00')",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let inbox = get_cursor_body(state.clone(), "INBOX").await;
        assert_eq!(inbox["uidvalidity"], 1);
        assert_eq!(inbox["last_uid"], 10);
        let archive = get_cursor_body(state, "Archive").await;
        assert_eq!(archive["uidvalidity"], 2);
        assert_eq!(archive["last_uid"], 20);
    }

    async fn requeue_status(state: AppState, id: i64) -> StatusCode {
        build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/email/{id}/requeue"))
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn requeueing_an_unknown_email_is_a_404() {
        let state = test_state().await;
        assert_eq!(requeue_status(state, 999).await, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn requeueing_a_classified_email_puts_it_back_in_the_queue() {
        let state = test_state().await;
        let id = sqlx::query(
            "INSERT INTO emails (message_id, mailbox, uidvalidity, uid, from_addr, body_text,
                                 received_at, ingested_at, triage_class, triage_summary,
                                 triaged_at, triage_attempts)
             VALUES ('<r@x>', 'INBOX', 1, 1, 'a@b', 'still here',
                     '2026-07-28T10:00:00+00:00', '2026-07-28T10:00:00+00:00',
                     'info', 'wrong call', '2026-07-28T10:05:00+00:00', 2)",
        )
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        assert_eq!(
            requeue_status(state.clone(), id).await,
            StatusCode::NO_CONTENT
        );
        let (class, attempts): (Option<String>, i64) =
            sqlx::query_as("SELECT triage_class, triage_attempts FROM emails WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(class, None);
        assert_eq!(attempts, 0);
    }

    /// Registers a project with a root on disk, and writes `.ai/autopilot.yaml` under it.
    async fn project_with_rules(state: &AppState, id: &str, yaml: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ai")).unwrap();
        std::fs::write(dir.path().join(".ai").join("autopilot.yaml"), yaml).unwrap();
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, 'shadow', ?)",
        )
        .bind(id)
        .bind(dir.path().to_string_lossy().to_string())
        .execute(&state.pool)
        .await
        .unwrap();
        dir
    }

    async fn read_rules(state: AppState, id: &str) -> (StatusCode, serde_json::Value) {
        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/projects/{id}/rules"))
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, parsed)
    }

    /// A rule the tick cannot parse is skipped and logged at debug — 2,880 times a day, which is
    /// the same as not being logged at all. The rule never runs and nothing anywhere says so, which
    /// is the failure this endpoint exists to make visible.
    #[tokio::test]
    async fn a_schedule_that_can_never_fire_says_why_instead_of_going_quiet() {
        let state = test_state().await;
        let _dir = project_with_rules(
            &state,
            "alpha",
            "schedules:\n\
             \x20 - name: nightly\n\
             \x20   cron: 'not a cron'\n\
             \x20   prompt: sweep\n\
             \x20 - name: lisbon\n\
             \x20   cron: '0 8 * * *'\n\
             \x20   prompt: morning\n\
             \x20   timezone: Mars/Olympus\n\
             \x20 - name: fine\n\
             \x20   cron: '0 8 * * *'\n\
             \x20   prompt: morning\n\
             \x20   timezone: Europe/Lisbon\n",
        )
        .await;

        let (status, body) = read_rules(state, "alpha").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["rules_file"], "present");

        let schedules = body["schedules"].as_array().unwrap();
        assert!(
            schedules[0]["problem"]
                .as_str()
                .unwrap()
                .contains("is not a cron expression"),
        );
        assert!(schedules[0]["next_fire_at"].is_null());
        // An unknown zone is an error rather than a silent fall back to UTC, for the reason
        // `rule_timezone` gives: reading `Europe/Lisbon` as UTC fires an hour off and looks fine.
        assert!(
            schedules[1]["problem"]
                .as_str()
                .unwrap()
                .contains("is not an IANA timezone"),
        );
        // The healthy one answers with a time, not a complaint.
        assert!(schedules[2]["problem"].is_null());
        assert!(schedules[2]["next_fire_at"].is_string());
    }

    /// `deny_unknown_fields` exists so a typo is an error instead of an empty ruleset — but the
    /// error only ever reached a log line, so `schedule:` for `schedules:` stopped every scheduled
    /// run for that project and looked exactly like having no rules.
    #[tokio::test]
    async fn a_misspelt_key_is_reported_rather_than_read_as_no_rules_at_all() {
        let state = test_state().await;
        let _dir = project_with_rules(
            &state,
            "alpha",
            "schedule:\n\x20 - name: nightly\n\x20   cron: '0 8 * * *'\n\x20   prompt: sweep\n",
        )
        .await;

        let (_, body) = read_rules(state, "alpha").await;
        assert_eq!(body["rules_file"], "unreadable");
        assert!(body["rules_error"].as_str().unwrap().contains("schedule"));
        assert_eq!(body["schedules"].as_array().unwrap().len(), 0);
    }

    /// A project with no root has no file to read. That is what `off` looks like, not a fault.
    #[tokio::test]
    async fn a_project_without_a_root_reports_no_rules_rather_than_an_error() {
        let state = test_state().await;
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES ('alpha', 'off')")
            .execute(&state.pool)
            .await
            .unwrap();

        let (status, body) = read_rules(state, "alpha").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["rules_file"], "absent");
        assert!(body["rules_error"].is_null());
        assert!(body["project_root"].is_null());
    }

    async fn set_wip_limit(state: AppState, id: &str, body: serde_json::Value) -> StatusCode {
        build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/projects/{id}/wip-limit"))
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn the_wip_ceiling_can_be_set_cleared_and_never_made_negative() {
        let state = test_state().await;
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES ('alpha', 'shadow')")
            .execute(&state.pool)
            .await
            .unwrap();

        assert_eq!(
            set_wip_limit(state.clone(), "alpha", serde_json::json!({ "limit": 2 })).await,
            StatusCode::NO_CONTENT,
        );
        let (_, body) = read_rules(state.clone(), "alpha").await;
        assert_eq!(body["wip_limit"], 2);

        // `queue_full` compares `open >= limit`, so a negative ceiling would mean "never start
        // anything again" while reading like a number somebody chose.
        assert_eq!(
            set_wip_limit(state.clone(), "alpha", serde_json::json!({ "limit": -1 })).await,
            StatusCode::BAD_REQUEST,
        );
        let (_, unchanged) = read_rules(state.clone(), "alpha").await;
        assert_eq!(unchanged["wip_limit"], 2);

        // Null is the brake switched off, which is a state the table already expresses.
        assert_eq!(
            set_wip_limit(state.clone(), "alpha", serde_json::json!({ "limit": null })).await,
            StatusCode::NO_CONTENT,
        );
        let limit: Option<i64> =
            sqlx::query_scalar("SELECT wip_limit FROM autopilot_state WHERE project_id = 'alpha'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(limit, None);

        assert_eq!(
            set_wip_limit(state, "nowhere", serde_json::json!({ "limit": 1 })).await,
            StatusCode::NOT_FOUND,
        );
    }

    /// `contact_addresses` has been written on every inbound message since it existed and read by
    /// nothing outside `priority.rs` — three of `Profile`'s fields still carry `#[allow(dead_code)]`
    /// pointing at a display surface that never arrived.
    #[tokio::test]
    async fn the_roster_reports_correspondents_busiest_first_with_their_standing_decision() {
        let state = test_state().await;
        for (address, messages_in) in [("quiet@example.com", 1), ("busy@example.com", 40)] {
            seen_from(&state, address).await;
            sqlx::query("UPDATE contact_addresses SET messages_in = ? WHERE address = ?")
                .bind(messages_in)
                .bind(address)
                .execute(&state.pool)
                .await
                .unwrap();
        }
        set_sender_verdict(
            state.clone(),
            serde_json::json!({ "address": "busy@example.com", "verdict": "mute" }),
        )
        .await;

        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/contacts")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let roster: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let rows = roster.as_array().unwrap();

        // Busiest first: the question this answers is "who fills my mailbox", and the answer is
        // useless in address order.
        assert_eq!(rows[0]["address"], "busy@example.com");
        assert_eq!(rows[0]["messages_in"], 40);
        // The standing decision travels with the row, so the one screen that lists everyone is also
        // the one place a mute can be found again after the message that prompted it is gone.
        assert_eq!(rows[0]["verdict"], "mute");
        assert_eq!(rows[1]["address"], "quiet@example.com");
        assert!(rows[1]["verdict"].is_null());
    }

    /// The mailbox name was hard-coded in the shell because nothing reported it, and a wrong one
    /// reads as an empty mailbox rather than as an error — the worst shape a wrong answer can take.
    #[tokio::test]
    async fn the_email_config_names_the_mailbox_and_never_the_password() {
        let mut state = test_state().await;
        state.email = std::sync::Arc::new(crate::state::EmailRuntime::from_config(
            &crate::config::EmailConfig {
                enabled: true,
                host: "imap.example.com".into(),
                port: 993,
                username: "me@example.com".into(),
                mailbox: "Trabalho".into(),
                poll_interval_secs: 120,
                ..Default::default()
            },
            std::path::PathBuf::new(),
            std::path::PathBuf::new(),
            None,
        ));

        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/config/email")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let config: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(config["mailbox"], "Trabalho");
        assert_eq!(config["host"], "imap.example.com");
        assert_eq!(config["poll_interval_secs"], 120);
        // Enabled is not armed. The barrier is proven at startup, and until it is, the pillar
        // stores and expires mail without triaging any of it — a state worth being able to see.
        assert_eq!(config["armed"], false);

        // The IMAP password lives in Credential Manager and is handed to the sidecar process. It
        // is not in `EmailRuntime` at all, and this is what says the readout must never grow it.
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            !body.contains("password"),
            "the email readout must not carry a credential: {body}"
        );
    }

    /// A sidecar that keeps failing to start is invisible without this: the supervisor restarts it
    /// and logs, and the Mail tab looks like a quiet mailbox rather than a broken poller.
    #[tokio::test]
    async fn the_sidecar_readout_answers_even_before_anything_has_been_supervised() {
        let state = test_state().await;

        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/sidecars")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let listed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // An array either way. The registry fills as supervisors start, so "none yet" has to be an
        // empty list rather than an error — a daemon with no sidecars configured is a normal daemon.
        assert!(listed.is_array());
    }

    /// The shell's transcript lived only in the window that made it, because nothing on a run row
    /// said which conversation the turn belonged to. `/runs?mode=assistant` could never stand in
    /// for this: it is every chat at once, the Telegram sidecar's turns included.
    #[tokio::test]
    async fn a_chat_reads_back_its_own_turns_and_nobody_else_s() {
        let state = test_state().await;
        for (chat, prompt) in [
            (Some("shell"), "first"),
            (Some("-100999"), "a telegram message"),
            (Some("shell"), "second"),
            (None, "an ordinary run"),
        ] {
            sqlx::query(
                "INSERT INTO runs (prompt, status, mode, session_id, chat_id, created_at)
                 VALUES (?, 'completed', ?, 's', ?, '2026-07-30T10:00:00+00:00')",
            )
            .bind(prompt)
            .bind(if chat.is_some() { "assistant" } else { "real" })
            .bind(chat)
            .execute(&state.pool)
            .await
            .unwrap();
        }

        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/assistant/chats/shell")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let turns: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        let asked: Vec<&str> = turns
            .as_array()
            .unwrap()
            .iter()
            .map(|turn| turn["asked"].as_str().unwrap())
            .collect();
        // Oldest first, so the conversation reads downwards the way it was had.
        assert_eq!(asked, vec!["first", "second"]);
    }

    /// A chat named like a number must not be read as a turn id. Static segments win in matchit,
    /// which is what keeps the two routes apart — asserted rather than assumed, because the failure
    /// would be a chat silently answering with one unrelated run.
    #[tokio::test]
    async fn a_numeric_chat_id_does_not_collide_with_a_turn_id() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, chat_id, created_at)
             VALUES ('hello', 'completed', 'assistant', 's', '1', '2026-07-30T10:00:00+00:00')",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/assistant/chats/1")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let turns: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // An array, not the single run object `/assistant/{turn_id}` would have answered with.
        assert_eq!(turns.as_array().unwrap().len(), 1);
    }

    async fn set_sender_verdict(state: AppState, body: serde_json::Value) -> StatusCode {
        build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/contacts/verdict")
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// Gives the daemon a contact for `address`, the way receiving mail would.
    async fn seen_from(state: &AppState, address: &str) -> i64 {
        let contact_id =
            sqlx::query("INSERT INTO contacts (display_name, created_at) VALUES (NULL, '2026-07-30T10:00:00+00:00')")
                .execute(&state.pool)
                .await
                .unwrap()
                .last_insert_rowid();
        sqlx::query(
            "INSERT INTO contact_addresses (address, contact_id, first_seen, last_seen, messages_in)
             VALUES (?, ?, '2026-07-30T10:00:00+00:00', '2026-07-30T10:00:00+00:00', 1)",
        )
        .bind(crate::contacts::normalize_address(address))
        .bind(contact_id)
        .execute(&state.pool)
        .await
        .unwrap();
        contact_id
    }

    /// The pin is the highest-authority rule in triage, and nothing in the product could set it —
    /// `contact_overrides` was read by `priority::adjust` and written only by a test. These four
    /// cover the door that was missing, and the two ways it must refuse.
    #[tokio::test]
    async fn pinning_a_sender_records_the_verdict_against_their_contact() {
        let state = test_state().await;
        let contact_id = seen_from(&state, "Maria <MARIA@example.com>").await;

        assert_eq!(
            set_sender_verdict(
                state.clone(),
                serde_json::json!({ "address": "maria@example.com", "verdict": "pin" }),
            )
            .await,
            StatusCode::NO_CONTENT,
        );

        let stored: Option<String> =
            sqlx::query_scalar("SELECT verdict FROM contact_overrides WHERE contact_id = ?")
                .bind(contact_id)
                .fetch_optional(&state.pool)
                .await
                .unwrap();
        assert_eq!(stored.as_deref(), Some("pin"));
    }

    #[tokio::test]
    async fn a_second_verdict_replaces_the_first_rather_than_colliding_with_it() {
        let state = test_state().await;
        seen_from(&state, "maria@example.com").await;

        for verdict in ["pin", "mute"] {
            assert_eq!(
                set_sender_verdict(
                    state.clone(),
                    serde_json::json!({ "address": "maria@example.com", "verdict": verdict }),
                )
                .await,
                StatusCode::NO_CONTENT,
            );
        }

        // One row, holding the later decision: `contact_id` is the primary key, so changing your
        // mind must be an update and not a constraint violation.
        let rows: Vec<String> = sqlx::query_scalar("SELECT verdict FROM contact_overrides")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(rows, vec!["mute".to_owned()]);

        assert_eq!(
            set_sender_verdict(
                state.clone(),
                serde_json::json!({ "address": "maria@example.com", "verdict": null }),
            )
            .await,
            StatusCode::NO_CONTENT,
        );
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contact_overrides")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0);
    }

    /// `priority::adjust` matches `pin` and `mute` and falls through for everything else, so a
    /// stored typo is not an error — it is a row that quietly does nothing forever. The 400 here is
    /// the only moment that mistake is ever visible.
    #[tokio::test]
    async fn a_verdict_the_policy_does_not_know_is_refused_rather_than_stored() {
        let state = test_state().await;
        seen_from(&state, "maria@example.com").await;

        assert_eq!(
            set_sender_verdict(
                state.clone(),
                serde_json::json!({ "address": "maria@example.com", "verdict": "urgent" }),
            )
            .await,
            StatusCode::BAD_REQUEST,
        );
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contact_overrides")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(stored, 0);
    }

    #[tokio::test]
    async fn pinning_an_address_nobody_has_written_from_is_a_404() {
        let state = test_state().await;

        // A contact exists because a message arrived. Inventing one here would let a mistyped
        // address become a permanent row that never matches anything and never explains itself.
        assert_eq!(
            set_sender_verdict(
                state,
                serde_json::json!({ "address": "nobody@example.com", "verdict": "pin" }),
            )
            .await,
            StatusCode::NOT_FOUND,
        );
    }

    /// The queue carries the sender's standing verdict so the list can draw the button. Matching it
    /// to a message means normalising `from_addr` — and a header that carries a display name is
    /// exactly where a SQL `LOWER(TRIM(...))` would have disagreed with `normalize_address`.
    #[tokio::test]
    async fn the_queue_reports_a_pin_even_when_the_header_carries_a_display_name() {
        let state = test_state().await;
        seen_from(&state, "maria@example.com").await;
        set_sender_verdict(
            state.clone(),
            serde_json::json!({ "address": "maria@example.com", "verdict": "pin" }),
        )
        .await;

        for (message_id, from_addr) in [
            ("<a@x>", "Maria Silva <Maria@Example.com>"),
            ("<b@x>", "maria@example.com"),
        ] {
            sqlx::query(
                "INSERT INTO emails (message_id, mailbox, uidvalidity, uid, from_addr,
                                     received_at, ingested_at)
                 VALUES (?, 'INBOX', 1, ABS(RANDOM() % 100000), ?,
                         '2026-07-30T10:00:00+00:00', '2026-07-30T10:00:00+00:00')",
            )
            .bind(message_id)
            .bind(from_addr)
            .execute(&state.pool)
            .await
            .unwrap();
        }

        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/email/queue")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let queue: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        let verdicts: Vec<Option<&str>> = queue
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["sender_verdict"].as_str())
            .collect();
        assert_eq!(verdicts, vec![Some("pin"), Some("pin")]);
    }

    /// Rejecting a proposal is two commits with a gap between them: the proposal flips to
    /// `rejected`, and only then is the paused run discarded and its worktree slot freed. A client
    /// that disconnects cancels the request, dropping the handler future the way `abort()` drops a
    /// run's — and what is left behind cannot be undone through the same door, because the proposal
    /// is no longer `pending` and a retry answers 409. The run stays `awaiting_approval`, which
    /// `one_open_worktree_run_per_project` (migration 0009) turns into a project-wide block that
    /// only the separate release queue can lift.
    #[tokio::test]
    async fn a_dropped_reject_request_still_discards_the_paused_run() {
        use std::future::Future;

        let dir = tempfile::tempdir().unwrap();
        // File-backed, with room for a second connection: the assertions have to watch the handler's
        // progress while the handler itself is parked on the pool.
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(dir.path().join("reject.db"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        let state = AppState {
            token: Token("test-token".into()),
            pool: pool.clone(),
            runner: Arc::new(FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        };

        let run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('proj', 'x', 'awaiting_approval', 'worktree', '2026-07-28T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let proposal_id = proposals::create_action_approval(
            &pool,
            run_id,
            None,
            Some("proj"),
            "Bash",
            "needs approval",
            None,
        )
        .await
        .unwrap();

        let mut handler = Box::pin(post_proposal_reject(State(state), Path(proposal_id)));

        // Drive the handler by hand and drop it once the proposal has been rejected — the commit
        // that cannot be replayed, and the point from which the run is on its own.
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut rejected = false;
        for _ in 0..10_000 {
            assert!(
                handler.as_mut().poll(&mut context).is_pending(),
                "the handler ran to completion before the request could be dropped"
            );
            let status: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
                .bind(proposal_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if status == "rejected" {
                rejected = true;
                break;
            }
        }
        assert!(rejected, "the handler never rejected the proposal");
        drop(handler);

        let mut status = String::new();
        for _ in 0..100 {
            status = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if status != "awaiting_approval" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            status, "cancelled",
            "a rejected proposal must not leave its run pinning the project"
        );
        // This test builds its own pool rather than using `storage::TempDb`, because the race it
        // drives depends on the exact pool it was written against. It still has to close it, or the
        // directory outlives the run for the same reason every other one did.
        pool.close().await;
    }

    #[tokio::test]
    async fn health_returns_200_ok() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), b"ok");
    }

    #[tokio::test]
    async fn health_readout_requires_the_bearer_token() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health/readout")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn health_readout_returns_200_when_the_verdict_is_down() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health/readout")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let readout: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(readout["status"], "down");
    }

    #[tokio::test]
    async fn projects_returns_json_array_with_bearer_token() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/projects")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(parsed.is_array());
    }

    #[tokio::test]
    async fn projects_rejects_requests_without_bearer_token() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/projects")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn project_ls_returns_404_when_project_has_no_root() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/projects/ghost/ls")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn project_cat_rejects_unsafe_path() {
        let state = test_state().await;
        let root = tempfile::tempdir().unwrap();
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('p', 'active', ?)",
        )
        .bind(root.path().to_string_lossy().into_owned())
        .execute(&state.pool)
        .await
        .unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/projects/p/cat?path=..%2f..%2fsecret")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn assistant_message_returns_turn_id_with_bearer_token() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/assistant/message")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "chat_id": "chat-1",
                            "text": "hello"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(parsed["turn_id"].is_number());
    }

    #[tokio::test]
    async fn assistant_message_rejects_without_bearer_token() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/assistant/message")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "chat_id": "chat-1",
                            "text": "hello"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn assistant_turn_status_returns_run_after_message() {
        let app = build_router(test_state().await);
        let post_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/assistant/message")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "chat_id": "chat-2",
                            "text": "hello"
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(post_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let turn_id = parsed["turn_id"].as_i64().unwrap();

        let get_response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/assistant/{turn_id}"))
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(get_response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(get_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["id"], serde_json::json!(turn_id));
    }

    #[tokio::test]
    async fn a_run_response_exposes_the_gate_verdict_and_its_reason() {
        let state = test_state().await;
        let run_id = sqlx::query(
            "INSERT INTO runs
             (prompt, status, mode, gate_status, gate_exit_code, gate_output, created_at)
             VALUES ('gate diagnostics', 'completed', 'worktree', 'errored', NULL,
                     'gate configuration is unreadable', '2026-07-29T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/runs/{run_id}"))
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["gate_status"], "errored");
        assert_eq!(parsed.get("gate_exit_code"), Some(&serde_json::Value::Null));
        assert_eq!(parsed["gate_output"], "gate configuration is unreadable");
    }

    #[tokio::test]
    async fn a_run_response_exposes_its_token_usage() {
        let state = test_state().await;
        let run_id = sqlx::query(
            "INSERT INTO runs
             (prompt, status, mode, input_tokens, output_tokens, cache_read_tokens, num_turns,
              created_at)
             VALUES ('measured run', 'completed', 'real', 1000, 500, 20000, 12,
                     '2026-07-29T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/runs/{run_id}"))
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["input_tokens"], 1000);
        assert_eq!(parsed["output_tokens"], 500);
        assert_eq!(parsed["cache_read_tokens"], 20000);
        assert_eq!(parsed["num_turns"], 12);
    }

    /// A runner that publishes a context fill and then never returns.
    ///
    /// Parked rather than slow: the test needs a run that is genuinely mid-stream when the request
    /// is served, and a delay long enough to be safe is a delay long enough to be slow.
    struct LiveContextFillRunner {
        fill: i64,
    }

    #[async_trait::async_trait]
    impl crate::runner::CommandRunner for LiveContextFillRunner {
        async fn run_prompt(
            &self,
            _request: crate::runner::RunRequest,
            _session_tx: tokio::sync::mpsc::UnboundedSender<String>,
            _transcript: Arc<std::sync::Mutex<String>>,
        ) -> std::io::Result<crate::runner::RunOutcome> {
            std::future::pending::<()>().await;
            unreachable!("a parked run never resolves")
        }

        async fn run_prompt_with_context_fill(
            &self,
            request: crate::runner::RunRequest,
            session_tx: tokio::sync::mpsc::UnboundedSender<String>,
            transcript: Arc<std::sync::Mutex<String>>,
            context_fill: Arc<std::sync::Mutex<Option<i64>>>,
        ) -> std::io::Result<crate::runner::RunOutcome> {
            // What the CLI runner does per streamed line, done once: the mirror carries the number
            // while the process is still alive, which is the only state this test is about.
            *context_fill.lock().unwrap() = Some(self.fill);
            self.run_prompt(request, session_tx, transcript).await
        }
    }

    /// Per-turn context fill is measured from the stream while the run is alive, and has to be
    /// readable then — that is the whole point of measuring it. The monitor was wired to the stream
    /// from the start, but `runs.context_fill` was written only by the terminal UPDATE, so a live
    /// run answered `context_fill: null` and the number landed exactly when it had stopped being
    /// something anyone could act on.
    ///
    /// Driven through the real handler over a run that is genuinely parked mid-stream. The sibling
    /// tests above write the column themselves and would pass against a daemon that never computed
    /// anything: they prove `SELECT` can read what `INSERT` wrote, not that a live run reports.
    #[tokio::test]
    async fn a_running_run_reports_its_context_fill() {
        let mut state = test_state().await;
        state.runner = Arc::new(LiveContextFillRunner { fill: 164_000 });
        let run_id = crate::runs::create_run_inner(
            &state,
            "a run that keeps talking".to_string(),
            None,
            None,
            "real",
            false,
        )
        .await
        .expect("a real-mode run needs neither a project nor a worktree");

        // Generous, because what is being asserted is that the number arrives at all — the mirror
        // is throttled, so anything shorter would be measuring the throttle rather than the wiring.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let reported = loop {
            let response = build_router(state.clone())
                .oneshot(
                    Request::builder()
                        .uri(format!("/runs/{run_id}"))
                        .header("Authorization", "Bearer test-token")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                parsed["status"], "running",
                "the run must still be live, or this says nothing about a live one"
            );
            if let Some(fill) = parsed["context_fill"].as_i64() {
                break fill;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "GET /runs/{{id}} never reported the context fill of a run that is still going"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };

        assert_eq!(
            reported, 164_000,
            "the number on the wire must be the one the stream reported"
        );
    }

    /// Two facts the daemon records about a run were invisible to every client reading it back:
    /// whether the run can be spoken to at all, and which run continued it after a context handoff.
    /// Both are columns on `runs`; neither was in `RunStatusResponse`, so the only way to learn
    /// either was to open the database. A steering caller could not tell a refusal it deserved
    /// (`steerable = 0`) from one caused by something else, and a handoff's successor could be found
    /// only by guessing at ids.
    #[tokio::test]
    async fn a_run_reports_whether_it_is_steerable_and_which_run_succeeded_it() {
        let state = test_state().await;
        let run_id = crate::runs::create_run_inner(
            &state,
            "a run that may be spoken to".to_string(),
            None,
            None,
            "real",
            true,
        )
        .await
        .expect("a real-mode run needs neither a project nor a worktree");

        // A real row, because `successor_run_id` carries `REFERENCES runs(id)` (migration 0041) —
        // an invented id is rejected, which is the constraint doing its job.
        let successor_id = crate::runs::create_run_inner(
            &state,
            "the run that continued the work".to_string(),
            None,
            None,
            "real",
            false,
        )
        .await
        .expect("a real-mode run needs neither a project nor a worktree");

        sqlx::query("UPDATE runs SET successor_run_id = ? WHERE id = ?")
            .bind(successor_id)
            .bind(run_id)
            .execute(&state.pool)
            .await
            .expect("link a successor the way a handoff does");

        let response = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/runs/{run_id}"))
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            parsed["steerable"],
            serde_json::json!(true),
            "a run created steerable must say so when read back"
        );
        assert_eq!(
            parsed["successor_run_id"],
            serde_json::json!(successor_id),
            "the run that continued this one must be reachable without reading the database"
        );
    }

    #[tokio::test]
    async fn awaiting_approval_runs_returns_seeded_run_with_bearer_token() {
        let state = test_state().await;
        let pool = state.pool.clone();
        sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, mode, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind("project-alpha")
        .bind("C:/worktrees/project-alpha/run-1")
        .bind("release the pinned worktree")
        .bind("awaiting_approval")
        .bind("worktree")
        .bind("2026-07-20T10:11:12Z")
        .execute(&pool)
        .await
        .unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/runs/awaiting-approval")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            parsed,
            serde_json::json!([{
                "id": 1,
                "project_id": "project-alpha",
                "prompt": "release the pinned worktree",
                "cwd": "C:/worktrees/project-alpha/run-1",
                "created_at": "2026-07-20T10:11:12Z"
            }])
        );
    }

    #[tokio::test]
    async fn awaiting_approval_runs_rejects_requests_without_bearer_token() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/runs/awaiting-approval")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// What a caller sends when it steers a run. One fixed string so every assertion below can say
    /// whether THIS text reached the run rather than whether something did.
    const STEERING_MESSAGE: &str = "actually, stop after the current file";

    /// The modes `runs::create_run_inner` derives `ToolPolicy::None` from. Such a run has no tools at
    /// all because it exists to read words nobody vouches for, and a mode added to that derivation
    /// belongs here too — this list is what keeps the refusal from silently narrowing to one mode.
    const TOOLLESS_MODES: &[&str] = &[crate::email::TRIAGE_MODE];

    /// A `runs` row plus everything a live run carries: an abort handle in `run_handles` and a
    /// steering channel in `run_messages`.
    ///
    /// Both are registered even for the runs that must be refused, and deliberately so. A refusal is
    /// only worth asserting beside evidence that a delivery WOULD have been visible — and a handler
    /// that read the presence of a handle or a channel as permission to write would pass a test whose
    /// fixture withheld them.
    async fn run_to_steer(
        state: &AppState,
        status: &str,
        mode: &str,
        steerable: bool,
    ) -> (i64, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, steerable, created_at)
             VALUES ('keep working', ?, ?, ?, '2026-07-30T00:00:00Z')",
        )
        .bind(status)
        .bind(mode)
        .bind(i64::from(steerable))
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let task =
            tokio::spawn(async { tokio::time::sleep(std::time::Duration::from_secs(60)).await });
        state
            .run_handles
            .lock()
            .unwrap()
            .insert(run_id, task.abort_handle());

        let (messages_tx, messages_rx) = tokio::sync::mpsc::unbounded_channel();
        state
            .run_messages
            .lock()
            .unwrap()
            .insert(run_id, messages_tx);

        (run_id, messages_rx)
    }

    async fn steer(state: AppState, run_id: i64, token: &str) -> StatusCode {
        api_token_request(
            state,
            "POST",
            &format!("/runs/{run_id}/message"),
            token,
            Some(serde_json::json!({ "message": STEERING_MESSAGE })),
        )
        .await
        .status()
    }

    /// Steering is opt-in at spawn time because only a run launched with `--input-format stream-json`
    /// has a stdin anything can be written to. A run that did not opt in is not merely inconvenient
    /// to reach — there is no channel — so the route must say no rather than buffer the text
    /// somewhere for a run that will never read it.
    #[tokio::test]
    async fn steering_a_run_that_did_not_opt_in_is_refused() {
        let state = test_state().await;
        let (run_id, mut messages) = run_to_steer(&state, "running", "real", false).await;

        let status = steer(state.clone(), run_id, "test-token").await;

        assert!(status.is_client_error(), "{status}");
        assert!(
            messages.try_recv().is_err(),
            "a run that did not opt in must receive nothing"
        );
    }

    /// A finished run has no process left to say anything to. Accepting the message anyway would
    /// record an instruction against a transcript that ended before it arrived, which reads
    /// afterwards as something the run was told and ignored.
    #[tokio::test]
    async fn steering_a_run_that_is_not_running_is_refused() {
        for finished in ["completed", "cancelled"] {
            let state = test_state().await;
            let (run_id, mut messages) = run_to_steer(&state, finished, "real", true).await;

            let status = steer(state.clone(), run_id, "test-token").await;

            assert!(status.is_client_error(), "{finished}: {status}");
            assert!(
                messages.try_recv().is_err(),
                "{finished}: a run that has stopped must receive nothing"
            );
        }
    }

    /// The untrusted-content boundary of spec §5.5, from the other side. The email pillar's premise
    /// is that text a stranger wrote never meets a tool; steering adds a second author to a live
    /// session, and the one session that must never gain an author is the one already holding a
    /// stranger's words. Refused with the flag set, the run alive and the control token presented —
    /// every other condition met.
    #[tokio::test]
    async fn steering_a_triage_spawned_run_is_refused() {
        let state = test_state().await;
        let (run_id, mut messages) =
            run_to_steer(&state, "running", crate::email::TRIAGE_MODE, true).await;

        let status = steer(state.clone(), run_id, "test-token").await;

        assert!(status.is_client_error(), "{status}");
        assert!(
            messages.try_recv().is_err(),
            "the email pillar's runs take no instructions from this route"
        );
    }

    /// Keyed on the run's tool policy rather than on where it came from, and separate from the triage
    /// test above for exactly that reason: the two rules coincide on today's single toolless mode,
    /// and each has to hold on its own the day a second one appears. A toolless run is the one the
    /// daemon spawns to read content it does not trust, so it is the last run that may be spoken to.
    #[tokio::test]
    async fn steering_a_run_under_tool_policy_none_is_refused() {
        for &mode in TOOLLESS_MODES {
            let state = test_state().await;
            let (run_id, mut messages) = run_to_steer(&state, "running", mode, true).await;

            let status = steer(state.clone(), run_id, "test-token").await;

            assert!(status.is_client_error(), "{mode}: {status}");
            assert!(
                messages.try_recv().is_err(),
                "{mode}: a run spawned with no tools must not be given a second author"
            );
        }
    }

    /// Steering is its own authorization, not a consequence of being allowed to start runs. A key
    /// that creates a run authorises the prompt it supplies at that moment; a later turn into a
    /// session that already holds tools is a prompt nobody reviewed, reaching a process already past
    /// every check its creation went through.
    #[tokio::test]
    async fn steering_without_the_required_scope_is_refused() {
        let state = test_state().await;
        let run_creating =
            store_api_token_at_level(&state, "launcher", ApiTokenLevel::RunCreating).await;
        let (run_id, mut messages) = run_to_steer(&state, "running", "real", true).await;

        let status = steer(state.clone(), run_id, &run_creating).await;

        assert!(
            matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN),
            "the right to start a run must not imply the right to speak into one: {status}"
        );
        assert!(
            messages.try_recv().is_err(),
            "an unauthorised steer must not reach the run"
        );
    }

    /// The one case that goes through: running, opted in, not a triage run, asked for by the control
    /// token. Asserted on the delivery and not only on the status, because a route that answers
    /// "accepted" and drops the text is worse than one that refuses — the caller believes the run was
    /// told.
    #[tokio::test]
    async fn steering_a_running_opted_in_run_delivers_the_message() {
        let state = test_state().await;
        let (run_id, mut messages) = run_to_steer(&state, "running", "real", true).await;

        let status = steer(state.clone(), run_id, "test-token").await;

        assert!(status.is_success(), "{status}");
        let delivered = messages
            .try_recv()
            .expect("an accepted steer must reach the run");
        assert!(
            delivered.contains(STEERING_MESSAGE),
            "the run must receive what was sent: {delivered}"
        );
    }

    /// A preset records WHAT to run, never who may speak into the run afterwards — `run_presets` has
    /// no column that could say otherwise, and every preset already stored was written before
    /// steering existed. Running one must therefore not be a way to obtain a listening run its
    /// author never asked for, which is the one way a stored row could hand out an opt-in nobody
    /// made.
    #[tokio::test]
    async fn a_preset_run_is_never_steerable() {
        let state = test_state().await;
        let preset = crate::presets::create(
            &state.pool,
            "nightly",
            crate::runs::CreateRunRequest {
                prompt: "do the nightly thing".to_owned(),
                project_id: None,
                cwd: None,
                mode: "real".to_owned(),
                steerable: false,
            },
        )
        .await
        .expect("a real-mode preset needs neither a project nor a worktree");

        let response = api_token_request(
            state.clone(),
            "POST",
            &format!("/presets/{}/run", preset.id),
            "test-token",
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        let steerable: Vec<i64> = sqlx::query_scalar("SELECT steerable FROM runs")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            steerable,
            vec![0],
            "a preset must not launch a run holding a stdin its author never asked for"
        );
    }

    /// A runner that models the single thing a steerable launch changes: `--input-format stream-json`
    /// makes the CLI read turns until stdin closes, so this returns only once the turn channel is
    /// gone. That is what makes the test below able to fail — a runner that answered immediately
    /// would reach a terminal status whether or not anything ever closed the channel.
    ///
    /// It records the turns it was handed, so a refusal can be asserted on what the run received
    /// rather than only on a status code.
    struct StdinEofRunner {
        heard: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl crate::runner::CommandRunner for StdinEofRunner {
        async fn run_prompt(
            &self,
            mut request: crate::runner::RunRequest,
            _session_tx: tokio::sync::mpsc::UnboundedSender<String>,
            _transcript: Arc<std::sync::Mutex<String>>,
        ) -> std::io::Result<crate::runner::RunOutcome> {
            if let Some(messages) = request.messages.as_mut() {
                while let Some(text) = messages.recv().await {
                    self.heard.lock().unwrap().push(text);
                }
            }
            Ok(crate::runner::RunOutcome {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                session_id: request.session_id.clone(),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
            })
        }
    }

    /// A steerable run has to be able to STOP, and until it could be closed it could not.
    ///
    /// The daemon holds the run's sender for the run's whole life, which holds the CLI's stdin open;
    /// a run nobody had anything more to say to therefore sat there until its clock ran out and was
    /// recorded `timed_out` — a failure status for a run that did exactly what it was asked. The
    /// wall clock here is deliberately short, so that is the status this would land on if closing
    /// did nothing: the assertion below distinguishes "ended normally" from "ended at all".
    ///
    /// Closing twice is not an error, and a closed run refuses further turns exactly as every other
    /// unreachable run does — asserted on the delivery too, because a route that answers "accepted"
    /// and drops the text tells the caller the run was informed.
    #[tokio::test]
    async fn a_steerable_run_closed_by_its_caller_ends_normally() {
        let mut state = test_state().await;
        state.run_timeout = std::time::Duration::from_secs(2);
        let heard = Arc::new(std::sync::Mutex::new(Vec::new()));
        state.runner = Arc::new(StdinEofRunner {
            heard: Arc::clone(&heard),
        });

        let run_id = crate::runs::create_run_inner(
            &state,
            "keep going".to_string(),
            None,
            None,
            "real",
            true,
        )
        .await
        .expect("a real-mode run may ask to be steerable");

        let status_of = |state: AppState| async move {
            sqlx::query_scalar::<_, String>("SELECT status FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap()
        };

        assert_eq!(
            steer(state.clone(), run_id, "test-token").await,
            StatusCode::ACCEPTED,
            "the run is running and opted in, so its turn must be taken"
        );
        assert_eq!(
            status_of(state.clone()).await,
            "running",
            "a run still holding an open channel has not finished"
        );

        let close = |state: AppState| async move {
            api_token_request(
                state,
                "DELETE",
                &format!("/runs/{run_id}/message"),
                "test-token",
                None,
            )
            .await
            .status()
        };
        let closed = close(state.clone()).await;
        assert!(closed.is_success(), "{closed}");
        let closed_again = close(state.clone()).await;
        assert!(
            closed_again.is_success(),
            "a channel that is already closed is the state the caller asked for: {closed_again}"
        );

        let refused = steer(state.clone(), run_id, "test-token").await;
        assert!(
            refused.is_client_error(),
            "a closed run must refuse a turn as every other unreachable run does: {refused}"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let final_status = loop {
            let status = status_of(state.clone()).await;
            if status != "running" {
                break status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "a closed run must reach a terminal status"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert_eq!(
            final_status, "completed",
            "closing the channel ends the turn normally; a run that could only stop on its deadline \
             would be recorded timed_out"
        );

        assert_eq!(
            *heard.lock().unwrap(),
            vec![STEERING_MESSAGE.to_string()],
            "the run must hear the turn it was sent and nothing sent after it was closed"
        );
    }

    #[tokio::test]
    async fn autopilot_kill_get_returns_false_by_default() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/kill")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed, serde_json::json!({ "engaged": false }));
    }

    #[tokio::test]
    async fn autopilot_kill_get_returns_true_after_post_engages_switch() {
        let app = build_router(test_state().await);
        let post_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/autopilot/kill")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({ "engaged": true })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post_response.status(), StatusCode::NO_CONTENT);

        let get_response = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/kill")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(get_response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(get_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed, serde_json::json!({ "engaged": true }));
    }

    #[tokio::test]
    async fn autopilot_kill_get_rejects_requests_without_bearer_token() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/kill")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn feed_scope_all_returns_global_and_project_rows() {
        let state = test_state().await;
        let pool = state.pool.clone();
        crate::feed::append(&pool, None, "global", "global summary", None)
            .await
            .unwrap();
        crate::feed::append(
            &pool,
            Some("project-a"),
            "project",
            "project a summary",
            None,
        )
        .await
        .unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/feed?scope=all")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let entries = parsed.as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["summary"], "project a summary");
        assert_eq!(entries[1]["summary"], "global summary");
    }

    #[tokio::test]
    async fn feed_project_query_returns_only_that_project() {
        let state = test_state().await;
        let pool = state.pool.clone();
        crate::feed::append(&pool, None, "global", "global summary", None)
            .await
            .unwrap();
        crate::feed::append(
            &pool,
            Some("project-a"),
            "project",
            "project a summary",
            None,
        )
        .await
        .unwrap();
        crate::feed::append(
            &pool,
            Some("project-b"),
            "project",
            "project b summary",
            None,
        )
        .await
        .unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/feed?project_id=project-a")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let entries = parsed.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["project_id"], "project-a");
        assert_eq!(entries[0]["summary"], "project a summary");
    }

    #[tokio::test]
    async fn feed_without_query_returns_only_global_rows() {
        let state = test_state().await;
        let pool = state.pool.clone();
        crate::feed::append(&pool, None, "global", "global summary", None)
            .await
            .unwrap();
        crate::feed::append(
            &pool,
            Some("project-a"),
            "project",
            "project a summary",
            None,
        )
        .await
        .unwrap();
        let legacy_entries = crate::feed::list_feed(&pool, None, 50).await.unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/feed")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let entries = parsed.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["project_id"], serde_json::Value::Null);
        assert_eq!(entries[0]["summary"], "global summary");
        assert_eq!(parsed, serde_json::to_value(legacy_entries).unwrap());
    }

    fn preset_body(name: &str, prompt: &str, mode: &str) -> Body {
        Body::from(
            serde_json::json!({
                "name": name,
                "prompt": prompt,
                "project_id": "project-a",
                "cwd": "C:/repo/project-a",
                "mode": mode,
            })
            .to_string(),
        )
    }

    async fn preset_response(
        state: AppState,
        method: &str,
        uri: &str,
        body: Body,
    ) -> (StatusCode, serde_json::Value) {
        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn presets_are_stored_and_duplicate_or_unknown_requests_are_mapped() {
        let state = test_state().await;
        let (status, created) = preset_response(
            state.clone(),
            "POST",
            "/presets",
            preset_body("daily", "check the branch", "real"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = created["id"].as_i64().unwrap();

        let (status, fetched) = preset_response(
            state.clone(),
            "GET",
            &format!("/presets/{id}"),
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fetched["name"], "daily");
        assert_eq!(fetched["prompt"], "check the branch");

        assert_eq!(
            preset_response(
                state.clone(),
                "POST",
                "/presets",
                preset_body("daily", "another", "real"),
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            preset_response(state.clone(), "GET", "/presets/999", Body::empty())
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            preset_response(
                state,
                "POST",
                "/presets",
                Body::from(r#"{"name":"bad","prompt":"x","mode":"worktree"}"#),
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn a_preset_run_obeys_the_global_kill_switch() {
        let state = test_state().await;
        let (_, preset) = preset_response(
            state.clone(),
            "POST",
            "/presets",
            preset_body("stopped", "do not start", "real"),
        )
        .await;
        crate::autopilot::set_kill_switch(&state.pool, true)
            .await
            .unwrap();

        assert_eq!(
            preset_response(
                state,
                "POST",
                &format!("/presets/{}/run", preset["id"].as_i64().unwrap()),
                Body::empty(),
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn a_preset_run_uses_the_saved_run_request_fields() {
        let state = test_state().await;
        let (_, preset) = preset_response(
            state.clone(),
            "POST",
            "/presets",
            preset_body("launch", "inspect project", "real"),
        )
        .await;
        let (_, started) = preset_response(
            state.clone(),
            "POST",
            &format!("/presets/{}/run", preset["id"].as_i64().unwrap()),
            Body::empty(),
        )
        .await;
        let id = started["id"].as_i64().unwrap();
        let row: (String, Option<String>, Option<String>, String) =
            sqlx::query_as("SELECT prompt, project_id, cwd, mode FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(row.0, "inspect project");
        assert_eq!(row.1.as_deref(), Some("project-a"));
        assert_eq!(row.2.as_deref(), Some("C:/repo/project-a"));
        assert_eq!(row.3, "real");
    }

    #[tokio::test]
    async fn feed_search_query_and_time_bound_filter_results() {
        let state = test_state().await;
        let pool = state.pool.clone();
        sqlx::query(
            "INSERT INTO feed (project_id, kind, summary, created_at)
             VALUES ('project-a', 'worktree_run_completed', 'Autopilot March work',
                     '2026-03-12T00:00:00+00:00'),
                    ('project-a', 'worktree_run_completed', 'Autopilot April work',
                     '2026-04-01T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/feed?scope=all&q=Autopilot%20March&since=2026-03-01T00%3A00%3A00Z")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 1);
        assert_eq!(parsed[0]["summary"], "Autopilot March work");
    }

    #[tokio::test]
    async fn runs_search_filters_by_project_and_hides_command_output() {
        let state = test_state().await;
        let pool = state.pool.clone();
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, stdout, stderr, created_at)
             VALUES ('project-a', 'Autopilot March work', 'completed', 'worktree',
                     'not for the index', 'also not for the index', '2026-03-12T00:00:00+00:00'),
                    ('project-b', 'Autopilot March work', 'completed', 'worktree',
                     'not for the index', 'also not for the index', '2026-03-13T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/runs?project_id=project-a")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let entries = parsed.as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["project_id"], "project-a");
        assert!(entries[0].get("stdout").is_none());
        assert!(entries[0].get("stderr").is_none());
    }

    #[tokio::test]
    async fn malformed_search_bound_returns_bad_request() {
        let response = build_router(test_state().await)
            .oneshot(
                Request::builder()
                    .uri("/feed?since=not-a-timestamp")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn non_numeric_search_limit_returns_bad_request() {
        let response = build_router(test_state().await)
            .oneshot(
                Request::builder()
                    .uri("/runs?limit=all")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_proposals_returns_pending_action_approvals() {
        let state = test_state().await;
        let pool = state.pool.clone();
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, created_at)
             VALUES (10, 'first run', 'awaiting_approval', 'worktree', '2026-07-20T12:00:00Z'),
                    (11, 'second run', 'awaiting_approval', 'worktree', '2026-07-20T12:01:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        proposals::create_action_approval(
            &pool,
            10,
            Some("s10"),
            Some("p"),
            "Bash",
            "first pending",
            None,
        )
        .await
        .unwrap();
        proposals::create_action_approval(
            &pool,
            11,
            Some("s11"),
            Some("p"),
            "Edit",
            "second pending",
            None,
        )
        .await
        .unwrap();
        let approved = proposals::create_action_approval(
            &pool,
            12,
            Some("s12"),
            Some("p"),
            "Write",
            "already approved",
            None,
        )
        .await
        .unwrap();
        assert!(
            proposals::transition(&pool, approved, "approved", "x")
                .await
                .unwrap()
        );
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/proposals")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let entries = parsed.as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["tool_name"], "Bash");
        assert_eq!(entries[0]["reasoning"], "first pending");
        assert_eq!(entries[0]["run_id"], 10);
        assert_eq!(entries[1]["tool_name"], "Edit");
        assert_eq!(entries[1]["reasoning"], "second pending");
        assert_eq!(entries[1]["run_id"], 11);
    }

    #[tokio::test]
    async fn list_proposals_rejects_without_token() {
        let app = build_router(test_state().await);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/proposals")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn reject_endpoint_discards_and_returns_204() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let result = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('reject this run', 'awaiting_approval', 'worktree', '2026-07-20T12:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let run_id = result.last_insert_rowid();
        let proposal_id = proposals::create_action_approval(
            &pool,
            run_id,
            Some("s"),
            Some("p"),
            "Bash",
            "push needs approval",
            None,
        )
        .await
        .unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/proposals/{proposal_id}/reject"))
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let proposal = proposals::get(&pool, proposal_id).await.unwrap().unwrap();
        assert_eq!(proposal.status, "rejected");
        let run_status = sqlx::query_scalar::<_, String>("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(run_status, "cancelled");
    }

    #[tokio::test]
    async fn reject_unknown_proposal_returns_404() {
        let app = build_router(test_state().await);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/proposals/999999/reject")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn approve_endpoint_resumes_and_returns_the_resume_run_id() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let created_at = chrono::Utc::now().to_rfc3339();
        let result = sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, session_id, mode, created_at)
             VALUES ('proj', 'C:/worktrees/proj/run-paused', 'x', 'awaiting_approval',
                     'sess-a', 'worktree', ?)",
        )
        .bind(&created_at)
        .execute(&pool)
        .await
        .unwrap();
        let original_run_id = result.last_insert_rowid();
        sqlx::query(
            "INSERT INTO worktrees
             (owner_kind, owner_id, project_id, project_root, path, branch, created_at)
             VALUES ('run', ?, 'proj', 'C:/repos/proj', 'C:/worktrees/proj/run-paused', ?, ?)",
        )
        .bind(original_run_id)
        .bind(format!("nucleos/run-{original_run_id}"))
        .bind(&created_at)
        .execute(&pool)
        .await
        .unwrap();
        let proposal_id = proposals::create_action_approval(
            &pool,
            original_run_id,
            Some("sess-a"),
            Some("proj"),
            "Bash",
            "push needs approval",
            Some("{}"),
        )
        .await
        .unwrap();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/proposals/{proposal_id}/approve"))
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(parsed["resume_run_id"].is_number());
        let proposal = proposals::get(&pool, proposal_id).await.unwrap().unwrap();
        assert_eq!(proposal.status, "approved");
    }

    #[tokio::test]
    async fn approve_unknown_proposal_returns_404() {
        let app = build_router(test_state().await);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/proposals/999999/approve")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn autopilot_budget_get_returns_defaults() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/budget")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            parsed,
            serde_json::json!({
                "limit_usd": null,
                "period": "monthly",
                "hourly_limit_usd": null,
                "per_run_reserve_usd": 0.5,
                "time_cost_per_hour_usd": 3.0,
                "window_spend_usd": 0.0,
                "hourly_spend_usd": 0.0,
                "paused": false,
                "reason": null
            })
        );
    }

    #[tokio::test]
    async fn autopilot_budget_post_sets_config_and_get_reflects_it() {
        let app = build_router(test_state().await);
        let post = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/autopilot/budget")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "limit_usd": 50.0,
                            "period": "weekly",
                            "hourly_limit_usd": 5.0,
                            "per_run_reserve_usd": 1.0,
                            "time_cost_per_hour_usd": 2.0
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post.status(), StatusCode::OK);

        let get = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/budget")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        let body = axum::body::to_bytes(get.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["limit_usd"], serde_json::json!(50.0));
        assert_eq!(parsed["period"], serde_json::json!("weekly"));
        assert_eq!(parsed["hourly_limit_usd"], serde_json::json!(5.0));
        assert_eq!(parsed["per_run_reserve_usd"], serde_json::json!(1.0));
        assert_eq!(parsed["time_cost_per_hour_usd"], serde_json::json!(2.0));
    }

    #[tokio::test]
    async fn autopilot_budget_post_rejects_invalid_period() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/autopilot/budget")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "limit_usd": 10.0,
                            "period": "yearly",
                            "hourly_limit_usd": null,
                            "per_run_reserve_usd": 0.5,
                            "time_cost_per_hour_usd": 3.0
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn autopilot_budget_get_reports_paused_when_over_budget() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let app = build_router(state);

        // $5 of autonomous spend recorded "now" (in the current window and hour).
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, cost_usd, created_at, completed_at)
             VALUES ('proj', 'prior spend', 'completed', 'worktree', 5.0, ?, ?)",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();

        let post = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/autopilot/budget")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "limit_usd": 1.0,
                            "period": "monthly",
                            "hourly_limit_usd": null,
                            "per_run_reserve_usd": 0.5,
                            "time_cost_per_hour_usd": 3.0
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post.status(), StatusCode::OK);

        let get = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/budget")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        let body = axum::body::to_bytes(get.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["paused"], serde_json::json!(true));
        assert_eq!(parsed["window_spend_usd"], serde_json::json!(5.0));
        assert!(parsed["reason"].is_string());
    }

    #[tokio::test]
    async fn autopilot_kill_scoped_get_is_empty_by_default() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/kill/scoped")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed, serde_json::json!([]));
    }

    #[tokio::test]
    async fn autopilot_kill_scoped_post_then_get_reflects_it() {
        let app = build_router(test_state().await);
        let post = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/autopilot/kill/scoped")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "scope_type": "project",
                            "scope_id": "alpha",
                            "engaged": true
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post.status(), StatusCode::NO_CONTENT);

        let get = app
            .oneshot(
                Request::builder()
                    .uri("/autopilot/kill/scoped")
                    .header("Authorization", "Bearer test-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        let body = axum::body::to_bytes(get.into_body(), usize::MAX)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            parsed,
            serde_json::json!([
                { "scope_type": "project", "scope_id": "alpha", "engaged": true }
            ])
        );
    }

    #[tokio::test]
    async fn autopilot_kill_scoped_post_rejects_invalid_scope_type() {
        let app = build_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/autopilot/kill/scoped")
                    .header("Authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "scope_type": "bogus",
                            "scope_id": "x",
                            "engaged": true
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// Seeds one shadow-mode run whose `read-local` class has `reviewed` approved decisions plus one
    /// still-unreviewed decision, and returns that unreviewed decision's id.
    ///
    /// Alongside it, a `push-merge-deploy` class that ALREADY clears the bar — because `promotable`
    /// also requires one ready class the classifier withheld. Without it no project seeded here could
    /// ever be promotable, and these three tests would all be asserting against a project held back
    /// by a criterion none of them is about: one would fail, and the other two would pass for a
    /// reason their names deny.
    async fn seed_shadow_class(pool: &sqlx::SqlitePool, project_id: &str, reviewed: usize) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES (?, 'seed', 'completed', 'shadow', '2026-07-27T00:00:00Z')",
        )
        .bind(project_id)
        .execute(pool)
        .await
        .unwrap();
        let run_id: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(pool)
            .await
            .unwrap();

        // Seeded first and complete, so the `read-local` class below stays the one whose crossing
        // these tests observe. `reject` agrees with `pending_approval`, so the class is unanimous.
        for index in 0..shadow::READINESS_MIN_REVIEWED {
            sqlx::query(
                "INSERT INTO shadow_decisions
                 (run_id, tool_name, tool_input, decision, reason, action_class,
                  classifier_version, human_verdict, reviewed_at, created_at)
                 VALUES (?, 'Bash', ?, 'pending_approval', 'seed', 'push-merge-deploy', 1,
                         'reject', NULL, '2026-07-27T00:00:00Z')",
            )
            .bind(run_id)
            .bind(format!(r#"{{"command":"git push seed-{index}"}}"#))
            .execute(pool)
            .await
            .unwrap();
        }

        let mut last = 0;
        for index in 0..=reviewed {
            let verdict = if index < reviewed {
                Some("approve")
            } else {
                None
            };
            // A DISTINCT action per row. Readiness counts distinct actions rather than rows, so
            // seeding one repeated `tool_input` would seed one piece of evidence N times and the
            // class would never clear the bar — which is the point of that rule, not a fixture
            // detail to work around.
            sqlx::query(
                "INSERT INTO shadow_decisions
                 (run_id, tool_name, tool_input, decision, reason, action_class,
                  classifier_version, human_verdict, reviewed_at, created_at)
                 VALUES (?, 'Read', ?, 'allow', 'seed', 'read-local', 1, ?, NULL,
                         '2026-07-27T00:00:00Z')",
            )
            .bind(run_id)
            .bind(format!(r#"{{"file_path":"seed-{index}.rs"}}"#))
            .bind(verdict)
            .execute(pool)
            .await
            .unwrap();
            last = sqlx::query_scalar("SELECT last_insert_rowid()")
                .fetch_one(pool)
                .await
                .unwrap();
        }
        last
    }

    async fn post_verdict(state: AppState, id: i64) -> StatusCode {
        build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/shadow-decisions/{id}/verdict"))
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({ "verdict": "approve" })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    async fn promotion_feed_rows(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'promotion_ready'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn verdict_that_clears_the_bar_announces_the_project_as_promotable() {
        let state = test_state().await;
        let pool = state.pool.clone();
        // Nine reviewed leaves the class one short of the ten-review floor.
        let last = seed_shadow_class(&pool, "project-a", 9).await;

        assert_eq!(promotion_feed_rows(&pool).await, 0);
        assert_eq!(post_verdict(state, last).await, StatusCode::NO_CONTENT);

        assert_eq!(promotion_feed_rows(&pool).await, 1);
        let summary: String =
            sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'promotion_ready'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(summary.contains("project-a"), "got: {summary}");
    }

    #[tokio::test]
    async fn verdict_below_the_bar_announces_nothing() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let last = seed_shadow_class(&pool, "project-a", 3).await;

        assert_eq!(post_verdict(state, last).await, StatusCode::NO_CONTENT);

        assert_eq!(promotion_feed_rows(&pool).await, 0);
    }

    #[tokio::test]
    async fn an_already_promotable_project_is_not_announced_again() {
        let state = test_state().await;
        let pool = state.pool.clone();
        // Ten reviewed already clears the bar, so the eleventh verdict is not a crossing.
        let last = seed_shadow_class(&pool, "project-a", 10).await;

        assert_eq!(post_verdict(state, last).await, StatusCode::NO_CONTENT);

        assert_eq!(promotion_feed_rows(&pool).await, 0);
    }
}
