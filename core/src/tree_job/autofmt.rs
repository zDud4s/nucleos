//! The daemon formats a branch itself: a rustfmt-only gate failure is deterministic and fixable, so
//! a tree never stops (or spends a leaf fix) on it.
//!
//! Three entries in `driver.rs` use this module: `red` (the findings name only formatter steps),
//! `at_work` after a leaf ended `done` (Rust paths the branch owns, before the gate) and `at_work`
//! for a leaf that ended `blocked` asking only that the controller run the formatter. The formatter
//! is chosen by the step label the gate already prints (`core: fmt`, `shell/src-tauri: fmt`,
//! `sidecars/x: gofmt`). `cargo fmt` and `gofmt -w` compile nothing and take no target dir, so no
//! broker slot is needed. Runs are counted on the branch (`extra["autofmt_runs"]`, cap
//! [`AUTOFMT_CAP`]) so a formatter that never settles falls back to the leaf hint.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use super::model::Branch;

/// Daemon-side formatter runs allowed per branch; past it the leaf is asked, as before.
pub(super) const AUTOFMT_CAP: i64 = 3;
const RUNS_KEY: &str = "autofmt_runs";
const FMT_TIMEOUT: Duration = Duration::from_secs(300);
const COMMIT_MESSAGE: &str = "nucleos: autoformat";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tool {
    CargoFmt,
    GoFmt,
}

/// One formatter run: the tool and the directory (relative to the worktree, `""` = its root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Step {
    pub dir: String,
    pub tool: Tool,
}

/// The step labels a gate output names as failed: `FAIL <label>` lines and the two-space list
/// under `gates FAILED:`.
fn failed_labels(findings: &str) -> Vec<String> {
    let mut labels = Vec::new();
    let mut listing = false;
    for line in findings.lines() {
        let trimmed = line.trim();
        if let Some(label) = trimmed.strip_prefix("FAIL ") {
            labels.push(label.trim().to_owned());
        } else if trimmed == "gates FAILED:" {
            listing = true;
        } else if listing && line.starts_with("  ") && !line.starts_with("   ") {
            labels.push(trimmed.to_owned());
        } else if !line.starts_with(' ') {
            listing = false;
        }
    }
    labels.sort();
    labels.dedup();
    labels
}

fn tool_of(label: &str) -> Option<(&str, Tool)> {
    if let Some(prefix) = label.strip_suffix(": gofmt") {
        return Some((prefix, Tool::GoFmt));
    }
    label.strip_suffix(": fmt").map(|prefix| (prefix, Tool::CargoFmt))
}

/// True when the findings fail on formatter steps only: at least one, and nothing else (no other
/// failed step, compiler error, panic or failed test).
pub(super) fn fmt_only(findings: &str) -> bool {
    let labels = failed_labels(findings);
    if labels.is_empty() || labels.iter().any(|label| tool_of(label).is_none()) {
        return false;
    }
    !findings.lines().map(str::trim).any(|line| {
        line.starts_with("error")
            || line.starts_with("thread '")
            || line.contains("panicked at")
            || (line.starts_with("test ") && line.ends_with("FAILED"))
    })
}

/// The formatter steps the findings call for. A label's prefix is a directory when `dir_exists`
/// says so, else the gate group (`core`), which runs from the worktree root.
pub(super) fn steps_from_findings(findings: &str, dir_exists: impl Fn(&str) -> bool) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    for label in failed_labels(findings) {
        let Some((prefix, tool)) = tool_of(&label) else {
            continue;
        };
        let dir = if dir_exists(prefix) { prefix } else { "" };
        let step = Step {
            dir: dir.to_owned(),
            tool,
        };
        if !steps.contains(&step) {
            steps.push(step);
        }
    }
    steps
}

/// The `cargo fmt` steps for the Rust files a branch owns: one per nearest directory holding a
/// `Cargo.toml` (`has_manifest`), looked up from each owned path up to the worktree root.
pub(super) fn steps_for_paths(paths: &[String], has_manifest: impl Fn(&str) -> bool) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    for path in paths {
        let path = path.trim().trim_end_matches("**").trim_end_matches('*');
        let path = path.trim_matches('/');
        let is_rust = path.ends_with(".rs") || path.ends_with("Cargo.toml");
        let mut dir = path;
        let mut found = None;
        loop {
            if has_manifest(dir) {
                found = Some(dir);
                break;
            }
            match dir.rfind('/') {
                Some(cut) => dir = &dir[..cut],
                None if dir.is_empty() => break,
                None => dir = "",
            }
        }
        let Some(dir) = found else { continue };
        if !is_rust && !has_manifest(dir) {
            continue;
        }
        let step = Step {
            dir: dir.to_owned(),
            tool: Tool::CargoFmt,
        };
        if !steps.contains(&step) {
            steps.push(step);
        }
    }
    steps
}

fn section<'a>(text: &'a str, title: &str) -> &'a str {
    let marker = format!("## {title}");
    let Some(start) = text.find(&marker) else {
        return "";
    };
    let rest = &text[start + marker.len()..];
    rest.find("\n## ").map_or(rest, |end| &rest[..end]).trim()
}

fn names_formatter(text: &str) -> bool {
    let text = text.to_lowercase();
    ["cargo fmt", "rustfmt", "gofmt", "formatter"]
        .iter()
        .any(|word| text.contains(word))
}

/// True when a `blocked` handoff asks for nothing but a formatter run: its blocked reason names a
/// formatter and every command it leaves to the root is a formatting one.
pub(super) fn blocked_asks_only_formatter(handoff: &str) -> bool {
    if !names_formatter(section(handoff, "Blocked reason")) {
        return false;
    }
    section(handoff, "Commands for the root")
        .lines()
        .map(|line| line.trim().trim_start_matches(['-', '*', ' ']).to_lowercase())
        .filter(|line| !line.is_empty() && line != "none")
        .all(|line| line.contains("fmt") || line.contains("format"))
}

/// Formatter runs already spent on the branch.
pub(super) fn runs(branch: &Branch) -> i64 {
    branch.extra.get(RUNS_KEY).and_then(Value::as_i64).unwrap_or(0)
}

pub(super) fn allowed(branch: &Branch) -> bool {
    runs(branch) < AUTOFMT_CAP
}

pub(super) fn count_run(branch: &mut Branch) {
    branch
        .extra
        .insert(RUNS_KEY.to_owned(), Value::from(runs(branch) + 1));
}

async fn run_step(worktree: &Path, step: &Step) -> Result<(), String> {
    let (program, args): (&str, &[&str]) = match step.tool {
        Tool::CargoFmt => ("cargo", &["fmt", "--all"]),
        Tool::GoFmt => ("gofmt", &["-w", "."]),
    };
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(worktree.join(&step.dir))
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(FMT_TIMEOUT, command.output())
        .await
        .map_err(|_| format!("{program} timed out"))?
        .map_err(|error| format!("{program}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

async fn dirty(worktree: &Path) -> Result<bool, String> {
    let output = tokio::process::Command::new("git")
        .args(["-c", "core.fsmonitor="])
        .arg("-C")
        .arg(worktree)
        .args(["status", "--porcelain"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("git status: {error}"))?;
    Ok(!output.stdout.is_empty())
}

/// Formats the worktree with `steps` and commits what changed. Answers whether anything changed.
/// Work the node left uncommitted is checkpointed first, so the formatter's diff is its own commit.
pub(super) async fn format_worktree(worktree: &Path, steps: &[Step]) -> Result<bool, String> {
    crate::execution::worktree::checkpoint(worktree)
        .await
        .map_err(|error| error.to_string())?;
    for step in steps {
        run_step(worktree, step).await?;
    }
    if !dirty(worktree).await? {
        return Ok(false);
    }
    crate::execution::worktree::checkpoint_with_message(worktree, COMMIT_MESSAGE)
        .await
        .map_err(|error| error.to_string())?;
    Ok(true)
}

/// The `dir_exists`/`has_manifest` probes over a real worktree.
pub(super) fn dir_probe(worktree: &Path) -> impl Fn(&str) -> bool + '_ {
    move |dir| !dir.is_empty() && worktree.join(dir).is_dir()
}

pub(super) fn manifest_probe(worktree: &Path) -> impl Fn(&str) -> bool + '_ {
    move |dir| worktree.join(dir).join("Cargo.toml").is_file()
}

/// Formats a branch for the formatter steps a red gate named. True when that produced a commit
/// (so a re-gate sees a new HEAD); false when nothing changed, the budget is spent or it failed:
/// the caller then falls back to asking the leaf.
pub(super) async fn heal_red(branch: &Branch, findings: &str) -> bool {
    if !fmt_only(findings) || !allowed(branch) {
        return false;
    }
    let Some(worktree) = branch.worktree.as_deref().map(Path::new) else {
        return false;
    };
    let steps = steps_from_findings(findings, dir_probe(worktree));
    matches!(format_worktree(worktree, &steps).await, Ok(true))
}

/// Formats the Rust paths a branch owns before its gate. Best effort: an error only means the gate
/// will say so. True when a commit was made.
pub(super) async fn format_owned(branch: &Branch) -> bool {
    let Some(worktree) = branch.worktree.as_deref().map(Path::new) else {
        return false;
    };
    if !allowed(branch) {
        return false;
    }
    let steps = steps_for_paths(&branch.paths, manifest_probe(worktree));
    if steps.is_empty() {
        return false;
    }
    matches!(format_worktree(worktree, &steps).await, Ok(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FMT_RED: &str = "gates FAILED:\n  core: fmt\n      Diff in x.rs:1";

    #[test]
    fn a_rustfmt_only_red_is_fmt_only() {
        assert!(fmt_only(FMT_RED));
        assert!(fmt_only("FAIL core: fmt\ngates FAILED:\n  core: fmt"));
    }

    #[test]
    fn a_red_with_another_failure_is_not_fmt_only() {
        assert!(!fmt_only("FAIL core: clippy\ngates FAILED:\n  core: clippy"));
        assert!(!fmt_only("FAIL core: fmt\nFAIL core: test\ngates FAILED:\n  core: fmt\n  core: test"));
        assert!(!fmt_only("gates FAILED:\n  core: fmt\nerror[E0432]: unresolved import"));
        assert!(!fmt_only("note: run fmt later"));
    }

    #[test]
    fn steps_follow_the_label_prefix() {
        let findings = "gates FAILED:\n  core: fmt\n  shell/src-tauri: fmt\n  sidecars/x: gofmt";
        let exists = |dir: &str| dir == "shell/src-tauri" || dir == "sidecars/x";
        let steps = steps_from_findings(findings, exists);
        assert_eq!(
            steps,
            vec![
                Step { dir: String::new(), tool: Tool::CargoFmt },
                Step { dir: "shell/src-tauri".into(), tool: Tool::CargoFmt },
                Step { dir: "sidecars/x".into(), tool: Tool::GoFmt },
            ]
        );
    }

    #[test]
    fn rust_paths_give_one_cargo_fmt_per_nearest_manifest() {
        let manifest = |dir: &str| matches!(dir, "" | "shell/src-tauri");
        let paths = vec![
            "core/src/tree_job/x.rs".to_owned(),
            "core/src/y.rs".to_owned(),
            "shell/src-tauri/src/main.rs".to_owned(),
        ];
        let steps = steps_for_paths(&paths, manifest);
        assert_eq!(steps.len(), 2, "{steps:?}");
        assert_eq!(steps[0].dir, "");
        assert_eq!(steps[1].dir, "shell/src-tauri");
    }

    #[test]
    fn non_rust_paths_give_no_step() {
        let manifest = |dir: &str| dir.is_empty();
        assert!(steps_for_paths(&["docs/a.md".to_owned()], manifest).is_empty());
        assert!(steps_for_paths(&["shell/src/app.ts".to_owned()], manifest).is_empty());
    }

    #[test]
    fn a_blocked_handoff_asking_only_for_the_formatter_is_recognised() {
        let handoff = "## Blocked reason\nI may not run cargo; the controller must run `cargo fmt -p nucleos-core`.\n\n## Commands for the root\n- cargo fmt -p nucleos-core\n\n## Open items\nnone";
        assert!(blocked_asks_only_formatter(handoff));
    }

    #[test]
    fn a_blocked_handoff_with_other_asks_is_not_a_formatter_ask() {
        let other = "## Blocked reason\nThe contract is missing a decision.\n\n## Commands for the root\n- none";
        assert!(!blocked_asks_only_formatter(other));
        let mixed = "## Blocked reason\nNeeds cargo fmt and a test run.\n\n## Commands for the root\n- cargo fmt -p nucleos-core\n- cargo test -p nucleos-core x";
        assert!(!blocked_asks_only_formatter(mixed));
    }

    #[test]
    fn the_run_cap_bounds_the_daemon_formatter() {
        let mut branch = Branch::new("b", &[], "code", None);
        assert!(allowed(&branch));
        for _ in 0..AUTOFMT_CAP {
            count_run(&mut branch);
        }
        assert!(!allowed(&branch));
    }
}
