use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateRunRequest {
    pub prompt: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_run_mode")]
    pub mode: String,
    /// Whether this run may be spoken to again after it starts.
    ///
    /// `#[serde(default)]` is load-bearing rather than tidy: every caller that predates this field —
    /// the shell, the sidecars, and every preset already stored — sends a body without it, and the
    /// answer for all of them has to stay the one they were built against. Opting in is therefore
    /// something a caller does on purpose, in the request, and never something a run acquires by
    /// being created a particular way.
    #[serde(default)]
    pub steerable: bool,
}

fn default_run_mode() -> String {
    "real".to_owned()
}

#[derive(Serialize, Deserialize)]
pub struct CreateRunResponse {
    pub id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct AwaitingRun {
    pub id: i64,
    pub project_id: Option<String>,
    pub prompt: String,
    pub cwd: Option<String>,
    pub created_at: String,
}

/// A lean run index entry. It intentionally excludes the full prompt and captured command output.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct RunSearchResult {
    pub id: i64,
    pub project_id: Option<String>,
    pub status: String,
    pub mode: String,
    pub created_at: String,
    pub completed_at: Option<String>,
    pub cost_usd: Option<f64>,
    pub prompt_excerpt: String,
}

#[derive(Debug, Clone)]
pub struct SearchFilter {
    pub project_id: Option<String>,
    pub status: Option<String>,
    pub mode: Option<String>,
    pub q: Option<String>,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    pub limit: i64,
}

/// Keep search results useful without turning the index into a prompt or output retrieval endpoint.
const PROMPT_EXCERPT_CHARS: i64 = 500;

fn escape_like(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub async fn list_awaiting_approval(pool: &sqlx::SqlitePool) -> sqlx::Result<Vec<AwaitingRun>> {
    sqlx::query_as::<_, AwaitingRun>(
        "SELECT id, project_id, prompt, cwd, created_at
         FROM runs WHERE status = 'awaiting_approval' ORDER BY id",
    )
    .fetch_all(pool)
    .await
}

/// Searches run metadata newest-first. The result is an index, so it never returns stdout or stderr.
pub async fn search(
    pool: &sqlx::SqlitePool,
    filter: &SearchFilter,
) -> sqlx::Result<Vec<RunSearchResult>> {
    let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(format!(
        "SELECT id, project_id, status, mode, created_at, completed_at, cost_usd, \
         substr(prompt, 1, {PROMPT_EXCERPT_CHARS}) AS prompt_excerpt FROM runs WHERE 1 = 1"
    ));

    if let Some(project_id) = &filter.project_id {
        query.push(" AND project_id = ").push_bind(project_id);
    }
    if let Some(status) = &filter.status {
        query.push(" AND status = ").push_bind(status);
    }
    if let Some(mode) = &filter.mode {
        query.push(" AND mode = ").push_bind(mode);
    }
    if let Some(q) = &filter.q {
        query
            .push(" AND prompt LIKE ")
            .push_bind(format!("%{}%", escape_like(q)))
            .push(" ESCAPE '\\'");
    }
    if let Some(since) = &filter.since {
        query
            .push(" AND created_at >= ")
            .push_bind(since.to_rfc3339());
    }
    if let Some(until) = &filter.until {
        query
            .push(" AND created_at <= ")
            .push_bind(until.to_rfc3339());
    }
    query
        .push(" ORDER BY created_at DESC, id DESC LIMIT ")
        .push_bind(filter.limit);

    query
        .build_query_as::<RunSearchResult>()
        .fetch_all(pool)
        .await
}

#[derive(Debug)]
pub enum CreateRunError {
    Invalid(&'static str),
    Busy,
    Worktree(std::io::Error),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for CreateRunError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

#[derive(Debug)]
pub enum ResumeError {
    ProposalNotFound,
    ProposalNotPending,
    NotResumable(&'static str),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ResumeError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for CreateRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::Busy => formatter.write_str("run creation is busy"),
            Self::Worktree(error) => write!(formatter, "worktree provisioning failed: {error}"),
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}

impl std::error::Error for CreateRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Worktree(error) => Some(error),
            Self::Db(error) => Some(error),
            Self::Invalid(_) | Self::Busy => None,
        }
    }
}

// `FromRow` rather than a positional tuple: sqlx only implements `FromRow` for tuples up to 16
// elements, and this row outgrew that. Deriving it also removes the column-order-to-field-order
// correspondence that a tuple made load-bearing and invisible — adding a column in the middle of
// the SELECT used to silently shift every field after it.
#[derive(Serialize, Deserialize, sqlx::FromRow)]
pub struct RunStatusResponse {
    pub id: i64,
    pub project_id: Option<String>,
    pub status: String,
    pub gate_status: Option<String>,
    pub gate_exit_code: Option<i32>,
    pub gate_output: Option<String>,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub num_turns: Option<i64>,
    pub context_fill: Option<i64>,
    /// Whether this run accepts `POST /runs/{id}/message`. Reported because a caller that is
    /// refused otherwise cannot tell a run that never opted in from one that has already ended.
    pub steerable: bool,
    /// The run that continued this one after a context handoff, when there was one. Without it the
    /// link the handoff records is reachable only by reading the database directly.
    pub successor_run_id: Option<i64>,
}

pub async fn create_run(
    State(state): State<AppState>,
    Json(req): Json<CreateRunRequest>,
) -> Result<Json<CreateRunResponse>, StatusCode> {
    // Checked here and not only in the tick loops, because the loops are not the only way a run
    // starts. An autonomous run's own key no longer opens this route (`auth::Scope::Run`), so the
    // original escape — a stopped run spawning its own successors through this endpoint — is closed
    // twice over. It stays checked here regardless: the shell and the sidecars still reach it with
    // the control token, and the emergency stop has to hold against them too.
    //
    // Only the GLOBAL switch, deliberately. The scoped kills, the budget and the WIP limit pace
    // proactive autonomy, and a person asking for a run through the shell is not that. The global
    // switch is the emergency stop, and an emergency stop with exemptions is not one.
    //
    // Fails closed: a switch that cannot be read stops runs rather than starting them.
    match crate::autopilot::kill_switch_engaged(&state.pool).await {
        Ok(false) => {}
        Ok(true) => return Err(StatusCode::CONFLICT),
        Err(error) => {
            tracing::warn!(%error, "create_run: could not read the kill switch — refusing");
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    }

    // Uncancellable: the run row is INSERTed `running` before the worktree is provisioned, so
    // `git worktree add` holds that window open for as long as git takes. A request dropped inside
    // it strands a `running` worktree row with no task and no abort handle — `/cancel` answers 404,
    // the GC skips it, and `one_open_worktree_run_per_project` (migration 0009) blocks the whole
    // project until the daemon restarts, the only thing that reconciles `running` rows.
    let id = crate::http::uncancellable(async move {
        create_run_inner(
            &state,
            req.prompt,
            req.project_id,
            req.cwd,
            &req.mode,
            req.steerable,
        )
        .await
    })
    .await?
    .map_err(|error| crate::http::create_run_status(&error))?;

    Ok(Json(CreateRunResponse { id }))
}

/// Autonomous runs (mode shadow/worktree) retry a launch failure up to this many TOTAL attempts.
/// Only the pre-execution launch-failure path retries (the CLI never ran, so no work is double-applied);
/// a run that executed and failed, and a timeout, are never retried.
const MAX_AUTONOMOUS_ATTEMPTS: u32 = 2;

/// The environment every autopilot/assistant CLI run needs: the daemon URL + token so the
/// PreToolUse hook can call back, and NUCLEOS_RUN_ID (== runs.id, spec §3.3) so the hook echoes it
/// back and the core can validate — and, for a `pending_approval`, terminate — the right run.
///
/// `token` is the caller's decision and not read from `state` on purpose. It used to be
/// `state.token` for every run, which handed a `shadow` run — a mode that has Bash and whose
/// classifier calls `echo $NUCLEOS_DAEMON_TOKEN` a `read-local` action — the key that approves
/// proposals and disengages the kill switch. Autonomous runs now get their own scoped key
/// (`auth::mint_run_token`); only orchestrator turns still carry the control token, and only
/// because `ToolPolicy::McpOnly` leaves them nothing to read it with.
pub(crate) fn run_env(token: &str, id: i64) -> Vec<(String, String)> {
    vec![
        (
            "NUCLEOS_DAEMON_URL".to_string(),
            "http://127.0.0.1:8791".to_string(),
        ),
        ("NUCLEOS_DAEMON_TOKEN".to_string(), token.to_string()),
        ("NUCLEOS_RUN_ID".to_string(), id.to_string()),
    ]
}

/// Mints a run's own daemon key and stores its secret, returning what goes in the environment.
///
/// Called before the CLI is spawned, never after: the hook fires on the run's first tool call, and
/// a secret that lands in the row a moment later would 401 that call for reasons no log explains.
/// A failure to store is not fatal — the run proceeds with a key that authenticates nothing, so its
/// tool calls are refused rather than ungoverned, which is the right direction to fail in.
async fn mint_run_token(pool: &sqlx::SqlitePool, id: i64) -> String {
    let (token, secret) = crate::auth::mint_run_token(id);
    if let Err(error) = sqlx::query("UPDATE runs SET token = ? WHERE id = ?")
        .bind(&secret)
        .bind(id)
        .execute(pool)
        .await
    {
        tracing::warn!(
            run_id = id,
            %error,
            "could not store the run's token — its tool calls will be refused"
        );
    }
    token
}

/// Records that a run's context now holds text a third party wrote.
///
/// One direction only: a turn that has read a mail body cannot un-read it, and every tool call that
/// follows in that turn is downstream of it.
///
/// On the row rather than in memory because the two readers are not the same task — `hooks.rs`
/// decides the next tool call, `assistant.rs` decides whether the turn may leave a resumable
/// session behind — and because a daemon restart in between must not lose it. A resumed session
/// carries the same words whether or not the process that read them is still alive.
pub(crate) async fn mark_untrusted_context(pool: &sqlx::SqlitePool, id: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE runs SET read_untrusted = 1 WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Whether this run has read third-party text.
///
/// A row that is not there answers `true`. The callers use this to decide whether to REFUSE
/// something, so the absent-row case has to fail in the direction that refuses: an id naming no run
/// is not evidence that a turn is clean.
pub(crate) async fn read_untrusted_context(pool: &sqlx::SqlitePool, id: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar::<_, i64>("SELECT read_untrusted FROM runs WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map(|flag| flag.unwrap_or(1) != 0)
}

/// PURE: barrier 1 of spec §5.5 — the tools a run in `mode` launches with.
///
/// One function rather than the same `if` at each reader, because the two readers ask opposite
/// questions about the same fact: `create_run_inner` asks what to launch, and
/// `http::post_run_message` asks whether a live run may be given a second author. A toolless run
/// exists to read words nobody vouches for, so it is the last run that may be spoken to — and the day
/// a second such mode appears, both answers have to change together or the barrier narrows to one
/// mode without anyone deciding to narrow it.
pub(crate) fn tool_policy_for_mode(mode: &str) -> crate::runner::ToolPolicy {
    if mode == crate::email::TRIAGE_MODE {
        crate::runner::ToolPolicy::None
    } else {
        crate::runner::ToolPolicy::Unrestricted
    }
}

/// PURE: the wall clock a run in `mode` gets, given the interactive default `base`.
///
/// `shadow` and `worktree` are the modes that check out a tree, edit it, build it and run a gate,
/// and they are the ones measured dying on the 600-second deadline with work still open (see
/// `state::AUTONOMOUS_RUN_TIMEOUT_MULTIPLIER`). `email_triage` is autonomous too and deliberately
/// stays on the short clock: it classifies one message against a local model, so a triage run still
/// going after ten minutes is stuck, not busy.
///
/// Derived from `base` rather than given a constant of its own, so a test that shortens the clock
/// still gets a short one, and an operator who tunes the deadline moves both together.
fn run_timeout_for_mode(base: std::time::Duration, mode: &str) -> std::time::Duration {
    if mode == "shadow" || mode == "worktree" {
        base * crate::state::AUTONOMOUS_RUN_TIMEOUT_MULTIPLIER
    } else {
        base
    }
}

/// Releases a run's abort handle when its task ends — by returning, by panicking, or by being
/// aborted, including aborted before its first poll, when the task drops its captured state without
/// running a line of the body.
struct Registration {
    handles: crate::state::RunHandles,
    /// Released here rather than by the steering endpoint or the runner, because the same three ways
    /// a task can end are the three ways a run stops listening — and a sender outliving its receiver
    /// would let `post_run_message` accept a turn nothing will ever read.
    messages: crate::state::RunMessages,
    id: i64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.handles.lock().unwrap().remove(&self.id);
        self.messages.lock().unwrap().remove(&self.id);
    }
}

/// Tells a steerable run that no more turns are coming.
///
/// Dropping the run's sender is the whole mechanism, and it is the same event the registry already
/// uses to mean "this run is no longer listening". `--input-format stream-json` ends the CLI's turn
/// at stdin EOF, and the writing task reaches EOF only once the last sender is gone — so a sender
/// kept for the run's whole life is not neutral bookkeeping. It holds stdin open, and a steerable
/// run then had no way to finish except its progress deadline: recorded `timed_out`, a failure
/// status, for a run that did exactly what it was asked and was simply never told to stop.
///
/// Idempotent, and reports nothing. A run with no channel is a run already not listening — because
/// it never opted in, because it has ended, or because it was closed a moment ago — and all three
/// are the state the caller asked for. What a closed run does with a later turn is the refusal
/// matrix's job, unchanged: `post_run_message` finds no sender and refuses it exactly as it refuses
/// every other run nothing is listening to.
pub(crate) fn close_steering_channel(state: &AppState, id: i64) {
    state.run_messages.lock().unwrap().remove(&id);
}

/// Spawn a run's driver task and register its abort handle so an in-flight run can be terminated
/// (cancel / `pending_approval`).
///
/// The handle is released by a guard the task captures rather than by a statement after
/// `body.await`, because a stale entry here is not merely untidy bookkeeping:
/// `hooks::pretooluse_decision` reads this map as its "is this run_id really in flight" check, so an
/// entry that outlives its run lets a finished run be terminated and pended all over again, and
/// `cancel_run` will overwrite a completed run's final status. An abort is already covered — the
/// terminator (`finalize_termination`) removes the entry itself — but a panicking body is not.
///
/// Registration holds the map lock across the spawn on purpose: the guard runs on whichever thread
/// picks the task up, so a body that finishes before the insert would otherwise release a handle
/// that is only inserted afterwards, pinning it for the life of the daemon.
pub(crate) fn spawn_registered<F>(state: &AppState, id: i64, body: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let registration = Registration {
        handles: state.run_handles.clone(),
        messages: state.run_messages.clone(),
        id,
    };
    let mut handles = state.run_handles.lock().unwrap();
    let join = tokio::spawn(async move {
        let _registration = registration;
        body.await;
    });
    handles.insert(id, join.abort_handle());

    // A panic unwinds past every terminal-status write in the body, and nothing awaits this handle,
    // so the panic went unobserved and the run stayed `running` behind a dead task — `/cancel`
    // answering 404, the GC skipping it, and migration 0009 blocking the project until a restart.
    // The guard above releases the abort handle on a panic, which the existing test checks; the
    // database row was the half nobody was watching.
    //
    // A supervisor rather than a guard because writing that status needs to await, and `Drop`
    // cannot. Cancellation is deliberately ignored here: `finalize_termination` owns that path and
    // has already written the status it chose.
    let pool = state.pool.clone();
    tokio::spawn(async move {
        let Err(error) = join.await else { return };
        if error.is_cancelled() {
            return;
        }
        tracing::error!(run_id = id, %error, "run task panicked; recording it as failed");
        let failed = sqlx::query(
            "UPDATE runs SET status = 'failed', completed_at = ? WHERE id = ? AND status = 'running'",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(id)
        .execute(&pool)
        .await;
        warn_on_terminal_write_err(&failed, id, "failed");
    });
}

/// A terminal-status UPDATE failing is not the same severity as a feed write failing: a feed
/// insert is genuinely best-effort, but a lost terminal write leaves the run `running` behind a
/// dead task — the exact jam class this crate guards against elsewhere, reached here through a DB
/// error instead of a dropped future. `rows_affected() == 0` is a lost first-writer race (see the
/// comments at each call site), a legal outcome rather than a failure, so only `Err` warns.
pub(crate) fn warn_on_terminal_write_err(
    result: &Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error>,
    run_id: i64,
    target_status: &str,
) {
    if let Err(error) = result {
        tracing::warn!(run_id, target_status, %error, "terminal-status update failed");
    }
}

/// Stores the runner's complete event stream without making observability part of run correctness.
async fn append_run_events(pool: &sqlx::SqlitePool, run_id: i64, stdout: &str) {
    let created_at = chrono::Utc::now().to_rfc3339();
    for (seq, payload) in stdout.lines().enumerate() {
        let kind = serde_json::from_str::<serde_json::Value>(payload)
            .ok()
            .and_then(|event| {
                event
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "unknown".to_owned());

        // Observability must stay best-effort: a missing or temporarily unavailable history table
        // cannot turn successfully completed work into a failed run.
        let _ = sqlx::query(
            "INSERT INTO run_events (run_id, seq, kind, payload, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(run_id)
        .bind(seq as i64)
        .bind(kind)
        .bind(payload)
        .bind(&created_at)
        .execute(pool)
        .await;
    }
}

/// How often a LIVE run's context fill is copied from the stream mirror into its row.
///
/// Throttled on purpose. The mirror is rewritten on every streamed line — many per second — and the
/// number is only ever read by a human or by the handoff check, neither of which needs line
/// resolution. A period plus a change check bounds this to at most one small UPDATE per run per
/// period, and to zero for a run that is thinking rather than emitting.
const CONTEXT_FILL_PERSIST_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Aborts the task it names when whatever owns it is dropped.
///
/// The run body is left by more paths than it returns from: the wall clock drops its future and
/// `finalize_termination` aborts it, and neither runs a statement placed after the await. A guard is
/// the only cleanup that fires on all of them — the same reason `Registration` is one.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Mirrors a live run's context fill into `runs.context_fill` until the returned guard is dropped.
///
/// The number was measured from the stream all along, but it only ever reached the database in the
/// terminal UPDATE — so `GET /runs/{id}` answered `context_fill: null` for the entire life of every
/// run, which is exactly when someone deciding whether to steer or to hand off wants to read it.
///
/// Written through to the row rather than kept in an `AppState` map beside `run_handles`: a cancel
/// aborts the run task and `finalize_termination` writes only a status, so an in-memory number would
/// be dropped with the task and the killed run — the one most worth inspecting — would report
/// nothing. A column already written outlives every one of those paths.
///
/// Compare-and-set on `status = 'running'` because this task is not the only writer: a tick that
/// lands after the terminal UPDATE must not put a stale number back onto a finished run.
fn mirror_context_fill(
    pool: &sqlx::SqlitePool,
    id: i64,
    context_fill: std::sync::Arc<std::sync::Mutex<Option<i64>>>,
) -> AbortOnDrop {
    let pool = pool.clone();
    AbortOnDrop(
        tokio::spawn(async move {
            let mut persisted: Option<i64> = None;
            loop {
                tokio::time::sleep(CONTEXT_FILL_PERSIST_INTERVAL).await;
                let current = context_fill.lock().map(|fill| *fill).unwrap_or(None);
                if current.is_none() || current == persisted {
                    continue;
                }
                let written = sqlx::query(
                    "UPDATE runs SET context_fill = ? WHERE id = ? AND status = 'running'",
                )
                .bind(current)
                .bind(id)
                .execute(&pool)
                .await;
                // Only a write that landed counts as persisted, so a transient database error is
                // retried on the next tick instead of being remembered as done.
                if written.is_ok() {
                    persisted = current;
                }
            }
        })
        .abort_handle(),
    )
}

/// Reduces the mirrored stream as a fallback for runners that do not publish context separately.
fn observed_context_fill(mirror: &std::sync::Mutex<Option<i64>>, transcript: &str) -> Option<i64> {
    let current = mirror.lock().map(|fill| *fill).unwrap_or(None);
    transcript.lines().fold(current, |fill, line| {
        crate::runner::context_fill_from_line(line, fill)
    })
}

/// The runner abstraction does not expose reliable per-model window metadata, and model aliases can
/// change underneath the daemon. 200k is therefore a conservative floor shared by the supported
/// Claude models: using the floor hands off early rather than risking a context-overflowing run.
const HANDOFF_CONTEXT_LIMIT_FLOOR: i64 = 200_000;
const HANDOFF_CONTINUATION_PROMPT: &str =
    "Continue the previous run in a fresh context. Re-check what remains, then finish the task.";

struct HandoffSuccessor {
    id: i64,
    session_id: String,
}

/// Applies the durable handoff policy to a prepared successor.
///
/// Both inputs to `already_handed_off` come from the run row rather than task-local state. That
/// makes a retry after a daemon restart observe the same decision and prevents a second event or
/// successor link for one crossing.
async fn record_handoff_if_needed(
    pool: &sqlx::SqlitePool,
    run_id: i64,
    successor_run_id: i64,
) -> sqlx::Result<bool> {
    let (context_fill, existing_successor): (Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT context_fill, successor_run_id FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(pool)
            .await?;
    let Some(context_fill) = context_fill else {
        return Ok(false);
    };
    if !crate::handoff::should_hand_off(
        context_fill,
        HANDOFF_CONTEXT_LIMIT_FLOOR,
        existing_successor.is_some(),
    ) {
        return Ok(false);
    }

    crate::handoff::record_handoff(pool, run_id, successor_run_id, context_fill).await?;
    Ok(true)
}

async fn prepare_handoff_successor(
    pool: &sqlx::SqlitePool,
    run_id: i64,
) -> sqlx::Result<Option<HandoffSuccessor>> {
    let (context_fill, existing_successor): (Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT context_fill, successor_run_id FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(pool)
            .await?;
    let Some(context_fill) = context_fill else {
        return Ok(None);
    };
    if !crate::handoff::should_hand_off(
        context_fill,
        HANDOFF_CONTEXT_LIMIT_FLOOR,
        existing_successor.is_some(),
    ) {
        return Ok(None);
    }

    let session_id = crate::auth::generate_uuid_v4();
    let inserted = sqlx::query(
        "INSERT INTO runs (
             project_id, cwd, prompt, status, mode, session_id, read_untrusted, created_at
         )
         SELECT project_id, cwd, ?, 'running', mode, ?, read_untrusted, ?
         FROM runs WHERE id = ?",
    )
    .bind(HANDOFF_CONTINUATION_PROMPT)
    .bind(&session_id)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(run_id)
    .execute(pool)
    .await?;
    if inserted.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    let successor_id = inserted.last_insert_rowid();

    if !record_handoff_if_needed(pool, run_id, successor_id).await? {
        let _ = sqlx::query(
            "UPDATE runs SET status = 'interrupted', completed_at = ? WHERE id = ? AND status = 'running'",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(successor_id)
        .execute(pool)
        .await;
        return Ok(None);
    }

    // A worktree handoff continues in the same checkout. Moving its ownership keeps approval,
    // cancellation, and GC pointed at the live successor instead of the completed predecessor.
    sqlx::query("UPDATE worktrees SET run_id = ? WHERE run_id = ? AND removed_at IS NULL")
        .bind(successor_id)
        .bind(run_id)
        .execute(pool)
        .await?;

    Ok(Some(HandoffSuccessor {
        id: successor_id,
        session_id,
    }))
}

/// Deliberately wide rather than taking an options struct: these are the axes on which a run's
/// lifecycle actually differs (plan-only, resumed, retried, worktree-bound), and naming each one at
/// every call site is what makes those differences readable where the runs are created.
#[derive(Clone)]
enum GateConfig {
    NotConfigured,
    /// The command, and the project root it was read from. They travel together because the gate
    /// verifies the second before trusting the first: a script the run rewrote inside its worktree
    /// is compared against the project root's copy, which is the one the operator configured.
    Command {
        command: String,
        project_root: String,
    },
    Unreadable(String),
}

#[allow(clippy::too_many_arguments)]
async fn spawn_handoff_if_needed(
    state: AppState,
    runner: std::sync::Arc<dyn crate::runner::CommandRunner>,
    run_id: i64,
    original_session_id: String,
    project_id: Option<String>,
    spawn_cwd: Option<std::path::PathBuf>,
    plan_only: bool,
    completion_feed: Option<(String, String)>,
    gate_config: GateConfig,
    max_attempts: u32,
    tool_policy: crate::runner::ToolPolicy,
    run_timeout: std::time::Duration,
) {
    let successor = match prepare_handoff_successor(&state.pool, run_id).await {
        Ok(Some(successor)) => successor,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(run_id, %error, "could not prepare context handoff");
            return;
        }
    };
    let daemon_token = mint_run_token(&state.pool, successor.id).await;
    spawn_run(
        &state,
        runner,
        successor.id,
        HANDOFF_CONTINUATION_PROMPT.to_owned(),
        project_id,
        spawn_cwd,
        plan_only,
        Some(original_session_id),
        successor.session_id,
        true,
        completion_feed,
        gate_config,
        max_attempts,
        tool_policy,
        daemon_token,
        // A successor is not steerable, whatever its predecessor was. `prepare_handoff_successor`
        // writes its row with the column's default, so a listening successor would contradict its
        // own record: `post_run_message` reads the row, refuses, and nothing would ever close the
        // stdin the launch had opened — a run that can only end on a deadline. Continuing a steered
        // conversation across a handoff means giving the successor row the flag too, which is a
        // decision to take deliberately rather than inherit.
        false,
        // Inherited, not re-derived: a successor continues one task, and a handoff that reset the
        // clock would let a run outlive its deadline by handing itself on.
        run_timeout,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_run(
    state: &AppState,
    runner: std::sync::Arc<dyn crate::runner::CommandRunner>,
    id: i64,
    prompt: String,
    project_id: Option<String>,
    spawn_cwd: Option<std::path::PathBuf>,
    plan_only: bool,
    resume_session_id: Option<String>,
    session_id: String,
    fork_session: bool,
    completion_feed: Option<(String, String)>,
    gate_config: GateConfig,
    max_attempts: u32,
    tool_policy: crate::runner::ToolPolicy,
    daemon_token: String,
    steerable: bool,
    run_timeout: std::time::Duration,
) {
    let pool = state.pool.clone();
    let feed_project_id = project_id.clone();
    let progress_timeout = state.progress_timeout;
    let env = run_env(&daemon_token, id);
    let handoff_state = state.clone();
    let run_messages = state.run_messages.clone();

    spawn_registered(state, id, async move {
        let mut attempt: u32 = 1;
        loop {
            // A fresh session channel per attempt: persist session_id the instant the runner parses it
            // (spec §3.3), in a separate task so it lands even if the run is terminated mid-flight.
            let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            {
                let pool = pool.clone();
                tokio::spawn(async move {
                    if let Some(session_id) = session_rx.recv().await {
                        let _ = sqlx::query("UPDATE runs SET session_id = ? WHERE id = ?")
                            .bind(&session_id)
                            .bind(id)
                            .execute(&pool)
                            .await;
                    }
                });
            }

            // Owned out here so it survives the timeout below dropping the run future.
            let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            let context_fill = std::sync::Arc::new(std::sync::Mutex::new(None));
            // Held for this attempt only: a retry starts a fresh CLI on a fresh context, so the
            // previous attempt's mirror has nothing left to say. Dropping the guard at the end of
            // the iteration stops it on every path out of the body, abort included.
            let _live_context_fill =
                mirror_context_fill(&pool, id, std::sync::Arc::clone(&context_fill));
            let mut request = crate::runner::RunRequest {
                prompt: prompt.clone(),
                env: env.clone(),
                cwd: spawn_cwd.clone(),
                plan_only,
                resume_session_id: resume_session_id.clone(),
                mcp_config: None,
                tool_policy,
                progress_timeout: Some(progress_timeout),
                session_id: Some(session_id.clone()),
                fork_session,
                include_partial_messages: false,
                steerable,
                messages: None,
            };
            // Driven by the request's own flag, and beside the spawn that decides it: which run may
            // be spoken to is settled where its argument vector is chosen, not by whatever later
            // change opens the door. The flag arrives from `create_run_inner`, which has already
            // refused the modes that must never be steerable — this is where that decision is read,
            // not where it is made.
            //
            // A fresh channel per attempt, because a turn addressed to an attempt that has already
            // failed is not owed to its retry — and `insert` replaces the previous attempt's sender,
            // so nothing can go on holding a stale one.
            if request.steerable {
                let (messages_tx, messages_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                run_messages.lock().unwrap().insert(id, messages_tx);
                request.messages = Some(messages_rx);
            }
            let result = tokio::time::timeout(
                run_timeout,
                runner.run_prompt_with_context_fill(
                    request,
                    session_tx,
                    std::sync::Arc::clone(&transcript),
                    std::sync::Arc::clone(&context_fill),
                ),
            )
            .await;
            let completed_at = chrono::Utc::now().to_rfc3339();
            // Every terminal write below is guarded on the run still being `running`. This body is
            // not the only writer racing for the last word: `finalize_termination` aborts the task,
            // but the abort only lands where the future is next dropped, so a cancel or an
            // approval-pause can already have written its own status while this attempt was on its
            // way here. First writer wins; no rows means someone else finalised the run, which is an
            // outcome rather than a failure — the loop breaks either way.
            match result {
                Ok(Ok(o)) => {
                    // A CLI that exited non-zero did not do the work, and recording it `completed`
                    // announced a success its own exit code denies — including in the feed row the
                    // user reads. The runner now also reports -1 for a stream that broke after
                    // launch, so that lands here as a failure rather than being mistaken for a
                    // launch failure and retried over work that was already applied.
                    let terminal_status =
                        if o.exit_code == crate::runner::PROGRESS_TIMEOUT_EXIT_CODE {
                            "timed_out"
                        } else if o.exit_code == 0 {
                            "completed"
                        } else {
                            "failed"
                        };
                    let context_fill = observed_context_fill(&context_fill, &o.stdout);
                    append_run_events(&pool, id, &o.stdout).await;
                    // `run_prompt` does not return until the CLI process is dead and reaped. The
                    // gate belongs after that boundary: an orphaned build can otherwise retain file
                    // locks in the worktree for the lifetime of every later cleanup retry.
                    let gate_outcome = match (terminal_status, &gate_config, spawn_cwd.as_deref()) {
                        (
                            "completed",
                            GateConfig::Command {
                                command,
                                project_root,
                            },
                            Some(worktree),
                        ) => Some(
                            crate::gate::run_gate(
                                worktree,
                                std::path::Path::new(project_root),
                                command,
                                crate::state::DEFAULT_GATE_TIMEOUT,
                            )
                            .await,
                        ),
                        ("completed", GateConfig::Unreadable(reason), _) => {
                            Some(crate::gate::GateOutcome::Errored {
                                reason: reason.clone(),
                            })
                        }
                        _ => None,
                    };
                    let (gate_status, gate_exit_code, gate_output) = match &gate_outcome {
                        Some(crate::gate::GateOutcome::Passed) => (Some("passed"), None, None),
                        Some(crate::gate::GateOutcome::Failed { exit_code, output }) => {
                            (Some("failed"), Some(*exit_code), Some(output.as_str()))
                        }
                        Some(crate::gate::GateOutcome::Errored { reason }) => {
                            (Some("errored"), None, Some(reason.as_str()))
                        }
                        None => (None, None, None),
                    };
                    let completed = sqlx::query(
                        "UPDATE runs SET status = ?, exit_code = ?, stdout = ?, stderr = ?, session_id = COALESCE(?, session_id), cost_usd = ?, input_tokens = ?, output_tokens = ?, cache_read_tokens = ?, num_turns = ?, context_fill = ?, completed_at = ?, attempt = ?, gate_status = ?, gate_exit_code = ?, gate_output = ? WHERE id = ? AND status = 'running'",
                    )
                    .bind(terminal_status)
                    .bind(o.exit_code)
                    .bind(&o.stdout)
                    .bind(&o.stderr)
                    .bind(&o.session_id)
                    .bind(o.cost_usd)
                    .bind(o.input_tokens)
                    .bind(o.output_tokens)
                    .bind(o.cache_read_tokens)
                    .bind(o.num_turns)
                    .bind(context_fill)
                    .bind(&completed_at)
                    .bind(attempt as i64)
                    .bind(gate_status)
                    .bind(gate_exit_code)
                    .bind(gate_output)
                    .bind(id)
                    .execute(&pool)
                    .await;
                    warn_on_terminal_write_err(&completed, id, terminal_status);
                    let terminal_write_won =
                        matches!(&completed, Ok(result) if result.rows_affected() == 1);
                    // The feed row announces this run *finished* — only true if this write won the
                    // CAS race. `Ok` with 0 rows means a concurrent terminator (cancel/timeout) got
                    // there first, so this attempt never actually completed as far as the runs table
                    // is concerned; appending anyway would announce a completion it denies.
                    if terminal_status == "completed" && terminal_write_won {
                        match gate_outcome {
                            Some(crate::gate::GateOutcome::Failed { exit_code, .. }) => {
                                let _ = crate::feed::append(
                                    &pool,
                                    feed_project_id.as_deref(),
                                    "worktree_gate_failed",
                                    &format!("worktree gate failed with exit code {exit_code}"),
                                    Some(id),
                                )
                                .await;
                            }
                            Some(crate::gate::GateOutcome::Errored { reason }) => {
                                let _ = crate::feed::append(
                                    &pool,
                                    feed_project_id.as_deref(),
                                    "worktree_gate_failed",
                                    &format!("worktree gate errored: {reason}"),
                                    Some(id),
                                )
                                .await;
                            }
                            Some(crate::gate::GateOutcome::Passed) | None => {
                                if let Some((kind, summary)) = completion_feed.as_ref() {
                                    let _ = crate::feed::append(
                                        &pool,
                                        feed_project_id.as_deref(),
                                        kind,
                                        summary,
                                        Some(id),
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                    if terminal_write_won {
                        let original_session_id =
                            o.session_id.clone().unwrap_or_else(|| session_id.clone());
                        Box::pin(spawn_handoff_if_needed(
                            handoff_state.clone(),
                            runner.clone(),
                            id,
                            original_session_id,
                            project_id.clone(),
                            spawn_cwd.clone(),
                            plan_only,
                            completion_feed.clone(),
                            gate_config.clone(),
                            max_attempts,
                            tool_policy,
                            run_timeout,
                        ))
                        .await;
                    }
                    break;
                }
                Ok(Err(e)) => {
                    // A launch failure means the CLI never ran — no work happened — so retrying cannot
                    // double-apply a mutation. Retry up to max_attempts; otherwise fail for good.
                    if attempt < max_attempts {
                        let _ = crate::feed::append(
                            &pool,
                            feed_project_id.as_deref(),
                            "run_retry",
                            &format!("run {id} attempt {attempt} failed to launch, retrying: {e}"),
                            Some(id),
                        )
                        .await;
                        attempt += 1;
                        let _ = sqlx::query("UPDATE runs SET attempt = ? WHERE id = ?")
                            .bind(attempt as i64)
                            .bind(id)
                            .execute(&pool)
                            .await;
                        continue;
                    }
                    let failed = sqlx::query(
                        "UPDATE runs SET status = 'failed', stderr = ?, completed_at = ?, attempt = ? WHERE id = ? AND status = 'running'",
                    )
                    .bind(e.to_string())
                    .bind(&completed_at)
                    .bind(attempt as i64)
                    .bind(id)
                    .execute(&pool)
                    .await;
                    warn_on_terminal_write_err(&failed, id, "failed");
                    // Same principle as the completion feed row above: this announces the run's
                    // terminal outcome, so it must only fire when this write actually won the race.
                    if max_attempts > 1
                        && matches!(&failed, Ok(result) if result.rows_affected() == 1)
                    {
                        let _ = crate::feed::append(
                            &pool,
                            feed_project_id.as_deref(),
                            "run_failed_final",
                            &format!("run {id} failed after {attempt} attempts"),
                            Some(id),
                        )
                        .await;
                    }
                    break;
                }
                Err(_elapsed) => {
                    // The wall clock dropped the run future, so there is no `RunOutcome` to read —
                    // no stdout, no usage, no session id. What the run did emit before the clock ran
                    // out is in the shared transcript, and it is the whole record of a run that hit
                    // this branch. Persisting it is not cosmetic: this is the run most worth reading
                    // afterwards, and until now it was the one that left nothing at all behind.
                    let seen = transcript
                        .lock()
                        .map(|shared| shared.clone())
                        .unwrap_or_default();
                    let context_fill = observed_context_fill(&context_fill, &seen);
                    append_run_events(&pool, id, &seen).await;
                    // A timeout is not a launch failure — retrying would likely time out again.
                    let timed_out = sqlx::query(
                        "UPDATE runs SET status = 'timed_out', stdout = ?, context_fill = ?, completed_at = ?, attempt = ? WHERE id = ? AND status = 'running'",
                    )
                    .bind(&seen)
                    .bind(context_fill)
                    .bind(&completed_at)
                    .bind(attempt as i64)
                    .bind(id)
                    .execute(&pool)
                    .await;
                    warn_on_terminal_write_err(&timed_out, id, "timed_out");
                    if matches!(&timed_out, Ok(result) if result.rows_affected() == 1) {
                        Box::pin(spawn_handoff_if_needed(
                            handoff_state.clone(),
                            runner.clone(),
                            id,
                            session_id.clone(),
                            project_id.clone(),
                            spawn_cwd.clone(),
                            plan_only,
                            completion_feed.clone(),
                            gate_config.clone(),
                            max_attempts,
                            tool_policy,
                            run_timeout,
                        ))
                        .await;
                    }
                    break;
                }
            }
        }
    });
}

/// Retires a run that failed somewhere in worktree provisioning, and says so in the feed.
///
/// Provisioning happens after the run row exists, so a failure that just returns leaves it
/// `running` with no task — a state only a daemon restart reconciles, while migration 0009 blocks
/// the project's next worktree run for as long as it lasts. Compare-and-set on `running` so a
/// concurrent cancel keeps the last word.
async fn fail_provisioning(state: &AppState, id: i64, project_id: Option<&str>, summary: &str) {
    let failed = sqlx::query(
        "UPDATE runs SET status = 'failed', completed_at = ? WHERE id = ? AND status = 'running'",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(id)
    .execute(&state.pool)
    .await;
    warn_on_terminal_write_err(&failed, id, "failed");
    let _ = crate::feed::append(
        &state.pool,
        project_id,
        "worktree_provision_failed",
        summary,
        Some(id),
    )
    .await;
}

pub async fn create_run_inner(
    state: &AppState,
    prompt: String,
    project_id: Option<String>,
    cwd: Option<String>,
    mode: &str,
    steerable: bool,
) -> Result<i64, CreateRunError> {
    if mode == "worktree" && (project_id.is_none() || cwd.is_none()) {
        return Err(CreateRunError::Invalid(
            "worktree mode requires project_id and cwd (the project root)",
        ));
    }
    // Spec §5.5 asked here, where the run is MADE, and not only where it is later spoken to.
    // `http::post_run_message` refuses these same runs and that refusal stays — but it is a second
    // barrier, not the first one. A run that must never gain a second author should never be created
    // able to hear one: the flag on the row is what decides the argument vector, so a `steerable`
    // triage run would be launched with a stdin open to whatever else can reach this process,
    // leaving the endpoint's refusal as the only thing between a stranger's words and a live
    // session.
    //
    // Two questions, asked separately for the reason the endpoint asks them separately: where the
    // run came from, and what it may touch. They coincide only while there is one toolless mode.
    if steerable
        && (mode == crate::email::TRIAGE_MODE
            || tool_policy_for_mode(mode) == crate::runner::ToolPolicy::None)
    {
        return Err(CreateRunError::Invalid(
            "a run that launches without tools cannot be created steerable",
        ));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let session_id = crate::auth::generate_uuid_v4();
    let inserted = sqlx::query(
        "INSERT INTO runs (project_id, cwd, prompt, status, mode, session_id, steerable, created_at)
         VALUES (?, ?, ?, 'running', ?, ?, ?, ?)",
    )
    .bind(&project_id)
    .bind(&cwd)
    .bind(&prompt)
    .bind(mode)
    .bind(&session_id)
    .bind(i64::from(steerable))
    .bind(&now)
    .execute(&state.pool)
    .await;
    let id = match inserted {
        Ok(result) => result.last_insert_rowid(),
        Err(error)
            if error
                .as_database_error()
                .is_some_and(|database_error| database_error.is_unique_violation()) =>
        {
            return Err(CreateRunError::Busy);
        }
        Err(error) => return Err(CreateRunError::Db(error)),
    };

    let plan_only = mode == "shadow";
    // Derived here for the same reason `plan_only` is: the mode is what the caller asked for, and
    // `spawn_run` must not learn to read modes.
    let tool_policy = tool_policy_for_mode(mode);
    let mut spawn_cwd = cwd.clone().map(std::path::PathBuf::from);
    let mut completion_feed = plan_only.then(|| {
        (
            "shadow_run_completed".to_owned(),
            "shadow run completed".to_owned(),
        )
    });
    let mut gate_config = GateConfig::NotConfigured;

    if mode == "worktree" {
        let project_root = cwd.as_deref().expect("worktree cwd validated above");
        let worktree_project_id = project_id
            .as_deref()
            .expect("worktree project_id validated above");
        gate_config = match crate::config::load_schedule_rules(std::path::Path::new(project_root)) {
            Ok(rules) => rules
                .gate_command
                .map_or(GateConfig::NotConfigured, |command| GateConfig::Command {
                    command,
                    project_root: project_root.to_string(),
                }),
            Err(error) => {
                tracing::warn!(
                    project_id = worktree_project_id,
                    project_root,
                    %error,
                    "failed to load worktree gate configuration"
                );
                GateConfig::Unreadable(format!("gate configuration is unreadable: {error}"))
            }
        };
        let info = match crate::worktree::create(std::path::Path::new(project_root), id).await {
            Ok(info) => info,
            Err(error) => {
                fail_provisioning(
                    state,
                    id,
                    project_id.as_deref(),
                    &format!("worktree provisioning failed: {error}"),
                )
                .await;
                return Err(CreateRunError::Worktree(error));
            }
        };
        // Every exit from here on has to leave a terminal status behind. Past the INSERT the row is
        // `running` with no task and no abort handle: `/cancel` answers 404, the GC skips it, and
        // `one_open_worktree_run_per_project` (migration 0009) blocks every later worktree run for
        // the project — until a restart, the only thing that reconciles `running`. The `create`
        // branch above compensated; these two propagated with `?` and stranded the run.
        let worktree_path = info.path.to_string_lossy().into_owned();
        if let Err(error) = crate::worktree::record(
            &state.pool,
            id,
            worktree_project_id,
            project_root,
            &worktree_path,
            &info.branch,
        )
        .await
        {
            fail_provisioning(
                state,
                id,
                project_id.as_deref(),
                &format!("worktree was created but could not be recorded: {error}"),
            )
            .await;
            return Err(CreateRunError::Db(error));
        }
        if let Err(error) = sqlx::query("UPDATE runs SET cwd = ? WHERE id = ?")
            .bind(&worktree_path)
            .bind(id)
            .execute(&state.pool)
            .await
        {
            fail_provisioning(
                state,
                id,
                project_id.as_deref(),
                &format!("worktree was recorded but the run's cwd could not be set: {error}"),
            )
            .await;
            return Err(CreateRunError::Db(error));
        }
        completion_feed = Some((
            "worktree_run_completed".to_owned(),
            format!("worktree run completed on {}", info.branch),
        ));
        spawn_cwd = Some(info.path);
    }

    let max_attempts = if mode == "shadow" || mode == "worktree" {
        MAX_AUTONOMOUS_ATTEMPTS
    } else {
        1
    };
    let daemon_token = mint_run_token(&state.pool, id).await;
    // Falling back toward the CLI preserves the operator's pre-feature behavior when no local
    // model is configured; that direction is the ship-dark guarantee.
    let runner = if mode == crate::email::TRIAGE_MODE {
        state
            .triage_runner
            .clone()
            .unwrap_or_else(|| state.runner.clone())
    } else {
        state.runner.clone()
    };
    spawn_run(
        state,
        runner,
        id,
        prompt,
        project_id,
        spawn_cwd,
        plan_only,
        None,
        session_id,
        false,
        completion_feed,
        gate_config,
        max_attempts,
        tool_policy,
        daemon_token,
        steerable,
        run_timeout_for_mode(state.run_timeout, mode),
    );

    Ok(id)
}

pub async fn resume_approved_run(state: &AppState, proposal_id: i64) -> Result<i64, ResumeError> {
    let proposal = crate::proposals::get(&state.pool, proposal_id)
        .await?
        .ok_or(ResumeError::ProposalNotFound)?;
    if proposal.kind != "action-approval" || proposal.status != "pending" {
        return Err(ResumeError::ProposalNotPending);
    }

    let original_run_id = proposal
        .run_id
        .ok_or(ResumeError::NotResumable("proposal has no run_id"))?;
    let session_id = proposal
        .session_id
        .clone()
        .ok_or(ResumeError::NotResumable("run has no session_id to resume"))?;
    let tool_name = proposal
        .tool_name
        .clone()
        .ok_or(ResumeError::NotResumable("proposal has no tool_name"))?;

    let (wt_project_id, project_root, wt_path) = sqlx::query_as::<_, (String, String, String)>(
        "SELECT project_id, project_root, path
         FROM worktrees WHERE run_id = ? AND removed_at IS NULL",
    )
    .bind(original_run_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ResumeError::NotResumable(
        "no live worktree for the paused run",
    ))?;

    let prompt = format!(
        "A previously paused autonomous run has been resumed after approval of proposal #{proposal_id}. Proceed with the {tool_name} action you attempted before the pause — that one high-risk action is now authorized for this run — then finish the task."
    );
    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = state.pool.begin().await?;

    // Guarded on the state this resume was authorised from: the paused run was `awaiting_approval`
    // when the proposal was read, and a release or a cancel can have finalised it since. No rows
    // means one of those got there first, so the supersede is a no-op rather than a status this
    // resume is entitled to overwrite — the live-worktree lookup above is what actually stops a
    // resume onto a discarded worktree, and `one_open_worktree_run_per_project` (migration 0009)
    // rejects the INSERT below if the slot is still held.
    sqlx::query("UPDATE runs SET status='superseded', completed_at=? WHERE id=? AND status='awaiting_approval'")
        .bind(&now)
        .bind(original_run_id)
        .execute(&mut *tx)
        .await?;
    let result = sqlx::query(
        "INSERT INTO runs (project_id, cwd, prompt, status, mode, session_id, created_at)
         VALUES (?, ?, ?, 'running', 'worktree', ?, ?)",
    )
    .bind(&wt_project_id)
    .bind(&wt_path)
    .bind(&prompt)
    .bind(&session_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    let resume_id = result.last_insert_rowid();
    sqlx::query("UPDATE worktrees SET run_id=? WHERE run_id=?")
        .bind(resume_id)
        .bind(original_run_id)
        .execute(&mut *tx)
        .await?;
    // `tool_input` rides along so the grant names the action the human actually read and approved,
    // not merely the tool that would perform it (migration 0020).
    sqlx::query(
        "INSERT INTO action_grants (run_id, tool_name, tool_input, proposal_id, created_at, consumed_at)
         VALUES (?, ?, ?, ?, ?, NULL)",
    )
    .bind(resume_id)
    .bind(&tool_name)
    .bind(proposal.tool_input.as_deref())
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    // Compare-and-set on the state this resume was authorised from, like every other writer of a
    // proposal decision (`proposals::transition`, `worktree::release`). The `status != "pending"`
    // check at the top of this function reads OUTSIDE the transaction, so a reject arriving in
    // between would otherwise be overwritten here: the user's rejection would be stamped
    // `approved`, its audit trail would read pending→approved, and the resume run would go on to
    // perform the action they had just refused.
    //
    // Deliberately untested. Landing a rejection inside the window means writing to `proposals`
    // after this transaction opens but before it takes SQLite's write lock, and once that lock is
    // held a second writer blocks rather than races — a hand-driven poll test for it only ever
    // reproduced the busy timeout. The guard costs one clause; a flaky test would cost more.
    let approved = sqlx::query(
        "UPDATE proposals SET status='approved', decided_at=? WHERE id=? AND status='pending'",
    )
    .bind(&now)
    .bind(proposal_id)
    .execute(&mut *tx)
    .await?;
    if approved.rows_affected() != 1 {
        // Rolls back by dropping `tx`: the supersede, the resume row, the worktree hand-over and
        // the grant all disappear with it, so a lost race leaves nothing half-applied.
        return Err(ResumeError::ProposalNotPending);
    }
    let note = format!("approved; resume run {resume_id}");
    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, 'pending', 'approved', ?, ?)",
    )
    .bind(proposal_id)
    .bind(&note)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // After the commit, because the resume row does not exist to be UPDATEd before it.
    let daemon_token = mint_run_token(&state.pool, resume_id).await;
    let gate_config = match crate::config::load_schedule_rules(std::path::Path::new(&project_root))
    {
        Ok(rules) => rules
            .gate_command
            .map_or(GateConfig::NotConfigured, |command| GateConfig::Command {
                command,
                project_root: project_root.clone(),
            }),
        Err(error) => {
            tracing::warn!(
                project_id = %wt_project_id,
                project_root = %project_root,
                %error,
                "failed to load worktree gate configuration for resumed run"
            );
            GateConfig::Unreadable(format!("gate configuration is unreadable: {error}"))
        }
    };
    spawn_run(
        state,
        state.runner.clone(),
        resume_id,
        prompt,
        Some(wt_project_id),
        Some(std::path::PathBuf::from(&wt_path)),
        false,
        Some(session_id.clone()),
        session_id,
        false,
        Some((
            "worktree_run_completed".to_owned(),
            format!("resumed run completed on nucleos/run-{original_run_id}"),
        )),
        gate_config,
        1,
        // A resume continues an approved worktree run, which is autopilot work: the hook and the
        // classifier govern it, exactly as they governed the run being resumed.
        crate::runner::ToolPolicy::Unrestricted,
        daemon_token,
        // Not steerable, for the reason the handoff successor is not: the resume row carries the
        // column's default, and a process listening on a stdin its own row denies could never be
        // told the conversation is over.
        false,
        // A resume is a worktree run, so it gets the worktree clock — the same one the run it
        // continues was given.
        run_timeout_for_mode(state.run_timeout, "worktree"),
    );

    Ok(resume_id)
}

pub async fn get_run(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<RunStatusResponse>, StatusCode> {
    let run = sqlx::query_as::<_, RunStatusResponse>(
        "SELECT id, project_id, status, gate_status, gate_exit_code, gate_output, exit_code, stdout,
                stderr, session_id, cost_usd, input_tokens, output_tokens, cache_read_tokens,
                num_turns, context_fill, steerable, successor_run_id
         FROM runs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(run))
}

/// Terminates an in-flight run: aborts its task (which, via `kill_on_drop`, kills the CLI process)
/// and records `status`. Removing the entry from the handle map is the atomic arbiter when several
/// termination reasons race (user cancel, timeout, or Chunk 3's §8.4 approval-pause): whoever removes
/// it first sets the final status; a later reason finds it gone and no-ops. Returns true iff this call
/// terminated the run. Factored out so Chunk 3's `pending_approval` path reuses the same arbiter.
///
/// The handle arbitrates between terminators, but it does not arbitrate against the run's own body:
/// `abort()` takes effect where the future is next dropped, so a body already past its last `.await`
/// keeps running, and a body that has just written its own terminal status is still registered until
/// its `Registration` drops. The status write below is therefore compare-and-set, and `true` keeps
/// meaning "this call terminated the task" — not "the run ended up in `status`". A caller that needs
/// the latter reads the row back; today none does.
pub async fn finalize_termination(state: &AppState, id: i64, status: &str) -> bool {
    let handle = state.run_handles.lock().unwrap().remove(&id);
    match handle {
        Some(h) => {
            h.abort();
            let now = chrono::Utc::now().to_rfc3339();
            // First writer wins: no rows means the run finalised itself while this call was on its
            // way, which is an outcome, not a failure — nothing to retry and nothing to report.
            let result = sqlx::query(
                "UPDATE runs SET status = ?, completed_at = ? WHERE id = ? AND status = 'running'",
            )
            .bind(status)
            .bind(&now)
            .bind(id)
            .execute(&state.pool)
            .await;
            warn_on_terminal_write_err(&result, id, status);
            true
        }
        None => false,
    }
}

pub async fn cancel_run(State(state): State<AppState>, Path(id): Path<i64>) -> StatusCode {
    // Uncancellable: `finalize_termination` removes the handle and kills the process before it
    // writes the status, so a request dropped on that write leaves a `running` row nothing can
    // reach — the handle is gone, so a second `/cancel` answers 404 and the GC never collects it.
    match crate::http::uncancellable(
        async move { finalize_termination(&state, id, "cancelled").await },
    )
    .await
    {
        Ok(true) => StatusCode::OK,
        Ok(false) => StatusCode::NOT_FOUND,
        Err(status) => status,
    }
}

/// Marks every run still `"running"` as `"interrupted"` — called once at startup to recover from a
/// daemon crash that left in-flight runs' rows stuck (spec §3.2). Returns how many rows it changed.
pub async fn reconcile_orphaned_runs(pool: &sqlx::SqlitePool) -> Result<u64, sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    let reconciled: Vec<(i64, Option<String>)> = sqlx::query_as(
        "UPDATE runs SET status = 'interrupted', completed_at = ? WHERE status = 'running'
         RETURNING id, project_id",
    )
    .bind(&now)
    .fetch_all(pool)
    .await?;
    for (id, project_id) in &reconciled {
        let _ = crate::feed::append(
            pool,
            project_id.as_deref(),
            "run_interrupted",
            "run interrupted during startup recovery",
            Some(*id),
        )
        .await;
    }
    Ok(reconciled.len() as u64)
}

/// Marks every `"awaiting_approval"` run whose action-approval proposal is gone or already decided
/// as `"interrupted"` — the pause's counterpart to `reconcile_orphaned_runs`, run once at startup.
/// Returns how many rows it changed.
///
/// Such a run is unreachable, not merely idle: both `/approve` and `/reject` start from the pending
/// proposal row, so with none there is no input left that can move it. It is also load-bearing:
/// `one_open_worktree_run_per_project` (migration 0009) counts `awaiting_approval`, so one strand
/// blocks every later worktree run of its project, and the worktree GC only collects terminal runs.
/// A run holding a *pending* proposal is resumable by design — the NOT EXISTS leaves it alone.
///
/// The doors that created these strands are closed, so this only heals rows predating that fix; the
/// pass stays because a strand is permanent and invisible otherwise. Worktrees are left to the GC,
/// which collects `interrupted` on its own — no removal here.
pub async fn reconcile_stranded_approvals(pool: &sqlx::SqlitePool) -> Result<u64, sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    // Single statement: the status guard and the proposal check must observe one snapshot, or an
    // approval landing mid-pass would be overwritten by a decision taken before it existed.
    let reconciled: Vec<(i64, Option<String>)> = sqlx::query_as(
        "UPDATE runs SET status = 'interrupted', completed_at = ?
         WHERE status = 'awaiting_approval'
           AND NOT EXISTS (
               SELECT 1 FROM proposals
               WHERE proposals.run_id = runs.id
                 AND proposals.kind = 'action-approval'
                 AND proposals.status = 'pending')
         RETURNING id, project_id",
    )
    .bind(&now)
    .fetch_all(pool)
    .await?;
    for (id, project_id) in &reconciled {
        let _ = crate::feed::append(
            pool,
            project_id.as_deref(),
            "run_interrupted",
            "run interrupted during startup recovery: its approval request no longer exists",
            Some(*id),
        )
        .await;
    }
    Ok(reconciled.len() as u64)
}

#[rustfmt::skip]
#[cfg(test)]
mod tests {
    // These `current_thread` async tests hold `worktree::test_env_lock()` — a
    // process-wide MutexGuard — across their awaits to serialise mutation of the
    // shared `WORKTREE_ROOT` env override. Holding it across `.await` is the whole
    // point (and there is no multi-thread runtime to starve), so
    // `await_holding_lock` is a false positive here.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use crate::auth::Token;
    use crate::proposals;
    use crate::runner::{FakeCommandRunner, RunOutcome};
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use std::ffi::{OsStr, OsString};
    use std::path::{Path as FsPath, PathBuf};
    use std::process::Command;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    async fn test_state_with_runner(
        delay: Option<Duration>,
        run_timeout: Duration,
    ) -> (AppState, Arc<FakeCommandRunner>) {
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

        let runner = Arc::new(FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(RunOutcome {
                exit_code: 0,
                stdout: "42".into(),
                stderr: String::new(),
                session_id: Some("fake-session-id".into()),
                cost_usd: Some(0.05),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
            })),
            delay: std::sync::Mutex::new(delay),
            last_plan_only: std::sync::Mutex::new(None),
            last_cwd: std::sync::Mutex::new(None),
            last_resume: std::sync::Mutex::new(None),
            ..Default::default()
        });
        let state = AppState {
            token: Token("test-token".into()),
            pool,
            runner: runner.clone(),
            triage_runner: None,
            local_triage_disabled: None,
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout,
        };
        (state, runner)
    }

    fn git_ok(dir: &FsPath, args: &[&OsStr]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start")
            .success()
    }

    fn space_free_tempdir(prefix: &str) -> tempfile::TempDir {
        let base = std::env::current_dir().expect("resolve current directory");
        assert!(
            !base.to_string_lossy().contains(' '),
            "test checkout must have a space-free path"
        );
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in(base)
            .expect("create space-free tempdir")
    }

    struct WorktreeRootEnv {
        previous: Option<OsString>,
    }

    impl WorktreeRootEnv {
        fn set(path: &FsPath) -> Self {
            let previous = std::env::var_os("NUCLEOS_WORKTREE_ROOT");
            unsafe {
                std::env::set_var("NUCLEOS_WORKTREE_ROOT", path);
            }
            Self { previous }
        }
    }

    impl Drop for WorktreeRootEnv {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("NUCLEOS_WORKTREE_ROOT", value),
                    None => std::env::remove_var("NUCLEOS_WORKTREE_ROOT"),
                }
            }
        }
    }

    fn initialize_repo(repo: &FsPath) {
        std::fs::create_dir_all(repo).expect("create repository directory");
        assert!(git_ok(repo, &[OsStr::new("init")]));
        assert!(git_ok(
            repo,
            &[
                OsStr::new("config"),
                OsStr::new("user.email"),
                OsStr::new("test@x"),
            ],
        ));
        assert!(git_ok(
            repo,
            &[
                OsStr::new("config"),
                OsStr::new("user.name"),
                OsStr::new("test"),
            ],
        ));
        std::fs::write(repo.join("seed.txt"), "seed\n").expect("write seed file");
        assert!(git_ok(repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            repo,
            &[OsStr::new("commit"), OsStr::new("-m"), OsStr::new("seed"),],
        ));
    }

    fn init_contained_repo(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let container = space_free_tempdir(prefix);
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        (container, repo)
    }

    fn configure_gate(repo: &FsPath, command: &str) {
        std::fs::create_dir_all(repo.join(".ai")).expect("create project config directory");
        std::fs::write(
            repo.join(".ai").join("autopilot.yaml"),
            format!("gate_command: '{command}'\n"),
        )
        .expect("write project gate command");
        assert!(git_ok(repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            repo,
            &[
                OsStr::new("commit"),
                OsStr::new("-m"),
                OsStr::new("configure gate"),
            ],
        ));
    }

    fn configure_unreadable_gate(repo: &FsPath) {
        std::fs::create_dir_all(repo.join(".ai")).expect("create project config directory");
        std::fs::write(
            repo.join(".ai").join("autopilot.yaml"),
            "gate_command: [unterminated\n",
        )
        .expect("write malformed project gate configuration");
        assert!(git_ok(repo, &[OsStr::new("add"), OsStr::new("-A")]));
        assert!(git_ok(
            repo,
            &[
                OsStr::new("commit"),
                OsStr::new("-m"),
                OsStr::new("configure unreadable gate"),
            ],
        ));
    }

    async fn test_state_with(delay: Option<Duration>, run_timeout: Duration) -> AppState {
        test_state_with_runner(delay, run_timeout).await.0
    }

    async fn test_state() -> AppState {
        test_state_with(None, crate::state::DEFAULT_RUN_TIMEOUT).await
    }

    async fn advance_run_ids_past(pool: &sqlx::SqlitePool, id: i64) {
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, created_at)
             VALUES (?, 'sequence placeholder', 'completed', 'real', ?)",
        )
        .bind(id)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn create_worktree_run(
        state: &AppState,
        prompt: &str,
        project_id: &str,
        project_root: &str,
    ) -> Result<i64, CreateRunError> {
        for attempt in 0..100 {
            let result = create_run_inner(
                state,
                prompt.to_owned(),
                Some(project_id.to_owned()),
                Some(project_root.to_owned()),
                "worktree",
            false,)
            .await;
            match result {
                Err(CreateRunError::Worktree(_)) if attempt < 99 => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                result => return result,
            }
        }
        unreachable!("the final retry always returns")
    }

    fn test_router(state: AppState) -> Router {
        Router::new()
            .route("/runs", post(create_run))
            .route("/runs/{id}", get(get_run))
            .route("/runs/{id}/cancel", post(cancel_run))
            .with_state(state)
    }

    async fn create_run_via_http(app: &Router, prompt: &str) -> CreateRunResponse {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runs")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"prompt":"{prompt}"}}"#)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn get_run_status(app: &Router, id: i64) -> RunStatusResponse {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/runs/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn seed_resumable_action_approval(
        state: &AppState,
        session_id: Option<&str>,
    ) -> (i64, i64, PathBuf) {
        let worktree_path = PathBuf::from("C:/worktrees/proj/run-paused");
        let project_root = "C:/repos/proj";
        let created_at = chrono::Utc::now().to_rfc3339();
        let result = sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, session_id, mode, created_at)
             VALUES ('proj', ?, 'x', 'awaiting_approval', ?, 'worktree', ?)",
        )
        .bind(worktree_path.to_string_lossy().as_ref())
        .bind(session_id)
        .bind(&created_at)
        .execute(&state.pool)
        .await
        .unwrap();
        let original_run_id = result.last_insert_rowid();

        sqlx::query(
            "INSERT INTO worktrees
             (run_id, project_id, project_root, path, branch, created_at)
             VALUES (?, 'proj', ?, ?, ?, ?)",
        )
        .bind(original_run_id)
        .bind(project_root)
        .bind(worktree_path.to_string_lossy().as_ref())
        .bind(format!("nucleos/run-{original_run_id}"))
        .bind(&created_at)
        .execute(&state.pool)
        .await
        .unwrap();

        let proposal_id = proposals::create_action_approval(
            &state.pool,
            original_run_id,
            session_id,
            Some("proj"),
            "Bash",
            "push needs approval",
            Some("{}"),
        )
        .await
        .unwrap();

        (original_run_id, proposal_id, worktree_path)
    }

    #[tokio::test]
    async fn approve_resumes_session_in_same_worktree_and_grants_the_action() {
        let (state, runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (original_run_id, proposal_id, worktree_path) =
            seed_resumable_action_approval(&state, Some("sess-a")).await;

        let resume_run_id = resume_approved_run(&state, proposal_id).await.unwrap();

        for _ in 0..100 {
            if runner.last_resume.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let original_status =
            sqlx::query_scalar::<_, String>("SELECT status FROM runs WHERE id = ?")
                .bind(original_run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(original_status, "superseded");

        let resume_run = sqlx::query_as::<_, (Option<String>, Option<String>, String, String)>(
            "SELECT project_id, cwd, status, mode FROM runs WHERE id = ?",
        )
        .bind(resume_run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(resume_run.0.as_deref(), Some("proj"));
        assert_eq!(resume_run.1.as_deref(), worktree_path.to_str());
        assert_eq!(resume_run.2, "running");
        assert_eq!(resume_run.3, "worktree");

        let transferred_run_id =
            sqlx::query_scalar::<_, i64>("SELECT run_id FROM worktrees WHERE path = ?")
                .bind(worktree_path.to_string_lossy().as_ref())
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(transferred_run_id, resume_run_id);

        let grant = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT tool_name, consumed_at FROM action_grants WHERE run_id = ?",
        )
        .bind(resume_run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(grant, ("Bash".to_owned(), None));

        let proposal = proposals::get(&state.pool, proposal_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(proposal.status, "approved");
        assert_eq!(
            *runner.last_resume.lock().unwrap(),
            Some("sess-a".to_owned())
        );
        assert_eq!(
            *runner.last_cwd.lock().unwrap(),
            Some(worktree_path.clone())
        );
    }

    #[tokio::test]
    async fn approve_non_pending_proposal_is_rejected() {
        let state = test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (_, proposal_id, _) = seed_resumable_action_approval(&state, Some("sess-a")).await;
        assert!(
            proposals::transition(&state.pool, proposal_id, "rejected", "x")
                .await
                .unwrap()
        );
        let count_before = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();

        let result = resume_approved_run(&state, proposal_id).await;

        assert!(matches!(result, Err(ResumeError::ProposalNotPending)));
        let count_after = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(count_after, count_before);
    }

    #[tokio::test]
    async fn approve_unknown_proposal_is_not_found() {
        let state = test_state().await;

        let result = resume_approved_run(&state, 999_999).await;

        assert!(matches!(result, Err(ResumeError::ProposalNotFound)));
    }

    #[tokio::test]
    async fn approve_requires_a_session_id() {
        let state = test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (_, proposal_id, _) = seed_resumable_action_approval(&state, None).await;

        let result = resume_approved_run(&state, proposal_id).await;

        assert!(matches!(result, Err(ResumeError::NotResumable(_))));
    }

    #[tokio::test]
    async fn list_awaiting_approval_is_empty_when_there_are_no_runs() {
        let pool = test_state().await.pool;

        let runs: Vec<AwaitingRun> = list_awaiting_approval(&pool).await.unwrap();

        assert!(runs.is_empty());
    }

    #[tokio::test]
    async fn list_awaiting_approval_returns_only_awaiting_runs_ordered_by_id() {
        let pool = test_state().await.pool;
        for (project_id, prompt, status) in [
            ("project-running", "running run", "running"),
            ("project-first", "first awaiting run", "awaiting_approval"),
            ("project-completed", "completed run", "completed"),
            ("project-second", "second awaiting run", "awaiting_approval"),
            ("project-cancelled", "cancelled run", "cancelled"),
        ] {
            sqlx::query(
                "INSERT INTO runs (project_id, cwd, prompt, status, mode, created_at)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(project_id)
            .bind(format!("C:/projects/{project_id}"))
            .bind(prompt)
            .bind(status)
            .bind("worktree")
            .bind("2026-07-20T10:00:00Z")
            .execute(&pool)
            .await
            .unwrap();
        }

        let runs: Vec<AwaitingRun> = list_awaiting_approval(&pool).await.unwrap();

        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, 2);
        assert_eq!(runs[0].prompt, "first awaiting run");
        assert_eq!(runs[1].id, 4);
        assert_eq!(runs[1].prompt, "second awaiting run");
    }

    #[tokio::test]
    async fn list_awaiting_approval_returns_all_release_queue_fields() {
        let pool = test_state().await.pool;
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

        let runs: Vec<AwaitingRun> = list_awaiting_approval(&pool).await.unwrap();

        assert_eq!(
            runs,
            vec![AwaitingRun {
                id: 1,
                project_id: Some("project-alpha".into()),
                prompt: "release the pinned worktree".into(),
                cwd: Some("C:/worktrees/project-alpha/run-1".into()),
                created_at: "2026-07-20T10:11:12Z".into(),
            }]
        );
    }

    #[tokio::test]
    async fn create_then_get_run_reaches_completed_status() {
        let app = test_router(test_state().await);
        let created = create_run_via_http(&app, "what is 6*7").await;

        let mut status = String::new();
        for _ in 0..20 {
            let parsed = get_run_status(&app, created.id).await;
            status = parsed.status.clone();
            if status == "completed" {
                assert_eq!(parsed.stdout.as_deref(), Some("42"));
                assert_eq!(parsed.session_id.as_deref(), Some("fake-session-id"));
                assert_eq!(parsed.cost_usd, Some(0.05));
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("run did not reach completed status in time, last status: {status}");
    }

    #[tokio::test]
    async fn a_completed_run_persists_its_token_usage() {
        let (state, runner) = test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        *runner.canned.lock().unwrap() = Some(RunOutcome {
            exit_code: 0,
            stdout: "measured".into(),
            stderr: String::new(),
            session_id: Some("usage-session".into()),
            cost_usd: Some(0.08),
            input_tokens: Some(1000),
            output_tokens: Some(500),
            cache_read_tokens: Some(20_000),
            num_turns: Some(12),
        });
        let pool = state.pool.clone();
        let app = test_router(state);
        let created = create_run_via_http(&app, "persist usage").await;

        for _ in 0..20 {
            let parsed = get_run_status(&app, created.id).await;
            if parsed.status == "completed" {
                let usage: (Option<i64>, Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
                    "SELECT input_tokens, output_tokens, cache_read_tokens, num_turns
                         FROM runs WHERE id = ?",
                )
                .bind(created.id)
                .fetch_one(&pool)
                .await
                .unwrap();
                assert_eq!(usage, (Some(1000), Some(500), Some(20_000), Some(12)));
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("run did not reach completed status in time");
    }

    #[tokio::test]
    async fn a_run_persists_its_trajectory_events() {
        let (state, runner) = test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        *runner.canned.lock().unwrap() = Some(RunOutcome {
            exit_code: 0,
            stdout: [
                r#"{"type":"system","subtype":"init","session_id":"trajectory-session"}"#,
                r#"{"type":"assistant","message":{"content":[]}}"#,
                r#"{"type":"result","result":"done"}"#,
            ]
            .join("\n"),
            stderr: String::new(),
            session_id: Some("trajectory-session".into()),
            cost_usd: Some(0.01),
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            num_turns: None,
        });
        let pool = state.pool.clone();
        let app = test_router(state);
        let created = create_run_via_http(&app, "persist trajectory").await;

        for _ in 0..20 {
            let parsed = get_run_status(&app, created.id).await;
            if parsed.status == "completed" {
                let events: Vec<(i64,)> =
                    sqlx::query_as("SELECT seq FROM run_events WHERE run_id = ? ORDER BY seq")
                        .bind(created.id)
                        .fetch_all(&pool)
                        .await
                        .unwrap();
                assert_eq!(events.len(), 3);
                assert_eq!(
                    events.into_iter().map(|(seq,)| seq).collect::<Vec<_>>(),
                    vec![0, 1, 2]
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("run did not reach completed status in time");
    }

    #[tokio::test]
    async fn a_run_crossing_the_threshold_records_a_handoff() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        let threshold = HANDOFF_CONTEXT_LIMIT_FLOOR * 4 / 5;
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, context_fill, created_at)
             VALUES (42001, 'crossing', 'completed', 'real', ?, '2026-07-30T12:00:00Z'),
                    (42002, 'crossing successor', 'running', 'real', NULL, '2026-07-30T12:01:00Z'),
                    (42003, 'below', 'completed', 'real', ?, '2026-07-30T12:02:00Z'),
                    (42004, 'below successor', 'running', 'real', NULL, '2026-07-30T12:03:00Z')",
        )
        .bind(threshold)
        .bind(threshold - 1)
        .execute(&pool)
        .await
        .unwrap();

        assert!(record_handoff_if_needed(&pool, 42001, 42002).await.unwrap());
        assert!(!record_handoff_if_needed(&pool, 42001, 42002).await.unwrap());
        assert!(!record_handoff_if_needed(&pool, 42003, 42004).await.unwrap());

        let links: (Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT
                 (SELECT successor_run_id FROM runs WHERE id = 42001),
                 (SELECT successor_run_id FROM runs WHERE id = 42003)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(links, (Some(42002), None));

        let event_counts: (i64, i64) = sqlx::query_as(
            "SELECT
                 (SELECT COUNT(*) FROM run_events
                  WHERE run_id = 42001 AND kind = 'context_handoff'),
                 (SELECT COUNT(*) FROM run_events
                  WHERE run_id = 42003 AND kind = 'context_handoff')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(event_counts, (1, 0));
    }

    /// Barrier 1 of spec §5.5, at the seam where it is decided. A triage run reads mail written by
    /// strangers, so the CLI must launch unable to touch anything — and every other mode must keep
    /// the tools its work depends on, or this hardening silently breaks the autopilot.
    #[tokio::test]
    async fn only_a_triage_run_launches_without_tools() {
        let (state, runner) = test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;

        for (mode, expected) in [
            (crate::email::TRIAGE_MODE, crate::runner::ToolPolicy::None),
            ("real", crate::runner::ToolPolicy::Unrestricted),
            ("shadow", crate::runner::ToolPolicy::Unrestricted),
        ] {
            *runner.last_tool_policy.lock().unwrap() = None;
            create_run_inner(&state, "prompt".into(), None, None, mode, false)
                .await
                .unwrap();
            for _ in 0..50 {
                if runner.last_tool_policy.lock().unwrap().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(
                *runner.last_tool_policy.lock().unwrap(),
                Some(expected),
                "mode {mode}"
            );
        }
    }

    /// The production opt-in, and the thing that was missing: `spawn_run` hardcoded `steerable:
    /// false`, so no caller outside a test fixture could make a run that `POST /runs/{id}/message`
    /// would accept — the whole authorization matrix guarded a door nothing could reach.
    ///
    /// Asserted on both readers, because they are two separate facts that must agree. The launch is
    /// what opens a stdin at all, and the row is what the endpoint consults minutes later on a
    /// different task; a run listening while its row says it is not would be refused every turn and
    /// never told the conversation was over.
    #[tokio::test]
    async fn a_run_can_be_created_steerable() {
        let (state, runner) = test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;

        let id = create_run_inner(&state, "keep going".into(), None, None, "real", true)
            .await
            .expect("a real-mode run may ask to be steerable");

        let recorded: i64 = sqlx::query_scalar("SELECT steerable FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(recorded, 1, "the row the endpoint reads must say so too");

        for _ in 0..50 {
            if runner.last_steerable.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            *runner.last_steerable.lock().unwrap(),
            Some(true),
            "the opt-in must reach the launch, which is what decides the argument vector"
        );
    }

    /// The ship-dark half of the opt-in, and the reason the field defaults rather than being
    /// required. Every caller that predates it — the shell, the sidecars, every preset already
    /// stored — sends a body with no `steerable` key, and each of them has to keep the run it has
    /// always had: launched with a closed stdin, and a row that tells the endpoint to refuse.
    #[tokio::test]
    async fn a_run_that_does_not_ask_is_not_steerable() {
        let (state, runner) = test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        let app = test_router(state.clone());

        let created = create_run_via_http(&app, "do the thing").await;

        let recorded: i64 = sqlx::query_scalar("SELECT steerable FROM runs WHERE id = ?")
            .bind(created.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(recorded, 0, "a body that says nothing asks for nothing");

        for _ in 0..50 {
            if runner.last_steerable.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            *runner.last_steerable.lock().unwrap(),
            Some(false),
            "the launch must keep the closed stdin every existing caller was built against"
        );
    }

    /// The creation-side half of spec §5.5. `http::post_run_message` already refuses a triage run,
    /// and that refusal stays — but it is the SECOND barrier. A run whose whole premise is that a
    /// stranger's words never meet a tool must not be launched holding an open stdin in the first
    /// place, or the endpoint's check is the only thing standing between the two.
    #[tokio::test]
    async fn a_triage_run_cannot_be_created_steerable() {
        let (state, runner) = test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;

        let refused = create_run_inner(
            &state,
            "a stranger wrote this".into(),
            None,
            None,
            crate::email::TRIAGE_MODE,
            true,
        )
        .await;

        assert!(
            matches!(refused, Err(CreateRunError::Invalid(_))),
            "the email pillar's runs must be refused a stdin, not merely refused turns on one"
        );
        assert_eq!(
            crate::http::create_run_status(&refused.unwrap_err()),
            StatusCode::BAD_REQUEST,
            "the caller has to learn its request was rejected, not that the daemon failed"
        );
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 0, "no row may be written for a refused run");
        assert_eq!(
            *runner.last_steerable.lock().unwrap(),
            None,
            "nothing may be launched for a refused run"
        );
    }

    /// Keyed on the tool policy rather than on where the run came from, and separate from the triage
    /// test above for exactly that reason: the two rules coincide on today's single toolless mode,
    /// and each has to hold on its own the day a second one appears. Asked of
    /// `tool_policy_for_mode`, the same function the endpoint asks, so a mode added there is refused
    /// by both or by neither.
    #[tokio::test]
    async fn a_toolless_run_cannot_be_created_steerable() {
        let state = test_state().await;

        for mode in ["real", "shadow", crate::email::TRIAGE_MODE] {
            let toolless =
                tool_policy_for_mode(mode) == crate::runner::ToolPolicy::None;
            let result =
                create_run_inner(&state, "prompt".into(), None, None, mode, true).await;

            assert_eq!(
                matches!(result, Err(CreateRunError::Invalid(_))),
                toolless,
                "{mode}: a run launched with no tools is the one run that must not be steerable"
            );
        }
    }

    /// Selecting the local runner by mode keeps message bodies on-machine without accidentally
    /// diverting ordinary autonomous work away from the established CLI runner.
    #[tokio::test]
    async fn a_triage_run_uses_the_local_runner_when_one_is_configured() {
        let (mut state, default_runner) =
            test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        let local_runner = Arc::new(FakeCommandRunner::default());
        state.triage_runner = Some(local_runner.clone());

        let triage_id = create_run_inner(
            &state,
            "private message body".into(),
            None,
            None,
            crate::email::TRIAGE_MODE,
        false,)
        .await
        .unwrap();
        let (triage_status, _) = poll_run(&state, triage_id, "completed").await;
        assert_eq!(triage_status, "completed");
        assert_eq!(*local_runner.calls.lock().unwrap(), 1);
        assert_eq!(*default_runner.calls.lock().unwrap(), 0);

        let ordinary_id = create_run_inner(&state, "ordinary work".into(), None, None, "real", false)
            .await
            .unwrap();
        let (ordinary_status, _) = poll_run(&state, ordinary_id, "completed").await;
        assert_eq!(ordinary_status, "completed");
        assert_eq!(*local_runner.calls.lock().unwrap(), 1);
        assert_eq!(*default_runner.calls.lock().unwrap(), 1);
    }

    /// Shipping the wiring dark depends on this fallback: without an opted-in local model, triage
    /// must keep using the existing CLI runner instead of becoming silently inoperable.
    #[tokio::test]
    async fn a_triage_run_falls_back_to_the_cli_when_no_local_model_is_configured() {
        let (state, default_runner) =
            test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;

        let id = create_run_inner(
            &state,
            "message body".into(),
            None,
            None,
            crate::email::TRIAGE_MODE,
        false,)
        .await
        .unwrap();
        let (status, _) = poll_run(&state, id, "completed").await;

        assert_eq!(status, "completed");
        assert_eq!(*default_runner.calls.lock().unwrap(), 1);
    }

    /// An autonomous run launches with `ToolPolicy::Unrestricted`, so it has a Bash tool, and the
    /// classifier calls `echo $NUCLEOS_DAEMON_TOKEN` a `read-local` action — allowed even in shadow
    /// mode. Whatever is in that environment must be assumed published, so it cannot be the key that
    /// approves proposals and disengages the kill switch.
    #[tokio::test]
    async fn an_autonomous_run_is_never_handed_the_control_token() {
        let (state, runner) = test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        let control = state.token.0.clone();

        for mode in [crate::email::TRIAGE_MODE, "real", "shadow"] {
            *runner.last_env.lock().unwrap() = None;
            let id = create_run_inner(&state, "prompt".into(), None, None, mode, false)
                .await
                .unwrap();
            for _ in 0..50 {
                if runner.last_env.lock().unwrap().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            let env: std::collections::HashMap<String, String> = runner
                .last_env
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| panic!("mode {mode} never launched"))
                .into_iter()
                .collect();
            let handed = &env["NUCLEOS_DAEMON_TOKEN"];

            assert_ne!(handed, &control, "mode {mode} was handed the control token");
            // Its own key, and recognisably so: the run id travels in it, which is what lets the
            // daemon check the run_id in a hook payload against something the caller cannot pick.
            assert_eq!(
                handed.split_once('.').map(|(prefix, _)| prefix),
                Some(id.to_string().as_str()),
                "mode {mode}"
            );

            // Stored before the CLI could possibly have called back — a secret that lands a moment
            // later would 401 the run's first tool call for reasons no log explains.
            let stored: Option<String> = sqlx::query_scalar("SELECT token FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(
                stored.as_deref(),
                handed.split_once('.').map(|(_, secret)| secret),
                "mode {mode}"
            );
        }
    }

    #[tokio::test]
    async fn create_run_inner_persists_mode_and_threads_plan_only_per_run() {
        let (state, runner) = test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;

        let shadow_id = create_run_inner(&state, "shadow".into(), None, None, "shadow", false)
            .await
            .unwrap();
        for _ in 0..20 {
            if runner.last_plan_only.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let shadow_mode: String = sqlx::query_scalar("SELECT mode FROM runs WHERE id = ?")
            .bind(shadow_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(shadow_mode, "shadow");
        assert_eq!(*runner.last_plan_only.lock().unwrap(), Some(true));
        let mut shadow_feed = None;
        for _ in 0..20 {
            let entries = crate::feed::list_feed(&state.pool, None, 50).await.unwrap();
            if !entries.is_empty() {
                shadow_feed = Some(entries);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let shadow_feed = shadow_feed.expect("shadow completion did not append a feed entry");
        assert_eq!(shadow_feed.len(), 1);
        assert_eq!(shadow_feed[0].kind, "shadow_run_completed");
        assert_eq!(shadow_feed[0].run_id, Some(shadow_id));

        let real_id = create_run_inner(&state, "real".into(), None, None, "real", false)
            .await
            .unwrap();
        for _ in 0..20 {
            if *runner.last_plan_only.lock().unwrap() == Some(false) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let real_mode: String = sqlx::query_scalar("SELECT mode FROM runs WHERE id = ?")
            .bind(real_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(real_mode, "real");
        assert_eq!(*runner.last_plan_only.lock().unwrap(), Some(false));
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(
            crate::feed::list_feed(&state.pool, None, 50)
                .await
                .unwrap()
                .len(),
            1,
            "real completion must not append a feed entry"
        );

        let app = test_router(state.clone());
        let default_id = create_run_via_http(&app, "default mode").await.id;
        let default_mode: String = sqlx::query_scalar("SELECT mode FROM runs WHERE id = ?")
            .bind(default_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(default_mode, "real");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worktree_mode_provisions_and_runs_inside_the_worktree() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-provision-");
        let (state, runner) = test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        advance_run_ids_past(&state.pool, 40_000).await;
        let project_root = repo.to_string_lossy().into_owned();

        let id = create_worktree_run(&state, "do it", "proj", &project_root)
            .await
            .unwrap();

        let mut spawn_cwd = None;
        for _ in 0..50 {
            spawn_cwd = runner.last_cwd.lock().unwrap().clone();
            if spawn_cwd.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let spawn_cwd = spawn_cwd.expect("runner did not receive a cwd");
        assert!(spawn_cwd.starts_with(wt_root.path()));
        assert_eq!(
            spawn_cwd.file_name(),
            Some(OsStr::new(&format!("run-{id}")))
        );
        assert_eq!(*runner.last_plan_only.lock().unwrap(), Some(false));

        let run_cwd: String = sqlx::query_scalar("SELECT cwd FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(PathBuf::from(&run_cwd), spawn_cwd);

        let (worktree_path, branch): (String, String) =
            sqlx::query_as("SELECT path, branch FROM worktrees WHERE run_id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(PathBuf::from(&worktree_path), spawn_cwd);
        assert_eq!(branch, format!("nucleos/run-{id}"));

        let mut completion_feed = None;
        for _ in 0..50 {
            completion_feed = sqlx::query_as::<_, (String, String)>(
                "SELECT kind, summary FROM feed WHERE run_id = ?",
            )
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .unwrap();
            if completion_feed.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let (kind, summary) = completion_feed.expect("completion feed entry was not appended");
        assert_eq!(kind, "worktree_run_completed");
        assert!(summary.contains(&branch));

        let _ = crate::worktree::remove(&repo, &spawn_cwd, &[]).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_worktree_run_records_its_gate_verdict() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-gate-passes-");
        configure_gate(&repo, r#"sh -c "exit 0""#);
        let (state, _runner) =
            test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        let project_root = repo.to_string_lossy().into_owned();

        let id = create_worktree_run(&state, "do it", "proj", &project_root)
            .await
            .unwrap();
        let app = test_router(state.clone());
        let mut status = String::new();
        for _ in 0..100 {
            status = get_run_status(&app, id).await.status;
            if status == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(status, "completed");

        let gate_status: Option<String> =
            sqlx::query_scalar("SELECT gate_status FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(gate_status.as_deref(), Some("passed"));

        let worktree_path: String =
            sqlx::query_scalar("SELECT path FROM worktrees WHERE run_id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let _ = crate::worktree::remove(&repo, FsPath::new(&worktree_path), &[]).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_failing_gate_is_announced_in_the_feed() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-gate-fails-");
        configure_gate(&repo, r#"sh -c "exit 7""#);
        let (state, _runner) =
            test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        let project_root = repo.to_string_lossy().into_owned();

        let id = create_worktree_run(&state, "do it", "proj", &project_root)
            .await
            .unwrap();
        let mut feed_kind = None;
        for _ in 0..100 {
            feed_kind = sqlx::query_scalar::<_, String>(
                "SELECT kind FROM feed WHERE run_id = ? ORDER BY id DESC LIMIT 1",
            )
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .unwrap();
            if feed_kind.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(feed_kind.as_deref(), Some("worktree_gate_failed"));

        // The feed row was all this asserted, which left the `failed` verdict itself unpinned:
        // `gate_status` is checked as 'passed', 'errored' and NULL elsewhere but never as 'failed',
        // and `gate_exit_code` is only ever asserted to be NULL. The column that carries the exit
        // code was never once checked holding one, so nothing distinguished exit 7 from exit 1 — or
        // from the gate not having run at all.
        let (gate_status, gate_exit_code, gate_output): (Option<String>, Option<i64>, Option<String>) =
            sqlx::query_as("SELECT gate_status, gate_exit_code, gate_output FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(gate_status.as_deref(), Some("failed"));
        assert_eq!(gate_exit_code, Some(7), "the gate's own exit code must reach the row");
        assert!(gate_output.is_some(), "a failing gate must keep its output tail");

        let worktree_path: String =
            sqlx::query_scalar("SELECT path FROM worktrees WHERE run_id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let _ = crate::worktree::remove(&repo, FsPath::new(&worktree_path), &[]).await;
    }

    /// Replaces `budget::a_gate_execution_is_not_an_autonomous_row`, which could not fail: it opened
    /// a pool that `run_gate` never receives — the function takes no pool and `gate.rs` has no SQL —
    /// then compared an empty table to itself. It was cited during this series as evidence that the
    /// property held.
    ///
    /// The property worth pinning is the one the spec actually claims: the gate costs the autonomy
    /// budget nothing. That holds because `completed_at` is captured BEFORE the gate runs, so the
    /// billed window closes when the agent stopped, not when the measurement finished. Moving that
    /// capture after the gate would silently start charging a project for verifying its own work.
    #[tokio::test]
    async fn a_gate_is_not_billed_to_the_run_it_measures() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-gate-billing-");
        // A full second of gate, so the margin below cannot be explained by scheduling noise.
        configure_gate(&repo, r#"sh -c "sleep 1; exit 0""#);
        let (state, _runner) =
            test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        let project_root = repo.to_string_lossy().into_owned();

        let started = tokio::time::Instant::now();
        let id = create_worktree_run(&state, "do it", "proj", &project_root)
            .await
            .unwrap();
        let mut row: Option<(String, Option<String>, Option<String>)> = None;
        for _ in 0..300 {
            row = sqlx::query_as(
                "SELECT created_at, completed_at, gate_status FROM runs WHERE id = ?",
            )
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .unwrap();
            if matches!(&row, Some((_, Some(_), Some(_)))) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let observed = started.elapsed();
        let (created_at, completed_at, gate_status) = row.expect("the run must reach a terminal row");
        assert_eq!(gate_status.as_deref(), Some("passed"));

        let created = chrono::DateTime::parse_from_rfc3339(&created_at).unwrap();
        let completed =
            chrono::DateTime::parse_from_rfc3339(&completed_at.expect("completed_at")).unwrap();
        let billed = (completed - created).to_std().unwrap_or_default();

        // The gate slept a second AFTER completed_at was captured, so real elapsed time must exceed
        // the billed window by roughly that second. Half of it is margin.
        assert!(
            observed > billed + Duration::from_millis(500),
            "the gate must fall outside the billed window: billed {billed:?}, observed {observed:?}"
        );

        let worktree_path: String =
            sqlx::query_scalar("SELECT path FROM worktrees WHERE run_id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let _ = crate::worktree::remove(&repo, FsPath::new(&worktree_path), &[]).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_unreadable_gate_config_fails_the_run_closed() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-unreadable-gate-");
        configure_unreadable_gate(&repo);
        let (state, _runner) =
            test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        let project_root = repo.to_string_lossy().into_owned();

        let id = create_worktree_run(&state, "do it", "proj", &project_root)
            .await
            .unwrap();
        let app = test_router(state.clone());
        let mut status = String::new();
        for _ in 0..100 {
            status = get_run_status(&app, id).await.status;
            if status == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(status, "completed");

        let (gate_status, gate_output): (Option<String>, Option<String>) =
            sqlx::query_as("SELECT gate_status, gate_output FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(gate_status.as_deref(), Some("errored"));
        let reason = gate_output.expect("an errored gate must record its reason");
        let reason = reason.to_ascii_lowercase();
        assert!(
            reason.contains("configuration") && reason.contains("unreadable"),
            "unexpected gate error reason: {reason}"
        );

        let worktree_path: String =
            sqlx::query_scalar("SELECT path FROM worktrees WHERE run_id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let _ = crate::worktree::remove(&repo, FsPath::new(&worktree_path), &[]).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_unreadable_gate_config_creates_no_proposal() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-unreadable-gate-");
        configure_unreadable_gate(&repo);
        let (state, _runner) =
            test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        let project_root = repo.to_string_lossy().into_owned();

        let id = create_worktree_run(&state, "do it", "proj", &project_root)
            .await
            .unwrap();
        let app = test_router(state.clone());
        let mut status = String::new();
        for _ in 0..100 {
            status = get_run_status(&app, id).await.status;
            if status == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(status, "completed");

        let proposals: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM proposals WHERE run_id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(proposals, 0);
        let feed_kind: String =
            sqlx::query_scalar("SELECT kind FROM feed WHERE run_id = ? ORDER BY id DESC LIMIT 1")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(feed_kind, "worktree_gate_failed");

        let worktree_path: String =
            sqlx::query_scalar("SELECT path FROM worktrees WHERE run_id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let _ = crate::worktree::remove(&repo, FsPath::new(&worktree_path), &[]).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_absent_gate_config_is_still_not_a_gate() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-without-gate-");
        assert!(!repo.join(".ai").join("autopilot.yaml").exists());
        let (state, _runner) =
            test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        let project_root = repo.to_string_lossy().into_owned();

        let id = create_worktree_run(&state, "do it", "proj", &project_root)
            .await
            .unwrap();
        let app = test_router(state.clone());
        let mut status = String::new();
        for _ in 0..100 {
            status = get_run_status(&app, id).await.status;
            if status == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(status, "completed");

        let gate_status: Option<String> =
            sqlx::query_scalar("SELECT gate_status FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(gate_status, None);

        let feed_kind: String =
            sqlx::query_scalar("SELECT kind FROM feed WHERE run_id = ? ORDER BY id DESC LIMIT 1")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(feed_kind, "worktree_run_completed");

        let worktree_path: String =
            sqlx::query_scalar("SELECT path FROM worktrees WHERE run_id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let _ = crate::worktree::remove(&repo, FsPath::new(&worktree_path), &[]).await;
    }

    /// The kill switch has to stop the endpoint, not just the schedulers. Every spawned CLI holds
    /// the daemon token, so autonomy that is "stopped" can otherwise start its own successors.
    #[tokio::test]
    async fn the_kill_switch_refuses_a_run_created_through_the_api() {
        let state = test_state().await;
        crate::autopilot::set_kill_switch(&state.pool, true)
            .await
            .unwrap();

        let result = create_run(
            State(state.clone()),
            Json(CreateRunRequest {
                prompt: "start something".to_owned(),
                project_id: None,
                cwd: None,
                mode: "real".to_owned(),
                steerable: false,
            }),
        )
        .await;

        assert!(matches!(result, Err(StatusCode::CONFLICT)));
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 0, "no row may be written for a refused run");
    }

    #[tokio::test]
    async fn a_run_is_created_normally_while_the_kill_switch_is_off() {
        let state = test_state().await;

        let result = create_run(
            State(state.clone()),
            Json(CreateRunRequest {
                prompt: "start something".to_owned(),
                project_id: None,
                cwd: None,
                mode: "real".to_owned(),
                steerable: false,
            }),
        )
        .await;

        assert!(result.is_ok(), "the switch is off; this must go through");
    }

    /// A worktree run's row is INSERTed `running` before its worktree is provisioned, and
    /// `git worktree add` takes real time — so the window between the two is wide enough to matter.
    /// A client that disconnects cancels the request it was making, which drops the handler's future
    /// exactly the way `abort()` drops a run's, and everything past that point is simply never done:
    /// no task, no abort handle (so `/cancel` answers 404), and a `running` worktree row that
    /// `one_open_worktree_run_per_project` (migration 0009) turns into a project-wide block on every
    /// later worktree run — until the daemon restarts, the only thing that reconciles `running` rows.
    #[tokio::test(flavor = "current_thread")]
    async fn a_dropped_create_request_still_finishes_the_run_it_started() {
        use std::future::Future;

        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-dropped-");
        let state = test_state().await;
        advance_run_ids_past(&state.pool, 47_000).await;
        let run_id = 47_001;

        let mut handler = Box::pin(create_run(
            State(state.clone()),
            Json(CreateRunRequest {
                prompt: "do it".to_owned(),
                project_id: Some("proj".to_owned()),
                cwd: Some(repo.to_string_lossy().into_owned()),
                mode: "worktree".to_owned(),
                steerable: false,
            }),
        ));

        // Drive the handler by hand so the request can be dropped at a chosen point: once the
        // worktree exists on disk, which is past the row INSERT and before the run is spawned.
        // Watching the directory rather than the database keeps the pool's single connection free
        // while the handler is parked on it.
        let worktree_path = wt_root.path().join(format!("run-{run_id}"));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut provisioned = false;
        for _ in 0..10_000 {
            assert!(
                handler.as_mut().poll(&mut context).is_pending(),
                "the handler ran to completion before the request could be dropped"
            );
            if worktree_path.exists() {
                provisioned = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(provisioned, "the worktree was never provisioned");
        drop(handler);

        let mut status = String::new();
        for _ in 0..200 {
            status = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            if status != "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            status, "completed",
            "a dropped request must not leave the project pinned by a half-created run"
        );

        let _ = crate::worktree::remove(&repo, &worktree_path, &[]).await;
    }

    /// A leaked abort handle is not just untidy bookkeeping: `hooks::pretooluse_decision` reads this
    /// map as its "is this run_id really in flight" check, so an entry that outlives its run lets a
    /// finished run be terminated and pended all over again — and `cancel_run` will happily overwrite
    /// a completed run's final status. Releasing the handle in a statement after `body.await` misses
    /// every way out of the task that is not a clean return.
    #[tokio::test]
    async fn a_panicking_run_task_still_releases_its_abort_handle() {
        let state = test_state().await;
        let id = 91_001;

        spawn_registered(&state, id, async { panic!("run body blew up") });

        for _ in 0..100 {
            if !state.run_handles.lock().unwrap().contains_key(&id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !state.run_handles.lock().unwrap().contains_key(&id),
            "a dead run must not stay registered as in-flight"
        );
    }

    /// The half the test above does not cover. Releasing the handle keeps the in-flight map honest,
    /// but the `runs` row was left `running` behind a dead task — `/cancel` answering 404, the GC
    /// skipping it, and `one_open_worktree_run_per_project` blocking the project until a restart.
    #[tokio::test]
    async fn a_panicking_run_task_records_the_run_as_failed() {
        let state = test_state().await;
        let id = sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, mode, created_at)
             VALUES ('proj', 'C:/work/repo', 'x', 'running', 'worktree', ?)",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        spawn_registered(&state, id, async { panic!("run body blew up") });

        let mut status = String::new();
        for _ in 0..200 {
            status = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            if status != "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(
            status, "failed",
            "a panicked run must not be left occupying the project's worktree slot"
        );
    }

    /// A cancelled task must NOT be rewritten by the panic supervisor: `finalize_termination` has
    /// already chosen and written the status, and stamping `failed` over it would report a failure
    /// for a run the user deliberately stopped.
    #[tokio::test]
    async fn an_aborted_run_keeps_the_status_its_terminator_wrote() {
        let state = test_state().await;
        let id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('x', 'running', 'real', ?)",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        spawn_registered(&state, id, async {
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        assert!(finalize_termination(&state, id, "cancelled").await);

        // Long enough for a supervisor that ignored cancellation to have overwritten this.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "cancelled");
    }

    /// `abort()` does not reach into a task that is already past its last `.await`: the body can
    /// have written its own `completed` and still be on its way out, with the `Registration` that
    /// releases its handle not yet dropped. `finalize_termination` therefore still finds a handle
    /// and, writing blind, would stamp `cancelled` over a run whose work genuinely finished —
    /// reporting a cancellation for output the user already has.
    #[tokio::test]
    async fn finalize_termination_leaves_an_already_finished_run_alone() {
        let state = test_state().await;
        let id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('x', 'running', 'real', ?)",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();
        // The body's own terminal write, which lands before its handle is released.
        sqlx::query("UPDATE runs SET status = 'completed', exit_code = 0 WHERE id = ?")
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
        // A handle still registered over that write is exactly the window: the task is finishing,
        // not gone. A parked body keeps it registered for as long as the test needs it.
        spawn_registered(&state, id, std::future::pending::<()>());

        assert!(
            finalize_termination(&state, id, "cancelled").await,
            "this call is still the one that terminated the task"
        );

        let (status, exit_code): (String, Option<i64>) =
            sqlx::query_as("SELECT status, exit_code FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(status, "completed");
        assert_eq!(exit_code, Some(0));
    }

    /// The same race from the other side. `abort()` only takes effect where the future is dropped,
    /// so a cancel that wins the status write can be followed by the run body waking up one last
    /// time and running its completion write — turning a run whose CLI was killed mid-flight into a
    /// `completed` one, exit code, output and all.
    #[tokio::test]
    async fn a_completion_write_never_overwrites_a_finalised_status() {
        let (state, runner) =
            test_state_with_runner(Some(Duration::from_secs(1)), Duration::from_secs(600)).await;
        let id = create_run_inner(&state, "a slow one".into(), None, None, "real", false)
            .await
            .unwrap();

        // The fake runner counts the call before it sleeps, so this parks the body inside the CLI
        // call — past the point of no return for the completion write, and short of running it.
        for _ in 0..500 {
            if *runner.calls.lock().unwrap() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            *runner.calls.lock().unwrap(),
            1,
            "the run body should be inside the CLI call"
        );

        // What `finalize_termination` writes when it gets there first. Written directly rather than
        // through it, because aborting the task would drop the very future whose last write is the
        // thing under test.
        sqlx::query("UPDATE runs SET status = 'cancelled', completed_at = ? WHERE id = ?")
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
        let exit_code: Option<i64> = sqlx::query_scalar("SELECT exit_code FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            exit_code, None,
            "the completion write must not have run yet"
        );

        // The handle is released by the guard the task captured, so an empty map is proof the body
        // reached the end — its terminal write included — rather than proof that time passed.
        for _ in 0..500 {
            if !state.run_handles.lock().unwrap().contains_key(&id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !state.run_handles.lock().unwrap().contains_key(&id),
            "the run body never finished"
        );

        let (status, exit_code): (String, Option<i64>) =
            sqlx::query_as("SELECT status, exit_code FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            status, "cancelled",
            "a killed run must not report itself completed"
        );
        assert_eq!(exit_code, None);
    }

    /// Same race, on the feed side: a completion write that loses the CAS race must not still
    /// announce a completion the runs table denies. Shadow mode is used because its completion
    /// feed row is unconditional (`plan_only.then(...)`), unlike worktree mode's provisioning.
    #[tokio::test]
    async fn a_lost_completion_race_never_appends_its_completion_feed_row() {
        let (state, runner) =
            test_state_with_runner(Some(Duration::from_secs(1)), Duration::from_secs(600)).await;
        let id = create_run_inner(&state, "a slow shadow one".into(), None, None, "shadow", false)
            .await
            .unwrap();

        // Park the body inside the CLI call, same as the sibling test above.
        for _ in 0..500 {
            if *runner.calls.lock().unwrap() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            *runner.calls.lock().unwrap(),
            1,
            "the run body should be inside the CLI call"
        );

        // A concurrent cancel wins the status write first.
        sqlx::query("UPDATE runs SET status = 'cancelled', completed_at = ? WHERE id = ?")
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();

        // Wait for the body to wake up, lose the CAS race, and finish.
        for _ in 0..500 {
            if !state.run_handles.lock().unwrap().contains_key(&id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !state.run_handles.lock().unwrap().contains_key(&id),
            "the run body never finished"
        );

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "cancelled");

        let feed_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE run_id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            feed_count, 0,
            "a lost completion race must not append the completion feed row"
        );
    }

    #[tokio::test]
    async fn worktree_mode_requires_project_id_and_cwd() {
        let state = test_state().await;
        let missing_project = create_run_inner(
            &state,
            "do it".into(),
            None,
            Some("root".into()),
            "worktree",
        false,)
        .await;
        assert!(matches!(missing_project, Err(CreateRunError::Invalid(_))));

        let missing_cwd = create_run_inner(
            &state,
            "do it".into(),
            Some("proj".into()),
            None,
            "worktree",
        false,)
        .await;
        assert!(matches!(missing_cwd, Err(CreateRunError::Invalid(_))));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn second_worktree_run_while_one_is_running_is_busy() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-exclusive-");
        let state = test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        advance_run_ids_past(&state.pool, 10_000).await;
        let project_root = repo.to_string_lossy().into_owned();

        create_worktree_run(&state, "first", "proj", &project_root)
            .await
            .unwrap();
        let second = create_run_inner(
            &state,
            "second".into(),
            Some("proj".into()),
            Some(project_root),
            "worktree",
        false,)
        .await;

        assert!(matches!(second, Err(CreateRunError::Busy)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worktree_run_for_a_different_project_is_allowed() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-project-");
        let (_repo2_container, repo2) = init_contained_repo("nucleos-runs-project2-");
        let state = test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        advance_run_ids_past(&state.pool, 20_000).await;

        create_worktree_run(&state, "first", "proj", &repo.to_string_lossy())
            .await
            .unwrap();
        let second = create_worktree_run(&state, "second", "proj2", &repo2.to_string_lossy()).await;

        assert!(second.is_ok(), "unexpected result: {second:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worktree_slot_is_held_while_awaiting_approval() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-pinned-");
        let state = test_state().await;
        let project_root = repo.to_string_lossy().into_owned();
        sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, mode, created_at)
             VALUES ('proj', ?, 'pinned', 'awaiting_approval', 'worktree', ?)",
        )
        .bind(&project_root)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap();

        let result = create_run_inner(
            &state,
            "new".into(),
            Some("proj".into()),
            Some(project_root),
            "worktree",
        false,)
        .await;

        assert!(matches!(result, Err(CreateRunError::Busy)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worktree_slot_frees_after_the_open_run_leaves() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-released-");
        let state = test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        advance_run_ids_past(&state.pool, 30_000).await;
        let project_root = repo.to_string_lossy().into_owned();
        let first = create_worktree_run(&state, "first", "proj", &project_root)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
            .bind(first)
            .execute(&state.pool)
            .await
            .unwrap();

        let second = create_worktree_run(&state, "second", "proj", &project_root).await;

        assert!(second.is_ok(), "unexpected result: {second:?}");
    }

    #[tokio::test]
    async fn shadow_and_real_runs_are_unaffected_by_the_index() {
        let state = test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;

        for mode in ["shadow", "real"] {
            for prompt in ["first", "second"] {
                let result =
                    create_run_inner(&state, prompt.into(), Some("proj".into()), None, mode, false).await;
                assert!(result.is_ok(), "{mode} run failed: {result:?}");
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worktree_mode_on_a_non_repo_marks_failed_and_feeds() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let non_repo_container = tempfile::tempdir().expect("create non-repository tempdir");
        let non_repo = non_repo_container.path().join("not-a-repo");
        std::fs::create_dir(&non_repo).expect("create non-repository directory");
        let state = test_state().await;

        let result = create_run_inner(
            &state,
            "do it".into(),
            Some("proj".into()),
            Some(non_repo.to_string_lossy().into_owned()),
            "worktree",
        false,)
        .await;
        assert!(
            matches!(result, Err(CreateRunError::Worktree(_))),
            "unexpected result: {result:?}"
        );

        let (id, status): (i64, String) =
            sqlx::query_as("SELECT id, status FROM runs ORDER BY id DESC LIMIT 1")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(status, "failed");
        let (kind, summary): (String, String) =
            sqlx::query_as("SELECT kind, summary FROM feed WHERE run_id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(kind, "worktree_provision_failed");
        assert!(summary.contains("worktree provisioning failed:"));
    }

    #[tokio::test]
    async fn get_unknown_run_returns_404() {
        let app = test_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/runs/999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn run_can_be_cancelled_mid_flight() {
        let app = test_router(
            test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await,
        );
        let created = create_run_via_http(&app, "a slow one").await;

        let cancel_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/runs/{}/cancel", created.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancel_response.status(), StatusCode::OK);

        let parsed = get_run_status(&app, created.id).await;
        assert_eq!(parsed.status, "cancelled");
    }

    #[tokio::test]
    async fn session_id_persists_even_when_cancelled_mid_flight() {
        let app = test_router(
            test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await,
        );
        let created = create_run_via_http(&app, "a slow one").await;

        let mut session_id = None;
        for _ in 0..50 {
            let parsed = get_run_status(&app, created.id).await;
            if parsed.session_id.is_some() {
                session_id = parsed.session_id;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(session_id.as_deref(), Some("fake-session-id"));

        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/runs/{}/cancel", created.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let parsed = get_run_status(&app, created.id).await;
        assert_eq!(parsed.status, "cancelled");
        assert_eq!(parsed.session_id.as_deref(), Some("fake-session-id"));
    }

    #[tokio::test]
    async fn cancelling_an_unknown_run_returns_404() {
        let app = test_router(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runs/999/cancel")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn run_times_out_when_it_outlives_the_configured_timeout() {
        let app = test_router(
            test_state_with(Some(Duration::from_millis(500)), Duration::from_millis(50)).await,
        );
        let created = create_run_via_http(&app, "a run that outlives its timeout").await;

        let mut status = String::new();
        for _ in 0..20 {
            let parsed = get_run_status(&app, created.id).await;
            status = parsed.status.clone();
            if status == "timed_out" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("run did not reach timed_out status in time, last status: {status}");
    }

    #[tokio::test]
    async fn a_silent_run_is_timed_out_before_the_wall_clock() {
        let run_timeout = Duration::from_secs(10);
        let (mut state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), run_timeout).await;
        state.progress_timeout = Duration::from_millis(50);
        let app = test_router(state);
        let started = tokio::time::Instant::now();
        let created = create_run_via_http(&app, "a run whose event stream goes silent").await;

        let mut status = String::new();
        for _ in 0..50 {
            let parsed = get_run_status(&app, created.id).await;
            status = parsed.status.clone();
            if status == "timed_out" {
                assert!(
                    started.elapsed() < run_timeout,
                    "the progress deadline must fire before the wall-clock timeout"
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "silent run did not reach timed_out before its wall clock, last status: {status}"
        );
    }

    /// PURE. `email_triage` is the case worth stating: it is autonomous, it spends money, and it
    /// still must not get the long clock — a triage run classifies one message, so one that is
    /// still going after ten minutes is stuck rather than busy.
    #[test]
    fn only_the_long_running_autonomous_modes_get_the_longer_wall_clock() {
        let base = Duration::from_secs(600);

        for mode in ["shadow", "worktree"] {
            assert_eq!(
                run_timeout_for_mode(base, mode),
                base * crate::state::AUTONOMOUS_RUN_TIMEOUT_MULTIPLIER,
                "{mode} builds and gates a tree; it is the mode that was measured hitting the wall"
            );
        }

        for mode in ["real", "plan", crate::email::TRIAGE_MODE] {
            assert_eq!(
                run_timeout_for_mode(base, mode),
                base,
                "{mode} keeps the interactive clock"
            );
        }
    }

    /// The wall clock is per mode, and this is the test that notices if it stops being — the pure
    /// test above would keep passing if nobody called the function.
    ///
    /// One state, one clock, two runs. The work takes longer than the interactive deadline and less
    /// than the autonomous one, so the same runner and the same 300ms setting have to produce two
    /// different outcomes. Drop the multiplier and the shadow run times out with the real one; apply
    /// it to everything and the real run completes. Both were run; both fail this.
    #[tokio::test]
    async fn an_autonomous_run_outlives_the_deadline_that_kills_an_interactive_one() {
        // 300ms clock against 520ms of work: 3x that is 900ms, so the margin either way is wider
        // than the work itself.
        let (mut state, _runner) =
            test_state_with_runner(Some(Duration::from_millis(520)), Duration::from_millis(300))
                .await;
        // Far out of the way: this test is about the wall clock, not about silence.
        state.progress_timeout = Duration::from_secs(30);

        let interactive = create_run_inner(&state, "interactive".into(), None, None, "real", false)
            .await
            .unwrap();
        let autonomous = create_run_inner(&state, "autonomous".into(), None, None, "shadow", false)
            .await
            .unwrap();

        let (interactive_status, _) = poll_run(&state, interactive, "timed_out").await;
        let (autonomous_status, _) = poll_run(&state, autonomous, "completed").await;

        assert_eq!(
            interactive_status, "timed_out",
            "520ms of work does not fit in a 300ms interactive clock"
        );
        assert_eq!(
            autonomous_status, "completed",
            "the same work fits in the autonomous clock, which is what the multiplier is for"
        );
    }

    /// The wall clock drops the run future, taking the `RunOutcome` and every byte of stdout it
    /// owned. A run killed that way used to persist nothing at all — no transcript, no trajectory —
    /// which made the run most worth reading afterwards the one that left no record. The shared
    /// transcript is the only thing that outlives the drop.
    #[tokio::test]
    async fn a_run_killed_by_the_wall_clock_keeps_what_it_had_already_emitted() {
        // Four events, one every 40ms, against a 150ms wall clock: the run cannot finish, and the
        // progress deadline is far enough out that it is the WALL clock being tested, not it.
        let (mut state, runner) =
            test_state_with_runner(Some(Duration::from_millis(40)), Duration::from_millis(150))
                .await;
        state.progress_timeout = Duration::from_secs(30);
        *runner.canned.lock().unwrap() = Some(RunOutcome {
            exit_code: 0,
            stdout: [
                r#"{"type":"system","subtype":"init"}"#,
                r#"{"type":"assistant"}"#,
                r#"{"type":"user"}"#,
                r#"{"type":"result"}"#,
            ]
            .join("\n"),
            stderr: String::new(),
            session_id: Some("fake-session-id".into()),
            cost_usd: Some(0.05),
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            num_turns: None,
        });
        let pool = state.pool.clone();
        let app = test_router(state);
        let created = create_run_via_http(&app, "a run the wall clock will cut short").await;

        let mut status = String::new();
        for _ in 0..60 {
            status = get_run_status(&app, created.id).await.status;
            if status == "timed_out" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(status, "timed_out", "the wall clock must terminate this run");

        let stdout: Option<String> = sqlx::query_scalar("SELECT stdout FROM runs WHERE id = ?")
            .bind(created.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let stdout = stdout.unwrap_or_default();
        assert!(
            stdout.contains(r#""subtype":"init""#),
            "the transcript emitted before the clock ran out must be persisted, got {stdout:?}"
        );

        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_events WHERE run_id = ?")
            .bind(created.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            events > 0,
            "a timed-out run must still leave a trajectory behind"
        );
        // Not all four: the point is that it was cut off mid-stream, so a test asserting the whole
        // transcript would be asserting that the timeout did not work.
        assert!(
            events < 4,
            "the run should have been cut short, but all {events} events arrived"
        );
    }

    #[tokio::test]
    async fn reconcile_marks_only_running_runs_as_interrupted() {
        let db = crate::storage::TempDb::new().await;
        let pool = db.pool.clone();

        // One run in flight when the daemon "died", one already completed.
        sqlx::query("INSERT INTO runs (prompt, status, created_at) VALUES ('x', 'running', '2026-07-17T00:00:00Z')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO runs (prompt, status, created_at) VALUES ('y', 'completed', '2026-07-17T00:00:00Z')")
            .execute(&pool).await.unwrap();

        let n = reconcile_orphaned_runs(&pool).await.unwrap();
        assert_eq!(n, 1, "exactly the one running row should be reconciled");

        let statuses: Vec<(String, String)> =
            sqlx::query_as("SELECT prompt, status FROM runs ORDER BY prompt")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            statuses,
            vec![
                ("x".to_string(), "interrupted".to_string()),
                ("y".to_string(), "completed".to_string()),
            ]
        );
        let entries = crate::feed::list_feed(&pool, None, 50).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, "run_interrupted");
        assert_eq!(entries[0].run_id, Some(1));
        db.close().await;
    }

    // One project per run: `one_open_worktree_run_per_project` (migration 0009) is exactly what a
    // stranded pause jams, so two of them cannot coexist under the same project_id.
    async fn insert_awaiting_run(pool: &sqlx::SqlitePool, project_id: &str, prompt: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES (?, ?, 'awaiting_approval', 'worktree', '2026-07-20T00:00:00Z')",
        )
        .bind(project_id)
        .bind(prompt)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn run_row(pool: &sqlx::SqlitePool, id: i64) -> (String, Option<String>) {
        sqlx::query_as("SELECT status, completed_at FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn reconcile_recovers_an_awaiting_approval_run_with_no_proposal() {
        let db = crate::storage::TempDb::new().await;
        let pool = db.pool.clone();
        let id = insert_awaiting_run(&pool, "p", "stranded").await;

        let n = reconcile_stranded_approvals(&pool).await.unwrap();
        assert_eq!(n, 1, "the run has nothing left that could decide it");

        let (status, completed_at) = run_row(&pool, id).await;
        assert_eq!(status, "interrupted");
        // The worktree GC keys off completed_at, so an unset one would strand the directory instead.
        assert!(completed_at.is_some());

        let entries = crate::feed::list_feed(&pool, Some("p"), 50).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, "run_interrupted");
        assert_eq!(entries[0].run_id, Some(id));
        db.close().await;
    }

    #[tokio::test]
    async fn reconcile_leaves_an_awaiting_approval_run_with_a_pending_proposal_alone() {
        let db = crate::storage::TempDb::new().await;
        let pool = db.pool.clone();
        let id = insert_awaiting_run(&pool, "p", "resumable").await;
        proposals::create_action_approval(&pool, id, Some("s"), Some("p"), "Bash", "why", None)
            .await
            .unwrap();

        let n = reconcile_stranded_approvals(&pool).await.unwrap();
        assert_eq!(
            n, 0,
            "a pending proposal makes the pause resumable by design"
        );

        let (status, completed_at) = run_row(&pool, id).await;
        assert_eq!(status, "awaiting_approval");
        assert_eq!(completed_at, None);
        assert!(crate::feed::list_all(&pool, 50).await.unwrap().is_empty());
        db.close().await;
    }

    #[tokio::test]
    async fn reconcile_recovers_an_awaiting_approval_run_whose_proposal_was_decided() {
        let db = crate::storage::TempDb::new().await;
        let pool = db.pool.clone();

        let rejected_run = insert_awaiting_run(&pool, "p-rejected", "rejected proposal").await;
        let rejected = proposals::create_action_approval(
            &pool,
            rejected_run,
            Some("s"),
            Some("p"),
            "Bash",
            "why",
            None,
        )
        .await
        .unwrap();
        assert!(
            proposals::transition(&pool, rejected, "rejected", "x")
                .await
                .unwrap()
        );

        let approved_run = insert_awaiting_run(&pool, "p-approved", "approved proposal").await;
        let approved = proposals::create_action_approval(
            &pool,
            approved_run,
            Some("s"),
            Some("p"),
            "Bash",
            "why",
            None,
        )
        .await
        .unwrap();
        assert!(
            proposals::transition(&pool, approved, "approved", "x")
                .await
                .unwrap()
        );

        let n = reconcile_stranded_approvals(&pool).await.unwrap();
        assert_eq!(n, 2, "a decided proposal can no longer release either run");

        for id in [rejected_run, approved_run] {
            let (status, completed_at) = run_row(&pool, id).await;
            assert_eq!(status, "interrupted");
            assert!(completed_at.is_some());
        }
        assert_eq!(crate::feed::list_all(&pool, 50).await.unwrap().len(), 2);
        db.close().await;
    }

    async fn poll_run(state: &AppState, id: i64, until: &str) -> (String, i64) {
        for _ in 0..100 {
            let row: (String, i64) =
                sqlx::query_as("SELECT status, attempt FROM runs WHERE id = ?")
                    .bind(id)
                    .fetch_one(&state.pool)
                    .await
                    .unwrap();
            if row.0 == until {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        sqlx::query_as("SELECT status, attempt FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn autonomous_run_retries_a_launch_failure_then_completes() {
        let (state, runner) = test_state_with_runner(None, Duration::from_secs(5)).await;
        *runner.fail_times.lock().unwrap() = 1; // 1st attempt fails to launch, 2nd succeeds

        let id = create_run_inner(&state, "go".into(), None, None, "shadow", false)
            .await
            .unwrap();

        let (status, attempt) = poll_run(&state, id, "completed").await;
        assert_eq!(status, "completed");
        assert_eq!(attempt, 2);
        assert_eq!(*runner.calls.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn autonomous_run_fails_after_exhausting_retries() {
        let (state, runner) = test_state_with_runner(None, Duration::from_secs(5)).await;
        *runner.fail_times.lock().unwrap() = 5; // always fails to launch

        let id = create_run_inner(&state, "go".into(), None, None, "shadow", false)
            .await
            .unwrap();

        let (status, attempt) = poll_run(&state, id, "failed").await;
        assert_eq!(status, "failed");
        assert_eq!(attempt, 2); // capped at the autonomous retry limit
        assert_eq!(*runner.calls.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn manual_run_does_not_retry_a_launch_failure() {
        let (state, runner) = test_state_with_runner(None, Duration::from_secs(5)).await;
        *runner.fail_times.lock().unwrap() = 5;

        let id = create_run_inner(&state, "go".into(), None, None, "real", false)
            .await
            .unwrap();

        let (status, attempt) = poll_run(&state, id, "failed").await;
        assert_eq!(status, "failed");
        assert_eq!(attempt, 1); // manual runs never retry
        assert_eq!(*runner.calls.lock().unwrap(), 1);
    }

    async fn search_test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    async fn insert_search_run(
        pool: &sqlx::SqlitePool,
        project_id: &str,
        status: &str,
        mode: &str,
        prompt: &str,
        created_at: &str,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, stdout, stderr, created_at)
             VALUES (?, ?, ?, ?, 'secret stdout', 'secret stderr', ?)",
        )
        .bind(project_id)
        .bind(prompt)
        .bind(status)
        .bind(mode)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    #[tokio::test]
    async fn search_filters_runs_and_returns_only_a_prompt_excerpt() {
        let pool = search_test_pool().await;
        let matching_prompt = "ã".repeat(PROMPT_EXCERPT_CHARS as usize + 100);
        let matching = insert_search_run(
            &pool,
            "project-a",
            "completed",
            "worktree",
            &matching_prompt,
            "2026-03-10T12:00:00+00:00",
        )
        .await;
        insert_search_run(
            &pool,
            "project-a",
            "failed",
            "worktree",
            "March Autopilot work",
            "2026-03-11T12:00:00+00:00",
        )
        .await;
        insert_search_run(
            &pool,
            "project-a",
            "completed",
            "shadow",
            "March Autopilot work",
            "2026-03-12T12:00:00+00:00",
        )
        .await;
        insert_search_run(
            &pool,
            "project-b",
            "completed",
            "worktree",
            "March Autopilot work",
            "2026-03-13T12:00:00+00:00",
        )
        .await;
        insert_search_run(
            &pool,
            "project-a",
            "completed",
            "worktree",
            "March Autopilot work outside window",
            "2026-04-01T00:00:00+00:00",
        )
        .await;

        let entries = search(
            &pool,
            &SearchFilter {
                project_id: Some("project-a".into()),
                status: Some("completed".into()),
                mode: Some("worktree".into()),
                q: Some("ã".into()),
                since: Some(
                    chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
                        .unwrap()
                        .with_timezone(&chrono::Utc),
                ),
                until: Some(
                    chrono::DateTime::parse_from_rfc3339("2026-03-31T23:59:59Z")
                        .unwrap()
                        .with_timezone(&chrono::Utc),
                ),
                limit: 50,
            },
        )
        .await
        .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, matching);
        let excerpt_chars = entries[0].prompt_excerpt.chars().count();
        assert_eq!(excerpt_chars, PROMPT_EXCERPT_CHARS as usize);
        assert!(entries[0].prompt_excerpt.len() > excerpt_chars);
        assert_eq!(
            entries[0].prompt_excerpt,
            "ã".repeat(PROMPT_EXCERPT_CHARS as usize)
        );
        let json = serde_json::to_value(&entries[0]).unwrap();
        assert!(json.get("stdout").is_none());
        assert!(json.get("stderr").is_none());
    }
}
