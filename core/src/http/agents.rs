//! Agents, loadouts, presets and model routing (`/agents`, `/loadout/*`, `/presets`, `/models`, `/route`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/agents", get(list_agents).post(create_agent))
        .route(
            "/agents/{id}",
            get(get_agent).put(update_agent).delete(delete_agent),
        )
        .route("/presets", get(list_presets).post(create_preset))
        .route(
            "/presets/{id}",
            get(get_preset).put(update_preset).delete(delete_preset),
        )
        .route("/presets/{id}/run", post(run_preset))
        // What the llm-router advised against what launched, for judging shadow -> apply. A pure
        // read of `runs`; see `route_report.rs`.
        .route(
            "/route/report",
            get(crate::models::route_report::get_route_report),
        )
        // The owner routes below are Control-only and in no `auth.rs` table. The request route is
        // the run's own door and is gated by its frozen loadout in `auth::LOADOUT_ROUTES`.
        .route("/loadout/tool-requests", post(post_tool_request))
        .route("/loadout/tools", get(list_loadout_tools))
        .route("/loadout/tools/{id}/approve", post(approve_loadout_tool))
        .route("/loadout/tools/{id}/reject", post(reject_loadout_tool))
        .route("/loadout/tools/{id}/revoke", post(revoke_loadout_tool))
        // Read-only: resolves what an agent would receive and records nothing. Owner-only too.
        .route("/loadout/preview", post(preview_loadout))
        .route("/models/latest", get(get_latest_models))
}

pub(super) fn agent_status(error: &agent::AgentError) -> StatusCode {
    match error {
        agent::AgentError::DuplicateName => StatusCode::CONFLICT,
        // The agent exists and the request is well formed; what refuses is the team standing on it.
        agent::AgentError::InUse => StatusCode::CONFLICT,
        // A package agent changes by shipping a new package version, not through the catalogue.
        agent::AgentError::PackageReadOnly => StatusCode::CONFLICT,
        agent::AgentError::Invalid(_) => StatusCode::BAD_REQUEST,
        agent::AgentError::NotFound => StatusCode::NOT_FOUND,
        agent::AgentError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// An agent as the API answers it: the stored fields plus the runner policy derived from its
/// capabilities. Read-only; `tool_policy` is never accepted on a write.
#[derive(Serialize)]
pub(super) struct AgentView {
    #[serde(flatten)]
    agent: agent::Agent,
    tool_policy: &'static str,
}

impl From<agent::Agent> for AgentView {
    fn from(agent: agent::Agent) -> Self {
        let tool_policy = agent.tool_policy();
        Self { agent, tool_policy }
    }
}

pub(super) async fn list_agents(
    State(state): State<AppState>,
) -> Result<Json<Vec<AgentView>>, StatusCode> {
    agent::list(&state.pool)
        .await
        .map(|agents| Json(agents.into_iter().map(AgentView::from).collect()))
        .map_err(|error| {
            tracing::warn!(%error, "listing agents failed");
            agent_status(&error)
        })
}

pub(super) async fn create_agent(
    State(state): State<AppState>,
    Json(request): Json<agent::AgentRequest>,
) -> Result<Json<AgentView>, StatusCode> {
    agent::create(&state.pool, request)
        .await
        .map(|created| Json(created.into()))
        .map_err(|error| {
            tracing::warn!(%error, "creating agent failed");
            agent_status(&error)
        })
}

pub(super) async fn get_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<AgentView>, StatusCode> {
    match agent::get(&state.pool, &id).await {
        Ok(Some(found)) => Ok(Json(found.into())),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(error) => {
            tracing::warn!(agent_id = %id, %error, "reading agent failed");
            Err(agent_status(&error))
        }
    }
}

pub(super) async fn update_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<agent::AgentRequest>,
) -> Result<Json<AgentView>, StatusCode> {
    agent::update(&state.pool, &id, request)
        .await
        .map(|updated| Json(updated.into()))
        .map_err(|error| {
            tracing::warn!(agent_id = %id, %error, "updating agent failed");
            agent_status(&error)
        })
}

pub(super) async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    agent::delete(&state.pool, &id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| {
            tracing::warn!(agent_id = %id, %error, "deleting agent failed");
            agent_status(&error)
        })
}

#[derive(Deserialize)]
pub(super) struct ToolRequestBody {
    pub(super) tool: String,
    #[serde(default)]
    pub(super) reason: String,
}

/// `POST /loadout/tool-requests`: a run asks for one more tool. The run is the key's own, or the
/// node named by `RUN_ID_HEADER` for a team key; `auth::loadout_admits` already checked it.
pub(super) async fn post_tool_request(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ToolRequestBody>,
) -> axum::response::Response {
    let run_id = match scope {
        Scope::Run(id) => Some(id),
        _ => headers
            .get(crate::daemon_client::RUN_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<i64>().ok()),
    };
    let Some(run_id) = run_id else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match crate::tool_loadout::request(&state.pool, run_id, &body.tool, &body.reason).await {
        Ok(requested) => {
            let status = if requested.created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            (
                status,
                Json(serde_json::json!({
                    "id": requested.id,
                    "status": requested.status,
                    "created": requested.created,
                })),
            )
                .into_response()
        }
        Err(crate::tool_loadout::RequestError::Db(error)) => {
            tracing::warn!(run_id, %error, "recording a tool request failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        Err(error) => (StatusCode::UNPROCESSABLE_ENTITY, error.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct LoadoutToolsQuery {
    pub(super) owner_kind: Option<String>,
    pub(super) owner_id: Option<String>,
    pub(super) status: Option<String>,
}

pub(super) async fn list_loadout_tools(
    State(state): State<AppState>,
    Query(query): Query<LoadoutToolsQuery>,
) -> Result<Json<Vec<crate::tool_loadout::ToolRow>>, StatusCode> {
    crate::tool_loadout::list(
        &state.pool,
        query.owner_kind.as_deref(),
        query.owner_id.as_deref(),
        query.status.as_deref(),
    )
    .await
    .map(Json)
    .map_err(|error| {
        tracing::warn!(%error, "listing loadout tools failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

#[derive(Deserialize, Default)]
pub(super) struct ApproveLoadoutBody {
    pub(super) owner: Option<String>,
    pub(super) team_id: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct LoadoutNoteBody {
    pub(super) note: Option<String>,
}

/// An empty body is the same as `{}`; anything else must parse.
pub(super) fn optional_json_body<T: serde::de::DeserializeOwned + Default>(
    bytes: &axum::body::Bytes,
) -> Result<T, StatusCode> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(bytes).map_err(|_| StatusCode::BAD_REQUEST)
}

pub(super) fn decision_response(
    result: Result<crate::tool_loadout::ToolRow, crate::tool_loadout::DecisionError>,
) -> axum::response::Response {
    use crate::tool_loadout::DecisionError;
    match result {
        Ok(row) => Json(row).into_response(),
        Err(DecisionError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(DecisionError::WrongState) => StatusCode::CONFLICT.into_response(),
        Err(DecisionError::UnknownTeam) => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
        Err(DecisionError::Db(error)) => {
            tracing::warn!(%error, "deciding a loadout tool failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub(super) async fn approve_loadout_tool(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let body: ApproveLoadoutBody = match optional_json_body(&body) {
        Ok(body) => body,
        Err(status) => return status.into_response(),
    };
    let target = match (body.owner.as_deref(), body.team_id) {
        (None | Some("agent"), _) => crate::tool_loadout::Target::Agent,
        (Some("team"), Some(team)) if !team.is_empty() => crate::tool_loadout::Target::Team(team),
        _ => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
    };
    decision_response(crate::tool_loadout::approve(&state.pool, id, target).await)
}

pub(super) async fn reject_loadout_tool(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let body: LoadoutNoteBody = match optional_json_body(&body) {
        Ok(body) => body,
        Err(status) => return status.into_response(),
    };
    decision_response(crate::tool_loadout::reject(&state.pool, id, body.note.as_deref()).await)
}

pub(super) async fn revoke_loadout_tool(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let body: LoadoutNoteBody = match optional_json_body(&body) {
        Ok(body) => body,
        Err(status) => return status.into_response(),
    };
    decision_response(crate::tool_loadout::revoke(&state.pool, id, body.note.as_deref()).await)
}

/// The box a preview resolves in: the same lists and policy the spawn site of that box hands over.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum PreviewBox {
    Team,
    JobNode,
}

#[derive(Deserialize)]
pub(super) struct PreviewLoadoutBody {
    pub(super) agent_id: String,
    #[serde(rename = "box")]
    pub(super) equipment_box: PreviewBox,
    pub(super) team_id: Option<String>,
    pub(super) project_id: Option<String>,
    #[serde(default)]
    pub(super) task: String,
}

#[derive(Serialize)]
pub(super) struct PreviewTool {
    pub(super) name: String,
    /// `base` (the box serves it to every run), `agent` or `team` (an approval of that owner).
    pub(super) origin: &'static str,
}

#[derive(Serialize)]
pub(super) struct PreviewRef {
    pub(super) owner_kind: &'static str,
    pub(super) path: String,
    pub(super) kind: String,
    pub(super) note: Option<String>,
    /// `active`, `missing` (listed, marked "(em falta)") or `refused` (left out of the index).
    pub(super) state: &'static str,
}

#[derive(Serialize)]
pub(super) struct PreviewLoadout {
    pub(super) agent_id: String,
    pub(super) team_id: Option<String>,
    #[serde(rename = "box")]
    pub(super) equipment_box: &'static str,
    /// The memory half only: the brief's block, `None` when nothing was selected or readable.
    pub(super) memory: Option<String>,
    /// Exactly what would be appended to the prompt: memory plus the context index.
    pub(super) block: String,
    pub(super) tools: Vec<PreviewTool>,
    pub(super) refs: Vec<PreviewRef>,
    pub(super) add_dirs: Vec<String>,
}

/// The tools one owner holds an active approval for; no owner holds none.
pub(super) async fn active_tools_of(
    pool: &sqlx::SqlitePool,
    owner_kind: &str,
    owner_id: Option<&str>,
) -> sqlx::Result<Vec<String>> {
    let Some(owner_id) = owner_id else {
        return Ok(Vec::new());
    };
    let rows =
        crate::tool_loadout::list(pool, Some(owner_kind), Some(owner_id), Some("active")).await?;
    Ok(rows.into_iter().map(|row| row.tool).collect())
}

/// `POST /loadout/preview`: what an agent would receive at spawn in the named box, for a sample
/// task. It goes through `loadout::resolve` like every spawn site and never calls `record`, so it
/// writes no `run_loadout` row.
pub(super) async fn preview_loadout(
    State(state): State<AppState>,
    Json(body): Json<PreviewLoadoutBody>,
) -> axum::response::Response {
    let internal = |error: sqlx::Error| {
        tracing::warn!(%error, "previewing a loadout failed");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    };
    let tool_policy = match agent::get(&state.pool, &body.agent_id).await {
        Ok(Some(found)) => found.tool_policy(),
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::warn!(%error, "previewing a loadout failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let team = body.team_id.as_deref().filter(|team| !team.is_empty());
    if let Some(team) = team {
        match sqlx::query_scalar::<_, i64>("SELECT 1 FROM teams WHERE id = ?")
            .bind(team)
            .fetch_optional(&state.pool)
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
            Err(error) => return internal(error),
        }
    }

    // The same equipment `team.rs` and `job.rs` hand the resolver for these boxes.
    let (equipment, node, label) = match body.equipment_box {
        PreviewBox::Team => (
            crate::loadout::Equipment {
                tool_policy: if tool_policy == "mcp_only" {
                    "mcp_only"
                } else {
                    "none"
                },
                base: crate::mcp_tools::TEAM_BASE,
                extras: crate::mcp_tools::TEAM_EXTRAS,
                managed_root: state.files_root.as_deref(),
                has_cwd: false,
            },
            None,
            "team",
        ),
        PreviewBox::JobNode => (
            crate::loadout::Equipment {
                tool_policy: "mcp_only",
                base: crate::mcp_tools::JOB_NODE_BASE,
                extras: crate::mcp_tools::JOB_NODE_EXTRAS,
                managed_root: state.files_root.as_deref(),
                has_cwd: true,
            },
            // An agent's job item runs as an implement node.
            crate::loadout::node_kind("implement"),
            "job_node",
        ),
    };
    let base = equipment.base;
    let loadout = crate::loadout::resolve(
        &state.pool,
        &crate::loadout::LoadoutInput {
            agent: Some(&body.agent_id),
            team,
            project: body
                .project_id
                .as_deref()
                .filter(|project| !project.is_empty()),
            job: None,
            task_text: &body.task,
            node,
            files: &[],
            equipment: Some(equipment),
        },
    )
    .await;
    let run = loadout.run.unwrap_or_default();

    // Where each extra came from: an active approval of the agent first, then of the team.
    let by_agent = match active_tools_of(&state.pool, "agent", run.agent.as_deref()).await {
        Ok(tools) => tools,
        Err(error) => return internal(error),
    };
    let by_team = match active_tools_of(&state.pool, "team", run.team.as_deref()).await {
        Ok(tools) => tools,
        Err(error) => return internal(error),
    };
    let tools = run
        .tools
        .iter()
        .map(|name| PreviewTool {
            name: name.clone(),
            origin: if base.contains(&name.as_str()) {
                "base"
            } else if by_team.contains(name) && !by_agent.contains(name) {
                "team"
            } else {
                "agent"
            },
        })
        .collect();

    // The refs as the resolver saw them: it offers them only to a run that holds `read_context`.
    let mut refs = Vec::new();
    if let Some(root) = state.files_root.as_deref()
        && run.tools.iter().any(|tool| tool == "read_context")
    {
        let spawned = match crate::context_refs::spawn_refs(
            &state.pool,
            root,
            run.agent.as_deref(),
            run.team.as_deref(),
        )
        .await
        {
            Ok(spawned) => spawned,
            Err(error) => return internal(error),
        };
        for spawned_ref in spawned {
            refs.push(PreviewRef {
                owner_kind: spawned_ref.owner_kind.as_str(),
                path: spawned_ref.path,
                kind: spawned_ref.kind,
                note: spawned_ref.note,
                state: match spawned_ref.state {
                    crate::context_refs::SpawnState::Active => "active",
                    crate::context_refs::SpawnState::Missing => "missing",
                    crate::context_refs::SpawnState::Refused => "refused",
                },
            });
        }
    }

    Json(PreviewLoadout {
        agent_id: body.agent_id,
        team_id: run.team,
        equipment_box: label,
        memory: loadout.brief.and_then(|brief| brief.block),
        block: loadout.block,
        tools,
        refs,
        add_dirs: run
            .add_dirs
            .iter()
            .map(|dir| dir.display().to_string())
            .collect(),
    })
    .into_response()
}

pub(super) fn preset_status(error: &presets::PresetError) -> StatusCode {
    match error {
        presets::PresetError::DuplicateName => StatusCode::CONFLICT,
        presets::PresetError::Invalid(_) => StatusCode::BAD_REQUEST,
        presets::PresetError::NotFound => StatusCode::NOT_FOUND,
        presets::PresetError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub(super) async fn list_presets(
    State(state): State<AppState>,
) -> Result<Json<Vec<presets::Preset>>, StatusCode> {
    presets::list(&state.pool).await.map(Json).map_err(|error| {
        tracing::warn!(%error, "listing presets failed");
        preset_status(&error)
    })
}

pub(super) async fn create_preset(
    State(state): State<AppState>,
    Json(request): Json<presets::PresetRequest>,
) -> Result<Json<presets::Preset>, StatusCode> {
    presets::create(&state.pool, &request.name, request.run)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "creating preset failed");
            preset_status(&error)
        })
}

pub(super) async fn get_preset(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<presets::Preset>, StatusCode> {
    presets::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(preset_id = id, %error, "reading preset failed");
            preset_status(&error)
        })?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

pub(super) async fn update_preset(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(request): Json<presets::PresetRequest>,
) -> Result<Json<presets::Preset>, StatusCode> {
    presets::update(&state.pool, id, &request.name, request.run)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(preset_id = id, %error, "updating preset failed");
            preset_status(&error)
        })
}

pub(super) async fn delete_preset(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    presets::delete(&state.pool, id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| {
            tracing::warn!(preset_id = id, %error, "deleting preset failed");
            preset_status(&error)
        })
}

pub(super) async fn run_preset(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<runs::CreateRunResponse>, (StatusCode, String)> {
    let preset = presets::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(preset_id = id, %error, "reading preset to run failed");
            (
                preset_status(&error),
                "the preset could not be read; the daemon logged why".to_owned(),
            )
        })?
        .ok_or((StatusCode::NOT_FOUND, format!("there is no preset {id}")))?;

    // Delegate to the sole person-initiated run front door. It owns the fail-closed global kill
    // switch, uncancellable launch window, and conversion from run-domain errors to HTTP status.
    //
    // **Which now includes the project's autopilot mode, and a preset is subject to it like any
    // other request.** A preset stored with an unattended mode against a project since switched off
    // is refused here rather than run — the alternative being a saved button that quietly does what
    // the same request typed by hand would be refused for, which is the shape of hole this door was
    // just closed against.
    //
    // The refusal's own sentence travels back out through this route rather than being flattened to
    // a bare status, which is why this signature changed with the door's.
    runs::create_run(
        State(state),
        Json(runs::CreateRunRequest {
            prompt: preset.prompt,
            project_id: preset.project_id,
            cwd: preset.cwd,
            mode: preset.mode,
            // A preset records what to run, not who may speak into it afterwards, and there is no
            // column here that could say otherwise. Every preset already stored was written before
            // steering existed, so `false` is the answer each of them was saved with — a preset must
            // not become a way to obtain a listening run that its author never asked for.
            steerable: false,
            permission_mode: None,
        }),
    )
    .await
}

/// Query parameters for `GET /models/latest`.
#[derive(serde::Deserialize)]
pub(super) struct LatestModelsQuery {
    pub(super) vendor: Option<String>,
}

/// The newest models per vendor, for the `latest_models` agent tool. The same snapshot the picker
/// serves, so there is one list; `current()` only waits on the process's very first call.
pub(super) async fn get_latest_models(
    Query(query): Query<LatestModelsQuery>,
) -> (StatusCode, Json<serde_json::Value>) {
    let snapshot = crate::models::model_catalog::current().await;
    match crate::models::model_catalog::latest(&snapshot, query.vendor.as_deref()) {
        Ok(body) => (StatusCode::OK, Json(body)),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error })),
        ),
    }
}
