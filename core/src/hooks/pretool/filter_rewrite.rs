//! The `--filter-run` rewrite of a recognized test/check/build command of an in-flight run.

use super::*;

/// The replacement input that routes a recognized test/check/build command of an in-flight run
/// through `--filter-run`, or `None` for anything else. Best effort: never panics, and a failure
/// to read anything simply leaves the command untouched.
pub(super) async fn filter_rewrite(
    state: &AppState,
    run_id: i64,
    tool_name: &str,
    tool_input: &Value,
) -> Option<Value> {
    if !state
        .run_handles
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains_key(&run_id)
    {
        return None;
    }
    let project_id: Option<String> = run_cache::run_facts(state, run_id, true)
        .await
        .ok()??
        .project_id
        .clone();
    let kinds = match project_id.as_deref() {
        Some(pid) => crate::project::rules::autopilot(pid)
            .map(|r| r.command_kinds)
            .unwrap_or_default(),
        None => crate::config::CommandKinds::default(),
    };
    let command = tool_input.get("command").and_then(Value::as_str)?;
    if crate::governance::command_kind::kind_of(command, classifier::shell_for(tool_name), &kinds)
        == crate::governance::command_kind::Kind::Other
    {
        return None;
    }
    // A replaced binary names a path that no longer exists (Linux: "<path> (deleted)"): fail open.
    let exe = std::env::current_exe().ok().filter(|p| p.is_file())?;
    let data = crate::daemon_client::data_dir()?;
    crate::execution::output_filter::rewrite_input(
        tool_name,
        tool_input,
        &exe,
        &crate::execution::output_filter::run_log_dir(&data, run_id),
    )
}
