use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::Path;

use crate::config::RepoTrigger;

/// Repo triggers whose watched-branch SHA has changed since it was last seen.
/// A trigger with no recorded last SHA is being seen for the first time (armed, not fired); a trigger
/// whose current SHA is unknown (git failed this poll) is skipped.
pub fn due_repo_triggers<'a>(
    rules: &'a [RepoTrigger],
    last_shas: &HashMap<String, String>,
    current_shas: &HashMap<String, String>,
) -> Vec<&'a RepoTrigger> {
    rules
        .iter()
        .filter(
            |rule| match (last_shas.get(&rule.name), current_shas.get(&rule.name)) {
                (Some(last), Some(current)) => last != current,
                _ => false,
            },
        )
        .collect()
}

/// The last-seen branch SHA for every repo trigger of a project, keyed by trigger name.
pub async fn last_shas_for_project(
    pool: &SqlitePool,
    project_id: &str,
) -> sqlx::Result<HashMap<String, String>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT trigger_name, last_sha FROM repo_trigger_state WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Record (upsert) the last-seen branch SHA for one trigger.
pub async fn record_sha(
    pool: &SqlitePool,
    project_id: &str,
    trigger_name: &str,
    sha: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO repo_trigger_state (project_id, trigger_name, last_sha) VALUES (?, ?, ?)
         ON CONFLICT(project_id, trigger_name) DO UPDATE SET last_sha = excluded.last_sha",
    )
    .bind(project_id)
    .bind(trigger_name)
    .bind(sha)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch the repo (best-effort when `fetch` is true) and return the current SHA of `git_ref`
/// (a branch name or any ref). Returns `None` when git is unavailable or the ref does not resolve —
/// the caller treats an unknown current SHA as "skip this trigger this poll".
fn git_bin() -> String {
    std::env::var("NUCLEOS_GIT_BIN").unwrap_or_else(|_| "git".into())
}

pub async fn current_branch_sha(repo: &Path, git_ref: &str, fetch: bool) -> Option<String> {
    if fetch {
        // Best-effort: an offline / remoteless fetch failure must not block reading the local ref.
        let _ = tokio::process::Command::new(git_bin())
            .arg("-C")
            .arg(repo)
            .arg("fetch")
            .arg("--quiet")
            .output()
            .await;
    }
    let output = tokio::process::Command::new(git_bin())
        .arg("-C")
        .arg(repo)
        .arg("rev-parse")
        .arg(git_ref)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(name: &str) -> RepoTrigger {
        RepoTrigger {
            name: name.to_string(),
            branch: "main".to_string(),
            prompt: "go".to_string(),
        }
    }

    #[test]
    fn unchanged_sha_is_not_due() {
        let rules = vec![trigger("t1")];
        let last = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        let current = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        assert!(due_repo_triggers(&rules, &last, &current).is_empty());
    }

    #[test]
    fn changed_sha_is_due() {
        let rules = vec![trigger("t1")];
        let last = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        let current = HashMap::from([("t1".to_string(), "bbb".to_string())]);
        let due = due_repo_triggers(&rules, &last, &current);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "t1");
    }

    #[test]
    fn first_sight_without_last_sha_is_not_due() {
        let rules = vec![trigger("t1")];
        let last = HashMap::new();
        let current = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        assert!(due_repo_triggers(&rules, &last, &current).is_empty());
    }

    #[test]
    fn missing_current_sha_is_not_due() {
        let rules = vec![trigger("t1")];
        let last = HashMap::from([("t1".to_string(), "aaa".to_string())]);
        let current = HashMap::new();
        assert!(due_repo_triggers(&rules, &last, &current).is_empty());
    }

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn last_shas_is_empty_for_a_fresh_project() {
        let pool = test_pool().await;
        assert!(last_shas_for_project(&pool, "p1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn record_sha_then_read_round_trips_and_upserts() {
        let pool = test_pool().await;
        record_sha(&pool, "p1", "t1", "aaa").await.unwrap();
        assert_eq!(
            last_shas_for_project(&pool, "p1").await.unwrap(),
            HashMap::from([("t1".to_string(), "aaa".to_string())])
        );

        record_sha(&pool, "p1", "t1", "bbb").await.unwrap();
        assert_eq!(
            last_shas_for_project(&pool, "p1").await.unwrap(),
            HashMap::from([("t1".to_string(), "bbb".to_string())])
        );
    }

    #[tokio::test]
    async fn shas_are_scoped_per_project() {
        let pool = test_pool().await;
        record_sha(&pool, "p1", "t1", "aaa").await.unwrap();
        record_sha(&pool, "p2", "t1", "zzz").await.unwrap();
        assert_eq!(
            last_shas_for_project(&pool, "p1").await.unwrap(),
            HashMap::from([("t1".to_string(), "aaa".to_string())])
        );
        assert_eq!(
            last_shas_for_project(&pool, "p2").await.unwrap(),
            HashMap::from([("t1".to_string(), "zzz".to_string())])
        );
    }

    fn git_ok(dir: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start")
            .success()
    }

    fn git_stdout(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git should start");
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn init_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("create repo tempdir");
        assert!(git_ok(repo.path(), &["init"]));
        assert!(git_ok(repo.path(), &["config", "user.email", "test@x"]));
        assert!(git_ok(repo.path(), &["config", "user.name", "test"]));
        std::fs::write(repo.path().join("seed.txt"), "seed\n").unwrap();
        assert!(git_ok(repo.path(), &["add", "-A"]));
        assert!(git_ok(repo.path(), &["commit", "-m", "seed"]));
        repo
    }

    #[tokio::test]
    async fn current_sha_matches_rev_parse_and_tracks_a_new_commit() {
        let repo = init_repo();
        let head = git_stdout(repo.path(), &["rev-parse", "HEAD"]);
        assert_eq!(
            current_branch_sha(repo.path(), "HEAD", false).await,
            Some(head.clone())
        );

        std::fs::write(repo.path().join("b.txt"), "b\n").unwrap();
        assert!(git_ok(repo.path(), &["add", "-A"]));
        assert!(git_ok(repo.path(), &["commit", "-m", "second"]));
        let head2 = git_stdout(repo.path(), &["rev-parse", "HEAD"]);
        assert_ne!(head, head2);
        assert_eq!(
            current_branch_sha(repo.path(), "HEAD", false).await,
            Some(head2)
        );
    }

    #[tokio::test]
    async fn unknown_ref_returns_none() {
        let repo = init_repo();
        assert_eq!(
            current_branch_sha(repo.path(), "no-such-branch", false).await,
            None
        );
    }

    #[tokio::test]
    async fn fetch_true_without_a_remote_is_best_effort() {
        let repo = init_repo();
        // fetch fails (no remote configured) but the local ref still resolves.
        assert!(
            current_branch_sha(repo.path(), "HEAD", true)
                .await
                .is_some()
        );
    }
}
