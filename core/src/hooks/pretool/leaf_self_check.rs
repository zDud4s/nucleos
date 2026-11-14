//! The leaf self-check admission: a tree leaf may run its self-check lines (see
//! `classifier::leaf_self_check_form`) that the classifier would otherwise park as unrecognized.

use super::*;

/// PURE: `autonomy.leaf_self_check` of a `speed.yaml` text. Absent, mistyped or unreadable is
/// `false`, the closed side.
pub(super) fn leaf_self_check_from_yaml(text: &str) -> bool {
    serde_yaml::from_str::<serde_yaml::Value>(text)
        .ok()
        .and_then(|doc| doc.get("autonomy")?.get("leaf_self_check")?.as_bool())
        .unwrap_or(false)
}

/// Whether the project's `speed.yaml` turns the leaf's `cargo check -p <crate> --tests` on. Read
/// from the MAIN checkout, never from the run's own worktree: the run could flip a switch it holds.
fn leaf_self_check_enabled(cwd: &Path) -> bool {
    cwd.ancestors()
        .find_map(crate::governance::autopilot::main_checkout_of)
        .is_some_and(|main| {
            std::fs::read_to_string(main.join(".nucleos/workflow/speed.yaml"))
                .is_ok_and(|text| leaf_self_check_from_yaml(&text))
        })
}

/// Whether this call is a tree leaf's self-check (S2): an `unrecognized` shell line that is exactly
/// one of the broker-wrapped forms `classifier::leaf_self_check_form` admits, run by a `Leaf`, with
/// the broker scripts byte-identical to the main checkout's. The cheap pure parse goes first, and
/// the role is read only once it matched. Never lifts a `deny` or a destructive class.
pub(super) async fn leaf_self_check_admitted(
    state: &AppState,
    payload: &PreToolUsePayload,
    run_id: i64,
    cwd: Option<&str>,
    classification: &classifier::Classification,
) -> bool {
    if classification.action_class != "unrecognized"
        || !matches!(payload.tool_name.as_str(), "Bash" | "PowerShell")
    {
        return false;
    }
    let (Some(command), Some(cwd)) = (
        payload.tool_input.get("command").and_then(Value::as_str),
        cwd.map(Path::new),
    ) else {
        return false;
    };
    if !classifier::leaf_self_check_form(command, leaf_self_check_enabled(cwd))
        || !classifier::leaf_self_check_scripts_intact(cwd, command)
    {
        return false;
    }
    let caller = crate::role_policy::Caller {
        agent_id: payload.agent_id.clone(),
        agent_type: payload.agent_type.clone(),
        // A call with subagent markers but no usable `agent_id` must not resolve to a leaf (D12).
        subagent: payload.subagent,
    };
    matches!(
        crate::role_policy::resolve(&state.pool, &caller, Some(run_id)).await,
        crate::role_policy::Role::Leaf
    )
}
