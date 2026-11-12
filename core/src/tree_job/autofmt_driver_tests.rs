//! Test-only: the daemon-side formatter (`autofmt.rs`) driven through the real tree driver tick, over
//! a real git repository and worktree. These tests spawn a real `cargo fmt`, as the daemon does.

#![allow(clippy::await_holding_lock)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use super::autofmt::{AUTOFMT_CAP, Step, Tool, format_worktree};
use super::driver::GateVerdict;
use super::driver::tests::{FakeGate, Fixture, code_branch, commit_file, git_in, wait_calls};
use super::model::Branch;
use crate::execution::git_exec::testkit::{init_contained_repo, space_free_tempdir};

const JOB: &str = "drv-autofmt";
const NODE: &str = "api-node";
const SUBJECT: &str = "nucleos: autoformat";
const UNFORMATTED: &str = "pub fn add(a:i32,b:i32)->i32{a+b}\n";
const FORMATTED: &str = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
const FMT_RED: &str = "FAIL core: fmt\ngates FAILED:\n  core: fmt\n      Diff in src/lib.rs:1";
const MIXED_RED: &str = "FAIL core: fmt\nFAIL core: test\ngates FAILED:\n  core: fmt\n  core: test\n      Diff in x.rs:1";
const FMT_BLOCKED: &str = "## Blocked reason\nI may not run cargo; the controller must run \
                           `cargo fmt`.\n\n## Commands for the root\n- cargo fmt\n";
const KEY_BLOCKED: &str = "## Blocked reason\nI need the owner's API key.\n\n\
                           ## Commands for the root\n- none\n";

// ---- fixtures ----------------------------------------------------------------------------------

fn git_out(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git should start");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn commit_count(dir: &Path) -> usize {
    git_out(dir, &["rev-list", "--count", "HEAD"])
        .parse()
        .expect("a commit count")
}

fn autofmt_commits(dir: &Path) -> usize {
    git_out(dir, &["log", "--format=%s"])
        .lines()
        .filter(|subject| *subject == SUBJECT)
        .count()
}

/// A one-crate tree at `dir` (a git root) with `source` committed as `src/lib.rs`. `Cargo.lock` and
/// `target/` are ignored so `cargo fmt` leaves the tree clean, and the empty `[workspace]` keeps
/// cargo from looking for a parent workspace.
fn write_crate(dir: &Path, source: &str) {
    std::fs::create_dir_all(dir.join("src")).expect("create src");
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"fmtprobe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .expect("write the manifest");
    std::fs::write(dir.join(".gitignore"), "Cargo.lock\ntarget/\n").expect("write .gitignore");
    commit_file(dir, "src/lib.rs", source, "add the crate");
}

fn root_step() -> Vec<Step> {
    vec![Step {
        dir: String::new(),
        tool: Tool::CargoFmt,
    }]
}

/// A worktree of `tree/api` holding the one-crate tree with `source` committed.
fn crate_worktree(fx: &Fixture, source: &str) -> (tempfile::TempDir, PathBuf) {
    let holder = space_free_tempdir("nucleos-autofmt-wt-");
    let tree = holder.path().join("api");
    assert!(git_in(
        &fx.repo,
        &["worktree", "add", "-q", &tree.to_string_lossy(), "tree/api"]
    ));
    write_crate(&tree, source);
    (holder, tree)
}

/// `api` at work behind `api-node`, standing in `tree`, in `state`.
async fn seed(fx: &Fixture, tree: &Path, state: &str, extra: &[(&str, Value)], gates: Vec<Value>) {
    let mut branch: Branch = code_branch("api", state, Some(NODE));
    branch.worktree = Some(tree.to_string_lossy().into_owned());
    branch.gates = gates;
    for (key, value) in extra {
        branch.extra.insert((*key).to_owned(), value.clone());
    }
    fx.seed_job(JOB, vec![branch], None).await;
    fx.add_node(JOB, NODE, "orchestrator").await;
    fx.save_launch(NODE).await;
}

fn red_gate(findings: &str) -> Vec<Value> {
    vec![
        json!({"at": "2026-10-31T10:00:00+00:00", "verdict": "red", "exit": 1,
                "log_tail": findings, "failures": ""}),
    ]
}

fn autofmt_runs(branch: &Branch) -> Option<i64> {
    branch.extra.get("autofmt_runs").and_then(Value::as_i64)
}

/// Gives the node's ended run a handoff file with exactly `text`.
async fn handoff_is(fx: &Fixture, text: &str) {
    let path = fx
        .node(NODE)
        .await
        .handoff_path
        .expect("the run left a handoff file");
    std::fs::write(path, text).expect("write the handoff");
}

// ---- format_worktree ---------------------------------------------------------------------------

#[tokio::test]
async fn format_worktree_commits_a_format_once_and_then_changes_nothing() {
    let (_container, repo) = init_contained_repo("nucleos-autofmt-repo-");
    write_crate(&repo, UNFORMATTED);
    let before = commit_count(&repo);

    let first = format_worktree(&repo, &root_step()).await;

    assert_eq!(first, Ok(true));
    assert_eq!(commit_count(&repo), before + 1, "exactly one new commit");
    assert_eq!(git_out(&repo, &["log", "-1", "--format=%s"]), SUBJECT);
    assert_eq!(
        std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(),
        FORMATTED
    );

    let second = format_worktree(&repo, &root_step()).await;

    assert_eq!(second, Ok(false), "an already formatted tree is left alone");
    assert_eq!(commit_count(&repo), before + 1);
    assert_eq!(autofmt_commits(&repo), 1);
}

// ---- a red gate --------------------------------------------------------------------------------

#[tokio::test]
async fn a_fmt_only_red_is_formatted_by_the_daemon_without_spending_a_fix() {
    let fx = Fixture::new().await;
    let (_holder, tree) = crate_worktree(&fx, UNFORMATTED);
    seed(&fx, &tree, "red", &[], red_gate(FMT_RED)).await;
    let run = fx.run_ended(NODE, Some("done")).await;
    let gate = FakeGate::answering(vec![]);

    fx.tick(JOB, &gate).await;

    let api = fx.branch(JOB, "api").await;
    assert_eq!(api.state, "fixing");
    assert_eq!(fx.launches(NODE, "fix-").await, 0, "no leaf fix was spent");
    assert_eq!(fx.launches(NODE, "rescue-").await, 0);
    assert_eq!(fx.current_run(NODE).await, run, "no leaf run was started");
    assert_eq!(autofmt_runs(&api), Some(1));
    assert_eq!(autofmt_commits(&tree), 1);
    assert_eq!(
        std::fs::read_to_string(tree.join("src/lib.rs")).unwrap(),
        FORMATTED
    );
    assert!(gate.calls().is_empty());
}

#[tokio::test]
async fn a_fmt_only_red_past_the_autofmt_cap_falls_back_to_a_leaf_fix() {
    let fx = Fixture::new().await;
    let (_holder, tree) = crate_worktree(&fx, UNFORMATTED);
    let spent = [("autofmt_runs", json!(AUTOFMT_CAP))];
    seed(&fx, &tree, "red", &spent, red_gate(FMT_RED)).await;
    fx.run_ended(NODE, Some("done")).await;
    let before = git_out(&tree, &["rev-parse", "HEAD"]);
    let gate = FakeGate::answering(vec![]);

    fx.tick(JOB, &gate).await;

    let api = fx.branch(JOB, "api").await;
    assert_eq!(api.state, "fixing");
    assert_eq!(fx.launches(NODE, "fix-").await, 1, "the leaf was asked");
    assert_eq!(
        autofmt_runs(&api),
        Some(AUTOFMT_CAP),
        "the cap is not passed"
    );
    assert_eq!(git_out(&tree, &["rev-parse", "HEAD"]), before);
    assert_eq!(autofmt_commits(&tree), 0, "the daemon did not format");
    let prompt = fx.prompt_of(fx.current_run(NODE).await).await;
    assert!(prompt.contains("cargo fmt"), "{prompt:?}");
}

#[tokio::test]
async fn a_red_with_a_failing_test_beside_fmt_takes_the_leaf_fix_path() {
    let fx = Fixture::new().await;
    let (_holder, tree) = crate_worktree(&fx, UNFORMATTED);
    seed(&fx, &tree, "red", &[], red_gate(MIXED_RED)).await;
    fx.run_ended(NODE, Some("done")).await;
    let gate = FakeGate::answering(vec![]);

    fx.tick(JOB, &gate).await;

    let api = fx.branch(JOB, "api").await;
    assert_eq!(api.state, "fixing");
    assert_eq!(fx.launches(NODE, "fix-").await, 1);
    assert_eq!(
        autofmt_runs(&api),
        None,
        "the daemon formatter was not used"
    );
    assert_eq!(autofmt_commits(&tree), 0);
}

// ---- a leaf that ends blocked ------------------------------------------------------------------

#[tokio::test]
async fn a_leaf_blocked_only_on_cargo_fmt_is_formatted_and_goes_on_to_its_gate() {
    let fx = Fixture::new().await;
    let (_holder, tree) = crate_worktree(&fx, UNFORMATTED);
    seed(&fx, &tree, "building", &[], vec![]).await;
    fx.run_ended(NODE, Some("blocked")).await;
    handoff_is(&fx, FMT_BLOCKED).await;
    let gate = FakeGate::answering(vec![GateVerdict::Green]);

    fx.tick(JOB, &gate).await;

    wait_calls(&gate, 1).await;
    assert_eq!(gate.calls(), vec![(JOB.to_owned(), "api".to_owned())]);
    fx.wait_branch_state(JOB, "api", "green").await;
    assert!(
        fx.open_questions(JOB, NODE).await.is_empty(),
        "nobody was asked"
    );
    assert_eq!(
        fx.launches(NODE, "rescue-").await,
        0,
        "no rescue run either"
    );
    let api = fx.branch(JOB, "api").await;
    assert_eq!(autofmt_runs(&api), Some(1));
    assert_eq!(autofmt_commits(&tree), 1);
    assert_eq!(
        std::fs::read_to_string(tree.join("src/lib.rs")).unwrap(),
        FORMATTED
    );
}

#[tokio::test]
async fn a_leaf_blocked_for_another_reason_is_parked_as_before() {
    let fx = Fixture::new().await;
    let (_holder, tree) = crate_worktree(&fx, UNFORMATTED);
    seed(&fx, &tree, "building", &[], vec![]).await;
    // The one rescue is already spent, so the branch parks at once.
    fx.save_launch_keyed(NODE, "rescue-0").await;
    fx.run_ended(NODE, Some("blocked")).await;
    handoff_is(&fx, KEY_BLOCKED).await;
    let before = git_out(&tree, &["rev-parse", "HEAD"]);
    let gate = FakeGate::answering(vec![GateVerdict::Green]);

    fx.tick(JOB, &gate).await;

    assert_eq!(fx.branch(JOB, "api").await.state, "parked");
    assert_eq!(fx.open_questions(JOB, NODE).await.len(), 1);
    assert!(gate.calls().is_empty(), "a parked branch is not gated");
    assert_eq!(autofmt_runs(&fx.branch(JOB, "api").await), None);
    assert_eq!(git_out(&tree, &["rev-parse", "HEAD"]), before);
    assert_eq!(autofmt_commits(&tree), 0);
}
