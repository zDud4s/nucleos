//! The state machines a project's code draws (`/projects/{id}/map/states`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/projects/{id}/map/states", get(get_project_map_states))
}

/// The machines plus the folder they were read from. `root` is the absolute folder so the shell
/// can open VS Code at an anchor; that is the same disclosure `/projects/{id}/worktree` makes.
#[derive(Serialize)]
pub(super) struct StatesAnswer {
    pub(super) root: String,
    #[serde(flatten)]
    pub(super) states: crate::map::states::States,
}

async fn get_project_map_states(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<StatesAnswer>, StatusCode> {
    let root = resolve_read_root(&state, &id, None).await?;
    let root_text = root.to_string_lossy().into_owned();
    let read = tokio::task::spawn_blocking(move || crate::map::states::machines(&root))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let states = read.map_err(|error| {
        // A renamed or deleted folder is something its owner did, not a fault of this daemon.
        if error.kind() == std::io::ErrorKind::NotFound {
            return StatusCode::NOT_FOUND;
        }
        tracing::warn!(%error, project_id = %id, "project map states failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(StatesAnswer {
        root: root_text,
        states,
    }))
}
