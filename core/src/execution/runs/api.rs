//! The `/runs` request and response shapes, search, and the create/get/tail handlers.

use super::*;

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
    /// What this run may do without stopping to ask, when there is nobody to ask.
    ///
    /// Absent means what it has always meant: the column stays NULL, `pretooluse_decision`
    /// reads NULL as `Auto`, and a call the classifier will not decide parks the run for a
    /// person. The one value that changes anything today is `dont_ask`, and what it changes is
    /// a PARK into a REFUSAL — the run keeps going and does the rest of the work.
    ///
    /// It is a tightening and never a widening, which is why this field is safe to expose on a
    /// route that starts unattended work. `dont_ask` permits precisely what `auto` permits
    /// (`allowed_at`) and refuses everything `auto` would have stopped to ask about; there is no
    /// spelling here that lets a run do something it could not do before.
    ///
    /// A spelling outside the six fails the whole request rather than falling back to `auto`,
    /// for the reason the two chat routes give: the READER is lenient so that rows written
    /// before the column existed still parse, and applying that leniency to a REQUEST would let
    /// a typo quietly widen what an unattended run may do.
    #[serde(default)]
    pub permission_mode: Option<crate::chats::PermissionMode>,
}

pub(super) fn default_run_mode() -> String {
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
/// A row `group_by_chat` folded stands for a whole conversation: `cost_usd` is the sum over its
/// matching turns and every other column is its latest matching turn's.
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
    // The same nine route columns `RunStatusResponse` answers for one run, under the same names and
    // with the same meaning: NULL is "not recorded", and each is always serialised so the list reads
    // an explicit `null` rather than a gap.
    pub model: Option<String>,
    pub effort: Option<String>,
    pub runner: Option<String>,
    pub route_mode: Option<String>,
    pub route_decision_id: Option<String>,
    pub advised_runner: Option<String>,
    pub advised_model: Option<String>,
    pub advised_effort: Option<String>,
    /// A JSON array of `model[@effort]`, as TEXT: handed over as the column holds it, not parsed.
    pub route_failed: Option<String>,
    /// The conversation this run is a turn of; `None` for every non-chat run.
    pub chat_id: Option<String>,
    /// The conversation whose turn asked for this run through `POST /runs` (0195); `None` for a run
    /// nobody started from a chat. Distinct from `chat_id`: this run is not a turn of it.
    pub launched_by_chat_id: Option<String>,
    /// That conversation's title, read at listing time so a rename shows at once.
    pub launched_by_chat_title: Option<String>,
    /// The conversation's title. Only on a row `group_by_chat` folded; null elsewhere.
    pub chat_title: Option<String>,
    /// How many matching turns the folded conversation has. Only on a folded row; null elsewhere.
    pub turns: Option<i64>,
    /// How many of those turns are still running. Only on a folded row; null elsewhere.
    pub running_turns: Option<i64>,
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
    /// One conversation's turns only.
    pub chat_id: Option<String>,
    /// Fold every chat turn into one row per conversation (`GET /runs?group=chat`).
    pub group_by_chat: bool,
    /// Leave chat turns out entirely (`GET /runs?chat_turns=hide`). The runs a chat LAUNCHED stay:
    /// they are not turns (decision of 2026-10-11).
    pub hide_chat_turns: bool,
}

/// Keep search results useful without turning the index into a prompt or output retrieval endpoint.
pub(super) const PROMPT_EXCERPT_CHARS: i64 = 500;

pub async fn list_awaiting_approval(pool: &sqlx::SqlitePool) -> sqlx::Result<Vec<AwaitingRun>> {
    sqlx::query_as::<_, AwaitingRun>(
        "SELECT id, project_id, prompt, cwd, created_at
         FROM runs WHERE status = 'awaiting_approval' ORDER BY id",
    )
    .fetch_all(pool)
    .await
}

/// The tail of the grouped shape: folds every chat turn of the filtered set into one row per
/// conversation, and passes every other run through as it is. Both arms carry the same 23 columns.
pub(super) const GROUPED_BY_CHAT: &str = "), g AS (SELECT chat_id AS g_chat_id, COUNT(*) AS g_turns,      SUM(status = 'running') AS g_running, SUM(cost_usd) AS g_cost, MAX(id) AS g_last      FROM f WHERE mode = 'assistant' AND chat_id IS NOT NULL GROUP BY chat_id)      SELECT * FROM (      SELECT f.*, NULL AS chat_title, NULL AS turns, NULL AS running_turns FROM f      WHERE NOT (f.mode = 'assistant' AND f.chat_id IS NOT NULL)      UNION ALL      SELECT f.id, f.project_id, f.status, f.mode, f.created_at, f.completed_at,      g.g_cost AS cost_usd, f.prompt_excerpt, f.model, f.effort, f.runner, f.route_mode,      f.route_decision_id, f.advised_runner, f.advised_model, f.advised_effort,      f.route_failed, f.chat_id, f.launched_by_chat_id, f.launched_by_chat_title,      (SELECT title FROM chats WHERE chats.chat_id = g.g_chat_id) AS chat_title,      g.g_turns AS turns, g.g_running AS running_turns      FROM g JOIN f ON f.id = g.g_last)";

/// Searches run metadata newest-first. The result is an index, so it never returns stdout or stderr.
/// With `group_by_chat`, every chat turn folds into one row per conversation.
pub async fn search(
    pool: &sqlx::SqlitePool,
    filter: &SearchFilter,
) -> sqlx::Result<Vec<RunSearchResult>> {
    let columns = format!(
        "id, project_id, status, mode, created_at, completed_at, cost_usd,          substr(prompt, 1, {PROMPT_EXCERPT_CHARS}) AS prompt_excerpt,          model, effort, runner, route_mode, route_decision_id,          advised_runner, advised_model, advised_effort, route_failed, chat_id,          launched_by_chat_id,          (SELECT title FROM chats WHERE chats.chat_id = runs.launched_by_chat_id)          AS launched_by_chat_title"
    );
    let mut query = if filter.group_by_chat {
        sqlx::QueryBuilder::<sqlx::Sqlite>::new(format!(
            "WITH f AS (SELECT {columns} FROM runs WHERE 1 = 1"
        ))
    } else {
        sqlx::QueryBuilder::<sqlx::Sqlite>::new(format!(
            "SELECT {columns}, NULL AS chat_title, NULL AS turns, NULL AS running_turns              FROM runs WHERE 1 = 1"
        ))
    };
    push_search_filters(&mut query, filter);
    if filter.group_by_chat {
        query.push(GROUPED_BY_CHAT);
    }
    query
        .push(" ORDER BY created_at DESC, id DESC LIMIT ")
        .push_bind(filter.limit);

    query
        .build_query_as::<RunSearchResult>()
        .fetch_all(pool)
        .await
}

/// Every filter `search` honours, appended to a query that ends in `WHERE 1 = 1`. In the grouped
/// shape they land inside the CTE, so they choose TURNS and the grouping counts what they chose.
pub(super) fn push_search_filters(
    query: &mut sqlx::QueryBuilder<sqlx::Sqlite>,
    filter: &SearchFilter,
) {
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
        for status in crate::execution::concurrency::LIVE_RUN_STATUSES {
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
    if let Some(chat_id) = &filter.chat_id {
        query.push(" AND chat_id = ").push_bind(chat_id);
    }
    // Every `assistant` row is a conversation turn, Telegram's and the pre-0045 ones with no
    // `chat_id` included, so the mode alone is the test.
    if filter.hide_chat_turns {
        query.push(" AND mode <> 'assistant'");
    }
}

#[derive(Debug)]
pub enum CreateRunError {
    Invalid(&'static str),
    /// The project's slot, or the house's, is taken: another run holds what this one needed.
    Busy,
    /// The volume the checkout would live on is below the free-space floor. The sentence says how
    /// much room there was and against which floor, and is the one `worktree_provision_failed` also
    /// carries.
    ///
    /// Its own variant rather than a `Busy`, and not because anything retries it differently —
    /// every caller still treats it as a condition that passes rather than a failure. On 2026-09-14
    /// a job refused its item this way read `status waiting, wait_reason slot` and fed `another run
    /// holds the project's worktree slot`; whoever reads that goes looking for the run holding it,
    /// finds none, and never learns the disk is full. The two refusals ask for different hands.
    NoRoomOnDisk(String),
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
    /// Spec B D2/D12: only the action was to be refused, and the run is a job's node. Inside a
    /// job every ending already has an owner — the job's policy — and a decline would go over it.
    BelongsToAJob,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ResumeError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

pub fn create_run_status(error: &CreateRunError) -> StatusCode {
    match error {
        CreateRunError::Invalid(_) => StatusCode::BAD_REQUEST,
        // The same 409 as a held slot: both are conditions that pass, and a caller that retries
        // on one should retry on the other. What tells them apart is the sentence below.
        CreateRunError::Busy | CreateRunError::NoRoomOnDisk(_) => StatusCode::CONFLICT,
        CreateRunError::Worktree(_) | CreateRunError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// What the caller is told, beside the status `create_run_status` gives.
///
/// Written here rather than in the route for the reason the function above it is: two places
/// deciding what one refusal is CALLED would drift, and a status and a sentence that disagree is
/// worse than either alone.
///
/// **`Invalid`'s and `NoRoomOnDisk`'s own words travel, and the internal errors' do not**, and that
/// split is the whole of this function. `Invalid` is a `&'static str` this codebase wrote about the
/// request — "worktree mode requires project_id and cwd" — and it was being thrown away, so a caller
/// got a bare 400 for a mistake it could have fixed in a second. `NoRoomOnDisk` is about the machine,
/// not the daemon: how much room there is and how much a checkout asks for. A `sqlx::Error` and an
/// `io::Error` are about the inside of this daemon: they go to the log, where whoever can act on them
/// is reading, and the caller gets the fact rather than the internals.
pub fn create_run_reason(error: &CreateRunError) -> String {
    match error {
        CreateRunError::Invalid(reason) => (*reason).to_owned(),
        CreateRunError::Busy => {
            "this project has no free slot right now, so nothing was started".to_owned()
        }
        // Its words travel, like `Invalid`'s, because they are about the machine rather than the
        // daemon's internals — how much room there is and how much a checkout asks for — and a
        // caller told "no free slot" instead goes looking for a run that is not there.
        CreateRunError::NoRoomOnDisk(refusal) => {
            format!("the disk is too full for another checkout, so nothing was started: {refusal}")
        }
        CreateRunError::Worktree(_) => {
            "the run's checkout could not be provisioned; the daemon logged why".to_owned()
        }
        CreateRunError::Db(_) => "the run could not be recorded; the daemon logged why".to_owned(),
    }
}

impl std::fmt::Display for CreateRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::Busy => formatter.write_str("run creation is busy"),
            Self::NoRoomOnDisk(refusal) => formatter.write_str(refusal),
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
            Self::Invalid(_) | Self::Busy | Self::NoRoomOnDisk(_) => None,
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
    /// The conversation this run is a turn of, so a client can link back to it. `None` outside a chat.
    pub chat_id: Option<String>,
    /// How much of this run's prompt this daemon wrote itself, as an estimated token count.
    ///
    /// The MCP tool schemas, `--append-system-prompt`, the `--agents` JSON and the prompt — the four
    /// things `runner::authored_prompt` prices off the argument vector. `null` for a run this daemon
    /// did not author a prompt for, and for every run launched before the column existed; neither is
    /// a run that wrote nothing, which is why this is not defaulted to zero anywhere on the way out.
    ///
    /// `estimate` and not `tokens` in the name, and the label on screen says the same. Four
    /// characters to the token — see `prompt_budget`, which holds the ruler and the argument for it.
    #[sqlx(default)]
    pub authored_prompt_estimate: Option<i64>,
    /// Everything else in the prompt, as an estimate: the CLI's own.
    ///
    /// The whole reported prompt minus the part above. **One residual and never a breakdown** — the
    /// CLI's system prompt, its built-in tool definitions, whatever it loaded from CLAUDE.md, the
    /// by-name tool listing a deferring run gets instead of schemas, and the conversation itself are
    /// all in here, undivided, because nothing in the stream separates them and this daemon does not
    /// send the one people ask about. `prompt_budget`'s header is where that refusal is argued; this
    /// field is where it shows.
    ///
    /// `null` whenever either side is unknown, which includes every run that reported no usage at
    /// all. A zero here means the estimate above met or exceeded the whole reported prompt.
    ///
    /// **What dominates that is DEFERRAL, and this doc used to blame the wrong thing.** It said a
    /// zero "happens on a short run because chars/4 overshoots on schema JSON", which is not the
    /// cause on the ordinary case: a chat turn is `ToolPolicy::Unrestricted`, the CLI keeps
    /// `ToolSearch` and advertises MCP tools by name, and the schema JSON is not in the prompt at all
    /// — so charging it here subtracted roughly 10,250 estimated tokens that were never paid, and
    /// understated this residual by exactly that. `runner::authored_prompt` now charges the schemas
    /// only when they are shipped, and holds the rule and the measurements. Nor does the old
    /// sentence survive being checked in the SHIPPED regime, where it would at least be on topic:
    /// measured there, chars/4 put the schema block at 10,250 against ~11,160 attributable, so it
    /// runs about 8% LOW rather than high. A residual can still reach zero on a genuinely short run,
    /// where the prompt this daemon authored is most of what the model read — but the ruler's few
    /// percent is not the reason, and deferral is the mechanism that used to force it.
    #[sqlx(default)]
    pub cli_own_estimate: Option<i64>,
    /// The stored character count, read only so the estimate above can be derived from it.
    ///
    /// Not serialised: the wire contract is estimates, and offering characters beside them would
    /// invite a second reader to divide by four somewhere else.
    #[serde(skip)]
    #[sqlx(default)]
    pub authored_prompt_chars: Option<i64>,
    /// Read only to complete the reported prompt total, which is input + cache reads + cache
    /// creation and not the first of the three — `token_efficiency::Measures::total_prompt_tokens`
    /// makes the same argument: "any ratio taken from `input_tokens` alone is wrong by omission".
    ///
    /// Not serialised, because adding a fourth token count to this response is a separate decision
    /// from computing a residual with it.
    #[serde(skip)]
    #[sqlx(default)]
    pub cache_creation_tokens: Option<i64>,
    // What the run was launched on and what the local llm-router advised for it, exactly as
    // `route_advice::resolve` wrote them (migration 0151). Flat and nullable, every one: NULL is
    // "not recorded" — routing off, a triage run, a handoff successor, a run older than the column —
    // never "none". Always serialised, so the shell reads an explicit `null` rather than a gap.
    pub model: Option<String>,
    pub effort: Option<String>,
    pub runner: Option<String>,
    pub route_mode: Option<String>,
    pub route_decision_id: Option<String>,
    pub advised_runner: Option<String>,
    pub advised_model: Option<String>,
    pub advised_effort: Option<String>,
    /// A JSON array of `model[@effort]`, as TEXT: handed over as the column holds it, not parsed.
    pub route_failed: Option<String>,
}

impl RunStatusResponse {
    /// The whole prompt this run reported, or `None` if any of its three parts is missing.
    ///
    /// Deliberately the same shape as `token_efficiency::Measures::total_prompt_tokens`, which is
    /// private to that module and belongs to a different feature. Any of the three being unknown
    /// makes the total unknown rather than smaller — a run that reported two of three and had the
    /// third counted as zero would show a residual that is too small by exactly the missing part.
    pub(super) fn reported_prompt_tokens(&self) -> Option<i64> {
        let input = self.input_tokens?;
        let read = self.cache_read_tokens?;
        let created = self.cache_creation_tokens?;
        input.checked_add(read)?.checked_add(created)
    }

    /// Fills the two derived fields from the columns just read.
    ///
    /// Derived on the way out rather than stored: the residual is the difference between something
    /// written at launch and something written at the end, so a stored copy would be wrong for the
    /// whole life of a running run and would have to be rewritten to stop being.
    pub(super) fn with_prompt_budget(mut self) -> Self {
        self.authored_prompt_estimate = self
            .authored_prompt_chars
            .map(crate::prompt_budget::estimate_from_chars);
        self.cli_own_estimate = crate::prompt_budget::residual_estimate(
            self.reported_prompt_tokens(),
            self.authored_prompt_estimate,
        );
        self
    }
}

/// `POST /runs` as the router mounts it: `create_run`, then the tag naming the chat that asked.
pub async fn create_run_route(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CreateRunRequest>,
) -> Result<Json<CreateRunResponse>, (StatusCode, String)> {
    let pool = state.pool.clone();
    let created = create_run(State(state), Json(req)).await?;
    tag_launching_chat(&pool, created.0.id, &headers).await;
    Ok(created)
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
    match crate::governance::autopilot::kill_switch_engaged(&state.pool).await {
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
    // it.** `grep -n 'autopilot::Mode' core/src/execution/runs.rs` returned nothing, while `POST /jobs` has
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
        match crate::governance::autopilot::project_mode(&state.pool, project).await {
            Ok(crate::governance::autopilot::Mode::Off) => {
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
    let id = crate::door::uncancellable(async move {
        // `create_run_with` rather than `create_run_inner`, which is the same funnel with this
        // one argument fixed at `None`. The three callers outside this module (the scheduler,
        // the repo trigger, the triage loop) go on using the wrapper and go on getting NULL,
        // which is what every run they have ever made carried.
        create_run_with(
            &state,
            NewRun {
                prompt: req.prompt,
                project_id: req.project_id,
                cwd: req.cwd,
                mode: &req.mode,
                steerable: req.steerable,
                provisioning: None,
                permission_mode: req.permission_mode,
            },
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
            crate::execution::runs::create_run_status(&error),
            crate::execution::runs::create_run_reason(&error),
        )
    })?;

    Ok(Json(CreateRunResponse { id }))
}

/// Tags run `id` with the conversation that asked for it, when the request came from a chat turn.
///
/// The speaking run is `RUN_ID_HEADER`, which the MCP server of every chat turn sends from the
/// `NUCLEOS_RUN_ID` the daemon gave it; a header naming a run that is not a conversation turn tags
/// nothing. Written after the INSERT and best effort, because it is a label for the Runs list and
/// decides nothing: a run created and left untagged is a run, and refusing one over its label
/// would be the wrong trade.
pub(super) async fn tag_launching_chat(
    pool: &sqlx::SqlitePool,
    id: i64,
    headers: &axum::http::HeaderMap,
) {
    let Some(speaking) = headers
        .get(crate::daemon_client::RUN_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
    else {
        return;
    };
    let tagged = sqlx::query(
        "UPDATE runs SET launched_by_chat_id =
                (SELECT chat_id FROM runs WHERE id = ? AND mode = 'assistant')
          WHERE id = ?",
    )
    .bind(speaking)
    .bind(id)
    .execute(pool)
    .await;
    if let Err(error) = tagged {
        tracing::warn!(%error, id, speaking, "the run was created but not tagged with the chat that asked for it");
    }
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
        // `cache_creation_tokens` and `authored_prompt_chars` are read and not answered: they are
        // the two inputs `with_prompt_budget` needs to derive the pair of estimates this response
        // does answer. Selecting them here rather than in a second query keeps the residual and the
        // numbers it was computed from as one read of one row.
        "SELECT id, project_id, status, gate_status, gate_exit_code, gate_output, exit_code, stdout,
                stderr, session_id, cost_usd, input_tokens, output_tokens, cache_read_tokens,
                num_turns, context_fill, steerable, successor_run_id, chat_id, cache_creation_tokens,
                authored_prompt_chars, model, effort, runner, route_mode, route_decision_id,
                advised_runner, advised_model, advised_effort, route_failed
         FROM runs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(run.with_prompt_budget()))
}
