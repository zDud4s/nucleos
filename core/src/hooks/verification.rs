//! Watching a run's verification commands and refusing the ones that would fake a pass.

use super::*;

/// What a refused verification command is told: the daemon verifies, the run does not.
pub(super) const VERIFY_GUARD_REFUSAL: &str = "this project verifies through the daemon: call mcp__nucleos__verify and wait on it with mcp__nucleos__verify_status instead of running build, test or gate commands yourself";

/// F2b steps 1-2: records a verification command the run executed by itself, and says whether
/// the call must be refused (`true`).
///
/// Step 1 only observes. Step 2 may refuse, and only by explicit opt-in (`verify_guard: refuse`
/// in the project's rules file that was actually read); it never replaces a classifier `deny`
/// and is not counted against the prober allowance. A missing or invalid tests map, an
/// unreadable rules file or a failed insert all end in `false` (or a warning), never in a
/// different answer for the call.
///
/// Step 3 detects against the union of the worktree's tests map and the project root's
/// (`verify_guard::detection_union`): the agent owns the worktree's file, not the root's. A missing
/// or invalid worktree map leaves the root's alone; a missing or invalid root map (or none
/// recorded) leaves the worktree's alone.
pub(super) async fn observe_verification(
    state: &AppState,
    run_id: i64,
    project_id: &str,
    cwd: &str,
    payload: &PreToolUsePayload,
    decision: &str,
    shadow_decision_id: Option<i64>,
) -> bool {
    let Some(command) = payload.tool_input.get("command").and_then(Value::as_str) else {
        return false;
    };
    use crate::tests_map::MapState;
    let target_root = match crate::inspect::project_root(&state.pool, project_id).await {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(
                run_id,
                %error,
                "pretooluse-decision: failed to read the project root for the target test map"
            );
            None
        }
    };
    let root = std::path::PathBuf::from(cwd);
    let (tree, target) = match tokio::task::spawn_blocking(move || {
        (
            crate::tests_map::load(&root),
            target_root.map(|root| crate::tests_map::load(std::path::Path::new(&root))),
        )
    })
    .await
    {
        Ok(maps) => maps,
        Err(_) => return false,
    };
    if matches!(&target, Some(MapState::Invalid(_))) {
        tracing::warn!(
            run_id,
            "pretooluse-decision: the project root's tests map is invalid; detecting against the worktree's alone"
        );
    }
    let map = match (tree, target) {
        (MapState::Valid(tree), Some(MapState::Valid(target))) => {
            crate::verify::guard::detection_union(&target, &tree)
        }
        (MapState::Valid(map), _) | (_, Some(MapState::Valid(map))) => map,
        _ => return false,
    };
    let machine_root = state.machine_config_root.as_deref();
    let (gate, guard) = match crate::config::load_schedule_rules(machine_root, project_id) {
        Ok(rules) => (rules.gate_command, rules.verify_guard),
        Err(error) => {
            tracing::warn!(
                run_id,
                %error,
                "pretooluse-decision: failed to read the project's gate command"
            );
            (None, crate::config::VerifyGuardMode::Observe)
        }
    };
    let Some(hit) = crate::verify::guard::detect(
        command,
        classifier::shell_for(&payload.tool_name),
        &map,
        gate.as_deref(),
    ) else {
        return false;
    };
    let refused = guard == crate::config::VerifyGuardMode::Refuse && decision != "deny";
    if let Err(error) = crate::verify::observe::record(
        &state.pool,
        &crate::verify::observe::Observation {
            project_id,
            run_id,
            shadow_decision_id,
            tool_name: &payload.tool_name,
            command,
            hit: &hit,
            decision: if refused { "deny" } else { decision },
        },
    )
    .await
    {
        tracing::warn!(
            run_id,
            %error,
            "pretooluse-decision: failed to record a verification observation"
        );
    }
    refused
}
