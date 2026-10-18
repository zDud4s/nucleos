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
/// or invalid worktree map leaves the root's alone.
///
/// Without a valid root map (or none recorded), the trusted map is the worktree's own as it stood
/// at the commit the daemon recorded the tree was born on (`worktrees.base_sha`, see [`base_map`])
/// — read out of git, which the agent's edits to the working file cannot reach — and the working
/// file can only add to it. Only when there is no such base map either does the working file
/// decide alone: then there was no guard to weaken, and a map the agent writes only tightens it.
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
            "pretooluse-decision: the project root's tests map is invalid; detecting against the worktree's base and working maps"
        );
    }
    let trusted = match target {
        Some(MapState::Valid(target)) => Some(target),
        _ => base_map(state, run_id, cwd).await,
    };
    let map = match (trusted, tree) {
        (Some(trusted), MapState::Valid(tree)) => {
            crate::verify::guard::detection_union(&trusted, &tree)
        }
        (Some(map), _) | (None, MapState::Valid(map)) => map,
        (None, _) => return false,
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

/// How long reading the base map out of the history may take before the guard goes on without it.
const BASE_MAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The tests map of the worktree at `cwd` as it stood at the commit the daemon recorded the tree
/// was born on, or `None` when there is no such map to trust.
///
/// Both halves of the read are out of the agent's reach: `base_sha` is written by the daemon when
/// it creates the tree, and the blob is read through the project root's repository (the row's
/// `project_root`, not the worktree's own `.git` file) by object id, with replace refs off. So
/// deleting, corrupting or narrowing `nucleos.tests.yaml` in the tree — or committing that edit —
/// leaves this answer as it was.
///
/// `None` for a tree with no recorded base (rows from before migration 0062), a
/// base without a map, a base map the loader refuses, or a git that would not answer — each of
/// them a tree whose base had no guard for the agent to weaken, or (the last) today's behaviour.
async fn base_map(state: &AppState, run_id: i64, cwd: &str) -> Option<crate::tests_map::TestsMap> {
    let row: Option<(String, String)> = match sqlx::query_as(
        "SELECT project_root, base_sha FROM worktrees
         WHERE path = ? AND removed_at IS NULL AND base_sha IS NOT NULL
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(cwd)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(row) => row,
        Err(error) => {
            tracing::warn!(
                run_id,
                %error,
                "pretooluse-decision: failed to read the worktree's base for its tests map"
            );
            return None;
        }
    };
    let (repo, sha) = row?;
    let object = format!("{sha}:{}", crate::tests_map::MAP_FILE);
    let argv = [
        std::ffi::OsStr::new("--no-replace-objects"),
        std::ffi::OsStr::new("cat-file"),
        std::ffi::OsStr::new("blob"),
        std::ffi::OsStr::new(&object),
    ];
    let text = match crate::execution::git_exec::run_git(
        std::path::Path::new(&repo),
        &argv,
        BASE_MAP_TIMEOUT,
    )
    .await
    {
        Ok(answer) if answer.succeeded() => answer.stdout,
        // Git looked and the base holds no map.
        Ok(_) => return None,
        Err(reason) => {
            tracing::warn!(
                run_id,
                %reason,
                "pretooluse-decision: could not read the worktree's base tests map"
            );
            return None;
        }
    };
    if text.len() as u64 > crate::tests_map::MAX_MAP {
        return None;
    }
    crate::tests_map::parse(&text).ok()
}
