//! Integration steps (spec 2026-11-09 passos de integracao §1-§2): the project's declared programs
//! run on the merged tree in the integration worktree, after the merge and before the gate, and what
//! they changed lands as one `integrate:` commit on top of the merge.
//!
//! Exclusivity comes from the queue: one operation per repository at a time, so nothing else moves
//! the integration worktree while a step runs. A step failing is terminal for the land — its reason
//! never says "moved while the merge was being computed", so the drain does not retry it.

use super::*;
use crate::config::{DEFAULT_STEP_TIMEOUT_SECS, IntegrationStep};

/// How much of a step's own output a report keeps. The tail, because a failing tool says why last.
const STEP_SUMMARY_BYTES: usize = 8 * 1024;

/// What one step did: the files it changed (repo-relative, renames as delete + add) and the end of
/// what it printed.
#[derive(Debug, Clone, PartialEq)]
pub struct StepReport {
    pub id: String,
    pub changed: Vec<String>,
    pub summary: String,
}

/// The outcome of running every step: the `integrate:` commit when any step changed a file, and one
/// report per step that ran, in declared order.
#[derive(Debug, Clone, PartialEq)]
pub struct StepsDone {
    pub commit: Option<String>,
    pub reports: Vec<StepReport>,
}

/// Where the steps run and what they are told about the merge they run on.
#[derive(Debug, Clone, Copy)]
pub struct StepRun<'a> {
    /// The integration worktree; its HEAD is `merge`.
    pub integration: &'a Path,
    /// The target before the merge (`Computed::old`).
    pub base: &'a str,
    /// The merge commit (`Computed::new`).
    pub merge: &'a str,
    pub source: &'a str,
    pub target: &'a str,
}

/// The active steps of `project_id`, in declared order. An unreadable document refuses, like
/// `gate_the_merge`: whether this land should be fixed first is unknown, so nothing is published.
pub fn integration_steps_of(project_id: &str) -> Result<Vec<IntegrationStep>, Outcome> {
    match crate::project::rules::autopilot(project_id) {
        Ok(rules) => Ok(rules
            .active_integration_steps()
            .into_iter()
            .cloned()
            .collect()),
        Err(error) => Err(Outcome::Failed {
            reason: format!(
                "the stored rules of this project could not be read, so which integration steps \
                 this land needs is unknown and nothing was published: {error}"
            ),
            exit_code: None,
            output_tail: String::new(),
        }),
    }
}

/// Runs `steps` in `run.integration` (HEAD == `run.merge`), then commits what they changed.
///
/// Each step's files are the difference between the tree staged before it and after it, so two
/// steps touching different files are reported apart even though they share one commit. A step
/// that moves HEAD (commits, resets, checks out) is refused: the queue owns every commit here.
/// `git_timeout` is the budget of each git command this function runs, counted fresh after each
/// step, because a step may legitimately take far longer than git ever should.
pub async fn run_integration_steps(
    run: &StepRun<'_>,
    steps: &[IntegrationStep],
    git_timeout: Duration,
) -> Result<StepsDone, Outcome> {
    let fresh = || std::time::Instant::now() + git_timeout;
    let first_tree = revision(run.integration, "HEAD^{tree}", fresh()).await?;
    let mut previous_tree = first_tree.clone();
    let mut reports = Vec::with_capacity(steps.len());

    for step in steps {
        let summary = run_one(run, step, git_timeout).await?;

        let deadline = fresh();
        let head = revision(run.integration, "HEAD", deadline).await?;
        if head != run.merge {
            return Err(Outcome::Failed {
                reason: format!(
                    "integration step `{}` moved HEAD (steps must not commit); nothing was \
                     published",
                    step.id
                ),
                exit_code: None,
                output_tail: summary,
            });
        }

        let staged = git(run.integration, &["add", "-A"], deadline).await?;
        if !staged.succeeded() {
            return Err(failed(
                format!(
                    "could not stage what integration step `{}` changed; nothing was published",
                    step.id
                ),
                &staged,
            ));
        }
        let tree = write_tree(run.integration, deadline).await?;
        let changed = changed_between(run.integration, &previous_tree, &tree, deadline).await?;
        previous_tree = tree;
        reports.push(StepReport {
            id: step.id.clone(),
            changed,
            summary,
        });
    }

    if previous_tree == first_tree {
        return Ok(StepsDone {
            commit: None,
            reports,
        });
    }

    let deadline = fresh();
    let (subject, body) = commit_message(run.source, &reports);
    let committed = git(
        run.integration,
        &["commit", "--no-verify", "-m", &subject, "-m", &body],
        deadline,
    )
    .await?;
    if !committed.succeeded() {
        return Err(failed(
            "could not commit what the integration steps changed; nothing was published".to_owned(),
            &committed,
        ));
    }
    let commit = revision(run.integration, "HEAD", deadline).await?;
    Ok(StepsDone {
        commit: Some(commit),
        reports,
    })
}

/// Runs one step and returns its output tail, or the terminal outcome when it did not succeed.
async fn run_one(
    run: &StepRun<'_>,
    step: &IntegrationStep,
    git_timeout: Duration,
) -> Result<String, Outcome> {
    if let Some(name) = step.kind.strip_prefix("builtin:") {
        return run_builtin(name, run, &step.config, git_timeout)
            .await
            .map_err(|reason| Outcome::Failed {
                reason: format!(
                    "integration step `{}` failed ({reason}); nothing was published",
                    step.id
                ),
                exit_code: None,
                output_tail: String::new(),
            });
    }

    let Some(program) = step.program.as_deref() else {
        return Err(Outcome::Failed {
            reason: format!(
                "integration step `{}` names no program; nothing was published",
                step.id
            ),
            exit_code: None,
            output_tail: String::new(),
        });
    };
    // A path runs from the merged tree, so the step that lands is the one the merge carries; a bare
    // name is looked up on PATH.
    let program = if program.contains('/') || program.contains('\\') {
        run.integration.join(program).to_string_lossy().into_owned()
    } else {
        program.to_owned()
    };
    let mut argv = Vec::with_capacity(step.args.len() + 1);
    argv.push(program);
    argv.extend(step.args.iter().cloned());

    let env = step_env(run, step);
    let timeout = Duration::from_secs(step.timeout_secs.unwrap_or(DEFAULT_STEP_TIMEOUT_SECS));
    let ran = crate::gate::run_argv(&argv, run.integration, &env, timeout).await;
    let summary = last_bytes(&ran.tail, STEP_SUMMARY_BYTES);
    let why = if ran.timed_out {
        Some("timed out".to_owned())
    } else if let Some(error) = ran.error {
        Some(error)
    } else {
        match ran.exit_code {
            Some(0) => None,
            Some(code) => Some(format!("exit {code}")),
            None => Some("no exit code".to_owned()),
        }
    };
    match why {
        None => Ok(summary),
        Some(why) => Err(Outcome::Failed {
            reason: format!(
                "integration step `{}` failed ({why}); nothing was published",
                step.id
            ),
            exit_code: ran.exit_code,
            output_tail: ran.tail,
        }),
    }
}

/// The factory steps. An unknown name never runs as a no-op.
async fn run_builtin(
    name: &str,
    run: &StepRun<'_>,
    config: &serde_json::Value,
    git_timeout: Duration,
) -> Result<String, String> {
    match name {
        "renumber-sequence" => super::step_renumber::run(run, config, git_timeout).await,
        _ => Err(format!("unknown builtin step `{name}`")),
    }
}

/// The environment contract of a step (plan 2026-11-09, decision 9).
fn step_env(run: &StepRun<'_>, step: &IntegrationStep) -> Vec<(String, String)> {
    let config = if step.config.is_null() {
        "{}".to_owned()
    } else {
        step.config.to_string()
    };
    let mut env = vec![
        ("NUCLEOS_STEP_ID".to_owned(), step.id.clone()),
        (
            "NUCLEOS_STEP_WORKTREE".to_owned(),
            run.integration.to_string_lossy().into_owned(),
        ),
        ("NUCLEOS_STEP_BASE".to_owned(), run.base.to_owned()),
        ("NUCLEOS_STEP_MERGE".to_owned(), run.merge.to_owned()),
        ("NUCLEOS_STEP_SOURCE".to_owned(), run.source.to_owned()),
        ("NUCLEOS_STEP_TARGET".to_owned(), run.target.to_owned()),
        ("NUCLEOS_STEP_CONFIG".to_owned(), config),
    ];
    if let Some(file) = step.config_file.as_deref() {
        env.push((
            "NUCLEOS_STEP_CONFIG_FILE".to_owned(),
            run.integration.join(file).to_string_lossy().into_owned(),
        ));
    }
    env
}

/// `integrate: <ids that changed something> for <source>`, and one body line per such step.
fn commit_message(source: &str, reports: &[StepReport]) -> (String, String) {
    let changing: Vec<&StepReport> = reports.iter().filter(|r| !r.changed.is_empty()).collect();
    let ids: Vec<&str> = changing.iter().map(|r| r.id.as_str()).collect();
    let subject = format!("integrate: {} for {source}", ids.join(", "));
    let body = changing
        .iter()
        .map(|r| format!("{}: {}", r.id, r.changed.join(", ")))
        .collect::<Vec<_>>()
        .join("\n");
    (subject, body)
}

async fn write_tree(repo: &Path, deadline: std::time::Instant) -> Result<String, Outcome> {
    let written = git(repo, &["write-tree"], deadline).await?;
    if !written.succeeded() {
        return Err(failed(
            "could not record the tree the integration steps left; nothing was published"
                .to_owned(),
            &written,
        ));
    }
    Ok(written.stdout.trim().to_owned())
}

/// The paths that differ between two trees, renames spelled as a delete and an add.
async fn changed_between(
    repo: &Path,
    before: &str,
    after: &str,
    deadline: std::time::Instant,
) -> Result<Vec<String>, Outcome> {
    if before == after {
        return Ok(Vec::new());
    }
    let diff = git(
        repo,
        &["diff", "--name-only", "-z", "--no-renames", before, after],
        deadline,
    )
    .await?;
    if !diff.succeeded() {
        return Err(failed(
            "could not list what an integration step changed; nothing was published".to_owned(),
            &diff,
        ));
    }
    Ok(diff
        .stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// A merge that changes a file an active step reads: the step's script in the repo, or its
/// `config_file`. Such a land waits for the owner, approved against `token` the way a test-map
/// change is (`Outcome::AwaitingOwner::map_blob`), because the step would run the branch's own
/// version of itself on the merged tree.
#[derive(Debug, Clone, PartialEq)]
pub struct StepPin {
    /// The watched paths the merge changes, repo-relative with `/`, sorted.
    pub files: Vec<String>,
    /// `steps:` + the sha256 of every pinned path and the blob the merge gives it, so an approval
    /// stops matching as soon as any of those files changes again.
    pub token: String,
}

/// What `StepPin::token` hashes for a path the merge deletes.
const DELETED_STEP_INPUT: &str = "deleted";

/// Whether `computed` (the merge, before any `integrate:` commit) changes a file one of `steps`
/// depends on. `steps` is the active list (`AutopilotRules::active_integration_steps`). A program
/// named bare is looked up on PATH, outside the repo, so it is not watched.
pub async fn changed_step_inputs(
    project_root: &Path,
    steps: &[IntegrationStep],
    computed: &Computed,
    deadline: std::time::Instant,
) -> Result<Option<StepPin>, Outcome> {
    let mut watched: Vec<String> = Vec::new();
    for step in steps {
        if let Some(program) = step.program.as_deref()
            && (program.contains('/') || program.contains('\\'))
        {
            watched.push(normalized_repo_path(program));
        }
        if let Some(file) = step.config_file.as_deref() {
            watched.push(normalized_repo_path(file));
        }
        // An interpreter's script: the first non-flag argument, when it names a path. Later
        // arguments are data (`git mv a.txt b.txt` does not run `a.txt`).
        if let Some(arg) = step.args.iter().find(|arg| !arg.starts_with('-'))
            && names_a_path(arg)
        {
            watched.push(normalized_repo_path(arg));
        }
    }
    if watched.is_empty() || computed.old == computed.new {
        return Ok(None);
    }
    let watched: Vec<String> = watched.iter().map(|path| path_key(path)).collect();

    let changed = changed_between(project_root, &computed.old, &computed.new, deadline).await?;
    let mut files: Vec<String> = changed
        .into_iter()
        .filter(|path| watched.contains(&path_key(path)))
        .collect();
    files.sort();
    files.dedup();
    if files.is_empty() {
        return Ok(None);
    }

    let mut hashed = String::new();
    for path in &files {
        let spec = format!("{}:{path}", computed.new);
        let blob = git(
            project_root,
            &["rev-parse", "--verify", "--quiet", &spec],
            deadline,
        )
        .await?;
        let blob = if blob.succeeded() {
            blob.stdout.trim().to_owned()
        } else {
            DELETED_STEP_INPUT.to_owned()
        };
        hashed.push_str(path);
        hashed.push('\0');
        hashed.push_str(&blob);
        hashed.push('\n');
    }
    let token = format!(
        "steps:{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(hashed.as_bytes())
    );
    Ok(Some(StepPin { files, token }))
}

/// Whether an argument looks like a file path: it has a separator, or ends in a short extension.
fn names_a_path(arg: &str) -> bool {
    if arg.contains('/') || arg.contains('\\') {
        return true;
    }
    arg.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (1..=5).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

/// The form two paths are compared in: case-folded on Windows, where the file system is.
fn path_key(path: &str) -> String {
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_owned()
    }
}

/// A declared repo path as `git diff` spells it: `/` separators, no leading `./`.
fn normalized_repo_path(path: &str) -> String {
    let mut path = path.replace('\\', "/");
    while let Some(rest) = path.strip_prefix("./") {
        path = rest.to_owned();
    }
    path
}

/// The last `limit` bytes of `text`, cut on a character boundary.
fn last_bytes(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut start = text.len() - limit;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

#[cfg(test)]
mod tests {
    // Held across awaits on purpose, as in `git_exec::tests`: it serialises the shared
    // NUCLEOS_WORKTREE_ROOT override.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use crate::execution::git_exec::testkit::{repo_with_a_branch_to_merge, space_free_tempdir};
    use crate::execution::git_exec::tests::{WorktreeRootEnv, sha_of};
    use std::process::Command;

    const GIT_TIMEOUT: Duration = Duration::from_secs(60);

    /// A `program: git` step: no scripts, so it runs the same on every OS.
    fn git_step(id: &str, args: &[&str]) -> IntegrationStep {
        IntegrationStep {
            id: id.to_owned(),
            kind: "program".to_owned(),
            program: Some("git".to_owned()),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            config: serde_json::Value::Null,
            config_file: None,
            timeout_secs: Some(60),
            enabled: true,
        }
    }

    /// A step that runs `script` through a git shell alias, from the worktree's top level.
    fn shell_step(id: &str, script: &str) -> IntegrationStep {
        git_step(id, &["-c", &format!("alias.x=!{script}"), "x"])
    }

    fn git_out(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git should start");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8(output.stdout)
            .expect("utf-8")
            .trim()
            .to_owned()
    }

    /// Runs `steps` on a real merge of `feat/x` into `master`; returns the integration worktree,
    /// the merge commit and what the steps did.
    async fn on_a_merge(
        prefix: &str,
        steps: &[IntegrationStep],
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        std::path::PathBuf,
        Computed,
        Result<StepsDone, Outcome>,
    ) {
        let (container, repo) = repo_with_a_branch_to_merge(prefix);
        let roots = space_free_tempdir("nucleos-steps-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        let deadline = std::time::Instant::now() + GIT_TIMEOUT;
        let computed = compute_merge(&repo, "feat/x", "master", deadline)
            .await
            .expect("the merge computes");
        let integration = integration_worktree(&repo);
        let run = StepRun {
            integration: &integration,
            base: &computed.old,
            merge: &computed.new,
            source: "feat/x",
            target: "master",
        };
        let done = run_integration_steps(&run, steps, GIT_TIMEOUT).await;
        (container, roots, integration, computed, done)
    }

    #[tokio::test]
    async fn a_step_that_changes_files_makes_one_integrate_commit_on_top_of_the_merge() {
        let _lock = crate::execution::worktree::test_env_lock();
        let steps = [git_step("move", &["mv", "feature.txt", "moved.txt"])];
        let (_c, _r, integration, computed, done) =
            on_a_merge("nucleos-steps-commit-", &steps).await;
        let done = done.expect("the step succeeds");

        let commit = done
            .commit
            .expect("a step that changed files makes a commit");
        assert_eq!(sha_of(&integration, "HEAD"), commit);
        assert_eq!(
            sha_of(&integration, "HEAD^"),
            computed.new,
            "parent is the merge"
        );
        assert_eq!(
            git_out(
                &integration,
                &["rev-list", "--count", &format!("{}..HEAD", computed.new)]
            ),
            "1"
        );
        let subject = git_out(&integration, &["log", "-1", "--format=%s"]);
        assert_eq!(subject, "integrate: move for feat/x");
        assert!(integration.join("moved.txt").is_file());
        assert!(!integration.join("feature.txt").exists());
    }

    #[tokio::test]
    async fn a_step_that_changes_nothing_makes_no_commit() {
        let _lock = crate::execution::worktree::test_env_lock();
        let steps = [git_step("noop", &["--version"])];
        let (_c, _r, integration, computed, done) = on_a_merge("nucleos-steps-noop-", &steps).await;
        let done = done.expect("the step succeeds");

        assert_eq!(done.commit, None);
        assert_eq!(sha_of(&integration, "HEAD"), computed.new);
        assert_eq!(done.reports.len(), 1);
        assert_eq!(done.reports[0].id, "noop");
        assert!(done.reports[0].changed.is_empty());
        assert!(
            done.reports[0].summary.contains("git version"),
            "{}",
            done.reports[0].summary
        );
    }

    #[tokio::test]
    async fn a_failing_step_fails_the_land_and_names_the_step() {
        let _lock = crate::execution::worktree::test_env_lock();
        let steps = [git_step("broken", &["rev-parse", "--verify", "nao-existe"])];
        let (_c, _r, integration, computed, done) = on_a_merge("nucleos-steps-fail-", &steps).await;

        match done {
            Err(Outcome::Failed {
                reason, exit_code, ..
            }) => {
                assert!(reason.contains("`broken`"), "{reason}");
                assert!(reason.contains("nothing was published"), "{reason}");
                assert!(
                    !reason.contains("moved while the merge was being computed"),
                    "a failing step must stay terminal: {reason}"
                );
                assert!(exit_code.is_some_and(|code| code != 0), "{exit_code:?}");
            }
            other => panic!("a failing step must fail the land, got {other:?}"),
        }
        assert_eq!(sha_of(&integration, "HEAD"), computed.new);
    }

    #[tokio::test]
    async fn a_step_after_a_failing_one_never_runs() {
        let _lock = crate::execution::worktree::test_env_lock();
        let steps = [
            git_step("broken", &["rev-parse", "--verify", "nao-existe"]),
            shell_step("later", "printf ran > later.txt"),
        ];
        let (_c, _r, integration, _computed, done) =
            on_a_merge("nucleos-steps-after-fail-", &steps).await;

        match done {
            Err(Outcome::Failed { reason, .. }) => {
                assert!(reason.contains("`broken`"), "{reason}");
                assert!(!reason.contains("`later`"), "{reason}");
            }
            other => panic!("a failing step must fail the land, got {other:?}"),
        }
        assert!(
            !integration.join("later.txt").exists(),
            "the step after the failing one ran"
        );
    }

    #[tokio::test]
    async fn a_step_that_commits_is_refused() {
        let _lock = crate::execution::worktree::test_env_lock();
        let steps = [git_step(
            "sneaky",
            &["commit", "--allow-empty", "--no-verify", "-m", "sneaky"],
        )];
        let (_c, _r, _integration, _computed, done) =
            on_a_merge("nucleos-steps-commits-", &steps).await;

        match done {
            Err(Outcome::Failed { reason, .. }) => {
                assert!(reason.contains("`sneaky`"), "{reason}");
                assert!(reason.contains("moved HEAD"), "{reason}");
            }
            other => panic!("a step that commits must be refused, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_step_sees_base_merge_source_and_target_in_its_environment() {
        let _lock = crate::execution::worktree::test_env_lock();
        let steps = [shell_step(
            "env",
            "printf '%s %s %s %s' \"$NUCLEOS_STEP_BASE\" \"$NUCLEOS_STEP_MERGE\" \
             \"$NUCLEOS_STEP_SOURCE\" \"$NUCLEOS_STEP_TARGET\" > env.txt",
        )];
        let (_c, _r, integration, computed, done) = on_a_merge("nucleos-steps-env-", &steps).await;
        let done = done.expect("the step succeeds");

        let seen = std::fs::read_to_string(integration.join("env.txt")).expect("env.txt written");
        assert_eq!(
            seen,
            format!("{} {} feat/x master", computed.old, computed.new)
        );
        assert_eq!(done.reports[0].changed, vec!["env.txt".to_owned()]);
    }

    #[tokio::test]
    async fn two_steps_share_one_commit_and_each_reports_its_own_files() {
        let _lock = crate::execution::worktree::test_env_lock();
        let steps = [
            git_step("move", &["mv", "feature.txt", "moved.txt"]),
            shell_step("write", "printf x > other.txt"),
        ];
        let (_c, _r, integration, computed, done) = on_a_merge("nucleos-steps-two-", &steps).await;
        let done = done.expect("both steps succeed");

        assert_eq!(done.reports.len(), 2);
        assert_eq!(done.reports[0].id, "move");
        assert_eq!(
            done.reports[0].changed,
            vec!["feature.txt".to_owned(), "moved.txt".to_owned()]
        );
        assert_eq!(done.reports[1].id, "write");
        assert_eq!(done.reports[1].changed, vec!["other.txt".to_owned()]);

        let commit = done.commit.expect("one commit for both");
        assert_eq!(sha_of(&integration, "HEAD"), commit);
        assert_eq!(sha_of(&integration, "HEAD^"), computed.new);
        assert_eq!(
            git_out(
                &integration,
                &["rev-list", "--count", &format!("{}..HEAD", computed.new)]
            ),
            "1"
        );
        assert_eq!(
            git_out(&integration, &["log", "-1", "--format=%s"]),
            "integrate: move, write for feat/x"
        );
        let body = git_out(&integration, &["log", "-1", "--format=%b"]);
        assert!(body.contains("move: feature.txt, moved.txt"), "{body}");
        assert!(body.contains("write: other.txt"), "{body}");
    }

    /// A `program` step that runs `program` with `config_file`, for the pin tests (nothing runs).
    fn pinned_step(id: &str, program: &str, config_file: Option<&str>) -> IntegrationStep {
        IntegrationStep {
            id: id.to_owned(),
            kind: "program".to_owned(),
            program: Some(program.to_owned()),
            args: Vec::new(),
            config: serde_json::Value::Null,
            config_file: config_file.map(str::to_owned),
            timeout_secs: Some(60),
            enabled: true,
        }
    }

    /// Commits `files` (path, contents) on `feat/x`, computes its merge into `master` and asks
    /// which step inputs it changes; returns the repo, the merge and the answer.
    async fn pin_of(
        prefix: &str,
        files: &[(&str, &str)],
        steps: &[IntegrationStep],
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        std::path::PathBuf,
        Computed,
        Result<Option<StepPin>, Outcome>,
    ) {
        let (container, repo) = repo_with_a_branch_to_merge(prefix);
        if !files.is_empty() {
            git_out(&repo, &["checkout", "feat/x"]);
            for (path, contents) in files {
                let target = repo.join(path);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).expect("mkdir");
                }
                std::fs::write(&target, contents).expect("write");
            }
            git_out(&repo, &["add", "-A"]);
            git_out(&repo, &["commit", "--no-verify", "-m", "step inputs"]);
            git_out(&repo, &["checkout", "master"]);
        }
        let roots = space_free_tempdir("nucleos-steps-pin-wt-");
        let _env = WorktreeRootEnv::set(roots.path());
        let deadline = std::time::Instant::now() + GIT_TIMEOUT;
        let computed = compute_merge(&repo, "feat/x", "master", deadline)
            .await
            .expect("the merge computes");
        let pin = changed_step_inputs(&repo, steps, &computed, deadline).await;
        (container, roots, repo, computed, pin)
    }

    fn expected_token(entries: &[(&str, &str)]) -> String {
        let mut hashed = String::new();
        for (path, blob) in entries {
            hashed.push_str(&format!("{path}\0{blob}\n"));
        }
        format!(
            "steps:{:x}",
            <sha2::Sha256 as sha2::Digest>::digest(hashed.as_bytes())
        )
    }

    #[tokio::test]
    async fn a_branch_that_changes_a_step_script_is_pinned_with_a_content_token() {
        let _lock = crate::execution::worktree::test_env_lock();
        let steps = [pinned_step("fmt", "./scripts/step.sh", None)];
        let (_c, _r, repo, computed, pin) = pin_of(
            "nucleos-steps-pin-script-",
            &[("scripts/step.sh", "#!/bin/sh\necho one\n")],
            &steps,
        )
        .await;
        let pin = pin
            .expect("the guard answers")
            .expect("a changed step script pins the land");

        assert_eq!(pin.files, vec!["scripts/step.sh".to_owned()]);
        let blob = git_out(
            &repo,
            &["rev-parse", &format!("{}:scripts/step.sh", computed.new)],
        );
        assert_eq!(
            pin.token,
            expected_token(&[("scripts/step.sh", blob.as_str())])
        );
        assert!(pin.token.starts_with("steps:"), "{}", pin.token);
    }

    #[tokio::test]
    async fn a_branch_that_changes_a_step_config_file_is_pinned() {
        let _lock = crate::execution::worktree::test_env_lock();
        // A bare program is on PATH, so only the config file is watched.
        let steps = [pinned_step("fmt", "git", Some("cfg.json"))];
        let (_c, _r, repo, computed, pin) = pin_of(
            "nucleos-steps-pin-config-",
            &[("cfg.json", "{\"a\":1}\n")],
            &steps,
        )
        .await;
        let pin = pin
            .expect("the guard answers")
            .expect("a changed config file pins the land");

        assert_eq!(pin.files, vec!["cfg.json".to_owned()]);
        let blob = git_out(&repo, &["rev-parse", &format!("{}:cfg.json", computed.new)]);
        assert_eq!(pin.token, expected_token(&[("cfg.json", blob.as_str())]));
    }

    #[tokio::test]
    async fn a_branch_that_changes_a_script_named_in_the_args_is_pinned() {
        let _lock = crate::execution::worktree::test_env_lock();
        let mut step = pinned_step("fix", "python", None);
        step.args = vec![".\\scripts\\fix.py".to_owned(), "data.txt".to_owned()];
        let (_c, _r, _repo, _computed, pin) = pin_of(
            "nucleos-steps-pin-args-",
            &[("scripts/fix.py", "print(1)\n"), ("data.txt", "d\n")],
            &[step],
        )
        .await;
        let pin = pin
            .expect("the guard answers")
            .expect("a changed script in the args pins the land");

        assert_eq!(
            pin.files,
            vec!["scripts/fix.py".to_owned()],
            "only the script position is watched, not the data after it"
        );
    }

    #[tokio::test]
    async fn a_branch_that_touches_no_step_input_is_not_pinned() {
        let _lock = crate::execution::worktree::test_env_lock();
        // The branch only adds `feature.txt`; a file named like the bare program is not watched.
        let steps = [
            pinned_step("fmt", "scripts/step.sh", Some("cfg.json")),
            pinned_step("bare", "git", None),
        ];
        let (_c, _r, _repo, _computed, pin) = pin_of(
            "nucleos-steps-pin-none-",
            &[("git", "not the program\n")],
            &steps,
        )
        .await;

        assert_eq!(pin.expect("the guard answers"), None);
    }

    #[tokio::test]
    async fn the_token_changes_when_the_pinned_file_changes() {
        let _lock = crate::execution::worktree::test_env_lock();
        let steps = [pinned_step("fmt", "scripts/step.sh", None)];
        let (_c1, _r1, repo, computed, first) = pin_of(
            "nucleos-steps-pin-token-a-",
            &[("scripts/step.sh", "#!/bin/sh\necho one\n")],
            &steps,
        )
        .await;
        let first = first.expect("the guard answers").expect("pinned");
        let again = changed_step_inputs(
            &repo,
            &steps,
            &computed,
            std::time::Instant::now() + GIT_TIMEOUT,
        )
        .await
        .expect("the guard answers")
        .expect("pinned");
        assert_eq!(
            first.token, again.token,
            "the same content gives the same token"
        );

        let (_c2, _r2, _repo, _computed, second) = pin_of(
            "nucleos-steps-pin-token-b-",
            &[("scripts/step.sh", "#!/bin/sh\necho two\n")],
            &steps,
        )
        .await;
        let second = second.expect("the guard answers").expect("pinned");
        assert_eq!(first.files, second.files);
        assert_ne!(
            first.token, second.token,
            "other content must not reuse an approval"
        );
    }

    #[tokio::test]
    async fn a_disabled_step_pins_nothing() {
        let _lock = crate::execution::worktree::test_env_lock();
        let mut disabled = pinned_step("fmt", "scripts/step.sh", Some("cfg.json"));
        disabled.enabled = false;
        let rules = crate::config::AutopilotRules {
            integration_steps: vec![disabled],
            ..Default::default()
        };
        let active: Vec<IntegrationStep> = rules
            .active_integration_steps()
            .into_iter()
            .cloned()
            .collect();
        let (_c, _r, _repo, _computed, pin) = pin_of(
            "nucleos-steps-pin-disabled-",
            &[("scripts/step.sh", "#!/bin/sh\n"), ("cfg.json", "{}\n")],
            &active,
        )
        .await;

        assert_eq!(pin.expect("the guard answers"), None);
    }
}
