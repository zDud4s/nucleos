//! Reads what a finished tree (or a merge request) left behind and turns it into [`TreeBlocks`]
//! (spec 2026-11-16-memoria-agentes-captura, F3 P1): node handoffs, answered decisions, failed
//! verifications, the last message of each node's run and, when the repository answers, the diff
//! stat and the commit subjects.

use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use super::{
    COMMIT_LINES, COMMITS_CHARS, DECISION_CHARS, DIFF_CHARS, ENTRY_CHARS, MAX_DECISIONS,
    MAX_FAILURES, MAX_NODES, NodeReport, SIGNAL_LINES, TreeBlocks, clip_head, known,
    node_report_block, signal_window,
};
use crate::tree_job::store::TreeJob;

/// Most commits whose summary is read for one tree.
const MAX_SHAS: usize = 8;
/// Deadline of each git read.
const GIT_DEADLINE: Duration = Duration::from_secs(10);

/// The blocks of a tree. `focus_node` restricts the node reports and the failures to that node.
#[allow(dead_code)]
pub(super) async fn gather_blocks(
    pool: &SqlitePool,
    job: &TreeJob,
    focus_node: Option<&str>,
    now: DateTime<Utc>,
) -> sqlx::Result<TreeBlocks> {
    let mut blocks = TreeBlocks::default();

    // Node reports: counts run over every node, the report list over the focused ones, capped.
    let nodes: Vec<(String, String, Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT id, status, summary, handoff_path, run_id FROM work_nodes
             WHERE tree_job_id = ? ORDER BY created_at, id",
        )
        .bind(&job.id)
        .fetch_all(pool)
        .await?;
    for (id, status, summary, handoff_path, run_id) in &nodes {
        let report = handoff_path
            .as_deref()
            .filter(|path| !path.is_empty())
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| crate::tree_job::report::parse_report(&text));
        let has_open_items = report.as_ref().is_some_and(|r| !r.open_items.trim().is_empty());
        if matches!(status.as_str(), "failed" | "blocked") {
            blocks.bad_nodes += 1;
        }
        if has_open_items {
            blocks.open_item_nodes += 1;
        }
        let in_focus = focus_node.is_none_or(|focus| focus == id);
        if in_focus && blocks.node_reports.len() < MAX_NODES {
            let text = match &report {
                Some(report) => node_report_block(report, status),
                None => clip_head(summary.as_deref().unwrap_or_default(), ENTRY_CHARS),
            };
            blocks.node_reports.push(NodeReport {
                node_id: id.clone(),
                status: status.clone(),
                text,
                has_open_items,
            });
        }
        // The last message of the node's run.
        if in_focus
            && let Some(run) = run_id.as_deref().and_then(|raw| raw.parse::<i64>().ok())
        {
            let stdout: Option<Option<String>> =
                sqlx::query_scalar("SELECT stdout FROM runs WHERE id = ?")
                    .bind(run)
                    .fetch_optional(pool)
                    .await?;
            if let Some(reply) = stdout
                .flatten()
                .and_then(|out| crate::execution::runner::extract_reply(&out))
                .filter(|reply| !reply.trim().is_empty())
            {
                blocks.agent_final.push((run, clip_head(&reply, ENTRY_CHARS)));
            }
        }
    }

    // Answered decisions of the tree.
    let decisions: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT question, answer, origin FROM decisions
         WHERE tree_id = ? AND status = 'answered' ORDER BY at, id LIMIT ?",
    )
    .bind(&job.id)
    .bind(MAX_DECISIONS as i64)
    .fetch_all(pool)
    .await?;
    for (question, answer, origin) in decisions {
        let line = format!(
            "Q: {question}\nA: {}\n(answered via {})",
            answer.unwrap_or_default(),
            origin.unwrap_or_else(|| "unknown".to_owned()),
        );
        blocks.decisions.push(clip_head(&line, DECISION_CHARS));
    }

    // Failed verifications of the branches' worktrees.
    let worktrees: Vec<&str> = job
        .manifest
        .branches
        .values()
        .filter(|branch| focus_node.is_none_or(|focus| branch.node_id.as_deref() == Some(focus)))
        .filter_map(|branch| branch.worktree.as_deref())
        .collect();
    if !worktrees.is_empty() {
        let list = serde_json::to_string(&worktrees).unwrap_or_else(|_| "[]".to_owned());
        let failures: Vec<(i64, Option<String>)> = sqlx::query_as(
            "SELECT id, output_tail FROM verify_runs
             WHERE status = 'failed' AND worktree IN (SELECT value FROM json_each(?))
             ORDER BY id DESC LIMIT ?",
        )
        .bind(list)
        .bind(MAX_FAILURES as i64)
        .fetch_all(pool)
        .await?;
        for (id, tail) in failures {
            let clean = crate::knowledge::strip_ansi(&tail.unwrap_or_default());
            let window = signal_window(&clean, SIGNAL_LINES);
            if !window.trim().is_empty() {
                blocks.red_failures.push((id, window));
            }
        }
    }

    // What landed, when the repository still answers.
    let mut shas: Vec<String> = match job.manifest.extra.get("land_sha").and_then(|v| v.as_str()) {
        Some(sha) if !sha.is_empty() => vec![sha.to_owned()],
        _ => job
            .manifest
            .branches
            .values()
            .filter_map(|branch| branch.extra.get("merge_sha").and_then(|v| v.as_str()))
            .filter(|sha| !sha.is_empty())
            .map(str::to_owned)
            .collect(),
    };
    shas.truncate(MAX_SHAS);
    let (diff_stat, commits) = summarise(Path::new(&job.manifest.project_root), &shas).await;
    blocks.diff_stat = diff_stat;
    blocks.commits = commits;

    if let Some(project) = job.project_id.as_deref().filter(|p| !p.is_empty()) {
        blocks.known_titles = known::titles(pool, project).await?;
        blocks.refused = known::refused(pool, project, now).await?;
    }
    Ok(blocks)
}

/// The blocks of a merge request: the commit summary of its result, plus the known and refused
/// titles of the project.
#[allow(dead_code)]
pub(super) async fn gather_request_blocks(
    pool: &SqlitePool,
    request_id: i64,
    project_id: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<TreeBlocks> {
    let mut blocks = TreeBlocks::default();
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT project_root, result_sha FROM vcs_requests WHERE id = ?")
            .bind(request_id)
            .fetch_optional(pool)
            .await?;
    if let Some((root, Some(sha))) = row
        && !sha.is_empty()
    {
        let (diff_stat, commits) = summarise(Path::new(&root), &[sha]).await;
        blocks.diff_stat = diff_stat;
        blocks.commits = commits;
    }
    blocks.known_titles = known::titles(pool, project_id).await?;
    blocks.refused = known::refused(pool, project_id, now).await?;
    Ok(blocks)
}

/// The diff stats and commit subjects of `shas`; a sha git cannot answer for is left out, and
/// nothing readable leaves both as `None`.
async fn summarise(root: &Path, shas: &[String]) -> (Option<String>, Option<String>) {
    let mut stats = Vec::new();
    let mut subjects: Vec<String> = Vec::new();
    for sha in shas {
        let deadline = Instant::now() + GIT_DEADLINE;
        if let Ok(summary) = crate::execution::git_exec::commit_summary(root, sha, deadline).await {
            if !summary.stat.trim().is_empty() {
                stats.push(summary.stat);
            }
            subjects.extend(summary.subjects);
        }
    }
    subjects.truncate(COMMIT_LINES);
    let diff_stat = (!stats.is_empty()).then(|| clip_head(&stats.join("\n"), DIFF_CHARS));
    let commits = (!subjects.is_empty()).then(|| clip_head(&subjects.join("\n"), COMMITS_CHARS));
    (diff_stat, commits)
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use sqlx::SqlitePool;

    use super::gather_blocks;
    use crate::tree_job::model::{Branch, Manifest};
    use crate::tree_job::report::{Report, render_handoff};
    use crate::tree_job::store::{self, TreeJob};

    const TREE: &str = "job-1";

    /// A tree whose project root is not a repository, so every git read fails; the temp dir keeps
    /// the handoff files.
    struct Fixture {
        job: TreeJob,
        _dir: tempfile::TempDir,
    }

    fn report(node: &str, status: &str, summary: &str, decisions: &[&str], open: &str, blocked: &str) -> Report {
        Report {
            node_id: node.to_owned(),
            root_id: format!("{TREE}-root"),
            status: status.to_owned(),
            role: "leaf".to_owned(),
            outcome: status.to_owned(),
            contract_used: Vec::new(),
            contract_used_raw: String::new(),
            summary: summary.to_owned(),
            done: Vec::new(),
            decisions: decisions.iter().map(|d| (*d).to_owned()).collect(),
            files: Vec::new(),
            commands: Vec::new(),
            open_items: open.to_owned(),
            contract_request: String::new(),
            blocked_reason: blocked.to_owned(),
            line_count: 0,
        }
    }

    async fn insert_node(
        pool: &SqlitePool,
        id: &str,
        status: &str,
        handoff: &std::path::Path,
        run_id: Option<i64>,
    ) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, parent_id, depth, kind, title, goal, status,
                                     summary, handoff_path, run_id, tree_job_id, created_at, updated_at)
             VALUES (?, ?, NULL, 1, 'leaf', 'Branch', 'goal', ?, 'row summary', ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(format!("{TREE}-root"))
        .bind(status)
        .bind(handoff.to_string_lossy().into_owned())
        .bind(run_id.map(|r| r.to_string()))
        .bind(TREE)
        .bind("2026-11-16T10:00:00+00:00")
        .bind("2026-11-16T10:00:00+00:00")
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_verify(pool: &SqlitePool, worktree: &str, status: &str, tail: &str) {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO verify_runs (project_id, worktree, scope, origin, requested_by, argv,
                                      status, duration_ms, started_at, finished_at, output_tail)
             VALUES ('alpha', ?, 'full', 'job', 'gate', '[]', ?, 10, ?, ?, ?)",
        )
        .bind(worktree)
        .bind(status)
        .bind(&now)
        .bind(&now)
        .bind(tail)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Two nodes (`n-api` done with open items and a run, `n-ui` blocked), an answered decision,
    /// failed verifications of both worktrees (one with ANSI colour), a passed one, and one live and
    /// one refused lesson.
    async fn fixture(pool: &SqlitePool) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();

        let mut manifest = Manifest::new(TREE, "feat/job-1", "Ship the cache", &root, &[]);
        manifest.state = "finished".to_owned();
        manifest.extra.insert(
            "land_sha".to_owned(),
            serde_json::json!("0123456789abcdef0123456789abcdef01234567"),
        );
        for (name, worktree, node) in [("api", "C:/wt/api", "n-api"), ("ui", "C:/wt/ui", "n-ui")] {
            let mut branch = Branch::new(name, &[], "leaf", None);
            branch.worktree = Some(worktree.to_owned());
            branch.node_id = Some(node.to_owned());
            manifest.branches.insert(name.to_owned(), branch);
        }
        let stamp = Utc::now().to_rfc3339();
        let job = TreeJob {
            id: TREE.to_owned(),
            root_node_id: format!("{TREE}-root"),
            project_id: Some("alpha".to_owned()),
            feature_branch: Some("feat/job-1".to_owned()),
            goal: "Ship the cache".to_owned(),
            state: "finished".to_owned(),
            manifest,
            contract: None,
            created_at: stamp.clone(),
            updated_at: stamp,
        };
        store::save(pool, &job).await.unwrap();

        let api_md = dir.path().join("n-api.md");
        std::fs::write(
            &api_md,
            render_handoff(&report(
                "n-api",
                "done",
                "Added the cache layer",
                &["Use LRU eviction"],
                "Wire the metrics",
                "",
            )),
        )
        .unwrap();
        let ui_md = dir.path().join("n-ui.md");
        std::fs::write(
            &ui_md,
            render_handoff(&report("n-ui", "blocked", "Started the panel", &[], "", "Waiting for the API key")),
        )
        .unwrap();

        let run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, stdout, created_at)
             VALUES ('work', 'completed', ?, ?)",
        )
        .bind("{\"type\":\"result\",\"result\":\"Shipped the cache layer\"}\n")
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        insert_node(pool, "n-api", "done", &api_md, Some(run_id)).await;
        insert_node(pool, "n-ui", "blocked", &ui_md, None).await;

        sqlx::query(
            "INSERT INTO decisions (id, tree_id, node_id, question, answer, origin, at, status)
             VALUES ('d1', ?, 'n-api', 'Which cache?', 'Pick LRU', 'telegram', ?, 'answered')",
        )
        .bind(TREE)
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap();

        insert_verify(pool, "C:/wt/api", "failed", "\u{1b}[31merror: gate failed on api\u{1b}[0m\nmore output").await;
        insert_verify(pool, "C:/wt/ui", "failed", "error: ui lint failed").await;
        insert_verify(pool, "C:/wt/api", "passed", "all green").await;

        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', 'project', 'alpha', 'distiller', 'memory', 'Known cache rule', 'b', 'active', ?)",
        )
        .bind(&now)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at, ended_at)
             VALUES ('semantic', 'project', 'alpha', 'distiller', 'memory', 'Refused cache idea', 'b', 'rejected', ?, ?)",
        )
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await
        .unwrap();

        Fixture { job, _dir: dir }
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[tokio::test]
    async fn gather_blocks_reads_reports_decisions_failures_and_final_message() {
        let pool = crate::testdb::fresh_pool().await;
        let fx = fixture(&pool).await;
        let blocks = gather_blocks(&pool, &fx.job, None, now()).await.unwrap();

        assert_eq!(blocks.node_reports.len(), 2, "{:?}", blocks.node_reports);
        let api = blocks.node_reports.iter().find(|n| n.node_id == "n-api").expect("n-api report");
        assert_eq!(api.status, "done");
        assert!(api.text.contains("Added the cache layer"), "{}", api.text);
        assert!(api.text.contains("Use LRU eviction"), "{}", api.text);
        assert!(api.has_open_items);
        let ui = blocks.node_reports.iter().find(|n| n.node_id == "n-ui").expect("n-ui report");
        assert_eq!(ui.status, "blocked");
        assert!(ui.text.contains("Waiting for the API key"), "{}", ui.text);
        assert!(!ui.has_open_items);
        assert_eq!(blocks.bad_nodes, 1);
        assert_eq!(blocks.open_item_nodes, 1);

        assert_eq!(blocks.decisions.len(), 1, "{:?}", blocks.decisions);
        assert!(blocks.decisions[0].contains("Which cache?"));
        assert!(blocks.decisions[0].contains("Pick LRU"));

        // Failed verifications only, of the branches' worktrees; the passed one is not a failure.
        assert_eq!(blocks.red_failures.len(), 2, "{:?}", blocks.red_failures);
        assert!(blocks.red_failures.iter().any(|(_, t)| t.contains("gate failed on api")));
        assert!(blocks.red_failures.iter().any(|(_, t)| t.contains("ui lint failed")));
        assert!(!blocks.red_failures.iter().any(|(_, t)| t.contains("all green")));

        assert_eq!(blocks.agent_final.len(), 1, "{:?}", blocks.agent_final);
        assert!(blocks.agent_final[0].1.contains("Shipped the cache layer"));

        assert_eq!(blocks.known_titles, vec!["Known cache rule".to_owned()]);
        assert_eq!(blocks.refused, vec!["Refused cache idea".to_owned()]);
    }

    #[tokio::test]
    async fn gather_blocks_strips_ansi_from_failures() {
        let pool = crate::testdb::fresh_pool().await;
        let fx = fixture(&pool).await;
        let blocks = gather_blocks(&pool, &fx.job, None, now()).await.unwrap();

        let api = blocks
            .red_failures
            .iter()
            .find(|(_, t)| t.contains("gate failed on api"))
            .expect("the api failure");
        assert!(!api.1.contains('\u{1b}'), "{:?}", api.1);
        assert!(blocks.red_failures.iter().all(|(_, t)| !t.contains('\u{1b}')));
    }

    #[tokio::test]
    async fn gather_blocks_without_a_git_repo_leaves_diff_and_commits_out() {
        let pool = crate::testdb::fresh_pool().await;
        let fx = fixture(&pool).await;
        // The manifest names a `land_sha`, but its project root is a plain directory.
        let blocks = gather_blocks(&pool, &fx.job, None, now()).await.unwrap();

        assert!(blocks.diff_stat.is_none(), "{:?}", blocks.diff_stat);
        assert!(blocks.commits.is_none(), "{:?}", blocks.commits);
        // The rest of the blocks still came out.
        assert_eq!(blocks.node_reports.len(), 2);
        assert_eq!(blocks.decisions.len(), 1);
    }

    #[tokio::test]
    async fn gather_blocks_focus_node_limits_the_reports() {
        let pool = crate::testdb::fresh_pool().await;
        let fx = fixture(&pool).await;
        let blocks = gather_blocks(&pool, &fx.job, Some("n-api"), now()).await.unwrap();

        assert_eq!(blocks.node_reports.len(), 1, "{:?}", blocks.node_reports);
        assert_eq!(blocks.node_reports[0].node_id, "n-api");
        // Only the failures of that node's branch worktree.
        assert_eq!(blocks.red_failures.len(), 1, "{:?}", blocks.red_failures);
        assert!(blocks.red_failures[0].1.contains("gate failed on api"));
        assert!(!blocks.red_failures.iter().any(|(_, t)| t.contains("ui lint failed")));
        // The counts stay over the whole tree.
        assert_eq!(blocks.bad_nodes, 1);
        assert_eq!(blocks.open_item_nodes, 1);
    }
}
