//! Declared git and GitHub operations and land targets (`/projects/{id}/git-ops`, `/github-ops`, `/land-targets`, `/github/*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/github-ops",
            get(get_project_github_ops)
                .post(post_project_github_op)
                .delete(delete_project_github_op),
        )
        // The git queue's own declarable set, the same trio as the one above with `vcs::GIT_OP_KINDS`
        // in place of `github::declarable_ops` — see that route's own doc for what stays true wholesale
        // (the emergency stop on the POST only, the DELETE's body-carried `op_kind`, the 404 off
        // `rows_affected`) and `GET /vcs/declarable-ops` for the one thing that does not: there is no
        // ceiling to intersect, so every kind here is declarable, always.
        .route(
            "/projects/{id}/git-ops",
            get(get_project_git_ops)
                .post(post_project_git_op)
                .delete(delete_project_git_op),
        )
        .route(
            "/projects/{id}/land-targets",
            get(get_project_land_targets)
                .post(post_project_land_target)
                .delete(delete_project_land_target),
        )
        // Which repository on GitHub this project IS — the one fact that stood between the typed
        // reads and a page able to show a project's remote. `Repo::new` was reachable only from
        // `ReadOp::from_request`, so the repository was always something the caller named;
        // `vcs::resolve_repo` answers with a folder on this disk, which is a different question with
        // a confusingly similar name.
        //
        // **Its own route rather than a field on a read this page already makes**, and the two
        // candidates were real ones. `ProjectSummary` would have cost nothing extra to fetch — the
        // roster is already on screen — and `GET /projects/{id}/branches` is already a git subprocess
        // against the same root, which the section below this one already calls. Both were rejected
        // for the same reason, and it is the reason this route is in no scope table: they are BOTH
        // in `READ_ONLY_ROUTES`, so hanging the repository off either would disclose it to every
        // read-only key as a side effect of tidying a round trip. The roster carries a second cost
        // besides — it is polled every three seconds, and this spawns git, so a fleet of
        // twenty-five projects would be twenty-five subprocesses a tick to draw a panel one page
        // reads.
        //
        // **In no scope table**, with `the_repository_a_project_points_at_is_in_no_scope_table`
        // carrying the argument — including the strongest case against it, which is that a
        // read-only key can already read `.git/config` through `GET /projects/{id}/cat`.
        .route("/projects/{id}/github-repo", get(get_project_github_repo))
        // Admin-only by construction: absent from BOTH scope tables in `auth.rs`, for the
        // `POST /email/send` reason rather than the `POST /runs` one. It is not out of a scoped
        // key's reach because it is expensive; it is out of reach because it LEAVES THE MACHINE.
        //
        // ONE route for both tools, and what makes that safe is exactly the line above: it is
        // unreachable by a `Scope::Run`, so it is not a second door for the agent the tools serve.
        // The partition between reading and acting is held by the parameter TYPES at the tool
        // boundary, never by the transport, which takes an `Op` and executes it.
        .route("/github/requests", post(submit_github_request))
        // The catalogue `POST /projects/{id}/github-ops` validates against, read out loud.
        //
        // **Machine-wide, so it hangs off no project, and the path says so.** The set is
        // `github::declarable_ops` — the operations this binary can build, intersected with two
        // compiled ceilings — and it is the same answer for every project on the roster. Under
        // `/projects/{id}/…` it would be a route whose `{id}` changed nothing about the response,
        // which is the shape that teaches a caller the set is per-project when it is per-BUILD.
        // `/github` because that is the noun: it sits beside `/github/requests` for the same reason
        // `/workflows/library` sits apart from a project's pins — one is the house's, the other is
        // this project's use of it.
        //
        // **In no scope table either, and NOT for `/github/requests`' reason.** That one is
        // admin-only because it leaves the machine; this reaches nothing and discloses no project's
        // configuration, no repository and no secret — the response is a function of the binary, so
        // two callers on the same version get the same bytes. What keeps it out is that the grant
        // would buy nobody anything: the page that needs it is the shell, which holds the control
        // token, and the twelve routes it exists to explain are themselves in no table. A key that
        // cannot read what a project HAS declared has no use for the list of what MAY be declared —
        // and handing it that list is a map of exactly which capability names to try. An unneeded
        // grant is one more thing to be wrong about later, which is `GET /vcs/requests/{id}/wait`'s
        // argument for its own absence. `the_declarable_ops_catalogue_is_in_no_scope_table` says it
        // where somebody tidying `auth.rs` will read it.
        .route("/github/declarable-ops", get(get_declarable_github_ops))
}

/// Every operation kind a project may declare: the ops this núcleo can actually build, narrowed to
/// what the two compiled ceilings admit.
///
/// **The derivation lives in `github.rs` and this is its name here**, because the WRITE side is no
/// longer its only reader: `Policy::for_project` narrows a project's stored rows against the same
/// set at decision time, for the rows this route never saw — one written before a ceiling moved, or
/// written out of band. Two spellings of that set is how the two sides would come to disagree about
/// which is which, and the disagreement would be silent in the direction that grants.
///
/// Two consequences worth stating here, because both look like bugs at this route and are not:
///
/// - **`api_read` is refused.** It is deliberately outside `ACTION_CEILING` — not even
///   `~/.nucleos/github.yaml` can turn it on — so it is outside this too. A project able to declare it
///   would be a way round the ceiling wearing a different route.
/// - **`run_logs` is refused, and it is a `ReadOp` whose prefix the ceiling admits.** Its argv
///   carries `--log`, which is in `REFUSED_READ_FLAGS`; `declarable_read_ops` says why in full.
///
/// `workflow_list` used to be a third: in `READ_CEILING` and undeclarable, because no `ReadOp` built
/// it. `ReadOp::WorkflowList` made it declarable with nothing here edited, which is what "derived,
/// never written out" was supposed to buy.
pub(super) fn declarable_github_ops() -> Vec<&'static str> {
    crate::integrations::github::declarable_ops()
}

/// One GitHub operation this núcleo can build, and where it stands against the ceilings.
///
/// **Every operation with a flag, and not the admitted names alone.** Two lists of strings, or one
/// list of the declarable, would have fixed a picker and left the other half unsayable — and the
/// half that cannot be said is the one that matters. `api_read` is outside `ACTION_CEILING`: no
/// route, no `~/.nucleos/github.yaml` and no owner can turn it on. A page that only knew the admitted names
/// would either omit it, which quietly claims this núcleo cannot do it at all, or draw a control for
/// it — and a checkbox that cannot be switched on is a lie about who decides. `declarable: false` is
/// how an operation gets drawn as a FACT: it exists, it is refused here, and nothing on this screen
/// is the thing that would change that.
///
/// Rows for `ShellRuleView`'s reason, restated because the two shapes landed together: a row that
/// travels alone keeps its facts, and a name lifted out of a `declarable` list into a form has
/// already forgotten which list it came from.
#[derive(serde::Serialize)]
pub(super) struct DeclarableOpView {
    pub(super) kind: &'static str,
    pub(super) half: &'static str,
    pub(super) declarable: bool,
}

/// Every GitHub operation kind, and whether a project may declare it.
///
/// **The route exists because this set had one way out and it was a refusal.** `post_project_github_op`
/// validates against `declarable_github_ops`, and until now the only place that set reached the wire
/// was inside `undeclarable_op`'s `detail` — so a caller building a picker had to hardcode the list,
/// which drifts from the ceilings in silence, or discover it by POSTing something invalid. Neither
/// is a way to read a constant.
///
/// No project id and no roster check, because there is nothing project-shaped to check: the answer
/// is compiled in. It cannot fail, which is why it returns `Json` and not `Result` — the one route
/// in this family with no database under it.
pub(super) async fn get_declarable_github_ops() -> Json<Vec<DeclarableOpView>> {
    Json(
        crate::integrations::github::every_op()
            .into_iter()
            .map(|standing| DeclarableOpView {
                kind: standing.kind,
                half: standing.half,
                declarable: standing.declarable,
            })
            .collect(),
    )
}

/// What the GitHub manager may do on this project's remote without asking.
///
/// `try_github_ops` and not `github_ops`: this is a DISPLAY, and to a page whose job is showing the
/// owner what their project may do, `[]` is a positive claim that nothing is declared rather than
/// an absence of information. The swallowing reader is right for the classifier's path and wrong
/// here — see its own doc.
pub(super) async fn get_project_github_ops(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<String>>, StatusCode> {
    crate::project::policy::try_github_ops(&state.pool, &id)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "reading a project's github ops failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// Which repository on GitHub this project is.
///
/// **The fact the typed reads were missing.** Every `ReadOp` carries a `Repo`, and `Repo::new` was
/// reachable only from `ReadOp::from_request` — so the repository was always something the CALLER
/// named, and nothing in this daemon could answer it for a project on the roster. A page that wanted
/// to show a project's open pull requests had either to invent the mapping itself out of
/// `GET /projects/detect` and a URL parse, or to show nothing. `github::project_repo` is that answer,
/// and this is the door onto it.
///
/// **A 200 for every state but a project nobody has heard of.** A project with no folder, a folder
/// that has moved, a folder git does not know, a repository with no `origin` and a remote that is
/// not GitHub's are five ordinary states and not five failures — §5.1 asks this page to EXPLAIN
/// rather than go blank, and it can only do that if the five arrive distinguishable. Collapsing them
/// into one refusal would leave the page saying "something is wrong" about a project whose only
/// peculiarity is being local-only.
pub(super) async fn get_project_github_repo(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::integrations::github::ProjectRepo>, StatusCode> {
    let deadline = std::time::Instant::now() + DETECT_GIT_BUDGET;
    match crate::integrations::github::project_repo(&state.pool, &id, deadline).await {
        Ok(Some(found)) => Ok(Json(found)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, project_id = %id, "resolving a project's repository failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GithubOpTarget {
    pub(super) op_kind: String,
}

/// Refuses a blank op kind before anything else can name it.
///
/// Its own refusal rather than falling through to `undeclarable_op`, whose message would name the
/// empty string — from the one route family whose stated rule is that a refusal must say what was
/// wrong with what it was given.
pub(super) fn named_op_kind(op_kind: &str) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if op_kind.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "refusal": "empty_op_kind",
                "detail": "a github declaration has to name an operation",
            })),
        ));
    }
    Ok(())
}

/// Grants one GitHub operation to this project without asking.
///
/// **Idempotent, and not by this handler's arithmetic.** `declare_github_op`'s
/// `ON CONFLICT (project_id, op_kind) DO NOTHING` is what makes a second POST leave the first row
/// and its `created_at` alone, rather than raising `UNIQUE constraint failed`;
/// `github_ops_and_land_targets_round_trip_and_forget` declares `run_list` twice to pin it.
///
/// Gated by the emergency stop, because every operation declarable here is one an autonomous run
/// performs without asking — this POST widens, immediately, exactly as an `allow` does. See
/// `declaration_halted`.
///
/// The refusal names the kind AND lists what would have been accepted, because "not a declarable
/// operation" without the set is a sentence that sends the owner to `github.rs`.
///
/// **A declared ACTION is recorded and inert today, and this route accepts it anyway.** The reading
/// half reaches the classifier through `Policy::for_project`; the acting half is only ever consulted
/// by `github::submit`, which reads the machine default because `POST /github/requests` carries no
/// project to look one up by. So `pr_create` here returns 204, is stored, and changes no verdict
/// until the typed door learns which project it is acting for — `Policy::for_project` carries the
/// reasoning and the reason it was not taken here. Inert is the safe direction, which is why this
/// route does not refuse it; what it costs is an owner reading a list of grants that includes one
/// they did not get, so the page has to SAY so beside it rather than showing the two alike.
pub(super) async fn post_project_github_op(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<GithubOpTarget>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    declaration_halted(&state, &id).await?;
    project_is_on_the_roster(&state, &id).await?;

    let op_kind = body.op_kind.trim();
    within_length("op kind", op_kind)?;
    named_op_kind(op_kind)?;

    let declarable = declarable_github_ops();
    if !declarable.contains(&op_kind) {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "refusal": "undeclarable_op",
                "detail": format!(
                    "`{op_kind}` is not an operation a project may declare; the ceilings admit: {}",
                    declarable.join(", ")
                ),
            })),
        ));
    }

    crate::project::policy::declare_github_op(&state.pool, &id, op_kind)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "declaring a project github op failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Withdraws one GitHub operation. 404 when it was never declared, off the DELETE's own
/// `rows_affected` — for `delete_project_shell_rule`'s reason, and here that is not merely tidier.
/// A presence check through `github_ops` would read an unreadable table as an empty list and tell
/// the owner by name that an operation was never declared, while the row stands and the operation
/// goes on running unattended. That is this route's own stated failure mode arriving through a 404
/// instead of a 204.
///
/// No kill switch, and no declarability check: withdrawing narrows, and an operation stored before
/// the ceilings moved still has to be removable.
pub(super) async fn delete_project_github_op(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<GithubOpTarget>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    project_is_on_the_roster(&state, &id).await?;

    let op_kind = body.op_kind.trim();
    within_length("op kind", op_kind)?;
    named_op_kind(op_kind)?;

    let forgotten = crate::project::policy::forget_github_op(&state.pool, &id, op_kind)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "forgetting a project github op failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;
    if !forgotten {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "refusal": "no_such_op",
                "detail": format!("{id} has not declared `{op_kind}`"),
            })),
        ));
    }

    Ok(StatusCode::NO_CONTENT)
}

/// What the shared git queue may do on this project's repository, for an autonomous run, without
/// asking.
///
/// `try_git_ops` and not `git_ops`, for `get_project_github_ops`'s reason: this is a DISPLAY, and
/// `[]` has to mean "nothing declared" rather than stand in for a read that failed.
pub(super) async fn get_project_git_ops(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<String>>, StatusCode> {
    crate::project::policy::try_git_ops(&state.pool, &id)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "reading a project's git ops failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GitOpTarget {
    pub(super) op_kind: String,
}

/// Refuses a blank op kind before anything else can name it — `named_op_kind`'s own reason, worded
/// for this family instead of GitHub's: reusing that message verbatim would tell somebody declaring
/// a git operation that their GitHub declaration was malformed.
pub(super) fn named_git_op_kind(
    op_kind: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if op_kind.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "refusal": "empty_op_kind",
                "detail": "a git declaration has to name an operation",
            })),
        ));
    }
    Ok(())
}

/// Grants one git operation to this project's autonomous runs without asking.
///
/// The git twin of `post_project_github_op`, and the checks run in the same order: the emergency
/// stop first, because this widens exactly as a GitHub `allow` does; then the roster; then the
/// length cap; then the blank-kind refusal; then the catalogue. Unlike its sibling there is no
/// inert-until-elsewhere caveat to record — `GIT_OP_KINDS` is exactly what the shared git queue
/// consults, with nothing standing between a declared row and the queue honouring it.
pub(super) async fn post_project_git_op(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<GitOpTarget>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    declaration_halted(&state, &id).await?;
    project_is_on_the_roster(&state, &id).await?;

    let op_kind = body.op_kind.trim();
    within_length("op kind", op_kind)?;
    named_git_op_kind(op_kind)?;

    if !vcs::GIT_OP_KINDS.contains(&op_kind) {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "refusal": "undeclarable_op",
                "detail": format!(
                    "`{op_kind}` is not an operation a project may declare; the queue admits: {}",
                    vcs::GIT_OP_KINDS.join(", ")
                ),
            })),
        ));
    }

    crate::project::policy::declare_git_op(&state.pool, &id, op_kind)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "declaring a project git op failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Withdraws one git operation. 404 when it was never declared, off the DELETE's own
/// `rows_affected` — `delete_project_github_op`'s reason, unchanged here: a presence check through
/// `git_ops` would read an unreadable table as an empty list and report a false 404 while the row,
/// and the operation, stand.
///
/// No kill switch: withdrawing narrows, and an operation declared before this route existed still
/// has to be removable.
pub(super) async fn delete_project_git_op(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<GitOpTarget>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    project_is_on_the_roster(&state, &id).await?;

    let op_kind = body.op_kind.trim();
    within_length("op kind", op_kind)?;
    named_git_op_kind(op_kind)?;

    let forgotten = crate::project::policy::forget_git_op(&state.pool, &id, op_kind)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "forgetting a project git op failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;
    if !forgotten {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "refusal": "no_such_op",
                "detail": format!("{id} has not declared `{op_kind}`"),
            })),
        ));
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Everywhere a `--land` may be sent in this project: the integration branch, and the extra targets.
///
/// **Both halves or neither, because half of this answer is a trap.** The extra targets used to be
/// served alone, and the page filled the other half in from `GET /projects/{id}/branches` —
/// `inspect::Branches::integration`, which is `current_branch(project_root)`. Its own doc says what
/// that costs: it is *"a heuristic for this panel"* kept from before `land.rs` existed, and the
/// design that created `land.rs` killed that read *"precisely because a checkout parked on the wrong
/// branch silently redirected every landing"*. So a project whose clone sat on a feature branch had
/// the page name that branch and caption it admissible, while `land::resolve_target` refused it by
/// name and the branch that IS admissible appeared nowhere. Serving the pair from the one route that
/// owns "where a landing may be sent" is what stops the two halves being answered by two different
/// modules with two different definitions.
///
/// **`integration_branch_reading` and NOT `integration_branch`**, and the difference is a write. The
/// landing path records a derived default so that later landings read a column; doing that here
/// would let opening a page pin a project's integration branch to whatever `origin/HEAD` said at
/// that moment. Its own doc carries the argument.
///
/// `try_land_targets` for `get_project_github_ops`' reason: a display must not serve an empty list
/// out of a database error.
pub(super) async fn get_project_land_targets(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<LandingView>, StatusCode> {
    let targets = crate::project::policy::try_land_targets(&state.pool, &id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "reading a project's land targets failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let deadline = std::time::Instant::now() + DETECT_GIT_BUDGET;
    let integration = crate::execution::land::integration_branch_reading(
        &state.pool,
        &id,
        deadline,
    )
    .await
    .map_err(|error| {
        tracing::warn!(%error, project_id = %id, "reading a project's integration branch failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(LandingView {
        integration,
        targets,
    }))
}

/// Where this project's work lands: the default, and the alternatives it admits.
///
/// The two are not a list. The integration branch is admissible with no row at all — an empty table
/// means "only the usual place" and never "nowhere" — so folding it into `targets` would make it
/// indistinguishable from a declared row, which is the one thing on that panel that can be closed.
#[derive(Serialize)]
pub(super) struct LandingView {
    pub(super) integration: crate::execution::land::IntegrationBranch,
    pub(super) targets: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LandTargetTarget {
    pub(super) branch: String,
}

/// Opens one more landing target for this project.
///
/// **A branch that does not exist yet is ACCEPTED, and that is deliberate — do not "fix" it.**
/// Existence is checked at the moment of landing, by `land::resolve_target`, which confirms
/// `refs/heads/<branch>` against the repository and refuses by name when it is gone. Declaring a
/// target before creating it is a reasonable order to work in — the branch is often made by the very
/// run that will land into it — and a check here would only be a second, staler copy of one that
/// already runs at the moment it can be right.
///
/// **The NAME, unlike its existence, is checked here.** `vcs::Branch::new` is pure, and
/// `resolve_target` already calls it before it asks git — its own comment says a name `Branch::new`
/// rejects "would otherwise be told it does not exist, which sends the caller looking for a missing
/// branch when the problem is the name". Asking it at declaration time means that sentence is read
/// by the person typing the name rather than by whoever is landing three weeks later. It is the
/// same "a rule stored that can never fire" the shell route refuses, in the one form available here.
///
/// **What that check is and is not.** `Branch::new` is `argv_safe`: it refuses an empty name, a
/// leading `-`, and whitespace or control characters — the properties that decide whether a name may
/// be handed to git as an argument. It is explicitly NOT a ref-name validator, and its own doc
/// refuses to be widened into one, so `..dots.lock` passes here and git rejects it at landing. That
/// residue is git's to own; `git check-ref-format` is the tool for it and is a subprocess, which is
/// the thing this route deliberately does not spawn.
///
/// Idempotent by `declare_land_target`'s `ON CONFLICT (project_id, branch) DO NOTHING`, like its
/// GitHub sibling above. Not gated by the emergency stop: a target binds nothing until a landing is
/// attempted, and a landing goes through the queue, which the stop governs at its own start point.
pub(super) async fn post_project_land_target(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<LandTargetTarget>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    project_is_on_the_roster(&state, &id).await?;

    let branch = body.branch.trim();
    within_length("branch", branch)?;
    // `Branch::new` refuses an empty name itself, and its message says so — one sentence for the
    // whole shape of a name rather than two that could disagree about the blank case.
    let branch = crate::execution::vcs::Branch::new(branch).map_err(|why| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "refusal": "unusable_branch",
                "detail": why,
            })),
        )
    })?;

    crate::project::policy::declare_land_target(&state.pool, &id, branch.as_str())
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "declaring a project land target failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Closes one landing target. 404 when it was never open, off the DELETE's own `rows_affected` —
/// for `delete_project_github_op`'s reason, including the unreadable-table half.
///
/// `integration_branch` needs no row to stay admissible, so a DELETE naming it answers 404 and takes
/// nothing away — which is the honest answer: there was no target of that name to close.
///
/// No `Branch::new` here, and no kill switch: a target stored before that check existed still has to
/// be closable, and closing one narrows.
pub(super) async fn delete_project_land_target(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<LandTargetTarget>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    project_is_on_the_roster(&state, &id).await?;

    let branch = body.branch.trim();
    within_length("branch", branch)?;
    if branch.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "refusal": "empty_branch",
                "detail": "a landing target has to name a branch",
            })),
        ));
    }

    let forgotten = crate::project::policy::forget_land_target(&state.pool, &id, branch)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "forgetting a project land target failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;
    if !forgotten {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "refusal": "no_such_target",
                "detail": format!("{id} has not declared `{branch}` as a landing target"),
            })),
        ));
    }

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(super) struct GithubRequestBody {
    /// The operation, as data. Deserialized through `github`'s validating node types, so a dashed
    /// string is refused HERE rather than reaching an argv.
    pub(super) op: crate::integrations::github::Op,
}

/// A second refusal of a call `auth.rs` has already refused, and deliberately so.
///
/// `POST /github/requests` is absent from both scope tables, so `permits` lets only the control
/// token and an Admin key reach this at all. This is not that boundary and must not be read as one
/// — it is the second of two independent refusals at a place that leaves the machine, which is what
/// `hooks.rs` says one wants at a boundary.
pub(super) fn github_caller_is_allowed(scope: &Scope) -> Result<(), StatusCode> {
    match scope {
        Scope::Control | Scope::ApiToken(ApiTokenLevel::Admin) => Ok(()),
        Scope::Run(_) | Scope::Service(_) | Scope::TeamRun(_) | Scope::ApiToken(_) => {
            Err(StatusCode::FORBIDDEN)
        }
    }
}

/// The one door both GitHub tools come through.
///
/// **Inside `uncancellable`, and by instruction rather than by precaution.** `submit_vcs_request`'s
/// doc names the exact condition that makes it necessary — "it stops being benign the moment
/// submitting becomes two writes … whoever writes that second write moves this through
/// `uncancellable` at the same time" — and this handler is that second write: it publishes to GitHub
/// and then records a proposal, or it records a proposal that a person will later act on. A client
/// disconnecting between the two would leave a comment posted and nothing saying so.
///
/// The cost is real and not a wrapper for free: `uncancellable` is `tokio::spawn`, so the work must
/// be `Send + 'static` and this body OWNS everything it touches. And it covers DISCONNECTION, not
/// panic — a panicking task becomes a 500 with the task gone, which for a path that has already
/// posted a comment is the same silence it protects against in the other case. Said here so nobody
/// reads the protection as larger than it is.
pub(super) async fn submit_github_request(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(body): Json<GithubRequestBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    github_caller_is_allowed(&scope).map_err(|status| {
        (
            status,
            "reaching GitHub is the owner's, not a run's".to_owned(),
        )
    })?;
    let submitted = uncancellable(async move {
        crate::integrations::github::submit(&state.pool, &state.github, body.op).await
    })
    .await
    .map_err(|status| (status, "the github task did not finish".to_owned()))?;

    match submitted {
        Ok(crate::integrations::github::Submitted::Ran(outcome)) => Ok(Json(serde_json::json!({
            "status": "ran",
            "operation": outcome.kind,
            "exit_code": outcome.exit_code,
            "stdout": outcome.stdout,
            "output_tail": outcome.output_tail,
        }))),
        // 200 and not 202: the turn is not waiting for this and there is nothing to poll. What the
        // caller needs is the number a person will see beside it.
        Ok(crate::integrations::github::Submitted::Filed { proposal_id, kind }) => {
            Ok(Json(serde_json::json!({
                "status": "filed_for_approval",
                "operation": kind,
                "proposal_id": proposal_id,
                "detail": format!("filed for approval as #{proposal_id}; the turn continues"),
            })))
        }
        Err(failure) => Err((github_failure_status(&failure), failure.to_string())),
    }
}
