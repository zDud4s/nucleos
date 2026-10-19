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
    let record = materialized_record(&state, &id)?;
    let project_id = id.clone();

    let (installed, placed) = tokio::task::spawn_blocking(move || {
        let bundle = bundle_or_refusal(&library, &body.name, Some(&body.version))?;
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
    let record = materialized_record(&state, &id)?;
    let now = chrono::Utc::now();
    let project_id = id.clone();

    let (bundle, placed) = tokio::task::spawn_blocking(move || {
        let bundle = bundle_or_refusal(&library, &name, body.version.as_deref())?;
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
