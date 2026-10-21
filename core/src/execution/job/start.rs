//! Starting a job: `resolve_start` (pure) and `start` (writes the row, provisions the worktree).

use super::*;

/// What `POST /jobs` accepts.
///
/// Neither the mode nor the root appears here, and neither ever will: both are resolved from the
/// project's own autopilot state (`resolve_start`), because a caller that could name its own root
/// would be naming a directory this daemon then creates a worktree in and writes to.
#[derive(Deserialize)]
pub struct CreateJobRequest {
    pub project_id: String,
    pub prompt: String,
    /// What this job may spend on itself, under the house limit rather than instead of it. `None`
    /// means only the house limit governs — what every `graph:` rule has always meant.
    pub budget_usd: Option<f64>,
    /// How many rounds it may run. Cut by `config::rounds_allowed` before it is stored, never after:
    /// this number comes from a model filling in a tool call, and a ceiling applied at read time is
    /// a ceiling one forgetful caller can walk past. `None` is one round.
    pub max_rounds: Option<i64>,
    /// The team to direct this job, by id. `None` is the sequential job in one shared checkout,
    /// which is what every job before this existed was and what every caller that says nothing gets.
    ///
    /// A field the caller fills in, unlike `max_items` — and the asymmetry is the same one
    /// `max_rounds` and `budget_usd` already have. `max_items` has a ceiling nobody may raise
    /// because it is fan-out the daemon pays for; a team is a choice about WHO does the work, and
    /// the ceiling that bounds what it costs is the project's slot count, applied on the way in by
    /// `room_for` and again by every item that asks for a checkout.
    #[serde(default)]
    pub team_id: Option<String>,
}

/// Why a job cannot be started for a project.
///
/// Three refusals rather than one error string, because a caller answers them differently: an
/// unknown project is a 404, and the other two are a 422 about a machine configured differently
/// from what the request assumed rather than a request that is malformed.
#[derive(Debug, PartialEq, Eq)]
pub enum StartRefusal {
    UnknownProject,
    NotActive(crate::governance::autopilot::Mode),
    NoRoot,
}

impl StartRefusal {
    /// The sentence handed back to whoever asked. It says which state the project is in, because a
    /// 422 that will not say what is wrong is a 422 somebody retries unchanged.
    pub fn reason(&self, project_id: &str) -> String {
        match self {
            Self::UnknownProject => format!("unknown project: {project_id}"),
            Self::NotActive(crate::governance::autopilot::Mode::Shadow) => format!(
                "project {project_id} is in shadow mode, which is plan-only; a job writes to a \
                 worktree and needs active"
            ),
            Self::NotActive(_) => format!("project {project_id} has autopilot off"),
            Self::NoRoot => format!("project {project_id} is active but has no root recorded"),
        }
    }
}

/// What a start resolved to.
///
/// One field today. It is a value rather than a bare `String` because Chunk 4 adds the claimed slot
/// number beside it, and a caller that had unwrapped a `String` would have to be rewritten then.
#[derive(Debug, PartialEq, Eq)]
pub struct ResolvedStart {
    pub project_root: String,
}

/// PURE: what a job request resolves to, or why it does not.
///
/// Neither the mode nor the root is ever chosen by whoever asks — both are read off the project's
/// own autopilot state, exactly as `resolve_run_request` does for runs.
///
/// **A job REQUIRES `Mode::Active`, and that is where this deliberately differs from that
/// function.** `resolve_run_request` maps `Shadow` onto the `shadow` run mode, which is right for a
/// run because a run can be plan-only. A job cannot: its plan node has to write the `plan.json`
/// that the queue is taken from, and a plan-only node writes nothing — so a job quietly demoted to
/// shadow would do nothing whatsoever while reporting that it was working all night. `Shadow` and
/// `Off` are refusals with reasons of their own, never fallbacks. Copying that table across without
/// noticing is the natural mistake here, which is why it has a test of its own.
pub fn resolve_start(
    roster: &[crate::governance::autopilot::ProjectSummary],
    project_id: &str,
) -> Result<ResolvedStart, StartRefusal> {
    let project = roster
        .iter()
        .find(|project| project.project_id == project_id)
        .ok_or(StartRefusal::UnknownProject)?;
    if project.mode != crate::governance::autopilot::Mode::Active {
        return Err(StartRefusal::NotActive(project.mode));
    }
    // Active with no root is a real state rather than an impossible one: the root is recorded when
    // a project is pointed at a directory, and the mode can be set without that having happened.
    let project_root = project.project_root.clone().ok_or(StartRefusal::NoRoot)?;
    Ok(ResolvedStart { project_root })
}

/// How a start attempt ended.
///
/// `NoRoom` replaced `AlreadyLive` when `one_live_job_per_project` came out (migration 0053). The
/// two are the same answer at different ceilings — that index could only ever say "one" — and they
/// are still not a check that failed here: the slot `INSERT` is the lock, so a scheduler tick and a
/// manual request racing for the same project cannot both pass a count and then both proceed.
///
/// Kept apart from `Failed` because the caller turns them into different answers: no room is a 409
/// the asker can act on by waiting, and `Failed` is a 500 nothing they do would have helped.
///
/// `Debug` so a test that expected a start and got a refusal can say WHICH refusal. Without it the
/// panic reads "the job did not start" and the next hour goes on finding out why.
#[derive(Debug)]
pub enum JobStart {
    Started(i64),
    NoRoom(String),
    /// The request named a team the catalogue does not have.
    ///
    /// Its own answer and not `Failed`, because it is the caller's mistake and a fixable one: a
    /// mistyped id, or a team deleted since the rule that names it was written. `Failed` would tell
    /// them the daemon broke.
    ///
    /// **Refused rather than run without one**, and that is the decision worth writing down.
    /// Starting the job with no team would do the work sequentially and report `completed`, and the
    /// only symptom would be that the parallelism somebody configured never seemed to happen — the
    /// exact failure this design has already walked into twice.
    NoTeam(String),
    /// The workflow the job runs has no usable graph: a pin whose `graph.yaml` does not parse, or
    /// a workflow the project does not pin.
    ///
    /// **Refused rather than replaced by the embedded autopilot**, for the reason `NoTeam` is
    /// refused rather than run without a team: a fallback would run a different workflow from the
    /// one the project pinned and report `completed`. A 422, the caller's to fix.
    NoGraph(String),
    Failed,
}

/// Everything `start` needs, with no trace of who is asking.
///
/// Deliberately plain values rather than the `&ScheduleRule` + `&GraphConfig` this used to take.
/// Those types made the scheduler the only caller that could exist — a rule and a graph config are
/// what a *rule* has — and the request now arrives from `POST /jobs` as well. Whatever ceiling the
/// caller applies to `max_items` is applied before it gets here, so this function has one job.
pub struct StartRequest<'a> {
    pub project_id: &'a str,
    pub project_root: &'a str,
    /// `None` for a job nobody scheduled. See `NewJob::rule_name`.
    pub rule_name: Option<&'a str>,
    pub prompt: &'a str,
    pub max_items: i64,
    pub gate_each: bool,
    pub review: bool,
    /// Already cut by whoever asked, exactly as `max_items` is. See [`NewJob::gate_retries`].
    pub gate_retries: i64,
    pub head_sha: Option<&'a str>,
    /// Both `None` for a scheduled job: a `graph:` rule asks for neither, which keeps it at one
    /// round under the house limit — exactly what it did before rounds existed.
    pub max_rounds: Option<i64>,
    pub budget_usd: Option<f64>,
    /// The team that will direct this job, or `None` for the job of today.
    ///
    /// `None` is not a lesser job: it is the sequential queue in one shared checkout that every
    /// job in this repository has been until now, and it stays the default in both callers. A team
    /// is asked for, per request and per rule, and never acquired.
    pub team_id: Option<&'a str>,
    /// The workflow this job runs, by pin name. `None` is [`crate::seed::NAME`] (`autopilot`),
    /// which is what every job has run until now (D13). Resolved to a graph and frozen on the row
    /// at start by [`resolve_job_graph`]; never re-resolved afterwards.
    pub workflow: Option<&'a str>,
}

/// A job's graph as resolved at start, before it is frozen into the row.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedGraph {
    pub effective: crate::workflows::effective::EffectiveGraph,
    /// `<name>@<version>`.
    pub bundle_ref: Option<String>,
    /// The pin's hash for a pinned bundle (that hash IS the bundle hash), `seed::digest()` for the
    /// embedded autopilot.
    pub bundle_hash: Option<String>,
    /// True when the embedded autopilot was used rather than a pinned bundle.
    pub builtin: bool,
}

/// The embedded autopilot, with `overlay` painted in.
fn embedded_graph(
    overlay: &std::collections::BTreeMap<String, crate::workflows::NodeOverlay>,
) -> Result<ResolvedGraph, String> {
    let graph = crate::workflows::graph::parse(crate::seed::graph_yaml())
        .map_err(|error| format!("the embedded autopilot graph does not parse: {error}"))?;
    Ok(ResolvedGraph {
        effective: crate::workflows::effective::effective(&graph, overlay),
        bundle_ref: Some(format!("{}@{}", crate::seed::NAME, crate::seed::VERSION)),
        bundle_hash: Some(crate::seed::digest()),
        builtin: true,
    })
}

/// Where a pin's `graph.yaml` is read from, or `None` when no trustworthy source exists.
///
/// The project's own copy (adopted or ejected) wins, as it does in `materialize::source_of`; the
/// library is used only while it still hashes to the pin, because a drifted bundle is bytes nobody
/// pinned.
fn pinned_graph_dir(
    project_root: &Path,
    library_root: Option<&Path>,
    pin: &crate::workflows::Pin,
) -> Option<PathBuf> {
    if (pin.path.is_some() || pin.ejected_at.is_some())
        && let Some(copy) =
            crate::workflows::copy_path(project_root, pin).filter(|dir| dir.is_dir())
    {
        return Some(copy);
    }
    let dir = library_root?.join(&pin.name).join(&pin.version);
    let bundle = crate::workflows::read_bundle(&dir, &pin.name, &pin.version)
        .ok()
        .flatten()?;
    (bundle.hash == pin.hash).then_some(dir)
}

/// PURE (reads files, touches no database): the graph `workflow` resolves to for a project.
///
/// Pins file -> pin named `workflow` -> project copy else library bundle -> `graph::parse` ->
/// `effective(graph, pin.nodes)`. No pins file, no such pin, or an unavailable source resolves to
/// the embedded autopilot, but ONLY for the `autopilot` workflow: asking for any other name that
/// the project does not pin is a refusal, not a different workflow. A source that is present but
/// does not parse is always a refusal.
pub fn resolve_graph_from(
    machine_root: Option<&Path>,
    library_root: Option<&Path>,
    project_id: &str,
    project_root: &Path,
    workflow: &str,
) -> Result<ResolvedGraph, String> {
    let pins_file = machine_root
        .and_then(|root| crate::project::state::dir(root, project_id))
        .map(|dir| dir.join(crate::project::state::PINS_FILE))
        .filter(|file| file.is_file());
    let pin = pins_file.and_then(|file| match crate::workflows::read_pins(&file) {
        Ok(pins) => pins.workflows.into_iter().find(|pin| pin.name == workflow),
        Err(error) => {
            tracing::warn!(project_id, %error, "unreadable workflows.yaml; the job starts unpinned");
            None
        }
    });
    let Some(pin) = pin else {
        return if workflow == crate::seed::NAME {
            embedded_graph(&std::collections::BTreeMap::new())
        } else {
            Err(format!(
                "project {project_id} does not pin a workflow named `{workflow}`"
            ))
        };
    };

    let text = pinned_graph_dir(project_root, library_root, &pin).and_then(|dir| {
        std::fs::read_to_string(dir.join(crate::workflows::graph::GRAPH_FILE)).ok()
    });
    match text {
        Some(text) => {
            let graph = crate::workflows::graph::parse(&text).map_err(|error| {
                format!(
                    "workflow {}@{} has a graph that does not parse: {error}",
                    pin.name, pin.version
                )
            })?;
            Ok(ResolvedGraph {
                effective: crate::workflows::effective::effective(&graph, &pin.nodes),
                bundle_ref: Some(format!("{}@{}", pin.name, pin.version)),
                bundle_hash: Some(pin.hash.clone()),
                builtin: false,
            })
        }
        None if workflow == crate::seed::NAME => embedded_graph(&pin.nodes),
        None => Err(format!(
            "workflow {}@{} is pinned but its graph.yaml cannot be read from the project's copy or \
             from an unchanged library bundle",
            pin.name, pin.version
        )),
    }
}

/// The graph a job starting now runs, resolved off the daemon's own pins and library.
pub async fn resolve_job_graph(
    state: &AppState,
    project_id: &str,
    project_root: &str,
    workflow: &str,
) -> Result<ResolvedGraph, String> {
    let machine_root = state.machine_config_root.clone();
    let library = state.workflow_library.clone();
    let project_id = project_id.to_string();
    let project_root = PathBuf::from(project_root);
    let workflow = workflow.to_string();
    tokio::task::spawn_blocking(move || {
        resolve_graph_from(
            machine_root.as_deref(),
            library.as_deref(),
            &project_id,
            &project_root,
            &workflow,
        )
    })
    .await
    .map_err(|error| format!("the graph resolution did not finish: {error}"))?
}

/// Creates a job and provisions the worktree it will live in.
///
/// The worktree belongs to the JOB, not to any of its nodes — that is the whole reason a job can
/// outlive one context window, and it is why this provisions it here rather than letting the first
/// node do it.
///
/// This lives in `job.rs` rather than in `scheduler.rs` because creating jobs is what this module
/// is for; the module map describes the scheduler as firing runs "through `runs::create_run_inner`;
/// never defining them", and the same applies to jobs. It moved here when a second caller appeared:
/// two copies of this sequence would mean one of them learning a fix the other never learns.
pub async fn start(state: &AppState, request: &StartRequest<'_>) -> JobStart {
    // Before the row, and before the slot, because it is the one refusal here that costs nothing to
    // discover and everything to discover late. `foreign_keys` is on, so an id naming no team would
    // fail the INSERT below with a raw constraint error, and the caller would report "the job was
    // created and could not be provisioned" — a sentence whose two halves are both untrue.
    //
    // Read rather than trusted even for a rule, because the file that names the team and the table
    // that holds it are edited in different places at different times: a team deleted this morning
    // leaves a `graph:` rule from last month naming it, and nothing anywhere would have complained.
    if let Some(team_id) = request.team_id {
        let known: Option<i64> = match sqlx::query_scalar("SELECT 1 FROM teams WHERE id = ?")
            .bind(team_id)
            .fetch_optional(&state.pool)
            .await
        {
            Ok(known) => known,
            Err(error) => {
                tracing::warn!(team_id, %error, "could not read the team a job asked for");
                return JobStart::Failed;
            }
        };
        if known.is_none() {
            return JobStart::NoTeam(format!(
                "no team named `{team_id}` is in the catalogue, and a job that asked for one is not \
                 started without it"
            ));
        }
    }

    // The workflow's graph, resolved ONCE here and frozen on the row (spec 5). Before the row for the
    // same reason as the team: a refusal costs nothing now and a retired row later. Body files are
    // not re-read after start; `into_worktree` below materialises them.
    let resolved = match resolve_job_graph(
        state,
        request.project_id,
        request.project_root,
        request.workflow.unwrap_or(crate::seed::NAME),
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(reason) => return JobStart::NoGraph(reason),
    };
    let snapshot = match snapshot_of_effective(
        &resolved.effective,
        resolved.bundle_ref.as_deref(),
        resolved.bundle_hash.as_deref(),
    ) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(project_id = request.project_id, %error, "could not freeze a job's graph");
            return JobStart::Failed;
        }
    };

    // Asked before the row exists, and it is not the decision — `claim` below is. A job cannot claim
    // until it has an id, so the authoritative answer costs a row, and a scheduler firing at a full
    // project every half hour would leave a retired job behind every time. This turns that into the
    // rare case where two starts actually crossed.
    match crate::execution::concurrency::room_for(&state.pool, request.project_id).await {
        Ok(Some(full)) => return JobStart::NoRoom(full.reason()),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                project_id = request.project_id,
                %error,
                "could not read the concurrency ceiling"
            );
            return JobStart::Failed;
        }
    }

    let job_id = match insert_job(
        &state.pool,
        &NewJob {
            project_id: request.project_id,
            project_root: request.project_root,
            rule_name: request.rule_name,
            prompt: request.prompt,
            max_items: request.max_items,
            gate_each: request.gate_each,
            review: request.review,
            gate_retries: request.gate_retries,
            head_sha: request.head_sha,
            max_rounds: request.max_rounds,
            budget_usd: request.budget_usd,
            team_id: request.team_id,
            graph: Some(snapshot),
        },
    )
    .await
    {
        Ok(job_id) => job_id,
        Err(error) => {
            tracing::warn!(
                project_id = request.project_id,
                rule_name = request.rule_name.unwrap_or("(none)"),
                %error,
                "could not start a job"
            );
            return JobStart::Failed;
        }
    };

    let owner = crate::execution::worktree::Owner::Job(job_id);

    // AFTER the row, not before it, and the reason is arithmetic rather than taste: a slot is keyed
    // on its owner's id, and the job has no id until it is inserted. So the row goes in first and is
    // retired again if there is no room — the same shape this function already uses when a worktree
    // cannot be provisioned, and it keeps the claim itself an INSERT that two racing jobs resolve on
    // the primary key rather than on a read.
    match crate::execution::concurrency::claim(&state.pool, request.project_id, owner).await {
        Ok(crate::execution::concurrency::ClaimOutcome::Claimed(_)) => {}
        // The race the pre-check cannot cover: somebody took the last slot in between. No room is
        // not a failure, so the row is retired quietly and the caller is told which ceiling it hit.
        Ok(crate::execution::concurrency::ClaimOutcome::Full(full)) => {
            let _ = retire(&state.pool, job_id, STATUS_CANCELLED).await;
            return JobStart::NoRoom(full.reason());
        }
        Err(error) => {
            return fail_early(
                state,
                request.project_id,
                job_id,
                &format!("its concurrency slot could not be claimed: {error}"),
            )
            .await;
        }
    }

    let info =
        match crate::execution::worktree::create(Path::new(request.project_root), owner).await {
            Ok(info) => info,
            Err(error) => {
                return fail_early(state, request.project_id, job_id, &format!("{error}")).await;
            }
        };
    let path = info.path.to_string_lossy().into_owned();

    // The project's workflow, into the tree its nodes will read it from. `.ai/`, `.claude/` and
    // their like are usually gitignored, so a fresh worktree has none of them; without this every
    // job ran without the rules its project pinned, and nothing said so. Fail-soft: a missing
    // workflow is a feed line, never a failed job (`workflow_materialize::into_worktree`).
    //
    // BEFORE the tree is recorded, not after: the record is what makes the tree visible to the
    // tick, and a tick landing between the two would start the first node in a tree with no rules.
    crate::workflows::materialize::into_worktree(
        &state.pool,
        state.machine_config_root.as_deref(),
        state.workflow_library.as_deref(),
        request.project_id,
        &info.path,
    )
    .await;

    if let Err(error) = crate::execution::worktree::record(
        &state.pool,
        owner,
        request.project_id,
        request.project_root,
        &path,
        &info.branch,
        info.base_sha.as_deref(),
    )
    .await
    {
        // The directory exists and nothing in the database knows it does, so the GC never will.
        // Taken down here, best effort; the startup orphan sweeper recognises `job-<id>` and is the
        // backstop when this removal fails too.
        if let Err(remove_error) =
            crate::execution::worktree::remove(Path::new(request.project_root), &info.path, &[])
                .await
        {
            tracing::warn!(
                job_id,
                path = %path,
                %remove_error,
                "an unrecorded job worktree could not be removed; the orphan sweeper will"
            );
        }
        return fail_early(
            state,
            request.project_id,
            job_id,
            &format!("its worktree was created but could not be recorded: {error}"),
        )
        .await;
    }

    // Asked once more before saying `Started`, because the row has been live the whole time this
    // function was in git: a cancel, or a pass past `PROVISIONING_GRACE`, may have ended it. A
    // caller told `Started` about a job that is already over reports work that will never happen.
    // The tree is recorded by now, so the worktree GC collects it with the ended job.
    match load_job(&state.pool, job_id).await {
        Ok(job) if LIVE_STATUSES.contains(&job.status.as_str()) => {}
        Ok(job) => {
            tracing::warn!(
                project_id = request.project_id,
                job_id,
                status = %job.status,
                "a job ended while it was being provisioned"
            );
            return JobStart::Failed;
        }
        // Unreadable is not ended. The row went in and the tree is recorded; the tick decides from
        // here, and a caller told `Failed` would retry a job that is in fact running.
        Err(error) => {
            tracing::warn!(
                project_id = request.project_id,
                job_id,
                %error,
                "could not re-read a job just started"
            );
        }
    }

    // Two sentences rather than one with a hole in it. A job nobody scheduled has no rule, and
    // `for rule 'None'` would be a line a person reads as a bug in the scheduler.
    let started = match request.rule_name {
        Some(rule_name) => format!(
            "job {job_id} started for rule '{rule_name}' on {}",
            info.branch
        ),
        None => format!("job {job_id} started on {}", info.branch),
    };
    let _ = crate::feed::append(
        &state.pool,
        Some(request.project_id),
        "job_started",
        &started,
        None,
        Some(&crate::feed::Subject::Job(job_id)),
    )
    .await;
    JobStart::Started(job_id)
}

/// Retires a job that never got as far as its first node, and says so where a person will see it.
///
/// A job left live with no worktree would be ticked forever and hold a concurrency slot,
/// which would take the whole project's autonomy down with it — silently, since nothing else logs.
pub(super) async fn fail_early(
    state: &AppState,
    project_id: &str,
    job_id: i64,
    why: &str,
) -> JobStart {
    if let Err(error) = retire(&state.pool, job_id, Outcome::Failed.as_status()).await {
        tracing::error!(
            project_id,
            job_id,
            %error,
            "a job could not be provisioned AND could not be retired; it holds the project's job slot"
        );
    }
    let _ = crate::feed::append(
        &state.pool,
        Some(project_id),
        "job_failed",
        &format!("job {job_id} could not start: {why}"),
        None,
        Some(&crate::feed::Subject::Job(job_id)),
    )
    .await;
    JobStart::Failed
}
