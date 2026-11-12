//! The background half of `Stop`: runs the pinned bundle's timer, which wakes a session parked on
//! background work before its prompt cache expires. Its own script and not `ask_daemon.py`: it
//! sleeps for most of an hour, and `asyncRewake` turns its exit 2 into a wake-up instead of a
//! refused stop.

use serde_json::{Map, Value};

use super::HOOK_INTERPRETER;

/// Under the hook directory, which `materialize::reserved` keeps out of a bundle's reach. A directory
/// of its own because a worktree has to stay clean and no project tracks this file: the `.gitignore`
/// inside hides it without a git process or an edit to anything the project owns.
pub(super) const SCRIPT: &str = ".claude/hooks/nucleos/stop_timer.py";
/// What is left of `SCRIPT` once `wire_hook` has pointed the entry at the unit config dir.
const SCRIPT_TAIL: &str = "hooks/nucleos/stop_timer.py";
pub(super) const SOURCE: &str = include_str!("../../../hooks/stop_timer.py");

/// Enforced by Claude Code on an `asyncRewake` hook, so it has to outlast the longest window the
/// timer sleeps (55 minutes, `spec_loop_timer.py`).
const TIMEOUT_SECONDS: u64 = 3500;

/// Adds the timer to the `Stop` list `wire_event` has already made, once. Beside the entry for
/// `ask_daemon.py`, and judged by its own script path: `wire_event`'s dedupe looks for the other one.
pub(super) fn wire(hooks: &mut Map<String, Value>) {
    let Some(list) = hooks.get_mut("Stop").and_then(Value::as_array_mut) else {
        return;
    };
    let already = list
        .iter()
        .any(|existing| serde_json::to_string(existing).is_ok_and(|text| text.contains(SCRIPT_TAIL)));
    if !already {
        list.push(serde_json::json!({
            "hooks": [{
                "type": "command",
                "command": format!("{HOOK_INTERPRETER} \"${{CLAUDE_PROJECT_DIR}}/{SCRIPT}\""),
                "asyncRewake": true,
                "timeout": TIMEOUT_SECONDS
            }]
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::super::{HOOK_SCRIPT, wire_classifier_hook};
    use super::*;

    #[test]
    fn wiring_adds_the_stop_timer_once_and_writes_its_script() {
        let root = tempfile::TempDir::new().unwrap();
        wire_classifier_hook(root.path()).unwrap();
        wire_classifier_hook(root.path()).unwrap();

        let config = crate::engines::unit_config::unit_config_dir(root.path());
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(crate::engines::unit_config::settings_file(&config)).unwrap(),
        )
        .unwrap();
        let stop = settings["hooks"]["Stop"].as_array().unwrap();
        let timers: Vec<_> = stop
            .iter()
            .filter(|entry| serde_json::to_string(entry).unwrap().contains(SCRIPT_TAIL))
            .collect();
        assert_eq!(timers.len(), 1, "{stop:?}");
        assert_eq!(timers[0]["hooks"][0]["asyncRewake"], true);
        assert_eq!(timers[0]["hooks"][0]["timeout"], TIMEOUT_SECONDS);
        assert_eq!(
            std::fs::read_to_string(config.join("hooks/nucleos/stop_timer.py")).unwrap(),
            SOURCE
        );
        assert!(
            serde_json::to_string(&stop[0])
                .unwrap()
                .contains(HOOK_SCRIPT),
            "the forwarding entry stays first: {stop:?}"
        );
    }

    #[test]
    fn a_bundle_cannot_write_over_the_stop_timer() {
        assert!(crate::workflows::materialize::reserved(SCRIPT));
    }
}
