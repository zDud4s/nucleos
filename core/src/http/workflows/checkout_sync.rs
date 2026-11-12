//! `POST /workflows/sync`: materialize a project's pinned workflows into a checkout somebody made
//! by hand, and record the project's integration branch in that repository's git config.

use super::*;

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
    let mut matches = loop {
        if crate::governance::autopilot::kill_switch_engaged(&state.pool)
            .await
            .unwrap_or(true)
        {
            return Err(refusal(StatusCode::LOCKED, "kill_switch"));
        }
        let deadline = std::time::Instant::now() + DETECT_GIT_BUDGET;
        let key = crate::execution::git_exec::repo_key(&checkout, deadline)
            .await
            .map_err(|detail| {
                refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "not_a_checkout", detail)
            })?;
        // Read before the roster, never after it. A repair writes the root and only then drops the
        // lease, so a lease seen gone here means the roster read below has the new root. Read
        // afterwards, a repair landing between the two looked like no repair at all, and the rows
        // it had just replaced became a `no_project`.
        let lease = super::projects::live_repair_wait(&state.pool)
            .await
            .map_err(|_| refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))?;
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
        if !matches.is_empty() {
            break matches;
        }
        let Some(remaining) = lease else {
            break matches;
        };
        tokio::time::sleep(remaining.min(std::time::Duration::from_millis(50))).await;
    };
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
    let is_main = tokio::fs::canonicalize(&root).await.ok().as_ref() == Some(&checkout);
    let record = if is_main {
        state_dir.join(crate::project::state::MATERIALIZED_FILE)
    } else {
        crate::workflows::materialize::worktree_record(&state_dir, &checkout)
    };
    let target = checkout.clone();
    let sync_id = project_id.clone();
    let outcomes = tokio::task::spawn_blocking(move || {
        crate::workflows::materialize::sync(
            &target,
            &sync_id,
            &state_dir,
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
    record_integration_branch(&state, &project_id, &root).await;

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
        workflow_feed(&state, &project_id, &synced_line(&checkout, &said)).await;
    }
    Ok(Json(WorkflowSyncAnswer {
        project_id,
        outcomes,
    }))
}

/// Writes the project's integration branch into its repository's local git config
/// (`nucleos.integrationBranch`), where the `reference-transaction` hook reads which branch only the
/// queue may move.
///
/// **Fail-open, and only a branch that is known.** A sync that placed its files is not undone by a
/// config write that failed, so every failure is a `warn!` and nothing more. A `Stale` or `Unknown`
/// reading writes nothing: closing a branch is only right when it is the branch work actually lands
/// on, and a wrong name would close the wrong one while leaving the real target open.
///
/// `integration_branch_reading` and not `integration_branch`: a sync must not pin the roster column
/// as a side effect, for the reason that function's doc gives for a GET.
async fn record_integration_branch(state: &AppState, project_id: &str, root: &std::path::Path) {
    let deadline = std::time::Instant::now() + DETECT_GIT_BUDGET;
    let branch = match crate::execution::land::integration_branch_reading(
        &state.pool,
        project_id,
        deadline,
    )
    .await
    {
        Ok(crate::execution::land::IntegrationBranch::Declared { branch })
        | Ok(crate::execution::land::IntegrationBranch::Derived { branch }) => branch,
        Ok(other) => {
            tracing::warn!(project_id, reading = ?other, "the integration branch is not known, so it was not recorded in git config");
            return;
        }
        Err(error) => {
            tracing::warn!(%error, project_id, "reading the integration branch failed, so it was not recorded in git config");
            return;
        }
    };
    if let Err(error) =
        crate::execution::git_exec::set_integration_config(root, &branch, deadline).await
    {
        tracing::warn!(%error, project_id, branch = %branch, "recording the integration branch in git config failed");
    }
}

/// The feed line of a sync. `checkout` is canonical and the line is for a person: the feed goes out
/// to the primary channel as it is, so the `\\?\` of a Windows canonical path stops here.
fn synced_line(checkout: &std::path::Path, said: &[String]) -> String {
    format!(
        "synced into {}: {}",
        crate::execution::git_exec::without_verbatim_prefix(checkout).display(),
        said.join("; ")
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_sync_line_names_the_checkout_without_the_verbatim_prefix() {
        assert_eq!(
            super::synced_line(
                std::path::Path::new(r"\\?\C:\Projects\nucleos"),
                &["harness: 1 written".to_string(), "b: 2 written".to_string()],
            ),
            r"synced into C:\Projects\nucleos: harness: 1 written; b: 2 written"
        );
    }
}
