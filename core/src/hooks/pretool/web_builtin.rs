//! The branch of the PreToolUse decision that lets a job node use Claude's `WebFetch`/`WebSearch`.

use super::*;

/// A job node whose agent declares `web` holds Claude's `WebFetch`/`WebSearch`: its loadout lists
/// them (`loadout::grant_builtins`). Everything else keeps falling through to the classifier,
/// which refuses these as an `unrecognized-tool` - an attended run, a team run, a run that is
/// not a job's node, or a node whose loadout does not list the tool.
///
/// Both tools bring third-party text into the run, so the read is recorded first, exactly as for
/// a `ReadsUntrusted` extra, and a failure to record it refuses the call: a read that
/// cannot be marked would let the run's later writes go ungoverned by that mark.
///
/// `Some` is the decision; `None` means fall through to the rules that follow.
pub(super) async fn web_builtin_decision(
    state: &AppState,
    payload: &PreToolUsePayload,
    run_id: i64,
    is_in_flight: bool,
    mode: &str,
) -> Option<Json<Decision>> {
    if !(job_node_web_builtin(&payload.tool_name)
        && is_in_flight
        && crate::execution::runs::runs_unattended(mode))
    {
        return None;
    }
    let deny = |reason: &str| {
        Some(Json(Decision {
            decision: "deny".to_owned(),
            reason: reason.to_owned(),
        }))
    };
    let job_id = match job_id_of(&state.pool, run_id).await {
        Ok(Some(job_id)) => job_id,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(
                run_id = run_id,
                %error,
                "pretooluse-decision: failed to resolve the run's job — failing closed"
            );
            return deny("could not resolve the run's job — failing closed");
        }
    };
    match crate::tool_loadout::run_lists_tool(&state.pool, run_id, &payload.tool_name).await {
        Ok(true) => {
            if let Err(error) =
                crate::execution::runs::mark_untrusted_context(&state.pool, run_id).await
            {
                tracing::warn!(
                    run_id = run_id,
                    tool = %payload.tool_name,
                    %error,
                    "pretooluse-decision: could not record that a job node read third-party content - refusing the read"
                );
                return deny("could not record that this run read third-party content");
            }
            tracing::info!(
                run_id = run_id,
                job_id = job_id,
                tool = %payload.tool_name,
                "pretooluse-decision: allowed a web tool the agent's loadout lists"
            );
            Some(Json(Decision {
                decision: "allow".to_owned(),
                reason: "this run's loadout lists it".to_owned(),
            }))
        }
        // Not listed: the classifier's own refusal says it, unchanged.
        Ok(false) => None,
        Err(error) => {
            tracing::warn!(
                run_id = run_id,
                %error,
                "pretooluse-decision: failed to read the run's loadout — failing closed"
            );
            deny("could not read the run's loadout — failing closed")
        }
    }
}
