//! Which rostered project a transcript's cwd belongs to: a roster root, a linked worktree found
//! through the git common dir, persisted in `devtime_cwd_map`. Daemon run/job worktrees are
//! recognised and skipped; a cwd nothing explains stays unmapped rather than being guessed.

use crate::devtime_store::{self, CwdMapping};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long an unmapped answer is trusted before the cwd is looked at again.
const UNMAPPED_RETRY: chrono::Duration = chrono::Duration::hours(1);
const GIT_DEADLINE: Duration = Duration::from_secs(5);

/// `\` becomes `/`, the trailing `/` goes, a Windows verbatim prefix goes, and the path is
/// lowercased on Windows, where the file system does not tell `C:` from `c:`.
pub fn normalise(cwd: &str) -> String {
    let mut path = cwd.replace('\\', "/");
    if let Some(rest) = path.strip_prefix("//?/") {
        path = rest.to_owned();
    }
    while path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    if cfg!(windows) {
        path = path.to_lowercase();
    }
    path
}

fn is_run_or_job(segment: &str) -> bool {
    ["run-", "job-"].iter().any(|prefix| {
        segment
            .strip_prefix(prefix)
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// A worktree the daemon opened for a run or a job: the path holds `.nucleos/worktrees/`, or its
/// last segment is `run-<digits>` / `job-<digits>`. Pure.
pub fn is_daemon_worktree(path: &str) -> bool {
    let path = normalise(path);
    if format!("{path}/").contains(".nucleos/worktrees/") {
        return true;
    }
    path.rsplit('/').next().is_some_and(is_run_or_job)
}

fn under(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn mapping(cwd: &str, project: Option<&str>, worktree: Option<String>, kind: &str) -> CwdMapping {
    CwdMapping {
        cwd: cwd.to_owned(),
        project_id: project.map(str::to_owned),
        worktree,
        kind: kind.to_owned(),
        resolved_at: now(),
    }
}

/// Resolve one cwd. See [`resolve_with`]; this keeps no root-key cache between calls.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn resolve(pool: &SqlitePool, cwd: &str, roster: &[(String, String)]) -> CwdMapping {
    resolve_with(pool, cwd, roster, &mut HashMap::new()).await
}

/// `root_keys` caches each roster root's repository key for as long as the caller keeps it (one
/// ingestion cycle). Every result is persisted; a store failure is logged and the answer still
/// returned, since the answer does not depend on the write.
pub async fn resolve_with(
    pool: &SqlitePool,
    cwd: &str,
    roster: &[(String, String)],
    root_keys: &mut HashMap<String, Option<String>>,
) -> CwdMapping {
    let norm = normalise(cwd);

    // 1. A cached row wins; an unmapped one only until it is an hour old.
    if let Ok(Some(row)) = devtime_store::cwd_mapping(pool, cwd).await {
        let stale_unmapped = row.kind == "unmapped"
            && chrono::DateTime::parse_from_rfc3339(&row.resolved_at)
                .map(|at| chrono::Utc::now().signed_duration_since(at) >= UNMAPPED_RETRY)
                .unwrap_or(true);
        // A row naming a project that has left the roster is not trusted: it is decided again
        // against the current roster (spec §3.1, sessions outside the roster are ignored).
        let left_roster = row
            .project_id
            .as_deref()
            .is_some_and(|project| !roster.iter().any(|(id, _)| id == project));
        if !stale_unmapped && !left_roster {
            return row;
        }
    }

    let result = decide(pool, cwd, &norm, roster, root_keys).await;
    if let Err(error) = devtime_store::save_cwd_mapping(pool, &result).await {
        tracing::warn!(%error, "devtime: could not persist a cwd mapping");
    }
    result
}

async fn decide(
    pool: &SqlitePool,
    cwd: &str,
    norm: &str,
    roster: &[(String, String)],
    root_keys: &mut HashMap<String, Option<String>>,
) -> CwdMapping {
    // 2. A daemon worktree.
    if is_daemon_worktree(norm) {
        return mapping(cwd, None, None, "daemon_worktree");
    }

    // 3. Equal to or under a root. The longest root wins when roots nest.
    let best = roster
        .iter()
        .filter(|(_, root)| under(norm, &normalise(root)))
        .max_by_key(|(_, root)| normalise(root).len());
    if let Some((project, root)) = best {
        return mapping(cwd, Some(project), Some(normalise(root)), "project");
    }

    // 4. Git: the checkout this cwd stands in, and whose repository it belongs to.
    let deadline = Instant::now() + GIT_DEADLINE;
    if let Ok(toplevel) = crate::git_exec::toplevel(Path::new(cwd), deadline).await {
        let toplevel_s = toplevel.to_string_lossy().into_owned();
        if let Ok(key) = crate::git_exec::repo_key(&toplevel, deadline).await {
            for (project, root) in roster {
                let root_key = match root_keys.get(root) {
                    Some(cached) => cached.clone(),
                    None => {
                        let found = crate::git_exec::repo_key(Path::new(root), deadline)
                            .await
                            .ok();
                        root_keys.insert(root.clone(), found.clone());
                        found
                    }
                };
                if root_key.as_deref() == Some(key.as_str()) {
                    return mapping(cwd, Some(project), Some(normalise(&toplevel_s)), "worktree");
                }
            }
        }
    } else {
        // 5. Git cannot see it (the worktree may be deleted): a worktree already mapped to a
        // project explains the cwd by prefix.
        for (project, _) in roster {
            let known = devtime_store::known_worktrees(pool, project)
                .await
                .unwrap_or_default();
            if let Some(worktree) = known
                .iter()
                .filter(|worktree| under(norm, &normalise(worktree)))
                .max_by_key(|worktree| worktree.len())
            {
                return mapping(cwd, Some(project), Some(normalise(worktree)), "worktree");
            }
        }
    }

    // 6. Nothing explains it.
    mapping(cwd, None, None, "unmapped")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime_store::{self, CwdMapping};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::path::Path;
    use std::process::Command;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        crate::storage::MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
            .args(args)
            .output()
            .expect("git on PATH");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn daemon_worktree_paths_are_recognised() {
        assert!(is_daemon_worktree("C:/p/proj/.nucleos/worktrees/job-7"));
        assert!(is_daemon_worktree(
            r"C:\p\proj\.nucleos\worktrees\anything\sub"
        ));
        assert!(is_daemon_worktree("/work/run-123"));
        assert!(is_daemon_worktree("/work/job-9/"));
        assert!(!is_daemon_worktree("/work/job-x"));
        assert!(!is_daemon_worktree("/work/run-"));
        assert!(!is_daemon_worktree("/work/nucleos"));
        assert_eq!(
            normalise(r"C:\A\b\"),
            if cfg!(windows) { "c:/a/b" } else { "C:/A/b" }
        );
    }

    #[tokio::test]
    async fn cwd_under_a_roster_root_maps_without_git() {
        let pool = test_pool().await;
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("proj");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let root_s = root.to_string_lossy().into_owned();
        let roster = vec![("p1".to_owned(), root_s.clone())];
        let cwd = root.join("sub").to_string_lossy().into_owned();

        let got = resolve(&pool, &cwd, &roster).await;
        assert_eq!(got.kind, "project");
        assert_eq!(got.project_id.as_deref(), Some("p1"));
        assert_eq!(got.worktree.as_deref(), Some(normalise(&root_s).as_str()));
        let stored = devtime_store::cwd_mapping(&pool, &cwd).await.unwrap();
        assert_eq!(stored.map(|row| row.kind).as_deref(), Some("project"));
    }

    #[tokio::test]
    async fn linked_worktree_maps_through_the_git_common_dir() {
        let pool = test_pool().await;
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("main");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let wt = dir.path().join("linked");
        git(
            &root,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "side"],
        );
        std::fs::create_dir_all(wt.join("deep")).unwrap();

        let roster = vec![("p1".to_owned(), root.to_string_lossy().into_owned())];
        let cwd = wt.join("deep").to_string_lossy().into_owned();
        let got = resolve(&pool, &cwd, &roster).await;
        assert_eq!(got.kind, "worktree", "{got:?}");
        assert_eq!(got.project_id.as_deref(), Some("p1"));
        let expected = normalise(&std::fs::canonicalize(&wt).unwrap().to_string_lossy());
        assert_eq!(got.worktree.as_deref(), Some(expected.as_str()));
    }

    #[tokio::test]
    async fn deleted_worktree_falls_back_to_known_prefix() {
        let pool = test_pool().await;
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("main");
        std::fs::create_dir_all(&root).unwrap();
        let gone = normalise(&dir.path().join("gone-wt").to_string_lossy());
        devtime_store::save_cwd_mapping(
            &pool,
            &CwdMapping {
                cwd: format!("{gone}/earlier"),
                project_id: Some("p1".into()),
                worktree: Some(gone.clone()),
                kind: "worktree".into(),
                resolved_at: "2026-01-01T00:00:00.000Z".into(),
            },
        )
        .await
        .unwrap();

        let roster = vec![("p1".to_owned(), root.to_string_lossy().into_owned())];
        let cwd = format!("{gone}/other/place");
        let got = resolve(&pool, &cwd, &roster).await;
        assert_eq!(got.kind, "worktree", "{got:?}");
        assert_eq!(got.project_id.as_deref(), Some("p1"));
        assert_eq!(got.worktree.as_deref(), Some(gone.as_str()));
    }

    #[tokio::test]
    async fn unknown_cwd_is_unmapped_never_guessed() {
        let pool = test_pool().await;
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("main");
        std::fs::create_dir_all(&root).unwrap();
        let roster = vec![("p1".to_owned(), root.to_string_lossy().into_owned())];

        let got = resolve(&pool, "C:/nowhere/at/all", &roster).await;
        assert_eq!(got.kind, "unmapped");
        assert!(got.project_id.is_none() && got.worktree.is_none());
        let stored = devtime_store::cwd_mapping(&pool, "C:/nowhere/at/all")
            .await
            .unwrap();
        assert_eq!(stored.map(|row| row.kind).as_deref(), Some("unmapped"));
    }

    #[tokio::test]
    async fn a_cached_mapping_whose_project_left_the_roster_is_redecided() {
        let pool = test_pool().await;
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("alpha");
        let other = dir.path().join("beta");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let cwd = root.join("sub").to_string_lossy().into_owned();

        let with_alpha = vec![("alpha".to_owned(), root.to_string_lossy().into_owned())];
        let first = resolve(&pool, &cwd, &with_alpha).await;
        assert_eq!(first.project_id.as_deref(), Some("alpha"));

        let without_alpha = vec![("beta".to_owned(), other.to_string_lossy().into_owned())];
        let again = resolve(&pool, &cwd, &without_alpha).await;
        assert_ne!(again.project_id.as_deref(), Some("alpha"), "{again:?}");
        assert_eq!(again.kind, "unmapped", "{again:?}");
        let stored = devtime_store::cwd_mapping(&pool, &cwd).await.unwrap();
        assert_eq!(stored.map(|row| row.kind).as_deref(), Some("unmapped"));
    }
}
