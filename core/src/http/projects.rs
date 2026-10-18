//! The roster: adding, reading and removing projects (`/projects*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/projects", get(get_projects))
        // Ahead of every `/projects/{id}/…` route, and a literal segment where those take a
        // parameter. A project called `detect` would be shadowed by it — which is why the wizard
        // sends the folder as a query rather than in the path, and why this answers about a folder
        // that has no project id yet at all.
        .route("/projects/detect", get(get_project_detect))
        // The way out, and the reading somebody takes before using it. One project could be added
        // and none could leave, so the roster only ever grew — a folder that moved, a repository
        // somebody finished with, a project added to try something once, all of them polled every
        // three seconds for ever.
        //
        // `DELETE /projects/{id}` takes the row off the roster and touches nothing on a disk. There
        // is deliberately no flag on it that would: deleting a folder is a second act with its own
        // route and its own refusals, and a `?delete_folder=` here would give two very different
        // weights one shape.
        .route("/projects/{id}", delete(delete_project))
        // The second act, and the only irreversible thing this app does. Its own route rather than
        // a flag on the one above, so that nothing can reach it by passing an argument it did not
        // read: `DELETE /projects/{id}` cannot be made to touch a disk, whatever it is sent.
        //
        // The GET beside it is what a person reads before deciding — what in that folder exists
        // nowhere else, and whether the daemon would refuse anyway.
        .route(
            "/projects/{id}/folder",
            get(get_project_folder).delete(delete_project_folder),
        )
        // Bringing a project under NucleOS: the GET is what a person reads before confirming (what is
        // in the folder, the proposed gate), the POST is the confirmation. In no table in `auth.rs`,
        // so Admin's by default-deny — it writes the gate command and an executable hook.
        .route(
            "/projects/{id}/onboard",
            get(get_project_onboarding).post(post_project_onboard),
        )
        .route("/projects/{id}/readings", get(get_project_readings))
        .route("/projects/{id}/tests-map", get(get_project_tests_map))
        // Control/admin only: absent from the `auth.rs` scope tables; the handler re-checks the owner.
        .route(
            "/projects/{id}/ide-verify",
            post(crate::verify::provision::post_project_ide_verify),
        )
        // Two doors and not one PUT with a nullable field, because the three states are not a value
        // and its absence: naming nobody is a decision, and taking the row away is withdrawing one.
        // A single writer would have had to spell the difference as null-versus-missing, which is
        // the distinction JSON is worst at carrying.
        .route(
            "/projects/{id}/judge",
            post(post_project_judge).delete(delete_project_judge),
        )
        // The write boundary, read and exercised. They sit together because the second is
        // unintelligible without the first: `POST /write` refuses everything the table does not
        // name, so a client that cannot read the table can only discover the fence by hitting it.
        .route("/projects/{id}/ownership", get(get_project_ownership))
        .route("/projects/{id}/owned", get(get_project_owned))
}

/// How far back a reading looks. Absent is the default window, not zero days.
#[derive(Deserialize)]
pub(super) struct WindowQuery {
    pub(super) days: Option<i64>,
}

/// One roster row, plus the one thing about it that is not in the database.
///
/// `#[serde(flatten)]` so the wire shape stays the summary with a field added, rather than a
/// nesting the shell would have to reach through — the same arrangement `GraphView` uses.
#[derive(serde::Serialize)]
pub(super) struct RosterEntry {
    /// Whether the recorded folder is actually on the disk.
    ///
    /// **Three states and not two, because the page that reads this used to collapse two of them.**
    /// `None` means no folder was ever named — the project exists in the daemon and nobody has
    /// pointed it anywhere, which is a thing to finish, not a thing that broke. `Some(false)` means
    /// a folder WAS named and is not there, which is a thing that broke. `Some(true)` is fine.
    ///
    /// Answered here rather than by the shell, which used to ask for a directory listing per
    /// project to find this out: on a roster of twenty-five that was twenty-five requests on every
    /// load, to learn one bit each. It is also what makes the page's headline and its rows agree —
    /// the headline counted *recorded roots* and the rows probed the *folder*, both worded
    /// "folder", so a roster could say "all with a folder" above seven rows saying it was gone.
    pub(super) root_exists: Option<bool>,
    #[serde(flatten)]
    pub(super) summary: ProjectSummary,
}

pub(super) async fn get_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<RosterEntry>>, StatusCode> {
    let summaries = autopilot::project_roster(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    /*
       On the blocking pool, and it is not superstition: this route is polled every three seconds,
       most of these roots are ordinary local folders answering in microseconds, and one of them on
       a disconnected network share is a `stat` that blocks for as long as the OS feels like. One
       such project would stall the executor thread this future is on, for every caller.
    */
    tokio::task::spawn_blocking(move || {
        summaries
            .into_iter()
            .map(|summary| RosterEntry {
                root_exists: summary
                    .project_root
                    .as_deref()
                    .map(|root| std::path::Path::new(root).is_dir()),
                summary,
            })
            .collect::<Vec<_>>()
    })
    .await
    .map(Json)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Whether the history goes with the roster row.
///
/// A query and not a body, because this is a DELETE and the default has to be the safe one: a
/// caller that sends nothing keeps everything. `serde`'s default gives exactly that, and it is the
/// owner's standing decision — history stays unless somebody says otherwise, at the moment they say
/// it.
#[derive(Deserialize)]
pub(super) struct RemoveProjectQuery {
    #[serde(default)]
    pub(super) forget_history: bool,
}

/// Take a project off the roster.
///
/// **Nothing on the disk is touched here, and there is no flag that would make it be.** Deleting a
/// folder is a second act with its own route, its own confirmation and its own refusals; a
/// `?delete_folder=true` on this one would be the same weight for two very different things, and
/// the caller that eventually passed it would be a caller that meant the reversible one.
pub(super) async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<RemoveProjectQuery>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    match crate::project_exit::remove(&state.pool, &id, query.forget_history).await {
        Ok(crate::project_exit::Removed::Done { forgotten }) => {
            tracing::info!(project_id = %id, forgotten, "a project left the roster");
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(crate::project_exit::Removed::NotRegistered) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "refusal": "not_registered" })),
        )),
        // 409 and not 423: the kill switch's `locked` means a standing decision that a person took,
        // and this is the opposite — work in progress that clears itself by finishing. The detail
        // names what is holding it, because "try again later" over an unnamed obstacle is how
        // somebody ends up pressing a button every minute.
        Ok(crate::project_exit::Removed::Held(holds)) => Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "refusal": "in_flight",
                "detail": held_by(&holds),
                "holds": holds,
            })),
        )),
        // Only reachable with `forget_history`, and the sentence says the escape rather than the
        // constraint: "FOREIGN KEY constraint failed" is true and useless, and the thing to do is
        // one untick away.
        Ok(crate::project_exit::Removed::Referenced) => Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "refusal": "history_referenced",
                "detail": "something outside this project still points at its history — \
                           removing it without forgetting works, and nothing was touched",
            })),
        )),
        Err(error) => {
            tracing::warn!(project_id = %id, %error, "removing a project failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "refusal": "internal" })),
            ))
        }
    }
}

/// A guard that would refuse, named so the screen can say it before anybody presses anything.
#[derive(serde::Serialize)]
pub(super) struct FolderBlock {
    pub(super) refusal: &'static str,
    pub(super) detail: String,
}

/// What deleting this project's folder would take, and whether it would be allowed at all.
///
/// **Three separate questions, kept apart in three fields**, because a screen has to say a different
/// sentence for each and a single "can I?" boolean would collapse them. `only_here` is what would be
/// lost for ever; `blocked` is a standing refusal about the path itself; `holds` is work in flight,
/// which clears on its own.
#[derive(serde::Serialize)]
pub(super) struct FolderView {
    /// The recorded root, or `null` for a project nobody ever pointed anywhere.
    pub(super) root: Option<String>,
    /// Whether that folder is actually on this disk. A project pointed at a folder that has already
    /// gone has nothing to delete, and the screen says so rather than offering the button.
    pub(super) exists: bool,
    /// What is in there and nowhere else, or `null` for a folder git knows nothing about — which is
    /// the more serious answer and not a missing one.
    pub(super) only_here: Option<crate::inspect::OnlyHere>,
    pub(super) blocked: Option<FolderBlock>,
    pub(super) holds: crate::project_exit::Holds,
}

pub(super) async fn get_project_folder(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<FolderView>, StatusCode> {
    let recorded: Option<Option<String>> =
        sqlx::query_scalar("SELECT project_root FROM autopilot_state WHERE project_id = ?")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Some(recorded) = recorded else {
        return Err(StatusCode::NOT_FOUND);
    };

    let blocked = match crate::project_exit::folder_check(&state.pool, &id, recorded.as_deref())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        Ok(_) => None,
        Err(refusal) => Some(FolderBlock {
            refusal: refusal.code(),
            detail: refusal.detail(),
        }),
    };

    let record = crate::project_exit::record(&state.pool, &id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    /*
       Two `git` subprocesses and a `stat`, on the blocking pool for the reason `get_projects` puts
       its own probe there: a project root on a disconnected network share blocks for as long as the
       operating system feels like, and this future is on an executor thread shared by every caller.
       Asked only when the folder is really there — git in a directory that does not exist answers
       with an error nobody would learn anything from.
    */
    let root = recorded.clone();
    let (exists, only_here) = tokio::task::spawn_blocking(move || match root {
        None => (false, None),
        Some(root) => {
            let path = std::path::Path::new(&root);
            if !path.is_dir() {
                return (false, None);
            }
            (true, crate::inspect::only_here(path).ok().flatten())
        }
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(FolderView {
        root: recorded,
        exists,
        only_here,
        blocked,
        holds: record.holds,
    }))
}

/// Delete a project's folder, and take it off the roster.
///
/// **The kill switch covers this, and it is the clearest case there is for it.** Everything else the
/// switch stops is autonomous — this is a person pressing a button — but it is also the largest
/// thing this app can do, and the switch's meaning is *nothing that changes anything happens right
/// now*. A folder deletion sailing past an engaged emergency stop would be the one exception nobody
/// would expect.
pub(super) async fn delete_project_folder(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<RemoveProjectQuery>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let halted = crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "refusal": "internal" })),
            )
        })?;
    if halted {
        return Err((
            StatusCode::LOCKED,
            Json(serde_json::json!({ "refusal": "kill_switch" })),
        ));
    }

    match crate::project_exit::delete_folder(&state.pool, &id, query.forget_history).await {
        Ok(crate::project_exit::Deleted::Done { forgotten }) => {
            tracing::info!(project_id = %id, forgotten, "a project's folder was deleted");
            Ok(StatusCode::NO_CONTENT)
        }
        Ok(crate::project_exit::Deleted::NotRegistered) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "refusal": "not_registered" })),
        )),
        Ok(crate::project_exit::Deleted::Held(holds)) => Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "refusal": "in_flight",
                "detail": held_by(&holds),
                "holds": holds,
            })),
        )),
        Ok(crate::project_exit::Deleted::Refused(refusal)) => Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "refusal": refusal.code(),
                "detail": refusal.detail(),
            })),
        )),
        // Logged at `error` and not `warn`: this is the state that needs a person, and it is the
        // only outcome in this file where the machine is left partway through something.
        Ok(crate::project_exit::Deleted::Partial { root, error }) => {
            tracing::error!(project_id = %id, %root, %error, "a folder was only partly deleted");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "refusal": "partly_deleted",
                    "detail": format!(
                        "{id} is off the roster and {root} did not fully delete: {error}. \
                         What is left has to be removed by hand."
                    ),
                })),
            ))
        }
        Err(error) => {
            tracing::warn!(project_id = %id, %error, "deleting a project folder failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "refusal": "internal" })),
            ))
        }
    }
}

/// The refusal's sentence, built where the numbers are rather than in the shell.
///
/// Both halves are named when both are non-zero. They are genuinely two facts — a run can hold a
/// slot before it has checked anything out, and a checkout can outlive the run that made it — and a
/// sentence that mentioned only the larger number would send somebody to wait for work that had
/// already finished.
pub(super) fn held_by(holds: &crate::project_exit::Holds) -> String {
    fn plural(count: i64, one: &str, many: &str) -> String {
        format!("{count} {}", if count == 1 { one } else { many })
    }

    let mut parts = Vec::new();
    if holds.slots > 0 {
        parts.push(format!(
            "{} still working here",
            plural(holds.slots, "slot", "slots")
        ));
    }
    if holds.worktrees > 0 {
        parts.push(format!(
            "{} still checked out",
            plural(holds.worktrees, "worktree", "worktrees")
        ));
    }
    format!("{} — nothing was removed", parts.join(" and "))
}

/// The four readings the project workspace leads with, in one answer.
///
/// One route and not four because all four are aggregations over the same rows in the same window —
/// this project's finished runs — and four routes would be four walks of one table for one panel.
///
/// No 404 for a project with no root. Unlike `ls` and `cat`, this asks nothing of the disk: a
/// project whose folder has moved still has a history of runs, and a reading of it is exactly what
/// somebody looking at a broken project wants. `resolve_project_root` would refuse it.
pub(super) async fn get_project_readings(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<WindowQuery>,
) -> Result<Json<crate::project::readings::Readings>, StatusCode> {
    let days = query
        .days
        .unwrap_or(crate::project::readings::DEFAULT_WINDOW_DAYS);
    crate::project::readings::readings(&state.pool, &id, days, chrono::Utc::now())
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "project readings failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// What `GET /projects/{id}/tests-map` answers: the state of the project's `nucleos.tests.yaml`
/// and, always, what this daemon would propose — beside a valid map too, so the owner can see
/// what a new build file would add.
#[derive(Debug, Serialize)]
pub(super) struct TestsMapView {
    /// `absent`, `invalid` or `valid`.
    pub(super) state: &'static str,
    pub(super) errors: Vec<String>,
    pub(super) groups: Vec<String>,
    pub(super) proposal: crate::detect::TestsMapProposal,
}

pub(super) fn tests_map_view(root: &std::path::Path) -> TestsMapView {
    let (state, errors, groups) = match crate::tests_map::load(root) {
        crate::tests_map::MapState::Absent => ("absent", Vec::new(), Vec::new()),
        crate::tests_map::MapState::Invalid(errors) => ("invalid", errors, Vec::new()),
        crate::tests_map::MapState::Valid(map) => {
            ("valid", Vec::new(), map.tests.groups.into_keys().collect())
        }
    };
    TestsMapView {
        state,
        errors,
        groups,
        proposal: crate::detect::propose_tests_map(root),
    }
}

pub(super) async fn get_project_tests_map(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TestsMapView>, StatusCode> {
    let root = resolve_project_root(&state, &id).await?;
    tokio::task::spawn_blocking(move || tests_map_view(&root))
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// One row of the write boundary, as the page draws it.
///
/// Owned strings now, because half the table is: an installed workflow's rows are read off its
/// manifest per project. They arrived as ROWS and not as a second shape, which is what the first
/// version of this struct said would happen.
///
/// `writable` is the field §12 asks for. Three states, not two: a file the app authors, a file
/// somebody else authors, and everything else — and the middle one needs its own answer because it
/// gets a different exit. The page must not infer it from `owner`, because `owner` is a name and
/// tomorrow there is a workflow called `core`.
#[derive(serde::Serialize)]
pub(super) struct ClaimView {
    /// The row's identity: what `POST /write` and `GET /owned` are sent back. Relative to `home`.
    pub(super) path: String,
    /// `state` for a file in `~/.nucleos/projects/<id>/`, `project` for one in the project's folder.
    pub(super) home: &'static str,
    /// What a person is shown — `~/.nucleos/projects/<id>/autopilot.yaml` for a state file. Served
    /// rather than composed by the page, so the page never carries a location of its own.
    pub(super) display: String,
    pub(super) owner: String,
    pub(super) what: String,
    pub(super) writable: bool,
}

/// Which files in this project the app is the legitimate author of.
///
/// **Served rather than hard-coded in the shell, and that is the point of §7.3.** A boundary the
/// client carries its own copy of is a boundary that goes out of date silently: the page would
/// offer an editor for a file the daemon refuses, or hide one for a file it would accept. Here the
/// only thing that decides is `ownership.rs`, and the page draws what it is told.
///
/// The root is resolved even though the table does not depend on it. A fence drawn for a project
/// the daemon has no folder for is a fence around nothing, and the write route answers 404 on the
/// same call — so this fails in the same place rather than showing an editor that cannot save.
pub(super) async fn get_project_ownership(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ClaimView>>, StatusCode> {
    let root = resolve_project_root(&state, &id).await?;
    let claims = claims_for_project(&state, &id, root).await?;
    Ok(Json(
        claims
            .into_iter()
            .map(|claim| ClaimView {
                writable: claim.validate.is_some(),
                home: match claim.home {
                    crate::ownership::Home::State => "state",
                    crate::ownership::Home::Project => "project",
                },
                display: claim.display(&id),
                path: claim.path.into_owned(),
                owner: claim.owner.into_owned(),
                what: claim.what.into_owned(),
            })
            .collect(),
    ))
}

/// The text of one file this app declares itself the author of, from wherever the registry says it
/// lives.
///
/// **Its own route because `cat` cannot serve it any more.** `cat` reads inside the project's
/// folder, and the núcleo's own two files moved out of it, to `~/.nucleos/projects/<id>/`. The page
/// asks for a claim by its identity and this answers from the claim's home — so neither the page nor
/// the caller ever names a directory.
///
/// 404 with `absent` for a claimed file that does not exist yet: that is the ordinary state of a
/// project nobody has given rules, and the editor offers to create it. 403 `not_ours` for anything
/// the table does not name, exactly as the write route refuses it.
///
/// In no table in `auth.rs`, and so Admin's by default-deny: the rules file carries `gate_command`
/// and `GET /projects/{id}/rules`, which serves the same facts, has always been Admin's. When the
/// file lived in the project a read-only key could `cat` it; the move is where that stops, and it is
/// the right place for it to stop.
pub(super) async fn get_project_owned(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ReadQuery>,
) -> Result<String, (StatusCode, Json<serde_json::Value>)> {
    let root = resolve_project_root(&state, &id)
        .await
        .map_err(|status| refusal(status, "no_project_root"))?;
    let claims = claims_for_project(&state, &id, root.clone())
        .await
        .map_err(|status| refusal(status, "internal"))?;
    let rel = query.path.unwrap_or_default();
    let crate::ownership::Owner::Declared(claim) = crate::ownership::owner_of(&claims, &rel) else {
        return Err(refusal(StatusCode::FORBIDDEN, "not_ours"));
    };
    let home = claim_home(&state, &id, &root, claim)?;
    let path = claim.path.clone().into_owned();
    tokio::task::spawn_blocking(move || {
        let target = inspect::safe_join(&home, &path)
            .map_err(|error| refusal(inspect_status(error), "unreadable"))?;
        match std::fs::read_to_string(&target) {
            Ok(text) => Ok(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(refusal(StatusCode::NOT_FOUND, "absent"))
            }
            Err(error) => {
                tracing::warn!(%error, "reading a claimed file failed");
                Err(refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
            }
        }
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
}

/* ------------------------------------------------------------- onboarding -- */

#[derive(Deserialize)]
pub(super) struct OnboardingQuery {
    /// The folder to read, for a project that has no root on record yet — one being added, or one
    /// in `off`, whose root `set_project_mode` cleared. Absent means the root on record.
    pub(super) root: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct OnboardRequest {
    /// As [`OnboardingQuery::root`].
    #[serde(default)]
    pub(super) project_root: Option<String>,
    /// The gate command a person confirmed. Absent, null or blank is "none confirmed", which leaves
    /// the project's rules exactly as they are — see `onboarding::onboard`.
    #[serde(default)]
    pub(super) gate_command: Option<String>,
}

/// What a person reads before onboarding a project, and what they read after.
#[derive(Serialize)]
pub(super) struct OnboardingView {
    pub(super) project_id: String,
    pub(super) root: String,
    /// The marker, when the project was onboarded; `null` when it was not.
    pub(super) onboarded: Option<crate::onboarding::Marker>,
    /// Where the marker is, as a person is shown it.
    pub(super) marker_path: String,
    pub(super) harnesses: Vec<crate::detect::Harness>,
    /// The gate this app would propose from the folder (`detect::propose_gate`).
    pub(super) proposed_gate: Option<crate::detect::GateProposal>,
    /// The gate the project's rules already name, which the field should start from when there is
    /// one: onboarding again must not quietly swap a gate somebody chose for a guess.
    pub(super) configured_gate: Option<String>,
    /// Whether this daemon's classifier hook is wired at the root right now.
    pub(super) hook_wired: bool,
}

/// The folder onboarding is about: the one the request names, or the one on record.
///
/// A named folder is held to what `GET /projects/detect` holds a path to — absolute, there, a
/// folder — because pointing at a folder is what bringing a project in IS, exactly as adding one
/// is. The same key reaches both doors, and it is the owner's.
pub(super) async fn onboarding_root(
    state: &AppState,
    id: &str,
    named: Option<String>,
) -> Result<PathBuf, (StatusCode, Json<serde_json::Value>)> {
    let Some(named) = named
        .map(|root| root.trim().to_string())
        .filter(|root| !root.is_empty())
    else {
        return resolve_project_root(state, id)
            .await
            .map_err(|status| refusal(status, "no_project_root"));
    };
    let root = PathBuf::from(named);
    if !root.is_absolute() {
        return Err(refusal(StatusCode::BAD_REQUEST, "not_absolute"));
    }
    match tokio::fs::metadata(&root).await {
        Ok(meta) if meta.is_dir() => Ok(root),
        Ok(_) => Err(refusal(StatusCode::UNPROCESSABLE_ENTITY, "not_a_folder")),
        Err(_) => Err(refusal(StatusCode::NOT_FOUND, "no_such_folder")),
    }
}

pub(super) async fn onboarding_view(
    state: &AppState,
    id: &str,
    root: PathBuf,
) -> Result<OnboardingView, (StatusCode, Json<serde_json::Value>)> {
    let machine_root = machine_config_root(state)?;
    let project_id = id.to_string();
    tokio::task::spawn_blocking(move || {
        let found = crate::detect::inspect_folder(&root);
        // An unreadable rules file has no gate this can name; the POST refuses it by name.
        let configured_gate = crate::config::load_schedule_rules(Some(&machine_root), &project_id)
            .ok()
            .and_then(|rules| rules.gate_command);
        OnboardingView {
            onboarded: crate::onboarding::read_marker(Some(&machine_root), &project_id),
            marker_path: crate::project::state::display_path(
                &project_id,
                crate::onboarding::MARKER_FILE,
            ),
            harnesses: found.harnesses,
            proposed_gate: found.gate,
            configured_gate,
            hook_wired: crate::governance::autopilot::classifier_hook_is_wired(&root),
            root: root.to_string_lossy().into_owned(),
            project_id,
        }
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
}

/// What onboarding would find and propose, and whether it already happened.
///
/// Reads and proposes, like `GET /projects/detect`; nothing is stored until the POST. Admin's by
/// default-deny, beside the POST: it reads the project's rules, and `GET /projects/{id}/rules`,
/// which serves the same gate, has always been Admin's.
pub(super) async fn get_project_onboarding(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<OnboardingQuery>,
) -> Result<Json<OnboardingView>, (StatusCode, Json<serde_json::Value>)> {
    project_state_dir(&state, &id)?;
    let root = onboarding_root(&state, &id, query.root).await?;
    onboarding_view(&state, &id, root).await.map(Json)
}

/// Onboards a project to NucleOS, with the gate command a person confirmed. Idempotent.
///
/// The one thing activation now asks of a project besides the classifier hook (`autopilot.rs`),
/// and the act is `onboarding::onboard`'s: check the gate, detect the harnesses, install the hook
/// with the same `wire_classifier_hook` every other door uses, store the gate as `gate_command` in
/// the project's `autopilot.yaml` — where the gate already lives — and write the marker last.
///
/// **In no table in `auth.rs`, so Admin's by default-deny**, for `POST /projects/{id}/write`'s
/// reason and a second one: it sets what *green* means, and it writes an executable hook into a
/// folder. The folder may be named in the body, which the two session doors that wire the hook
/// refuse to allow; here it is allowed because a project being brought in has no folder on record
/// yet, and naming one is what `GET /projects/detect` already lets this same key do.
///
/// **Kill-switch checked first, like `POST /write` and `POST /config/machine`.** Onboarding writes
/// `gate_command` into the project's `autopilot.yaml` and installs an executable classifier hook —
/// the same shape of governance write those two routes already refuse the stop for — so a stop
/// engaged here must refuse before either lands. The shell's new-project wizard used to lean on
/// this route succeeding while the stop was engaged, because registering a project runs onboarding
/// as its first step; it now catches this refusal there, registers the project anyway, and leaves
/// onboarding for the project's own Settings once the stop is released (see `NewProject.tsx`).
///
/// Refusals, all `{refusal, detail?}`: `kill_switch` (423) first, then the root's
/// (`no_project_root`, `not_absolute`, `no_such_folder`, `not_a_folder`), the state directory's
/// (`no_machine_root`, `bad_project_id`), then `invalid_gate` and `rules_unreadable` (422, with the
/// reason) before anything is written, and `hook_unwritable` (409, with the reason) for a
/// `.claude/settings.json` this cannot parse.
pub(super) async fn post_project_onboard(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<OnboardRequest>,
) -> Result<Json<OnboardingView>, (StatusCode, Json<serde_json::Value>)> {
    // Unreadable reads as engaged, the rule `assistant.rs` already pins: a stop nobody can ask
    // about is not a stop anybody may assume is off.
    if crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return Err(refusal(StatusCode::LOCKED, "kill_switch"));
    }

    project_state_dir(&state, &id)?;
    let machine_root = machine_config_root(&state)?;
    let root = onboarding_root(&state, &id, body.project_root).await?;
    // Asked before onboarding writes anything. The wizard onboards a project before it registers
    // it, so no roster row is what a NEW project looks like here — and only a new one gets the
    // default workflow; a project already on the roster keeps whatever it uses. An unreadable
    // roster reads as rostered: pinning is the one of the two answers that changes something.
    let rostered =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM autopilot_state WHERE project_id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .map(|rows| rows > 0)
            .unwrap_or(true);
    let home = machine_root.clone();
    let gate = body
        .gate_command
        .map(|command| command.trim().to_string())
        .filter(|command| !command.is_empty());

    let (project_id, project_root, confirmed) = (id.clone(), root.clone(), gate.clone());
    let now = chrono::Utc::now().to_rfc3339();
    tokio::task::spawn_blocking(move || {
        crate::onboarding::onboard(
            &machine_root,
            &project_id,
            &project_root,
            confirmed.as_deref(),
            &now,
        )
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map_err(|error| match error {
        crate::onboarding::OnboardError::BadId => {
            refusal(StatusCode::UNPROCESSABLE_ENTITY, "bad_project_id")
        }
        crate::onboarding::OnboardError::Gate(detail) => {
            refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "invalid_gate", detail)
        }
        crate::onboarding::OnboardError::Rules(detail) => {
            refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "rules_unreadable", detail)
        }
        crate::onboarding::OnboardError::Hook(detail) => {
            refusal_with_detail(StatusCode::CONFLICT, "hook_unwritable", detail)
        }
        crate::onboarding::OnboardError::Io(error) => {
            tracing::warn!(%error, project_id = %id, "onboarding a project failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    })?;

    let summary = match &gate {
        Some(command) => format!(
            "onboarded at {}: classifier hook installed, gate command confirmed as `{command}`",
            root.display()
        ),
        None => format!(
            "onboarded at {}: classifier hook installed, no gate command confirmed",
            root.display()
        ),
    };
    // After the act, and loudly on failure, for `post_project_write`'s reason. Spelled as a literal
    // and not as `onboarding::FEED_KIND`, because the shell's completeness test reads feed kinds out
    // of this source; the test below holds the two equal.
    if let Err(error) = feed::append(
        &state.pool,
        Some(&id),
        "project_onboarded",
        &summary,
        None,
        None,
    )
    .await
    {
        tracing::error!(
            %error,
            project_id = %id,
            "a project was onboarded and its feed line was not recorded"
        );
    }
    if !rostered && let Some(library) = state.workflow_library.clone() {
        pin_default_workflow(&state, &id, &root, home, library).await;
    }

    onboarding_view(&state, &id, root).await.map(Json)
}

/// Pin the default workflow for a project being brought in, place its files in the project, and
/// say so in the feed.
///
/// **Never fails the onboarding.** The hook and the gate are what onboarding promises; the
/// workflow is a default on top of them, and a project whose files could not be placed is told so
/// in the feed line (`materialize_after_pin`) rather than refused a hook it already has.
async fn pin_default_workflow(
    state: &AppState,
    id: &str,
    root: &std::path::Path,
    machine_root: PathBuf,
    library: PathBuf,
) {
    let (project_id, project_root) = (id.to_string(), root.to_path_buf());
    let pinned = tokio::task::spawn_blocking(move || {
        let Some(bundle) =
            crate::onboarding::pin_default_workflow(&machine_root, &library, &project_id)?
        else {
            return Ok(None);
        };
        let state_dir = crate::project::state::dir(&machine_root, &project_id)
            .ok_or_else(|| format!("`{project_id}` cannot name a state directory"))?;
        let placed = super::workflows::materialize_after_pin(
            &project_root,
            &state_dir.join(crate::project::state::PINS_FILE),
            &library,
            &state_dir.join(crate::project::state::MATERIALIZED_FILE),
            &bundle.name,
        );
        Ok::<_, String>(Some((bundle, placed)))
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|result| result);
    match pinned {
        Ok(Some((bundle, placed))) => {
            super::workflows::workflow_feed(
                state,
                id,
                &format!(
                    "{}@{} pinned by default for a new project{placed}",
                    bundle.name, bundle.version
                ),
            )
            .await
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, project_id = %id, "the default workflow was not pinned")
        }
    }
}

#[derive(Deserialize)]
pub(super) struct DetectQuery {
    /// An absolute path to a folder. See `detect.rs` for why this route takes one at all.
    pub(super) path: String,
}

/// What is already in a folder somebody is about to add.
///
/// **§9's second step.** A project worth adding has a history, commands its people type, and often a
/// written-down way of working — this repository's is `.ai/`, and it built NucleOS. This reports all
/// of it so the wizard can propose rather than interrogate.
///
/// The git half runs even when the folder turns out not to be a repository: `is_git` is a finding
/// and not a refusal, because `set_project_mode` only insists on a repository for `active`, and a
/// wizard that refused to look at a folder would be deciding a question the last step asks.
///
/// In no scope table, like every route under `/projects/{id}/workflows`: it reads a path nobody has
/// vouched for, so `permits` — default-deny — leaves it to the key of the person at the machine.
pub(super) async fn get_project_detect(
    State(state): State<AppState>,
    Query(query): Query<DetectQuery>,
) -> Result<Json<crate::detect::Detected>, (StatusCode, Json<serde_json::Value>)> {
    let root = std::path::PathBuf::from(query.path.trim());
    if !root.is_absolute() {
        return Err(refusal(StatusCode::BAD_REQUEST, "not_absolute"));
    }

    let root = tokio::fs::canonicalize(&root)
        .await
        .map_err(|_| refusal(StatusCode::NOT_FOUND, "no_such_folder"))?;
    if !root.is_dir() {
        return Err(refusal(StatusCode::UNPROCESSABLE_ENTITY, "not_a_folder"));
    }

    let looking = root.clone();
    let mut found = tokio::task::spawn_blocking(move || crate::detect::inspect_folder(&looking))
        .await
        .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?;

    if found.is_git {
        let deadline = std::time::Instant::now() + DETECT_GIT_BUDGET;
        found.branch = crate::execution::git_exec::current_branch(&root, deadline)
            .await
            .ok();
        let (remote, head) = crate::execution::git_exec::origin_and_head(&root, deadline).await;
        found.remote = remote;
        found.head = head;
    }

    // The one thing here that cannot be seen from the folder. Adding a project twice under two
    // names is the mistake this prevents, and neither name would look wrong on its own.
    found.taken_by = already_registered(&state, &root).await;
    Ok(Json(found))
}

/// A project this daemon already keeps at this folder, if any.
///
/// Compared after canonicalising both sides, because the stored root is whatever string was handed
/// in when the project was registered — a trailing slash, a different case on Windows, a path
/// through a junction — and a plain string comparison would report "no" for the same folder.
pub(super) async fn already_registered(state: &AppState, root: &std::path::Path) -> Option<String> {
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT project_id, project_root FROM autopilot_state")
            .fetch_all(&state.pool)
            .await
            .ok()?;

    for (project_id, stored) in rows {
        let Some(stored) = stored else { continue };
        let Ok(stored) = tokio::fs::canonicalize(&stored).await else {
            continue;
        };
        if stored == root {
            return Some(project_id);
        }
    }
    None
}

/// What `POST /projects/{id}/judge` accepts. A null brain switches the judge off.
#[derive(serde::Deserialize)]
pub(super) struct JudgeRequest {
    /// `local` or `openrouter`. Null means nobody answers but a person.
    pub(super) brain: Option<String>,
    /// Which model, when a brain is named. Null means that brain's configured one.
    ///
    /// A choice id from `GET /assistant/models`, checked against that list and against the brain
    /// beside it — the same menu `patch_chat` validates a pinned model against, and for a sharper
    /// version of the same reason. There a wrong name costs a turn that dies at spawn, which is
    /// visible. Here it costs nothing visible at all: an unservable judge is indistinguishable
    /// from no judge, so the mistake would present as the feature simply not working.
    pub(super) model: Option<String>,
}

/// Names this project's judge, or switches it off.
///
/// **Both halves of the request are checked against something real, and that is what makes a picker
/// safe to build on this route.** Until this check existed the route took any brain spelling and
/// any model string under 4 KiB, and stored them. Neither failure was loud: an unservable model
/// makes `assistant_for` refuse, `judge_for` return `None`, and the person be asked — which is the
/// SAME behaviour as having no judge at all. So somebody who mistyped a model name would configure
/// a judge, see the panel say a judge was configured, and never once be answered by it.
///
/// The brain arm is subtler and is why it is not a two-line match. `Brain::from_wire` folds
/// anything it cannot parse to `Cloud` — documented, deliberate, and safe for a column default —
/// and `declare_judge` refuses `Cloud` with a sentence about the CLI re-entering this hook. Fold a
/// typo through both and somebody who wrote `lokal` is told, truthfully and uselessly, about the
/// cloud route. A word this daemon has no route for is refused as that; `cloud` is passed through
/// to `declare_judge`, which owns the reason it cannot judge.
///
/// The refusal for `cloud` therefore still comes back from `declare_judge` rather than being
/// restated here: the reason is about what a cloud brain IS, and a second copy of that sentence in
/// the routing layer is a second copy free to drift from the first.
pub(super) async fn post_project_judge(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<JudgeRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    project_is_on_the_roster(&state, &id).await?;

    let brain = match body.brain.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(named) => {
            let route = crate::chats::Brain::from_wire(named);
            if route == crate::chats::Brain::Cloud && named != crate::chats::Brain::Cloud.as_str() {
                return Err(unusable_judge(format!(
                    "`{named}` is not a route this daemon has; a judge is `local` or `openrouter`"
                )));
            }
            Some(route)
        }
    };

    if let Some(model) = body.model.as_deref() {
        within_length("model", model)?;
    }
    // Blank is nothing, not a name. The column already means "that brain's configured model" when
    // it is NULL, and storing an empty string would be a second spelling of the same state that
    // only `assistant_for` would ever discover was not one.
    let model = body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty());

    // Only when a brain was named. `brain: null` is the switch-off, and `declare_judge` drops the
    // model beside it — so a model there is not a judge's model and has nothing to be checked
    // against.
    if let (Some(named), Some(model)) = (brain, model) {
        match chosen_brain(&state, model, Asking::Daemon).await {
            Ok(route) if route == named => {}
            Ok(route) => {
                return Err(unusable_judge(format!(
                    "`{model}` is answered by the {} route, not {}",
                    route.as_str(),
                    named.as_str()
                )));
            }
            Err(BrainRefusal::UnknownModel) => {
                return Err(unusable_judge(format!(
                    "nothing on this daemon's model menu is called `{model}`"
                )));
            }
            // `can_serve`'s own words, carried through rather than summarised — it is the layer that
            // knows WHY, and the two cases that reach here are the ordinary ones: a local model
            // this machine has not pulled, and a hosted model named with no key stored.
            Err(BrainRefusal::CannotServe(why)) => return Err(unusable_judge(why)),
        }
    }

    crate::project::policy::declare_judge(&state.pool, &id, brain, model)
        .await
        .map_err(unusable_judge)?;

    Ok(StatusCode::NO_CONTENT)
}

/// The one refusal this route gives, so its three callers cannot spell the code three ways.
pub(super) fn unusable_judge(detail: String) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({ "refusal": "unusable_judge", "detail": detail })),
    )
}

/// Puts the project back on the default judge.
///
/// 404 when there was no row, off the DELETE's own `rows_affected` — the same shape
/// `delete_project_land_target` uses, and the same honesty: a project that never named a judge is
/// already on the default, and reporting a deletion that did not happen would be a lie the window
/// would draw as a change.
pub(super) async fn delete_project_judge(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    project_is_on_the_roster(&state, &id).await?;

    let cleared = crate::project::policy::clear_judge(&state.pool, &id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, project_id = %id, "clearing a project judge failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })?;

    if cleared {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(refusal(StatusCode::NOT_FOUND, "no_judge"))
    }
}
