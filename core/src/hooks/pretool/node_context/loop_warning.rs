//! The live warning of spec `2026-11-12-encaixe-de-contexto-por-no` §6.3: a tree node's run that
//! loops is told so, in the context line of the PreToolUse answer. It mirrors the devtime rules
//! that need nothing but the calls the hook already sees:
//!
//! - **D1 THRASH**: the same (path, offset) `Read` `READ_REPEATS` times with no edit of the path
//!   in between.
//! - **D7 / D8**: the same shell command run `COMMAND_REPEATS` times with no edit in between (a
//!   check run again with nothing changed, or a command polled again and again). D3 (searches with
//!   no read) is not mirrored: it needs the whole lane, not a counter.
//!
//! The warning never denies and never changes the decision. It is shadow first: the switch is the
//! `devtime_action_state` row `(inject, CTX)`: no row or `shadow` records what would have been
//! said in `shadow_decisions` (reason `context_warning`) and adds nothing; `live` adds the
//! advisory; `off` does neither. Everything here fails open.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use serde_json::Value;

use crate::state::AppState;

/// The `devtime_action_state` pair that switches the warning.
const ACTION: &str = "inject";
const RULE: &str = "CTX";
/// The `reason` a would-have-warned row is stored under.
const CONTEXT_WARNING_REASON: &str = "context_warning";
const READ_REPEATS: u32 = 3;
const COMMAND_REPEATS: u32 = 3;
const TARGET_CHARS: usize = 120;
/// A hard bound on the runs tracked at once; the map is dropped whole past it.
const MAX_RUNS: usize = 256;

/// One loop the hook noticed.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Loop {
    /// The devtime rule it mirrors: `D1` or `D7`.
    pub rule: &'static str,
    pub target: String,
    pub count: u32,
}

impl Loop {
    fn advisory(&self) -> String {
        let what = if self.rule == "D1" {
            "re-reading"
        } else {
            "re-running"
        };
        format!(
            "You are going in circles: {what} `{}` ({} times) with nothing changed in between. \
             Narrow the task to what you can finish with what you already have, or ask for what \
             is missing with `contract_request`, instead of repeating the call.",
            self.target, self.count
        )
    }
}

#[derive(Default)]
struct RunLoop {
    reads: HashMap<(String, i64), u32>,
    commands: HashMap<String, u32>,
    /// Targets already reported, so a loop is said once, not on every further call.
    told: HashSet<String>,
}

impl RunLoop {
    /// Counts one call; `Some` the moment a pattern reaches its threshold.
    fn observe(&mut self, tool: &str, input: &Value) -> Option<Loop> {
        match tool {
            "Read" => {
                let path = input.get("file_path")?.as_str()?.replace('\\', "/");
                let offset = input.get("offset").and_then(Value::as_i64).unwrap_or(0);
                let count = bump(self.reads.entry((path.clone(), offset)).or_default());
                self.first_time("D1", path, count, READ_REPEATS)
            }
            "Bash" | "PowerShell" => {
                let command = input.get("command")?.as_str()?.trim().to_owned();
                // A sleep alone is waiting by hand (D8 i), not a repeated check.
                if command.is_empty() || command.starts_with("sleep") {
                    return None;
                }
                let count = bump(self.commands.entry(command.clone()).or_default());
                self.first_time("D7", command, count, COMMAND_REPEATS)
            }
            "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
                if let Some(path) = ["file_path", "notebook_path"]
                    .iter()
                    .find_map(|key| input.get(*key)?.as_str())
                {
                    let path = path.replace('\\', "/");
                    self.reads.retain(|(read, _), _| *read != path);
                }
                self.commands.clear();
                None
            }
            _ => None,
        }
    }

    fn first_time(
        &mut self,
        rule: &'static str,
        target: String,
        count: u32,
        at: u32,
    ) -> Option<Loop> {
        if count < at || !self.told.insert(format!("{rule}:{target}")) {
            return None;
        }
        Some(Loop {
            rule,
            target: target.chars().take(TARGET_CHARS).collect(),
            count,
        })
    }
}

fn bump(count: &mut u32) -> u32 {
    *count = count.saturating_add(1);
    *count
}

type Key = (usize, i64);

/// The entry keeps a clone of its pool, so the address its key is made of is never handed to
/// another pool while the entry exists.
static RUNS: LazyLock<Mutex<HashMap<Key, (sqlx::SqlitePool, RunLoop)>>> =
    LazyLock::new(Mutex::default);

fn observe_call(state: &AppState, run_id: i64, tool: &str, input: &Value) -> Option<Loop> {
    // `options()` lives inside the pool's shared allocation: every clone names it alike, and two
    // pools (the tests) never share a run id.
    let key = (std::ptr::from_ref(state.pool.options()) as usize, run_id);
    let mut runs = RUNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if runs.len() >= MAX_RUNS && !runs.contains_key(&key) {
        runs.clear();
    }
    runs.entry(key)
        .or_insert_with(|| (state.pool.clone(), RunLoop::default()))
        .1
        .observe(tool, input)
}

/// What the warning does for this run: the switch of `(inject, CTX)`. A read error is `shadow`,
/// the side that changes nothing the run sees.
async fn mode(state: &AppState) -> Option<String> {
    match crate::devtime::store::get_action_state(&state.pool, ACTION, RULE).await {
        Ok(Some(row)) if row.mode == "off" => None,
        Ok(Some(row)) => Some(row.mode),
        Ok(None) => Some("shadow".to_owned()),
        Err(error) => {
            tracing::warn!(%error, "pretooluse-decision: could not read the context warning's mode");
            Some("shadow".to_owned())
        }
    }
}

/// The advisory for this call, when the run behind a tree node has just looped and the warning is
/// live. In shadow it writes the `shadow_decisions` row and returns `None`.
pub(super) async fn loop_advisory(
    state: &AppState,
    run_id: i64,
    tool: &str,
    input: &Value,
) -> Option<String> {
    let in_flight = super::super::run_cache::in_flight(state, run_id);
    let facts = super::super::run_cache::run_facts(state, run_id, in_flight)
        .await
        .ok()??;
    facts.node.as_ref()?;
    let found = observe_call(state, run_id, tool, input)?;
    match mode(state).await?.as_str() {
        "live" => Some(found.advisory()),
        _ => {
            record_would_have_said(state, run_id, &found).await;
            None
        }
    }
}

/// Fail-open: a write error is logged and changes nothing about the answer.
async fn record_would_have_said(state: &AppState, run_id: i64, found: &Loop) {
    let classification = crate::governance::classifier::Classification {
        decision: crate::governance::classifier::Decision {
            decision: "allow".to_owned(),
            reason: CONTEXT_WARNING_REASON.to_owned(),
        },
        action_class: "context-warning",
        reason: CONTEXT_WARNING_REASON.to_owned(),
        culprit: None,
        segments: Vec::new(),
    };
    let input = serde_json::json!({
        "rule": found.rule,
        "target": found.target,
        "count": found.count,
        "would_say": found.advisory(),
    });
    if let Err(error) = crate::governance::shadow::record_decision(
        &state.pool,
        run_id,
        "context_warning",
        &input,
        &classification,
        state.github.policy.digest(),
    )
    .await
    {
        tracing::warn!(run_id, %error, "pretooluse-decision: failed to record the context warning");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(path: &str, offset: i64) -> Value {
        json!({"file_path": path, "offset": offset})
    }

    #[test]
    fn the_third_read_of_the_same_page_fires_once() {
        let mut run = RunLoop::default();
        assert!(run.observe("Read", &read("a.rs", 0)).is_none());
        assert!(run.observe("Read", &read("a.rs", 0)).is_none());
        let found = run.observe("Read", &read("a.rs", 0)).expect("third read");
        assert_eq!((found.rule, found.count), ("D1", 3));
        assert!(run.observe("Read", &read("a.rs", 0)).is_none(), "said once");
    }

    #[test]
    fn paging_and_an_edit_between_do_not_count_as_a_loop() {
        let mut run = RunLoop::default();
        for offset in [0, 100, 200, 300] {
            assert!(run.observe("Read", &read("a.rs", offset)).is_none());
        }
        run.observe("Read", &read("b.rs", 0));
        run.observe("Read", &read("b.rs", 0));
        run.observe("Edit", &json!({"file_path": "b.rs"}));
        assert!(run.observe("Read", &read("b.rs", 0)).is_none());
    }

    #[test]
    fn the_same_command_three_times_with_no_edit_fires_and_an_edit_resets_it() {
        let mut run = RunLoop::default();
        let test = json!({"command": "cargo test -p x"});
        run.observe("Bash", &test);
        run.observe("Bash", &test);
        let found = run.observe("Bash", &test).expect("third run");
        assert_eq!((found.rule, found.count), ("D7", 3));

        let mut run = RunLoop::default();
        run.observe("Bash", &test);
        run.observe("Bash", &test);
        run.observe("Write", &json!({"file_path": "a.rs"}));
        assert!(run.observe("Bash", &test).is_none());
    }

    #[test]
    fn a_sleep_and_other_tools_are_not_counted() {
        let mut run = RunLoop::default();
        let sleep = json!({"command": "sleep 5"});
        for _ in 0..5 {
            assert!(run.observe("Bash", &sleep).is_none());
            assert!(run.observe("Grep", &json!({"pattern": "x"})).is_none());
        }
    }
}
