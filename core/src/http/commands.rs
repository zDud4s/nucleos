//! A project's commands, rules and limits (`/projects/{id}/commands*`, `/rules`, `/shell-rules`, `/wip-limit`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/rules", get(get_project_rules))
        .route("/projects/{id}/wip-limit", post(post_project_wip_limit))
        // What this project can be asked to do to itself. The literal `commands` ahead of nothing
        // ambiguous; the run route is a third segment under a command's own id, because running one
        // is an action ON that command and not a second way of listing them.
        .route(
            "/projects/{id}/commands",
            get(get_project_commands).post(post_project_command),
        )
        .route(
            "/projects/{id}/commands/{command_id}",
            delete(delete_project_command),
        )
        .route(
            "/projects/{id}/commands/{command_id}/run",
            post(post_project_command_run),
        )
        // The reach this project declares for itself: what its worktrees may run unattended, what
        // the GitHub manager may do on its remote, what the shared git queue may do on its
        // repository, and where a landing may be sent. `project/policy.rs` holds all four tables
        // and says why they are one module; until these routes there was no way into them but SQL
        // typed by hand.
        //
        // **In no table in `auth.rs`, the GETs included, and that absence is the boundary.** These
        // are not a configuration edit that takes effect at the next start — the hook reads the
        // shell table per decision and caches nothing, so one `allow` row binds the very next tool
        // call of every in-flight run of this project. `a_projects_declared_reach_is_in_no_scope_table`
        // carries the argument, and is what stops the four GETs being filed beside the
        // `/projects/{id}/…` reads they share a prefix with.
        //
        // **The DELETEs carry what they delete in the BODY.** A shell prefix is not a safe path
        // segment: it contains spaces, slashes and dots, and encoding one into a route only to
        // decode it again buys nothing — the same reason `POST /contacts/verdict` takes the address
        // in the body. The other three follow it rather than splitting the shape four ways, since a
        // branch name carries slashes too.
        //
        // **The emergency stop reaches three of these twelve, and that is the one place in this file
        // where `post_project_command`'s argument does not transfer.** It says a route that writes a
        // database row "starts nothing", which holds because every reader of ITS rows is a start
        // point and the stop guards every start point. The rows written here are read by `hooks.rs`,
        // per decision, and `hooks.rs` calls `kill_switch_engaged` nowhere; `set_kill_switch` flips
        // a flag and cancels nothing already running. So `POST shell-rules` with `allow` and `POST
        // github-ops` and `POST git-ops` are gated — they widen what in-flight runs may do,
        // immediately — while a `deny`, `POST land-targets` and all four DELETEs stay open, because
        // a stop that stopped somebody NARROWING autonomy would be holding the door open on the way
        // out.
        // `declaration_halted` carries the argument in full.
        //
        // **Two refusal shapes reach a caller here, and the page will need both.** Everything these
        // handlers refuse is `{"refusal", "detail"}`; a body axum cannot deserialize at all —
        // malformed JSON, a missing field, an unknown one now that all four structs carry
        // `deny_unknown_fields` — is rejected by the extractor before any of this runs, and arrives
        // as axum's own `text/plain`. That is true of every JSON route in this file rather than
        // something these twelve introduce, and it is written down here because these are the routes
        // whose refusals a person is meant to read.
        .route(
            "/projects/{id}/shell-rules",
            get(get_project_shell_rules)
                .post(post_project_shell_rule)
                .delete(delete_project_shell_rule),
        )
}

/// One rule's recorded scheduler state, as the tick writes it.
///
/// Named rather than left as a five-element tuple because the fields are read by position in three
/// places here, and `fires_date` and `fires_today` are the pair whose meaning depends entirely on
/// being read together — a count carrying yesterday's date is a count of nothing.
#[derive(sqlx::FromRow)]
pub(super) struct RuleRunState {
    pub(super) rule_name: String,
    pub(super) last_fired_at: String,
    pub(super) last_head_sha: Option<String>,
    pub(super) fires_date: Option<String>,
    pub(super) fires_today: i64,
    pub(super) command_outcome: Option<String>,
    pub(super) command_exit_code: Option<i64>,
    pub(super) command_output: Option<String>,
    pub(super) command_ended_at: Option<String>,
}

/// What a scheduled `command:` rule did the last time it ran.
#[derive(serde::Serialize)]
pub(super) struct ScheduleCommandView {
    /// `passed`, `failed`, `errored` or `refused`.
    pub(super) outcome: String,
    /// `null` when the command never produced one (refused, timed out, killed).
    pub(super) exit_code: Option<i64>,
    /// The redacted tail of its output, or the reason it was refused.
    pub(super) output: Option<String>,
    pub(super) ended_at: String,
}

/// One scheduled rule, with what the daemon knows about it having run.
#[derive(serde::Serialize)]
pub(super) struct ScheduleView {
    pub(super) name: String,
    pub(super) cron: String,
    /// The one-shot instant, exactly as written in the rules file; empty for a recurring rule.
    pub(super) at: Option<String>,
    /// The program a command rule runs instead of a prompt.
    pub(super) command: Option<String>,
    /// The last result of a command rule; `null` until it has run.
    pub(super) last_command: Option<ScheduleCommandView>,
    pub(super) prompt: String,
    pub(super) cwd: Option<String>,
    pub(super) timezone: Option<String>,
    /// When this fires next, counted from the last time it did — the same anchor the tick uses.
    pub(super) next_fire_at: Option<String>,
    /// Why it will never fire, when that is the answer instead.
    ///
    /// An unparseable cron or an unknown timezone makes the tick skip the rule and log at debug,
    /// 2,880 times a day. The rule simply never runs and nothing says so; this is where that stops
    /// being invisible.
    pub(super) problem: Option<String>,
    pub(super) last_fired_at: Option<String>,
    /// How many times it has fired today, against the daemon's own per-rule daily cap.
    pub(super) fires_today: i64,
    pub(super) daily_cap: u32,
}

/// One repo trigger, with the commit it last saw.
#[derive(serde::Serialize)]
pub(super) struct RepoTriggerView {
    pub(super) name: String,
    pub(super) branch: String,
    pub(super) prompt: String,
    /// The SHA recorded the last time this trigger was evaluated. `null` means it is armed and has
    /// not yet seen a first commit to compare against — which fires nothing, by design.
    pub(super) last_sha: Option<String>,
}

/// A project's post-merge gate state as the rules read serves it: read-only, and whole.
#[derive(serde::Serialize)]
pub(super) struct PostgateView {
    pub(super) target: String,
    pub(super) last_green: Option<String>,
    /// The sha a post-merge gate is measuring right now.
    pub(super) running: Option<String>,
    pub(super) red_groups: Vec<String>,
    pub(super) red_since: Option<String>,
    pub(super) red_sha: Option<String>,
    pub(super) red_base: Option<String>,
    /// `flake_check` or `bisect` while a red is being handled, else `null`.
    pub(super) phase: Option<&'static str>,
    pub(super) culprit: Option<String>,
    pub(super) candidates: Vec<String>,
    pub(super) also_suspect: Vec<String>,
}

impl From<crate::verify::postgate::State> for PostgateView {
    fn from(state: crate::verify::postgate::State) -> Self {
        PostgateView {
            target: state.target,
            last_green: state.last_green_sha,
            running: state.running_sha,
            red_groups: state.red_groups,
            red_since: state.red_since_sha,
            red_sha: state.red_sha,
            red_base: state.red_base_sha,
            phase: state.red_phase.map(|phase| phase.as_str()),
            culprit: state.culprit_sha,
            candidates: state.candidates,
            also_suspect: state.also_suspect,
        }
    }
}

/// Everything a project will do without being asked, and everything currently holding it back.
#[derive(serde::Serialize)]
pub(super) struct ProjectRules {
    pub(super) project_id: String,
    pub(super) project_root: Option<String>,
    /// `present`, `absent`, or `unreadable` — the three states the project's `autopilot.yaml` can
    /// be in.
    pub(super) rules_file: &'static str,
    /// Where that file is, as a person is shown it: `~/.nucleos/projects/<id>/autopilot.yaml`.
    ///
    /// Served rather than spelled by the page, because the page used to carry `.ai/autopilot.yaml`
    /// as a literal and was wrong the day the file moved. The daemon is the one that knows where it
    /// reads from.
    pub(super) rules_path: String,
    /// Why the file could not be read, when it could not be.
    ///
    /// `config.rs` uses `deny_unknown_fields` precisely so a typo is an error rather than a silently
    /// empty ruleset — but that error only reached a log line, so writing `schedule:` for
    /// `schedules:` stopped all autonomy for the project and looked like nothing had happened.
    pub(super) rules_error: Option<String>,
    pub(super) gate_command: Option<String>,
    /// Whether the VCS queue measures a merge into this project's target branch before publishing
    /// it. Served beside `gate_command` because it decides whether that command runs at a second
    /// moment entirely — and a brake nobody can see through this route is one nobody thinks to
    /// check when a landing takes twenty minutes.
    pub(super) gate_before_publish: bool,
    /// Whether this project's IDE verify switch is on (`POST /projects/{id}/ide-verify`). Served
    /// read-only here so the owner's toggle can show it; nothing on this route flips it.
    pub(super) ide_verify: bool,
    /// The target branch's post-merge gate state, or `null` while the gate has never recorded one.
    /// Read-only: nothing on this route changes it.
    pub(super) postgate: Option<PostgateView>,
    /// Why the post-merge state could not be read; `postgate` is then null. Absent when it was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) postgate_error: Option<String>,
    /// Who answers an approval a conversation on `auto` would otherwise put to a person.
    pub(super) judge: JudgeView,
    pub(super) schedules: Vec<ScheduleView>,
    pub(super) repo_triggers: Vec<RepoTriggerView>,
    /// The effective open-proposal ceiling: the project's own, else the global default. `null` means
    /// the brake is off.
    pub(super) wip_limit: Option<i64>,
    pub(super) open_review_items: i64,
    /// The two queues that total is made of, so the panel can name the screen that clears it.
    /// A total alone reads as "the proposals list", which on this project was empty while the
    /// brake held: all of it was unreviewed shadow decisions (`wip::OpenReviewItems`).
    pub(super) open_proposals: i64,
    pub(super) open_shadow_decisions: i64,
    /// Whether that ceiling is currently refusing new autonomous work.
    pub(super) queue_full: bool,
}

/// What a project does on its own.
///
/// The rules live in `~/.nucleos/projects/<id>/autopilot.yaml` and were readable only by opening
/// the file; the WIP ceiling lives in the database and was readable only through the roster's
/// summary. Both decide whether autonomous work happens at all, so they answer one question and are
/// served together.
pub(super) async fn get_project_rules(
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

    // Read whether or not the project has a root. The file is keyed by the project's id, so an
    // `off` project — which has no root on record — can still have rules waiting for the day it is
    // switched back on, and saying `absent` about a file that is there would be the wrong fact.
    let machine_root = state.machine_config_root.clone();
    let rules_id = id.clone();
    let (rules_file, rules_error, loaded) = tokio::task::spawn_blocking(move || {
        match crate::config::load_schedule_rules(machine_root.as_deref(), &rules_id) {
            Ok(rules) => {
                let present = crate::project::state::file(
                    machine_root.as_deref(),
                    &rules_id,
                    crate::project::state::AUTOPILOT_FILE,
                )
                .is_some_and(|path| path.exists());
                (if present { "present" } else { "absent" }, None, rules)
            }
            Err(error) => (
                "unreadable",
                Some(error.to_string()),
                crate::config::AutopilotRules::default(),
            ),
        }
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let rules_path =
        crate::project::state::display_path(&id, crate::project::state::AUTOPILOT_FILE);

    let state_rows: Vec<RuleRunState> = sqlx::query_as(
        "SELECT rule_name, last_fired_at, last_head_sha, fires_date, fires_today,
                command_outcome, command_exit_code, command_output, command_ended_at
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
            // A one-shot whose instant parses and is after its recorded last fire has already run:
            // nothing is left to fire and nothing is wrong, so it shows neither a next fire nor the
            // "at most once" problem `next_fire` reports for it. A last fire EQUAL to the instant
            // is the arming of an `at` that was already past when the daemon first saw it: it never
            // fires, and says so.
            let one_shot_order = last_fired_at
                .as_deref()
                .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
                .and_then(|fired| match crate::execution::scheduler::at_instant(rule) {
                    Some(Ok(at)) => Some((fired.with_timezone(&chrono::Utc), at)),
                    _ => None,
                })
                .map(|(fired, at)| (fired.cmp(&at), at));
            let (next_fire_at, problem) = match one_shot_order {
                Some((std::cmp::Ordering::Greater, _)) => (None, None),
                Some((std::cmp::Ordering::Equal, _)) => (
                    None,
                    Some(format!(
                        "its at ({}) was already in the past when the daemon first saw it; it never fires",
                        rule.at.as_deref().unwrap_or_default()
                    )),
                ),
                _ => match crate::execution::scheduler::next_fire(rule, since) {
                    Ok(next) => (Some(next.to_rfc3339()), None),
                    Err(problem) => (None, Some(problem)),
                },
            };
            // A count carrying another day's date is a count of nothing — the daemon resets by
            // comparing rather than by sweeping at midnight, so this reads it the same way.
            let fires_today = recorded
                .filter(|row| row.fires_date.as_deref() == Some(today.as_str()))
                .map_or(0, |row| row.fires_today);
            let last_command = recorded.and_then(|row| {
                Some(ScheduleCommandView {
                    outcome: row.command_outcome.clone()?,
                    exit_code: row.command_exit_code,
                    output: row.command_output.clone(),
                    ended_at: row.command_ended_at.clone()?,
                })
            });
            ScheduleView {
                name: rule.name.clone(),
                cron: rule.cron.clone(),
                at: rule.at.clone(),
                command: rule.command.clone(),
                last_command,
                prompt: rule.prompt.clone(),
                cwd: rule.cwd.clone(),
                timezone: rule.timezone.clone(),
                next_fire_at,
                problem,
                last_fired_at,
                fires_today,
                daily_cap: crate::execution::scheduler::DAILY_CAP,
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
    let items = crate::wip::open_review_items(&state.pool, &id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let open_review_items = items.total();

    // Read before the struct takes `id`, which is what the borrow checker was pointing at and is
    // also the clearer order: every other field here is gathered above.
    let judge: JudgeView = crate::project::policy::judge(&state.pool, &id).await.into();
    let ide_verify = crate::governance::autopilot::ide_verify_enabled(&state.pool, &id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (postgate, postgate_error) = match crate::verify::postgate::load(&state.pool, &id).await {
        Ok(row) => (row.map(PostgateView::from), None),
        Err(error) => {
            tracing::warn!(
                %error, project_id = %id,
                "rules: cannot read the post-merge state; serving the rules without it"
            );
            (
                None,
                Some("the post-merge state could not be read; see the daemon log".to_owned()),
            )
        }
    };

    Ok(Json(ProjectRules {
        project_id: id,
        project_root,
        rules_file,
        rules_path,
        rules_error,
        gate_command: loaded.gate_command.clone(),
        gate_before_publish: loaded.gate_before_publish,
        ide_verify,
        postgate,
        postgate_error,
        judge,
        schedules,
        repo_triggers,
        wip_limit,
        open_review_items,
        open_proposals: items.proposals,
        open_shadow_decisions: items.shadow_decisions,
        queue_full: crate::wip::queue_full(open_review_items, wip_limit),
    }))
}

#[derive(Deserialize)]
pub(super) struct WipLimitRequest {
    /// `null` switches the brake off for this project.
    pub(super) limit: Option<i64>,
}

/// Sets one project's open-proposal ceiling.
///
/// The brake it controls is self-clearing — it releases the moment you review something — so the
/// number is the answer to "how much unreviewed work am I willing to be holding", and until now it
/// could only be changed with sqlite3. A negative ceiling is refused rather than stored: `queue_is_full`
/// compares `open >= limit`, so a negative one would mean "never start anything again" while reading
/// like a number somebody chose.
pub(super) async fn post_project_wip_limit(
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

/* ------------------------------------------------------- project commands -- */

/// Everything this project can be asked to do to itself, overlay applied.
pub(super) async fn get_project_commands(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::project::commands::ProjectCommand>>, StatusCode> {
    crate::project::commands::list(&state.pool, &id)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "listing project commands failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[derive(Deserialize)]
pub(super) struct DeclareRequest {
    pub(super) name: String,
    pub(super) command: String,
    #[serde(default)]
    pub(super) cwd: Option<String>,
    #[serde(default)]
    pub(super) is_gate: bool,
    /// Absent means 0, which is what almost every command means by passing.
    #[serde(default)]
    pub(super) pass_exit_code: Option<i64>,
    /// Absent means `person`. Default-deny: a command declared by somebody who never thought about
    /// this question must not thereby become something an autonomous run may execute.
    #[serde(default)]
    pub(super) runnable_by: Option<crate::project::commands::RunnableBy>,
}

/// Declares one of this project's own commands, replacing any of the same name.
///
/// **No kill-switch check here, and the asymmetry with `POST /write` is deliberate.** That one puts
/// bytes in the project's own folder, which §7.5 says must go the way an agent's write goes. This
/// writes a database row, like `POST /wip-limit` beside it, and neither of those starts anything.
/// The stop belongs on the route that spawns a process, which is the next one down.
pub(super) async fn post_project_command(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<DeclareRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let root = resolve_project_root(&state, &id)
        .await
        .map_err(|status| refusal(status, "no_project_root"))?;

    if let Err(invalid) =
        crate::project::commands::validate(&body.name, &body.command, body.cwd.as_deref())
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "refusal": "invalid", "detail": invalid.to_string() })),
        ));
    }

    // The half `validate` cannot do: whether the directory is actually there, and is a directory.
    // Checked when it is declared so a typo is caught while the person who made it is looking at
    // it — and again when it runs, because a folder that existed on Tuesday can be gone on
    // Wednesday.
    if let Some(cwd) = body.cwd.as_deref() {
        working_directory(&root, cwd)?;
    }

    let id_for_log = id.clone();
    let new_id = crate::project::commands::declare(
        &state.pool,
        &id,
        crate::project::commands::Declaration {
            name: body.name,
            command: body.command,
            cwd: body.cwd,
            is_gate: body.is_gate,
            pass_exit_code: body.pass_exit_code.unwrap_or(0),
            runnable_by: body
                .runnable_by
                .unwrap_or(crate::project::commands::RunnableBy::Person),
        },
    )
    .await
    .map_err(|error| {
        tracing::warn!(%error, project_id = %id_for_log, "declaring a project command failed");
        refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;

    Ok(Json(serde_json::json!({ "id": new_id })))
}

/// Forgets one of this project's commands.
pub(super) async fn delete_project_command(
    State(state): State<AppState>,
    Path((id, command_id)): Path<(String, i64)>,
) -> Result<StatusCode, StatusCode> {
    match crate::project::commands::remove(&state.pool, &id, command_id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(%error, project_id = %id, "forgetting a project command failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Where a command runs, resolved and refused as one answer.
///
/// `None` is the project root. A relative path is refused unless it resolves inside the project and
/// is a directory — the same guard the readers use, and it has to be re-asked at run time because
/// the declaration was checked at some earlier moment.
pub(super) fn working_directory(
    root: &std::path::Path,
    cwd: &str,
) -> Result<PathBuf, (StatusCode, Json<serde_json::Value>)> {
    let resolved =
        inspect::resolved_within(root, cwd).map_err(|error| match inspect_status(error) {
            StatusCode::NOT_FOUND => refusal(StatusCode::UNPROCESSABLE_ENTITY, "cwd_missing"),
            status => refusal(status, "cwd_unsafe"),
        })?;
    if !resolved.is_dir() {
        return Err(refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            "cwd_not_a_folder",
        ));
    }
    Ok(resolved)
}

/// Runs one of this project's commands and answers before it finishes.
///
/// **202 and not the output.** A suite takes minutes; an HTTP request that held the connection for
/// them would be a request that any proxy, any sleep and any closed lid would kill halfway. The
/// result lands on the command's own row and the page reads it back — which is also what makes the
/// answer survive a reload, and what makes "is the gate green" a fact the page already has rather
/// than something it has to ask for.
///
/// The refusals, in the order they are asked:
///
/// 1. **423, the emergency stop.** First, because it is the one refusal that is about the machine
///    rather than about this request. A stop that did not stop a button spawning `cargo test` would
///    be a stop in name only.
/// 2. **404**, no folder, or no such command in THIS project — the `project_id` in the lookup being
///    what keeps an integer in a URL from naming another project's row.
/// 3. **403**, a command marked `person` reached by a key that is nobody's but an agent's.
/// 4. **422**, a working directory that has gone, or that is not a directory.
/// 5. **409**, already running. The claim is a conditional UPDATE rather than a read followed by a
///    write, so two clicks landing together cannot both start the suite in one folder.
pub(super) async fn post_project_command_run(
    State(state): State<AppState>,
    Extension(scope): Extension<crate::auth::Scope>,
    Path((id, command_id)): Path<(String, i64)>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let halted = crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
        || crate::governance::autopilot::scoped_kill_engaged(&state.pool, "project", &id)
            .await
            .unwrap_or(true);
    if halted {
        return Err(refusal(StatusCode::LOCKED, "kill_switch"));
    }

    let root = resolve_project_root(&state, &id)
        .await
        .map_err(|status| refusal(status, "no_project_root"))?;

    let command = crate::project::commands::get(&state.pool, &id, command_id)
        .await
        .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
        .ok_or_else(|| refusal(StatusCode::NOT_FOUND, "no_such_command"))?;

    if crate::project::commands::caller_is_an_agent(&scope)
        && command.runnable_by != crate::project::commands::RunnableBy::Agent
    {
        return Err(refusal(StatusCode::FORBIDDEN, "person_only"));
    }

    let cwd = match command.cwd.as_deref() {
        Some(cwd) => working_directory(&root, cwd)?,
        None => root.clone(),
    };

    let pool = state.pool.clone();
    let project_id = id.clone();
    let name = command.name.clone();
    let text = command.command.clone();
    let pass = command.pass_exit_code;

    // The claim and the spawn together, uncancellable, for the reason `create_run` gives: a client
    // that disconnects mid-request drops this future, and a drop landing between the two would
    // leave a row saying `running` with nothing running.
    let claimed = uncancellable(async move {
        // A failed claim is a storage error, not a command already running: reporting it as
        // `already_running` would send the owner looking for a second click that never happened.
        match crate::project::commands::mark_running(&pool, &project_id, command_id).await {
            Ok(true) => {}
            Ok(false) => return Ok(false),
            Err(error) => return Err(error),
        }
        tokio::spawn(async move {
            // **The reference is the same directory the command runs in, on purpose.**
            // `run_gate`'s tamper check exists because a gate measures a run's own worktree and
            // that run could have rewritten the script it is measured by. There is no second copy
            // here — this runs in the project's own checkout, which IS the reference — so passing
            // the same path makes the check a no-op by construction rather than by luck. Passing
            // the project root instead would compare a subdirectory's scripts against paths that
            // do not exist there and report a tamper that never happened.
            //
            // In a task of its own, awaited here, so a panic inside the measurement still reaches
            // `finish` below. Without that the row would say `running` until the next restart,
            // and the conditional claim above would refuse every later click as `already_running`.
            let measured = tokio::spawn(async move {
                crate::gate::run_gate(&cwd, &cwd, &text, crate::project::commands::COMMAND_TIMEOUT)
                    .await
            })
            .await;
            let finished = match measured {
                Ok(outcome) => crate::project::commands::verdict(pass, outcome),
                Err(error) => {
                    tracing::error!(%error, command_id, "a project command's task ended without a result");
                    crate::project::commands::Finished {
                        outcome: crate::project::commands::Outcome::Errored,
                        exit_code: None,
                        output: Some(format!("the command's task ended without a result: {error}")),
                    }
                }
            };
            let said = match finished.outcome {
                crate::project::commands::Outcome::Passed => format!("{name} passed"),
                crate::project::commands::Outcome::Failed => match finished.exit_code {
                    Some(code) => format!("{name} failed with exit {code}"),
                    None => format!("{name} failed"),
                },
                // Never "failed". A command that could not be measured says nothing about the
                // project, and reporting it as a failure would stop the wrong work.
                _ => format!("{name} could not be measured"),
            };
            if let Err(error) =
                crate::project::commands::finish(&pool, command_id, finished).await
            {
                tracing::error!(%error, command_id, "a project command finished and was not recorded");
            }
            let _ = crate::feed::append(&pool, Some(&project_id), "command_finished", &said, None, None)
                .await;
        });
        Ok(true)
    })
    .await
    .map_err(|status| refusal(status, "internal"))?
    .map_err(|error: sqlx::Error| {
        tracing::warn!(%error, command_id, "claiming a project command failed");
        refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;

    if !claimed {
        return Err(refusal(StatusCode::CONFLICT, "already_running"));
    }
    Ok(StatusCode::ACCEPTED)
}

/// One declared shell rule, whole.
///
/// A `View` and not a fourth `ProjectRules`. There are already two: the struct above, which answers
/// `GET /projects/{id}/rules` about the project's `autopilot.yaml`, and `hooks::ProjectRules`, which
/// is an
/// enum about whether the table could be read at all. A third spelling of that name would tell a
/// reader nothing about which of the three they are holding.
///
/// **A ROW per rule, and not the two lists of prefixes this route used to serve.** The two shapes
/// carry the same facts and only one of them survives a rule travelling alone, which is the whole
/// argument:
///
/// - The note was the reason to change anything, and it cannot ride on a `Vec<String>`. Migration
///   `0128` calls that column "a única defesa contra uma lista que daqui a seis meses ninguém sabe
///   justificar", and a defence that can only be written is no defence.
/// - A rule's verdict belongs ON the rule. In `{allow: [...], deny: [...]}` the verdict lives in the
///   CONTAINER, so the moment a page hands one rule to an edit form the verdict falls off — and
///   `POST /projects/{id}/shell-rules` rewrites BOTH `verdict` and `note` from what it is sent. That
///   is the same loss this change exists to close, one field over. Served as rows, what comes back
///   from the GET is field-for-field what the POST takes, and re-declaring is changing one of them.
/// - The split stays legible because every row says which side it is on, which is strictly more
///   than the old shape said: it named the sides once, at the top, and a page still had to
///   re-associate every prefix with the list it came out of before it could draw anything. Grouping
///   by `verdict` is what the page does instead, and `deny` beating `allow` is a sentence it can now
///   write next to a rule rather than next to a heading.
///
/// The prefixes come back FOLDED, because that is how the table stores them and how they are
/// enforced. Serving the typed spelling would show an owner a `Remove-Item` the classifier knows as
/// `remove-item`, which is the very lie `project::policy::fold_prefix` exists to stop. WHICH fold a
/// prefix got follows `tool`: a command is lower-cased and a path is not, so two rows that look
/// alike on screen were folded by two different functions and only that field says which.
///
/// **`created_at` is served, and it is not padding.** The note says WHY a rule is there; this says
/// WHEN, and the column's stated purpose is a list nobody can justify *six months from now* — which
/// is a claim about time. A list where every rule looks equally fresh has to be re-read whole; one
/// that carries dates can be triaged by age, and the oldest rules are the ones whose author has
/// most likely left. It costs nothing: same row, `NOT NULL`, already written. Note for whoever
/// draws it — this is when the prefix was FIRST declared and NOT when it was last edited, because
/// `declare_shell_rule`'s `DO UPDATE` deliberately leaves the column alone; labelling it "last
/// changed" would be a caption the daemon never agreed to.
#[derive(serde::Serialize)]
pub(super) struct ShellRuleView {
    pub(super) prefix: String,
    /// Which tool this rule governs the writes of, or `null` for a rule about a COMMAND prefix.
    ///
    /// **Serialised on every row, `null` included, and that is not a formality.** The two kinds of
    /// rule live in one list and a prefix alone cannot tell them apart: `deny migrations` is a
    /// command nobody may run, `deny Edit migrations` is a directory nothing may write into, and
    /// served without this field the second reads on screen as the first. That was true for exactly
    /// one chunk — `0131` gave the column to the table and to the classifier, and no route could
    /// write it — and it stops being true here, where the POST below learns to carry a tool.
    ///
    /// `null` and never `""`, for `DeclaredShellRule::tool`'s reason: the empty string is SQLite's
    /// business, forced by a UNIQUE index that treats every NULL as distinct, and a page made to
    /// compare against a magic empty string would get it wrong once.
    pub(super) tool: Option<String>,
    pub(super) verdict: crate::project::policy::Verdict,
    /// `null` for a rule declared with no justification — an absent note and not an absent field,
    /// so a page can tell "nobody said why" from a shape it failed to parse.
    pub(super) note: Option<String>,
    pub(super) created_at: String,
}

/// What this project's worktrees may run without asking, and what they may never run.
///
/// Ordered by tool and then by prefix, which is `declared_shell_rules`' `ORDER BY` and not this
/// handler's arithmetic — the verdict and the tool are on every row, so any grouping a page wants
/// is a filter it can do itself. Nothing is filtered out here either: a write rule and a command
/// rule come back in one list, because the route serves ROWS and the screen decides how to show
/// them. That is the argument `ShellRuleView` already makes about the allow/deny split, and the
/// tool is that same argument one field over.
pub(super) async fn get_project_shell_rules(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ShellRuleView>>, StatusCode> {
    crate::project::policy::declared_shell_rules(&state.pool, &id)
        .await
        .map(|rules| {
            Json(
                rules
                    .into_iter()
                    .map(|rule| ShellRuleView {
                        prefix: rule.prefix,
                        tool: rule.tool,
                        verdict: rule.verdict,
                        note: rule.note,
                        created_at: rule.created_at,
                    })
                    .collect(),
            )
        })
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "reading a project's shell rules failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// `deny_unknown_fields` on this and its three siblings, for `config.rs`'s reason rather than for
/// tidiness: a misspelled field would otherwise be dropped in silence. Here that is not a cosmetic
/// loss — `note` is the field most likely to be typed wrong, and `declare_shell_rule` overwrites
/// the stored note with whatever arrives, so `"notes"` for `"note"` currently answers 204 and
/// erases the justification the rule already carried.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ShellRuleDeclaration {
    pub(super) prefix: String,
    /// The tool whose writes this rule is about, or absent for a rule about a command prefix.
    ///
    /// **`#[serde(default)]` because every client that existed before this field sent none**, and a
    /// body without it is a shell rule — which is what those clients meant and what the route did.
    /// It has to be DECLARED all the same, and not merely tolerated: this struct carries
    /// `deny_unknown_fields`, so until the field exists here a client sending `"tool": "Edit"` is
    /// answered 422 by serde with a sentence about an unknown field, which reads as a malformed
    /// request rather than as a route that has not learned the word yet.
    ///
    /// Which values are admissible is decided in the handler and not by a type, because the answer
    /// is the migration's `CHECK (tool IN ('', 'Edit', 'Write'))` and a refusal that names the
    /// offending value is worth more here than an enum's parse error — see `unknown_tool`.
    #[serde(default)]
    pub(super) tool: Option<String>,
    pub(super) verdict: crate::project::policy::Verdict,
    /// Why, for whoever reads the list next. Never consulted by the classifier.
    #[serde(default)]
    pub(super) note: Option<String>,
}

/// Declares one shell rule, or changes the verdict of one already declared.
///
/// **Idempotent, and not by this handler's arithmetic.** `declare_shell_rule`'s
/// `ON CONFLICT (project_id, tool, prefix) DO UPDATE SET verdict = excluded.verdict, note =
/// excluded.note` is what makes a second POST of the same rule an EDIT. The identity of a rule is
/// the TOOL and the prefix together and the unique index enforces that triple, so a redeclaration
/// can never become a second row — while `deny migrations` and `deny Edit migrations` are two rules
/// that may both stand, because they are two claims about two different things. The
/// two ways of losing the `DO UPDATE` cost something else instead: no `ON CONFLICT` clause at all is
/// `UNIQUE constraint failed` and a 500, and `DO NOTHING` — the spelling its two siblings in
/// `project_policy` use — is worse for being silent, leaving the first verdict standing while the
/// caller is told 204. `a_declared_shell_rule_comes_back_folded_and_then_goes` pins the edit through
/// this route; `project::policy::tests::declaring_the_same_prefix_again_changes_its_verdict` pins it
/// one layer down.
///
/// **`note = excluded.note` is the half a caller has to know about.** The clause is a plain
/// overwrite and not a `COALESCE`, deliberately: `declare_shell_rule` says "this rule is now in this
/// state", and a note that fell back to the stored one could never be REMOVED. The consequence this
/// route is the first thing to expose is that **a second POST carrying no `note` writes `NULL` over
/// an existing one** — so an editor that sends only `prefix` and `verdict` silently discards the
/// justification. Migration `0128` calls that column "a única defesa contra uma lista que daqui a
/// seis meses ninguém sabe justificar"; whoever builds the page in a later chunk must send the
/// existing note back with the verdict, or changing a verdict throws that defence away.
///
/// **The shape guard is asked of an `allow` and never of a `deny`, and the asymmetry is the whole
/// correctness of this route.** `classify_segment` reads `shell_form_is_readable(&normalized) &&
/// rules.allows(&normalized)`, so an `allow` of a shape that guard refuses could never fire, and
/// storing one would leave the owner holding a permission that does nothing. Nothing of the kind is
/// true of a `deny`: `classify_shell_command` consults `rules.denies` at the LINE level, above the
/// segment loop and with no shape guard over it, so a project can and does refuse `tail -f`,
/// `sort -o`, `find . -exec` and `curl … | sh` — precisely the shapes that otherwise reach
/// `pending_approval`, and precisely the ones a project would most want to close. Guarding the deny
/// side would refuse to store the refusals most worth writing down, and tell the owner they would
/// never be enforced while the engine went on enforcing them. Migration `0128` says the same in one
/// line: "Do lado `deny` não há nada a validar — uma recusa a mais nunca deixou correr nada."
///
/// **A write rule can only DENY, and this route is the third place that says so and the only one a
/// person meets.** The other two are silent by construction: `classifier::classify` has no allow
/// branch anywhere on a write path — "there is no `allow` counterpart here, and there must not be
/// one" — and `project::policy::shell_rules` drops a tool-carrying `allow` on its way out of the
/// table, with a `tracing::warn!` nobody standing at a screen will ever read. An owner who wrote
/// one would be holding a permission that decides nothing, believing it decides something. So an
/// `allow` that names a tool is refused at the door with `unenforceable_allow`, and the sentence
/// says what a `deny` of the same path WOULD do — the same move `unmatchable_prefix` makes one
/// paragraph up, for the same reason: a permission that is silently not one is worse than a refusal
/// somebody can act on.
///
/// The refusals name the offending value, because a refusal that does not say what was wrong sends
/// the owner to read source code to find out which of seven shape guards they tripped.
pub(super) async fn post_project_shell_rule(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ShellRuleDeclaration>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    // The stop first, following `post_project_command_run`: it is the one refusal that is about the
    // machine rather than about this request. Asked only for an `allow`, which is the only verdict
    // here that widens — see `declaration_halted`.
    if body.verdict == crate::project::policy::Verdict::Allow {
        declaration_halted(&state, &id).await?;
    }
    project_is_on_the_roster(&state, &id).await?;

    // **Emptiness is two questions since the `tool` column existed, and this is the first.**
    //
    // What follows asks whether anything ARRIVED, and until there were write rules that was the
    // whole guard — correctly, because there was one fold and it cannot do this. A command prefix
    // goes through `project::policy::fold_prefix`, which is `classifier::normalize_command`: it
    // collapses whitespace and lower-cases, so it can shorten a string and never empty one that
    // survived `trim()`. A PATH prefix goes through `fold_path_prefix` instead, which rewrites `\`
    // as `/` and only THEN strips the trailing `/` — so `"/"`, `"\\"` and `"///"` arrive non-empty,
    // pass this check, and come out of that fold as the empty string. The second question — does
    // it survive the fold — therefore lives in the `Some(tool)` arm below, beside the fold that is
    // able to answer it no.
    let prefix = body.prefix.trim();
    within_length("prefix", prefix)?;
    if prefix.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "refusal": "empty_prefix",
                "detail": "a shell rule has to name a prefix",
            })),
        ));
    }

    // Which tool's writes this rule is about, or `None` for a rule about a command prefix — which
    // is every rule anybody could declare before this handler learned the word.
    //
    // Refused BY NAME here rather than left to the table, exactly as `empty_prefix` above stands in
    // front of the column's `prefix <> ''`: `tool` is stored under a closed `CHECK`, and a value
    // the CHECK would reject must never reach the CHECK, because a constraint failure comes back
    // through the `internal` arm below as a 500 that names nothing.
    //
    // **Asked of `classifier::WRITE_TOOLS` rather than spelled again here.** This guard carried
    // its own `["Edit", "Write"]` until 2026-09-08, which is two lists for one fact, and the day
    // the classifier admitted `NotebookEdit` this door would have gone on refusing a rule the
    // write chain had just learned to enforce — a refusal nobody could have debugged from the
    // message, since the message was quoting the wrong list with complete confidence.
    //
    // The column's CHECK is still a second copy and cannot be anything else: SQL cannot read a
    // Rust const. `0138` is the migration that widened it, and the two are kept in step by the
    // test below that sends every name on this list at the real table.
    let tool = body.tool.as_deref();
    if let Some(tool) = tool
        && !crate::governance::classifier::WRITE_TOOLS.contains(&tool)
    {
        let governed = crate::governance::classifier::WRITE_TOOLS
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "refusal": "unknown_tool",
                "detail": format!(
                    "`{tool}` is not a tool whose writes this núcleo governs: only {governed} \
                     reach the write chain in `classifier::classify`, so a rule about anything \
                     else would be stored and never consulted. Send no tool at all for a rule \
                     about a COMMAND prefix."
                ),
            })),
        ));
    }

    // The shape a prefix has to be in, and WHICH question that is depends on what kind of prefix
    // this is. Written as a match on the tool rather than as two conditions in a row, because the
    // two guards are mutually exclusive as a matter of MEANING and not of ordering: a path is not a
    // shell line, and running one through `shell_form_is_readable` would refuse ordinary directory
    // names for looking like flags and redirections.
    match tool {
        // See the doc above for why this is inside the `Allow` arm and must stay there.
        None => {
            if body.verdict == crate::project::policy::Verdict::Allow
                && !crate::governance::classifier::shell_form_is_readable(prefix)
            {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({
                        "refusal": "unmatchable_prefix",
                        "detail": format!(
                            "`{prefix}` can never be allowed: `classify_segment` guards \
                             `rules.allows` with the same shape check this just failed, so the \
                             permission would be stored and never fire. Declared as a `deny` it \
                             would be enforced."
                        ),
                    })),
                ));
            }
        }
        Some(tool) => {
            // The rule this whole chunk is built around, refused where somebody can read it. The
            // doc above names the two silent places that already enforce it.
            if body.verdict == crate::project::policy::Verdict::Allow {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({
                        "refusal": "unenforceable_allow",
                        "detail": format!(
                            "a rule about a tool can only REFUSE. `{tool}` allowed to write to \
                             `{prefix}` would be stored and decide nothing: the write chain in \
                             `classifier::classify` ends at `read-local`, so there is no allow \
                             side for it to reach, and `project::policy::shell_rules` drops such a \
                             row on the way back out of the table. Declared as a `deny` the same \
                             path WOULD be enforced — `{tool}` would be refused at it and at \
                             everything under it."
                        ),
                    })),
                ));
            }
            // The second half of `empty_prefix`, and it is here because only this fold can reach
            // it. `fold_path_prefix` writes `\` as `/` and then strips the trailing `/`, so a
            // prefix of nothing but separators — `/`, `//`, `\`, `\\`, `///` — survives the trim
            // check above and lands on the column's `prefix CHECK (prefix <> '')` as the empty
            // string. A CHECK failure comes back through the `internal` arm below as a 500 that
            // names nothing, which is precisely what the guard above promises cannot happen: a
            // value the CHECK would reject must never reach the CHECK.
            //
            // Asked by CALLING the fold, not by restating what it does. A condition written to
            // match a fold is a second source of truth that stops matching in silence the day the
            // fold learns to strip something else, and what it lets back through is this same
            // nameless 500. `fold_for` is `pub(crate)` for exactly that reason: one function
            // answers what gets stored, and every guard about what gets stored asks that function.
            if crate::project::policy::fold_for(Some(tool), prefix).is_empty() {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({
                        "refusal": "empty_prefix",
                        "detail": format!(
                            "`{prefix}` names no path: a path prefix is stored with `\\` written \
                             as `/` and the trailing `/` dropped, so a prefix of nothing but \
                             separators folds to the empty string and there is no rule left to \
                             enforce. Write the path out, relative to the project root — there is \
                             no prefix that means the whole worktree."
                        ),
                    })),
                ));
            }
            // The three characters `classifier::lands_inside_the_workspace` refuses in a `cd` or
            // `mkdir` target, refused here for the same reason and against the same code. Nothing
            // expands them on the way to a comparison: `write_denied_by_project` puts the stored
            // prefix through `normalize_path`, which GLUES an ordinary-looking `~`, `$HOME` or
            // `%USERPROFILE%` onto the workspace — so `~/notes` would be stored as a refusal of
            // `<workspace>/~/notes`, a directory no write can ever land in. A refusal that cannot
            // match is an allow, which is why this is a 422 and not a warning.
            if prefix.starts_with('~') || prefix.contains('$') || prefix.contains('%') {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({
                        "refusal": "unmatchable_prefix",
                        "detail": format!(
                            "`{prefix}` can never match a write: a leading `~`, a `$` or a `%` \
                             arrives UNEXPANDED, and `normalize_path` joins it onto the workspace \
                             as an ordinary name — so the refusal would be about a directory that \
                             cannot exist, and a refusal that cannot match is an allow. Write the \
                             path out, relative to the project root."
                        ),
                    })),
                ));
            }
        }
    }

    // A note of nothing but whitespace is NO note, and it is folded to `NULL` here rather than
    // stored. The column exists to be "the only defence against a list nobody can justify in six
    // months", and an empty string is not a justification — it is the same absence spelled a second
    // way, which would leave two rows that mean one thing and a reader unable to tell "nobody wrote
    // one" from "somebody wrote nothing". The page renders `null` as "no justification" and an empty
    // string as a blank space, so the two are already visibly different for no reason.
    let note = body
        .note
        .as_deref()
        .map(str::trim)
        .filter(|note| !note.is_empty());

    // The tool goes down as it arrived — `None` is a shell rule and `Some` is a write rule, and
    // `declare_shell_rule` folds the prefix by whichever of the two folds that answer picks.
    crate::project::policy::declare_shell_rule(&state.pool, &id, tool, prefix, body.verdict, note)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "declaring a project shell rule failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Which rule to withdraw, and it takes TWO fields for a reason worth stating.
///
/// **A prefix alone no longer names a row.** The unique index is `(project_id, tool, prefix)`, so a
/// project may hold `deny migrations` — a command — and `deny Edit migrations` — a directory — at
/// once, and they are two rules. A DELETE that carried only the prefix would have to pick one of
/// them, and whichever it picked would be right half the time; `forget_shell_rule`'s `WHERE` names
/// the triple, so the tool has to arrive for the statement to be able to name the row.
///
/// **Absent means the SHELL rule, and that is the answer that keeps the old clients honest.** Every
/// DELETE written before this field existed meant a command prefix, because a command prefix was
/// the only thing that could be declared. `#[serde(default)]` makes that reading the default one,
/// rather than "whichever row sorts first" — which is a delete that lands on a rule nobody named.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ShellRuleTarget {
    pub(super) prefix: String,
    /// The tool whose rule to withdraw, or absent for the rule about a command prefix.
    #[serde(default)]
    pub(super) tool: Option<String>,
}

/// Withdraws one shell rule.
///
/// **404 when nothing matched, and the answer comes from the DELETE itself.** `forget_shell_rule`
/// returns `Ok(false)` off `rows_affected`, which is `project::commands::remove`'s shape one section
/// up. Asking a reader first and then deleting would be two queries answering about two moments,
/// and — worse — the presence check would have had to fold the prefix itself, which is a second
/// spelling of the key `project::policy::fold_prefix` exists to be the only one of.
///
/// **No shape guard and no kill switch.** A rule stored before either existed still has to be
/// withdrawable, and an emergency stop must never stand between somebody and narrowing what their
/// project may do. That covers the tool as well: `unknown_tool` guards the POST because a value
/// outside the CHECK cannot be STORED, while here an unknown tool simply matches no row and earns
/// the 404 it deserves — refusing it by name would put a shape guard in front of a withdrawal, and
/// the one row it could keep somebody from removing is a row written before the guard existed.
pub(super) async fn delete_project_shell_rule(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ShellRuleTarget>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    project_is_on_the_roster(&state, &id).await?;

    let prefix = body.prefix.trim();
    within_length("prefix", prefix)?;
    if prefix.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "refusal": "empty_prefix",
                "detail": "a shell rule has to name a prefix",
            })),
        ));
    }

    let tool = body.tool.as_deref();
    let forgotten = crate::project::policy::forget_shell_rule(&state.pool, &id, tool, prefix)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "forgetting a project shell rule failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;
    if !forgotten {
        // **The sentence names the tool as well as the prefix**, because with two rules able to
        // share a prefix "there is no rule for `migrations`" is a claim that can be false on the
        // very screen it is read on: the other one is still listed, and "I deleted it and it is
        // still there" has no answer without the missing word.
        //
        // The COMMAND half names the folded spelling — the answer somebody who typed the wrong case
        // needs — through `normalize_command`, which is the function `project::policy::fold_prefix`
        // wraps and therefore not a second spelling of the fold. The write half deliberately echoes
        // what was sent instead: `fold_path_prefix` is private to `project_policy`, and restating
        // its three steps here to decorate a 404 would be exactly the second spelling of a key that
        // module exists to prevent. It keeps its case either way, which is the part of a path
        // somebody could get wrong and needs to see.
        let detail = match tool {
            None => format!(
                "{id} has declared no rule for `{}`",
                crate::governance::classifier::normalize_command(prefix)
            ),
            Some(tool) => format!("{id} has declared no `{tool}` rule for `{prefix}`"),
        };
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "refusal": "no_such_rule", "detail": detail })),
        ));
    }

    Ok(StatusCode::NO_CONTENT)
}

/// The judge as the window reads it.
///
/// Tagged rather than flattened into two nullable fields, because the three states are what this
/// type is FOR and `{"brain": null}` cannot tell "nobody has chosen" from "somebody chose nobody" —
/// which is the whole distinction `project::policy::Judge` exists to keep.
#[derive(serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum JudgeView {
    /// No row. The local brain, on the model it is already configured with.
    Default,
    /// Somebody switched it off. Every question goes to a person.
    Off,
    /// Somebody named one.
    Named {
        brain: String,
        model: Option<String>,
    },
}

impl From<crate::project::policy::Judge> for JudgeView {
    fn from(judge: crate::project::policy::Judge) -> Self {
        match judge {
            crate::project::policy::Judge::Default => Self::Default,
            crate::project::policy::Judge::Off => Self::Off,
            crate::project::policy::Judge::Named { brain, model } => Self::Named {
                brain: brain.as_str().to_owned(),
                model,
            },
        }
    }
}
