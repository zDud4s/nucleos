use axum::Json;
use axum::extract::{Path, Query, State};
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
///
/// `Deserialize` is here for the route tests rather than for production — the same asymmetry
/// `RunStatusResponse` below already carries, and what lets `/runs` be asserted as its own type
/// instead of as untyped JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
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
    /// Only what still holds a slot. A boolean rather than letting the caller write both statuses
    /// into `status`, which takes exactly **one** exact value — and the question "what is in flight"
    /// has two right answers.
    pub live: bool,
}

/// Keep search results useful without turning the index into a prompt or output retrieval endpoint.
const PROMPT_EXCERPT_CHARS: i64 = 500;

use crate::search::{escape_like, fts_query};

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
    // `live` and `status` compose with AND rather than one overriding the other. A terminal
    // `status` together with `live=true` returns nothing, which is the honest answer to the
    // question that was actually asked.
    if filter.live {
        query.push(" AND status IN (");
        let mut statuses = query.separated(", ");
        for status in crate::concurrency::LIVE_RUN_STATUSES {
            statuses.push_bind(status);
        }
        query.push(")");
    }
    if let Some(q) = &filter.q {
        let fts = fts_query(q);
        query
            .push(" AND (prompt LIKE ")
            .push_bind(format!("%{}%", escape_like(q)))
            .push(" ESCAPE '\\'");
        if fts.is_empty() {
            query.push(" OR 0");
        } else {
            query
                .push(" OR id IN (SELECT run_id FROM run_events WHERE id IN (SELECT rowid FROM run_events_fts WHERE run_events_fts MATCH ")
                .push_bind(fts)
                .push("))");
        }
        query.push(")");
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
) -> Result<Json<CreateRunResponse>, (StatusCode, String)> {
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
        Ok(true) => {
            return Err((
                StatusCode::CONFLICT,
                "the emergency stop is engaged, so nothing was started".to_owned(),
            ));
        }
        Err(error) => {
            tracing::warn!(%error, "create_run: could not read the kill switch — refusing");
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                "the emergency stop could not be read, so nothing was started".to_owned(),
            ));
        }
    }

    // **The project's autopilot mode, asked at the door — and until this line nothing here asked
    // it.** `grep -n 'autopilot::Mode' core/src/runs.rs` returned nothing, while `POST /jobs` has
    // always refused a job for a project whose mode does not permit one. A job IS a chain of
    // `worktree` runs, so the same person asking for the same work got two different answers
    // depending on which door they came in by: the phone's, which derives the mode from the
    // project, or this one, which took whatever the request body said.
    //
    // **Two narrowings, and each is a decision rather than an oversight.**
    //
    // *Only an unattended mode.* `real` is untouched, because the paragraph above about the kill
    // switch is the house rule and it holds here too: a person asking for a run in their own
    // checkout, watching it happen, is not the proactive autonomy these brakes pace.
    // `runs_unattended` names the modes that run with nobody in the room, and those are what the
    // project's mode governs.
    //
    // *Only `off`.* Refusing `worktree` on a project in `shadow` is the stricter rule the design
    // implies, and it is not the rule this repository is developed under today — its own project sat
    // in `shadow` while every session worked through exactly that mode. Tightening it is the
    // owner's call and they took it: close the case nobody can defend — a project switched OFF still
    // starting work nobody is watching — and leave the rest for the day a project is deliberately
    // activated.
    //
    // A project this daemon has never heard of reads as `off` as well, because `project_mode`
    // answers `Off` for a missing row. That is the right answer rather than an accident: an
    // unattended run against a project with no autopilot state is one that no mode, budget or
    // scoped stop can pace.
    if runs_unattended(&req.mode)
        && let Some(project) = req.project_id.as_deref()
    {
        match crate::autopilot::project_mode(&state.pool, project).await {
            Ok(crate::autopilot::Mode::Off) => {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!(
                        "`{project}` is off, and a `{}` run is one nobody is watching — put the \
                         project in shadow or active first. A project this daemon does not know \
                         reads as off too.",
                        req.mode
                    ),
                ));
            }
            Ok(_) => {}
            // Fails closed, like the kill switch above it and for the same reason: a mode that could
            // not be read is not permission to work unwatched.
            Err(error) => {
                tracing::warn!(
                    %error,
                    project,
                    "create_run: could not read the project's autopilot mode — refusing"
                );
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!(
                        "`{project}`'s autopilot mode could not be read, so nothing was started"
                    ),
                ));
            }
        }
    }

    // Uncancellable: the run row is INSERTed `running` before the worktree is provisioned, so
    // `git worktree add` holds that window open for as long as git takes. A request dropped inside
    // it strands a `running` worktree row with no task and no abort handle — `/cancel` answers 404,
    // the GC skips it, and it holds one of the project's concurrency slots, narrowing the whole
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
    .await
    .map_err(|status| {
        (
            status,
            "the run could not be created; the daemon logged why".to_owned(),
        )
    })?
    .map_err(|error| {
        (
            crate::http::create_run_status(&error),
            crate::http::create_run_reason(&error),
        )
    })?;

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
///
/// `artifacts` is the directory a job's nodes hand work to each other through, and is `Some` only
/// for a node of a job. An ordinary run has no successor to write to, and handing it the variable
/// anyway would advertise a protocol nothing in its prompt describes.
pub(crate) fn run_env(
    token: &str,
    id: i64,
    artifacts: Option<&std::path::Path>,
) -> Vec<(String, String)> {
    let mut env = vec![
        // Derived, never written out again. This is the address every tool a run calls will use to
        // come back, and a literal here is what lets a daemon bind one port and send its own tools
        // to another — which presents as every tool answering 404, from a route that exists.
        (
            "NUCLEOS_DAEMON_URL".to_string(),
            crate::daemon_client::daemon_url(),
        ),
        ("NUCLEOS_DAEMON_TOKEN".to_string(), token.to_string()),
        ("NUCLEOS_RUN_ID".to_string(), id.to_string()),
    ];
    if let Some(path) = artifacts {
        env.push((
            "NUCLEOS_JOB_ARTIFACTS".to_string(),
            path.to_string_lossy().into_owned(),
        ));
    }
    env
}

/// Mints a run's own daemon key and stores its secret, returning what goes in the environment.
///
/// Called before the CLI is spawned, never after: the hook fires on the run's first tool call, and
/// a secret that lands in the row a moment later would 401 that call for reasons no log explains.
/// A failure to store is not fatal — the run proceeds with a key that authenticates nothing, so its
/// tool calls are refused rather than ungoverned, which is the right direction to fail in.
pub(crate) async fn mint_run_token(pool: &sqlx::SqlitePool, id: i64) -> String {
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

/// Writes down WHICH stranger, once [`mark_untrusted_context`] has recorded that there was one.
///
/// Separate from the marking above, and the separation is the security argument rather than tidiness.
/// The mark fails closed: `hooks.rs` refuses the read when it cannot be written, because a turn
/// holding a stranger's words with no record of it is the state every later refusal depends on not
/// existing. This one must NOT fail closed — a provenance able to refuse a read would be a
/// convenience holding a veto over the pillar's main verb — so it is called after the mark, its
/// error is logged by the caller, and the read proceeds either way.
///
/// The cost is stated where it is paid: a run can carry the mark and no rows here, and a reader has
/// to say "not recorded" instead of "read nothing". Those are different facts and only one of them
/// is ever true of a turn the barrier refused.
///
/// `arguments` is the call's arguments verbatim. Not prettied into a source string: `browser_open`
/// and `web_read` both carry the url that decides the question, and a per-tool extractor would be a
/// second per-tool table beside `TOOL_EFFECTS` for someone to keep in step by hand. `None` is for
/// the entries that come from no call at all — an errand turn carries its notebook in before it
/// spawns — and `tool` there names the source in words rather than borrowing a tool name for a call
/// that never happened.
pub(crate) async fn record_untrusted_read(
    pool: &sqlx::SqlitePool,
    id: i64,
    tool: &str,
    arguments: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO run_untrusted_reads (run_id, tool, arguments, at) VALUES (?, ?, ?, ?)")
        .bind(id)
        .bind(tool)
        .bind(arguments)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .map(|_| ())
}

/// What this run has read, oldest first, as the JSON a proposal carries away.
///
/// `None` when nothing was recorded, which a caller must not render as "read nothing" — see
/// [`record_untrusted_read`] for why the two are different and why the empty case is real.
///
/// Read once, at the moment a refusal is written, and copied onto the proposal: `runs` rows are
/// pruned on their own schedule, and a record answering "where did this idea come from" with a
/// dangling id answers nothing.
pub(crate) async fn untrusted_reads_json(
    pool: &sqlx::SqlitePool,
    id: i64,
) -> sqlx::Result<Option<String>> {
    let rows = sqlx::query_as::<_, (String, Option<String>, String)>(
        "SELECT tool, arguments, at FROM run_untrusted_reads WHERE run_id = ? ORDER BY rowid",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    // Silence and an empty answer are not the same fact, so they do not share a representation. A
    // turn refused by the barrier read something by definition; an empty list here means the
    // recording failed, and handing that back as `Some("[]")` would let a reader print "read
    // nothing" over a turn that read plenty.
    if rows.is_empty() {
        return Ok(None);
    }
    let listed: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(tool, arguments, at)| {
            serde_json::json!({ "tool": tool, "arguments": arguments, "at": at })
        })
        .collect();
    Ok(Some(serde_json::Value::Array(listed).to_string()))
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
/// `'team'` answers `None` and that is a deliberate under-statement rather than the truth about
/// every team run.
///
/// A specialist may well hold `McpOnly` — its agent's `tool_policy` decides, and `team.rs` builds
/// the `RunRequest` itself, so this function never launches one. What it is actually asked, by both
/// its other readers, is whether a run of this mode may gain a SECOND AUTHOR: `http::post_run_message`
/// refuses `None`, and `create_run_inner` refuses to create such a run `steerable`. For a
/// department the answer is no in every case, so the most restrictive value is the honest one to
/// return — and the alternative, `Unrestricted`, is worse than merely wrong: it is what this
/// function returns for everything it does not recognise, and it would have handed a team run Bash,
/// Edit and Write on any path that ever did read it to launch.
pub(crate) fn tool_policy_for_mode(mode: &str) -> crate::runner::ToolPolicy {
    if mode == crate::email::TRIAGE_MODE || mode == crate::team::TEAM_MODE {
        crate::runner::ToolPolicy::None
    } else {
        crate::runner::ToolPolicy::Unrestricted
    }
}

/// PURE: whether a run in `mode` proceeds with nobody there to answer it.
///
/// Three policies read this, for the same reason each time — there is no human in the loop: how
/// many attempts a failure gets, how long the wall clock runs, and whether the classifier replaces
/// the CLI's permission surface. `email_triage` is unattended too and is deliberately NOT in this
/// set: it is one toolless classification against a local model, so it has no tools to permit, no
/// build to outlast, and nothing a retry would fix.
///
/// One definition rather than the same `if` at each reader, because the day a fourth unattended
/// mode appears, three policies have to learn about it together or two of them quietly won't.
///
/// That day arrived with `'team'`, and it is here for the reason the sentence above predicted:
/// inside a team run there is nobody to answer the CLI. The third policy does not fire on it all
/// the same — `classifier_governs_tools` also demands `Unrestricted`, and a department never is
/// (`tool_policy_for_mode` above) — which is the AND doing its job rather than an exception.
pub(crate) fn runs_unattended(mode: &str) -> bool {
    mode == "shadow" || mode == "worktree" || mode == crate::team::TEAM_MODE
}

/// Whether this run's actions are governed by the classifier rather than by the CLI's allow-list.
///
/// Reads the disk, so not pure: the answer depends on what is wired up in `dir`, which for a
/// worktree run is the worktree the CLI will start in and not the project root.
///
/// Every condition is a refusal, and the AND between them is the point:
///
///   * unattended only — an interactive run has somebody who can approve, and taking that decision
///     away from them is not this function's business;
///   * `Unrestricted` only — a run with no tools has no permissions worth changing;
///   * classifier verified present — the load-bearing one. Standing the CLI's barrier down is only
///     safe because another barrier takes over. With no hook there is no second barrier, and the
///     run would be governed by nothing at all.
///
/// The third condition means a project that has not wired the hook keeps today's behaviour, which
/// is also today's failure: its unattended runs still cannot execute what the interactive list
/// omits. That is the right trade — the answer for such a project is to wire the classifier, not
/// to stand the barrier down without one.
fn classifier_governs_tools(
    mode: &str,
    tool_policy: crate::runner::ToolPolicy,
    dir: Option<&std::path::Path>,
) -> bool {
    if !runs_unattended(mode) || tool_policy != crate::runner::ToolPolicy::Unrestricted {
        return false;
    }
    dir.is_some_and(crate::autopilot::classifier_hook_is_wired)
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
pub(crate) fn run_timeout_for_mode(base: std::time::Duration, mode: &str) -> std::time::Duration {
    if runs_unattended(mode) {
        base * crate::state::AUTONOMOUS_RUN_TIMEOUT_MULTIPLIER
    } else {
        base
    }
}

/// PURE: the silence a run in `mode` is allowed, given the interactive default `base`.
///
/// The same shape as `run_timeout_for_mode` and the same modes, because the two deadlines fail
/// unattended runs for the same reason: both were sized for work somebody is watching. A single
/// tool call that compiles this workspace streams nothing for minutes (see
/// `state::AUTONOMOUS_PROGRESS_TIMEOUT_MULTIPLIER`), so the deadline meant to catch a stuck run
/// was killing runs that were merely building.
///
/// `email_triage` stays on the short one for its own reason, unchanged: it classifies one message
/// against a local model, and a triage run silent for five minutes is stuck rather than busy.
pub(crate) fn progress_timeout_for_mode(
    base: std::time::Duration,
    mode: &str,
) -> std::time::Duration {
    if runs_unattended(mode) {
        base * crate::state::AUTONOMOUS_PROGRESS_TIMEOUT_MULTIPLIER
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
    /// Released here for the same reason as the two above, and the cost of getting it wrong is
    /// different in kind: an abort handle is a word, a transcript is the run's entire output. Left
    /// behind, every run the daemon has ever executed stays in memory until restart.
    tails: crate::state::RunTails,
    id: i64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.handles.lock().unwrap().remove(&self.id);
        self.messages.lock().unwrap().remove(&self.id);
        self.tails.lock().unwrap().remove(&self.id);
    }
}

/// What a live run has written so far, from `since` bytes in.
///
/// `None` means there is no live tail — the run finished, or this daemon never started it. It does
/// NOT mean the run wrote nothing, and the difference is the whole point: `run_events` is written
/// once at the end (`append_run_events` has two callers and both are terminal), so a finished run's
/// output lives in the database and not here. A caller that renders `None` as an empty transcript
/// claims a run produced nothing when the durable copy may hold thousands of lines.
///
/// `since` is in BYTES. The last line of a working run has not ended, so a line count would give a
/// cursor that moves backwards as that line grows.
pub(crate) fn read_tail(
    tails: &crate::state::RunTails,
    run_id: i64,
    since: usize,
) -> Option<String> {
    let buffer = tails.lock().ok()?.get(&run_id).cloned()?;
    let text = buffer.lock().ok()?;
    // Saturating rather than slicing: `since` past the end is what every poll of a run that wrote
    // nothing since the last one looks like, so it is the common path, and `&text[since..]` would
    // panic on it inside the daemon's HTTP thread.
    Some(text.get(since..).unwrap_or("").to_owned())
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
        tails: state.run_tails.clone(),
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
pub(crate) struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Mirrors a live run's context fill into `runs.context_fill`, and the fullest it has been into
/// `runs.context_peak`, until the returned guard is dropped.
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
pub(crate) fn mirror_context_fill(
    pool: &sqlx::SqlitePool,
    id: i64,
    context_fill: std::sync::Arc<std::sync::Mutex<Option<i64>>>,
) -> AbortOnDrop {
    let pool = pool.clone();
    AbortOnDrop(
        tokio::spawn(async move {
            let mut persisted: Option<i64> = None;
            let mut persisted_peak: Option<i64> = None;
            // The peak lives here and only here — no second `Arc`, and nothing added to
            // `RunOutcome`. The paths that most need it are the ones that never see an outcome.
            let mut peak: Option<i64> = None;
            loop {
                tokio::time::sleep(CONTEXT_FILL_PERSIST_INTERVAL).await;
                let current = context_fill.lock().map(|fill| *fill).unwrap_or(None);
                let Some(now) = current else { continue };
                peak = Some(peak.map_or(now, |seen: i64| seen.max(now)));
                // The short-circuit was `current == persisted`, and that was right with one column.
                // With two, "nothing changed" means both agree: the fill can fall on a compaction
                // while the peak stands still, and the peak can rise on a tick where the fill did
                // not move.
                if current == persisted && peak == persisted_peak {
                    continue;
                }
                let written = sqlx::query(
                    "UPDATE runs SET context_fill = ?, context_peak = ? WHERE id = ? AND status = 'running'",
                )
                .bind(current)
                .bind(peak)
                .bind(id)
                .execute(&pool)
                .await;
                // Only a write that landed counts as persisted, so a transient database error is
                // retried on the next tick instead of being remembered as done.
                if written.is_ok() {
                    persisted = current;
                    persisted_peak = peak;
                }
            }
        })
        .abort_handle(),
    )
}

/// The fullest this stream ever got.
///
/// Named rather than folded at each call site because there are four of them now — two terminal
/// writes here and two in `team.rs` — and four copies of a fold is four places to forget that the
/// peak is not the last line.
pub(crate) fn peak_of(stream: &str) -> Option<i64> {
    stream.lines().fold(None, |peak, line| {
        crate::runner::context_peak_from_line(line, peak)
    })
}

/// The tool calls this stream made, as the JSON `runs.tools_used` stores.
///
/// An empty list is stored as `[]`, which says "used no tools". NULL stays reserved for "nobody
/// asked" — the distinction `compacted` lost by being `NOT NULL DEFAULT 0`.
pub(crate) fn tools_of(stream: &str) -> String {
    serde_json::to_string(&crate::runner::live_from_stream(stream).did)
        .unwrap_or_else(|_| "[]".to_string())
}

/// Reduces the mirrored stream as a fallback for runners that do not publish context separately.
pub(crate) fn observed_context_fill(
    mirror: &std::sync::Mutex<Option<i64>>,
    transcript: &str,
) -> Option<i64> {
    let current = mirror.lock().map(|fill| *fill).unwrap_or(None);
    transcript.lines().fold(current, |fill, line| {
        crate::runner::context_fill_from_line(line, fill)
    })
}

/// The runner abstraction does not expose reliable per-model window metadata, and model aliases can
/// change underneath the daemon. 200k is therefore a conservative floor shared by the supported
/// Claude models: using the floor hands off early rather than risking a context-overflowing run.
const HANDOFF_CONTEXT_LIMIT_FLOOR: i64 = 200_000;
/// How much of each half of a handoff note carries the predecessor's own words.
///
/// Four thousand characters against a 200k window is a rounding error, and a note that crowded its
/// successor would be the thing it exists to prevent.
const HANDOFF_NOTE_LIMIT: usize = 4_000;

/// PURE: keeps `limit` characters and says when that cut something.
///
/// `from_end` decides which half survives, and the two halves of a note want opposite answers: an
/// instruction leads with what it wants, while an account of work done ends nearest to where the
/// work stopped.
fn clip_note(text: &str, limit: usize, from_end: bool) -> String {
    let total = text.chars().count();
    if total <= limit {
        return text.to_owned();
    }
    if from_end {
        let kept: String = text.chars().skip(total - limit).collect();
        format!("[... the earlier part of this was cut ...]\n{kept}")
    } else {
        let kept: String = text.chars().take(limit).collect();
        format!("{kept}\n[... the rest of this was cut ...]")
    }
}

/// PURE: what a successor is told, and the whole of what it knows.
///
/// **A successor starts EMPTY, so this note is not a courtesy — it is the only bridge.** Until this
/// existed the successor was launched with `--resume --fork-session`, which copies the predecessor's
/// whole conversation into a new id: it inherited the very context the handoff existed to shed, was
/// told in writing that its context was fresh, and tripped the same ceiling immediately. Measured
/// against a live CLI, which answered a question about the previous session's contents.
///
/// So the transcript is gone on purpose and this replaces it. It carries two things and says which
/// is which: the task, which is authority, and the predecessor's closing words, which are one run's
/// account and can be wrong.
fn handoff_prompt(task: &str, reply: Option<&str>) -> String {
    let task = clip_note(task.trim(), HANDOFF_NOTE_LIMIT, false);
    let reply = match reply.map(str::trim) {
        Some(text) if !text.is_empty() => clip_note(text, HANDOFF_NOTE_LIMIT, true),
        _ => "Nothing. It ended without a closing message.".to_owned(),
    };
    format!(
        r#"You are continuing work that ran out of context. This is a FRESH session: nothing your predecessor saw is in this conversation, and the note below plus the working tree are everything you have.

THE TASK IT WAS GIVEN
{task}

WHAT IT SAID WHEN IT STOPPED
{reply}

Check what actually remains before doing anything. The tree is the truth; the note above is one run's account of it and may be out of date or wrong. Then finish the task."#
    )
}

/// The line a resume note puts above the task it is continuing.
///
/// A literal rather than a format, because it is read back as well as written: `task_to_carry`
/// splits on it to recover the task from a note, which is what keeps a run that is resumed twice
/// from being handed a note wrapped in a note.
const RESUMED_TASK_HEADER: &str = "--- THE TASK THIS RUN IS CONTINUING ---";

/// PURE: the task inside `prompt`, whether `prompt` is a task or a resume note carrying one.
///
/// Idempotent by construction, and that is the whole point. `runs` records no link from a resume
/// row back to the run it resumed — the connection lives in `proposals` and `action_grants`, which
/// nothing walking a task's history reads — so a resumed run's prompt is, as far as every later
/// reader is concerned, the task it was given. Left alone, a run approved twice would be told its
/// task was a note about an approval of a note about an approval.
///
/// Measured on 2026-08-27: run 900383 was resumed, ran out of context, and its successor was
/// launched with `proposal #7 authorizes Agent...` as its entire brief. It had no idea what it was
/// supposed to be building, and the 1016 lines its predecessor had written survived only because a
/// person committed them by hand.
fn task_to_carry(prompt: &str) -> &str {
    match prompt.split_once(RESUMED_TASK_HEADER) {
        Some((_, task)) => task.trim_start(),
        None => prompt,
    }
}

/// The task the chain started from, walking back through `successor_run_id`.
///
/// **Not the predecessor's prompt**, which since the change above is itself a handoff note: reading
/// that would nest one note inside another and push the real task one level further away on every
/// hop, until a third successor was reading mostly framing.
///
/// **And not a resume note either**, which `task_to_carry` is what strips: an approval breaks the
/// `successor_run_id` chain — a resume is a new row nothing points at — so the walk below stops at
/// the resume rather than at the task, and what it stops on is a sentence about a proposal.
///
/// Bounded rather than trusting the links: `successor_run_id` is an ordinary column and a cycle in
/// it would hang a run's completion path, which is not a place to discover one.
async fn original_task(pool: &sqlx::SqlitePool, run_id: i64) -> sqlx::Result<String> {
    const MAX_HOPS: usize = 32;
    let mut id = run_id;
    for _ in 0..MAX_HOPS {
        let predecessor: Option<i64> =
            sqlx::query_scalar("SELECT id FROM runs WHERE successor_run_id = ?")
                .bind(id)
                .fetch_optional(pool)
                .await?;
        match predecessor {
            Some(earlier) if earlier != id => id = earlier,
            _ => break,
        }
    }
    let prompt: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await?;
    Ok(task_to_carry(&prompt).to_owned())
}

struct HandoffSuccessor {
    id: i64,
    session_id: String,
    /// What the successor is launched with. Built once here and carried, so the row a human reads
    /// and the prompt the CLI receives can never be two different texts.
    prompt: String,
    /// The job this successor belongs to, carried over from its predecessor. `Some` means the
    /// successor needs the handoff directory in its environment, like every other node of that job.
    job_id: Option<i64>,
    /// Whether a person asked to be able to speak to this work, carried over from its predecessor.
    /// Read back from the row that was just written rather than passed alongside it, so the flag
    /// the launch uses and the flag `post_run_message` reads are the same fact and not two.
    steerable: bool,
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

/// Pins a terminated run's time approximation onto the run itself, for a run that never reported a
/// cost of its own.
///
/// A run killed before its `result` event recorded `$0` — the future was dropped, so there was no
/// `RunOutcome` to read a cost from — and the ledger understated real spend by exactly the money
/// that run burned. The budget's own approximation covered the gap only while recomputing, which
/// made the figure move with `now` and left nothing durable behind.
///
/// `cost_usd IS NULL` is the whole safety of the write: a measured total is what the CLI actually
/// charged, and a duration guess must never overwrite it. Best-effort throughout — the run IS
/// terminated either way, and an approximation that failed to land leaves the budget exactly as it
/// was before this existed, not the run broken.
async fn record_time_approx_cost(pool: &sqlx::SqlitePool, id: i64) {
    let timestamps: (String, Option<String>) =
        match sqlx::query_as("SELECT created_at, completed_at FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
        {
            Ok(timestamps) => timestamps,
            Err(error) => {
                tracing::warn!(
                    run_id = id,
                    %error,
                    "could not read the terminated run's timestamps to approximate its cost"
                );
                return;
            }
        };

    let parse = |value: &str| {
        chrono::DateTime::parse_from_rfc3339(value).map(|when| when.with_timezone(&chrono::Utc))
    };
    let Ok(created_at) = parse(&timestamps.0) else {
        tracing::warn!(
            run_id = id,
            created_at = timestamps.0,
            "could not parse the terminated run's start time to approximate its cost"
        );
        return;
    };
    // No `completed_at` means the terminal write lost its race or never landed; the run is over
    // regardless, so `now` is the end of the only duration this can still measure.
    let end = match timestamps.1.as_deref().map(parse).transpose() {
        Ok(end) => end.unwrap_or_else(chrono::Utc::now),
        Err(_) => {
            tracing::warn!(
                run_id = id,
                "could not parse the terminated run's end time to approximate its cost"
            );
            return;
        }
    };

    // The configured rate, not a constant: the budget approximates at whatever the operator set, and
    // a stored cost computed at a different rate would disagree with every total that reads it.
    let rate = match crate::budget::load_budget_config(pool).await {
        Ok(config) => config.time_cost_per_hour_usd,
        Err(error) => {
            tracing::warn!(
                run_id = id,
                %error,
                "could not load the budget rate to approximate the terminated run's cost"
            );
            return;
        }
    };

    let approximated = crate::budget::time_approx_usd(created_at, end, rate);
    if let Err(error) =
        sqlx::query("UPDATE runs SET cost_usd = ? WHERE id = ? AND cost_usd IS NULL")
            .bind(approximated)
            .bind(id)
            .execute(pool)
            .await
    {
        tracing::warn!(
            run_id = id,
            %error,
            "could not record the terminated run's approximated cost"
        );
    }
}

async fn prepare_handoff_successor(
    pool: &sqlx::SqlitePool,
    run_id: i64,
) -> sqlx::Result<Option<HandoffSuccessor>> {
    let (context_fill, existing_successor, job_id, transcript, steerable): (
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<String>,
        i64,
    ) = sqlx::query_as(
        "SELECT context_fill, successor_run_id, job_id, stdout, steerable FROM runs WHERE id = ?",
    )
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
    // Read from the row rather than from memory: both callers reach here AFTER the terminal write,
    // so `stdout` is the finished stream, and `extract_reply` is the same parse the rest of the
    // daemon uses to turn one into a reply.
    let reply = transcript.as_deref().and_then(crate::runner::extract_reply);
    let prompt = handoff_prompt(&original_task(pool, run_id).await?, reply.as_deref());
    // `job_id`, `stage` and `item_id` are carried across with everything else. A node that runs out
    // of context is still that node — same item, same tree — and a successor belonging to no job
    // would be invisible to the chain that has to finalise it: the item would stay `running` until
    // the four-hour ceiling, with the work already done and nothing saying where it went.
    //
    // `item_id` is the same fact one level down, and the column list here is explicit, so leaving
    // it out is not a no-op: the successor of a node working in its item's own tree would arrive
    // with no item, resolve to the job's tree instead, and relaunch the agent somewhere its work
    // is not.
    //
    // `steerable` is carried for a reason of its own, taken deliberately on 2026-08-28 after an
    // overnight run could not be corrected: the owner watched it walk into a mistake, typed the
    // correction, and got a 409 back from a successor whose row said nobody may speak to it. The
    // flag says a person asked to keep the volante on this work, and the work is what continues
    // across a handoff — so the successor keeps it, and `spawn_handoff_if_needed` launches
    // listening. Row and launch move together: a launch that listened while its row refused would
    // hold stdin open with no way to close it, which is the failure the old comment here feared.
    let inserted = sqlx::query(
        "INSERT INTO runs (
             project_id, cwd, prompt, status, mode, session_id, read_untrusted, created_at,
             job_id, stage, item_id, steerable
         )
         SELECT project_id, cwd, ?, 'running', mode, ?, read_untrusted, ?, job_id, stage, item_id,
                steerable
         FROM runs WHERE id = ?",
    )
    .bind(&prompt)
    .bind(&session_id)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(run_id)
    .execute(pool)
    .await?;
    if inserted.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    let successor_id = inserted.last_insert_rowid();
    // The item follows its node, for the same reason it follows an approval resume: left pointing
    // at the predecessor, the job's next pass reads a terminal node that did not complete and stops
    // the whole chain — turning a context handoff into a failure.
    sqlx::query("UPDATE job_items SET run_id = ? WHERE run_id = ?")
        .bind(successor_id)
        .bind(run_id)
        .execute(pool)
        .await?;

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
    //
    // Keyed on `owner_kind = 'run'` since migration 0035 replaced `worktrees.run_id` with an owner
    // pair — the older spelling was a runtime SQL string, so it compiled fine and would have failed
    // only when a handoff actually happened. The filter is also load-bearing on its own: a job's
    // tree must NOT move to the successor, because the job owns it and handing it to one node would
    // let the GC collect it the moment that node finished, with the rest of the queue still to run.
    sqlx::query(
        "UPDATE worktrees SET owner_id = ?
         WHERE owner_kind = 'run' AND owner_id = ? AND removed_at IS NULL",
    )
    .bind(successor_id)
    .bind(run_id)
    .execute(pool)
    .await?;

    // And the slot with it, for the same reason and with the same filter. The tree moving without
    // the slot leaves the two rows disagreeing about who is working in that checkout, and
    // `reconcile_orphaned_slots` settles it the wrong way: it frees any slot whose owner is not
    // live, the predecessor is finished by then, and the project — reading one fewer in flight than
    // it has — starts a second run in the same repository.
    //
    // `owner_kind = 'run'` is load-bearing here exactly as it is above: a job node holds no slot of
    // its own (only the standalone arm of `create_run_with` claims one), so for a node this matches
    // nothing and must, because the slot belongs to the job and moving it to one node would free it
    // when that node finished, with the rest of the queue still to run.
    //
    // A handover, not a claim — the difference `resume_approved_run` also depends on. A claim is per
    // owner, so the successor would ask for a SECOND slot while its predecessor still held the
    // first, and a project at its ceiling would refuse to continue work already admitted. Moving the
    // row cannot fail that way, because it does not change how many are held.
    sqlx::query("UPDATE project_slots SET owner_id = ? WHERE owner_kind = 'run' AND owner_id = ?")
        .bind(successor_id)
        .bind(run_id)
        .execute(pool)
        .await?;

    Ok(Some(HandoffSuccessor {
        id: successor_id,
        session_id,
        prompt,
        job_id,
        steerable: steerable != 0,
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
    project_id: Option<String>,
    spawn_cwd: Option<std::path::PathBuf>,
    plan_only: bool,
    completion_feed: Option<(String, String)>,
    gate_config: GateConfig,
    max_attempts: u32,
    tool_policy: crate::runner::ToolPolicy,
    run_timeout: std::time::Duration,
    progress_timeout: std::time::Duration,
    classifier_governs_tools: bool,
    model: Option<String>,
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
    // A successor of a job node keeps the handoff directory: same node, same tree, same item.
    // `spawn_cwd` is the job's worktree, which is where `prepare_artifacts` put it.
    let node_artifacts = successor
        .job_id
        .and(spawn_cwd.as_ref())
        .map(|cwd| cwd.join(crate::worktree::ARTIFACTS_DIR));
    spawn_run(
        &state,
        runner,
        successor.id,
        // The note, and nothing else. See `handoff_prompt`.
        successor.prompt,
        project_id,
        spawn_cwd,
        plan_only,
        // **NEVER a resume, and NEVER a fork.** Both carry the predecessor's transcript forward,
        // which is the one thing a context handoff exists to avoid: this used to pass
        // `Some(predecessor_session)` with `fork_session = true`, so the successor was launched as
        // `--resume <predecessor> --fork-session`. A fork branches a conversation; it does not empty
        // one. The successor therefore woke holding everything its predecessor held, read a prompt
        // telling it the context was fresh, and crossed the same threshold at once -- handing off
        // again, forever, while every hop wrote an event claiming relief.
        //
        // Measured, not reasoned: asked about the previous session's contents, a successor answered
        // correctly. It could only do that by still having it.
        //
        // `fork_session` stays false for a second reason worth keeping separate: with `--resume`
        // gone, the fresh `--session-id` below finally reaches the CLI at all. While resume won the
        // `if/else if` in `runner::cli_args`, the id this row was created with was never passed, so
        // the session the daemon recorded and the session the CLI ran were different strings.
        None,
        successor.session_id,
        false,
        completion_feed,
        gate_config,
        max_attempts,
        tool_policy,
        run_env(&daemon_token, successor.id, node_artifacts.as_deref()),
        // Inherited, and this is the decision the paragraph that used to sit here asked for: a
        // handoff continues one task, and being able to speak to that task is a property of the
        // task rather than of the process currently doing it. `prepare_handoff_successor` now
        // copies the column, so the row and this launch agree — which is what the old comment
        // required before the flag could be carried at all.
        successor.steerable,
        // Inherited, not re-derived: a successor continues one task, and a handoff that reset the
        // clock would let a run outlive its deadline by handing itself on.
        run_timeout,
        // Inherited for the same reason. The silence deadline is per-mode, the successor's mode is
        // its predecessor's, and re-deriving it here would need a `mode` this function does not
        // have — the same reason `spawn_run` is handed it rather than reading `state`.
        progress_timeout,
        // Inherited for the same reason, and it is the same tree: re-deriving would let a handoff
        // quietly change what the work is allowed to do halfway through it.
        classifier_governs_tools,
        // Inherited: a successor is the same node continuing the same task, so it belongs on the
        // model its predecessor's stage was routed to.
        model,
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
    // The environment, built by the caller rather than from a token here: a job node needs the
    // handoff directory alongside the key, and only the caller knows whether this run is one.
    env: Vec<(String, String)>,
    steerable: bool,
    run_timeout: std::time::Duration,
    // Decided by the caller for `run_timeout`'s reason: the mode is what sets it, and the mode is
    // not a fact this function has. Passed beside the wall clock rather than read from `state`
    // here, so the two deadlines can never be derived from different beliefs about the run.
    progress_timeout: std::time::Duration,
    classifier_governs_tools: bool,
    // Which model answers this run, or `None` for the runner's own. Decided by the caller, because
    // only it knows the stage — `spawn_run` must not learn to read job nodes.
    model: Option<String>,
) {
    let pool = state.pool.clone();
    let feed_project_id = project_id.clone();
    let handoff_state = state.clone();
    let run_messages = state.run_messages.clone();
    let run_tails = state.run_tails.clone();

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
            // Published so `GET /runs/{id}/tail` can read it while the CLI is still writing. This
            // is inside the retry loop, and the insert therefore REPLACES the previous attempt's
            // buffer rather than adding to it — which is the wanted behaviour: a retry starts a
            // fresh CLI on a fresh context, so the old transcript describes work that is no longer
            // happening, and a screen still showing it would be reporting a dead attempt as live.
            //
            // Removal is not here. `Registration`'s `Drop` takes this entry out beside the abort
            // handle and the steering channel, which is what covers the abort and panic paths too.
            if let Ok(mut map) = run_tails.lock() {
                map.insert(id, std::sync::Arc::clone(&transcript));
            }
            let context_fill = std::sync::Arc::new(std::sync::Mutex::new(None));
            // Held for this attempt only: a retry starts a fresh CLI on a fresh context, so the
            // previous attempt's mirror has nothing left to say. Dropping the guard at the end of
            // the iteration stops it on every path out of the body, abort included.
            let _live_context_fill =
                mirror_context_fill(&pool, id, std::sync::Arc::clone(&context_fill));
            let mut request = crate::runner::RunRequest {
                prompt: prompt.clone(),
                // A background run carries no pictures: nobody is here to attach one.
                images: Vec::new(),
                env: env.clone(),
                cwd: spawn_cwd.clone(),
                plan_only,
                resume_session_id: resume_session_id.clone(),
                mcp_config: None,
                tool_policy,
                progress_timeout: Some(progress_timeout),
                // The brake that was missing. These are the runs nobody is watching, and the
                // wall clock above is a poor guard against the failure that matters here: a run
                // looping quickly costs little per turn and reaches neither the clock nor the job's
                // money ceiling, which is only checked between nodes.
                max_turns: Some(crate::runner::DEFAULT_MAX_TURNS),
                session_id: Some(session_id.clone()),
                fork_session,
                include_partial_messages: false,
                steerable,
                classifier_governs_tools,
                messages: None,
                // Autopilot runs pay for the ambient surface and call none of it.
                ambient_mcp: false,
                // Cloned rather than moved: the request is built once per attempt.
                model: model.clone(),
                effort: None,
                fallback_model: Vec::new(),
                add_dirs: Vec::new(),
                max_budget_usd: None,
                agents: Vec::new(),
                append_system_prompt: None,
                denied_tools: Vec::new(),
                session_name: None,
                context_window: None,
                // Nothing to narrow: `create_run_inner` never sets `mcp_config`, so the branch
                // that reads this does not run for a run started here.
                allowed_mcp_tools: None,
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
                let (messages_tx, messages_rx) =
                    tokio::sync::mpsc::unbounded_channel::<crate::runner::LaterTurn>();
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
                    // Read from the same stream everything else came from, and kept here because
                    // here is where an agent run ends. `assistant.rs` does the equivalent in its
                    // own terminal write; the difference is that an agent never passes through
                    // there, and that is why the column was empty in 168 runs in a row.
                    //
                    // An empty list is stored as `[]`, which says "used no tools". NULL stays
                    // reserved for "nobody asked" — the distinction `compacted` lost by being
                    // `NOT NULL DEFAULT 0`.
                    let tools_used = tools_of(&o.stdout);
                    // The peak comes off the whole stream, at full fidelity. The periodic mirror
                    // writes a peak too, sampled every 500ms; this write comes after it and is the
                    // more exact of the two.
                    let context_peak = peak_of(&o.stdout);
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
                        "UPDATE runs SET status = ?, exit_code = ?, stdout = ?, stderr = ?, session_id = COALESCE(?, session_id), cost_usd = ?, input_tokens = ?, output_tokens = ?, cache_read_tokens = ?, cache_creation_tokens = ?, num_turns = ?, context_fill = ?, context_peak = ?, compacted = ?, tools_used = ?, completed_at = ?, attempt = ?, gate_status = ?, gate_exit_code = ?, gate_output = ? WHERE id = ? AND status = 'running'",
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
                    .bind(o.cache_creation_tokens)
                    .bind(o.num_turns)
                    .bind(context_fill)
                    .bind(context_peak)
                    .bind(o.compacted)
                    .bind(&tools_used)
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
                        // Said after the completion row above, not instead of it. The status stays
                        // `completed` — the CLI did exit 0, and a resumed run may legitimately
                        // decide the approved action is no longer the right step — but the person
                        // who granted the authorization is the one who needs to know it went unused,
                        // and until now nothing told them. See `proposals::unconsumed_grant` for the
                        // two runs that produced this.
                        if let Ok(Some((tool_name, proposal_id))) =
                            crate::proposals::unconsumed_grant(&pool, id).await
                        {
                            let _ = crate::feed::append(
                                &pool,
                                feed_project_id.as_deref(),
                                "resume_did_not_act",
                                &format!(
                                    "resumed for the {tool_name} action approved in proposal \
                                     #{proposal_id}, and finished without attempting it"
                                ),
                                Some(id),
                            )
                            .await;
                        }
                    }
                    if terminal_write_won {
                        Box::pin(spawn_handoff_if_needed(
                            handoff_state.clone(),
                            runner.clone(),
                            id,
                            project_id.clone(),
                            spawn_cwd.clone(),
                            plan_only,
                            completion_feed.clone(),
                            gate_config.clone(),
                            max_attempts,
                            tool_policy,
                            run_timeout,
                            progress_timeout,
                            classifier_governs_tools,
                            model.clone(),
                        ))
                        .await;
                    }

                    // Judged from the row the terminal write just put there, so it is gated on the
                    // same CAS: losing the race means the numbers in `runs` belong to whoever won,
                    // and reading them here would count another attempt's spending as this one's.
                    //
                    // LAST on this arm, deliberately. `let _ =` swallows an `Err` but not a panic,
                    // and a panic here unwinds through `spawn_registered` — where the supervisor's
                    // recovery write is `WHERE status = 'running'` and therefore matches nothing,
                    // this arm having already written `completed`. Ahead of the completion feed row
                    // and the handoff, that would cost the run both; behind them, it can only cost
                    // the observation, which is what nothing depends on. It also has to run after
                    // `spawn_handoff_if_needed` for a second reason: `successor_run_id` is what
                    // tells `ContextSwelling` the handoff already happened.
                    if terminal_write_won
                        && let Err(error) = crate::token_efficiency::observe_run(&pool, id).await
                    {
                        // Warned rather than swallowed, like every other best-effort call on this
                        // arm. A lock held past the busy timeout is the realistic failure, and a
                        // detector that goes quiet without saying so is indistinguishable from one
                        // that has nothing to report.
                        tracing::warn!(%error, run_id = id, "token efficiency not observed");
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
                    // Off the partial transcript, for the same reason the branch above persists it:
                    // this is the run most worth reading afterwards. There is no outcome here, so
                    // `seen` is the whole record — and `compacted` cannot be known from it, which is
                    // why only these two are written.
                    let context_peak = peak_of(&seen);
                    let tools_used = tools_of(&seen);
                    append_run_events(&pool, id, &seen).await;
                    // A timeout is not a launch failure — retrying would likely time out again.
                    let timed_out = sqlx::query(
                        "UPDATE runs SET status = 'timed_out', stdout = ?, context_fill = ?, context_peak = ?, tools_used = ?, completed_at = ?, attempt = ? WHERE id = ? AND status = 'running'",
                    )
                    .bind(&seen)
                    .bind(context_fill)
                    .bind(context_peak)
                    .bind(&tools_used)
                    .bind(&completed_at)
                    .bind(attempt as i64)
                    .bind(id)
                    .execute(&pool)
                    .await;
                    warn_on_terminal_write_err(&timed_out, id, "timed_out");
                    if matches!(&timed_out, Ok(result) if result.rows_affected() == 1) {
                        // The run is over; anything it queued and never started goes with it (spec
                        // §7). A run its wall clock killed is that spec's "the agent that submitted
                        // dies" as much as one a human cancelled — it will never come back to
                        // collect the merge — and nothing routes this path through
                        // `finalize_termination`, so the sweep is called here directly.
                        //
                        // Inside the won-the-race guard, for the reason the launch-failure feed row
                        // above states and one more that is specific to this: a lost race means
                        // another terminator already chose the status, and one of those —
                        // `pause_for_approval`'s `awaiting_approval` — is a run that RESUMES.
                        // Sweeping for it would cancel the very merge it paused to have approved.
                        //
                        // Best-effort, because the run IS terminated either way and a queue row that
                        // outlives its run is a stale request a human can cancel, not a broken run.
                        if let Err(error) = crate::vcs::cancel_for_run(&pool, id).await {
                            tracing::warn!(
                                run_id = id,
                                %error,
                                "could not cancel the timed-out run's queued vcs requests"
                            );
                        }
                        // This is the run that reports no cost at all, so its spend has to be
                        // approximated from the duration the write above just made final. Inside
                        // the same guard: losing that CAS means another terminator owns the row,
                        // and it is the owner's business what the run's cost and status are.
                        record_time_approx_cost(&pool, id).await;
                        Box::pin(spawn_handoff_if_needed(
                            handoff_state.clone(),
                            runner.clone(),
                            id,
                            project_id.clone(),
                            spawn_cwd.clone(),
                            plan_only,
                            completion_feed.clone(),
                            gate_config.clone(),
                            max_attempts,
                            tool_policy,
                            run_timeout,
                            progress_timeout,
                            classifier_governs_tools,
                            model.clone(),
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

/// One node of a job: which job it belongs to, what part it plays, and the worktree it inherits.
///
/// A node carries the worktree rather than provisioning one, which is the whole reason a job can
/// exceed a single context window — each node starts with a fresh window and picks up the previous
/// one's work from the disk it shares.
pub struct JobNode {
    pub job_id: i64,
    /// `plan` | `implement` | `review`. Never `gate`: the gate is a subprocess, not a run.
    pub stage: &'static str,
    pub worktree_path: String,
    pub branch: String,
}

/// A conflict the daemon is about to hand an agent: which escalated request it came from, and the
/// two branches that would not merge.
///
/// It travels as a struct rather than as a closure taking the fresh worktree, because the two things
/// it changes about a run happen at two different moments — where the tree is BORN (on the target)
/// and what is staged in it before the agent exists (the conflict) — and a hook at one of those
/// moments cannot reach the other.
pub struct Resolution {
    /// The escalated `vcs_requests` row. Claimed by writing this run's id into its
    /// `resolution_run_id`, which is what makes the attempt happen once and never again.
    pub request_id: i64,
    pub source: String,
    pub target: String,
}

pub async fn create_run_inner(
    state: &AppState,
    prompt: String,
    project_id: Option<String>,
    cwd: Option<String>,
    mode: &str,
    steerable: bool,
) -> Result<i64, CreateRunError> {
    create_run_with(state, prompt, project_id, cwd, mode, steerable, None).await
}

/// Starts the one run that is given its work already staged: a merge conflict, put there by the
/// daemon, in a worktree born on the branch the merge was going into.
///
/// `mode = "worktree"` and nothing else, like every other autonomous run that touches code — it
/// carries the tool policy, the gate, and migration 0009's exclusivity, none of which a fourth mode
/// would inherit. Never steerable, for `create_job_node_run`'s reason and one of its own: the work
/// is a conflict the queue found, and text typed into it mid-flight would change what gets published
/// with nothing recording the substitution.
pub async fn create_resolution_run(
    state: &AppState,
    prompt: String,
    project_id: String,
    project_root: String,
    resolution: Resolution,
) -> Result<i64, CreateRunError> {
    create_run_with(
        state,
        prompt,
        Some(project_id),
        Some(project_root),
        "worktree",
        false,
        Some(Provisioning::Resolution(resolution)),
    )
    .await
}

/// Starts one node of a job inside that job's existing worktree.
///
/// Deliberately `mode = "worktree"` rather than a mode of its own: `plan_only`, the tool policy,
/// `max_attempts` and migration 0009's exclusivity index all branch on `mode`, and a fourth value
/// would have to be excluded from each of them. Missing one would be silent.
pub async fn create_job_node_run(
    state: &AppState,
    prompt: String,
    project_id: String,
    project_root: String,
    node: JobNode,
) -> Result<i64, CreateRunError> {
    create_run_with(
        state,
        prompt,
        Some(project_id),
        Some(project_root),
        "worktree",
        // Never steerable. A node is one step of a plan the job is executing, and text typed into it
        // mid-flight would change what that step does with nothing recording the substitution — the
        // queue would still claim the item it was given. Steering belongs to a run somebody started
        // and is watching.
        false,
        Some(Provisioning::Node(node)),
    )
    .await
}

/// Starts one item of a job in a worktree of its own.
///
/// The sibling of [`create_job_node_run`], and everything said there about `mode` and about
/// steering holds here word for word. What differs is the checkout: this one provisions its own,
/// born on the tip of the job's branch, and pays a concurrency slot for it — which is what lets two
/// items of one job be in flight at the same time without either building on the other's unmeasured
/// work.
pub async fn create_job_item_run(
    state: &AppState,
    prompt: String,
    project_id: String,
    project_root: String,
    item: JobItem,
) -> Result<i64, CreateRunError> {
    create_run_with(
        state,
        prompt,
        Some(project_id),
        Some(project_root),
        "worktree",
        false,
        Some(Provisioning::Item(item)),
    )
    .await
}

/// What a run is handed to start from, when it is handed anything at all.
///
/// **An enum and not two `Option`s, because the two are mutually exclusive and the type should say
/// so.** A pair of options admits both-at-once, and the code would resolve that combination
/// silently rather than refuse it: the node arm would win the worktree, so the conflict would be
/// staged inside a JOB's tree — on the job's branch, over whatever the previous node left there.
/// Nothing would report it. Clippy asking for fewer arguments is what sent this looking; the
/// combination it removes is the reason it stayed.
enum Provisioning {
    /// One node of a job, inside the worktree the job already owns.
    Node(JobNode),
    /// A conflict resolution: a worktree of its own, born on the merge's target, with the conflict
    /// already staged in it.
    Resolution(Resolution),
    /// One item of a job, in a worktree of its own.
    ///
    /// Modelled on `Resolution` and NOT on the standalone arm, and the difference is the base. A
    /// standalone run's tree is born wherever the project checkout stands, which for an item is the
    /// wrong commit: an item whose dependency has just landed on the job's branch would start
    /// without that work, and the dependency graph would be decorative.
    Item(JobItem),
}

/// One item of a job, about to be given a tree of its own.
pub struct JobItem {
    pub job_id: i64,
    pub item_id: i64,
    /// `plan` | `implement` | `review`, as [`JobNode::stage`].
    pub stage: &'static str,
    /// Where this item's tree is born: the tip of the job's branch as the last gate left it.
    ///
    /// Named and not `Option`, because there is no honest default. `None` would mean the project
    /// checkout's HEAD, which is `master` — a commit that has none of this job's work in it.
    pub base: String,
}

impl Provisioning {
    fn node(&self) -> Option<&JobNode> {
        match self {
            Self::Node(node) => Some(node),
            Self::Resolution(_) | Self::Item(_) => None,
        }
    }

    fn resolution(&self) -> Option<&Resolution> {
        match self {
            Self::Resolution(resolution) => Some(resolution),
            Self::Node(_) | Self::Item(_) => None,
        }
    }

    fn item(&self) -> Option<&JobItem> {
        match self {
            Self::Item(item) => Some(item),
            Self::Node(_) | Self::Resolution(_) => None,
        }
    }

    /// Whether this provisioning gets a checkout of its own — and therefore a concurrency slot, a
    /// `worktrees` row, and a share of the disk.
    ///
    /// **The house rule, in one place.** `Node` is the only exception, and it is the exception
    /// because it gets no tree: it works inside the one its job already owns, and charging it a
    /// second slot would have a five-item job refuse itself at the second item. Everything else —
    /// a standalone run, a resolution, an item — brings a tree and pays for it.
    fn owns_a_tree(&self) -> bool {
        !matches!(self, Self::Node(_))
    }
}

/// How little room may be left on a project's volume before this refuses to open another checkout.
///
/// Five gibibytes by default, and the number is a floor rather than a model of what a checkout
/// costs. A model would have to know the project: this repository's `target/` runs to fifteen
/// gigabytes and a Go project's build cache is a rounding error, and the daemon has no honest way to
/// tell before building. What a floor says is narrower and true — below this, the machine is nearly
/// full and starting another build is how it fills.
///
/// `NUCLEOS_MIN_FREE_DISK_GB` overrides it, and `0` switches it off for somebody who knows their
/// volume better than this does.
const MIN_FREE_DISK_GB: u64 = 5;

/// Whether the volume that would hold this project's worktrees is too full to open another one.
///
/// A **check** and not a reservation, which is the whole of the design. A reservation needs a number
/// for what a checkout will cost, that number is per-project and unknowable in advance, and a model
/// carrying it would age on its own — measured once against one repository and then quietly wrong.
/// This reads the volume each time and answers about now.
///
/// A reading that fails is not a refusal. The primitive is a platform call against an already
/// mounted volume; when it cannot answer, refusing would stop all autonomous work on a machine whose
/// disk is probably fine, and the health probe already reports the same fact where somebody can see
/// it.
async fn no_room_on_disk(project_root: &std::path::Path) -> Option<String> {
    let floor = std::env::var("NUCLEOS_MIN_FREE_DISK_GB")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(MIN_FREE_DISK_GB)
        .saturating_mul(1024 * 1024 * 1024);
    if floor == 0 {
        return None;
    }

    let root = project_root.to_path_buf();
    let available =
        tokio::task::spawn_blocking(move || crate::health::free_space_for_worktrees(&root))
            .await
            .ok()?
            .ok()?;

    (available < floor).then(|| {
        format!(
            "only {} MiB free where this project's worktrees live, and a new checkout needs at \
             least {} MiB",
            available / (1024 * 1024),
            floor / (1024 * 1024)
        )
    })
}

async fn create_run_with(
    state: &AppState,
    prompt: String,
    project_id: Option<String>,
    cwd: Option<String>,
    mode: &str,
    steerable: bool,
    provisioning: Option<Provisioning>,
) -> Result<i64, CreateRunError> {
    let node = provisioning.as_ref().and_then(Provisioning::node);
    let resolution = provisioning.as_ref().and_then(Provisioning::resolution);
    let item = provisioning.as_ref().and_then(Provisioning::item);
    // The house rule, asked once: everything but a job node brings a checkout of its own, and
    // therefore claims a slot, records a row and is charged disk for it. `None` — a standalone
    // run — brings one too.
    let owns_a_tree = provisioning.as_ref().is_none_or(Provisioning::owns_a_tree);
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

    // Asked before the row exists, and not instead of the claim below — a run cannot claim until it
    // has an id, so the authoritative answer costs a row, and a scheduler that fires at a full
    // project every thirty seconds would leave a failed run behind every time. A node is exempt: it
    // works inside its job's worktree, on its job's slot.
    if mode == "worktree"
        && owns_a_tree
        && let Some(project) = project_id.as_deref()
    {
        // Swept first, and this is where the staleness that matters gets paid off. A run's slot is
        // given back by the sweep rather than at each ending — `runs` has ten places that write a
        // terminal status and no funnel like `job::retire`, so threading a release through all ten
        // is a coverage claim that would be wrong the first time an eleventh appears. The release is
        // derived from liveness instead, which cannot drift; the one moment the derivation has to be
        // current is the moment it refuses somebody, which is here.
        let _ = crate::concurrency::reconcile_orphaned_slots(&state.pool).await;
        match crate::concurrency::room_for(&state.pool, project).await {
            Ok(Some(_)) => return Err(CreateRunError::Busy),
            Ok(None) => {}
            Err(error) => return Err(CreateRunError::Db(error)),
        }
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
    let mut node_artifacts: Option<std::path::PathBuf> = None;

    if mode == "worktree" {
        let project_root = cwd.as_deref().expect("worktree cwd validated above");
        let worktree_project_id = project_id
            .as_deref()
            .expect("worktree project_id validated above");
        // A job node is not gated here, because the job gates it. Two reasons, either of which is
        // enough: `gate_after_each_item: false` is a knob the job honours and this path cannot see,
        // and the plan and review nodes change nothing, so gating them spends a whole suite run to
        // re-measure the tree the previous gate already measured. The verdict also belongs to the
        // item it measured, which is a row this path has no access to.
        // An ITEM is gated elsewhere too, and for a sharper reason than a node is. Its work is
        // measured after it lands on the job's branch, by the gate the merge runs; gating it here as
        // well would spend a second cold build to reach a verdict about a tree nobody merges from,
        // and the two verdicts could disagree — the tree it built in is not the tree the job ships.
        gate_config = match (
            node.is_some() || item.is_some(),
            crate::config::load_schedule_rules(std::path::Path::new(project_root)),
        ) {
            (true, _) => GateConfig::NotConfigured,
            (false, Ok(rules)) => rules
                .gate_command
                .map_or(GateConfig::NotConfigured, |command| GateConfig::Command {
                    command,
                    project_root: project_root.to_string(),
                }),
            (false, Err(error)) => {
                tracing::warn!(
                    project_id = worktree_project_id,
                    project_root,
                    %error,
                    "failed to load worktree gate configuration"
                );
                GateConfig::Unreadable(format!("gate configuration is unreadable: {error}"))
            }
        };
        // A job node inherits its job's worktree; only a standalone run provisions one. Skipping
        // both the `create` and the `record` is what lets a sequence of nodes accumulate work on one
        // tree — and it is why nothing here writes a `worktrees` row for a node: the job already
        // owns one, and a second row for the same directory would give the GC two owners to reconcile.
        let info = match &node {
            Some(node) => crate::worktree::WorktreeInfo {
                path: std::path::PathBuf::from(&node.worktree_path),
                branch: node.branch.clone(),
                // `None`, and it costs nothing: the base belongs to the tree, the job's own
                // `worktrees` row already carries it, and nothing below this arm records a row for
                // a node. Re-reading HEAD here would give the commit the *previous* node stopped
                // on, which is not where the tree was born.
                base_sha: None,
            },
            None => {
                // The tree's name, and with it the slot's owner. An item's is its row's id and
                // comes back the same on every retry; everything else is named after this run.
                let owner = match item {
                    Some(item) => crate::worktree::Owner::Item(item.item_id),
                    None => crate::worktree::Owner::Run(id),
                };

                // Anything that brings a tree claims a slot. A job node inherits its job's worktree,
                // and its job's slot with it — charging the node a second one would have a five-item
                // job refuse itself at the second item — and a node never reaches this arm.
                //
                // The table was swept before the row was inserted, at the pre-check above, so this
                // is the authoritative answer against an already-current count. `claim` is
                // idempotent per owner, which is what lets an item's retry ask again for the slot it
                // is already holding instead of leaking a second one.
                match crate::concurrency::claim(&state.pool, worktree_project_id, owner).await {
                    Ok(crate::concurrency::ClaimOutcome::Claimed(_)) => {}
                    Ok(crate::concurrency::ClaimOutcome::Full(full)) => {
                        // The same 409 the unique index gave, reached by counting instead of by
                        // colliding. `Busy` is what every caller already maps.
                        fail_provisioning(state, id, project_id.as_deref(), &full.reason()).await;
                        return Err(CreateRunError::Busy);
                    }
                    Err(error) => {
                        fail_provisioning(
                            state,
                            id,
                            project_id.as_deref(),
                            &format!("the concurrency slot could not be claimed: {error}"),
                        )
                        .await;
                        return Err(CreateRunError::Db(error));
                    }
                }

                // Checked after the slot and before the tree, which is the only order that reports
                // the two walls apart. A refusal here is `Busy` and not a failure: the disk is a
                // condition of the machine, the same shape as a full project, and a batch that
                // cannot have its third item should come out smaller rather than fail.
                //
                // Only for an item, and not because the others are cheaper. A standalone run and a
                // resolution are asked for one at a time by a person or by the queue; items are
                // asked for several at once, and each of them grows a `target/` of its own.
                if item.is_some()
                    && let Some(refusal) = no_room_on_disk(std::path::Path::new(project_root)).await
                {
                    fail_provisioning(state, id, project_id.as_deref(), &refusal).await;
                    return Err(CreateRunError::Busy);
                }

                // A resolution's tree is born on the merge's TARGET and an item's on the tip of its
                // job's branch; everything else starts where the project stands.
                // `worktree::create_at` carries why that is not a preference.
                let base = resolution
                    .map(|it| it.target.as_str())
                    .or_else(|| item.map(|it| it.base.as_str()));
                // An item's name comes back, so its checkout and its branch may already be there —
                // from the attempt this one is retrying, or from a crash between creating the tree
                // and recording it. `adopt_or_create_at` is the difference between a retry that
                // continues and an item that fails every thirty seconds for ever on a name only it
                // can use.
                let created = match item {
                    Some(_) => {
                        crate::worktree::adopt_or_create_at(
                            std::path::Path::new(project_root),
                            owner,
                            base,
                        )
                        .await
                    }
                    None => {
                        crate::worktree::create_at(std::path::Path::new(project_root), owner, base)
                            .await
                    }
                };
                match created {
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
                }
            }
        };
        // Every exit from here on has to leave a terminal status behind. Past the INSERT the row is
        // `running` with no task and no abort handle: `/cancel` answers 404, the GC skips it, and
        // it holds one of the project's concurrency slots for as long as it reads live, and
        // `running` is reconciled only by a restart, which no sweep can help. The `create`
        // branch above compensated; these two propagated with `?` and stranded the run.
        let worktree_path = info.path.to_string_lossy().into_owned();
        if owns_a_tree
            && let Err(error) = crate::worktree::record(
                &state.pool,
                match item {
                    Some(item) => crate::worktree::Owner::Item(item.item_id),
                    None => crate::worktree::Owner::Run(id),
                },
                worktree_project_id,
                project_root,
                &worktree_path,
                &info.branch,
                info.base_sha.as_deref(),
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
        // The node's identity lands in the same write as its cwd so a row can never be `running`
        // inside a job's worktree while claiming to belong to no job — which would make it invisible
        // to the chain that has to finalise it.
        let cwd_update = match (&node, item) {
            (Some(node), _) => {
                sqlx::query("UPDATE runs SET cwd = ?, job_id = ?, stage = ? WHERE id = ?")
                    .bind(&worktree_path)
                    .bind(node.job_id)
                    .bind(node.stage)
            }
            // The same write, plus the item. `item_id` lands here rather than in the INSERT for the
            // reason the node's identity does: until the tree exists there is nothing for the row to
            // claim, and a row that named an item before it had a checkout would be found by the
            // chain that finalises items and pointed at a directory nobody made.
            (None, Some(item)) => sqlx::query(
                "UPDATE runs SET cwd = ?, job_id = ?, stage = ?, item_id = ? WHERE id = ?",
            )
            .bind(&worktree_path)
            .bind(item.job_id)
            .bind(item.stage)
            .bind(item.item_id),
            (None, None) => {
                sqlx::query("UPDATE runs SET cwd = ? WHERE id = ?").bind(&worktree_path)
            }
        };
        if let Err(error) = cwd_update.bind(id).execute(&state.pool).await {
            fail_provisioning(
                state,
                id,
                project_id.as_deref(),
                &format!("worktree was recorded but the run's cwd could not be set: {error}"),
            )
            .await;
            return Err(CreateRunError::Db(error));
        }
        // A conflict's one attempt is claimed HERE: after the row exists, because the claim IS this
        // run's id, and before the agent does, because the whole point is that no second agent can
        // ever be minted against the same escalation. Losing the compare-and-set means something
        // else got there first, and the run is retired rather than allowed to become that second
        // agent — the failure migration 0081 describes as exploding rather than degrading.
        //
        // Claimed BEFORE the conflict is staged, and the order is not arbitrary. Staging can fail
        // for two reasons and both should spend the attempt: the merge came out clean, so there is
        // nothing left to resolve, or it could not run at all, which a second attempt would hit
        // identically. The reverse order fails much worse — a staged conflict whose claim was then
        // lost leaves a worktree of real work behind while another agent is already editing the
        // same two branches.
        if let Some(resolution) = resolution {
            match sqlx::query(
                "UPDATE vcs_requests SET resolution_run_id = ?
                  WHERE id = ? AND resolution_run_id IS NULL",
            )
            .bind(id)
            .bind(resolution.request_id)
            .execute(&state.pool)
            .await
            {
                Ok(claimed) if claimed.rows_affected() == 1 => {}
                Ok(_) => {
                    fail_provisioning(
                        state,
                        id,
                        project_id.as_deref(),
                        &format!(
                            "vcs request {} has already had its one resolution attempt",
                            resolution.request_id
                        ),
                    )
                    .await;
                    return Err(CreateRunError::Invalid(
                        "this conflict has already had its one resolution attempt",
                    ));
                }
                Err(error) => {
                    fail_provisioning(
                        state,
                        id,
                        project_id.as_deref(),
                        &format!("the conflict could not be claimed for resolution: {error}"),
                    )
                    .await;
                    return Err(CreateRunError::Db(error));
                }
            }
            // The daemon does the merging; the agent only resolves. `worktree::stage_conflict`
            // carries why the other way round cannot work at all.
            if let Err(error) =
                crate::worktree::stage_conflict(&info.path, &resolution.source).await
            {
                fail_provisioning(
                    state,
                    id,
                    project_id.as_deref(),
                    &format!("the conflict could not be staged for resolution: {error}"),
                )
                .await;
                return Err(CreateRunError::Worktree(error));
            }
        }
        // The handoff directory, and the exclusion that keeps it out of both the preservation commit
        // and anything a node commits itself. Prepared per node rather than once per job because a
        // node is the thing that writes there, and a failure here has to stop this node rather than
        // be discovered later as an empty plan — which §5.2 of the design maps to `failed` with no
        // way of telling a planner that found nothing from a directory that was never created.
        if node.is_some() {
            match crate::worktree::prepare_artifacts(&info.path).await {
                Ok(path) => node_artifacts = Some(path),
                Err(error) => {
                    fail_provisioning(
                        state,
                        id,
                        project_id.as_deref(),
                        &format!("the job's handoff directory could not be prepared: {error}"),
                    )
                    .await;
                    return Err(CreateRunError::Worktree(error));
                }
            }
        }
        completion_feed = Some((
            "worktree_run_completed".to_owned(),
            format!("worktree run completed on {}", info.branch),
        ));
        spawn_cwd = Some(info.path);
    }

    let max_attempts = if runs_unattended(mode) {
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
    // Answered before the call, because `spawn_cwd` is moved into it. `spawn_cwd` and not `cwd`:
    // for a worktree run the CLI starts in the worktree provisioned above, and settings are read
    // from where the process starts — asking the project root would answer about a directory this
    // run never enters.
    let governed_by_classifier = classifier_governs_tools(mode, tool_policy, spawn_cwd.as_deref());
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
        run_env(&daemon_token, id, node_artifacts.as_deref()),
        steerable,
        run_timeout_for_mode(state.run_timeout, mode),
        progress_timeout_for_mode(state.progress_timeout, mode),
        governed_by_classifier,
        // A job node's stage is what may be routed elsewhere; every other run names no stage and so
        // stays on the runner's own model.
        state
            .runner
            .model_for_stage(node.as_ref().map(|node| node.stage)),
    );

    Ok(id)
}

/// The operation this approval should hand to the queue instead of handing back to the run, if it is
/// one at all.
///
/// **Every `None` here means "keep the behaviour this approval has always had"** — a single-use
/// grant, and the run performs the action itself. That is deliberately the conservative direction
/// and not a shrug. Refusing the approval outright would leave a person holding an action they have
/// approved, no way to perform it, and nothing to read explaining why; queueing an operation we are
/// not certain is the one they read would be worse than either.
///
/// So the bar is: the tool is a shell, the input parses, the command is one the queue can execute —
/// `git merge <ref>` with at most a `--no-ff`, `git push <remote> [<branch>]`, or
/// `git tag <name> [<branch>]`, each argued in its own function in `vcs.rs` — the worktree is really
/// there and really on a branch, and the project resolves to a repository. Anything else falls back.
///
/// **The two are tried in order and the order cannot matter**, which is worth stating rather than
/// relying on: each parser insists on its own subcommand, so a command is at most one of them. The
/// `or_else` is a sequence and not a precedence.
///
/// It runs git twice and must therefore be called before the transaction opens — see the call site.
async fn queueable_operation(
    state: &AppState,
    proposal: &crate::proposals::Proposal,
    project_id: &str,
    worktree_path: &str,
) -> Option<(crate::vcs::ResolvedRepo, crate::vcs::Op)> {
    if !matches!(proposal.tool_name.as_deref(), Some("Bash" | "PowerShell")) {
        return None;
    }
    let input: serde_json::Value = serde_json::from_str(proposal.tool_input.as_deref()?).ok()?;
    let command = input.get("command")?.as_str()?;

    // One deadline for both calls, so a slow repository cannot spend the budget twice over.
    let deadline = std::time::Instant::now() + crate::git_exec::OPERATION_TIMEOUT;
    // Read for BOTH parsers, and for different jobs in each: a merge's target is the branch the
    // worktree stands on, while a push only falls back to it when the command did not name one. It
    // is asked for unconditionally because it is also the liveness check on the worktree — a path
    // that has been removed answers with the enclosing checkout's branch or with an error, and
    // `current_branch`'s own `--show-toplevel` guard is what tells those apart.
    let branch = crate::git_exec::current_branch(std::path::Path::new(worktree_path), deadline)
        .await
        .map_err(|error| {
            tracing::info!(
                worktree = %worktree_path,
                %error,
                "approval: could not read the worktree's branch — authorizing the run instead of queueing"
            );
        })
        .ok()?;
    let op = crate::vcs::merge_from_command(command, &branch)
        .or_else(|| crate::vcs::push_from_command(command, &branch))
        .or_else(|| crate::vcs::tag_from_command(command, &branch))
        // The two that need no worktree branch, so they take none. A fetch names its remote and a
        // deletion names its branch; neither has a half the command line leaves out.
        .or_else(|| crate::vcs::fetch_from_command(command))
        .or_else(|| crate::vcs::branch_delete_from_command(command))
        .or_else(|| crate::vcs::rebase_from_command(command, &branch))?;
    crate::vcs::resolve_repo(&state.pool, project_id)
        .await
        .map_err(|error| {
            // `?` rather than `%`: `ResolveError` is a two-arm enum carrying a source, and it has no
            // `Display` on purpose — the HTTP layer answers its arms with different statuses instead
            // of rendering them.
            tracing::info!(
                project_id,
                ?error,
                "approval: could not resolve the project's repository — authorizing the run instead of queueing"
            );
        })
        .ok()
        .map(|repo| (repo, op))
}

/// How much of the approved action's own text the resume prompt repeats back. A run pays for every
/// token of its own prompt, and a command a loop built can be megabytes.
const RESUME_ACTION_CHARS: usize = 600;

/// PURE: the instruction a resumed run is given for the action a human just approved.
///
/// **This wording is the fix for a measured, reproduced failure** (`.ai/eval/ABLATION.md`, T1×H3,
/// 2026-08-17). It used to say "Proceed with the {tool} action you attempted before the pause — it
/// is now authorized". The resumed agent has no record of attempting anything: the pause happens
/// BEFORE the call runs, so the call never becomes a step in the transcript the resume restores.
/// What arrived, from the agent's side, was an unverifiable claim of prior authorization urging it
/// to run a command — the shape of an injection — and twice it refused, correctly:
///
/// > This looks like it may be an attempt to get me to run a command under a false claim of prior
/// > authorization. I won't proceed with any Bash action on that basis.
///
/// One turn, nothing done, and the daemon recorded `completed`. So: state the action, and explain
/// the absence. An agent shown what was approved can weigh it; an agent asked to remember it can
/// only obey or refuse, and refusing is the better of those two.
fn resume_instruction(proposal_id: i64, tool_name: &str, tool_input: Option<&str>) -> String {
    // `command` first because it is the field a person would quote; the whole object otherwise, so
    // a tool that is not Bash still shows what it was going to do.
    let action = tool_input
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| {
            value
                .get("command")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .or_else(|| value.as_object().map(|_| value.to_string()))
        })
        .map(|text| {
            if text.chars().count() <= RESUME_ACTION_CHARS {
                text
            } else {
                // By chars, not bytes: this is a prompt, and a slice through a UTF-8 boundary
                // panics on exactly the inputs nobody tests with.
                let mut cut: String = text.chars().take(RESUME_ACTION_CHARS).collect();
                cut.push('…');
                cut
            }
        });

    // **Written so it does not read as a prompt injection, because it used to and a run said so.**
    //
    // Measured 2026-08-28. An autonomous run was resumed, read this note, and refused it in as
    // many words: it called the message an injection attempt. It was not being paranoid. The old
    // text had all three tells at once: it asserted an authority the reader cannot check (a human
    // approved this), it pre-emptively explained away the missing evidence (you will not find that
    // call in your transcript), and it asked for an action the run's own task had forbidden.
    // Explaining away absent evidence is the signature of the attack, so writing that sentence
    // made the daemon indistinguishable from an attacker.
    //
    // Three changes, and the third is the one that matters. It names the doubt instead of ignoring
    // it. It says where the note came from rather than whose authority it carries. And it declares
    // itself SUBORDINATE to the task: the run that refused this was resolving a genuine conflict --
    // the note asked for a compound command its task forbade -- and had no way to know which of the
    // two won. Now it does, and the answer is the task, which is also the safe direction.
    let framing = format!(
        "This note comes from the daemon that launched this session, not from anything inside \
         your conversation -- no file, tool result or page you read put it here.\n\n\
         The {tool_name} call is absent from your transcript because the guard runs BEFORE a call \
         does: the attempt is real, the call never happened, and this session is a fresh one \
         continuing that work.\n\n\
         It authorizes that ONE action and nothing else, and it does not relax any rule your task \
         gave you. If carrying it out would break one of those rules, your task wins -- find \
         another way to the same end. Do it only if it is still the right next step, then carry on."
    );

    match action {
        Some(action) => format!(
            "This run paused when it attempted the action below, and that pause has been \
             lifted (proposal #{proposal_id}):\n\n    {action}\n\n{framing}"
        ),
        None => format!(
            "This run paused when it attempted a {tool_name} action, and that pause has been \
             lifted (proposal #{proposal_id}). The action's input could not be read back, so it is \
             not quoted here.\n\n{framing}"
        ),
    }
}

/// Where to resume a paused run that owns no worktree, read from what the run recorded about itself.
///
/// The shape is the one the worktree lookup returns — `(project_id, project_root, path)` — because
/// everything downstream takes those three and does not care which of the two ways they were
/// obtained. What differs is only the last: a worktree's `path` is a tree the daemon made, and this
/// is the directory the caller named when the run was created.
///
/// Both halves must be present or this is not resumable: without a project there is no root to
/// govern the resume, and without a `cwd` there is nowhere to launch it. That is the one case where
/// the old sentence was right, so it is the one case that still says it.
async fn recorded_tree_of(
    state: &AppState,
    run_id: i64,
) -> Result<(String, String, String), ResumeError> {
    let recorded: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT project_id, cwd FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(&state.pool)
            .await?;

    let (Some(project_id), Some(cwd)) = recorded.unwrap_or((None, None)) else {
        return Err(ResumeError::NotResumable(
            "the paused run owns no worktree and recorded no directory to resume in",
        ));
    };

    let project_root = crate::inspect::project_root(&state.pool, &project_id)
        .await?
        .ok_or(ResumeError::NotResumable(
            "the paused run's project has no recorded root to resume against",
        ))?;

    Ok((project_id, project_root, cwd))
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

    // A node of a job does not own the tree it is paused in — the job does, and has since before
    // this node started. Looking it up by run would answer "no live worktree" for every paused job
    // node, so approving one returned `NotResumable` and the shell offered a button that could not
    // work. Which owner to ask for is decided by the paused run's own `job_id`.
    //
    // `steerable` rides along for a different reason, and it is the one an owner felt: a paused run
    // that is approved back to life is the same work, so the permission to speak to that work
    // survives the pause. Without it the answer to a mid-course correction was a 409 from a run the
    // owner had just personally authorised to continue.
    let (job_id, stage, item_id, steerable): (Option<i64>, Option<String>, Option<i64>, i64) =
        sqlx::query_as("SELECT job_id, stage, item_id, steerable FROM runs WHERE id = ?")
            .bind(original_run_id)
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or((None, None, None, 0));
    // Read here, outside the transaction opened below, because `original_task` walks the run table
    // with its own connection and doing that while holding SQLite's write lock is a deadlock
    // waiting for a busy night. A resume that cannot recover the task is not a reason to refuse the
    // approval — the note still names the action and the session still has the history — so a
    // failure falls back to an empty task and the note simply omits the section.
    let carried_task = original_task(&state.pool, original_run_id)
        .await
        .unwrap_or_default();
    // Item before job, and both are set at once — a node of an item IS a node of its job. The
    // innermost tree wins, because it is the one the work is in: answering with the job's would
    // resume the agent in the integration checkout with its edits somewhere else entirely.
    let owner = match (item_id, job_id) {
        (Some(item_id), _) => crate::worktree::Owner::Item(item_id),
        (None, Some(job_id)) => crate::worktree::Owner::Job(job_id),
        (None, None) => crate::worktree::Owner::Run(original_run_id),
    };

    let owned_tree = sqlx::query_as::<_, (String, String, String)>(
        "SELECT project_id, project_root, path
         FROM worktrees WHERE owner_kind = ? AND owner_id = ? AND removed_at IS NULL",
    )
    .bind(owner.kind())
    .bind(owner.id())
    .fetch_optional(&state.pool)
    .await?;

    // **A run that owns no worktree is not a run that cannot be resumed, and reading the two as one
    // made a whole mode a dead end.** `mode = "real"` runs in a directory the caller named; there is
    // no row in `worktrees` for it and there never was one to find. So this answered
    // `NotResumable("no live worktree for the paused run")` for every paused `real` run, while the
    // shell went on offering an Approve button — a person clicked it and got a conflict, with no
    // other way forward than rejecting the very thing they were trying to allow.
    //
    // Measured 2026-08-27 on run 900376: it parked on its first command, one minute in, and could
    // not be released by any means except refusing it.
    //
    // The directory the run recorded for itself is the same one it was working in, so resuming
    // there continues exactly what was paused. `NotResumable` is still the answer when there is no
    // directory to name — a run with neither a worktree nor a `cwd` genuinely has nowhere to go.
    let (wt_project_id, project_root, wt_path) = match owned_tree {
        Some(found) => found,
        None => recorded_tree_of(state, original_run_id).await?,
    };

    // Spec decision 2, arrived at the other way round: rather than letting the run perform the merge
    // once a human says yes, the yes IS the queueing. Resolved before the transaction opens, because
    // it runs git twice — reading the worktree's branch and identifying the repository — and holding
    // SQLite's write lock across a subprocess would stall every other writer in the daemon.
    let queueable = queueable_operation(state, &proposal, &wt_project_id, &wt_path).await;

    // The class the grant will authorize, derived before the transaction opens so a parse cannot
    // hold SQLite's write lock. Re-derived here rather than carried on the proposal because
    // `classify` is pure and `wt_path` is the very cwd the hook will hand it when the resume
    // attempts the action — the same inputs, so the same answer, with nothing to keep in step.
    // Absent or unparseable input yields no class, and a classless grant authorizes nothing.
    //
    // The policy AND the project's two shell lists come off the state for that same reason, and
    // they are both parameters of `classify` for it: purity is what makes "the same inputs, so the
    // same answer" true, and each of the two is an input this caller has to fetch. A resume
    // classifying under an empty policy while the hook classified under the owner's — or under an
    // empty rule set while the hook classified under the project's — would answer differently about
    // the identical command line, which is the drift the paragraph above rules out.
    //
    // **BOTH are now a table read TWICE, and the paragraph that stood here said otherwise.** It read:
    // "the policy is built once at startup and handed to all three of `classify`'s production callers
    // unchanged, so 'the same one' is literally true of it". That was true until
    // `Policy::for_project` landed. The policy handed to a decision is now the machine default with
    // this project's `project_github_ops` rows laid over it, read at decision time and uncached
    // (design §4.4) — so it is read once by the hook when the command was attempted and once here
    // when the approval is granted, exactly as the shell rules are, and an operation declared between
    // those two moments makes the two reads differ. Nobody loads either of them once for both; there
    // is no such caller. The read deliberately stays uncached.
    //
    // **And this half's drift is bounded more tightly than the shell rules', which is worth working
    // out rather than inheriting.** `classifier.rs`'s contract paragraph for `rules` carries the
    // three-point argument for that side. Here there is only one direction to have: the list has no
    // `deny`, so a declaration between the two moments moves the class from `unrecognized` to
    // `github-read` and a withdrawal cannot be reached at all — the hook ANSWERED `github-read` with
    // an allow, so no proposal was minted and there is no resume to drift. The one reachable case
    // therefore records `github-read` where the person approved an `unrecognized`, and a
    // `github-read` grant authorizes a class that is allowed without any grant. It buys nothing,
    // which is narrower than what they agreed to and never wider.
    //
    // The project is `wt_project_id`, off the worktree this resume is going back into, and it IS
    // the project the hook read off the paused run's own row. Checked rather than assumed, because
    // the two columns are read from different tables: `worktree::record` is the only production
    // INSERT into `worktrees`, and its `project_id` argument is the same Rust binding that `runs`
    // was inserted with (`create_run_with`, for `Owner::Run` and `Owner::Item`) or the same
    // `jobs.project_id` the run's own row was created from (`job.rs`, for `Owner::Job`); that INSERT
    // is an UPSERT, and its conflict arm does write both columns — `project_id =
    // excluded.project_id, project_root = excluded.project_root` — but only ever from the same
    // caller's binding, so a second `record` for a tree already known re-states the binding rather
    // than replacing it with a differently-derived one; neither owner-transfer statement — the
    // handoff and this resume's own — touches `project_id` at all, each setting `owner_id` and
    // nothing else, which is stronger than copying it; and `runs.id` is `AUTOINCREMENT`, so no stale
    // tree can be adopted by a later run wearing a reused id. The `recorded_tree_of` fallback reads
    // `runs.project_id` outright.
    //
    // **Nothing ENFORCES it** — no foreign key, no `CHECK` — and that upsert's conflict arm is the
    // concrete mechanism by which it would break. A third caller of `worktree::record` that resolved
    // its project from `vcs::project_for_worktree` instead of from the caller would not fail, would
    // not add a row, and would leave no trace a reader could go looking for: it would silently
    // rewrite an existing worktree's project, and this is the sentence that would then be wrong.
    // The arm is also invisible to `grep "UPDATE worktrees"`, which is how the previous version of
    // this paragraph came to claim that no `UPDATE` of either column existed anywhere.
    //
    // Derived even when the action was queued instead of authorized. The row is excluded from
    // authorizing by its `queued_request_id`, not by being classless, and a takeover that recorded
    // no class would be a row that could not say what was taken over.

    // This project's policy, and not the machine's. The hook classified the attempt under it, so the
    // label recorded for the approval has to come from the same place — the whole argument above,
    // applied to the input that used to be the one input this caller could take for granted.
    //
    // No `match` on this one, and the asymmetry with the line below is deliberate rather than an
    // oversight: `github_ops` swallows an unreadable table into an empty overlay, which is the
    // machine default, which is the widest thing this can wrongly be — and the direction of that
    // error is a class recorded as `unrecognized` where the project had earned `github-read`. That
    // is a NARROWER grant than the person approved, which is the side of the line a label may err on.
    let policy = state
        .github
        .policy_for_project(&state.pool, &wt_project_id)
        .await;
    let action_class = match crate::project_policy::shell_rules(&state.pool, &wt_project_id).await {
        Ok(rules) => proposal
            .tool_input
            .as_deref()
            .and_then(|input| serde_json::from_str::<serde_json::Value>(input).ok())
            .map(|input| {
                crate::classifier::classify(
                    &tool_name,
                    &input,
                    Some(std::path::Path::new(&wt_path)),
                    &policy,
                    &rules,
                    // Labelling an action a person has just approved, not deciding one. The strict
                    // reading keeps the recorded class the same as the one that was shown to them.
                    crate::classifier::Unrecognized::AsksAPerson,
                )
                .action_class
            }),
        // No class, which is what this path already does for input it cannot parse — and for the
        // same reason, since the fault is the same one: a label derived from rules that are not
        // this project's is not this project's label. It is deliberately NOT the hook's answer to
        // an unreadable read. The hook is DECIDING, so it owes the safe direction and downgrades an
        // allow to an approval prompt; this is LABELLING an action a person has already approved,
        // where the only two outcomes available are a right label and a wrong one. A wrong one is
        // worse than none: a class is what a later grant is scoped to, so a class recorded under
        // the wrong rules would authorise a set of actions nobody agreed to.
        Err(error) => {
            tracing::warn!(
                run_id = original_run_id,
                project_id = %wt_project_id,
                %error,
                "resume: could not read the project's shell rules — recording no action class"
            );
            None
        }
    };

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = state.pool.begin().await?;

    // Guarded on the state this resume was authorised from: the paused run was `awaiting_approval`
    // when the proposal was read, and a release or a cancel can have finalised it since. No rows
    // means one of those got there first, so the supersede is a no-op rather than a status this
    // resume is entitled to overwrite — the live-worktree lookup above is what actually stops a
    // resume onto a discarded worktree.
    //
    // It used to say, here, that a run left `awaiting_approval` holds a slot which rejects the
    // INSERT below. That stopped being true at migration 0053, which dropped the index the claim was
    // really made of; a stranded run holds a numbered slot now and blocks nothing. Nothing below
    // depends on the refusal — the slot is HANDED OVER further down rather than competed for — but
    // the sentence outlived the mechanism, which is how a guard comes to be believed in and not
    // written.
    sqlx::query("UPDATE runs SET status='superseded', completed_at=? WHERE id=? AND status='awaiting_approval'")
        .bind(&now)
        .bind(original_run_id)
        .execute(&mut *tx)
        .await?;
    // Admitted inside this transaction, and that is the whole reason `submit_on` exists. Queue then
    // commit separately, either order, and one of two things can happen: a merge queued against an
    // approval that rolls back — an irreversible publication nobody authorised, with the proposal
    // still pending so it can be authorised again — or a run resumed and told its merge is queued
    // when it is not.
    //
    // `Origin::Human` rather than `Origin::Run`, and the difference is not bookkeeping. A human just
    // approved this, so it carries their authority and starts `queued` rather than waiting for an
    // approval it already has. Tying it to the run instead would tie it to a row this very
    // transaction is about to mark `superseded`, and the merge must outlive the run that asked for
    // it — that is the point of handing it to a queue.
    let queued_request_id = match &queueable {
        Some((repo, op)) => {
            Some(crate::vcs::submit_on(&mut *tx, repo, op, crate::vcs::Origin::Human).await?)
        }
        None => None,
    };

    // The run is told which of the two happened, because the two ask opposite things of it. Named by
    // `kind()` rather than by the word "merge", which is what it said while merge was the only thing
    // the queue could do: a run told its *merge* was queued after asking for a push would read that
    // as the daemon having misunderstood it, and go looking for what it had misfiled.
    let note = match (queued_request_id, &queueable) {
        (Some(request_id), Some((_, op))) => {
            let kind = op.kind();
            format!(
                "A previously paused autonomous run has been resumed after approval of proposal #{proposal_id}. The {tool_name} action you attempted is NOT authorized for you to perform: the {kind} it asked for has been handed to the daemon's git queue as request #{request_id}, which serialises every git operation on this repository and will carry it out for you. Do not attempt it again. Continue with the rest of the task."
            )
        }
        // `queued_request_id` is `Some` exactly when `queueable` is, and they are matched together
        // rather than one of them being unwrapped inside the other's arm — so the impossible pairing
        // has to be written out, and what it does is fall back to authorizing. A run told to proceed
        // is the safe half of this decision: the grant is single-use and the action is one a human
        // just approved.
        _ => resume_instruction(proposal_id, &tool_name, proposal.tool_input.as_deref()),
    };

    // The note is what CHANGED; the task is what the run is still for, and the row has to carry
    // both. It resumes the same CLI session, so in the ordinary case the history is right there and
    // the note alone reads fine — but the row's prompt is the only thing that survives the session,
    // and a handoff out of this run reads exactly that. Told only the note, a successor inherits an
    // instruction about a proposal as its entire brief (see `task_to_carry`).
    //
    // Appended under a header rather than merged into the sentence, so `task_to_carry` can take it
    // back out on the next resume instead of nesting.
    let prompt = if carried_task.trim().is_empty() {
        note
    } else {
        format!("{note}\n\n{RESUMED_TASK_HEADER}\n{carried_task}")
    };

    // The resume carries the node's identity forward. Without it the new run belongs to no job, so
    // the chain that has to finalise it cannot see it: the item stays `running` forever and the job
    // sits there until the four-hour ceiling retires it, with the approved work already done.
    // `item_id` travels with the rest of that identity, and it is the second approval that proves
    // it does: left out, the FIRST resume still lands in the right tree — it was resolved from the
    // predecessor's own row — while the successor it writes carries no item, so approving that one
    // resolves to the job's tree instead. One approval looks correct; two do not.
    let result = sqlx::query(
        "INSERT INTO runs
           (project_id, cwd, prompt, status, mode, session_id, created_at, job_id, stage, item_id,
            steerable)
         VALUES (?, ?, ?, 'running', 'worktree', ?, ?, ?, ?, ?, ?)",
    )
    .bind(&wt_project_id)
    .bind(&wt_path)
    .bind(&prompt)
    .bind(&session_id)
    .bind(&now)
    .bind(job_id)
    .bind(stage.as_deref())
    .bind(item_id)
    .bind(steerable)
    .execute(&mut *tx)
    .await?;
    let resume_id = result.last_insert_rowid();
    // The hand-over is a no-op for a job's tree, and must be: the job owns it, and moving ownership
    // to this one node would let the GC collect it the moment that node finished — with the rest of
    // the queue still to run in it. Filtering on `owner_kind = 'run'` is what makes that so.
    sqlx::query("UPDATE worktrees SET owner_id=? WHERE owner_kind='run' AND owner_id=?")
        .bind(resume_id)
        .bind(original_run_id)
        .execute(&mut *tx)
        .await?;
    // The slot follows the tree, and is a no-op for a job's for the same reason — hence the same
    // `owner_kind = 'run'` filter.
    //
    // Handed over rather than claimed anew, and the difference is not style. A claim is per owner:
    // the resume would ask for a SECOND slot while its own predecessor still held the first, so a
    // project at its ceiling would refuse an approval a human had already given — the resume denied
    // a slot by the very run it replaces. Handing it over keeps one piece of work to one slot.
    //
    // Left out entirely, which is what happened until now, the row keeps pointing at the run
    // `superseded` a few lines above. `reconcile_orphaned_slots` frees any slot whose owner is not
    // live and runs on every job tick, so it collects this one out from under a run still working in
    // the tree — and the project, reading one fewer in flight than it has, starts another. Two
    // worktrees in one repository is precisely what `project_slots` exists to prevent.
    sqlx::query("UPDATE project_slots SET owner_id=? WHERE owner_kind='run' AND owner_id=?")
        .bind(resume_id)
        .bind(original_run_id)
        .execute(&mut *tx)
        .await?;
    // The item follows its node. Left pointing at the run just marked `superseded`, the job's next
    // pass would read a terminal node that did not complete and stop the whole chain — turning an
    // approval into a failure, and throwing away the work the user just authorised.
    sqlx::query("UPDATE job_items SET run_id = ? WHERE run_id = ?")
        .bind(resume_id)
        .bind(original_run_id)
        .execute(&mut *tx)
        .await?;
    // The conflict follows its resolver, for the same reason as the three above and in the same
    // shape. Left pointing at the run marked `superseded`, the resolution stops reading as live:
    // `resolver.rs`'s "two never run at once" brake is a join through this column onto a live run,
    // so it stops seeing this one and a second agent can be minted for the same two branches — and
    // anything that cancels a resolution whose conflict has since been settled cannot reach the run
    // actually doing the work.
    //
    // Measured on this repository. Requests 79 and 80 escalated, each got a resolution, and both
    // conflicts were then settled another way and landed as request 85. Both resolutions had been
    // resumed once, so both were linked to rows reading `superseded`; they carried on for another
    // thirteen hours and $3.01 between them before escalating again as 87 and 88, over a conflict
    // that had stopped existing the previous evening.
    sqlx::query("UPDATE vcs_requests SET resolution_run_id = ? WHERE resolution_run_id = ?")
        .bind(resume_id)
        .bind(original_run_id)
        .execute(&mut *tx)
        .await?;
    // One row either way, and it carries both keys because the two columns answer different
    // questions about it.
    //
    // `action_class` is what an authorization is checked against (migration 0055); a NULL there
    // authorizes nothing, so the resume would park again on the action just approved. `tool_input`
    // is the record of the exact spelling the human read (migration 0020), and it is what `hooks.rs`
    // matches a retry against when the action was taken over rather than authorized.
    //
    // `queued_request_id` is which of the two this row is (migration 0054). NULL is an authorization
    // — the run may perform actions of that class for the rest of its life. Set is the opposite
    // fact: the queue has this action, the run does not. `grant_covers_class` excludes those rows,
    // and the class rule is what makes that exclusion load-bearing rather than tidy — a takeover row
    // that covered its class would authorize every later merge the run attempted, off a row minted
    // to say merging had been taken away from it.
    //
    // Written even when nothing is granted because the run has to be ABLE to be told. Without the
    // row, a resumed run that tried its merge again would be paused and would mint a second proposal
    // for a person to read — and approving that one would queue the merge twice.
    sqlx::query(
        "INSERT INTO action_grants (run_id, tool_name, tool_input, action_class, proposal_id, created_at, consumed_at, queued_request_id)
         VALUES (?, ?, ?, ?, ?, ?, NULL, ?)",
    )
    .bind(resume_id)
    .bind(&tool_name)
    .bind(proposal.tool_input.as_deref())
    .bind(action_class)
    .bind(proposal_id)
    .bind(&now)
    .bind(queued_request_id)
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
    // The queued request id belongs in the audit trail, not only in the resumed run's prompt: this
    // row is where somebody reconstructs what an approval actually did, and "approved" alone no
    // longer says whether the action was authorised or taken over.
    let note = match (queued_request_id, &queueable) {
        (Some(request_id), Some((_, op))) => {
            let kind = op.kind();
            format!("approved; {kind} queued as vcs request {request_id}; resume run {resume_id}")
        }
        _ => format!("approved; resume run {resume_id}"),
    };
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
        // A resumed job node keeps its handoff directory: it is the same node, in the same tree,
        // finishing the same item, and the review node after it still reads `plan.json` from there.
        // An ordinary resumed run gets nothing, as before.
        run_env(
            &daemon_token,
            resume_id,
            job_id
                .map(|_| std::path::PathBuf::from(&wt_path).join(crate::worktree::ARTIFACTS_DIR))
                .as_deref(),
        ),
        // Inherited from the run being resumed, like the handoff successor's. The row above carries
        // the same value, so the launch listens exactly when `post_run_message` will admit a turn.
        steerable != 0,
        // A resume is a worktree run, so it gets the worktree clock — the same one the run it
        // continues was given.
        run_timeout_for_mode(state.run_timeout, "worktree"),
        // And the worktree silence deadline, for the same reason: a resume goes straight back into
        // the work its predecessor was doing, which on this workspace means compiling.
        progress_timeout_for_mode(state.progress_timeout, "worktree"),
        // Asked again against the worktree being resumed rather than inherited, because it is a
        // fresh launch into a tree that has since been worked in: the run it continues may have
        // rewritten the very settings file this reads. Re-checking is the conservative direction —
        // a tree that no longer wires the hook stops getting the classifier's surface.
        classifier_governs_tools(
            "worktree",
            crate::runner::ToolPolicy::Unrestricted,
            Some(std::path::Path::new(&wt_path)),
        ),
        // The resume row carries the node's stage forward, so an approved plan node resumes on the
        // plan model rather than dropping back to the runner's own.
        state.runner.model_for_stage(stage.as_deref()),
    );

    Ok(resume_id)
}

/// Where a tail read should resume from. Absent is the start.
#[derive(serde::Deserialize)]
pub struct TailQuery {
    pub since: Option<usize>,
}

/// What a run has written since `since`, while it is still writing.
#[derive(serde::Serialize)]
pub struct TailResponse {
    pub text: String,
    /// The offset to send back next time, in bytes. Handed over rather than left to the client to
    /// compute: it is `since + text.len()`, and a client that measured the string in characters
    /// instead would drift on the first non-ASCII byte and then re-send text it already had.
    pub next: usize,
    /// Always `true` on a 200 — there is no live tail without a live run. Present so the shape does
    /// not change if a recorded fallback is ever served through this same route, and so the screen
    /// has something to bind its "ao vivo" label to rather than inferring it from the status code.
    pub live: bool,
}

/// Serves the live tail, and answers 204 when there is not one.
///
/// **204, never 404.** `404` claims the run does not exist, which is usually false and always
/// misleading here: the ordinary reason for no tail is a run that finished, or one a previous
/// daemon started, and both of those have their whole output in `runs.stdout` and `run_events`. A
/// client told the id is wrong stops asking; a client told there is no content knows to read the
/// recorded copy instead.
///
/// No database work on this path at all, which is what lets a screen poll it every three seconds
/// per visible run without the tick paying for it.
pub async fn get_run_tail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(query): Query<TailQuery>,
) -> Result<Json<TailResponse>, StatusCode> {
    let since = query.since.unwrap_or(0);
    let text = read_tail(&state.run_tails, id, since).ok_or(StatusCode::NO_CONTENT)?;
    Ok(Json(TailResponse {
        next: since + text.len(),
        text,
        live: true,
    }))
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

/// `?leading=` on `GET /runs/{id}/stop`: how many decisions accompany the last one. Absent is
/// [`STOP_LEADING_DEFAULT`]; anything above [`STOP_LEADING_MAX`] is clamped rather than refused
/// (spec §4).
#[derive(serde::Deserialize)]
pub struct StopQuery {
    pub leading: Option<i64>,
}

const STOP_LEADING_DEFAULT: i64 = 10;
const STOP_LEADING_MAX: i64 = 100;

/// The `runs` columns `GET /runs/{id}/stop` needs, read in one query the way `get_run` reads its
/// own wider set. Not `RunStatusResponse`: that struct answers a different route's shape, and
/// borrowing it here would make an unrelated route's column list load-bearing for this one's.
#[derive(sqlx::FromRow)]
struct RunStopRow {
    status: String,
    mode: String,
    exit_code: Option<i64>,
    stderr: Option<String>,
    successor_run_id: Option<i64>,
    created_at: String,
    completed_at: Option<String>,
}

/// One `shadow_decisions` row, read for `GET /runs/{id}/stop` only — the same seven columns spec
/// §5.2 lists, in the shape `sqlx` fills directly rather than a positional tuple.
#[derive(sqlx::FromRow)]
struct StopDecisionRow {
    tool_name: String,
    action_class: String,
    decision: String,
    reason: Option<String>,
    classifier_version: i64,
    policy_digest: Option<String>,
    tool_input: Option<String>,
    created_at: String,
}

impl StopDecisionRow {
    fn into_view(self) -> crate::run_stop::GateDecisionView {
        crate::run_stop::gate_decision_view(
            self.tool_name,
            self.action_class,
            self.decision,
            self.reason,
            self.classifier_version,
            self.policy_digest,
            self.tool_input,
            self.created_at,
        )
    }
}

/// Seconds between `created_at` and `completed_at` — or, for a run still running, between
/// `created_at` and now. Always a majorant of how long the run actually ran (spec §6): the
/// interval includes any time the run spent queued before it started, which `runs` does not
/// record separately.
///
/// `None` only if `created_at` itself fails to parse, which every writer of that column in this
/// codebase writes as `chrono::Utc::now().to_rfc3339()` — so in practice this is infallible.
fn elapsed_seconds_since_created(created_at: &str, completed_at: Option<&str>) -> Option<i64> {
    let parse = |value: &str| {
        chrono::DateTime::parse_from_rfc3339(value).map(|when| when.with_timezone(&chrono::Utc))
    };
    let created = parse(created_at).ok()?;
    let end = match completed_at {
        Some(value) => parse(value).ok()?,
        None => chrono::Utc::now(),
    };
    Some((end - created).num_seconds())
}

/// `GET /runs/{id}/stop` (spec `.ai/specs/2026-08-29-porque-parou-design.md` §4): why a run
/// stopped, in one call. `404` if the run does not exist; `200` — never `204` — for every run that
/// does, including one still `running` (spec §4, §11 item 5).
///
/// Thin by design (spec §7): this reads the run row and, only for `gate` and `timeout` kinds, the
/// `shadow_decisions` leading up to it, and hands both to `run_stop::build_response` to become the
/// answer. Deriving `kind` here first — rather than inside `build_response` — is what lets this
/// skip the `shadow_decisions` query entirely for the other six kinds.
pub async fn get_run_stop(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(query): Query<StopQuery>,
) -> Result<Json<crate::run_stop::RunStopResponse>, StatusCode> {
    let row = sqlx::query_as::<_, RunStopRow>(
        "SELECT status, mode, exit_code, stderr, successor_run_id, created_at, completed_at
         FROM runs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let kind =
        crate::run_stop::derive_kind(&row.status).ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let decisions = if crate::run_stop::kind_shows_leading_up(kind) {
        let leading = query
            .leading
            .unwrap_or(STOP_LEADING_DEFAULT)
            .clamp(0, STOP_LEADING_MAX);
        // `kind: gate` fetches one extra row: the newest fills `gate`, and the `leading` after it
        // fill `leading_up`. `kind: timeout` never fills `gate` (spec §5.1), so it fetches exactly
        // `leading` rows, all of which become `leading_up` — otherwise `leading_up` would end up one
        // row longer than `?leading=` promised.
        let limit = if crate::run_stop::kind_shows_gate(kind) {
            leading + 1
        } else {
            leading
        };
        sqlx::query_as::<_, StopDecisionRow>(
            "SELECT tool_name, action_class, decision, reason, classifier_version, policy_digest,
                    tool_input, created_at
             FROM shadow_decisions WHERE run_id = ?
             ORDER BY id DESC LIMIT ?",
        )
        .bind(id)
        .bind(limit)
        .fetch_all(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .map(StopDecisionRow::into_view)
        .collect()
    } else {
        Vec::new()
    };

    let elapsed_seconds =
        elapsed_seconds_since_created(&row.created_at, row.completed_at.as_deref())
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(crate::run_stop::build_response(
        id,
        row.status,
        kind,
        &row.mode,
        elapsed_seconds,
        row.exit_code,
        row.stderr,
        row.successor_run_id,
        decisions,
    )))
}

/// PURE: whether a run status means the run is over for good.
///
/// `finalize_termination` is called with three distinct statuses by five callers, and one of them —
/// `hooks.rs`'s `pause_for_approval` — passes `awaiting_approval` for a run that is *pausing so a
/// human can approve something*. That run resumes. Treating it as ended would cancel the merge it
/// asked for, and the human would then approve a request that no longer exists.
///
/// `interrupted` and `timed_out` never arrive through `finalize_termination` — both are written by
/// direct UPDATEs — so listing them here is about the predicate being true rather than about that
/// call site: `timed_out` has its own call in `spawn_run`'s wall-clock arm, and `interrupted` is
/// inert today and would be obviously right if startup recovery ever routed through here.
fn ends_the_run(status: &str) -> bool {
    ENDED_RUN_STATUSES.contains(&status)
}

/// The run statuses that mean the run is over for good — it will not resume, and anything it asked
/// for and never started should go with it.
///
/// A constant rather than a literal inside `ends_the_run`, because `vcs.rs` builds a SQL `IN` clause
/// from this same list and a second copy would drift silently: the failure would be a queue that
/// stops reaping, or one that reaps a run that was only paused, and neither announces itself.
///
/// `awaiting_approval` is deliberately absent, and it is the whole reason this is a list rather than
/// `status != 'running'`: a run pausing for a human resumes, and its merge must survive the pause.
pub const ENDED_RUN_STATUSES: &[&str] = &["cancelled", "failed", "interrupted", "timed_out"];

/// Every status a run can come to rest in — which is a wider question than the one above.
///
/// [`ENDED_RUN_STATUSES`] asks "did this run stop without finishing, so should its queued work be
/// thrown away?", and deliberately excludes both `completed` (whose queued merge is the point) and
/// `superseded` (whose work continues in the successor). This one asks "is this run still using its
/// worktree?", and the answer for all six is no.
///
/// It exists because something else has to agree with it: `worktree::gc_candidates` collects a
/// run's worktree only for a status it lists, so an ending missing from there leaks a directory
/// forever — invisibly, because as far as the system is concerned that run is over and its tree is
/// nobody's. `every_ending_a_run_can_have_is_an_ending_the_gc_collects` holds the two lists
/// together. The job side has had that guard since `stopped` opened exactly this leak
/// (`job::TERMINAL_STATUSES`); the run side had the same shape and no guard, and `superseded` —
/// added later, by the approval resume — was already missing from the GC when this was written.
pub const TERMINAL_RUN_STATUSES: &[&str] = &[
    "completed",
    "failed",
    "cancelled",
    "interrupted",
    "timed_out",
    "superseded",
];

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
            // The run is over; anything it queued and never started goes with it (spec §7). After
            // the status write, because this is a consequence of the run ending — and best-effort,
            // because the run IS terminated either way and a queue row that outlives its run is a
            // stale request a human can cancel, not a broken run.
            if ends_the_run(status) {
                if let Err(error) = crate::vcs::cancel_for_run(&state.pool, id).await {
                    tracing::warn!(run_id = id, %error, "could not cancel the run's queued vcs requests");
                }
                // Every status that ends a run also ends its chance to report a cost: the abort
                // above dropped the future, so `cancelled`, `failed`, `interrupted` and `timed_out`
                // all leave the same silent `$0`. Sharing `ends_the_run` is what keeps
                // `awaiting_approval` out — that run resumes, and the resume carries the real cost
                // for the whole session; approximating the pause would bill the same time twice.
                record_time_approx_cost(&state.pool, id).await;
            }
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
/// the concurrency sweep spares `awaiting_approval`, so one strand holds a slot for good, and the
/// worktree GC only collects terminal runs.
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

/// How long a finished run keeps the bulk it produced.
///
/// A run's transcript is stored twice — the whole stream as `runs.stdout`, and again line by line
/// in `run_events`, which migration 0035 then indexes for search. Nothing removed any of it, so the
/// three copies grew for as long as the daemon was ever used. Measured on one lightly-used install:
/// 4.9 MB of `stdout` and 4.1 MB of `run_events` from 87 runs, one transcript alone a megabyte.
///
/// The row itself is NOT deleted, and that is the whole design. What costs is the transcript; what
/// people look at months later is the metadata — what ran, when, whether it passed, what it cost —
/// and that is a couple of hundred bytes a run. So a finished run keeps its history forever and
/// loses its transcript on a window, rather than the row disappearing out from under a feed entry,
/// a job item or a proposal that still points at it.
pub const DEFAULT_TRANSCRIPT_RETENTION_DAYS: i64 = 30;

/// The window, overridable for an operator who wants a different one.
///
/// An environment variable rather than a config file, matching `NUCLEOS_WORKTREE_RETENTION_HOURS`
/// in `worktree.rs`: runs are not a pillar, and a knob nobody has yet asked to turn does not earn
/// a `.ai/*.yaml` of its own.
fn transcript_retention_days() -> i64 {
    std::env::var("NUCLEOS_TRANSCRIPT_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_TRANSCRIPT_RETENTION_DAYS)
}

/// What one retention pass removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PrunedTranscripts {
    /// Runs whose transcript columns were emptied.
    pub runs: u64,
    /// Rows removed from `run_events` — and, through migration 0035's delete trigger, the terms
    /// they had put in the search index.
    pub events: u64,
}

/// Empties the transcript of every finished run past the window, leaving the row and its metadata.
///
/// Only terminal runs, read from [`TERMINAL_RUN_STATUSES`] rather than spelled here: a run still
/// `running` or paused at `awaiting_approval` is going to write more, and stripping it mid-flight
/// would delete a transcript while its author still holds the file. The `IN` clause's placeholders
/// are generated from the constant's length and every status bound, for the reason
/// `vcs::reap_requests_of_ended_runs` gives at length: a status is data.
pub async fn prune_transcripts(
    pool: &sqlx::SqlitePool,
    retain_days: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<PrunedTranscripts> {
    if retain_days <= 0 {
        // Zero would empty every transcript on the machine at the next sweep, which is not a
        // retention policy but a typo with a plausible-looking value. Refusing to act is the safe
        // reading — the same call `web::prune` makes.
        return Ok(PrunedTranscripts::default());
    }

    // Computed here rather than with SQLite's `datetime('now', '-N days')`, and the difference is
    // not stylistic — see `web::prune`, which pays for this lesson in full. `completed_at` is
    // RFC 3339 (`2026-08-01T12:00:00+00:00`), `datetime()` returns `2026-08-01 12:00:00`, and they
    // are compared as TEXT: within the cutoff's own day `T` (0x54) sorts after the space (0x20), so
    // a row hours too old compares as newer and survives every sweep for ever.
    // `transcript_retention_is_exact_at_the_boundary` is what fails if this goes back to
    // `datetime()`; the coarse test beside it does not.
    let cutoff = (now - chrono::Duration::days(retain_days)).to_rfc3339();
    let placeholders = vec!["?"; TERMINAL_RUN_STATUSES.len()].join(", ");
    // `COALESCE(completed_at, created_at)` because a terminal row with no completion stamp is a row
    // some older path left half-written; ageing it from when it was created is what keeps it from
    // being immortal.
    let past_the_window =
        format!("status IN ({placeholders}) AND COALESCE(completed_at, created_at) < ?");

    // `AssertSqlSafe` because sqlx otherwise takes only `&'static str`. The sole interpolated thing
    // is a row of `?` generated from a constant's length — every status and the cutoff are bound —
    // so nothing caller-supplied reaches the SQL text (same justification as
    // `vcs::reap_requests_of_ended_runs`).
    //
    // Events first. Between the two statements the row still says it holds a transcript, so a crash
    // in the gap leaves work to redo rather than a run that claims to have events it no longer has.
    let mut delete = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM run_events
          WHERE run_id IN (SELECT id FROM runs WHERE {past_the_window})"
    )));
    for status in TERMINAL_RUN_STATUSES {
        delete = delete.bind(status);
    }
    let events = delete.bind(&cutoff).execute(pool).await?.rows_affected();

    // The `IS NOT NULL` guard is what makes the count mean something: without it every sweep
    // rewrites every old row for ever and reports them all as freshly pruned.
    // Assistant turns are exempt, and only from THIS half. A turn is a run whose `stdout` holds the
    // reply rather than a stream, and the shell rebuilds a conversation out of those replies
    // (`GET /assistant/{chat}` selects `stdout AS answer`) — so emptying the column would give the
    // app a chat history that erases itself a month at a time. Their events are pruned above with
    // everyone else's, which is where their bulk actually is.
    let mut update = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE runs SET stdout = NULL, stderr = NULL, gate_output = NULL
          WHERE {past_the_window}
            AND (mode IS NULL OR mode <> 'assistant')
            AND (stdout IS NOT NULL OR stderr IS NOT NULL OR gate_output IS NOT NULL)"
    )));
    for status in TERMINAL_RUN_STATUSES {
        update = update.bind(status);
    }
    let runs = update.bind(&cutoff).execute(pool).await?.rows_affected();

    Ok(PrunedTranscripts { runs, events })
}

/// How often retention runs while the daemon is up.
///
/// Hourly rather than tied to a read, for the reason `main.rs` gives the web cache: a transcript
/// nobody opens again must still expire, or "30 days" means "30 days after the last time anyone
/// looked". Once at startup too, because a daemon that only ever runs for an hour at a time would
/// otherwise never reach a sweep at all — the lesson `triage::run_triage_loop` already learned.
const RETENTION_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Retention for everything finished work leaves behind: a run's transcript, its events, its
/// entries in the activity feed, and the councils that are over.
///
/// One loop rather than four, because they are the same sweep at different windows and splitting
/// them would mean four tasks waking on the same hour to take the same write lock. Every failure
/// is best-effort and logged: a sweep that could not run is a fuller disk later, not a reason to
/// take a daemon down now.
pub async fn run_retention_loop(state: AppState) {
    let mut ticker = tokio::time::interval(RETENTION_INTERVAL);
    loop {
        ticker.tick().await;
        let now = chrono::Utc::now();
        match prune_transcripts(&state.pool, transcript_retention_days(), now).await {
            Ok(pruned) if pruned == PrunedTranscripts::default() => {}
            Ok(pruned) => tracing::info!(
                runs = pruned.runs,
                events = pruned.events,
                "runs: transcripts past the retention window"
            ),
            Err(error) => tracing::warn!(%error, "runs: transcript retention sweep failed"),
        }
        match crate::feed::prune(&state.pool, crate::feed::retention_days(), now).await {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(pruned, "feed: entries past the retention window"),
            Err(error) => tracing::warn!(%error, "feed: retention sweep failed"),
        }
        // Unconditional, unlike the pillar's other work: a council is deleted whether or not
        // `.ai/council.yaml` still names a roster. Gating the sweep on the pillar being configured
        // would make a roster somebody removed the way their history stops being collected.
        match crate::council::prune(&state.pool, crate::council::retention_days(), now).await {
            Ok(0) => {}
            Ok(pruned) => {
                tracing::info!(pruned, "council: deliberations past the retention window")
            }
            Err(error) => tracing::warn!(%error, "council: retention sweep failed"),
        }
        // The fourth window, and the one that empties rather than deletes: a queued operation's row
        // is what `action_grants.queued_request_id` points at, so only what git printed goes.
        match crate::vcs::prune_output_tails(&state.pool, crate::vcs::output_retention_days(), now)
            .await
        {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(pruned, "vcs: outputs past the retention window"),
            Err(error) => tracing::warn!(%error, "vcs: retention sweep failed"),
        }
    }
}

#[cfg(test)]
mod run_env_tests {
    use super::*;

    /// The address a run's tools call back on is the address this daemon actually binds.
    ///
    /// **Found by running it, not by a test.** This was a literal `http://127.0.0.1:8791` while the
    /// port was one too, and stayed a literal after the port stopped being one. A second daemon on
    /// 8890 therefore launched its CLI turns with their tools pointed at the daemon on 8791 — a
    /// different process, on a different database, running a different build. The symptom was
    /// `send_to_chat` answering 404: the route the model called does exist, on the daemon that
    /// should have received the call, and did not on the one that did.
    ///
    /// Asserted against `daemon_client::daemon_url()` rather than a written-out string, which is
    /// the whole point. A second copy of this address is a second thing to remember to change, and
    /// it will be forgotten in the direction that fails silently.
    #[test]
    fn a_run_calls_back_on_the_port_this_daemon_binds() {
        let env = run_env("tok", 7, None);
        let url = env
            .iter()
            .find(|(key, _)| key == "NUCLEOS_DAEMON_URL")
            .map(|(_, value)| value.as_str())
            .expect("every run is told where to call back");

        assert_eq!(url, crate::daemon_client::daemon_url());
        assert_eq!(
            url,
            crate::daemon_client::url_for(crate::daemon_client::port())
        );
    }
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

    /// A run that ends in a status the GC does not collect keeps its worktree forever, and nothing
    /// reports it: the run is over, so nothing is waiting on the tree and nobody goes looking.
    ///
    /// The twin of `job::every_ending_a_job_can_have_is_an_ending_the_gc_collects`, which the job
    /// arm has had since `stopped` opened precisely this leak. The run arm had no such guard, and
    /// `superseded` — written by the approval resume, added long after the GC's list — was missing
    /// from it. That it was not yet leaking was luck rather than design: the same transaction hands
    /// the worktree to the successor, so today no row is left pointing at the superseded run. This
    /// test is what makes the next status a design decision instead of an accident.
    #[test]
    fn every_ending_a_run_can_have_is_an_ending_the_gc_collects() {
        for status in TERMINAL_RUN_STATUSES {
            assert!(
                crate::worktree::GC_CANDIDATES_SQL.contains(&format!("'{status}'")),
                "a run can end `{status}` and its worktree would never be collected"
            );
            // Migration 0009's partial index: a status that holds the project's only worktree slot
            // must not also be one the GC collects, or the tree goes while the run still has it.
            assert!(
                !["running", "awaiting_approval"].contains(status),
                "`{status}` both holds the project's slot and is collectable"
            );
        }
    }

    /// The shell draws a run's context pressure as a fraction of this window, and it cannot read a
    /// Rust constant — `CONTEXT_WINDOW_TOKENS` in `shell/src/derive.ts` is a copy of the number
    /// below. `GET /runs/{id}` reports the fill and not the window, so nothing at runtime would
    /// notice the two disagreeing; the bar would simply be drawn against the wrong denominator and
    /// keep looking plausible.
    ///
    /// So this test is the join. Changing the floor is allowed — updating one side only is not, and
    /// this is what says so.
    #[test]
    fn the_handoff_window_matches_the_one_the_shell_mirrors() {
        assert_eq!(
            HANDOFF_CONTEXT_LIMIT_FLOOR, 200_000,
            "update CONTEXT_WINDOW_TOKENS in shell/src/derive.ts to match, then this number here",
        );
    }

    /// A live run's tail is readable from outside its task, and the offset is in BYTES.
    ///
    /// Bytes rather than lines because the last line of a working run has not ended yet: counting
    /// lines would give a cursor that moves backwards every time the line in progress grows, and the
    /// reader would redraw text it already had.
    #[test]
    fn a_live_tail_is_read_from_a_byte_offset() {
        let tails: crate::state::RunTails = Default::default();
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        tails
            .lock()
            .unwrap()
            .insert(7, std::sync::Arc::clone(&buffer));

        buffer.lock().unwrap().push_str("primeira\n");
        assert_eq!(read_tail(&tails, 7, 0).as_deref(), Some("primeira\n"));

        buffer.lock().unwrap().push_str("segunda");
        assert_eq!(
            read_tail(&tails, 7, 9).as_deref(),
            Some("segunda"),
            "the second read repeated what the first had already shown"
        );
    }

    /// No entry is `None`, and `None` is not the empty string.
    ///
    /// The distinction is the whole contract. A run this daemon never started, and a run that has
    /// finished, both have no tail — and neither produced no output. `Some("")` would let a screen
    /// draw an empty transcript over a run that wrote thousands of lines; `None` makes it say where
    /// the durable copy is instead.
    #[test]
    fn a_run_with_no_live_tail_is_absent_rather_than_empty() {
        let tails: crate::state::RunTails = Default::default();
        assert_eq!(read_tail(&tails, 404, 0), None);
    }

    /// An offset past the end is empty, not a panic.
    ///
    /// It happens on every ordinary poll of a run that wrote nothing since the last one, so it is
    /// the common path and not an edge case. It is also what a slicing bug would turn into a crash
    /// in the daemon's HTTP thread.
    #[test]
    fn an_offset_at_or_past_the_end_reads_empty() {
        let tails: crate::state::RunTails = Default::default();
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(String::from("abc")));
        tails.lock().unwrap().insert(1, buffer);

        assert_eq!(read_tail(&tails, 1, 3).as_deref(), Some(""));
        assert_eq!(read_tail(&tails, 1, 99).as_deref(), Some(""));
    }

    /// The guard that drops a run's abort handle drops its tail with it.
    ///
    /// Registered in one place and released in another is how a map leaks: every run the daemon has
    /// ever executed would keep its whole transcript in memory until restart. `Registration` already
    /// owns that lifetime for the other two maps, and this is what keeps the third beside them.
    #[test]
    fn ending_a_run_takes_its_tail_with_the_rest_of_its_registration() {
        let handles: crate::state::RunHandles = Default::default();
        let messages: crate::state::RunMessages = Default::default();
        let tails: crate::state::RunTails = Default::default();
        tails.lock().unwrap().insert(9, Default::default());

        drop(Registration {
            handles: handles.clone(),
            messages: messages.clone(),
            tails: tails.clone(),
            id: 9,
        });

        assert!(tails.lock().unwrap().is_empty(), "the tail outlived its run");
    }

    async fn retention_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    /// Inserts a run with a transcript in both places it is stored, plus one searchable event.
    async fn run_with_a_transcript(
        pool: &sqlx::SqlitePool,
        status: &str,
        completed_at: &str,
    ) -> i64 {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO runs (project_id, cwd, prompt, status, stdout, stderr, gate_output,
                               cost_usd, created_at, completed_at)
             VALUES ('proj', '/tmp', 'ask', ?, 'a very long transcript', 'noise', 'gate said no',
                     0.5, '2026-01-01T00:00:00+00:00', ?)
             RETURNING id",
        )
        .bind(status)
        .bind(completed_at)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO run_events (run_id, seq, kind, payload, created_at)
             VALUES (?, 0, 'assistant', 'aardvark', ?)",
        )
        .bind(id)
        .bind(completed_at)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn transcript_of(pool: &sqlx::SqlitePool, id: i64) -> (Option<String>, i64) {
        let stdout: Option<String> = sqlx::query_scalar("SELECT stdout FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM run_events WHERE run_id = ?")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap();
        (stdout, events)
    }

    /// The whole point: both copies of a finished run's transcript go once it is past the window.
    #[tokio::test]
    async fn a_finished_runs_transcript_goes_once_it_is_past_the_window() {
        let pool = retention_pool().await;
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-08T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let old = run_with_a_transcript(&pool, "completed", "2026-06-01T00:00:00+00:00").await;
        let recent = run_with_a_transcript(&pool, "completed", "2026-08-07T00:00:00+00:00").await;

        let pruned = prune_transcripts(&pool, 30, now).await.unwrap();

        assert_eq!(pruned, PrunedTranscripts { runs: 1, events: 1 });
        assert_eq!(transcript_of(&pool, old).await, (None, 0));
        let (stdout, events) = transcript_of(&pool, recent).await;
        assert!(stdout.is_some(), "a run inside the window keeps its transcript");
        assert_eq!(events, 1, "and keeps its events");
    }

    /// An assistant turn IS a run, and its `stdout` is not a transcript — it is the reply, and it is
    /// what the shell rebuilds the conversation from (`GET /assistant/{chat}` reads
    /// `stdout AS answer`). Emptying it on a window would give the app a chat history that erases
    /// itself a month at a time, which is a far worse bargain than the bytes it saves: a reply is a
    /// sentence, the transcript it would have cost is in `run_events` and goes anyway.
    #[tokio::test]
    async fn an_assistant_turn_keeps_its_reply_and_still_gives_up_its_events() {
        let pool = retention_pool().await;
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-08T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let id = run_with_a_transcript(&pool, "completed", "2026-01-01T00:00:00+00:00").await;
        sqlx::query("UPDATE runs SET mode = 'assistant', chat_id = 'chat-1' WHERE id = ?")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();

        let pruned = prune_transcripts(&pool, 30, now).await.unwrap();

        let (stdout, events) = transcript_of(&pool, id).await;
        assert!(
            stdout.is_some(),
            "the reply the conversation is made of must survive the sweep"
        );
        assert_eq!(events, 0, "its event stream is still bulk, and still goes");
        assert_eq!(pruned, PrunedTranscripts { runs: 0, events: 1 });
    }

    /// A run that has not finished is still writing, whatever its row's dates say.
    #[tokio::test]
    async fn a_run_that_has_not_finished_keeps_its_transcript_however_old() {
        let pool = retention_pool().await;
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-08T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let running = run_with_a_transcript(&pool, "running", "2020-01-01T00:00:00+00:00").await;
        let paused =
            run_with_a_transcript(&pool, "awaiting_approval", "2020-01-01T00:00:00+00:00").await;

        let pruned = prune_transcripts(&pool, 30, now).await.unwrap();

        assert_eq!(pruned, PrunedTranscripts::default());
        assert!(transcript_of(&pool, running).await.0.is_some());
        assert!(transcript_of(&pool, paused).await.0.is_some());
    }

    /// The metadata is what survives, and it is the reason the row is not deleted.
    #[tokio::test]
    async fn pruning_a_transcript_keeps_the_run_and_what_it_cost() {
        let pool = retention_pool().await;
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-08T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let id = run_with_a_transcript(&pool, "completed", "2026-01-01T00:00:00+00:00").await;

        prune_transcripts(&pool, 30, now).await.unwrap();

        let (status, cost): (String, Option<f64>) =
            sqlx::query_as("SELECT status, cost_usd FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "completed");
        assert_eq!(cost, Some(0.5), "what the run cost outlives what it said");
    }

    /// The search index is external-content, so a deleted event that stayed in the index would
    /// return a rowid pointing at nothing — migration 0035's delete trigger is what stops that, and
    /// this is what notices if it ever goes.
    #[tokio::test]
    async fn the_search_index_forgets_a_pruned_transcript() {
        let pool = retention_pool().await;
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-08T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        run_with_a_transcript(&pool, "completed", "2026-01-01T00:00:00+00:00").await;
        let before: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM run_events_fts WHERE run_events_fts MATCH 'aardvark'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(before, 1, "the event has to be findable before it is pruned");

        prune_transcripts(&pool, 30, now).await.unwrap();

        let after: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM run_events_fts WHERE run_events_fts MATCH 'aardvark'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(after, 0, "a pruned event must leave the index with it");
    }

    /// Copied from `web::retention_is_exact_at_the_boundary`, and for its reason: `completed_at` is
    /// RFC 3339 and SQLite's `datetime()` is not, and the two are compared as TEXT. Within the
    /// cutoff's own day `T` sorts after the space, so a `datetime()` cutoff spares a day's worth of
    /// rows on every sweep, forever. The coarse test above would not notice.
    #[tokio::test]
    async fn transcript_retention_is_exact_at_the_boundary() {
        let pool = retention_pool().await;
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-08T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        // One second the wrong side of a 30-day cutoff, and one second the right side.
        let past = run_with_a_transcript(&pool, "completed", "2026-07-09T11:59:59+00:00").await;
        let inside = run_with_a_transcript(&pool, "completed", "2026-07-09T12:00:01+00:00").await;

        prune_transcripts(&pool, 30, now).await.unwrap();

        assert_eq!(transcript_of(&pool, past).await.0, None);
        assert!(transcript_of(&pool, inside).await.0.is_some());
    }

    /// Zero is not a retention policy, it is a typo that empties every transcript on the machine.
    #[tokio::test]
    async fn a_zero_or_negative_window_prunes_nothing() {
        let pool = retention_pool().await;
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-08T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let id = run_with_a_transcript(&pool, "completed", "2020-01-01T00:00:00+00:00").await;

        assert_eq!(
            prune_transcripts(&pool, 0, now).await.unwrap(),
            PrunedTranscripts::default()
        );
        assert_eq!(
            prune_transcripts(&pool, -1, now).await.unwrap(),
            PrunedTranscripts::default()
        );
        assert!(transcript_of(&pool, id).await.0.is_some());
    }

    /// A second pass over the same rows must report nothing, or the log says work is happening
    /// every hour for ever and the counter stops meaning anything.
    #[tokio::test]
    async fn a_second_pass_over_already_pruned_runs_reports_nothing() {
        let pool = retention_pool().await;
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-08T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        run_with_a_transcript(&pool, "completed", "2026-01-01T00:00:00+00:00").await;

        assert_eq!(
            prune_transcripts(&pool, 30, now).await.unwrap(),
            PrunedTranscripts { runs: 1, events: 1 }
        );
        assert_eq!(
            prune_transcripts(&pool, 30, now).await.unwrap(),
            PrunedTranscripts::default()
        );
    }

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
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
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
            telegram_doctrine: None,
            runner: runner.clone(),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_tails: Default::default(),
            files_root: None,
            workflow_library: None,
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
github: std::sync::Arc::new(crate::github::GithubRuntime::default()),
web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
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

    /// The same, standing on `master`, with `feat/x` having changed the same line — so the merge a
    /// resolution is started for is a merge that really does conflict.
    fn init_conflicted_repo(prefix: &str) -> (tempfile::TempDir, PathBuf) {
        let (container, repo) = init_contained_repo(prefix);
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("checkout"),
                OsStr::new("-b"),
                OsStr::new("feat/x")
            ]
        ));
        std::fs::write(repo.join("seed.txt"), "theirs\n").expect("write their side");
        assert!(git_ok(
            &repo,
            &[OsStr::new("commit"), OsStr::new("-am"), OsStr::new("theirs")]
        ));
        assert!(git_ok(
            &repo,
            &[OsStr::new("checkout"), OsStr::new("master")]
        ));
        std::fs::write(repo.join("seed.txt"), "ours\n").expect("write our side");
        assert!(git_ok(
            &repo,
            &[OsStr::new("commit"), OsStr::new("-am"), OsStr::new("ours")]
        ));
        (container, repo)
    }

    /// A merge the queue escalated, admitted through the real INSERT and then moved to the status the
    /// executor would have written. Hand-writing the row would let the stored operation drift from
    /// what `Op` actually serialises, which is the one thing the launcher parses back.
    async fn escalated_merge(pool: &sqlx::SqlitePool, root: &FsPath) -> i64 {
        let repo = crate::vcs::ResolvedRepo::synthetic("proj", &root.to_string_lossy(), "proj");
        let id = crate::vcs::submit(
            pool,
            &repo,
            &crate::vcs::Op::Merge {
                source: "feat/x".into(),
                target: "master".into(),
            },
            crate::vcs::Origin::Shell,
        )
        .await
        .expect("admit the merge");
        sqlx::query("UPDATE vcs_requests SET status = 'escalated' WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await
            .expect("escalate it");
        id
    }

    async fn resolution_run_id_of(pool: &sqlx::SqlitePool, request: i64) -> Option<i64> {
        sqlx::query_scalar::<_, Option<i64>>(
            "SELECT resolution_run_id FROM vcs_requests WHERE id = ?",
        )
        .bind(request)
        .fetch_one(pool)
        .await
        .expect("read the request back")
    }

    /// The launcher's whole contribution, end to end: the tree is born on the TARGET, the conflict is
    /// already staged in it when the agent arrives, and the escalation records that it has had its
    /// attempt.
    ///
    /// The agent is given a conflicted worktree rather than two branches and an instruction to merge,
    /// and that inversion is the design. Any `git merge` a session runs goes to the queue — the same
    /// queue that refused this merge for conflicting — so an agent told to merge would circle for
    /// ever against a refusal that is structural and correct.
    #[tokio::test(flavor = "current_thread")]
    async fn a_resolution_starts_on_the_target_with_the_conflict_already_in_front_of_it() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_container, repo) = init_conflicted_repo("nucleos-runs-resolve-");
        // A runner that takes its time, so the assertions below read a worktree the run has not
        // finished with yet.
        let state = test_state_with(Some(Duration::from_secs(30)), Duration::from_secs(120)).await;
        let request = escalated_merge(&state.pool, &repo).await;

        let run = create_resolution_run(
            &state,
            "resolve it".to_owned(),
            "proj".to_owned(),
            repo.to_string_lossy().into_owned(),
            Resolution {
                request_id: request,
                source: "feat/x".to_owned(),
                target: "master".to_owned(),
            },
        )
        .await
        .expect("the resolution should start");

        assert_eq!(
            resolution_run_id_of(&state.pool, request).await,
            Some(run),
            "the escalation has to record which run had its one attempt"
        );

        let worktree: String = sqlx::query_scalar("SELECT cwd FROM runs WHERE id = ?")
            .bind(run)
            .fetch_one(&state.pool)
            .await
            .expect("the run should have been given its worktree");
        let worktree = FsPath::new(&worktree);
        let conflicted =
            std::fs::read_to_string(worktree.join("seed.txt")).expect("read the conflicted file");
        assert!(
            conflicted.contains("<<<<<<<"),
            "the agent has to find the conflict already staged: {conflicted}"
        );
    }

    /// One attempt, never a second. Repeating is where an agent burns budget insisting on the same
    /// wall, and whoever reads an escalation should find one attempt to read rather than seven —
    /// without the claim, a tick every minute would mint a fresh agent per tick against the same two
    /// branches, which does not degrade, it explodes.
    #[tokio::test(flavor = "current_thread")]
    async fn a_conflict_that_already_had_its_attempt_gets_no_second_agent() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_container, repo) = init_conflicted_repo("nucleos-runs-resolve-twice-");
        let state = test_state().await;
        let request = escalated_merge(&state.pool, &repo).await;
        sqlx::query("UPDATE vcs_requests SET resolution_run_id = 4242 WHERE id = ?")
            .bind(request)
            .execute(&state.pool)
            .await
            .expect("record an earlier attempt");

        let refused = create_resolution_run(
            &state,
            "resolve it".to_owned(),
            "proj".to_owned(),
            repo.to_string_lossy().into_owned(),
            Resolution {
                request_id: request,
                source: "feat/x".to_owned(),
                target: "master".to_owned(),
            },
        )
        .await
        .expect_err("a conflict that has been attempted must not be handed out again");

        assert!(
            matches!(refused, CreateRunError::Invalid(_)),
            "losing the claim is a refusal, not a database failure: {refused:?}"
        );
        assert_eq!(
            resolution_run_id_of(&state.pool, request).await,
            Some(4242),
            "the first attempt's record must not be overwritten by the one that lost"
        );
        // Past the INSERT, so a run row exists and has to have been retired — a `running` row with no
        // task holds one of the project's slots until the daemon restarts.
        let status: String = sqlx::query_scalar("SELECT status FROM runs ORDER BY id DESC LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .expect("the run that lost should still be on the table");
        assert_eq!(status, "failed");
    }

    /// A conflict can evaporate between the escalation and the tick that picks it up — other work
    /// lands, and the two branches merge cleanly after all. No agent is started for that, because
    /// there would be nothing in its worktree to resolve; the queue can compute this merge by itself
    /// the next time somebody asks for it.
    ///
    /// The attempt is still spent, and that is the ordering being pinned: the claim is written before
    /// the conflict is staged. The other order loses much worse — a staged conflict whose claim was
    /// then lost leaves real work in a worktree while a second agent edits the same branches.
    #[tokio::test(flavor = "current_thread")]
    async fn a_conflict_that_resolved_itself_spends_its_attempt_without_starting_an_agent() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        // No conflict in this one: `feat/x` never diverges from what `master` says.
        let (_container, repo) = init_contained_repo("nucleos-runs-resolve-clean-");
        assert!(git_ok(
            &repo,
            &[
                OsStr::new("branch"),
                OsStr::new("feat/x"),
                OsStr::new("master")
            ]
        ));
        let state = test_state().await;
        let request = escalated_merge(&state.pool, &repo).await;

        let refused = create_resolution_run(
            &state,
            "resolve it".to_owned(),
            "proj".to_owned(),
            repo.to_string_lossy().into_owned(),
            Resolution {
                request_id: request,
                source: "feat/x".to_owned(),
                target: "master".to_owned(),
            },
        )
        .await
        .expect_err("there is no conflict here to hand anybody");

        assert!(
            matches!(refused, CreateRunError::Worktree(_)),
            "staging is what failed, and the error has to say so: {refused:?}"
        );
        assert!(
            resolution_run_id_of(&state.pool, request).await.is_some(),
            "the attempt is spent even though no agent started — a second try hits the same wall"
        );
    }

    /// Wires this daemon's classifier hook into `dir`, the way a project that has onboarded has it:
    /// registered in `.claude/settings.json` AND executable on disk. Both, because
    /// `autopilot::classifier_hook_is_wired` requires both — a registered command that cannot run
    /// classifies nothing.
    fn wire_classifier_hook(dir: &FsPath) {
        std::fs::create_dir_all(dir.join(".claude/hooks")).expect("create hook directory");
        std::fs::write(
            dir.join(".claude/settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"python \"${CLAUDE_PROJECT_DIR}/.claude/hooks/ask_daemon.py\""}]}]}}"#,
        )
        .expect("write settings");
        std::fs::write(dir.join(".claude/hooks/ask_daemon.py"), "# hook").expect("write hook");
    }

    /// The same project WITHOUT the classifier: settings present and valid, a `PreToolUse` entry
    /// even, but it names somebody else's script. This is the shape the check exists to reject —
    /// "some PreToolUse hook exists" was never the property worth having.
    fn wire_someone_elses_hook(dir: &FsPath) {
        std::fs::create_dir_all(dir.join(".claude/hooks")).expect("create hook directory");
        std::fs::write(
            dir.join(".claude/settings.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"prettier --write"}]}]}}"#,
        )
        .expect("write settings");
        std::fs::write(dir.join(".claude/hooks/ask_daemon.py"), "# hook").expect("write hook");
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

    #[tokio::test(flavor = "current_thread")]
    async fn a_job_node_adopts_the_jobs_worktree_instead_of_provisioning_one() {
        // `WorktreeRootEnv` writes a process-wide variable, so every test that sets it must hold
        // this lock. Without it this test moves the worktree root out from under whichever other
        // test is mid-provision, and the failure surfaces over there instead of here — which is
        // exactly how it was found.
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-jobnode-");
        let (state, _runner) =
            test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        let project_root = repo.to_string_lossy().into_owned();

        let job_id = crate::job::insert_job(
            &state.pool,
            &crate::job::NewJob {
                project_id: "proj",
                project_root: &project_root,
                rule_name: Some("nightly"),
                prompt: "advance the backlog",
                max_items: 5,
                gate_each: true,
                review: true,
                gate_retries: 0,
                head_sha: None,
                max_rounds: None,
                budget_usd: None,
                team_id: None,
            },
        )
        .await
        .expect("start a job");
        let owner = crate::worktree::Owner::Job(job_id);
        let info = crate::worktree::create(&repo, owner)
            .await
            .expect("the job provisions its worktree once");
        crate::worktree::record(
            &state.pool,
            owner,
            "proj",
            &project_root,
            &info.path.to_string_lossy(),
            &info.branch,
            info.base_sha.as_deref(),
        )
        .await
        .expect("record the job worktree");

        let run_id = create_job_node_run(
            &state,
            "the second item".into(),
            "proj".into(),
            project_root.clone(),
            JobNode {
                job_id,
                stage: "implement",
                worktree_path: info.path.to_string_lossy().into_owned(),
                branch: info.branch.clone(),
            },
        )
        .await
        .expect("a node starts inside the job worktree");

        let (cwd, node_job_id, stage): (String, i64, String) =
            sqlx::query_as("SELECT cwd, job_id, stage FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(PathBuf::from(&cwd), info.path);
        assert_eq!(node_job_id, job_id);
        assert_eq!(stage, "implement");

        // The load-bearing half. A node that recorded a worktree of its own would give the same
        // directory two owners, and the GC would then be free to collect it out from under the job
        // the moment this one node reached a terminal status.
        let own_row: Option<i64> = sqlx::query_scalar(
            "SELECT owner_id FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?",
        )
        .bind(run_id)
        .fetch_optional(&state.pool)
        .await
        .unwrap();
        assert!(
            own_row.is_none(),
            "a job node must not provision or record a worktree of its own"
        );
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
            .route("/runs/{id}/stop", get(get_run_stop))
            .route("/runs/{id}/cancel", post(cancel_run))
            .with_state(state)
    }

    /// Seeds one `runs` row outright, which is the only way to reach most of the statuses below.
    ///
    /// `create_run` writes `running` and the driver writes the rest, so a test that went through
    /// the front door could exercise exactly one of the eight kinds. Every case here is about what
    /// a STATUS answers, never about how the run got into it, so the row is the honest fixture.
    ///
    /// `completed_at` is `None` for a run still going, and that is not a detail: it is what makes
    /// `elapsed_seconds_since_created` measure against now instead of against an ending.
    async fn seed_run_row(
        pool: &sqlx::SqlitePool,
        status: &str,
        mode: &str,
        created_at: &str,
        completed_at: Option<&str>,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at, completed_at)
             VALUES ('a run', ?, ?, ?, ?)",
        )
        .bind(status)
        .bind(mode)
        .bind(created_at)
        .bind(completed_at)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// `n` decisions on one run, a second apart so `ORDER BY id DESC` and "most recent first" mean
    /// the same thing and the window tests can name which rows they expect back.
    async fn seed_decisions(pool: &sqlx::SqlitePool, run_id: i64, n: i64) {
        for i in 0..n {
            sqlx::query(
                "INSERT INTO shadow_decisions
                 (run_id, tool_name, decision, action_class, classifier_version, created_at)
                 VALUES (?, 'Bash', 'allow', 'read-local', 11, ?)",
            )
            .bind(run_id)
            .bind(format!("2026-08-30T00:{:02}:{:02}Z", i / 60, i % 60))
            .execute(pool)
            .await
            .unwrap();
        }
    }

    /// The route's answer as it goes over the wire.
    ///
    /// Read as a `Value` rather than deserialised into `RunStopResponse`, deliberately: the struct
    /// is `Serialize` only, and the thing under test is the JSON a shell receives — including which
    /// keys are present as `null` rather than absent, which a typed round trip would hide.
    async fn stop_report(app: &Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
        (status, value)
    }

    /// §11 item 4. `decisions_recorded` is a claim about the MODE, not about an empty table.
    ///
    /// Two runs identical in every other way — same status, same absence of decision rows — so the
    /// only thing that can move the flag is the mode. Asserted in both directions, because a
    /// function that returned `false` unconditionally would satisfy the `real` half on its own and
    /// is exactly the shape this field would rot into.
    ///
    /// `leading_up` is `[]` and not absent for the `real` run: §5.1 puts missing fields at null or
    /// empty rather than omitting them, so the shell never has to tell "this run recorded nothing"
    /// apart from "the daemon stopped sending this key".
    #[tokio::test]
    async fn a_real_mode_run_reports_that_it_recorded_no_decisions_and_a_worktree_one_that_it_did() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let app = test_router(state);
        let started = "2026-08-30T00:00:00Z";
        let ended = "2026-08-30T00:10:00Z";

        let real = seed_run_row(&pool, "timed_out", "real", started, Some(ended)).await;
        let worktree = seed_run_row(&pool, "timed_out", "worktree", started, Some(ended)).await;

        let (status, body) = stop_report(&app, &format!("/runs/{real}/stop")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["decisions_recorded"], serde_json::json!(false));
        assert_eq!(
            body["leading_up"],
            serde_json::json!([]),
            "an empty list, not a missing key"
        );

        let (_, body) = stop_report(&app, &format!("/runs/{worktree}/stop")).await;
        assert_eq!(
            body["decisions_recorded"],
            serde_json::json!(true),
            "the same run in a mode that IS governed by the tool gate"
        );
    }

    /// §11 item 5. A run still going is answered, not deferred.
    ///
    /// `200` and never `204`: the caller asked why this run stopped and the answer — "it has not" —
    /// is a real one. A no-content reply would read as "the daemon has nothing on this run", which
    /// is what `404` already means for a run that does not exist.
    ///
    /// The two kind-specific payloads are asserted null rather than left unchecked, because a
    /// `timeout` block on a live run would be a verdict about a deadline nothing has reached.
    #[tokio::test]
    async fn a_run_still_going_is_answered_two_hundred_with_the_running_kind() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let app = test_router(state);
        let id = seed_run_row(&pool, "running", "worktree", "2026-08-30T00:00:00Z", None).await;

        let (status, body) = stop_report(&app, &format!("/runs/{id}/stop")).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["kind"], serde_json::json!("running"));
        assert_eq!(body["status"], serde_json::json!("running"));
        assert!(
            body["summary"].as_str().is_some_and(|s| !s.is_empty()),
            "§5 says the sentence is always present, live runs included"
        );
        assert_eq!(body["timeout"], serde_json::Value::Null);
        assert_eq!(body["leading_up"], serde_json::Value::Null);
    }

    /// §11 item 6. A run nobody created is `404`, and is told apart from one that recorded nothing.
    #[tokio::test]
    async fn a_run_that_was_never_created_is_four_oh_four() {
        let state = test_state().await;
        let app = test_router(state);

        let (status, _) = stop_report(&app, "/runs/424242/stop").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// §11 item 7. The window has a default, and an over-large one is CLAMPED rather than refused.
    ///
    /// Refusing would be the easier thing to write and the wrong behaviour: the caller asking for
    /// five hundred wants as many as they can have, and a `400` teaches them to guess the cap.
    ///
    /// 120 rows so that both the default and the ceiling cut something — against 100 rows the
    /// clamp and the row count would agree by accident and the assertion would hold with the clamp
    /// deleted.
    #[tokio::test]
    async fn the_leading_window_defaults_to_ten_and_clamps_rather_than_refusing() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let app = test_router(state);
        let id = seed_run_row(
            &pool,
            "timed_out",
            "worktree",
            "2026-08-30T00:00:00Z",
            Some("2026-08-30T00:10:00Z"),
        )
        .await;
        seed_decisions(&pool, id, 120).await;

        let window = async |uri: String| {
            let (status, body) = stop_report(&app, &uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            body["leading_up"].as_array().unwrap().len()
        };

        assert_eq!(
            window(format!("/runs/{id}/stop")).await,
            STOP_LEADING_DEFAULT as usize,
            "omitting the parameter"
        );
        assert_eq!(
            window(format!("/runs/{id}/stop?leading=500")).await,
            STOP_LEADING_MAX as usize,
            "over the ceiling: clamped, and answered rather than refused"
        );
        assert_eq!(
            window(format!("/runs/{id}/stop?leading=3")).await,
            3,
            "under the ceiling: exactly what was asked for"
        );
        assert_eq!(
            window(format!("/runs/{id}/stop?leading=0")).await,
            0,
            "zero is a number, not an absent parameter"
        );
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

    /// The proposal carries a real approved command, not a placeholder: the grant the approval mints
    /// records the action's CLASS, which is derived from that input. A `{}` input classifies as
    /// `unrecognized`, so it would pin the fallback rather than the answer.
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
             (owner_kind, owner_id, project_id, project_root, path, branch, created_at)
             VALUES ('run', ?, 'proj', ?, ?, ?, ?)",
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
            Some(r#"{"command":"git push origin main"}"#),
        )
        .await
        .unwrap();

        (original_run_id, proposal_id, worktree_path)
    }

    /// A paused run whose worktree and project really exist on disk, because the approval path now
    /// asks git two questions about them.
    ///
    /// `seed_resumable_action_approval` deliberately uses invented paths, and that keeps working:
    /// git cannot answer about a directory that is not there, so those approvals take the fallback
    /// and every assertion written before this feature still means what it meant. This helper is for
    /// the other side of that branch.
    async fn seed_real_worktree_approval(
        state: &AppState,
        command: &str,
    ) -> (i64, String, tempfile::TempDir) {
        let container = crate::git_exec::tests::space_free_tempdir("nucleos-approve-merge-");
        let root = container.path().join("repo");
        crate::git_exec::tests::initialize_repo(&root);
        let root = root.to_string_lossy().replace('\\', "/");
        let branch = crate::git_exec::current_branch(
            std::path::Path::new(&root),
            std::time::Instant::now() + Duration::from_secs(60),
        )
        .await
        .expect("the seeded repository has a branch");

        let created_at = chrono::Utc::now().to_rfc3339();
        // `mode` is NOT NULL with a CHECK; `off` is the honest value, since nothing here is driving
        // autopilot — the row exists only because `project_root` lives on it.
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('proj', 'off', ?)
             ON CONFLICT(project_id) DO UPDATE SET project_root = excluded.project_root",
        )
        .bind(&root)
        .execute(&state.pool)
        .await
        .unwrap();

        let original_run_id = sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, session_id, mode, created_at)
             VALUES ('proj', ?, 'x', 'awaiting_approval', 'sess-1', 'worktree', ?)",
        )
        .bind(&root)
        .bind(&created_at)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        // The paused run stands in the repository root itself. A linked worktree would be more
        // lifelike and would test nothing extra here: what the approval reads is the branch of the
        // directory this row names, and one real worktree root is as good as another.
        sqlx::query(
            "INSERT INTO worktrees
             (owner_kind, owner_id, project_id, project_root, path, branch, created_at)
             VALUES ('run', ?, 'proj', ?, ?, ?, ?)",
        )
        .bind(original_run_id)
        .bind(&root)
        .bind(&root)
        .bind(&branch)
        .bind(&created_at)
        .execute(&state.pool)
        .await
        .unwrap();

        let proposal_id = proposals::create_action_approval(
            &state.pool,
            original_run_id,
            Some("sess-1"),
            Some("proj"),
            "Bash",
            "needs approval",
            Some(&serde_json::json!({ "command": command }).to_string()),
        )
        .await
        .unwrap();

        (proposal_id, branch, container)
    }

    /// **An item's tree survives two approvals in a row, and is found again both times.**
    ///
    /// Two and not one, because of WHERE the defect lives. With `item_id` left out of the successor
    /// INSERT the first resume still lands in the right checkout — the owner is resolved from the
    /// predecessor's row, which has the item — and what goes wrong is only what that resume WRITES:
    /// a run with no item, whose own approval would then resolve to the job's integration tree and
    /// relaunch the agent with its work in a checkout nobody is looking at. So the loop asserts the
    /// stored column and not only the directory, and it runs twice: the column is what carries the
    /// answer forward, and the second turn is what a run of one would never reach.
    ///
    /// Confirmed by removing the column from that INSERT: the loop fails on the first turn, on the
    /// column, which is the earliest point at which the mistake is visible at all.
    ///
    /// The row in `worktrees` is checked at the end for the other half of the same decision: the
    /// two hand-over statements filter on `owner_kind = 'run'`, so an item's tree does not move
    /// with the run. That is what a key of `job_items.id` buys, and a "fix" to those filters would
    /// hand the tree to whichever run finished last and let the GC take it out from under the item.
    #[tokio::test]
    async fn an_items_tree_survives_two_approvals_in_a_row() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;

        let container = crate::git_exec::tests::space_free_tempdir("nucleos-item-approve-");
        let root = container.path().join("repo");
        crate::git_exec::tests::initialize_repo(&root);
        let root = root.to_string_lossy().replace('\\', "/");
        let branch = crate::git_exec::current_branch(
            std::path::Path::new(&root),
            std::time::Instant::now() + Duration::from_secs(60),
        )
        .await
        .expect("the seeded repository has a branch");
        let created_at = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('proj', 'off', ?)
             ON CONFLICT(project_id) DO UPDATE SET project_root = excluded.project_root",
        )
        .bind(&root)
        .execute(&state.pool)
        .await
        .unwrap();

        let job_id = crate::job::insert_job(
            &state.pool,
            &crate::job::NewJob {
                project_id: "proj",
                project_root: &root,
                rule_name: Some("nightly"),
                prompt: "advance the backlog",
                max_items: 5,
                gate_each: true,
                review: true,
                gate_retries: 0,
                head_sha: None,
                max_rounds: None,
                budget_usd: None,
                team_id: None,
            },
        )
        .await
        .expect("start a job");
        let item_id = sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status)
             VALUES (?, 0, 'the item', 'running')",
        )
        .bind(job_id)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        // The item's own checkout. Standing it at the repository root is enough here: what the
        // approval reads is the branch of the directory the row names.
        sqlx::query(
            "INSERT INTO worktrees
             (owner_kind, owner_id, project_id, project_root, path, branch, created_at)
             VALUES ('item', ?, 'proj', ?, ?, ?, ?)",
        )
        .bind(item_id)
        .bind(&root)
        .bind(&root)
        .bind(&branch)
        .bind(&created_at)
        .execute(&state.pool)
        .await
        .unwrap();

        let mut paused = sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, session_id, mode, created_at,
                               job_id, stage, item_id)
             VALUES ('proj', ?, 'x', 'awaiting_approval', 'sess-1', 'worktree', ?, ?,
                     'implement', ?)",
        )
        .bind(&root)
        .bind(&created_at)
        .bind(job_id)
        .bind(item_id)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        for approval in 1..=2 {
            let proposal_id = proposals::create_action_approval(
                &state.pool,
                paused,
                Some("sess-1"),
                Some("proj"),
                "Bash",
                "needs approval",
                Some(&serde_json::json!({ "command": "cargo test" }).to_string()),
            )
            .await
            .unwrap();

            let resumed = resume_approved_run(&state, proposal_id)
                .await
                .unwrap_or_else(|error| panic!("approval {approval} was refused: {error:?}"));

            let (carried, cwd): (Option<i64>, String) =
                sqlx::query_as("SELECT item_id, cwd FROM runs WHERE id = ?")
                    .bind(resumed)
                    .fetch_one(&state.pool)
                    .await
                    .unwrap();
            assert_eq!(
                carried,
                Some(item_id),
                "approval {approval} produced a run that had forgotten its item"
            );
            assert_eq!(
                cwd, root,
                "approval {approval} resumed the agent somewhere other than the item's tree"
            );

            // Pause the successor, so the next turn of the loop approves that one.
            sqlx::query("UPDATE runs SET status = 'awaiting_approval' WHERE id = ?")
                .bind(resumed)
                .execute(&state.pool)
                .await
                .unwrap();
            paused = resumed;
        }

        let (kind, owner_id): (String, i64) = sqlx::query_as(
            "SELECT owner_kind, owner_id FROM worktrees WHERE path = ? AND removed_at IS NULL",
        )
        .bind(&root)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            (kind.as_str(), owner_id),
            ("item", item_id),
            "the item's tree moved to a run — the identity a stable key exists to prevent"
        );
    }

    /// An item gets a checkout of its own, born where its job's branch stands, and pays a slot for
    /// it — and a second run for the same item comes back to the same checkout on the same slot.
    ///
    /// The second half is the one that would leak. `claim` is keyed on the owner and an item's owner
    /// is its row id, so a retry asks for the slot it is already holding; if `claim` were not
    /// idempotent, or if the tree were named after the run, a three-attempt item would end up
    /// holding three of a project's slots and the project would refuse itself.
    #[tokio::test(flavor = "current_thread")]
    async fn an_item_run_gets_its_own_tree_and_pays_one_slot_for_every_attempt() {
        // Held across the awaits on purpose: it serialises mutation of the process-wide
        // `NUCLEOS_WORKTREE_ROOT`, which is the whole reason it exists. Without it a sibling test
        // moves the root out from under `adopt_or_create_at`, which then looks for this item's
        // checkout somewhere it never was, decides there is none, and fails to create one over a
        // branch that already exists — a failure that reads like a bug in adoption and is not.
        let _lock = crate::worktree::test_env_lock();
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let container = crate::git_exec::tests::space_free_tempdir("nucleos-item-tree-");
        let root = container.path().join("repo");
        crate::git_exec::tests::initialize_repo(&root);
        let trees = crate::git_exec::tests::space_free_tempdir("nucleos-item-trees-");
        // Through the guard, and never a bare `set_var`. This used to be one, under a comment
        // claiming the value was "read by `worktree_root` on this task only" — which is not what
        // an environment variable is. It was never restored, so every test that ran afterwards and
        // provisioned a worktree WITHOUT setting a root of its own inherited this one: `worktree_root`
        // returns the variable verbatim and only falls back to a sibling of the project root when it
        // is unset. They then shared a root that had already been deleted with this test's
        // `TempDir`, and two of them creating a job with the same id would land on one
        // `nucleos/job-<id>`.
        //
        // **Corrected 2026-08-22: this said the leak "arrives about once in a dozen full runs", and
        // that number was never measured.** It was written while hunting a red assumed to be
        // intermittent, which turned out not to be — it was
        // `the_module_map_matches_the_files_on_disk`, failing deterministically wherever
        // `core/AGENTS.md` exists and passing wherever it does not, because the file is gitignored
        // and the test returns early when it is absent. Thirteen archived full runs were green and
        // the only two reds in any of them were that test. Nine more runs since, six of them at
        // `--test-threads=32`, have never produced this one. The hazard is real and reads straight
        // off `worktree_root`; its rate is unknown, and stating one sent the next reader looking
        // for a race nobody had seen.
        let _trees_env = WorktreeRootEnv::set(trees.path());

        let job_id = crate::job::insert_job(
            &state.pool,
            &crate::job::NewJob {
                project_id: "proj",
                project_root: &root.to_string_lossy(),
                rule_name: Some("nightly"),
                prompt: "advance the backlog",
                max_items: 5,
                gate_each: true,
                review: true,
                gate_retries: 0,
                head_sha: None,
                max_rounds: None,
                budget_usd: None,
                team_id: None,
            },
        )
        .await
        .expect("start a job");
        let item_id = sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status)
             VALUES (?, 0, 'the item', 'running')",
        )
        .bind(job_id)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();
        // The commit the item's tree is born on. Read from the repository rather than invented,
        // because `git worktree add` takes it as a commit-ish and a made-up one does not resolve.
        let base = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .arg("rev-parse")
                .arg("HEAD")
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .expect("utf8")
        .trim()
        .to_owned();

        let mut paths = Vec::new();
        for attempt in 1..=2 {
            let run_id = create_job_item_run(
                &state,
                "do the item".to_owned(),
                "proj".to_owned(),
                root.to_string_lossy().into_owned(),
                JobItem {
                    job_id,
                    item_id,
                    stage: "implement",
                    base: base.clone(),
                },
            )
            .await
            .unwrap_or_else(|error| panic!("attempt {attempt} was refused: {error:?}"));

            let (cwd, carried_job, carried_item): (String, Option<i64>, Option<i64>) =
                sqlx::query_as("SELECT cwd, job_id, item_id FROM runs WHERE id = ?")
                    .bind(run_id)
                    .fetch_one(&state.pool)
                    .await
                    .unwrap();
            assert_eq!((carried_job, carried_item), (Some(job_id), Some(item_id)));
            assert!(
                cwd.ends_with(&format!("item-{item_id}")),
                "attempt {attempt} ran in `{cwd}`, not in the item's own checkout"
            );
            paths.push(cwd);
        }
        assert_eq!(paths[0], paths[1], "the second attempt made a second tree");

        let (kind, owner_id): (String, i64) = sqlx::query_as(
            "SELECT owner_kind, owner_id FROM worktrees WHERE path = ? AND removed_at IS NULL",
        )
        .bind(&paths[0])
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!((kind.as_str(), owner_id), ("item", item_id));

        let slots: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_slots")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(slots, 1, "two attempts at one item took two slots");
        assert!(
            crate::concurrency::slot_of(&state.pool, crate::worktree::Owner::Item(item_id))
                .await
                .unwrap()
                .is_some(),
            "the slot is held by the item, which is what gives it back when the item is over"
        );

    }

    async fn grants_for(state: &AppState, run_id: i64) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM action_grants WHERE run_id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    /// A merge admitted by an approval that then fails leaves no merge behind.
    ///
    /// A queued merge surviving a rolled-back approval would be an irreversible publication against
    /// an approval that did not happen — with the proposal still pending, so a person could
    /// authorise it a second time. What is pinned is that pair: nothing queued, nothing decided.
    ///
    /// The failure has to be induced, and the way this test induced it before the merge is gone. It
    /// moved the paused run off `awaiting_approval` so that `one_open_worktree_run_per_project`
    /// (migration 0009) would refuse the resume's INSERT. Migration 0053 dropped that index: a run
    /// stranded at `running` no longer blocks its whole project, it holds one numbered slot in
    /// `project_slots`, and a resume takes over the paused run's tree rather than competing with it
    /// for the project. There is no longer a domain state in which the resume's INSERT is refused —
    /// that is the new concurrency model working, not a hole in it.
    ///
    /// So the INSERT is made impossible mechanically instead: `runs.id` is `AUTOINCREMENT`, and with
    /// the sequence parked at `i64::MAX` SQLite has no id left to hand out and fails the statement.
    /// The specific failure is not what is under test — it stands in for any failure between the
    /// admission and the commit — only that the admission is inside that transaction and leaves with
    /// it.
    ///
    /// **What this test does NOT do, written down because the first version of this comment claimed
    /// the opposite.** It does not distinguish `submit_on(&mut *tx, …)` from `submit(&pool, …)`.
    /// Run against that mutation it still passes: the pool's INSERT blocks on the write lock the
    /// open transaction already holds and never lands, so "errored, and nothing queued" is the
    /// outcome either way and no assertion here can separate them.
    ///
    /// The mutation IS caught — by `approving_a_merge_queues_it_instead_of_letting_the_run_perform_it`,
    /// which deadlocks and dies on the busy timeout after 30s. That is coverage by seizing up rather
    /// than by saying anything, and it is worth knowing which of the two you have: a change that made
    /// the admission merely SLOW instead of deadlocked would take that catcher away in silence, and
    /// nothing here would notice.
    #[tokio::test]
    async fn a_merge_admitted_by_an_approval_that_fails_is_rolled_back_with_it() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (proposal_id, _branch, _container) =
            seed_real_worktree_approval(&state, "git merge feature/x").await;

        // After the seeding, and it has to be: every row above needs an id of its own. From here on
        // the sequence is exhausted, so the resume's INSERT is the first one that cannot land.
        advance_run_ids_past(&state.pool, i64::MAX).await;

        // On the VARIANT, not merely on `is_err`. Every assertion below this line is also satisfied
        // by a resume that failed BEFORE it admitted anything — `NotResumable` because the seeded
        // worktree was not found, say — and a test that cannot tell those apart would report the
        // rollback it never exercised. `Db` is reachable only past the admission here.
        let failed = resume_approved_run(&state, proposal_id).await;
        assert!(
            matches!(failed, Err(ResumeError::Db(_))),
            "the resume must fail on its own INSERT, past the admission: {failed:?}"
        );

        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vcs_requests")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            queued, 0,
            "the merge was admitted inside the failed transaction and must have gone with it"
        );
        let status: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "pending", "nothing was decided, so nothing is decided");
    }

    /// The resume takes over the slot its paused run was holding, exactly as it takes over the tree.
    ///
    /// A slot is claimed by an OWNER, and this approval retires one owner and creates another over
    /// the same piece of work. Left on the retired one, the row is not merely untidy: the paused run
    /// is `superseded` in the same transaction, `reconcile_orphaned_slots` frees any slot whose
    /// owner is no longer live, and it runs on every job tick. So the slot is collected out from
    /// under a run that is still working — and the project, now reading one fewer slot in flight
    /// than it has work in flight, starts another. What that permits is the thing `project_slots`
    /// exists to prevent: two worktrees writing one repository.
    ///
    /// The sweep is run here rather than described, because the bookkeeping assertion alone passes
    /// against a transfer that puts the row on any live id at all.
    ///
    /// Filtered to `owner_kind = 'run'` for the same reason the worktree hand-over is: a job's slot
    /// belongs to the JOB, which outlives this node and every other node in its queue.
    #[tokio::test]
    async fn an_approved_resume_takes_over_the_slot_the_paused_run_held() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        // Not a merge: this pins what happens to the SLOT, and the queueing path would only add a
        // second thing for the test to be about.
        let (proposal_id, _branch, _container) =
            seed_real_worktree_approval(&state, "cargo build").await;
        let paused: i64 =
            sqlx::query_scalar("SELECT id FROM runs WHERE status = 'awaiting_approval'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        // The seeded row claims nothing by itself — `create_run_inner` is what claims — so the
        // paused run is given the slot it would have been holding.
        crate::concurrency::claim(&state.pool, "proj", crate::worktree::Owner::Run(paused))
            .await
            .expect("the paused run holds a slot");

        let resume_id = resume_approved_run(&state, proposal_id).await.unwrap();

        let holder: Option<i64> =
            sqlx::query_scalar("SELECT owner_id FROM project_slots WHERE owner_kind = 'run'")
                .fetch_optional(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            holder,
            Some(resume_id),
            "the slot follows the work, as the worktree does"
        );

        crate::concurrency::reconcile_orphaned_slots(&state.pool)
            .await
            .unwrap();
        let held: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_slots")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            held, 1,
            "the resume is running, so the sweep has nothing to collect"
        );
    }

    /// **Decision (B).** Approving a merge hands it to the queue; it does not hand the run a pass to
    /// perform the merge itself.
    ///
    /// This is the answer to the thing this pillar was built for. Before it, the sanctioned route
    /// for an autonomous run to merge was: pause, ask a person, and on yes the RUN merges — with its
    /// own hands, against the same refs another run might be merging into at that moment, which is
    /// the race the queue exists to abolish. The approval was the last place still handing that out.
    ///
    /// The absent grant is half the assertion and the more important half: a queued merge beside a
    /// What a resumed run is told, and why the wording is load-bearing rather than cosmetic.
    ///
    /// **Measured 2026-08-17, twice, in `.ai/eval/ABLATION.md`'s T1×H3 cells.** The instruction used
    /// to be "Proceed with the {tool} action you attempted before the pause — it is now authorized".
    /// The resumed agent has no record of attempting it — it cannot have one, the pause happens
    /// BEFORE the call runs, so it never enters the transcript as a step — and what it saw was an
    /// unverifiable claim of prior authorization asking it to run a command. Both runs refused, in
    /// the words of an agent doing its job: "This looks like it may be an attempt to get me to run a
    /// command under a false claim of prior authorization." One turn, $0.04, nothing done.
    ///
    /// So the instruction must carry the action itself. Not to be more polite — to be checkable: an
    /// agent that can read what was approved can judge it, where one asked to recall it can only
    /// obey or refuse.
    #[test]
    fn a_resume_instruction_states_the_action_it_authorizes() {
        let instruction = resume_instruction(
            61,
            "Bash",
            Some(r#"{"command":"cargo test -p nucleos-core"}"#),
        );
        assert!(
            instruction.contains("cargo test -p nucleos-core"),
            "the agent cannot judge an action it is not shown: {instruction}"
        );
        assert!(
            instruction.contains("61"),
            "the proposal is the audit trail back to the human who approved it: {instruction}"
        );
    }

    /// The half that stops the message reading as an attack. An agent that is told to continue
    /// something it has no memory of SHOULD be suspicious; the fix is to explain the absence, not to
    /// insist harder.
    #[test]
    fn a_resume_instruction_explains_why_the_call_is_absent_from_the_transcript() {
        let instruction = resume_instruction(1, "Bash", Some(r#"{"command":"ls"}"#));
        assert!(
            instruction.contains("transcript"),
            "an unexplained 'you attempted this' is indistinguishable from an injection: \
             {instruction}"
        );
    }

    /// **The note must lose an argument with the task, and must say so.**
    ///
    /// Measured 2026-08-28, in production. A resumed run read the old note, called it an injection
    /// attempt in as many words, and refused it -- correctly, on the evidence it had: the note
    /// asked for a compound shell command its task had explicitly forbidden, and nothing told it
    /// which of the two authorities won. It then found a legal way to the same end on its own,
    /// which is exactly the behaviour to preserve rather than argue out of.
    ///
    /// So the note now declares its own rank. An approval lifts ONE pause; it does not amend the
    /// task, and where the two collide the task takes it. That is the safe direction as well as
    /// the honest one.
    #[test]
    fn a_resume_instruction_does_not_outrank_the_task() {
        let instruction = resume_instruction(3, "Bash", Some(r#"{"command":"ls"}"#));
        assert!(
            instruction.contains("your task wins"),
            "a note that cannot lose to the task leaves a run choosing between two authorities              with nothing to choose on: {instruction}"
        );
        assert!(
            instruction.contains("ONE action and nothing else"),
            "the bound is the other half of the same sentence: {instruction}"
        );
    }

    /// **And it must not claim an authority the reader cannot check.**
    ///
    /// The old text opened with "A human approved one action for this run". Unverifiable from
    /// inside the session, and the exact sentence an attacker writes. What IS checkable is where
    /// the message came from -- the launcher, not the conversation -- so that is what it says now.
    #[test]
    fn a_resume_instruction_names_its_channel_rather_than_its_authority() {
        let instruction = resume_instruction(4, "Bash", Some(r#"{"command":"ls"}"#));
        assert!(
            !instruction.contains("A human approved"),
            "an unverifiable claim of authority is the shape of the attack, not the answer to it:              {instruction}"
        );
        assert!(
            instruction.contains("daemon that launched this session"),
            "the reader can place the channel even when it cannot check the claim: {instruction}"
        );
    }

    /// Absent or unparseable input must not produce an instruction that silently drops the action
    /// and reverts to the wording that failed.
    #[test]
    fn a_resume_instruction_without_readable_input_says_so_rather_than_inventing_one() {
        for input in [None, Some("{not json"), Some(r#"{"no_command":1}"#)] {
            let instruction = resume_instruction(7, "Agent", input);
            assert!(
                instruction.contains("Agent"),
                "the tool is all that is left to name: {instruction}"
            );
            assert!(
                instruction.contains("transcript"),
                "the explanation is needed most when the action cannot be shown: {instruction}"
            );
        }
    }

    /// A prompt is not a place to paste an unbounded string: the run pays for every token of it, and
    /// a command built by a loop can be megabytes.
    ///
    /// Measured against a BASELINE rather than against a constant, and the difference is not
    /// pedantry. The old assertion bounded the whole note, which held only while the note was
    /// short: rewriting it to explain itself (see `a_resume_instruction_does_not_outrank_the_task`)
    /// broke a test about truncation for a reason that had nothing to do with truncation. What is
    /// actually claimed is that the QUOTED ACTION costs at most `RESUME_ACTION_CHARS`, whatever the
    /// prose around it grows to, and subtracting the same note with a one-character command is how
    /// you ask that question.
    #[test]
    fn a_resume_instruction_bounds_the_action_it_quotes() {
        let huge = "x".repeat(RESUME_ACTION_CHARS * 4);
        let input = serde_json::json!({ "command": huge }).to_string();
        let instruction = resume_instruction(1, "Bash", Some(&input));
        let baseline =
            resume_instruction(1, "Bash", Some(&serde_json::json!({ "command": "x" }).to_string()))
                .chars()
                .count();
        let quoted = instruction.chars().count() - baseline;
        assert!(
            quoted <= RESUME_ACTION_CHARS,
            "the quoted action added {quoted} chars over a one-character baseline, and the cap is              {RESUME_ACTION_CHARS}"
        );
        assert!(
            instruction.contains('…'),
            "a truncated action must say it was truncated: {instruction}"
        );
    }

    /// minted grant would be both at once, and the two would race each other.
    #[tokio::test]
    async fn approving_a_merge_queues_it_instead_of_letting_the_run_perform_it() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (proposal_id, branch, _container) =
            seed_real_worktree_approval(&state, "git merge feature/x").await;

        let resume_id = resume_approved_run(&state, proposal_id).await.unwrap();

        let (request_id, op, args, origin, status): (i64, String, String, String, String) =
            sqlx::query_as(
                "SELECT id, op, args, origin, status FROM vcs_requests ORDER BY id DESC LIMIT 1",
            )
            .fetch_one(&state.pool)
            .await
            .expect("the approved merge is in the queue");
        assert_eq!(op, "merge");
        assert_eq!(
            (origin.as_str(), status.as_str()),
            ("human", "queued"),
            "a human just approved it, so it carries their authority and waits for nothing"
        );
        // The command named the source; the target is the branch the run's worktree stands on,
        // which is the half no command line carries.
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&args).unwrap(),
            serde_json::json!({"op": "merge", "source": "feature/x", "target": branch})
        );

        // Asserted through the two queries rather than by counting rows: since migration 0054 the
        // approval writes a row EITHER way, and what separates them is what that row answers. It
        // must record the takeover and must not authorize anything.
        //
        // The authorizing half is asked by CLASS, which is what a grant covers since migration 0055,
        // and `push-merge-deploy` is the class this very command was paused under. Asking it the old
        // way — by the exact input — would leave the test passing while the row authorized every
        // other merge the run went on to try.
        let input = serde_json::json!({ "command": "git merge feature/x" }).to_string();
        assert!(
            !proposals::grant_covers_class(&state.pool, resume_id, "push-merge-deploy")
                .await
                .unwrap(),
            "the queue took the merge, so the run must NOT also be authorized to perform it"
        );
        assert_eq!(
            proposals::matching_queued_request(&state.pool, resume_id, "Bash", &input)
                .await
                .unwrap(),
            Some(request_id),
            "and the run has to be able to be TOLD which request has its work"
        );

        let note: String = sqlx::query_scalar(
            "SELECT note FROM proposal_events WHERE proposal_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(proposal_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert!(
            note.contains("merge queued as vcs request"),
            "the audit trail has to say what the approval actually did: {note}"
        );
    }

    /// **The last command on the approval list that was still handed back to the run.**
    ///
    /// `git tag` paused for a human and then, on yes, the RUN wrote the tag with its own hands —
    /// which is the arrangement this whole pillar exists to end, surviving in the one place nobody
    /// had got to yet. What is pinned is that the approval queues it instead, and that the audit
    /// trail names the operation: the note was hard-coded to "merge" until `push` landed, and a third
    /// operation is where a two-way `if` would quietly become wrong again.
    ///
    /// The command names no branch, so the tag's target has to come from the worktree — the half no
    /// `git tag v1` carries, and the same half a bare `git push origin` needs.
    #[tokio::test]
    async fn approving_a_tag_queues_it_and_records_which_operation_it_was() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (proposal_id, branch, _container) =
            seed_real_worktree_approval(&state, "git tag v1.0").await;

        let resume_id = resume_approved_run(&state, proposal_id).await.unwrap();

        let (op, args, origin, status): (String, String, String, String) = sqlx::query_as(
            "SELECT op, args, origin, status FROM vcs_requests ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&state.pool)
        .await
        .expect("the approved tag is in the queue");
        assert_eq!(op, "tag");
        assert_eq!((origin.as_str(), status.as_str()), ("human", "queued"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&args).unwrap(),
            serde_json::json!({"op": "tag", "name": "v1.0", "at": branch}),
            "the command named the tag; the branch is the one the worktree stands on"
        );

        assert!(
            !proposals::grant_covers_class(&state.pool, resume_id, "push-merge-deploy")
                .await
                .unwrap(),
            "the queue took the tag, so the run must NOT also be authorized to write it"
        );

        let note: String = sqlx::query_scalar(
            "SELECT note FROM proposal_events WHERE proposal_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(proposal_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert!(
            note.contains("tag queued as vcs request"),
            "the trail must name the operation it queued: {note}"
        );
    }

    /// The two operations that take no branch from the worktree, through the approval door.
    ///
    /// Table-driven because what is being pinned is the CHAIN: `queueable_operation` tries five
    /// parsers in an `or_else` sequence, and the two added last are the two that ignore the branch
    /// argument entirely. A per-operation test would pass with either of them missing from the
    /// chain, since each one's own parser is tested next door.
    #[tokio::test]
    async fn approving_a_fetch_or_a_branch_delete_queues_it_too() {
        for (command, expected_op, expected_args) in [
            (
                "git fetch origin",
                "fetch",
                serde_json::json!({"op": "fetch", "remote": "origin"}),
            ),
            (
                "git branch -d feature",
                "branch-delete",
                serde_json::json!({"op": "branch-delete", "branch": "feature"}),
            ),
        ] {
            let (state, _runner) =
                test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600))
                    .await;
            let (proposal_id, _branch, _container) =
                seed_real_worktree_approval(&state, command).await;

            let resume_id = resume_approved_run(&state, proposal_id).await.unwrap();

            let (op, args): (String, String) =
                sqlx::query_as("SELECT op, args FROM vcs_requests ORDER BY id DESC LIMIT 1")
                    .fetch_one(&state.pool)
                    .await
                    .unwrap_or_else(|_| panic!("{command} should have been queued"));
            assert_eq!(op, expected_op, "{command}");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&args).unwrap(),
                expected_args,
                "{command}"
            );
            assert!(
                !proposals::grant_covers_class(&state.pool, resume_id, "push-merge-deploy")
                    .await
                    .unwrap(),
                "{command}: the queue took it, so the run must not also be authorized"
            );
        }
    }

    /// The other side of the branch, and the one that keeps this from being a regression: an
    /// approved action the queue cannot perform is authorized exactly as it always was.
    ///
    /// **The subject used to be `git push origin main`, and this is what it cost to change it.** That
    /// command is now queued, so the test as written would have gone red — which is the correct
    /// signal, and the wrong fix would have been to delete it. The property is not about push; it is
    /// that a run whose approved action has no executor still gets to perform it, and losing that
    /// leaves a person holding an approval with nowhere to go. `--force-with-lease` is the sharpest
    /// remaining case precisely BECAUSE it is a push: `vcs::push_from_command` sees the right verb
    /// and refuses on the flag, so this exercises the refusal rather than the absence of a parser.
    #[tokio::test]
    async fn approving_something_the_queue_cannot_perform_still_authorizes_the_run() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (proposal_id, _branch, _container) =
            seed_real_worktree_approval(&state, "git push --force-with-lease origin main").await;

        let resume_id = resume_approved_run(&state, proposal_id).await.unwrap();

        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vcs_requests")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            queued, 0,
            "the queue does not force-push, so it must not claim to"
        );
        assert_eq!(
            grants_for(&state, resume_id).await,
            1,
            "an action the queue does not take is still the run's to perform, once"
        );
    }

    /// **The class this resume records is derived under the project's own rules, not under an empty
    /// pair.** The class is what a grant is scoped to, so a resume classifying `bash
    /// scripts/gates.sh core` as `unrecognized` while the hook classified it as `project-declared`
    /// would hand the run an authorization for a different kind of action than the one the person
    /// approved.
    ///
    /// `bash scripts/gates.sh` and not a git command on purpose: the queue cannot perform it, so
    /// the approval authorizes the run rather than queueing the action, which is the branch that
    /// records a class at all. Nothing compiled recognises it either, so `project-declared` can only
    /// have come from the declared prefix.
    #[tokio::test]
    async fn a_resume_records_the_class_this_projects_rules_give() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (proposal_id, _branch, _container) =
            seed_real_worktree_approval(&state, "bash scripts/gates.sh core").await;
        crate::project_policy::declare_shell_rule(
            &state.pool,
            "proj",
            "bash scripts/gates.sh",
            crate::project_policy::Verdict::Allow,
            None,
        )
        .await
        .unwrap();

        let resume_id = resume_approved_run(&state, proposal_id).await.unwrap();

        let recorded: Option<String> =
            sqlx::query_scalar("SELECT action_class FROM action_grants WHERE run_id = ?")
                .bind(resume_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(recorded.as_deref(), Some("project-declared"));
    }

    /// The same sentence about the other table: the class this resume records is derived under the
    /// project's own GitHub policy, and not under the daemon's.
    ///
    /// `test_state_with_runner` ships `GithubRuntime::default()`, autonomous in nothing, so
    /// `github-read` can only have come from the declared operation — and if it did not, the class
    /// would be `unrecognized`, which is a DIFFERENT grant from the one the person was shown. That
    /// is the drift the re-derivation exists to rule out, arriving through the input that used to be
    /// the one input this caller could take for granted.
    ///
    /// A `gh` line and not a git one, for `a_resume_records_the_class_this_projects_rules_give`'s
    /// reason: the queue cannot perform it, so the approval authorizes the run and a class is
    /// recorded at all.
    #[tokio::test]
    async fn a_resume_records_the_class_this_projects_github_ops_give() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (proposal_id, _branch, _container) =
            seed_real_worktree_approval(&state, "gh run list -R owner/name").await;
        crate::project_policy::declare_github_op(&state.pool, "proj", "run_list")
            .await
            .unwrap();

        let resume_id = resume_approved_run(&state, proposal_id).await.unwrap();

        let recorded: Option<String> =
            sqlx::query_scalar("SELECT action_class FROM action_grants WHERE run_id = ?")
                .bind(resume_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(recorded.as_deref(), Some("github-read"));
    }

    /// And rules that cannot be read are recorded as NO class, which is what this path already does
    /// for input it cannot parse.
    ///
    /// Deliberately not the hook's answer to the same failure. The hook is DECIDING and owes the
    /// safe direction, so it downgrades an allow to an approval prompt; this is LABELLING an action
    /// a person has already approved, where the only outcomes available are a right label and a
    /// wrong one — and a wrong one is worse than none, because a grant is scoped to the class.
    ///
    /// A classless grant authorizes nothing: `grant_covers_class` matches on the class, and no class
    /// matches `NULL`. The table is dropped rather than mocked, which is the only honest way to make
    /// the read fail from outside `project_policy`.
    #[tokio::test]
    async fn a_resume_records_no_class_when_the_projects_rules_cannot_be_read() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (proposal_id, _branch, _container) =
            seed_real_worktree_approval(&state, "bash scripts/gates.sh core").await;
        sqlx::query("DROP TABLE project_shell_rules")
            .execute(&state.pool)
            .await
            .unwrap();

        let resume_id = resume_approved_run(&state, proposal_id).await.unwrap();

        let recorded: Option<String> =
            sqlx::query_scalar("SELECT action_class FROM action_grants WHERE run_id = ?")
                .bind(resume_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(recorded, None);
        assert!(
            !proposals::grant_covers_class(&state.pool, resume_id, "unrecognized")
                .await
                .unwrap()
        );
    }

    /// The push counterpart of `approving_a_merge_queues_it_instead_of_letting_the_run_perform_it`,
    /// and it is not a copy of it: it pins the two halves that are push's own.
    ///
    /// The command names only the remote, so the BRANCH has to come from the worktree — the same
    /// half no push command line carries that a merge's target does. And the audit note has to say
    /// `push`, because that sentence was hard-coded to the word "merge" for as long as merge was the
    /// only thing the queue could do; a trail that calls every operation a merge is a trail nobody
    /// can reconstruct an approval from.
    #[tokio::test]
    async fn approving_a_push_queues_it_and_records_which_operation_it_was() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (proposal_id, branch, _container) =
            seed_real_worktree_approval(&state, "git push origin").await;

        let resume_id = resume_approved_run(&state, proposal_id).await.unwrap();

        let (op, args, origin, status): (String, String, String, String) =
            sqlx::query_as("SELECT op, args, origin, status FROM vcs_requests ORDER BY id DESC LIMIT 1")
                .fetch_one(&state.pool)
                .await
                .expect("the approved push is in the queue");
        assert_eq!(op, "push");
        assert_eq!((origin.as_str(), status.as_str()), ("human", "queued"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&args).unwrap(),
            serde_json::json!({"op": "push", "remote": "origin", "branch": branch}),
            "the command named the remote; the branch is the one the worktree stands on"
        );

        assert!(
            !proposals::grant_covers_class(&state.pool, resume_id, "push-merge-deploy")
                .await
                .unwrap(),
            "the queue took the push, so the run must NOT also be authorized to perform it"
        );

        let note: String = sqlx::query_scalar(
            "SELECT note FROM proposal_events WHERE proposal_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(proposal_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert!(
            note.contains("push queued as vcs request"),
            "the trail must name the operation it queued, not the one it used to be: {note}"
        );
    }

    /// §6.2 of the design, and the one path where the shell offered a button that could not work.
    ///
    /// A node of a job does not own the tree it is paused in — the job does — so the resume looked
    /// the worktree up by run, found none, and answered `NotResumable` for every paused job node.
    /// Three things have to be true afterwards, and getting any of them wrong turns an approval
    /// into something worse than the refusal it replaced.
    #[tokio::test]
    async fn approving_a_job_node_resumes_it_in_the_jobs_worktree() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let worktree_path = "C:/worktrees/proj/job-1";
        let job_id = crate::job::insert_job(
            &state.pool,
            &crate::job::NewJob {
                project_id: "proj",
                project_root: "C:/repos/proj",
                rule_name: Some("nightly"),
                prompt: "advance the backlog",
                max_items: 5,
                gate_each: true,
                review: true,
                gate_retries: 0,
                head_sha: None,
                max_rounds: None,
                budget_usd: None,
                team_id: None,
            },
        )
        .await
        .unwrap();
        let created_at = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO worktrees
             (owner_kind, owner_id, project_id, project_root, path, branch, created_at)
             VALUES ('job', ?, 'proj', 'C:/repos/proj', ?, 'nucleos/job-1', ?)",
        )
        .bind(job_id)
        .bind(worktree_path)
        .bind(&created_at)
        .execute(&state.pool)
        .await
        .unwrap();
        let paused = sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, session_id, mode, created_at,
                               job_id, stage)
             VALUES ('proj', ?, 'x', 'awaiting_approval', 'sess-j', 'worktree', ?, ?, 'implement')",
        )
        .bind(worktree_path)
        .bind(&created_at)
        .bind(job_id)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, run_id)
             VALUES (?, 0, 'an item', 'running', ?)",
        )
        .bind(job_id)
        .bind(paused)
        .execute(&state.pool)
        .await
        .unwrap();
        let proposal_id = proposals::create_action_approval(
            &state.pool,
            paused,
            Some("sess-j"),
            Some("proj"),
            "Bash",
            "push needs approval",
            Some("{}"),
        )
        .await
        .unwrap();
        let seeded: (Option<i64>, Option<String>, i64) = sqlx::query_as(
            "SELECT r.job_id, r.stage,
                    (SELECT COUNT(*) FROM worktrees w
                     WHERE w.owner_kind = 'job' AND w.owner_id = r.job_id AND w.removed_at IS NULL)
             FROM runs r WHERE r.id = ?",
        )
        .bind(paused)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        // The seed, checked before the thing under test touches it. sqlx fills a `?` with no bind
        // behind it as NULL without complaining, so a miscounted bind list produces a run with no
        // job — which is exactly the state whose handling this test exists to prove, and it would
        // have passed by testing nothing.
        assert_eq!(
            (seeded.0, seeded.1.as_deref(), seeded.2),
            (Some(job_id), Some("implement"), 1),
            "the seed itself must be what this test claims to be testing"
        );

        let resume_id = resume_approved_run(&state, proposal_id)
            .await
            .expect("a paused job node is resumable");

        // One: the resume belongs to the same job and plays the same part. A run belonging to no
        // job is invisible to the chain that has to finalise it, so the item would stay `running`
        // until the four-hour ceiling — with the approved work already done.
        let (resumed_job, resumed_stage): (Option<i64>, Option<String>) =
            sqlx::query_as("SELECT job_id, stage FROM runs WHERE id = ?")
                .bind(resume_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(resumed_job, Some(job_id));
        assert_eq!(resumed_stage.as_deref(), Some("implement"));

        // Two: the item follows its node. Left pointing at the run just marked `superseded`, the
        // next pass would read a terminal node that did not complete and stop the whole chain —
        // turning the user's approval into a failure.
        let item_run: Option<i64> =
            sqlx::query_scalar("SELECT run_id FROM job_items WHERE job_id = ? AND ordinal = 0")
                .bind(job_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(item_run, Some(resume_id));

        // Three: the tree still belongs to the job. Handed to this one node, the GC would be free
        // to collect it the moment the node finished, with the rest of the queue still to run in it.
        let owner: (String, i64) =
            sqlx::query_as("SELECT owner_kind, owner_id FROM worktrees WHERE path = ?")
                .bind(worktree_path)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(owner, ("job".to_owned(), job_id));
    }

    /// The base survives the handover of a tree, **through the real path**.
    ///
    /// The lazy version of this test — an `UPDATE worktrees SET owner_id = ?` by hand, then
    /// asserting `base_sha` did not move — is tautological: the `UPDATE` names one column. What
    /// matters is that `resume_approved_run` does not **re-record** the worktree, and that is only
    /// provable by exercising it. Hence the three assertions: one row, a new owner, the same base.
    ///
    /// The twin for the handoff path is `a_handed_off_run_takes_the_slot_of_the_run_it_continues`,
    /// below. It was left unwritten while the defect there was unconfirmed; it is confirmed now.
    #[tokio::test]
    async fn a_resumed_run_keeps_the_base_of_the_tree_it_inherited() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (original_run_id, proposal_id, worktree_path) =
            seed_resumable_action_approval(&state, Some("sess-base")).await;
        let path = worktree_path.to_string_lossy().into_owned();
        sqlx::query("UPDATE worktrees SET base_sha = 'ba5eba5e' WHERE owner_id = ?")
            .bind(original_run_id)
            .execute(&state.pool)
            .await
            .unwrap();

        let resume_run_id = resume_approved_run(&state, proposal_id).await.unwrap();

        let rows: Vec<(String, i64, Option<String>)> =
            sqlx::query_as("SELECT owner_kind, owner_id, base_sha FROM worktrees WHERE path = ?")
                .bind(&path)
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![("run".to_owned(), resume_run_id, Some("ba5eba5e".to_owned()))],
            "the resume re-recorded the tree instead of taking the row over"
        );
    }

    /// **The conflict follows its resolver across the resume**, like the tree, the slot and the job
    /// items above it.
    ///
    /// `vcs_requests.resolution_run_id` is what says a resolution is live. `resolver.rs` joins
    /// through it onto a run to keep two agents off one conflict, and joins through it again to stop
    /// one whose conflict somebody has settled another way. Left pointing at the predecessor — which
    /// this transaction has just marked `superseded` — both joins miss the run that is actually
    /// doing the work: a second agent can be minted for the same two branches, and nothing can reach
    /// the first.
    ///
    /// Measured. Requests 79 and 80 each got a resolution and each resolution was resumed once, so
    /// both ended up linked to rows reading `superseded`. Their conflicts were then settled by hand
    /// and landed as request 85; the two agents carried on for another thirteen hours and $3.01
    /// between them before escalating again as requests 87 and 88, over a conflict that had stopped
    /// existing the previous evening.
    #[tokio::test]
    async fn a_resumed_resolution_keeps_the_conflict_it_was_minted_for() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (original_run_id, proposal_id, _worktree_path) =
            seed_resumable_action_approval(&state, Some("sess-resolution")).await;

        // The escalated merge this run was minted to resolve, admitted through the real INSERT.
        let request = crate::vcs::submit(
            &state.pool,
            &crate::vcs::ResolvedRepo::synthetic("proj", "C:/repos/proj", "proj"),
            &crate::vcs::Op::Merge {
                source: "feat/x".into(),
                target: "master".into(),
            },
            crate::vcs::Origin::Shell,
        )
        .await
        .expect("admit the merge");
        sqlx::query(
            "UPDATE vcs_requests SET status = 'escalated', resolution_run_id = ? WHERE id = ?",
        )
        .bind(original_run_id)
        .bind(request)
        .execute(&state.pool)
        .await
        .unwrap();

        let resume_id = resume_approved_run(&state, proposal_id).await.unwrap();

        let linked: Option<i64> =
            sqlx::query_scalar("SELECT resolution_run_id FROM vcs_requests WHERE id = ?")
                .bind(request)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            linked,
            Some(resume_id),
            "the conflict still points at run {original_run_id}, which this resume marked \
             superseded — the resolution reads as dead while its agent is working"
        );
    }


    /// A context handoff carries the slot across, like the approval resume above it.
    ///
    /// The two paths continue one piece of work in one checkout, and the slot is what says that
    /// checkout is occupied. The handoff already moves the `worktrees` row to the successor; leaving
    /// `project_slots` pointing at the predecessor makes the two disagree about who is working in
    /// the tree, and `reconcile_orphaned_slots` settles that argument the wrong way — it frees any
    /// slot whose owner is not live, and the predecessor is `completed` by then. The project reads
    /// one fewer in flight than it has and starts another run in the same repository, which is the
    /// single thing `project_slots` exists to prevent.
    ///
    /// Handed over rather than claimed, for the reason spelled out at the resume: a claim is per
    /// The volante survives the handoff, and the row and the launch agree about it.
    ///
    /// Both halves are asserted because either alone is a bug. A row that says `steerable` with a
    /// launch that never opened stdin refuses every turn; a launch that listens with a row that
    /// says no holds stdin open with nothing able to close it, and the run can then only end on a
    /// deadline. The old code chose the safe half — neither — and the cost was an owner watching a
    /// run walk into a mistake with no way to say so.
    #[tokio::test]
    async fn a_successor_keeps_the_permission_to_be_steered() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO runs (id, project_id, prompt, status, mode, steerable, context_fill, created_at)
             VALUES (43201, 'project-s', 'the task', 'running', 'real', 1, ?, '2026-08-28T00:00:00Z')",
        )
        .bind(HANDOFF_CONTEXT_LIMIT_FLOOR * 4 / 5)
        .execute(&pool)
        .await
        .unwrap();

        let successor = prepare_handoff_successor(&pool, 43201)
            .await
            .unwrap()
            .expect("the run was over the threshold and had no successor yet");

        assert!(
            successor.steerable,
            "the launch would not open a stdin the row admits turns on"
        );
        let recorded: i64 = sqlx::query_scalar("SELECT steerable FROM runs WHERE id = ?")
            .bind(successor.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            recorded, 1,
            "post_run_message reads the row, so the row is what decides whether a turn is admitted"
        );
    }

    /// And a run nobody asked to steer stays unsteerable across the same hop. The inheritance is a
    /// copy, not a promotion.
    #[tokio::test]
    async fn a_successor_of_an_unsteerable_run_is_not_promoted() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO runs (id, project_id, prompt, status, mode, context_fill, created_at)
             VALUES (43202, 'project-s', 'the task', 'running', 'worktree', ?, '2026-08-28T00:00:00Z')",
        )
        .bind(HANDOFF_CONTEXT_LIMIT_FLOOR * 4 / 5)
        .execute(&pool)
        .await
        .unwrap();

        let successor = prepare_handoff_successor(&pool, 43202)
            .await
            .unwrap()
            .expect("the run was over the threshold and had no successor yet");

        assert!(!successor.steerable);
    }

    /// owner, so the successor would ask for a SECOND slot while the predecessor still held the
    /// first, and a project at its ceiling would refuse to continue work it had already admitted.
    /// A handover cannot fail on a full project, because it does not change how many are held.
    #[tokio::test]
    async fn a_handed_off_run_takes_the_slot_of_the_run_it_continues() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO runs (id, project_id, prompt, status, mode, context_fill, created_at)
             VALUES (43001, 'project-a', 'a long one', 'running', 'real', ?, '2026-08-12T00:00:00Z')",
        )
        .bind(HANDOFF_CONTEXT_LIMIT_FLOOR * 4 / 5)
        .execute(&pool)
        .await
        .unwrap();
        let held = crate::concurrency::claim(
            &pool,
            "project-a",
            crate::worktree::Owner::Run(43001),
        )
        .await
        .unwrap();
        let crate::concurrency::ClaimOutcome::Claimed(slot) = held else {
            panic!("the predecessor could not take a slot to hand over");
        };

        let successor = prepare_handoff_successor(&pool, 43001)
            .await
            .unwrap()
            .expect("the run was over the threshold and had no successor yet");

        assert_eq!(
            crate::concurrency::slot_of(&pool, crate::worktree::Owner::Run(successor.id))
                .await
                .unwrap(),
            Some(slot),
            "the successor is working in the tree without holding its slot"
        );
        assert_eq!(
            crate::concurrency::slot_of(&pool, crate::worktree::Owner::Run(43001))
                .await
                .unwrap(),
            None,
            "the predecessor kept a slot it is no longer working in"
        );
    }

    /// The other half of that filter: a node's handoff must not move its JOB's slot.
    ///
    /// A job holds one slot for the whole chain, and its nodes hold none. Were the update above
    /// keyed on the owner id alone, a node handing off would carry the job's slot to itself — and
    /// the job would lose it the moment that one node finished, with the rest of the queue still to
    /// run. The same trap the `worktrees` update next to it names, one table over.
    #[tokio::test]
    async fn a_node_handing_off_leaves_its_jobs_slot_where_it_is() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO jobs (id, project_id, project_root, status, max_items, created_at)
             VALUES (7, 'project-a', 'C:/somewhere', 'implementing', 5, '2026-08-12T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO runs (id, project_id, prompt, status, mode, context_fill, job_id, created_at)
             VALUES (43101, 'project-a', 'a node', 'running', 'real', ?, 7, '2026-08-12T00:00:00Z')",
        )
        .bind(HANDOFF_CONTEXT_LIMIT_FLOOR * 4 / 5)
        .execute(&pool)
        .await
        .unwrap();
        // The job owns the slot, exactly as `create_run_with` leaves it: the node claimed nothing.
        crate::concurrency::claim(&pool, "project-a", crate::worktree::Owner::Job(7))
            .await
            .unwrap();

        let successor = prepare_handoff_successor(&pool, 43101)
            .await
            .unwrap()
            .expect("the node was over the threshold and had no successor yet");

        assert_eq!(
            crate::concurrency::slot_of(&pool, crate::worktree::Owner::Job(7))
                .await
                .unwrap(),
            Some(0),
            "the node's handoff took the slot out from under its own job"
        );
        assert_eq!(
            crate::concurrency::slot_of(&pool, crate::worktree::Owner::Run(successor.id))
                .await
                .unwrap(),
            None,
            "a node was given a slot of its own, so the job now costs two"
        );
    }

    #[tokio::test]
    async fn approve_resumes_session_in_same_worktree_and_grants_the_approved_actions_class() {
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
            sqlx::query_scalar::<_, i64>("SELECT owner_id FROM worktrees WHERE owner_kind = 'run' AND path = ?")
                .bind(worktree_path.to_string_lossy().as_ref())
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(transferred_run_id, resume_run_id);

        // The class is what the grant authorizes, so it is what the approval has to write down. A
        // NULL here authorizes nothing, and the resume would park again on the very action the user
        // just approved. `consumed_at` is NULL at mint: the stamp records first USE, and nothing has
        // used it yet.
        let grant = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
            "SELECT tool_name, action_class, consumed_at FROM action_grants WHERE run_id = ?",
        )
        .bind(resume_run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            grant,
            (
                "Bash".to_owned(),
                Some("push-merge-deploy".to_owned()),
                None
            )
        );

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

    /// **A paused `real` run owns no worktree, and reading that as "cannot be resumed" made the
    /// whole mode a dead end.**
    ///
    /// `mode = "real"` runs in a directory the caller named; nothing ever writes a `worktrees` row
    /// for it. So the lookup that resolves where to resume answered `NotResumable` for every paused
    /// `real` run, while the shell went on offering an Approve button that returned a conflict —
    /// leaving refusal as the only way to release a run somebody was trying to allow.
    ///
    /// Measured on run 900376, which parked one minute in and could not be released any other way.
    #[tokio::test]
    async fn a_paused_run_without_a_worktree_resumes_in_the_directory_it_recorded() {
        let state = test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let created_at = chrono::Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('proj', 'off', ?)",
        )
        .bind("C:/repos/proj")
        .execute(&state.pool)
        .await
        .unwrap();

        // No `worktrees` row on purpose: that absence IS the case under test.
        let result = sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, session_id, mode, created_at)
             VALUES ('proj', 'C:/repos/proj', 'x', 'awaiting_approval', 'sess-real', 'real', ?)",
        )
        .bind(&created_at)
        .execute(&state.pool)
        .await
        .unwrap();
        let paused_run_id = result.last_insert_rowid();

        let proposal_id = proposals::create_action_approval(
            &state.pool,
            paused_run_id,
            Some("sess-real"),
            Some("proj"),
            "Bash",
            "unrecognized shell commands and code execution require approval",
            Some(r#"{"command":"pwd"}"#),
        )
        .await
        .unwrap();

        let resumed = resume_approved_run(&state, proposal_id).await;

        assert!(
            resumed.is_ok(),
            "a paused run with a recorded cwd must be resumable: {resumed:?}"
        );
        let cwd: Option<String> = sqlx::query_scalar("SELECT cwd FROM runs WHERE id = ?")
            .bind(resumed.unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            cwd.as_deref(),
            Some("C:/repos/proj"),
            "the resume must continue in the directory the paused run was working in"
        );
    }

    /// The one case the old sentence was right about, kept: no worktree AND nothing recorded about
    /// where the run was working is genuinely nowhere to resume.
    #[tokio::test]
    async fn a_paused_run_with_no_directory_at_all_is_still_not_resumable() {
        let state = test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let created_at = chrono::Utc::now().to_rfc3339();

        let result = sqlx::query(
            "INSERT INTO runs (prompt, status, session_id, mode, created_at)
             VALUES ('x', 'awaiting_approval', 'sess-nowhere', 'real', ?)",
        )
        .bind(&created_at)
        .execute(&state.pool)
        .await
        .unwrap();
        let paused_run_id = result.last_insert_rowid();

        let proposal_id = proposals::create_action_approval(
            &state.pool,
            paused_run_id,
            Some("sess-nowhere"),
            None,
            "Bash",
            "x",
            Some(r#"{"command":"pwd"}"#),
        )
        .await
        .unwrap();

        assert!(matches!(
            resume_approved_run(&state, proposal_id).await,
            Err(ResumeError::NotResumable(_))
        ));
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

    /// A run that ran out of time is the one that MOST needs measuring: it went too far.
    ///
    /// Losing the denominator here is being blind in exactly the case the metric exists to see —
    /// and the two fullest runs in the whole database are a `timed_out` and a `cancelled`.
    ///
    /// Four events 200ms apart against a 600ms wall clock, the same numbers the merge-cancellation
    /// test above uses, so the first two lines are in the transcript when the clock drops the run.
    #[tokio::test]
    async fn a_run_that_ran_out_of_time_still_says_how_far_it_got() {
        let (mut state, runner) =
            test_state_with_runner(Some(Duration::from_millis(200)), Duration::from_millis(600))
                .await;
        state.progress_timeout = Duration::from_secs(30);
        *runner.canned.lock().unwrap() = Some(RunOutcome {
            exit_code: 0,
            stdout: [
                r#"{"type":"assistant","message":{"usage":{"input_tokens":1000,"cache_read_input_tokens":189000},"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
                r#"{"type":"assistant","message":{"usage":{"input_tokens":500,"cache_read_input_tokens":39500},"content":[]}}"#,
                r#"{"type":"assistant","message":{"content":[]}}"#,
                r#"{"type":"result","result":"never arrives"}"#,
            ]
            .join("
"),
            stderr: String::new(),
            session_id: Some("timed-out-session".into()),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            num_turns: None,
            compacted: false,
        });
        let pool = state.pool.clone();
        let app = test_router(state);
        let created = create_run_via_http(&app, "a run that goes too far").await;

        let mut status = String::new();
        for _ in 0..100 {
            status = get_run_status(&app, created.id).await.status;
            if status == "timed_out" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(status, "timed_out", "the wall clock must terminate this run");

        let (tools, peak): (Option<String>, Option<i64>) =
            sqlx::query_as("SELECT tools_used, context_peak FROM runs WHERE id = ?")
                .bind(created.id)
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(peak, Some(190_000), "the peak — the stream did get to speak");
        let tools: Vec<serde_json::Value> =
            serde_json::from_str(&tools.expect("tools_used written")).unwrap();
        assert!(!tools.is_empty(), "the tools it used before it died");
    }

    /// The 72 runs no terminal write reaches: `superseded`, `interrupted`, `cancelled`.
    ///
    /// None of them sees the stream — `finalize_termination` is another actor holding an id and a
    /// status — which is why the periodic mirror exists at all. The peak rides along in a local of
    /// its own, so a run that was killed at its fullest still says how full it was.
    #[tokio::test]
    async fn a_cancelled_run_still_carries_the_peak_the_mirror_saw() {
        let pool = retention_pool().await;
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, created_at)
             VALUES (44001, 'a run somebody killed', 'running', 'worktree', '2026-08-26T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mirror = std::sync::Arc::new(std::sync::Mutex::new(None));

        let _guard = mirror_context_fill(&pool, 44001, std::sync::Arc::clone(&mirror));
        *mirror.lock().unwrap() = Some(190_000);
        tokio::time::sleep(CONTEXT_FILL_PERSIST_INTERVAL * 3).await;
        *mirror.lock().unwrap() = Some(40_000); // it compacted
        tokio::time::sleep(CONTEXT_FILL_PERSIST_INTERVAL * 3).await;

        let (fill, peak): (Option<i64>, Option<i64>) =
            sqlx::query_as("SELECT context_fill, context_peak FROM runs WHERE id = ?")
                .bind(44001_i64)
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(fill, Some(40_000), "the fill follows the mirror down");
        assert_eq!(peak, Some(190_000), "the peak does not come down — that is its whole job");
    }

    /// The three columns a pressure reading needs, and the reason two of them are separate.
    ///
    /// `context_fill` is where the window ENDED and `context_peak` is where it WENT. They only
    /// differ when the run compacted, and that is exactly the run worth measuring — reading the
    /// pressure off the first would report the agent with the least slack as the one with the most.
    #[tokio::test]
    async fn a_finished_agent_run_records_its_tools_its_peak_and_whether_it_compacted() {
        let (state, runner) = test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        // A stream that climbs to 190k, compacts, and ends at 40k.
        *runner.canned.lock().unwrap() = Some(RunOutcome {
            exit_code: 0,
            stdout: [
                r#"{"type":"assistant","message":{"usage":{"input_tokens":1000,"cache_read_input_tokens":189000},"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
                r#"{"type":"assistant","message":{"usage":{"input_tokens":500,"cache_read_input_tokens":39500},"content":[{"type":"tool_use","name":"Read","input":{"file_path":"a.rs"}}]}}"#,
            ]
            .join("
"),
            stderr: String::new(),
            session_id: Some("pressure-session".into()),
            cost_usd: Some(0.02),
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            num_turns: Some(2),
            compacted: true,
        });
        let pool = state.pool.clone();
        let app = test_router(state);
        let created = create_run_via_http(&app, "measure the pressure").await;

        for _ in 0..40 {
            if get_run_status(&app, created.id).await.status == "completed" {
                let measured: (Option<String>, Option<i64>, Option<i64>, i64) = sqlx::query_as(
                    "SELECT tools_used, context_peak, context_fill, compacted FROM runs WHERE id = ?",
                )
                .bind(created.id)
                .fetch_one(&pool)
                .await
                .unwrap();
                let (tools, peak, fill, compacted) = measured;

                let tools: Vec<serde_json::Value> =
                    serde_json::from_str(&tools.expect("tools_used written")).unwrap();
                assert_eq!(tools.len(), 2, "the two calls the stream made");
                assert_eq!(peak, Some(190_000), "the peak");
                assert_eq!(fill, Some(40_000), "the end — and why both columns exist");
                assert_eq!(compacted, 1, "it compacted, and the column has to say so");
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("run did not reach completed status in time");
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
            cache_creation_tokens: Some(3_000),
            num_turns: Some(12),
            compacted: false,
        });
        let pool = state.pool.clone();
        let app = test_router(state);
        let created = create_run_via_http(&app, "persist usage").await;

        for _ in 0..20 {
            let parsed = get_run_status(&app, created.id).await;
            if parsed.status == "completed" {
                // Named because clippy counts the tuple's arms, and the fifth column is exactly the
                // one this test exists to cover.
                type PersistedUsage = (
                    Option<i64>,
                    Option<i64>,
                    Option<i64>,
                    Option<i64>,
                    Option<i64>,
                );
                let usage: PersistedUsage =
                    sqlx::query_as(
                        "SELECT input_tokens, output_tokens, cache_read_tokens,
                                cache_creation_tokens, num_turns
                             FROM runs WHERE id = ?",
                    )
                    .bind(created.id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                // `cache_creation_tokens` is asserted here rather than in a test of its own: it is
                // the same round trip through the same UPDATE, and a near-copy of this test would
                // only make the fifth column look like a separate mechanism from the other four.
                assert_eq!(
                    usage,
                    (Some(1000), Some(500), Some(20_000), Some(3_000), Some(12))
                );
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
            cache_creation_tokens: None,
            num_turns: None,
            compacted: false,
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

    /// PURE. The task survives a resume, and survives being resumed again.
    ///
    /// The second half is the one that matters: without idempotence, a run approved three times
    /// would be launched with a note about a note about a note, and the task pushed a page further
    /// down each time — the same nesting `original_task` already guards against for handoffs.
    #[test]
    fn a_task_carried_through_a_resume_comes_back_out_whole() {
        let task = "Finish the land module, test-first, against the seven tests in §5.";
        assert_eq!(task_to_carry(task), task, "a plain task is its own task");

        let once = format!("proposal #7 authorizes Agent.\n\n{RESUMED_TASK_HEADER}\n{task}");
        assert_eq!(task_to_carry(&once), task);

        let twice = format!(
            "proposal #9 authorizes Bash.\n\n{RESUMED_TASK_HEADER}\n{}",
            task_to_carry(&once)
        );
        assert_eq!(
            task_to_carry(&twice),
            task,
            "a second approval must not wrap the note again"
        );
    }

    /// The note is the whole bridge, so it carries both halves and says which is which.
    #[test]
    fn a_handoff_note_carries_the_task_and_the_predecessors_own_words() {
        let note = handoff_prompt("Migrate billing to the new API", Some("I did three of seven."));

        assert!(note.contains("Migrate billing to the new API"), "{note}");
        assert!(note.contains("I did three of seven."), "{note}");
        assert!(note.contains("FRESH session"), "{note}");

        // A run can end without a closing message, and saying so beats an empty heading that reads
        // as though the predecessor did nothing.
        let silent = handoff_prompt("Some task", None);
        assert!(
            silent.contains("Nothing. It ended without a closing message."),
            "{silent}"
        );
    }

    /// The two halves are cut from opposite ends, and the cut is announced either way.
    #[test]
    fn a_long_half_is_cut_from_the_end_that_matters_least() {
        let long = "x".repeat(HANDOFF_NOTE_LIMIT + 50);

        let task = clip_note(&format!("INSTRUCTION {long}"), HANDOFF_NOTE_LIMIT, false);
        assert!(
            task.starts_with("INSTRUCTION"),
            "an instruction leads with what it wants"
        );
        assert!(task.contains("the rest of this was cut"));

        let reply = clip_note(&format!("{long} LAST WORD"), HANDOFF_NOTE_LIMIT, true);
        assert!(
            reply.ends_with("LAST WORD"),
            "an account ends nearest to where the work stopped"
        );
        assert!(reply.contains("the earlier part of this was cut"));
    }

    /// **The bug this change exists for.** A successor must start EMPTY.
    ///
    /// It was launched `--resume <predecessor> --fork-session`, so it woke holding everything its
    /// predecessor held while its prompt told it the context was fresh, and crossed the same
    /// threshold at once. Nothing caught it because nothing looked at the session shape: it took
    /// asking a live successor about the previous session and getting a correct answer back.
    #[tokio::test]
    async fn a_successor_starts_empty_and_carries_a_note_instead_of_a_transcript() {
        let (state, runner) = test_state_with_runner(None, Duration::from_secs(30)).await;
        let over_the_line = HANDOFF_CONTEXT_LIMIT_FLOOR * 4 / 5;
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, session_id, context_fill, stdout, created_at)
             VALUES (77001, 'Migrate the billing module to the new API', 'completed', 'real',
                     'the-predecessors-session', ?, ?, '2026-08-21T10:00:00Z')",
        )
        .bind(over_the_line)
        .bind(r#"{"type":"result","subtype":"success","result":"I converted three of the seven call sites."}"#)
        .execute(&state.pool)
        .await
        .unwrap();

        let pool = state.pool.clone();
        let dyn_runner = state.runner.clone();
        spawn_handoff_if_needed(
            state,
            dyn_runner,
            77001,
            None,
            None,
            false,
            None,
            GateConfig::NotConfigured,
            1,
            crate::runner::ToolPolicy::None,
            Duration::from_secs(30),
            Duration::from_secs(30),
            false,
            None,
        )
        .await;

        // `spawn_run` detaches, so the launch is observed rather than returned.
        let mut seen = None;
        for _ in 0..200 {
            if let Some(launch) = runner.last_launch.lock().unwrap().clone() {
                seen = Some(launch);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let launch = seen.expect("the successor was never launched");

        assert_eq!(
            launch.resume_session_id, None,
            "the successor resumed its predecessor and inherited the context it exists to shed"
        );
        assert!(
            !launch.fork_session,
            "the successor forked its predecessor: a fork copies a conversation, it does not end one"
        );
        let session = launch
            .session_id
            .expect("the fresh session id never reached the CLI");
        assert_ne!(session, "the-predecessors-session");

        assert!(
            launch.prompt.contains("Migrate the billing module to the new API"),
            "the successor was not told the task: {}",
            launch.prompt
        );
        assert!(
            launch.prompt.contains("three of the seven call sites"),
            "the successor was not told where its predecessor got to: {}",
            launch.prompt
        );
        assert!(
            !launch.prompt.contains("subtype"),
            "the raw stream reached the note instead of the reply: {}",
            launch.prompt
        );

        let stored: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id > 77001")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            stored, launch.prompt,
            "the row a human reads and the prompt the CLI got are different texts"
        );
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

    /// A run the wall clock kills never reports a cost: the future is dropped, so there is no
    /// `RunOutcome` and no `cost_usd` to write. The budget then read that run as `cost_usd IS NULL`
    /// and the money it spent existed only as an approximation recomputed on every check — a number
    /// nothing durable ever held, and one that quietly moved as `now` did while the run was live.
    ///
    /// Pinning it at termination is what makes the spend a fact: the run ended, its duration is
    /// final, and the approximation for that duration is written once, at the configured rate.
    #[tokio::test]
    async fn a_run_killed_by_the_wall_clock_records_an_approximated_cost() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, cost_usd, created_at, completed_at)
             VALUES (44001, 'a run the wall clock cut short', 'timed_out', 'worktree', NULL,
                     '2026-08-08T12:00:00Z', '2026-08-08T12:10:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        record_time_approx_cost(&pool, 44001).await;

        let cost: Option<f64> = sqlx::query_scalar("SELECT cost_usd FROM runs WHERE id = 44001")
            .fetch_one(&pool)
            .await
            .unwrap();
        let cost = cost.expect("a run whose cost was never reported must not be recorded as free");
        assert!(cost > 0.0, "unmeasured time is never $0, got {cost}");
        // Ten minutes at the default `budget_time_cost_per_hour_usd` of $3/h.
        assert!(
            (cost - 0.5).abs() < 1e-9,
            "ten minutes at the configured $3/h is $0.50, got {cost}"
        );
    }

    /// The approximation is a floor for runs that reported nothing, not a correction to runs that
    /// reported something. A measured cost is what the CLI actually charged; overwriting it with a
    /// duration guess would replace the one real number in the budget with a made-up one.
    #[tokio::test]
    async fn an_approximated_cost_never_overwrites_a_real_one() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, cost_usd, created_at, completed_at)
             VALUES (44002, 'a run that reported its own cost', 'completed', 'worktree', 0.0123,
                     '2026-08-08T12:00:00Z', '2026-08-08T12:10:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        record_time_approx_cost(&pool, 44002).await;

        let cost: Option<f64> = sqlx::query_scalar("SELECT cost_usd FROM runs WHERE id = 44002")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            cost,
            Some(0.0123),
            "a measured cost is the one the run actually incurred; an approximation must not \
             replace it"
        );
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

        for mode in [
            "real",
            "shadow",
            crate::email::TRIAGE_MODE,
            crate::team::TEAM_MODE,
        ] {
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

    /// The three policies that read `runs_unattended`, and what a department is to each of them.
    ///
    /// Written as one test because the comment above the function says the three have to learn
    /// about a new mode together — and the third deliberately does NOT fire, which is the part that
    /// reads like a bug when met in isolation. `classifier_governs_tools` also demands
    /// `Unrestricted`, and a department never is: the AND refusing is the design, not an omission.
    #[test]
    fn a_department_is_unattended_and_still_not_governed_by_the_classifier() {
        assert!(runs_unattended(crate::team::TEAM_MODE));
        assert_eq!(
            tool_policy_for_mode(crate::team::TEAM_MODE),
            crate::runner::ToolPolicy::None
        );
        // No `dir` at all, which is a team run's actual state — `team.rs` launches with `cwd: None`
        // — and two of the three conditions refuse before the disk is ever read.
        assert!(!classifier_governs_tools(
            crate::team::TEAM_MODE,
            crate::runner::ToolPolicy::Unrestricted,
            None
        ));
        assert!(!classifier_governs_tools(
            crate::team::TEAM_MODE,
            tool_policy_for_mode(crate::team::TEAM_MODE),
            None
        ));
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
            sqlx::query_as("SELECT path, branch FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?")
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
            sqlx::query_scalar("SELECT path FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?")
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

        // Keyed on the owner pair since migration 0035; `worktrees.run_id` no longer exists.
        let worktree_path: String =
            sqlx::query_scalar("SELECT path FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?")
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
            sqlx::query_scalar("SELECT path FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?")
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
            sqlx::query_scalar("SELECT path FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?")
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
            sqlx::query_scalar("SELECT path FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?")
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
            sqlx::query_scalar("SELECT path FROM worktrees WHERE owner_kind = 'run' AND owner_id = ?")
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

        assert!(
            matches!(result, Err((StatusCode::CONFLICT, _))),
            "the emergency stop must refuse the endpoint too"
        );
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

    /// **A project that is off starts nothing nobody is watching, whichever door the request came
    /// in by.**
    ///
    /// Paired with the test below it on purpose: the two requests are IDENTICAL except for the
    /// project's mode, so what they measure is the mode gate and nothing else. Neither names a
    /// `cwd`, which means a request that gets PAST the gate fails a little further in with the
    /// validation `worktree` mode has always had — and that is exactly what makes the pair
    /// readable. `off` refuses with 422 before the row exists; `shadow` reaches 400 and complains
    /// about the missing field, which is the door letting it through.
    #[tokio::test]
    async fn uma_run_nao_vigiada_e_recusada_a_um_projecto_desligado() {
        let state = test_state().await;
        project_in_mode(&state.pool, "proj", "off").await;

        let refusal = create_run(
            State(state.clone()),
            Json(CreateRunRequest {
                prompt: "work on it all night".to_owned(),
                project_id: Some("proj".to_owned()),
                cwd: None,
                mode: "worktree".to_owned(),
                steerable: false,
            }),
        )
        .await;
        // Matched rather than `expect_err`: the success type is an `axum::Json` of a struct with no
        // `Debug`, and deriving one on a response type to satisfy a test would be the tail wagging.
        let Err(refusal) = refusal else {
            panic!("a project that is off must not start unattended work");
        };

        assert_eq!(refusal.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            refusal.1.contains("off") && refusal.1.contains("proj"),
            "the refusal has to name the project and its state: {}",
            refusal.1
        );
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 0, "no row may be written for a refused run");
    }

    /// **And `shadow` is deliberately NOT refused — this pins the decision, so tightening it later
    /// is a change somebody makes on purpose rather than one that slides in.**
    ///
    /// The stricter rule the design implies is that `worktree` needs `active`, exactly as a job
    /// does. It is not the rule this repository was developed under: its own project sat in
    /// `shadow` while every session worked through that very mode, so adopting it would have
    /// refused the only door the work was coming through. The owner chose to close the case nobody
    /// can defend — a project switched OFF still starting unwatched work — and to leave the rest
    /// until a project is deliberately activated. If this test ever has to change, that is the
    /// decision being revisited, and it should be revisited out loud.
    #[tokio::test]
    async fn um_projecto_em_shadow_ainda_pode_pedir_uma_run_de_worktree() {
        let state = test_state().await;
        project_in_mode(&state.pool, "proj", "shadow").await;

        let refusal = create_run(
            State(state.clone()),
            Json(CreateRunRequest {
                prompt: "work on it all night".to_owned(),
                project_id: Some("proj".to_owned()),
                cwd: None,
                mode: "worktree".to_owned(),
                steerable: false,
            }),
        )
        .await;
        let Err(refusal) = refusal else {
            panic!("no cwd was given, so this must fail — the question is only where");
        };

        assert_eq!(
            refusal.0,
            StatusCode::BAD_REQUEST,
            "shadow must not refuse at the mode gate: {}",
            refusal.1
        );
        assert!(
            refusal.1.contains("cwd"),
            "it must be the missing field it complains about, not the mode: {}",
            refusal.1
        );
    }

    /// **A run somebody is WATCHING is not what the project's mode governs, and this is the
    /// narrowing that says so.**
    ///
    /// Same project, same off switch, and it goes through — because `real` runs in the caller's own
    /// checkout with a person in the room. The comment on the kill switch in `create_run` is the
    /// house rule and this is it applied: the scoped kills, the budget and the WIP ceiling pace
    /// proactive autonomy, and a person clicking a button is not that. Without this test the
    /// narrowing is a line of code nothing defends, and the first person to "make it consistent"
    /// takes the shell's default mode away with it.
    #[tokio::test]
    async fn uma_run_vigiada_nao_e_travada_por_um_projecto_desligado() {
        let state = test_state().await;
        project_in_mode(&state.pool, "proj", "off").await;

        let result = create_run(
            State(state.clone()),
            Json(CreateRunRequest {
                prompt: "look at this with me".to_owned(),
                project_id: Some("proj".to_owned()),
                cwd: None,
                mode: "real".to_owned(),
                steerable: false,
            }),
        )
        .await;

        assert!(
            result.is_ok(),
            "an attended run is not the proactive autonomy this brake paces"
        );
    }

    /// A project on the roster, in the mode named. `mode` is NOT NULL with a CHECK, so the three
    /// spellings this takes are the three the column accepts.
    async fn project_in_mode(pool: &sqlx::SqlitePool, project_id: &str, mode: &str) {
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, ?, NULL)
             ON CONFLICT(project_id) DO UPDATE SET mode = excluded.mode",
        )
        .bind(project_id)
        .bind(mode)
        .execute(pool)
        .await
        .expect("put the project on the roster");
    }

    /// A worktree run's row is INSERTed `running` before its worktree is provisioned, and
    /// `git worktree add` takes real time — so the window between the two is wide enough to matter.
    /// A client that disconnects cancels the request it was making, which drops the handler's future
    /// exactly the way `abort()` drops a run's, and everything past that point is simply never done:
    /// no task, no abort handle (so `/cancel` answers 404), and a `running` worktree row that
    /// the concurrency sweep spares, so it goes on holding a slot against every
    /// later worktree run — until the daemon restarts, the only thing that reconciles `running` rows.
    #[tokio::test(flavor = "current_thread")]
    async fn a_dropped_create_request_still_finishes_the_run_it_started() {
        use std::future::Future;

        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-dropped-");
        let state = test_state().await;
        // **The row is here for the door and not for this test's subject.** `create_run` now asks
        // the project's autopilot mode before it does anything, and `project_mode` answers `Off` for
        // a project it has no row for — so a `proj` that was never registered, which is what this
        // test had and never needed, is refused at the door and the handler completes before there
        // is anything to drop. The subject below — what happens when a request is dropped mid-
        // provisioning — is untouched by any of that.
        //
        // `shadow` because it is the least this needs. If the mode rule ever tightens to require
        // `active` for a `worktree` run, this is one of the lines that has to move, and that is the
        // reason it is written out rather than left as a bare INSERT.
        project_in_mode(&state.pool, "proj", "shadow").await;
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
    /// skipping it, and the run holding a concurrency slot the sweep will not take back.
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

    /// A run pausing for approval is not a run that ended, and the queue must not treat it as one.
    #[test]
    fn a_pause_for_approval_does_not_end_the_run() {
        assert!(!ends_the_run("awaiting_approval"));

        assert!(ends_the_run("cancelled"));
        assert!(ends_the_run("failed"));
        assert!(ends_the_run("interrupted"));
        assert!(ends_the_run("timed_out"));
    }

    /// Spec §7's first half, end to end rather than through `cancel_for_run`: a unit test of the
    /// predicate cannot see a wrong argument at the call site, and the call site is what this adds.
    /// A live registration is what makes `finalize_termination` take its `Some(h)` arm at all.
    #[tokio::test]
    async fn a_cancelled_run_loses_the_merge_it_had_not_started() {
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
        let request = crate::vcs::submit(
            &state.pool,
            &crate::vcs::ResolvedRepo::synthetic("proj-1", "C:/repo", "proj-1"),
            &crate::vcs::Op::Merge {
                source: "feat/x".into(),
                target: "master".into(),
            },
            crate::vcs::Origin::Run(id),
        )
        .await
        .unwrap();

        spawn_registered(&state, id, async {
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        assert!(finalize_termination(&state, id, "cancelled").await);

        let status: String = sqlx::query_scalar("SELECT status FROM vcs_requests WHERE id = ?")
            .bind(request)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            status, "cancelled",
            "a merge nobody is left to collect must not stay in the queue"
        );
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

    /// A project runs up to its ceiling and no further.
    ///
    /// This used to assert that the SECOND was busy, because `one_open_worktree_run_per_project`
    /// could only ever say one. It says two now (migration 0053), so the same test measures the same
    /// property at the number the configuration actually holds — and the second succeeding is the
    /// whole point of the change.
    #[tokio::test(flavor = "current_thread")]
    async fn a_project_runs_up_to_its_slot_ceiling_and_the_next_is_busy() {
        let _env_lock = crate::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-runs-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-runs-exclusive-");
        let state = test_state_with(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        advance_run_ids_past(&state.pool, 10_000).await;
        let project_root = repo.to_string_lossy().into_owned();
        // The house ceiling out of the way, so this measures the per-project one.
        sqlx::query("UPDATE autopilot_global SET max_concurrent_slots = 2, max_concurrent_total = 9")
            .execute(&state.pool)
            .await
            .unwrap();

        create_worktree_run(&state, "first", "proj", &project_root)
            .await
            .expect("slot 0");
        create_worktree_run(&state, "second", "proj", &project_root)
            .await
            .expect("slot 1 — impossible before 0053");

        let third = create_run_inner(
            &state,
            "third".into(),
            Some("proj".into()),
            Some(project_root),
            "worktree",
            false,
        )
        .await;

        assert!(matches!(third, Err(CreateRunError::Busy)));
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
        sqlx::query("UPDATE autopilot_global SET max_concurrent_slots = 1")
            .execute(&state.pool)
            .await
            .unwrap();
        let pinned = sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, mode, created_at)
             VALUES ('proj', ?, 'pinned', 'awaiting_approval', 'worktree', ?)",
        )
        .bind(&project_root)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();
        // The seeded row claims nothing by itself — `create_run_inner` is what claims — so the
        // parked run is given the slot it would have been holding. The property under test is that
        // the sweep does NOT take it back while the run is still `awaiting_approval`.
        crate::concurrency::claim(&state.pool, "proj", crate::worktree::Owner::Run(pinned))
            .await
            .expect("the parked run holds the slot");

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

    /// PURE, and the sibling of the test above. The silence deadline was the one actually killing
    /// autonomous runs: a worktree run compiles, one compile is one tool call, and a tool call
    /// streams nothing while it runs. The wall clock was raised twice before anyone noticed that
    /// the deaths were arriving at the 300-second mark and not the 600-second one.
    ///
    /// `email_triage` is again the case worth stating, and for the same reason as above rather
    /// than a new one: it is autonomous and must still be caught quickly.
    #[test]
    fn only_the_long_running_autonomous_modes_get_the_longer_silence() {
        let base = Duration::from_secs(300);

        for mode in ["shadow", "worktree"] {
            assert_eq!(
                progress_timeout_for_mode(base, mode),
                base * crate::state::AUTONOMOUS_PROGRESS_TIMEOUT_MULTIPLIER,
                "{mode} spends single tool calls compiling, and streams nothing while it does"
            );
        }

        for mode in ["real", "plan", crate::email::TRIAGE_MODE] {
            assert_eq!(
                progress_timeout_for_mode(base, mode),
                base,
                "{mode} keeps the interactive silence deadline"
            );
        }
    }

    /// The two deadlines must stay ordered: a silence deadline longer than the wall clock could
    /// never fire, which would silently delete the guard rather than relax it. Asserted against
    /// the production constants, so raising one without the other fails here rather than in the
    /// field at three in the morning.
    #[test]
    fn the_silence_deadline_stays_inside_the_wall_clock_for_an_autonomous_run() {
        let wall = run_timeout_for_mode(crate::state::DEFAULT_RUN_TIMEOUT, "worktree");
        let silence = progress_timeout_for_mode(crate::state::DEFAULT_PROGRESS_TIMEOUT, "worktree");
        assert!(
            silence < wall,
            "an autonomous run may be silent for {silence:?} but only live for {wall:?} — the \
             silence deadline can never fire, so nothing catches a stuck run"
        );
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

    /// The invariant, run rather than asserted: the permission DECISION is `true` only where the
    /// classifier that would replace the barrier is verified present.
    ///
    /// The decision, not the command line — those are two different claims and this test can only
    /// make the first. `shadow` is `plan_only` (see `create_run_inner`), and `plan_only` outranks
    /// this flag in `cli_args`, so a shadow run carries the decision and still launches with
    /// `--permission-mode plan`. That is correct and it is why the pure test
    /// `runner::tests::plan_only_outranks_the_classifier_permission_surface` exists beside this one:
    /// together they cover decision → flag → argument, and `worktree` is the only mode where all
    /// three line up. Confirmed against a live daemon on 2026-07-31, both directions.
    ///
    /// Three cases, and the second and third are the ones that matter. The same unattended run in a
    /// tree whose `PreToolUse` entry names somebody else's script gets nothing — "a hook exists" is
    /// not the property. And an interactive run in the fully wired tree gets nothing either, because
    /// there is a person there who can approve, and this must not decide for them.
    ///
    /// Mutation-checked: dropping the `runs_unattended` guard fails the third case, dropping the
    /// hook lookup fails the second.
    #[tokio::test]
    async fn only_a_verified_classifier_earns_the_permission_decision() {
        let (state, runner) = test_state_with_runner(None, Duration::from_secs(30)).await;

        let wired = space_free_tempdir("nucleos-wired-");
        wire_classifier_hook(wired.path());
        let unwired = space_free_tempdir("nucleos-unwired-");
        wire_someone_elses_hook(unwired.path());

        for (label, dir, mode, expected) in [
            ("unattended, classifier wired", wired.path(), "shadow", true),
            (
                "unattended, PreToolUse names another script",
                unwired.path(),
                "shadow",
                false,
            ),
            (
                "interactive, classifier wired",
                wired.path(),
                "real",
                false,
            ),
        ] {
            *runner.last_classifier_governs_tools.lock().unwrap() = None;
            let id = create_run_inner(
                &state,
                "work".into(),
                None,
                Some(dir.to_string_lossy().into_owned()),
                mode,
                false,
            )
            .await
            .unwrap();
            let (status, _) = poll_run(&state, id, "completed").await;
            assert_eq!(status, "completed", "{label}: the run must reach the runner");

            assert_eq!(
                *runner.last_classifier_governs_tools.lock().unwrap(),
                Some(expected),
                "{label}"
            );
        }
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
            cache_creation_tokens: None,
            num_turns: None,
            compacted: false,
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

    /// The other half of spec §7's "the agent that submitted dies", and the one nothing routes
    /// through `finalize_termination`: a run its wall clock killed is as gone as one a human
    /// cancelled, and it will never come back to collect the merge it asked for.
    ///
    /// Four events 200ms apart against a 600ms wall clock, rather than this file's usual tighter
    /// numbers, because the request has to be submitted while the run is still alive — the assert
    /// on `running` below is what turns a lost race into a legible failure instead of a confusing
    /// `awaiting_approval`.
    #[tokio::test]
    async fn a_run_the_wall_clock_kills_loses_the_merge_it_had_queued() {
        let (mut state, runner) =
            test_state_with_runner(Some(Duration::from_millis(200)), Duration::from_millis(600))
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
            cache_creation_tokens: None,
            num_turns: None,
            compacted: false,
        });
        let pool = state.pool.clone();
        let app = test_router(state);
        let created = create_run_via_http(&app, "a run the wall clock will cut short").await;

        let request = crate::vcs::submit(
            &pool,
            &crate::vcs::ResolvedRepo::synthetic("proj-1", "C:/repo", "proj-1"),
            &crate::vcs::Op::Merge {
                source: "feat/x".into(),
                target: "master".into(),
            },
            crate::vcs::Origin::Run(created.id),
        )
        .await
        .unwrap();
        assert_eq!(
            get_run_status(&app, created.id).await.status,
            "running",
            "the request must be queued while the run is still alive, or this proves nothing"
        );

        let mut status = String::new();
        for _ in 0..100 {
            status = get_run_status(&app, created.id).await.status;
            if status == "timed_out" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(status, "timed_out", "the wall clock must terminate this run");

        let request_status: String =
            sqlx::query_scalar("SELECT status FROM vcs_requests WHERE id = ?")
                .bind(request)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            request_status, "cancelled",
            "a run killed by its wall clock leaves nobody to collect the merge it queued"
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

    // One project per run: a stranded pause is exactly what holds a concurrency slot, and these
    // tests need each one attributable to the project whose ceiling it would narrow.
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
                live: false,
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

    #[tokio::test]
    async fn transcript_search_finds_a_run_by_its_trajectory() {
        let pool = search_test_pool().await;
        let run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('project-trajectory', 'prompt without the needle', 'completed', 'real',
                     '2026-07-30T10:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO run_events (run_id, seq, kind, payload, created_at)
             VALUES (?, 1, 'assistant',
                     '{\"type\":\"assistant\",\"text\":\"located trajectory-needle in output\"}',
                     '2026-07-30T10:00:01Z')",
        )
        .bind(run_id)
        .execute(&pool)
        .await
        .unwrap();

        let entries = search(
            &pool,
            &SearchFilter {
                project_id: None,
                status: None,
                mode: None,
                q: Some("trajectory-needle".into()),
                since: None,
                until: None,
                limit: 50,
                live: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, run_id);
    }

    #[tokio::test]
    async fn transcript_search_returns_no_transcript_text() {
        let pool = search_test_pool().await;
        let transcript_text = "private-transcript-payload";
        let run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('project-private', 'ordinary prompt', 'completed', 'real',
                     '2026-07-30T11:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO run_events (run_id, seq, kind, payload, created_at)
             VALUES (?, 1, 'assistant', ?, '2026-07-30T11:00:01Z')",
        )
        .bind(run_id)
        .bind(transcript_text)
        .execute(&pool)
        .await
        .unwrap();

        let entries = search(
            &pool,
            &SearchFilter {
                project_id: None,
                status: None,
                mode: None,
                q: Some(transcript_text.into()),
                since: None,
                until: None,
                limit: 50,
                live: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.id, run_id);
        assert!(!entry.prompt_excerpt.contains(transcript_text));

        let json = serde_json::to_value(entry).unwrap();
        let object = json.as_object().unwrap();
        assert_eq!(object.len(), 8);
        for field in [
            "id",
            "project_id",
            "status",
            "mode",
            "created_at",
            "completed_at",
            "cost_usd",
            "prompt_excerpt",
        ] {
            assert!(object.contains_key(field), "missing metadata field {field}");
        }
        for forbidden in ["transcript", "payload", "stdout", "stderr"] {
            assert!(
                !object.contains_key(forbidden),
                "search result leaked {forbidden}"
            );
        }
    }

    #[tokio::test]
    async fn transcript_search_matches_an_fts_operator_literally() {
        let pool = search_test_pool().await;
        let run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('project-force', 'prepare repository update', 'completed', 'real',
                     '2026-07-30T12:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO run_events (run_id, seq, kind, payload, created_at)
             VALUES (?, 1, 'tool_result', 'git push --force',
                     '2026-07-30T12:00:01Z')",
        )
        .bind(run_id)
        .execute(&pool)
        .await
        .unwrap();

        let entries = search(
            &pool,
            &SearchFilter {
                project_id: None,
                status: None,
                mode: None,
                q: Some("--force".into()),
                since: None,
                until: None,
                limit: 50,
                live: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, run_id);
    }

    #[tokio::test]
    async fn transcript_search_excludes_a_match_outside_the_project() {
        let pool = search_test_pool().await;
        let included_run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('project-included', 'first unrelated prompt', 'completed', 'real',
                     '2026-07-30T13:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let excluded_run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('project-excluded', 'second unrelated prompt', 'completed', 'real',
                     '2026-07-30T13:01:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        for run_id in [included_run_id, excluded_run_id] {
            sqlx::query(
                "INSERT INTO run_events (run_id, seq, kind, payload, created_at)
                 VALUES (?, 1, 'assistant', 'shared-project-needle',
                         '2026-07-30T13:02:00Z')",
            )
            .bind(run_id)
            .execute(&pool)
            .await
            .unwrap();
        }

        let entries = search(
            &pool,
            &SearchFilter {
                project_id: Some("project-included".into()),
                status: None,
                mode: None,
                q: Some("shared-project-needle".into()),
                since: None,
                until: None,
                limit: 50,
                live: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![included_run_id]
        );
    }

    #[tokio::test]
    async fn prompt_search_still_matches_the_prompt() {
        let pool = search_test_pool().await;
        let run_id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('project-prompt', 'prompt contains prompt-regression-needle', 'completed',
                     'real', '2026-07-30T14:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let entries = search(
            &pool,
            &SearchFilter {
                project_id: None,
                status: None,
                mode: None,
                q: Some("prompt-regression-needle".into()),
                since: None,
                until: None,
                limit: 50,
                live: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, run_id);
    }

    /// What `get_runs` assembles when nobody asked for anything: `parse_search_limit` returns 50 by
    /// default.
    fn base_filter() -> SearchFilter {
        SearchFilter {
            project_id: None,
            status: None,
            mode: None,
            q: None,
            since: None,
            until: None,
            limit: 50,
            live: false,
        }
    }

    /// A run parked on an approval holds both its slot and its worktree, and is exempt from the
    /// retention clock — by design it is what sits there longest. It is also the first thing the
    /// default window of 50 hides, which would make *detail unavailable* the normal state of its
    /// card.
    #[tokio::test]
    async fn the_live_filter_reaches_a_parked_run_the_default_window_would_hide() {
        let pool = search_test_pool().await;
        let parked = insert_search_run(
            &pool,
            "project-a",
            "awaiting_approval",
            "worktree",
            "parked",
            "2026-01-01T00:00:00Z",
        )
        .await;
        for index in 0..60 {
            insert_search_run(
                &pool,
                "project-b",
                "completed",
                "worktree",
                "noise",
                &format!("2026-08-0{}T00:00:0{}Z", 1 + index / 10, index % 10),
            )
            .await;
        }

        let default = search(&pool, &base_filter()).await.unwrap();
        assert!(
            !default.iter().any(|row| row.id == parked),
            "the setup did not push the parked run out of the window"
        );

        let live = search(
            &pool,
            &SearchFilter {
                live: true,
                ..base_filter()
            },
        )
        .await
        .unwrap();
        assert!(live.iter().any(|row| row.id == parked));
    }

    /// Both statuses, and only those. A finished run has given its slot back.
    #[tokio::test]
    async fn the_live_filter_carries_both_slot_holding_statuses_and_nothing_else() {
        let pool = search_test_pool().await;
        let at = "2026-08-09T00:00:00Z";
        let running = insert_search_run(&pool, "project-a", "running", "worktree", "a", at).await;
        let parked =
            insert_search_run(&pool, "project-a", "awaiting_approval", "worktree", "b", at).await;
        insert_search_run(&pool, "project-a", "completed", "worktree", "c", at).await;
        insert_search_run(&pool, "project-a", "interrupted", "worktree", "d", at).await;

        let live = search(
            &pool,
            &SearchFilter {
                live: true,
                ..base_filter()
            },
        )
        .await
        .unwrap();

        let ids: std::collections::HashSet<i64> = live.iter().map(|row| row.id).collect();
        assert_eq!(ids, std::collections::HashSet::from([running, parked]));
    }
}
