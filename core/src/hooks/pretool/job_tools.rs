//! The branches of the PreToolUse decision that let a job node call tools an unattended run
//! would otherwise be refused: its box's base tools and the extras its loadout lists.

use super::*;

/// Whether `tool_name` is one of the job-node tools: `mcp__nucleos__<name>` for a `<name>` in
/// `mcp_tools::JOB_NODE_BASE`, matched as a WHOLE name and never by prefix or wildcard.
///
/// This is the base list an unattended run may call, and it is a branch in
/// `pretooluse_decision` and not a classifier class: the classifier is pure and per-tool, and a
/// class would also have to be kept away from the judge and the scoreboard. A widening of
/// `JOB_NODE_BASE` widens this by construction, so that list is the thing review must watch. The
/// extras (`JOB_NODE_EXTRAS`) are admitted only from the run's loadout, see `job_node_extra`.
fn is_job_node_tool(tool_name: &str) -> bool {
    tool_name
        .strip_prefix("mcp__nucleos__")
        .is_some_and(|name| crate::mcp_tools::JOB_NODE_BASE.contains(&name))
}

/// The `<n>` of a whole-segment `mcp__nucleos__<n>` that names a job-node extra, else `None`.
fn job_node_extra(tool_name: &str) -> Option<&str> {
    tool_name
        .strip_prefix("mcp__nucleos__")
        .filter(|name| !name.contains("__"))
        .filter(|name| crate::mcp_tools::JOB_NODE_EXTRAS.contains(name))
}

/// Whether `tool_name` is a Claude built-in web tool an agent's `web` capability can grant: a WHOLE
/// name match on `WebFetch` or `WebSearch`. Unlike `job_node_extra` these are the engine's own
/// tools, not `mcp__nucleos__` ones, so the grant lives in the run's loadout under the bare name.
pub(super) fn job_node_web_builtin(tool_name: &str) -> bool {
    crate::loadout::is_engine_builtin(tool_name)
}

/// The job a run belongs to, `None` for a run that is not a job's node (or whose row is gone).
pub(super) async fn job_id_of(
    pool: &sqlx::SqlitePool,
    run_id: i64,
) -> Result<Option<i64>, sqlx::Error> {
    Ok(
        sqlx::query_scalar::<_, Option<i64>>("SELECT job_id FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await?
            .flatten(),
    )
}

/// A job's node may call its box's base tools (a finding, verification, `request_tool`,
/// `read_context`): the `mcp__` tools an unattended run is allowed without a loadout row. Before
/// the classifier, like the branches around it, so it never reaches the judge, never parks and
/// writes no scoreboard row (the scoreboard measures the classifier, and this is not its decision).
/// Anything short of ALL the conditions falls through (`None`) and is refused as an unrecognised
/// tool.
pub(super) async fn base_tool_decision(
    state: &AppState,
    payload: &PreToolUsePayload,
    run_id: i64,
    is_in_flight: bool,
    mode: &str,
) -> Option<Json<Decision>> {
    if !(is_job_node_tool(&payload.tool_name)
        && is_in_flight
        && crate::execution::runs::runs_unattended(mode))
    {
        return None;
    }
    match job_id_of(&state.pool, run_id).await {
        Ok(Some(job_id)) => {
            // A base tool that carries a third party's words (`read_context`) taints the run
            // exactly as an extra does: the mark is what the later steps are judged against.
            let bare = payload
                .tool_name
                .strip_prefix("mcp__nucleos__")
                .unwrap_or_default();
            if crate::mcp_tools::effect_of_call(&state.pool, bare, &payload.tool_input).await
                == crate::mcp_tools::ToolEffect::ReadsUntrusted
                && let Err(error) =
                    crate::execution::runs::mark_untrusted_context(&state.pool, run_id).await
            {
                tracing::warn!(
                    run_id = run_id,
                    tool = bare,
                    %error,
                    "pretooluse-decision: could not record that a job node read third-party content - refusing the read"
                );
                return Some(Json(Decision {
                    decision: "deny".to_owned(),
                    reason: "could not record that this run read third-party content".to_owned(),
                }));
            }
            tracing::info!(
                run_id = run_id,
                job_id = job_id,
                tool = %payload.tool_name,
                "pretooluse-decision: allowed a job-node tool"
            );
            Some(Json(Decision {
                decision: "allow".to_owned(),
                reason: "a job's run may use its box's base tools".to_owned(),
            }))
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                run_id = run_id,
                %error,
                "pretooluse-decision: failed to resolve the run's job — failing closed"
            );
            Some(Json(Decision {
                decision: "deny".to_owned(),
                reason: "could not resolve the run's job — failing closed".to_owned(),
            }))
        }
    }
}

/// A job's node may also call an EXTRA tool its run's loadout lists. Same preconditions as the
/// base branch; a tool that is not listed is refused with the way to ask for it.
pub(super) async fn extra_tool_decision(
    state: &AppState,
    payload: &PreToolUsePayload,
    run_id: i64,
    is_in_flight: bool,
    mode: &str,
) -> Option<Json<Decision>> {
    let extra = job_node_extra(&payload.tool_name)?;
    if !(is_in_flight && crate::execution::runs::runs_unattended(mode)) {
        return None;
    }
    let job_id = match job_id_of(&state.pool, run_id).await {
        Ok(Some(job_id)) => job_id,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(
                run_id = run_id,
                %error,
                "pretooluse-decision: failed to resolve the run's job — failing closed"
            );
            return Some(Json(Decision {
                decision: "deny".to_owned(),
                reason: "could not resolve the run's job — failing closed".to_owned(),
            }));
        }
    };
    match crate::tool_loadout::run_lists_tool(&state.pool, run_id, extra).await {
        Ok(true) => {
            let effect =
                crate::mcp_tools::effect_of_call(&state.pool, extra, &payload.tool_input).await;
            if effect == crate::mcp_tools::ToolEffect::ReadsUntrusted
                && let Err(error) =
                    crate::execution::runs::mark_untrusted_context(&state.pool, run_id).await
            {
                tracing::warn!(
                    run_id = run_id,
                    tool = extra,
                    %error,
                    "pretooluse-decision: could not record that a job node read third-party content - refusing the read"
                );
                return Some(Json(Decision {
                    decision: "deny".to_owned(),
                    reason: "could not record that this run read third-party content".to_owned(),
                }));
            }
            tracing::info!(
                run_id = run_id,
                job_id = job_id,
                tool = extra,
                "pretooluse-decision: allowed a job-node extra tool its loadout lists"
            );
            Some(Json(Decision {
                decision: "allow".to_owned(),
                reason: "this run's loadout lists it".to_owned(),
            }))
        }
        Ok(false) => Some(Json(Decision {
            decision: "deny".to_owned(),
            reason: format!(
                "{extra} is not available to an autonomous run: it is not in this run's loadout; ask the owner for it with request_tool"
            ),
        })),
        Err(error) => {
            tracing::warn!(
                run_id = run_id,
                %error,
                "pretooluse-decision: failed to read the run's loadout — failing closed"
            );
            Some(Json(Decision {
                decision: "deny".to_owned(),
                reason: "could not read the run's loadout — failing closed".to_owned(),
            }))
        }
    }
}
