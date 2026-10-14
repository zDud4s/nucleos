//! Runs, the vcs queue, worktrees and the feed (`/runs/*`, `/vcs/*`, `/worktrees`, `/feed`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/feed", get(get_feed))
        // Beside `/feed` and not a parameter on it: `/feed` answers the Telegram sidecar's newest-50
        // listing, and a window of a day oldest-first is a different question with a different
        // shape. Folding it in would put that listing one query-string typo away from a 5000-row
        // answer.
        .route("/feed/timeline", get(get_feed_timeline))
        .route("/feed/seen", get(get_feed_seen).post(post_feed_seen))
        .route("/runs", get(get_runs).post(runs::create_run_route))
        // The literal path coexists with `/runs/{id}`; static segments win in matchit.
        .route("/runs/awaiting-approval", get(list_awaiting_approval_runs))
        .route("/runs/{id}", get(get_run))
        // Beside the run it belongs to. Reads no table: the tail lives in `AppState`, because
        // `run_events` is not written until the run ends and there is nothing durable to read while
        // the thing is actually happening.
        .route("/runs/{id}/tail", get(crate::execution::runs::get_run_tail))
        // Why it stopped, not how it is doing: reads the run row plus, for a `gate` or `timeout`
        // kind only, the `shadow_decisions` leading up to it (spec
        // `.ai/specs/2026-08-29-porque-parou-design.md` §4, §7). Read-only front to back — see
        // `run_stop.rs`.
        .route("/runs/{id}/stop", get(crate::execution::runs::get_run_stop))
        .route("/runs/{id}/knowledge", get(get_run_knowledge))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route(
            "/runs/{id}/message",
            post(post_run_message).delete(delete_run_message),
        )
        .route(
            "/vcs/requests",
            post(submit_vcs_request).get(list_vcs_requests),
        )
        // Beside `/vcs/requests` because it admits one, and apart from it because the caller knows
        // something different: a worktree knows where it is standing and nothing else, while
        // `/vcs/requests` is for a caller that already names a project and an operation.
        .route("/vcs/land", post(land_worktree))
        // Two spellings of one read, separated only by how long the caller is willing to hold the
        // line. `/wait` blocks up to `vcs::DEFAULT_WAIT`; the bare route is the same read with a
        // zero deadline, which `wait_for` answers from its first look at the row.
        .route("/vcs/requests/{id}", get(get_vcs_request))
        .route("/vcs/requests/{id}/wait", get(wait_vcs_request))
        .route("/vcs/requests/{id}/dismiss", post(post_vcs_request_dismiss))
        .route("/vcs/requests/{id}/approve", post(post_vcs_request_approve))
        .route("/vcs/requests/{id}/refuse", post(post_vcs_request_refuse))
        // The catalogue `POST /projects/{id}/git-ops` validates against, read out loud — the git
        // twin of the route above.
        //
        // **No project id, for the same reason and more completely.** `declarable_github_ops` still
        // intersects two compiled ceilings against a bigger catalogue; `vcs::GIT_OP_KINDS` IS the
        // catalogue, so `vcs::declarable_git_ops` has nothing project-shaped to consult at all — the
        // answer is compiled in, identical for every project on the roster, today and until this
        // binary changes.
        //
        // **In no scope table, for `/github/declarable-ops`' reason:** the response is a function of
        // the binary and discloses no project's configuration, so the grant would buy nobody
        // anything — the caller who needs it is the shell, which already holds the control token,
        // and the three routes it exists to explain are themselves in no table.
        // `the_declarable_ops_catalogue_is_in_no_scope_table` covers both routes together.
        .route("/vcs/declarable-ops", get(get_declarable_git_ops))
        .route("/worktrees/{run_id}/release", post(post_worktree_release))
        .route("/runs/{id}/judge-verdicts", get(get_judge_opinions_of_run))
}

#[derive(Deserialize)]
pub(super) struct FeedQuery {
    pub(super) project_id: Option<String>,
    pub(super) scope: Option<String>,
    pub(super) q: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) since: Option<String>,
    pub(super) until: Option<String>,
    pub(super) limit: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct FeedTimelineQuery {
    /// Required, and an `Option` only so a missing one is this handler's 400 rather than the
    /// extractor's — the same answer, reached through the one place that explains it.
    pub(super) since: Option<String>,
    pub(super) until: Option<String>,
    pub(super) after_id: Option<i64>,
}

#[derive(Deserialize)]
pub(super) struct FeedSeenRequest {
    pub(super) through: i64,
}

#[derive(Deserialize)]
pub(super) struct RunsQuery {
    pub(super) project_id: Option<String>,
    pub(super) status: Option<String>,
    pub(super) mode: Option<String>,
    pub(super) q: Option<String>,
    pub(super) since: Option<String>,
    pub(super) until: Option<String>,
    pub(super) limit: Option<String>,
    /// Only the runs still holding a slot. `status` takes one exact value and a slot-holding run is
    /// `running` *or* `awaiting_approval`, so this is not something the existing filter can express.
    ///
    /// Absent, the answer is byte for byte today's — which is what leaves the Runs tab as it is.
    pub(super) live: Option<bool>,
    /// Only one conversation's turns.
    pub(super) chat_id: Option<String>,
    /// `chat` folds every chat turn into one row per conversation; anything else is a 400.
    pub(super) group: Option<String>,
    /// `hide` leaves conversation turns out; `show`, or absent, keeps them. Anything else is a 400.
    pub(super) chat_turns: Option<String>,
}

/// Every operation the shared git queue performs, and whether a project may declare it.
///
/// **The git twin of `get_declarable_github_ops`, and simpler by exactly the gap `vcs::DeclarableGitOp`'s
/// own doc names.** The GitHub catalogue has to serve operations the ceilings refuse, so a caller can
/// draw them as facts rather than as controls; the git queue has no ceiling narrower than
/// `GIT_OP_KINDS` to intersect, so `declarable_git_ops` returns everything it performs and every row
/// comes back `declarable: true`. Nothing here computes a `half` either — every operation in this
/// family is a write the queue performs the same way, never a read in force.
///
/// No project id, for the reason the route comment gives: the answer is compiled into `GIT_OP_KINDS`
/// and is the same for every project on the roster.
pub(super) async fn get_declarable_git_ops() -> Json<Vec<vcs::DeclarableGitOp>> {
    Json(vcs::declarable_git_ops())
}

/// The search endpoints never return more than this many rows, even when a caller requests more.
pub(super) const SEARCH_LIMIT_MAX: i64 = 200;

pub(super) fn parse_time_bound(
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

pub(super) fn parse_search_limit(value: Option<String>) -> Result<i64, StatusCode> {
    match value {
        Some(value) => value
            .parse::<i64>()
            .map(|limit| limit.clamp(1, SEARCH_LIMIT_MAX))
            .map_err(|_| StatusCode::BAD_REQUEST),
        None => Ok(50),
    }
}

pub(super) async fn get_feed(
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

pub(super) async fn get_feed_timeline(
    State(state): State<AppState>,
    Query(query): Query<FeedTimelineQuery>,
) -> Result<Json<feed::Timeline>, StatusCode> {
    let since = parse_time_bound(query.since)?.ok_or(StatusCode::BAD_REQUEST)?;
    let until = parse_time_bound(query.until)?;
    feed::timeline_window(since, until, chrono::Utc::now()).map_err(|_| StatusCode::BAD_REQUEST)?;
    feed::timeline(
        &state.pool,
        since,
        until,
        query.after_id,
        feed::TIMELINE_MAX,
    )
    .await
    .map(Json)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(super) async fn get_feed_seen(
    State(state): State<AppState>,
) -> Result<Json<feed::Seen>, StatusCode> {
    feed::seen(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(super) async fn post_feed_seen(
    State(state): State<AppState>,
    Json(body): Json<FeedSeenRequest>,
) -> Result<Json<feed::Seen>, StatusCode> {
    // Refused rather than clamped to zero: no line has a negative id, so a caller sending one has
    // mixed something up, and storing the nearest legal value would hide that from it.
    if body.through < 0 {
        return Err(StatusCode::BAD_REQUEST);
    }
    feed::mark_seen(&state.pool, body.through, chrono::Utc::now())
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub(super) async fn get_runs(
    State(state): State<AppState>,
    Query(query): Query<RunsQuery>,
) -> Result<Json<Vec<runs::RunSearchResult>>, StatusCode> {
    let live = query.live == Some(true);
    // One grouping exists. Anything else is refused, not ignored: a caller that asked for a
    // grouping and got the flat list would read turns as conversations.
    let group_by_chat = match query.group.as_deref() {
        None => false,
        Some("chat") => true,
        Some(_) => return Err(StatusCode::BAD_REQUEST),
    };
    let hide_chat_turns = match query.chat_turns.as_deref() {
        None | Some("show") => false,
        Some("hide") => true,
        Some(_) => return Err(StatusCode::BAD_REQUEST),
    };
    // A live listing with no explicit limit inherits the ceiling of live listings, not search's 50.
    // Without this the daemon would promise a shared constant and hand back the search window — and
    // the client would be the only thing guaranteeing the number, which is no guarantee at all.
    let limit = if live && query.limit.is_none() {
        crate::execution::concurrency::LIVE_LIST_LIMIT
    } else {
        parse_search_limit(query.limit)?
    };
    runs::search(
        &state.pool,
        &runs::SearchFilter {
            project_id: query.project_id,
            status: query.status,
            mode: query.mode,
            q: query.q,
            since: parse_time_bound(query.since)?,
            until: parse_time_bound(query.until)?,
            limit,
            live,
            chat_id: query.chat_id,
            group_by_chat,
            hide_chat_turns,
        },
    )
    .await
    .map(Json)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// What a caller may say when asking the VCS queue for something.
///
/// Two fields the queue needs are deliberately NOT here, and their absence is the security posture:
///
/// - **`project_root`** is resolved by `vcs::resolve_repo`, together with the key the queue locks
///   on, and the two can no longer be set apart from each other. A caller that could name the
///   repository root could point the daemon's git at any directory the daemon can reach, which is
///   the same reason `inspect.rs` "never resolves a root itself".
/// - **`origin`** comes from the bearer token's scope. `hooks.rs` states the rule this follows: a
///   value in the body is a claim the caller makes about itself, and here that claim decides whether
///   a human still has to approve the operation — precisely the thing a caller must not choose.
#[derive(Deserialize)]
pub(super) struct VcsRequestBody {
    pub(super) project_id: String,
    pub(super) operation: vcs::Op,
}

/// Who the queue records as having asked, derived from the key that authenticated the call.
///
/// **`Origin` decides whether a human still has to approve**, so the mapping is a security decision,
/// not bookkeeping. `Human` and `Shell` skip approval; `Run` and `Job` do not.
///
/// `Control` → `Human` is the load-bearing arm, and it is also where the *autonomous* path lands —
/// which is not obvious. An orchestrator turn is handed the control token (`assistant.rs`), and only
/// orchestrator turns are given an `--mcp-config`, so when the MCP door opens its requests arrive as
/// `Control`, not as `Run`. That is spec decision 6 working as intended: a turn acting on an order
/// you just gave carries your approval. It is worth stating plainly because the `Run` arm below
/// looks like the one that handles agents, and it is not.
///
/// `ApiToken(Admin)` → `Human`, deliberately, and **not** `Shell`. In this repo "shell" means the
/// Tauri desktop app — which holds the *control* token and therefore already maps to `Human` — so
/// recording `shell` for a durable API key would put a word in the listing that names the one client
/// that did not make the call. `Human` claims only what is true: a person minted this key on purpose
/// and it carries their approval. Whether an unattended admin key *should* pre-approve a merge is a
/// real question, and it belongs with the chunk that defines durable-key provenance rather than
/// being settled by a name chosen here.
///
/// `Run` cannot reach this route today — a run token opens exactly one route, the safety gate — but
/// mapping it costs nothing and is what the MCP tools will need once a run can submit directly.
/// `Service`, `TeamRun` and the lesser API levels are refused rather than guessed at: `permits`
/// should already have turned them away, so a scope arriving here unaccounted for is a routing bug,
/// and defaulting it would mean guessing about approval.
///
/// `TeamRun` is the sharpest of the three. A department has no `vcs::Origin` because it is not
/// allowed to want one: queueing a merge is the act that makes work survive on a branch other
/// people build on, and the teams design gives a department no authority to act at all. When that
/// authority arrives it arrives as its own spec, with a value here chosen on purpose — which is
/// exactly what a default would have taken away.
pub(super) fn vcs_origin(scope: &Scope) -> Result<vcs::Origin, StatusCode> {
    match scope {
        Scope::Control | Scope::ApiToken(ApiTokenLevel::Admin) => Ok(vcs::Origin::Human),
        Scope::Run(id) => Ok(vcs::Origin::Run(*id)),
        Scope::Service(_) | Scope::TeamRun(_) | Scope::ApiToken(_) => Err(StatusCode::FORBIDDEN),
    }
}

/// The one handler here that writes, and therefore the one that owes the cancellation question an
/// answer.
///
/// It awaits on both sides of its one write, and the two sides fail differently. **Before** the
/// insert it awaits git: `resolve_repo` runs a `rev-parse` under `git_exec::OPERATION_TIMEOUT`
/// (300s), and a disconnect during it leaves nothing written at all — the safest of the outcomes
/// here, and worth knowing for the other reason, that a pathological repository can hold this
/// handler for five minutes rather than the moment an INSERT takes. **After** the insert it awaits
/// the read that builds the ticket, so a client disconnecting there drops the future with the row
/// already committed. That half is benign **today** and only today: what is left behind is a
/// `queued` row that will still execute, appears in the listing, and holds no repository — the
/// caller loses its reply, not its request. So `http::uncancellable` is not needed yet.
///
/// It stops being benign the moment submitting becomes two writes — the row plus an approval
/// proposal, which is what the approval chunk adds. A disconnect between them would leave a request
/// that can never be approved. Whoever writes that second write moves this through `uncancellable`
/// at the same time.
#[derive(serde::Deserialize)]
pub(super) struct LandBody {
    /// Anywhere inside the worktree that is asking. Resolved to its root, so a session standing
    /// in a subdirectory asks the same question as one standing at the top.
    pub(super) cwd: String,
    /// The branch this landing is for, when the caller named one. `None` — every caller until
    /// 2026-09-03 — means the project's integration branch, and that has to keep being what an
    /// absent field means: `--land` alone is the spelling almost every session uses.
    #[serde(default)]
    pub(super) target: Option<String>,
}

/// "I am finished — take this branch." The one request a worktree could not previously express.
///
/// **A session can only ever have asked for merges INTO its own branch**, because
/// `merge_from_command` takes the target from the worktree it is standing in and there is no other
/// branch it could name. The reverse — landing the work — has no git spelling from inside the
/// worktree at all: you would have to check out the integration branch, which is the isolation
/// violation a worktree session must not commit, and which its harness blocks outright. So this is
/// not a command to be intercepted; it is a request, and it needed a door of its own.
///
/// **Asking is authorisation; it is not scheduling.** The session's own completion is the decision
/// that the work is ready — a person commanding it, or the agent when it has finished — so the row
/// enters `queued` rather than `awaiting_approval`, and no second approval is invented for a
/// judgement that has already been made. What stays with the queue is WHEN, which is the part a
/// session cannot know: one operation per repository, in order, against a repository that may have
/// moved since the asking.
///
/// **Re-evaluation is not added here because it is already how the queue works.** The merge is
/// computed fresh in the integration worktree at the moment of execution, not at the moment of
/// asking, so a branch that has diverged or a merge that has come to conflict aborts before
/// publishing and the row records `failed` with git's own output. A request that can no longer land
/// says so and stops, which is the behaviour wanted rather than a new mechanism.
///
/// The target is `land::integration_branch` — declared once per project — unless the caller named
/// one this project admits, and in neither case is it read off any worktree's HEAD, which is the
/// defect `land.rs` exists to prevent. This handler no longer decides that, or anything else about
/// what the landing means: it resolves *who is asking* (a `cwd`, turned into a worktree root, a
/// branch and a repository) and hands the rest to `land.rs`. Design decision #8 — the ~60 lines
/// this used to carry moved to the module that now owns them.
pub(super) async fn land_worktree(
    State(state): State<AppState>,
    Json(body): Json<LandBody>,
) -> Result<Json<vcs::Ticket>, (StatusCode, String)> {
    let deadline = std::time::Instant::now() + crate::execution::git_exec::OPERATION_TIMEOUT;
    let refuse = |code: StatusCode, reason: String| (code, reason);

    let root = crate::execution::git_exec::toplevel(std::path::Path::new(&body.cwd), deadline)
        .await
        .map_err(|reason| refuse(StatusCode::UNPROCESSABLE_ENTITY, reason))?;
    let source = crate::execution::git_exec::current_branch(&root, deadline)
        .await
        .map_err(|reason| refuse(StatusCode::UNPROCESSABLE_ENTITY, reason))?;
    if source.trim() == "HEAD" {
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "this worktree is on a detached HEAD, so there is no branch to land".to_owned(),
        ));
    }

    let project_id = crate::execution::vcs::project_for_worktree(&state.pool, &root, deadline)
        .await
        .map_err(|reason| refuse(StatusCode::NOT_FOUND, reason))?;
    let repo = crate::execution::vcs::resolve_repo(&state.pool, &project_id)
        .await
        .map_err(|error| {
            tracing::warn!(project_id, ?error, "land: could not resolve the repository");
            refuse(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("{project_id}'s repository could not be resolved"),
            )
        })?;

    let id = crate::execution::land::submit(
        &state.pool,
        &repo,
        std::path::Path::new(repo.root()),
        source.trim(),
        body.target.as_deref(),
        deadline,
    )
    .await
    .map_err(|refusal| {
        let code = match &refusal {
            crate::execution::land::LandRefusal::AlreadyOnTarget(_)
            | crate::execution::land::LandRefusal::NothingToLand(_) => StatusCode::CONFLICT,
            crate::execution::land::LandRefusal::Refused(_) => StatusCode::UNPROCESSABLE_ENTITY,
            crate::execution::land::LandRefusal::NotAdmitted(error) => {
                tracing::warn!(%error, "land: admitting the request failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        refuse(code, refusal.message().to_owned())
    })?;

    vcs_ticket(&state, id, std::time::Duration::ZERO)
        .await
        .map_err(|code| {
            refuse(
                code,
                "the request was admitted but could not be read back".to_owned(),
            )
        })
}

pub(super) async fn submit_vcs_request(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(body): Json<VcsRequestBody>,
) -> Result<Json<vcs::Ticket>, StatusCode> {
    let origin = vcs_origin(&scope)?;
    let repo = vcs::resolve_repo(&state.pool, &body.project_id)
        .await
        .map_err(|error| match error {
            vcs::ResolveError::UnknownProject => StatusCode::NOT_FOUND,
            // The caller named a project that exists; what is wrong is the root this daemon has
            // recorded for it. 422 rather than 400 or 500: the request was well-formed and the
            // daemon is working, but the state it would act on is not a repository.
            vcs::ResolveError::NotARepository(reason) => {
                tracing::warn!(project_id = %body.project_id, %reason, "vcs: project root is not a repository");
                StatusCode::UNPROCESSABLE_ENTITY
            }
            vcs::ResolveError::Database(error) => {
                tracing::warn!(%error, "vcs: could not resolve a project");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        })?;
    let id = vcs::submit(&state.pool, &repo, &body.operation, origin)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "submitting a vcs request failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    // Answered with a zero deadline rather than a bare id: the caller gets the same shape back from
    // submitting as from asking later, so nothing has to special-case the first reply.
    vcs_ticket(&state, id, std::time::Duration::ZERO).await
}

pub(super) async fn get_vcs_request(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<vcs::Ticket>, StatusCode> {
    vcs_ticket(&state, id, std::time::Duration::ZERO).await
}

#[derive(Deserialize)]
pub(super) struct VcsListQuery {
    pub(super) project_id: Option<String>,
}

pub(super) async fn list_vcs_requests(
    State(state): State<AppState>,
    Query(query): Query<VcsListQuery>,
) -> Result<Json<Vec<vcs::RequestSummary>>, StatusCode> {
    vcs::list(&state.pool, query.project_id.as_deref())
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "listing vcs requests failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// Blocks up to `vcs::DEFAULT_WAIT`, then hands back whatever the ticket says.
///
/// Deliberately NOT wrapped in `uncancellable`: this handler only ever reads, so a client that
/// disconnects mid-wait costs a dropped `SELECT` loop and nothing else. The rule it must keep
/// obeying is the other one — `vcs::drain_once` executes git and must never be awaited from a
/// handler, because a disconnect there would strand a claimed row and jam the repository.
pub(super) async fn wait_vcs_request(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<vcs::Ticket>, StatusCode> {
    vcs_ticket(&state, id, vcs::DEFAULT_WAIT).await
}

pub(super) async fn vcs_ticket(
    state: &AppState,
    id: i64,
    deadline: std::time::Duration,
) -> Result<Json<vcs::Ticket>, StatusCode> {
    match vcs::wait_for(&state.pool, id, deadline).await {
        Ok(ticket) => Ok(Json(ticket)),
        Err(sqlx::Error::RowNotFound) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, id, "reading a vcs request failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub(super) async fn list_awaiting_approval_runs(
    State(state): State<AppState>,
) -> Result<Json<Vec<AwaitingRun>>, StatusCode> {
    runs::list_awaiting_approval(&state.pool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Deserialize)]
pub(super) struct RunMessageRequest {
    pub(super) message: String,
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
pub(super) async fn post_run_message(
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
    if mode == crate::integrations::email::TRIAGE_MODE {
        return Err(StatusCode::FORBIDDEN);
    }
    if runs::tool_policy_for_mode(&mode) == crate::execution::runner::ToolPolicy::None {
        return Err(StatusCode::FORBIDDEN);
    }

    let sender = state.run_messages.lock().unwrap().get(&id).cloned();
    let Some(sender) = sender else {
        return Err(StatusCode::CONFLICT);
    };
    // Raw text, and no pictures: framing a turn as a `stream-json` line is `runner.rs`'s job,
    // because knowing the CLI's wire format is what that module is for, and a second copy of that
    // shape here would drift the day the format does. This route takes a message and nothing else —
    // the door that carries pictures is a conversation's, not a run's.
    sender
        .send(crate::execution::runner::LaterTurn {
            text: body.message,
            images: Vec::new(),
            interrupt: false,
            stop_task: None,
        })
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
pub(super) async fn delete_run_message(
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

/// Puts an open git request away without touching its status: 204, 404 for an unknown id, 409 for
/// one that is not open (succeeded, running, or already settled). Admin-only by default-deny, like
/// `/waiting/count` — no scope table entry.
pub(super) async fn post_vcs_request_dismiss(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    match vcs::dismiss(&state.pool, id).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(vcs::DismissError::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(vcs::DismissError::NotOpen) => Err(StatusCode::CONFLICT),
        Err(vcs::DismissError::Db(error)) => {
            tracing::warn!(request_id = id, %error, "dismissing a git request failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// The owner approves the test map change a paused merge carries (spec 2026-10-05 §3.4): 204,
/// 404 for an unknown id, 409 for a row that is not waiting for the owner. Admin-only by
/// default-deny, like `/dismiss`.
pub(super) async fn post_vcs_request_approve(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    owner_decision_status(
        id,
        "approving",
        vcs::approve_for_owner(&state.pool, id).await,
    )
}

/// The owner refuses it; the row ends `rejected`. Same answers as approving.
pub(super) async fn post_vcs_request_refuse(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    owner_decision_status(id, "refusing", vcs::refuse_for_owner(&state.pool, id).await)
}

pub(super) fn owner_decision_status(
    id: i64,
    verb: &str,
    decided: Result<(), vcs::DismissError>,
) -> Result<StatusCode, StatusCode> {
    match decided {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(vcs::DismissError::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(vcs::DismissError::NotOpen) => Err(StatusCode::CONFLICT),
        Err(vcs::DismissError::Db(error)) => {
            tracing::warn!(request_id = id, %error, "{verb} a paused git request failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub(super) async fn get_run_knowledge(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<crate::brief::RunTrace>, StatusCode> {
    match crate::brief::trace_of(&state.pool, id).await {
        Ok(Some(trace)) => Ok(Json(trace)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(run_id = id, %error, "reading what a run was told failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

pub(super) async fn post_worktree_release(
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

pub(super) async fn get_judge_opinions_of_run(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<crate::judge::JudgeOpinion>>, StatusCode> {
    crate::judge::opinions_for_run(&state.pool, id)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
