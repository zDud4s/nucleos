//! The timeout on the classifier hook's tool-event entries that are already on disk.
//!
//! A shell command can wait for admission inside the hook (`admit_heavy`), far longer than the
//! CLI's default hook timeout. A new entry is written with [`HOOK_TIMEOUT_SECONDS`]; an entry
//! wired before admission existed has none, or a short one, and is raised here.

use serde_json::Value;

use super::{HOOK_SCRIPT, HOOK_TIMEOUT_SECONDS};

/// Raises OUR inner hook's timeout in one event's existing entries and touches nothing else: not
/// another hook in the same entry, and not ours when it already allows as long or longer.
pub(super) fn raise_short_timeouts(list: &mut [Value]) {
    for existing in list.iter_mut() {
        let Some(inner_hooks) = existing.get_mut("hooks").and_then(Value::as_array_mut) else {
            continue;
        };
        for inner in inner_hooks {
            let ours = inner
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| command.contains(HOOK_SCRIPT));
            let short = inner
                .get("timeout")
                .and_then(Value::as_u64)
                .is_none_or(|seconds| seconds < HOOK_TIMEOUT_SECONDS);
            if ours && short {
                inner["timeout"] = serde_json::json!(HOOK_TIMEOUT_SECONDS);
            }
        }
    }
}
