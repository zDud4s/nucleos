//! Switching a project to the release-branch flow (spec 2026-11-11 D1-activation, D8): create the
//! integration branch from the tip of the lote branch, point the roster row and the git config at
//! it, and seed the postgate for the new target. Nothing here runs at startup; an owner command
//! calls it once, on a quiet project.

use std::path::Path;
use std::time::Instant;

use sqlx::SqlitePool;

use crate::execution::git_exec;

/// The branch the integration branch is born from. Hard-coded on purpose: the activation is a
/// one-off migration of the existing lote, not a general feature.
const LOTE_BRANCH: &str = "land/lote1-1111";

/// Activates the release flow for `project_id`: `release` stays the promoted branch and
/// `integration` becomes the landing target. Every precondition is measured before anything is
/// written, so a refusal leaves the repository and the database untouched.
pub async fn activate_release(
    pool: &SqlitePool,
    project_id: &str,
    root: &Path,
    release: &str,
    integration: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + git_exec::OPERATION_TIMEOUT;

    // The rules must name this very branch as the release branch; anything else is refused before
    // a single write.
    let rules = crate::project::rules::autopilot(project_id).map_err(|e| e.to_string())?;
    match rules.release_branch() {
        None => {
            return Err(format!(
                "`release_branch` is not set in the rules of {project_id}; set it to `{release}` first"
            ));
        }
        Some(configured) if configured != release => {
            return Err(format!(
                "the rules of {project_id} name `{configured}` as the release branch, not `{release}`"
            ));
        }
        Some(_) => {}
    }

    // Quiet project: no queued, running or gate-waiting request, no live run worktree.
    let busy_requests: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM vcs_requests
         WHERE project_id = ? AND status IN ('queued', 'running', 'awaiting_gate')",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    if busy_requests > 0 {
        return Err(format!(
            "{busy_requests} request(s) of {project_id} are still in flight; wait for the queue to drain"
        ));
    }
    let live_worktrees: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM worktrees WHERE project_id = ? AND removed_at IS NULL",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    if live_worktrees > 0 {
        return Err(format!(
            "{live_worktrees} run worktree(s) of {project_id} are still live; wait for the runs to finish"
        ));
    }

    // No postgate verify in flight and no red being handled.
    let gate_busy: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM postgate_state
         WHERE project_id = ? AND (running_sha IS NOT NULL OR red_phase IS NOT NULL)",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    if gate_busy > 0 {
        return Err(format!(
            "the postgate of {project_id} is running or handling a red; wait for it to settle"
        ));
    }

    // Git facts.
    let release_exists = git_exec::branch_exists(root, release, deadline).await?;
    let lote_exists = git_exec::branch_exists(root, LOTE_BRANCH, deadline).await?;
    if !lote_exists {
        return Err(format!("the lote branch `{LOTE_BRANCH}` does not exist"));
    }
    // An integration branch left by a partial earlier activation is accepted only when it still
    // sits exactly on the lote tip; at any other sha it is refused.
    let integration_exists = git_exec::branch_exists(root, integration, deadline).await?;
    if integration_exists {
        let integration_tip = branch_tip(root, integration, deadline).await?;
        let lote_tip = branch_tip(root, LOTE_BRANCH, deadline).await?;
        if integration_tip != lote_tip {
            return Err(format!(
                "integration branch `{integration}` already exists at another commit than `{LOTE_BRANCH}`"
            ));
        }
    }
    let release_is_ancestor =
        release_exists && git_exec::is_ancestor(root, release, LOTE_BRANCH, deadline).await?;
    // The integration branch is about to be born from the lote, so it counts as existing here.
    crate::config::release_activation_preconditions(
        &rules,
        integration,
        release_exists,
        lote_exists,
        release_is_ancestor,
    )?;

    // Seed facts, read before any write: the release tip, and whether it is known green.
    let spec = format!("refs/heads/{release}");
    let tip = git_exec::run_git(
        root,
        &[
            std::ffi::OsStr::new("rev-parse"),
            std::ffi::OsStr::new("--verify"),
            std::ffi::OsStr::new(&spec),
        ],
        deadline.saturating_duration_since(Instant::now()),
    )
    .await?;
    let release_tip = tip.stdout.trim().to_owned();
    if !tip.succeeded() || release_tip.is_empty() {
        return Err(format!("could not read the tip of `{release}`"));
    }
    let release_green = crate::verify::postgate::load(pool, project_id)
        .await
        .map_err(|e| e.to_string())?
        .is_some_and(|state| state.is_green(&release_tip));

    // Effects.
    if !integration_exists {
        let created = git_exec::run_git(
            root,
            &[
                std::ffi::OsStr::new("branch"),
                std::ffi::OsStr::new(integration),
                std::ffi::OsStr::new(LOTE_BRANCH),
            ],
            deadline.saturating_duration_since(Instant::now()),
        )
        .await?;
        if !created.succeeded() {
            return Err(format!(
                "could not create `{integration}` from `{LOTE_BRANCH}`: {}",
                created.output_tail.trim()
            ));
        }
    }
    sqlx::query("UPDATE autopilot_state SET integration_branch = ? WHERE project_id = ?")
        .bind(integration)
        .bind(project_id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    git_exec::set_integration_config(root, integration, deadline).await?;
    git_exec::set_release_config(root, release, deadline).await?;

    // Reset every per-target field of the row: nothing of the old target's history carries over.
    sqlx::query(
        "INSERT INTO postgate_state (project_id, target) VALUES (?, ?)
         ON CONFLICT(project_id) DO UPDATE SET
             target = excluded.target, last_green_sha = NULL, last_attempted_sha = NULL,
             covered_sha = NULL, running_sha = NULL, running_request_id = NULL,
             running_started_at = NULL, last_started_at = NULL,
             red_groups = '[]', red_since_sha = NULL,
             red_phase = NULL, red_sha = NULL, red_base_sha = NULL, probe_sha = NULL,
             probe_request_id = NULL, probes = '[]', reported_sha = NULL, culprit_sha = NULL,
             candidates = '[]', also_suspect = '[]',
             revert_merge_sha = NULL, revert_request_id = NULL, revert_sha = NULL,
             fix_branch = NULL, fix_run_id = NULL, held_note = NULL, unmeasured_runs = 0,
             updated_at = CURRENT_TIMESTAMP",
    )
    .bind(project_id)
    .bind(integration)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
    if release_green {
        // Only a release tip known green seeds the new target.
        crate::verify::postgate::mark_covered(pool, project_id, integration, &release_tip)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::query(
            "UPDATE postgate_state SET running_sha = NULL, running_request_id = NULL,
                 running_started_at = NULL WHERE project_id = ?",
        )
        .bind(project_id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The commit a local branch points at.
async fn branch_tip(root: &Path, branch: &str, deadline: Instant) -> Result<String, String> {
    let spec = format!("refs/heads/{branch}");
    let out = git_exec::run_git(
        root,
        &[
            std::ffi::OsStr::new("rev-parse"),
            std::ffi::OsStr::new("--verify"),
            std::ffi::OsStr::new(&spec),
        ],
        deadline.saturating_duration_since(Instant::now()),
    )
    .await?;
    let sha = out.stdout.trim().to_owned();
    if !out.succeeded() || sha.is_empty() {
        return Err(format!("could not read the tip of `{branch}`"));
    }
    Ok(sha)
}

#[cfg(test)]
mod tests {
    // The rules gate (`Home::new`) is a process-wide guard held across awaits on purpose, like the
    // land.rs tests that publish rules.
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const LOTE: &str = "land/lote1-1111";

    fn git_in(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start")
            .success()
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git should start");
        String::from_utf8(output.stdout)
            .expect("utf-8")
            .trim()
            .to_owned()
    }

    fn sha_of(repo: &Path, revision: &str) -> String {
        crate::execution::git_exec::tests::sha_of(repo, revision)
    }

    fn commit_file(repo: &Path, name: &str) {
        std::fs::write(repo.join(name), format!("{name}\n")).expect("write a file");
        assert!(git_in(repo, &["add", "-A"]));
        assert!(git_in(repo, &["commit", "-q", "-m", name]));
    }

    /// `master` (the seed commit) and `land/lote1-1111` one commit ahead of it, standing on
    /// `master`. With `master_moved`, `master` gets a commit the lote does not have, so master is
    /// no longer an ancestor of the lote.
    fn repo_with_lote(prefix: &str, master_moved: bool) -> (tempfile::TempDir, PathBuf) {
        let container = crate::execution::git_exec::testkit::space_free_tempdir(prefix);
        let repo = container.path().join("repo");
        crate::execution::git_exec::testkit::initialize_repo(&repo);
        assert!(git_in(&repo, &["branch", "-M", "master"]));
        assert!(git_in(&repo, &["checkout", "-q", "-b", LOTE]));
        commit_file(&repo, "lote.txt");
        assert!(git_in(&repo, &["checkout", "-q", "master"]));
        if master_moved {
            commit_file(&repo, "master-only.txt");
        }
        (container, repo)
    }

    async fn test_pool() -> sqlx::SqlitePool {
        // Same rules-gate dance as the land.rs tests: shared hold first, `Home::new` upgrades it.
        crate::project::rules::testing::hold_shared();
        crate::testdb::fresh_pool().await
    }

    async fn seed_project(pool: &sqlx::SqlitePool, project_id: &str, root: &Path) {
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root, integration_branch)
             VALUES (?, 'active', ?, 'master')",
        )
        .bind(project_id)
        .bind(root.to_string_lossy().into_owned())
        .execute(pool)
        .await
        .expect("seed the project's roster row");
    }

    /// Publishes the rules every activation precondition needs (gate on, a gate command, the release
    /// branch). The returned `Home` keeps the settings snapshot held for the test.
    fn publish_release_rules(project_id: &str) -> crate::project::rules::testing::Home {
        let home = crate::project::rules::testing::Home::new();
        crate::project::rules::testing::publish_rules(
            project_id,
            "gate_after_land: true\ngate_command: gate\nrelease_branch: master\n",
        );
        home
    }

    async fn queue_request(pool: &sqlx::SqlitePool, project_id: &str, root: &Path, status: &str) {
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, status, created_at)
             VALUES ('merge', '{}', ?, ?, 'daemon', ?, '2026-11-11 00:00:00')",
        )
        .bind(project_id)
        .bind(root.to_string_lossy().into_owned())
        .bind(status)
        .execute(pool)
        .await
        .expect("insert a vcs request");
    }

    async fn integration_column(pool: &sqlx::SqlitePool, project_id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT integration_branch FROM autopilot_state WHERE project_id = ?")
            .bind(project_id)
            .fetch_one(pool)
            .await
            .expect("read the roster row")
    }

    #[tokio::test]
    async fn activation_creates_integration_from_the_lote_tip() {
        let pool = test_pool().await;
        let (_keep, repo) = repo_with_lote("rb-p5-tip-", false);
        seed_project(&pool, "alpha", &repo).await;
        let _home = publish_release_rules("alpha");

        activate_release(&pool, "alpha", &repo, "master", "integration")
            .await
            .expect("every precondition holds");

        assert_eq!(sha_of(&repo, "integration"), sha_of(&repo, LOTE));
        assert_ne!(sha_of(&repo, "integration"), sha_of(&repo, "master"));
        // Activation creates the branch; it moves neither master nor the lote.
        assert!(git_in(
            &repo,
            &["merge-base", "--is-ancestor", "master", "integration"]
        ));
    }

    #[tokio::test]
    async fn activation_refuses_when_master_is_not_an_ancestor() {
        let pool = test_pool().await;
        let (_keep, repo) = repo_with_lote("rb-p5-anc-", true);
        seed_project(&pool, "alpha", &repo).await;
        let _home = publish_release_rules("alpha");
        let master_before = sha_of(&repo, "master");

        let err = activate_release(&pool, "alpha", &repo, "master", "integration")
            .await
            .unwrap_err()
            .to_string();

        assert!(err.contains("ancestor"), "{err}");
        // Nothing was changed on the way to the refusal.
        assert!(!git_in(
            &repo,
            &["show-ref", "--verify", "--quiet", "refs/heads/integration"]
        ));
        assert_eq!(sha_of(&repo, "master"), master_before);
        assert_eq!(
            integration_column(&pool, "alpha").await.as_deref(),
            Some("master")
        );
        assert_eq!(
            git_out(
                &repo,
                &["config", "--local", "--get", "nucleos.integrationBranch"]
            ),
            ""
        );
    }

    #[tokio::test]
    async fn activation_refuses_with_queued_requests() {
        for status in ["queued", "running", "awaiting_gate"] {
            let pool = test_pool().await;
            let (_keep, repo) = repo_with_lote("rb-p5-queue-", false);
            seed_project(&pool, "alpha", &repo).await;
            let _home = publish_release_rules("alpha");
            queue_request(&pool, "alpha", &repo, status).await;

            let result = activate_release(&pool, "alpha", &repo, "master", "integration").await;

            assert!(
                result.is_err(),
                "a {status} request must refuse the activation"
            );
            assert!(
                !git_in(
                    &repo,
                    &["show-ref", "--verify", "--quiet", "refs/heads/integration"]
                ),
                "no integration branch after a refusal ({status})"
            );
            assert_eq!(
                integration_column(&pool, "alpha").await.as_deref(),
                Some("master")
            );
        }
    }

    #[tokio::test]
    async fn activation_refuses_with_a_run_in_flight() {
        let pool = test_pool().await;
        let (_keep, repo) = repo_with_lote("rb-p5-run-", false);
        seed_project(&pool, "alpha", &repo).await;
        let _home = publish_release_rules("alpha");
        // A run whose worktree is still live (not removed) targets the project's current branch.
        sqlx::query(
            "INSERT INTO worktrees (owner_kind, owner_id, project_id, project_root, path, branch, created_at)
             VALUES ('run', 7, 'alpha', ?, ?, 'run/7', '2026-11-11 00:00:00')",
        )
        .bind(repo.to_string_lossy().into_owned())
        .bind(repo.join("wt7").to_string_lossy().into_owned())
        .execute(&pool)
        .await
        .expect("insert a live run worktree");

        let result = activate_release(&pool, "alpha", &repo, "master", "integration").await;

        assert!(
            result.is_err(),
            "a run in flight must refuse the activation"
        );
        assert!(!git_in(
            &repo,
            &["show-ref", "--verify", "--quiet", "refs/heads/integration"]
        ));
        assert_eq!(
            integration_column(&pool, "alpha").await.as_deref(),
            Some("master")
        );
    }

    #[tokio::test]
    async fn activation_sets_column_and_both_git_config_keys() {
        let pool = test_pool().await;
        let (_keep, repo) = repo_with_lote("rb-p5-cfg-", false);
        seed_project(&pool, "alpha", &repo).await;
        let _home = publish_release_rules("alpha");

        activate_release(&pool, "alpha", &repo, "master", "integration")
            .await
            .expect("every precondition holds");

        assert_eq!(
            integration_column(&pool, "alpha").await.as_deref(),
            Some("integration")
        );
        assert_eq!(
            git_out(
                &repo,
                &["config", "--local", "--get", "nucleos.integrationBranch"]
            ),
            "integration"
        );
        assert_eq!(
            git_out(
                &repo,
                &["config", "--local", "--get", "nucleos.releaseBranch"]
            ),
            "master"
        );
    }

    #[tokio::test]
    async fn activation_refuses_without_a_release_branch_rule() {
        let pool = test_pool().await;
        let (_keep, repo) = repo_with_lote("rb-p5-norule-", false);
        seed_project(&pool, "alpha", &repo).await;
        let _home = crate::project::rules::testing::Home::new();
        crate::project::rules::testing::publish_rules(
            "alpha",
            "gate_after_land: true\ngate_command: gate\n",
        );

        let err = activate_release(&pool, "alpha", &repo, "master", "integration")
            .await
            .unwrap_err();

        assert!(err.contains("release_branch"), "{err}");
        assert!(!git_in(
            &repo,
            &["show-ref", "--verify", "--quiet", "refs/heads/integration"]
        ));
        assert_eq!(
            integration_column(&pool, "alpha").await.as_deref(),
            Some("master")
        );
    }

    #[tokio::test]
    async fn activation_refuses_when_the_rule_names_another_branch() {
        let pool = test_pool().await;
        let (_keep, repo) = repo_with_lote("rb-p5-otherrule-", false);
        seed_project(&pool, "alpha", &repo).await;
        let _home = crate::project::rules::testing::Home::new();
        crate::project::rules::testing::publish_rules(
            "alpha",
            "gate_after_land: true\ngate_command: gate\nrelease_branch: main\n",
        );

        let err = activate_release(&pool, "alpha", &repo, "master", "integration")
            .await
            .unwrap_err();

        assert!(err.contains("main") && err.contains("master"), "{err}");
        assert!(!git_in(
            &repo,
            &["show-ref", "--verify", "--quiet", "refs/heads/integration"]
        ));
    }

    #[tokio::test]
    async fn activation_refuses_while_the_postgate_is_running() {
        for (column, seed) in [
            (
                "running_sha",
                "INSERT INTO postgate_state (project_id, target, running_sha) VALUES ('alpha', 'master', 'x')",
            ),
            (
                "red_phase",
                "INSERT INTO postgate_state (project_id, target, red_phase) VALUES ('alpha', 'master', 'x')",
            ),
        ] {
            let pool = test_pool().await;
            let (_keep, repo) = repo_with_lote("rb-p5-gate-", false);
            seed_project(&pool, "alpha", &repo).await;
            let _home = publish_release_rules("alpha");
            sqlx::query(seed)
                .execute(&pool)
                .await
                .expect("seed a busy postgate row");

            let result = activate_release(&pool, "alpha", &repo, "master", "integration").await;

            assert!(result.is_err(), "{column} set must refuse the activation");
            assert!(!git_in(
                &repo,
                &["show-ref", "--verify", "--quiet", "refs/heads/integration"]
            ));
            assert_eq!(
                integration_column(&pool, "alpha").await.as_deref(),
                Some("master")
            );
        }
    }

    #[tokio::test]
    async fn activation_clears_the_old_targets_postgate_history() {
        let pool = test_pool().await;
        let (_keep, repo) = repo_with_lote("rb-p5-reset-", false);
        seed_project(&pool, "alpha", &repo).await;
        let _home = publish_release_rules("alpha");
        sqlx::query(
            "INSERT INTO postgate_state
                 (project_id, target, red_sha, last_attempted_sha, covered_sha, last_green_sha)
             VALUES ('alpha', 'master', 'r1', 'a1', 'c1', 'g1')",
        )
        .execute(&pool)
        .await
        .expect("seed a master-era row");

        activate_release(&pool, "alpha", &repo, "master", "integration")
            .await
            .expect("every precondition holds");

        // One tuple per selected column: a named type would only restate the SELECT.
        #[allow(clippy::type_complexity)]
        let row: (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT target, red_sha, last_attempted_sha, covered_sha, last_green_sha,
                    running_sha, red_phase
             FROM postgate_state WHERE project_id = 'alpha'",
        )
        .fetch_one(&pool)
        .await
        .expect("read the postgate row");
        assert_eq!(row.0, "integration");
        assert_eq!(row.1, None);
        assert_eq!(row.2, None);
        assert_eq!(row.3, None);
        assert_eq!(row.4, None, "an unknown release tip is not seeded green");
        assert_eq!(row.5, None);
        assert_eq!(row.6, None);
    }

    #[tokio::test]
    async fn activation_resumes_when_integration_already_equals_the_lote_tip() {
        let pool = test_pool().await;
        let (_keep, repo) = repo_with_lote("rb-p5-resume-", false);
        seed_project(&pool, "alpha", &repo).await;
        let _home = publish_release_rules("alpha");
        assert!(git_in(&repo, &["branch", "integration", LOTE]));

        activate_release(&pool, "alpha", &repo, "master", "integration")
            .await
            .expect("an integration branch on the lote tip is accepted");

        assert_eq!(sha_of(&repo, "integration"), sha_of(&repo, LOTE));
        assert_eq!(
            integration_column(&pool, "alpha").await.as_deref(),
            Some("integration")
        );

        // At any other commit it is still refused.
        let pool = test_pool().await;
        let (_keep2, repo2) = repo_with_lote("rb-p5-resume2-", false);
        seed_project(&pool, "beta", &repo2).await;
        crate::project::rules::testing::publish_rules(
            "beta",
            "gate_after_land: true\ngate_command: gate\nrelease_branch: master\n",
        );
        assert!(git_in(&repo2, &["branch", "integration", "master"]));
        let result = activate_release(&pool, "beta", &repo2, "master", "integration").await;
        assert!(result.is_err());
        assert_eq!(
            integration_column(&pool, "beta").await.as_deref(),
            Some("master")
        );
    }

    #[tokio::test]
    async fn activation_seeds_postgate_state_for_the_new_target() {
        // Master known green: its tip becomes the seeded last_green_sha.
        let pool = test_pool().await;
        let (_keep, repo) = repo_with_lote("rb-p5-seed-", false);
        seed_project(&pool, "alpha", &repo).await;
        let _home = publish_release_rules("alpha");
        let master_tip = sha_of(&repo, "master");
        crate::verify::postgate::mark_covered(&pool, "alpha", "master", &master_tip)
            .await
            .unwrap();

        activate_release(&pool, "alpha", &repo, "master", "integration")
            .await
            .expect("every precondition holds");

        let (target, green): (String, Option<String>) = sqlx::query_as(
            "SELECT target, last_green_sha FROM postgate_state WHERE project_id = 'alpha'",
        )
        .fetch_one(&pool)
        .await
        .expect("a seeded postgate row");
        assert_eq!(target, "integration");
        assert_eq!(green.as_deref(), Some(master_tip.as_str()));

        // Master with no known green state: the row targets integration and nothing is seeded green,
        // so the first gate measures everything.
        let pool = test_pool().await;
        let (_keep2, repo2) = repo_with_lote("rb-p5-seed2-", false);
        seed_project(&pool, "beta", &repo2).await;
        crate::project::rules::testing::publish_rules(
            "beta",
            "gate_after_land: true\ngate_command: gate\nrelease_branch: master\n",
        );

        activate_release(&pool, "beta", &repo2, "master", "integration")
            .await
            .expect("every precondition holds");

        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT target, last_green_sha FROM postgate_state WHERE project_id = 'beta'",
        )
        .fetch_optional(&pool)
        .await
        .expect("read the postgate row");
        if let Some((target, green)) = row {
            assert_eq!(target, "integration");
            assert_eq!(green, None, "an unknown master must not be seeded green");
        }
    }
}
