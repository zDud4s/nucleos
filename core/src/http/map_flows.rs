//! Declared flows of a project, verified against its code (`/projects/{id}/map/flows`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/projects/{id}/map/flows", get(get_project_map_flows))
}

/// The flows a project declares in `.nucleos/flows.yaml`, each step checked against the code.
///
/// A GET and read-only: it reads the files the steps name, so it runs on a blocking thread.
pub(super) async fn get_project_map_flows(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::map::flows::Flows>, StatusCode> {
    let root = resolve_read_root(&state, &id, None).await?;
    tokio::task::spawn_blocking(move || crate::map::flows::check(&root))
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
