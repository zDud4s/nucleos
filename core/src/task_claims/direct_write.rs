//! The daemon's half of "master is closed": it notices the integration branch moving without the
//! queue, opens the machine ticket `direct-write:<target>` and writes one feed line.
//!
//! The local half is the `reference-transaction` hook `scripts/install-hooks.sh` writes: it refuses a
//! write to `refs/heads/<target>` that does not carry `QUEUE_MARKER`, and lets one through under
//! `NUCLEOS_ALLOW_DIRECT_GIT`, recording it as `bypass <epoch> <old> <new> <ref>` in
//! `<git-common-dir>/nucleos-direct-writes.log`. A hook is a convention, not a boundary (no
//! `core.hooksPath`, git older than 2.28, or a deleted hook and nothing refuses), so this scan is
//! what makes every write that got past it visible.
//!
//! One row per project and target in `target_watch` (migration 0298) holds the last tip accounted
//! for. A tick that finds the branch elsewhere lists the first-parent commits since; the queue's
//! merges are always `--no-ff`, so every commit it puts on that line is one succeeded request's
//! `result_sha`, and anything else is direct. The row advances only once the move is accounted for.
//! Runs on the resolver's 60 s tick, fail-open: an error is logged and the next tick tries again.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::SqlitePool;

use super::machine::{self, DirectWriteKind};
use crate::execution::git_exec;
use crate::execution::land::{self, IntegrationBranch};

/// The file the `reference-transaction` hook appends a bypass to, under the git common dir.
pub const LOG_FILE: &str = "nucleos-direct-writes.log";
/// Feed kind of a write the hook let through because `NUCLEOS_ALLOW_DIRECT_GIT` was set.
pub const BYPASS_FEED_KIND: &str = "direct_write_bypass";
/// Feed kind of a write nothing recorded: the hook was missing, skipped or beaten.
pub const VIOLATION_FEED_KIND: &str = "direct_write_violation";

/// Git budget of one project's scan; well inside the resolver's `STEP_TIMEOUT`.
const SCAN_BUDGET: Duration = Duration::from_secs(10);
/// At most this many first-parent commits are listed per scan (the newest, which hold the tip).
const MAX_LISTED: usize = 200;

/// What one scan of one project found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scan {
    /// No tip was recorded for this target yet; the current one is now the baseline, no ticket.
    Baseline,
    /// The target is where it was.
    Unchanged,
    /// Nothing was decided and the recorded tip did not move; the reason says why.
    Skipped(&'static str),
    /// The target moved, and every new first-parent commit is a succeeded queue request's result.
    Queue,
    /// The target moved outside the queue: `shas` are the commits nobody queued (the new tip alone
    /// when the target was rewound or rewritten).
    Detected {
        kind: DirectWriteKind,
        shas: Vec<String>,
    },
}

/// Scans every project with a folder. Fail-open: a failed project is logged and the next one runs.
pub async fn scan_all(pool: &SqlitePool) {
    let projects: Vec<(String, String)> = match sqlx::query_as(
        "SELECT project_id, project_root FROM autopilot_state WHERE project_root IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    {
        Ok(projects) => projects,
        Err(error) => {
            tracing::warn!(%error, "direct writes: could not list the projects to scan");
            return;
        }
    };
    for (project_id, root) in projects {
        if let Err(error) = scan_project(pool, &project_id, Path::new(&root), Utc::now()).await {
            tracing::warn!(project = %project_id, %error, "direct writes: scan failed");
        }
    }
}

/// Scans one project's integration branch; see the module doc for the rule.
///
/// While a request of the project is `running` nothing is decided and the recorded tip stays put:
/// between the queue's publish and its `result_sha` being written, the queue's own merge would read
/// as direct.
pub async fn scan_project(
    pool: &SqlitePool,
    project_id: &str,
    root: &Path,
    now: DateTime<Utc>,
) -> Result<Scan, String> {
    let deadline = Instant::now() + SCAN_BUDGET;
    let target = match land::integration_branch_reading(pool, project_id, deadline).await? {
        IntegrationBranch::Declared { branch } | IntegrationBranch::Derived { branch } => branch,
        IntegrationBranch::Stale { .. } | IntegrationBranch::Unknown { .. } => {
            return Ok(Scan::Skipped("no integration branch"));
        }
    };
    let reference = format!("refs/heads/{target}");
    let tip = crate::verify::resolve_commit(root, &reference, deadline).await?;

    let recorded: Option<String> =
        sqlx::query_scalar("SELECT tip FROM target_watch WHERE project_id = ? AND target = ?")
            .bind(project_id)
            .bind(&target)
            .fetch_optional(pool)
            .await
            .map_err(|error| format!("could not read the watched tip: {error}"))?;
    let Some(previous) = recorded else {
        record_tip(pool, project_id, &target, &tip, now).await?;
        return Ok(Scan::Baseline);
    };
    if previous == tip {
        return Ok(Scan::Unchanged);
    }
    if queue_running(pool, project_id).await? {
        return Ok(Scan::Skipped("queue running"));
    }

    // A previous tip git no longer knows also reads as "not an ancestor": the branch was rewritten.
    let rewritten = !git_exec::is_ancestor(root, &previous, &tip, deadline).await?;
    let direct = if rewritten {
        vec![tip.clone()]
    } else {
        let range = first_parent_range(root, &previous, &tip, deadline).await?;
        let mut queued: HashSet<String> = HashSet::new();
        for sha in &range {
            if queue_published(pool, sha).await? {
                queued.insert(sha.clone());
                // A land that ran integration steps publishes an `integrate: ...` commit on top
                // of the merge; the request holds only that commit, so the merge is queued too.
                if let Some(merge) = integrate_first_parent(root, sha, deadline).await? {
                    queued.insert(merge);
                }
            }
        }
        range
            .into_iter()
            .filter(|sha| !queued.contains(sha))
            .collect()
    };
    if direct.is_empty() {
        record_tip(pool, project_id, &target, &tip, now).await?;
        return Ok(Scan::Queue);
    }

    // An approximation, documented: a bypass that put the tip there explains the move, as does one
    // per direct commit. A log git cannot locate reads as empty, which errs toward a violation.
    let bypassed = match bypass_log(root, deadline).await {
        Ok(log) => bypassed_tips(&log, &reference),
        Err(error) => {
            tracing::warn!(project = %project_id, %error, "direct writes: could not read the bypass log");
            HashSet::new()
        }
    };
    let kind = if bypassed.contains(&tip) || direct.iter().all(|sha| bypassed.contains(sha)) {
        DirectWriteKind::Bypass
    } else {
        DirectWriteKind::Violation
    };

    let author = tip_author(root, &tip, deadline).await;
    let detail = json!({
        "target": &target,
        "previous": &previous,
        "tip": &tip,
        "shas": &direct,
        "rewritten": rewritten,
    });
    machine::open_direct_write_ticket(pool, project_id, &target, kind, detail, now)
        .await
        .map_err(|error| format!("could not open the direct-write ticket: {error}"))?;
    let line = summary(
        &target,
        kind,
        rewritten,
        &previous,
        &direct,
        author.as_deref(),
    );
    // The ticket is the record; a lost feed line is logged rather than re-reporting next tick.
    // Each kind is a bare constant at its own call site, so the shell's completeness scan reads it.
    let appended = match kind {
        DirectWriteKind::Bypass => {
            crate::feed::append(pool, Some(project_id), BYPASS_FEED_KIND, &line, None, None).await
        }
        DirectWriteKind::Violation => {
            crate::feed::append(
                pool,
                Some(project_id),
                VIOLATION_FEED_KIND,
                &line,
                None,
                None,
            )
            .await
        }
    };
    if let Err(error) = appended {
        tracing::warn!(project = %project_id, %error, "direct writes: could not write the feed line");
    }
    record_tip(pool, project_id, &target, &tip, now).await?;
    Ok(Scan::Detected { kind, shas: direct })
}

async fn record_tip(
    pool: &SqlitePool,
    project_id: &str,
    target: &str,
    tip: &str,
    now: DateTime<Utc>,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO target_watch (project_id, target, tip, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (project_id, target) DO UPDATE SET tip = excluded.tip,
                                                        updated_at = excluded.updated_at",
    )
    .bind(project_id)
    .bind(target)
    .bind(tip)
    .bind(now.to_rfc3339())
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(|error| format!("could not record the watched tip: {error}"))
}

async fn queue_running(pool: &SqlitePool, project_id: &str) -> Result<bool, String> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM vcs_requests WHERE project_id = ? AND status = 'running')",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map_err(|error| format!("could not ask whether the queue is running: {error}"))
}

/// Whether a succeeded request put `sha` there. Not scoped to the project: a sha names one commit,
/// and a sibling project publishing into the same repository is still the queue.
async fn queue_published(pool: &SqlitePool, sha: &str) -> Result<bool, String> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM vcs_requests WHERE status = 'succeeded' AND result_sha = ?)",
    )
    .bind(sha)
    .fetch_one(pool)
    .await
    .map_err(|error| format!("could not look up the queue's results: {error}"))
}

/// The first parent of `sha` when `sha` is an `integrate: ...` commit (the queue's step commit).
async fn integrate_first_parent(
    root: &Path,
    sha: &str,
    deadline: Instant,
) -> Result<Option<String>, String> {
    let shown = git_stdout(root, &["log", "-1", "--format=%s%n%P", sha], deadline).await?;
    let mut lines = shown.lines();
    let subject = lines.next().unwrap_or("");
    let parents = lines.next().unwrap_or("");
    if !subject.starts_with("integrate: ") {
        return Ok(None);
    }
    Ok(parents.split_whitespace().next().map(str::to_owned))
}

/// One git command's stdout, or why it gave none. The budget is what is left of `deadline`.
async fn git_stdout(root: &Path, args: &[&str], deadline: Instant) -> Result<String, String> {
    let budget = deadline.saturating_duration_since(Instant::now());
    if budget.is_zero() {
        return Err(format!(
            "the scan ran out of time before git {}",
            args.join(" ")
        ));
    }
    let argv: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let result = git_exec::run_git(root, &argv, budget).await?;
    if !result.succeeded() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            result.output_tail.trim()
        ));
    }
    Ok(result.stdout)
}

/// The first-parent commits of `(base, tip]`, oldest first, the newest `MAX_LISTED` only. One
/// `rev-list` rather than `git_exec::first_parent_commits`, which also diffs every commit for the
/// tests map: a scan after a long downtime would spawn one git per commit for a flag it never reads.
async fn first_parent_range(
    root: &Path,
    base: &str,
    tip: &str,
    deadline: Instant,
) -> Result<Vec<String>, String> {
    let range = format!("{base}..{tip}");
    let limit = format!("--max-count={MAX_LISTED}");
    let listed = git_stdout(
        root,
        &["rev-list", "--first-parent", &limit, &range],
        deadline,
    )
    .await?;
    let mut shas: Vec<String> = listed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    shas.reverse();
    Ok(shas)
}

/// The bypass log's text; a log that does not exist yet is empty.
async fn bypass_log(root: &Path, deadline: Instant) -> Result<String, String> {
    let common = git_stdout(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        deadline,
    )
    .await?;
    let path = PathBuf::from(common.trim()).join(LOG_FILE);
    match tokio::fs::read_to_string(&path).await {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

/// The `<new>` shas of the log's `bypass <epoch> <old> <new> <ref>` lines for `reference`.
fn bypassed_tips(log: &str, reference: &str) -> HashSet<String> {
    log.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields.as_slice() {
                ["bypass", _epoch, _old, new, name] if *name == reference => {
                    Some((*new).to_owned())
                }
                _ => None,
            }
        })
        .collect()
}

/// The tip commit's author, when git will say; only for the feed line.
async fn tip_author(root: &Path, tip: &str, deadline: Instant) -> Option<String> {
    let author = git_stdout(root, &["log", "-1", "--format=%an", tip], deadline)
        .await
        .ok()?;
    let author = author.trim();
    (!author.is_empty()).then(|| author.to_owned())
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// One English line: the target, the short shas, how, and the author when known.
fn summary(
    target: &str,
    kind: DirectWriteKind,
    rewritten: bool,
    previous: &str,
    direct: &[String],
    author: Option<&str>,
) -> String {
    const SHOWN: usize = 5;
    let what = if rewritten {
        format!(
            "{target} was rewritten outside the queue ({} -> {})",
            short(previous),
            direct.first().map(|sha| short(sha)).unwrap_or("?")
        )
    } else {
        let listed: Vec<&str> = direct.iter().take(SHOWN).map(|sha| short(sha)).collect();
        let more = direct.len().saturating_sub(SHOWN);
        let more = if more > 0 {
            format!(" +{more} more")
        } else {
            String::new()
        };
        let noun = if direct.len() == 1 {
            "commit"
        } else {
            "commits"
        };
        let shas = listed.join(", ");
        format!(
            "{target} advanced outside the queue: {} {noun} ({shas}{more})",
            direct.len()
        )
    };
    let how = match kind {
        DirectWriteKind::Bypass => "through NUCLEOS_ALLOW_DIRECT_GIT",
        DirectWriteKind::Violation => "not recorded by the hook",
    };
    match author {
        Some(author) => format!("{what}, {how}, by {author}"),
        None => format!("{what}, {how}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::git_exec::testkit;
    use chrono::TimeZone;

    const PROJECT: &str = "alpha";

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn commit_by_hand(repo: &Path, name: &str) -> String {
        std::fs::write(repo.join(name), format!("{name}\n")).expect("write a file");
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", name]);
        git(repo, &["rev-parse", "refs/heads/master"])
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 9, 10, 0, 0).unwrap()
    }

    /// Project `alpha` on a repository whose `master` is the declared integration branch.
    async fn setup() -> (SqlitePool, tempfile::TempDir, PathBuf) {
        let pool = crate::testdb::fresh_pool().await;
        let (container, repo) = testkit::init_contained_repo("nucleos-direct-write-");
        git(&repo, &["branch", "-M", "master"]);
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root, integration_branch)
             VALUES (?, 'active', ?, 'master')",
        )
        .bind(PROJECT)
        .bind(repo.to_string_lossy().into_owned())
        .execute(&pool)
        .await
        .expect("register the project");
        (pool, container, repo)
    }

    async fn scan(pool: &SqlitePool, repo: &Path) -> Scan {
        scan_project(pool, PROJECT, repo, now())
            .await
            .expect("the scan runs")
    }

    async fn watched_tip(pool: &SqlitePool) -> Option<String> {
        sqlx::query_scalar(
            "SELECT tip FROM target_watch WHERE project_id = ? AND target = 'master'",
        )
        .bind(PROJECT)
        .fetch_optional(pool)
        .await
        .expect("read the watched tip")
    }

    async fn request(pool: &SqlitePool, status: &str, result_sha: Option<&str>) -> i64 {
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, status, result_sha, created_at)
             VALUES ('merge', '{}', ?, 'C:/work', 'job', ?, ?, '2026-11-09T00:00:00Z')",
        )
        .bind(PROJECT)
        .bind(status)
        .bind(result_sha)
        .execute(pool)
        .await
        .expect("insert a request")
        .last_insert_rowid()
    }

    async fn ticket_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM task_claims")
            .fetch_one(pool)
            .await
            .expect("count the tickets")
    }

    async fn live_ticket(pool: &SqlitePool) -> crate::task_claims::TaskRow {
        let key = machine::direct_write_key("master").expect("a direct-write key");
        crate::task_claims::store::live_by_key(pool, PROJECT, &key, now())
            .await
            .expect("read the ticket")
            .expect("the ticket is live")
    }

    async fn feed_lines(pool: &SqlitePool) -> Vec<(String, String)> {
        sqlx::query_as("SELECT kind, summary FROM feed WHERE project_id = ? ORDER BY id")
            .bind(PROJECT)
            .fetch_all(pool)
            .await
            .expect("read the feed")
    }

    #[tokio::test]
    async fn the_first_scan_only_records_a_baseline() {
        let (pool, _container, repo) = setup().await;
        let tip = git(&repo, &["rev-parse", "refs/heads/master"]);

        assert_eq!(scan(&pool, &repo).await, Scan::Baseline);
        assert_eq!(watched_tip(&pool).await.as_deref(), Some(tip.as_str()));
        assert_eq!(ticket_count(&pool).await, 0);
        assert!(feed_lines(&pool).await.is_empty());
        assert_eq!(scan(&pool, &repo).await, Scan::Unchanged);
    }

    #[tokio::test]
    async fn a_queue_publish_is_not_a_direct_write() {
        let (pool, _container, repo) = setup().await;
        assert_eq!(scan(&pool, &repo).await, Scan::Baseline);
        let published = commit_by_hand(&repo, "merged.txt");
        request(&pool, "succeeded", Some(&published)).await;

        assert_eq!(scan(&pool, &repo).await, Scan::Queue);
        assert_eq!(
            watched_tip(&pool).await.as_deref(),
            Some(published.as_str())
        );
        assert_eq!(ticket_count(&pool).await, 0);
        assert!(feed_lines(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn an_integrate_commit_over_a_merge_is_all_queue() {
        let (pool, _container, repo) = setup().await;
        assert_eq!(scan(&pool, &repo).await, Scan::Baseline);
        // old <- M (the merge) <- I (`integrate: ...`); the request holds only I.
        let _merge = commit_by_hand(&repo, "merged.txt");
        std::fs::write(repo.join("fixed.txt"), "fixed\n").expect("write a file");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "integrate: fmt for feat/x"]);
        let integrate = git(&repo, &["rev-parse", "refs/heads/master"]);
        request(&pool, "succeeded", Some(&integrate)).await;

        assert_eq!(scan(&pool, &repo).await, Scan::Queue);
        assert_eq!(ticket_count(&pool).await, 0);
        assert!(feed_lines(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn a_commit_made_by_hand_on_the_target_opens_a_violation_ticket_and_a_feed_line() {
        let (pool, _container, repo) = setup().await;
        let previous = git(&repo, &["rev-parse", "refs/heads/master"]);
        assert_eq!(scan(&pool, &repo).await, Scan::Baseline);
        let tip = commit_by_hand(&repo, "by-hand.txt");

        assert_eq!(
            scan(&pool, &repo).await,
            Scan::Detected {
                kind: DirectWriteKind::Violation,
                shas: vec![tip.clone()],
            }
        );
        let ticket = live_ticket(&pool).await;
        assert_eq!(ticket.detail["kind"], json!("violation"));
        assert_eq!(ticket.detail["target"], json!("master"));
        assert_eq!(ticket.detail["previous"], json!(previous));
        assert_eq!(ticket.detail["tip"], json!(tip));
        assert_eq!(ticket.detail["shas"], json!([tip]));
        let lines = feed_lines(&pool).await;
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].0, VIOLATION_FEED_KIND);
        assert!(lines[0].1.contains("master"), "{}", lines[0].1);
        assert!(lines[0].1.contains(short(&tip)), "{}", lines[0].1);
        assert_eq!(watched_tip(&pool).await.as_deref(), Some(tip.as_str()));
    }

    #[tokio::test]
    async fn a_commit_covered_by_the_hook_log_is_recorded_as_a_bypass() {
        let (pool, _container, repo) = setup().await;
        let previous = git(&repo, &["rev-parse", "refs/heads/master"]);
        assert_eq!(scan(&pool, &repo).await, Scan::Baseline);
        let tip = commit_by_hand(&repo, "escape.txt");
        // The line the hook writes at "committed" under NUCLEOS_ALLOW_DIRECT_GIT.
        std::fs::write(
            repo.join(".git").join(LOG_FILE),
            format!("bypass 1762682400 {previous} {tip} refs/heads/master\n"),
        )
        .expect("write the bypass log");

        assert_eq!(
            scan(&pool, &repo).await,
            Scan::Detected {
                kind: DirectWriteKind::Bypass,
                shas: vec![tip.clone()],
            }
        );
        assert_eq!(live_ticket(&pool).await.detail["kind"], json!("bypass"));
        let lines = feed_lines(&pool).await;
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].0, BYPASS_FEED_KIND);
        assert!(
            lines[0].1.contains("NUCLEOS_ALLOW_DIRECT_GIT"),
            "{}",
            lines[0].1
        );
    }

    #[tokio::test]
    async fn a_running_queue_request_defers_the_scan_without_advancing() {
        let (pool, _container, repo) = setup().await;
        let baseline = git(&repo, &["rev-parse", "refs/heads/master"]);
        assert_eq!(scan(&pool, &repo).await, Scan::Baseline);
        // The queue has published, and has not written `result_sha` yet.
        let published = commit_by_hand(&repo, "publishing.txt");
        let id = request(&pool, "running", None).await;

        assert_eq!(scan(&pool, &repo).await, Scan::Skipped("queue running"));
        assert_eq!(watched_tip(&pool).await.as_deref(), Some(baseline.as_str()));
        assert_eq!(ticket_count(&pool).await, 0);

        sqlx::query("UPDATE vcs_requests SET status = 'succeeded', result_sha = ? WHERE id = ?")
            .bind(&published)
            .bind(id)
            .execute(&pool)
            .await
            .expect("finish the request");
        assert_eq!(scan(&pool, &repo).await, Scan::Queue);
        assert_eq!(
            watched_tip(&pool).await.as_deref(),
            Some(published.as_str())
        );
        assert_eq!(ticket_count(&pool).await, 0);
    }

    #[tokio::test]
    async fn a_rewound_target_is_a_violation() {
        let (pool, _container, repo) = setup().await;
        let first = commit_by_hand(&repo, "one.txt");
        let second = commit_by_hand(&repo, "two.txt");
        assert_eq!(scan(&pool, &repo).await, Scan::Baseline);
        git(&repo, &["reset", "-q", "--hard", &first]);

        assert_eq!(
            scan(&pool, &repo).await,
            Scan::Detected {
                kind: DirectWriteKind::Violation,
                shas: vec![first.clone()],
            }
        );
        let ticket = live_ticket(&pool).await;
        assert_eq!(ticket.detail["previous"], json!(second));
        assert_eq!(ticket.detail["tip"], json!(first));
        assert_eq!(ticket.detail["rewritten"], json!(true));
        assert_eq!(watched_tip(&pool).await.as_deref(), Some(first.as_str()));
    }

    #[tokio::test]
    async fn a_second_direct_write_adds_a_report_not_a_second_ticket() {
        let (pool, _container, repo) = setup().await;
        assert_eq!(scan(&pool, &repo).await, Scan::Baseline);
        let first = commit_by_hand(&repo, "first.txt");
        let found = scan(&pool, &repo).await;
        assert!(matches!(found, Scan::Detected { .. }), "{found:?}");
        let opened = live_ticket(&pool).await;
        let second = commit_by_hand(&repo, "second.txt");

        assert_eq!(
            scan(&pool, &repo).await,
            Scan::Detected {
                kind: DirectWriteKind::Violation,
                shas: vec![second.clone()],
            },
            "only the commit after {first} is new"
        );
        let again = live_ticket(&pool).await;
        assert_eq!(again.id, opened.id, "the same target is one ticket");
        assert_eq!(again.report_count, 2);
        assert_eq!(ticket_count(&pool).await, 1);
        assert_eq!(feed_lines(&pool).await.len(), 2);
    }
}
