//! Creating a run row: the entry points for each kind of run, provisioning, and `create_run_with`.

use super::*;

/// One node of a job: which job it belongs to, what part it plays, and the worktree it inherits.
///
/// A node carries the worktree rather than provisioning one, which is the whole reason a job can
/// exceed a single context window — each node starts with a fresh window and picks up the previous
/// one's work from the disk it shares.
pub struct JobNode {
    pub job_id: i64,
    /// `plan` | `implement` | `review`. Never `gate`: the gate is a subprocess, not a run.
    pub stage: String,
    pub worktree_path: String,
    pub branch: String,
    /// The queue position of the item this node implements, or `None` for a node with no item (plan,
    /// replan, review). Read for the llm-router's request, which names what already failed the item.
    pub item_ordinal: Option<usize>,
    /// The tools and directories this node was resolved to by `job.rs`, recorded in `run_loadout`
    /// here before the CLI starts. `None` launches with the box's fixed tools and no row.
    pub loadout: Option<crate::loadout::RunTools>,
    /// What the node's launcher asked of the router (M4 C2). `Default` is "route as before".
    pub launch: NodeLaunchRoute,
}

/// The engine-side half of a node launch, as `create_run_with` copies it into the router query:
/// a restriction of the runners the router may choose from, a pin that bypasses it, and whether the
/// run's answer will be validated later (which keeps the router's outcome report for after that).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeLaunchRoute {
    pub restrict_runners: Option<Vec<crate::models::route_advice::RunnerKind>>,
    pub pin: Option<crate::models::route_advice::EnginePinRoute>,
    pub judged_later: bool,
    /// The node's own effort when it is not pinned; see `RouteQuery::effort`.
    pub effort: Option<String>,
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
    create_run_with(
        state,
        NewRun {
            prompt,
            project_id,
            cwd,
            mode,
            steerable,
            provisioning: None,
            permission_mode: None,
        },
    )
    .await
}

/// Starts the one run that is given its work already staged: a merge conflict, put there by the
/// daemon, in a worktree born on the branch the merge was going into.
///
/// Shape and shared reasoning in [`NewRun::provisioned`]. Its "never steerable" holds here for one
/// reason of its own besides: the work is a conflict the queue found, and text typed into it
/// mid-flight would change what gets published with nothing recording the substitution.
pub async fn create_resolution_run(
    state: &AppState,
    prompt: String,
    project_id: String,
    project_root: String,
    resolution: Resolution,
) -> Result<i64, CreateRunError> {
    create_run_with(
        state,
        NewRun::provisioned(
            prompt,
            project_id,
            project_root,
            Provisioning::Resolution(resolution),
        ),
    )
    .await
}

/// Starts the correction run the post-merge gate opens over a reverted merge, in a worktree born on
/// the `fix/` branch prepared for it.
///
/// Shape and shared reasoning in [`NewRun::provisioned`]. Nothing is claimed on the way in: the
/// caller (`verify_postgate_worker::open_corrections`) claims the branch before it asks.
pub async fn create_correction_run(
    state: &AppState,
    prompt: String,
    project_id: String,
    project_root: String,
    correction: Correction,
) -> Result<i64, CreateRunError> {
    create_run_with(
        state,
        NewRun::provisioned(
            prompt,
            project_id,
            project_root,
            Provisioning::Correction(correction),
        ),
    )
    .await
}

/// Starts one node of a job inside that job's existing worktree.
///
/// Shape and shared reasoning in [`NewRun::provisioned`]. Its `mode = "worktree"` is deliberate
/// rather than a mode of its own: `plan_only`, the tool policy, `max_attempts` and migration 0009's
/// exclusivity index all branch on `mode`, and a fourth value would have to be excluded from each
/// of them. Missing one would be silent.
pub async fn create_job_node_run(
    state: &AppState,
    prompt: String,
    project_id: String,
    project_root: String,
    node: JobNode,
) -> Result<i64, CreateRunError> {
    create_run_with(
        state,
        NewRun::provisioned(prompt, project_id, project_root, Provisioning::Node(node)),
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
        NewRun::provisioned(prompt, project_id, project_root, Provisioning::Item(item)),
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
pub(super) enum Provisioning {
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
    /// A correction run: a worktree of its own, born on the `fix/` branch the post-merge gate
    /// prepared. Standalone for the gate and the slot, `Item`-like only for the base.
    Correction(Correction),
}

/// A correction run about to be given a tree of its own (F3-4, spec 2026-10-05 §6.2 D4).
pub struct Correction {
    /// The `fix/` branch the tree is born on, whose first commit is the revert of the revert.
    pub base: String,
}

/// One item of a job, about to be given a tree of its own.
pub struct JobItem {
    pub job_id: i64,
    pub item_id: i64,
    /// `plan` | `implement` | `review`, as [`JobNode::stage`].
    pub stage: String,
    /// Where this item's tree is born: the tip of the job's branch as the last gate left it.
    ///
    /// Named and not `Option`, because there is no honest default. `None` would mean the project
    /// checkout's HEAD, which is `master` — a commit that has none of this job's work in it.
    pub base: String,
    /// The tools and directories this item's node was resolved to by `job.rs`, recorded in
    /// `run_loadout` here before the CLI starts. `None` launches with the box's fixed tools and
    /// no row.
    pub loadout: Option<crate::loadout::RunTools>,
}

impl Provisioning {
    pub(super) fn node(&self) -> Option<&JobNode> {
        match self {
            Self::Node(node) => Some(node),
            Self::Resolution(_) | Self::Item(_) | Self::Correction(_) => None,
        }
    }

    pub(super) fn resolution(&self) -> Option<&Resolution> {
        match self {
            Self::Resolution(resolution) => Some(resolution),
            Self::Node(_) | Self::Item(_) | Self::Correction(_) => None,
        }
    }

    pub(super) fn item(&self) -> Option<&JobItem> {
        match self {
            Self::Item(item) => Some(item),
            Self::Node(_) | Self::Resolution(_) | Self::Correction(_) => None,
        }
    }

    pub(super) fn correction(&self) -> Option<&Correction> {
        match self {
            Self::Correction(correction) => Some(correction),
            Self::Node(_) | Self::Resolution(_) | Self::Item(_) => None,
        }
    }

    /// Whether this provisioning gets a checkout of its own — and therefore a concurrency slot, a
    /// `worktrees` row, and a share of the disk.
    ///
    /// **The house rule, in one place.** `Node` is the only exception, and it is the exception
    /// because it gets no tree: it works inside the one its job already owns, and charging it a
    /// second slot would have a five-item job refuse itself at the second item. Everything else —
    /// a standalone run, a resolution, an item — brings a tree and pays for it.
    pub(super) fn owns_a_tree(&self) -> bool {
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
pub(super) const MIN_FREE_DISK_GB: u64 = 5;

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
pub(super) async fn no_room_on_disk(project_root: &std::path::Path) -> Option<String> {
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

/// Everything a new run IS, apart from the daemon it is created in.
///
/// **One argument and not seven, and the lint that asked for this was right about more than
/// counting.** Four of the callers below pass the same five values in the same order, three of
/// them pass a literal `"worktree"`, `false`, and a `None` carrying six lines of explanation
/// apiece — repeated verbatim, three times, because there was nowhere else to put it. A struct
/// gives the shared shape a name ([`NewRun::provisioned`]) and the shared reasoning one home.
pub(super) struct NewRun<'a> {
    pub(super) prompt: String,
    pub(super) project_id: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) mode: &'a str,
    pub(super) steerable: bool,
    pub(super) provisioning: Option<Provisioning>,
    pub(super) permission_mode: Option<crate::chats::PermissionMode>,
}

impl NewRun<'static> {
    /// The shape the daemon starts its OWN work in: a job's node, a job's item, and the queue's
    /// conflict resolution. All three agree on every field but the provisioning, and the agreement
    /// is not a coincidence -- each one is the daemon acting on a plan it already holds.
    ///
    /// `mode = "worktree"` and nothing else, like every other autonomous run that touches code: it
    /// carries the tool policy, the gate, and migration 0009's exclusivity, none of which a fourth
    /// mode would inherit.
    ///
    /// Never steerable. Each is one step of a plan already agreed, and text typed into it mid-flight
    /// would change what that step does with nothing recording the substitution -- the queue would
    /// still claim the item it was given. Steering belongs to a run somebody started and is watching.
    ///
    /// No rung asked for. These are started by the daemon rather than by a caller who could have an
    /// opinion -- and the park is the RIGHT answer for them: `a_park_here_would_only_destroy`
    /// already refuses the cases where it is not, on evidence, and a blanket rung here would take
    /// that judgement away from it.
    pub(super) fn provisioned(
        prompt: String,
        project_id: String,
        project_root: String,
        provisioning: Provisioning,
    ) -> Self {
        Self {
            prompt,
            project_id: Some(project_id),
            cwd: Some(project_root),
            mode: "worktree",
            steerable: false,
            provisioning: Some(provisioning),
            permission_mode: None,
        }
    }
}

pub(super) async fn create_run_with(
    state: &AppState,
    run: NewRun<'_>,
) -> Result<i64, CreateRunError> {
    let NewRun {
        prompt,
        project_id,
        cwd,
        mode,
        steerable,
        provisioning,
        permission_mode,
    } = run;
    let node = provisioning.as_ref().and_then(Provisioning::node);
    let resolution = provisioning.as_ref().and_then(Provisioning::resolution);
    let item = provisioning.as_ref().and_then(Provisioning::item);
    let correction = provisioning.as_ref().and_then(Provisioning::correction);
    // The house rule, asked once: everything but a job node brings a checkout of its own, and
    // therefore claims a slot, records a row and is charged disk for it. `None` — a standalone
    // run — brings one too.
    let owns_a_tree = provisioning.as_ref().is_none_or(Provisioning::owns_a_tree);
    let standalone = provisioning.is_none();
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
        && (mode == crate::integrations::email::TRIAGE_MODE
            || tool_policy_for_mode(mode) == crate::execution::runner::ToolPolicy::None)
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
        let _ = crate::execution::concurrency::reconcile_orphaned_slots(&state.pool).await;
        match crate::execution::concurrency::room_for(&state.pool, project).await {
            Ok(Some(_)) => return Err(CreateRunError::Busy),
            Ok(None) => {}
            Err(error) => return Err(CreateRunError::Db(error)),
        }
    }

    let now = chrono::Utc::now().to_rfc3339();
    let session_id = crate::auth::generate_uuid_v4();
    // `permission_mode` binds NULL for every caller that says nothing, which is every caller
    // that existed before the field did. `0129` documents NULL on this column as "no CLI turn
    // wrote one", and that reading survives: a run that ASKS for a rung is a run somebody
    // deliberately gave one to, and the hook tells the two apart by `Option`, not by spelling.
    // Spec A D2: the project's judge setting is photographed onto the run, so a run's rules do
    // not change halfway through, the reason `permission_mode` is a snapshot. Only for the two
    // modes the judge serves; every other run reads `off`, which is what it is.
    // Spec B D11: `judge_resolve` is photographed like `judge`, for the same reason, but only for
    // worktree runs: the B never acts anywhere else (D2), and a snapshot it can never read would
    // be a value that looks like a setting and means nothing.
    let inserted = sqlx::query(
        "INSERT INTO runs (project_id, cwd, prompt, status, mode, session_id, steerable,
                           permission_mode, judge, judge_resolve, created_at)
         VALUES (?, ?, ?, 'running', ?, ?, ?, ?,
                 CASE WHEN ? IN ('shadow', 'worktree')
                      THEN COALESCE((SELECT judge FROM autopilot_state WHERE project_id = ?), 'off')
                      ELSE 'off' END,
                 CASE WHEN ? = 'worktree'
                      THEN COALESCE((SELECT judge_resolve FROM autopilot_state WHERE project_id = ?), 'off')
                      ELSE 'off' END,
                 ?)",
    )
    .bind(&project_id)
    .bind(&cwd)
    .bind(&prompt)
    .bind(mode)
    .bind(&session_id)
    .bind(i64::from(steerable))
    .bind(permission_mode.map(crate::chats::PermissionMode::as_str))
    .bind(mode)
    .bind(&project_id)
    .bind(mode)
    .bind(&project_id)
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
    // The project's ceiling, when it names one. Only a worktree run reads it, because that is the
    // only mode with a project to read the file for - and it is the mode all autonomous work runs
    // in. Everything else keeps the house ceiling, which is what `None` means here.
    let mut max_turns: Option<i64> = None;
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
        let rules = crate::config::load_schedule_rules(
            state.machine_config_root.as_deref(),
            worktree_project_id,
        );
        // Read OUTSIDE the gate's branch below, deliberately. A node and an item are not gated
        // here, but they are still runs of this project's work, and the ceiling is what stops one
        // that will not finish - the case this key exists for is a job item, which reaches this
        // function down the very arm that skips the gate.
        //
        // An unreadable file leaves this `None` and the run on the house ceiling rather than
        // refusing: the gate below already turns that into `Unreadable`, which stops the run with a
        // reason. A second refusal here would report the same fault as a different one.
        max_turns = rules.as_ref().ok().and_then(|rules| rules.max_turns);
        gate_config = match (node.is_some() || item.is_some(), rules) {
            (true, _) => GateConfig::NotConfigured,
            (false, Ok(rules)) => {
                let scoped_final = rules.scoped_final();
                rules
                    .gate_command
                    .map_or(GateConfig::NotConfigured, |command| GateConfig::Command {
                        command,
                        project_root: project_root.to_string(),
                        scoped_final,
                    })
            }
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
            Some(node) => crate::execution::worktree::WorktreeInfo {
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
                    Some(item) => crate::execution::worktree::Owner::Item(item.item_id),
                    None => crate::execution::worktree::Owner::Run(id),
                };

                // Anything that brings a tree claims a slot. A job node inherits its job's worktree,
                // and its job's slot with it — charging the node a second one would have a five-item
                // job refuse itself at the second item — and a node never reaches this arm.
                //
                // The table was swept before the row was inserted, at the pre-check above, so this
                // is the authoritative answer against an already-current count. `claim` is
                // idempotent per owner, which is what lets an item's retry ask again for the slot it
                // is already holding instead of leaking a second one.
                match crate::execution::concurrency::claim(&state.pool, worktree_project_id, owner)
                    .await
                {
                    Ok(crate::execution::concurrency::ClaimOutcome::Claimed(_)) => {}
                    Ok(crate::execution::concurrency::ClaimOutcome::Full(full)) => {
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
                // the two walls apart. A refusal here is not a failure: the disk is a condition of
                // the machine, the same shape as a full project, and a batch that cannot have its
                // third item should come out smaller rather than fail. It is not `Busy` either, and
                // that half was learned the hard way — see `CreateRunError::NoRoomOnDisk`.
                //
                // Only for an item, and not because the others are cheaper. A standalone run and a
                // resolution are asked for one at a time by a person or by the queue; items are
                // asked for several at once, and each of them grows a `target/` of its own.
                if item.is_some()
                    && let Some(refusal) = no_room_on_disk(std::path::Path::new(project_root)).await
                {
                    fail_provisioning(state, id, project_id.as_deref(), &refusal).await;
                    return Err(CreateRunError::NoRoomOnDisk(refusal));
                }

                // A resolution's tree is born on the merge's TARGET and an item's on the tip of its
                // job's branch; everything else starts where the project stands.
                // `worktree::create_at` carries why that is not a preference.
                let base = resolution
                    .map(|it| it.target.as_str())
                    .or_else(|| item.map(|it| it.base.as_str()))
                    .or_else(|| correction.map(|it| it.base.as_str()));
                // An item's name comes back, so its checkout and its branch may already be there —
                // from the attempt this one is retrying, or from a crash between creating the tree
                // and recording it. `adopt_or_create_at` is the difference between a retry that
                // continues and an item that fails every thirty seconds for ever on a name only it
                // can use.
                let created = match item {
                    Some(_) => {
                        crate::execution::worktree::adopt_or_create_at(
                            std::path::Path::new(project_root),
                            owner,
                            base,
                        )
                        .await
                    }
                    None => {
                        crate::execution::worktree::create_at(
                            std::path::Path::new(project_root),
                            owner,
                            base,
                        )
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
            && let Err(error) = crate::execution::worktree::record(
                &state.pool,
                match item {
                    Some(item) => crate::execution::worktree::Owner::Item(item.item_id),
                    None => crate::execution::worktree::Owner::Run(id),
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
                    .bind(node.stage.as_str())
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
            .bind(item.stage.as_str())
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
        // The project's pinned workflow, into a tree this run brought — never into a job's tree a
        // node inherits, which got it when the job opened it. Fail-soft and said out loud: see
        // `workflow_materialize::into_worktree`. Before a resolution stages its conflict, so the
        // resolver reads the same rules every other agent in this project does.
        if owns_a_tree {
            crate::workflows::materialize::into_worktree(
                &state.pool,
                state.machine_config_root.as_deref(),
                state.workflow_library.as_deref(),
                worktree_project_id,
                &info.path,
            )
            .await;
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
                crate::execution::worktree::stage_conflict(&info.path, &resolution.source).await
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
            match crate::execution::worktree::prepare_artifacts(&info.path).await {
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
    let runner = if mode == crate::integrations::email::TRIAGE_MODE {
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
    if let (Some(root), Some(tree)) = (cwd.as_deref(), spawn_cwd.as_deref()) {
        let (root, tree) = (std::path::Path::new(root), tree);
        if root != tree {
            inherit_classifier_hook(root, tree);
        }
    }
    // A node pinned to Codex is shaped for Codex up front: `codex exec` has no `PreToolUse` hook and
    // no permission modes, so the classifier does not govern it and `Bypass` would be refused. Its
    // barrier is the sandbox the pin carries (derived from the agent's capabilities). Only when the
    // router holds Codex, since otherwise the pin never reaches a Codex runner and the run keeps the
    // shape it would have had.
    let pinned_to_codex = node.is_some_and(|node| {
        node.launch
            .pin
            .as_ref()
            .is_some_and(|pin| pin.runner == crate::models::route_advice::RunnerKind::Codex)
    }) && state.runner.router().is_some_and(|router| {
        router
            .available(crate::models::route_advice::RunnerKind::Codex)
            .is_some()
    });
    let governed_by_classifier =
        !pinned_to_codex && classifier_governs_tools(mode, tool_policy, spawn_cwd.as_deref());
    // An unattended, unrestricted run the classifier does not govern runs with nothing watching it
    // and the CLI's barrier still up; say so on the feed instead of leaving it to be discovered.
    // Not for a Codex pin, where the sandbox is the barrier by design.
    if runs_unattended(mode)
        && tool_policy == crate::execution::runner::ToolPolicy::Unrestricted
        && !plan_only
        && !governed_by_classifier
        && !pinned_to_codex
    {
        let spawn_dir = spawn_cwd.as_deref().map_or_else(
            || "(no directory)".to_owned(),
            |dir| dir.display().to_string(),
        );
        tracing::warn!(run_id = id, %spawn_dir, "run launched without the classifier");
        let _ = crate::feed::append(
            &state.pool,
            project_id.as_deref(),
            "run_launched_unclassified",
            &format!("run {id} was launched without the classifier governing it (in {spawn_dir})"),
            Some(id),
            Some(&crate::feed::run_subject(&state.pool, id).await),
        )
        .await;
    }
    // Which configured role this run's model and effort come from: a job node's stage, or the
    // resolver's own stage name for a conflict resolution. Asked of the runner at spawn below.
    let model_stage = node.map(|node| node.stage.as_str()).or(resolution
        .as_ref()
        .map(|_| crate::execution::runner::RESOLVE_STAGE));
    // What the llm-router is told. Never for triage: mail text must not leave the machine. The
    // item's history is read now, before `job.rs` points the item at this run and the attempt that
    // failed it stops being reachable from there.
    let route = if mode == crate::integrations::email::TRIAGE_MODE {
        None
    } else {
        let item_ref = match (node, item) {
            (Some(node), _) => {
                node.item_ordinal
                    .map(|ordinal| crate::models::route_advice::ItemRef::Ordinal {
                        job_id: node.job_id,
                        ordinal: ordinal as i64,
                    })
            }
            (None, Some(item)) => Some(crate::models::route_advice::ItemRef::Id(item.item_id)),
            (None, None) => None,
        };
        let item_context = match item_ref {
            Some(item_ref) if state.runner.router().is_some() => {
                Some(crate::models::route_advice::item_context(&state.pool, item_ref, id).await)
            }
            _ => None,
        };
        Some(crate::models::route_advice::RouteQuery {
            task: prompt.clone(),
            // A resolution names its own stage, so the router knows a merge conflict from an
            // ad-hoc run and can choose its model and effort like any job stage's.
            stage: node
                .map(|node| node.stage.as_str())
                .or(item.map(|item| item.stage.as_str()))
                .or(resolution.map(|_| crate::execution::runner::RESOLVE_STAGE))
                .map(str::to_owned),
            item: item_context,
            resume: false,
            restrict_runners: node.and_then(|node| node.launch.restrict_runners.clone()),
            pin: node.and_then(|node| node.launch.pin.clone()),
            judged_later: node.is_some_and(|node| node.launch.judged_later),
            effort: node.and_then(|node| node.launch.effort.clone()),
        })
    };
    // Folded HERE, where both facts are in hand, rather than in `cli_args` where they used to be
    // two fields and an `else if`. A shadow run is planning and stays planning even in a tree that
    // wires the classifier — which is the old precedence, kept, but now as a value that cannot be
    // two things at once rather than as an order two writes happen in.
    let permission = if plan_only {
        crate::execution::runner::Permission::Plan
    } else if governed_by_classifier {
        crate::execution::runner::Permission::Bypass
    } else {
        crate::execution::runner::Permission::Default
    };
    // Read here rather than in `spawn_run`: provisioned job nodes are already briefed, and that
    // lower seam also launches handoff successors and approval resumes. Append to the prompt so
    // Codex can receive it; its runner refuses an appended system prompt. Triage and provisioned
    // runs stay untouched, while the row keeps the prompt exactly as the caller asked it.
    let prompt = if standalone && mode != crate::integrations::email::TRIAGE_MODE {
        let context = crate::knowledge::Context::for_project(project_id.as_deref());
        match crate::brief::for_prompt(&state.pool, &context, &prompt, "run").await {
            Some(briefing) => {
                if let Err(error) =
                    crate::brief::record(&state.pool, id, None, &briefing.trace).await
                {
                    tracing::warn!(run_id = id, %error, "could not record a run's briefing trace");
                }
                match briefing.block {
                    Some(block) => format!("{prompt}{block}"),
                    None => prompt,
                }
            }
            None => prompt,
        }
    } else {
        prompt
    };
    // Written before the CLI starts, so the hook and the loadout routes find the row on the run's
    // first call. A failed write is not fatal: the run then has its box's base tools only.
    let run_tools = node
        .and_then(|node| node.loadout.as_ref())
        .or(item.and_then(|item| item.loadout.as_ref()));
    if let Some(run_tools) = run_tools
        && let Err(error) = crate::loadout::record(&state.pool, id, run_tools).await
    {
        tracing::warn!(run_id = id, %error, "could not record a run's loadout");
    }
    spawn_run(
        state,
        runner,
        id,
        prompt,
        project_id,
        spawn_cwd,
        permission,
        None,
        session_id,
        false,
        completion_feed,
        gate_config,
        max_attempts,
        tool_policy,
        run_env(
            &daemon_token,
            id,
            node_artifacts.as_deref(),
            crate::speed::Capacity::solo(),
        ),
        steerable,
        run_timeout_for_mode(state.run_timeout, mode),
        progress_timeout_for_mode(state.progress_timeout, mode),
        governed_by_classifier,
        // A job node's stage is what may be routed elsewhere, and so is a conflict resolution, under
        // the stage name only it carries; every other run names no stage and so stays on the
        // runner's own model and the CLI's own effort.
        state.runner.model_for_stage(model_stage),
        state.runner.effort_for_stage(model_stage),
        route,
        JobNodeMcp::for_run(
            id,
            node.as_ref()
                .map(|node| node.job_id)
                .or(item.as_ref().map(|item| item.job_id)),
        )
        .map(|mcp| match run_tools {
            Some(run_tools) => mcp.with_loadout(run_tools),
            None => mcp,
        }),
        max_turns,
    );

    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::runs::tests::{
        WorktreeRootEnv, init_contained_repo, space_free_tempdir, test_state_with_runner,
    };

    /// A node's stage is whatever the workflow graph's node id is, not one of three literals: the
    /// run row records it verbatim.
    // The env lock is held across awaits on purpose: it serialises tests that set process-wide
    // env vars.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn a_job_node_run_records_any_stage_string() {
        let _env_lock = crate::execution::worktree::test_env_lock();
        let wt_root = space_free_tempdir("nucleos-create-stage-wt-");
        let _env = WorktreeRootEnv::set(wt_root.path());
        let (_repo_container, repo) = init_contained_repo("nucleos-create-stage-");
        let (state, _runner) =
            test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        let project_root = repo.to_string_lossy().into_owned();

        let job_id = crate::execution::job::insert_job(
            &state.pool,
            &crate::execution::job::NewJob {
                project_id: "proj",
                project_root: &project_root,
                rule_name: Some("nightly"),
                prompt: "triage the backlog",
                max_items: 5,
                gate_each: true,
                review: true,
                gate_retries: 0,
                head_sha: None,
                max_rounds: None,
                budget_usd: None,
                team_id: None,
                graph: None,
            },
        )
        .await
        .expect("start a job");
        let owner = crate::execution::worktree::Owner::Job(job_id);
        let info = crate::execution::worktree::create(&repo, owner)
            .await
            .expect("the job provisions its worktree once");
        crate::execution::worktree::record(
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
            "sort the inbox".into(),
            "proj".into(),
            project_root.clone(),
            JobNode {
                job_id,
                stage: "triage".into(),
                worktree_path: info.path.to_string_lossy().into_owned(),
                branch: info.branch.clone(),
                item_ordinal: None,
                loadout: None,
                launch: Default::default(),
            },
        )
        .await
        .expect("a node with any stage string starts");

        let (stage, node_job_id): (String, i64) =
            sqlx::query_as("SELECT stage, job_id FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(stage, "triage");
        assert_eq!(node_job_id, job_id);
    }
}
