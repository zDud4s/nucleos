//! The land gate of repeated migration numbers (spec 2026-11-06 tickets-globais). A branch that adds
//! `core/migrations/NNNN_x.sql` when the target already has a different file numbered NNNN is refused
//! with a `migration:<target>:<NNNN>` ticket, owned by whoever renumbers. Fail-open like `claim_gate`:
//! a git or database error lets the landing through.

use std::path::Path;
use std::time::Instant;

use chrono::{DateTime, Utc};

use super::claim_gate::{Landing, Verdict};
use crate::execution::git_exec;
use crate::execution::vcs::{Branch, Op, Origin};
use crate::task_claims::detect::colliding_numbers;
use crate::task_claims::{self, TaskKey};

const MIGRATIONS_DIR: &str = "core/migrations";

fn key_for(target: &str, number: &str) -> Option<TaskKey> {
    task_claims::keys::normalize(&format!("migration:{target}:{number}"), Path::new("")).ok()
}

/// Whether an active `renumber-sequence` step renumbers `dir`: the land fixes a colliding number
/// itself, so refusing it would only push the work back to the branch owner.
fn renumber_step_covers(rules: &crate::config::AutopilotRules, dir: &str) -> bool {
    rules.active_integration_steps().into_iter().any(|step| {
        step.kind == "builtin:renumber-sequence"
            && step
                .config
                .get("dir")
                .and_then(|value| value.as_str())
                .is_some_and(|configured| {
                    configured.replace('\\', "/").trim_end_matches('/') == dir
                })
    })
}

pub async fn check(
    pool: &sqlx::SqlitePool,
    landing: &Landing<'_>,
    deadline: Instant,
    now: DateTime<Utc>,
) -> Verdict {
    if landing.exempt {
        return Verdict::Clear;
    }
    // Unreadable rules take the normal path: the gate stays on.
    if let Ok(rules) = crate::project::rules::autopilot(landing.project_id)
        && renumber_step_covers(&rules, MIGRATIONS_DIR)
    {
        return Verdict::Clear;
    }
    let (source, target) = (landing.source.as_str(), landing.target.as_str());
    let listed = async {
        let theirs =
            git_exec::list_dir_names(landing.project_root, target, MIGRATIONS_DIR, deadline)
                .await?;
        let ours = git_exec::list_dir_names(landing.project_root, source, MIGRATIONS_DIR, deadline)
            .await?;
        Ok::<_, String>((theirs, ours))
    }
    .await;
    let (target_files, branch_files) = match listed {
        Ok(both) => both,
        Err(reason) => {
            tracing::warn!(project = landing.project_id, %reason, "land: migration gate cannot list the migrations");
            return Verdict::Clear;
        }
    };
    let collisions = colliding_numbers(&target_files, &branch_files);
    if collisions.is_empty() {
        close_resolved(pool, landing, now).await;
        return Verdict::Clear;
    }
    let worktree = git_exec::worktree_of_branch(landing.project_root, source, deadline)
        .await
        .ok()
        .flatten()
        .map(|p| p.to_string_lossy().into_owned());
    let mut parts = Vec::new();
    for collision in &collisions {
        let Some(key) = key_for(target, &collision.number) else {
            continue;
        };
        let branch_file = format!("{MIGRATIONS_DIR}/{}", collision.branch_file);
        let target_file = format!("{MIGRATIONS_DIR}/{}", collision.target_file);
        let opened = task_claims::machine::open_machine_ticket(
            pool,
            landing.project_id,
            key.clone(),
            format!("migration number {} is used by two files", collision.number),
            serde_json::json!({
                "branch": source,
                "branch_file": branch_file,
                "target_file": target_file,
                "run_id": landing.run_id,
                "worktree": worktree,
            }),
            vec![branch_file.clone(), target_file.clone()],
            now,
        )
        .await;
        let ticket = match opened {
            Ok(row) => row,
            Err(error) => {
                tracing::warn!(project = landing.project_id, %error, "land: migration gate cannot open its ticket");
                return Verdict::Clear;
            }
        };
        parts.push(format!(
            "branch {source} adds {branch_file} but {target} already has {} with the same number; ticket #{} `{}` is yours to renumber (the branch owner). Renumber to the next free number and land again.",
            collision.target_file, ticket.id, key
        ));
    }
    if parts.is_empty() {
        return Verdict::Clear;
    }
    Verdict::Refused(parts.join(" "))
}

/// The gate passed: any open `migration:<target>:*` ticket of this very branch is resolved.
async fn close_resolved(pool: &sqlx::SqlitePool, landing: &Landing<'_>, now: DateTime<Utc>) {
    let prefix = format!("migration:{}:", landing.target.as_str());
    let Ok(rows) = task_claims::store::list_live(pool, landing.project_id, now).await else {
        return;
    };
    for row in rows {
        let ours =
            row.detail.get("branch").and_then(|b| b.as_str()) == Some(landing.source.as_str());
        if row.key.as_str().starts_with(&prefix) && ours {
            let _ = task_claims::machine::close_machine_ticket(
                pool,
                landing.project_id,
                &row.key,
                "branch no longer collides",
                now,
            )
            .await;
        }
    }
}

/// The queue-time re-check, same shape and exemption rule as `claim_gate::refusal_for_op`.
#[allow(clippy::too_many_arguments)]
pub async fn refusal_for_op(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    project_root: &Path,
    op: &Op,
    from_resolution: bool,
    origin: Origin,
    run_id: Option<i64>,
    integration: Option<&Branch>,
) -> Option<String> {
    let Op::Merge { source, target } = op else {
        return None;
    };
    if integration.is_none_or(|branch| branch.as_str() != target.as_str()) {
        return None;
    }
    let landing = Landing {
        project_id,
        project_root,
        source,
        target,
        exempt: from_resolution || matches!(origin, Origin::Daemon),
        run_id,
    };
    let deadline = Instant::now() + git_exec::OPERATION_TIMEOUT;
    match check(pool, &landing, deadline, Utc::now()).await {
        Verdict::Refused(message) => {
            Some(format!("refused when the queue came to run it: {message}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};
    use chrono::TimeZone;

    const PROJECT: &str = "p";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 6, 10, 0, 0).unwrap()
    }

    fn git(repo: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Writes `core/migrations/<name>` and commits it on the current branch.
    fn add_migration(repo: &Path, name: &str) {
        let dir = repo.join(MIGRATIONS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), "-- sql\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-m", name]);
    }

    /// `master` with `0275_base.sql`, and back on `master`.
    fn repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let container = space_free_tempdir("migration-gate-");
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        git(&repo, &["branch", "-M", "master"]);
        add_migration(&repo, "0275_base.sql");
        (container, repo)
    }

    /// A branch off `master` adding one migration, then back on `master`.
    fn branch_adding(repo: &Path, branch: &str, file: &str) {
        git(repo, &["checkout", "-b", branch, "master"]);
        add_migration(repo, file);
        git(repo, &["checkout", "master"]);
    }

    async fn run_check(
        pool: &sqlx::SqlitePool,
        repo: &Path,
        source: &str,
        exempt: bool,
    ) -> Verdict {
        // `check` reads the project's rules: hold the shared side (a no-op after a `publish_rules`).
        crate::project::rules::testing::hold_shared();
        let (source, target) = (Branch::new(source).unwrap(), Branch::new("master").unwrap());
        let landing = Landing {
            project_id: PROJECT,
            project_root: repo,
            source: &source,
            target: &target,
            exempt,
            run_id: Some(7),
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        check(pool, &landing, deadline, now()).await
    }

    fn refusal(verdict: Verdict) -> String {
        match verdict {
            Verdict::Refused(message) => message,
            _ => panic!("expected a refusal"),
        }
    }

    async fn count(pool: &sqlx::SqlitePool, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
    }

    #[tokio::test]
    async fn a_branch_with_a_migration_number_the_target_already_has_is_refused_with_the_ticket() {
        let (_keep, repo) = repo();
        add_migration(&repo, "0276_target.sql");
        branch_adding(&repo, "feat/a", "0276_branch.sql");
        let pool = crate::testdb::fresh_pool().await;
        let message = refusal(run_check(&pool, &repo, "feat/a", false).await);
        assert!(message.contains("migration:master:0276"), "{message}");
        let open = count(
            &pool,
            "SELECT COUNT(*) FROM task_claims WHERE task_key = 'migration:master:0276' AND status = 'open'",
        )
        .await;
        assert_eq!(open, 1);
    }

    #[tokio::test]
    async fn the_message_names_the_ticket_key_and_both_files() {
        let (_keep, repo) = repo();
        add_migration(&repo, "0276_target.sql");
        branch_adding(&repo, "feat/a", "0276_branch.sql");
        let pool = crate::testdb::fresh_pool().await;
        let message = refusal(run_check(&pool, &repo, "feat/a", false).await);
        assert!(
            message.contains("core/migrations/0276_branch.sql"),
            "{message}"
        );
        assert!(message.contains("0276_target.sql"), "{message}");
        assert!(message.contains("renumber"), "{message}");
        assert!(message.contains('#'), "{message}");
    }

    #[tokio::test]
    async fn a_second_refusal_for_the_same_number_adds_a_report_not_a_ticket() {
        let (_keep, repo) = repo();
        add_migration(&repo, "0276_target.sql");
        branch_adding(&repo, "feat/a", "0276_branch.sql");
        let pool = crate::testdb::fresh_pool().await;
        refusal(run_check(&pool, &repo, "feat/a", false).await);
        refusal(run_check(&pool, &repo, "feat/a", false).await);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM task_claims").await, 1);
        assert_eq!(
            count(&pool, "SELECT COUNT(*) FROM task_claim_reports").await,
            2
        );
    }

    #[tokio::test]
    async fn a_branch_renumbered_after_the_ticket_lets_the_land_through_and_closes_it() {
        let (_keep, repo) = repo();
        add_migration(&repo, "0276_target.sql");
        branch_adding(&repo, "feat/a", "0276_branch.sql");
        let pool = crate::testdb::fresh_pool().await;
        refusal(run_check(&pool, &repo, "feat/a", false).await);
        git(&repo, &["checkout", "feat/a"]);
        git(
            &repo,
            &[
                "mv",
                "core/migrations/0276_branch.sql",
                "core/migrations/0277_branch.sql",
            ],
        );
        git(&repo, &["commit", "-m", "renumber"]);
        git(&repo, &["checkout", "master"]);
        assert!(matches!(
            run_check(&pool, &repo, "feat/a", false).await,
            Verdict::Clear
        ));
        let done = count(
            &pool,
            "SELECT COUNT(*) FROM task_claims WHERE task_key = 'migration:master:0276' AND status = 'done'",
        )
        .await;
        assert_eq!(done, 1);
    }

    #[tokio::test]
    async fn an_exempt_landing_is_not_checked() {
        let (_keep, repo) = repo();
        add_migration(&repo, "0276_target.sql");
        branch_adding(&repo, "feat/a", "0276_branch.sql");
        let pool = crate::testdb::fresh_pool().await;
        assert!(matches!(
            run_check(&pool, &repo, "feat/a", true).await,
            Verdict::Clear
        ));
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM task_claims").await, 0);
    }

    #[tokio::test]
    async fn a_git_failure_lets_the_landing_through() {
        let (_keep, repo) = repo();
        let pool = crate::testdb::fresh_pool().await;
        // The branch does not exist, so listing its migrations fails.
        assert!(matches!(
            run_check(&pool, &repo, "feat/missing", false).await,
            Verdict::Clear
        ));
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM task_claims").await, 0);
    }

    #[tokio::test]
    async fn two_branches_both_adding_0276_queued_together_the_second_is_refused_once_the_first_merged()
     {
        let (_keep, repo) = repo();
        branch_adding(&repo, "feat/a", "0276_a.sql");
        branch_adding(&repo, "feat/b", "0276_b.sql");
        let pool = crate::testdb::fresh_pool().await;
        crate::project::rules::testing::hold_shared();
        let master = Branch::new("master").unwrap();
        let ask = |name: &str| {
            let op = Op::Merge {
                source: Branch::new(name).unwrap(),
                target: master.clone(),
            };
            let (pool, repo, master) = (&pool, &repo, &master);
            async move {
                refusal_for_op(
                    pool,
                    PROJECT,
                    repo,
                    &op,
                    false,
                    Origin::Human,
                    Some(1),
                    Some(master),
                )
                .await
            }
        };
        // Both pass while the target has neither.
        assert!(ask("feat/a").await.is_none());
        assert!(ask("feat/b").await.is_none());
        git(&repo, &["merge", "--no-ff", "-m", "land a", "feat/a"]);
        let held = ask("feat/b").await.expect("the second is refused");
        assert!(
            held.starts_with("refused when the queue came to run it: "),
            "{held}"
        );
        assert!(held.contains("migration:master:0276"), "{held}");
    }

    /// Rules with one `renumber-sequence` step over `dir`, enabled or not.
    fn renumber_rules(dir: &str, enabled: bool) -> String {
        format!(
            "gate_command: git --version
integration_steps:
  - id: renumber
    kind: builtin:renumber-sequence
    enabled: {enabled}
    config: {{ dir: \"{dir}\", pattern: \"{{N:4}}_*.sql\" }}
"
        )
    }

    /// A colliding `0276` on `feat/a`, checked after `rules` were published for the project.
    async fn collision_verdict_under(rules: &str) -> Verdict {
        let (_keep, repo) = repo();
        add_migration(&repo, "0276_target.sql");
        branch_adding(&repo, "feat/a", "0276_branch.sql");
        let pool = crate::testdb::fresh_pool().await;
        crate::project::rules::testing::publish_rules(PROJECT, rules);
        let verdict = run_check(&pool, &repo, "feat/a", false).await;
        if matches!(verdict, Verdict::Clear) {
            assert_eq!(count(&pool, "SELECT COUNT(*) FROM task_claims").await, 0);
        }
        verdict
    }

    #[tokio::test]
    async fn an_active_renumber_step_for_the_migrations_dir_lets_a_colliding_number_through() {
        // A trailing slash is the same directory.
        for dir in ["core/migrations", "core/migrations/"] {
            let verdict = collision_verdict_under(&renumber_rules(dir, true)).await;
            assert!(matches!(verdict, Verdict::Clear), "dir {dir}");
        }
    }

    #[tokio::test]
    async fn a_renumber_step_for_another_dir_keeps_the_migration_gate() {
        let verdict = collision_verdict_under(&renumber_rules("db/migrations", true)).await;
        let message = refusal(verdict);
        assert!(message.contains("migration:master:0276"), "{message}");
    }

    #[tokio::test]
    async fn a_disabled_renumber_step_keeps_the_migration_gate() {
        let verdict = collision_verdict_under(&renumber_rules("core/migrations", false)).await;
        let message = refusal(verdict);
        assert!(message.contains("migration:master:0276"), "{message}");
    }
}
