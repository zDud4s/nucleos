//! `run_launch`: an `EdgeSource` over "who launched this" columns (spec 2026-10-20 §4, item 5).
//!
//! - `runs.launched_by_chat_id` (migration 0195) links a run launched from a chat turn to that chat.
//!   A chat has no node of its own: its turns are `runs` rows (`mode='assistant'`, `chat_id`) that
//!   `daemon_runs` sights as ordinary runs, usually all under one session key. The stored value is
//!   only the chat id, so the parent is the chat's latest assistant turn with a smaller run id than
//!   the launched run (turns are serial; the launch happens inside the speaking turn). If no source
//!   sights that turn, fusion drops the edge (contract rule 3).
//! - `chat_tasks.launched_by_run_id` (migration 0168) links a task a chat turn spawned to the run
//!   of that turn. `chat_tasks.task_id` is the Claude `agentId` (`agent-<task_id>.jsonl`), so the
//!   child is the Claude Code subagent node. `background_bash` tasks are shell commands, not
//!   agents, and rows without a `task_id` have nothing to link: both are skipped.
//!
//! A run is keyed as `work_nodes_edges::run_keys` does: every candidate key `daemon_runs` could
//! have sighted it under, because the runner→label mapping lives only in `daemon_runs`.

use std::collections::HashSet;

use async_trait::async_trait;
use sqlx::SqlitePool;

use super::model::{NodeKey, Timestamp};
use super::source::{Edge, EdgeSource, SourceResult};
use super::work_nodes_edges::{MAX_EDGE_ROWS, coarse_bound, in_window, run_keys};

/// The harness label of a Claude Code subagent node, keyed by the bare `agentId` (= `chat_tasks.task_id`;
/// contract amendment 5).
const CLAUDE_HARNESS: &str = "claude-code";

fn task_key(task_id: &str) -> NodeKey {
    NodeKey::new(CLAUDE_HARNESS, task_id)
}

/// A run tagged with the chat that launched it, plus that chat's speaking turn.
#[derive(sqlx::FromRow)]
struct LaunchRow {
    id: i64,
    session_id: Option<String>,
    status: String,
    created_at: String,
    completed_at: Option<String>,
    turn_id: Option<i64>,
    turn_session: Option<String>,
}

/// A chat task and the session of the run that launched it, joined in one query (contract amendment 12).
#[derive(sqlx::FromRow)]
struct TaskRow {
    launched_by_run_id: i64,
    /// `runs.session_id` of that run (NULL when the run is gone or has no session).
    run_session: Option<String>,
    task_id: String,
    status: String,
    started_at: String,
    finished_at: Option<String>,
}

/// `EdgeSource` over `runs.launched_by_chat_id` and `chat_tasks.launched_by_run_id`.
pub struct RunLaunchEdges {
    pool: SqlitePool,
}

impl RunLaunchEdges {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    async fn run_edges(&self, since: Timestamp, out: &mut Vec<Edge>, seen: &mut HashSet<(NodeKey, NodeKey)>) -> SourceResult<()> {
        let rows: Vec<LaunchRow> = sqlx::query_as(
            "SELECT r.id AS id, r.session_id AS session_id, r.status AS status, r.created_at AS created_at,
                    r.completed_at AS completed_at, t.id AS turn_id, t.session_id AS turn_session
               FROM runs r
               LEFT JOIN runs t ON t.id = (
                    SELECT MAX(t2.id) FROM runs t2
                     WHERE t2.chat_id = r.launched_by_chat_id AND t2.mode = 'assistant' AND t2.id < r.id)
              WHERE r.launched_by_chat_id IS NOT NULL
                AND (r.status = 'running' OR COALESCE(r.completed_at, r.created_at) >= ?)
              ORDER BY r.id DESC
              LIMIT ?",
        )
        .bind(coarse_bound(since))
        .bind(MAX_EDGE_ROWS)
        .fetch_all(&self.pool)
        .await?;
        for row in rows {
            let stamp = row.completed_at.as_deref().unwrap_or(&row.created_at);
            if row.status != "running" && !in_window(stamp, since) {
                continue;
            }
            let Some(turn) = row.turn_id else { continue };
            push_pairs(
                out,
                seen,
                &run_keys(row.id, row.session_id.as_deref()),
                &run_keys(turn, row.turn_session.as_deref()),
            );
        }
        Ok(())
    }

    async fn task_edges(&self, since: Timestamp, out: &mut Vec<Edge>, seen: &mut HashSet<(NodeKey, NodeKey)>) -> SourceResult<()> {
        let rows: Vec<TaskRow> = sqlx::query_as(
            "SELECT ct.launched_by_run_id AS launched_by_run_id, r.session_id AS run_session,
                    ct.task_id AS task_id, ct.status AS status, ct.started_at AS started_at,
                    ct.finished_at AS finished_at
               FROM chat_tasks ct
               LEFT JOIN runs r ON r.id = ct.launched_by_run_id
              WHERE ct.kind <> 'background_bash' AND ct.task_id IS NOT NULL AND ct.task_id <> ''
                AND (ct.status = 'running' OR COALESCE(ct.finished_at, ct.started_at) >= ?)
              ORDER BY ct.id DESC
              LIMIT ?",
        )
        .bind(coarse_bound(since))
        .bind(MAX_EDGE_ROWS)
        .fetch_all(&self.pool)
        .await?;
        for row in rows {
            let stamp = row.finished_at.as_deref().unwrap_or(&row.started_at);
            if row.status != "running" && !in_window(stamp, since) {
                continue;
            }
            let parents = run_keys(row.launched_by_run_id, row.run_session.as_deref());
            push_pairs(out, seen, &[task_key(&row.task_id)], &parents);
        }
        Ok(())
    }
}

/// Every child x parent pair not yet emitted, never a node under itself (equal native ids are one
/// session seen through two candidate labels).
fn push_pairs(out: &mut Vec<Edge>, seen: &mut HashSet<(NodeKey, NodeKey)>, children: &[NodeKey], parents: &[NodeKey]) {
    for child in children {
        for parent in parents {
            if child != parent && child.native_id != parent.native_id && seen.insert((child.clone(), parent.clone())) {
                out.push(Edge { child: child.clone(), parent: parent.clone() });
            }
        }
    }
}

#[async_trait]
impl EdgeSource for RunLaunchEdges {
    fn name(&self) -> &'static str {
        "run_launch"
    }

    async fn edges(&self, since: Timestamp) -> SourceResult<Vec<Edge>> {
        let mut edges = Vec::new();
        let mut seen = HashSet::new();
        self.run_edges(since, &mut edges, &mut seen).await?;
        self.task_edges(since, &mut edges, &mut seen).await?;
        Ok(edges)
    }
}

#[cfg(test)]
mod tests {
    // `Duration` and `Utc` are test-only here (the code above never touches the clock).
    use super::*;
    use chrono::{Duration, Utc};

    fn since_10_min() -> Timestamp {
        Utc::now() - Duration::minutes(10)
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_run(
        pool: &SqlitePool,
        mode: &str,
        chat: Option<&str>,
        launched_by_chat: Option<&str>,
        session: Option<&str>,
        status: &str,
        created_at: &str,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at, session_id, chat_id, launched_by_chat_id)
             VALUES ('p', ?, ?, ?, ?, ?, ?)",
        )
        .bind(status)
        .bind(mode)
        .bind(created_at)
        .bind(session)
        .bind(chat)
        .bind(launched_by_chat)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn insert_task(pool: &SqlitePool, run: i64, task_id: Option<&str>, kind: &str, status: &str, started: &str) {
        sqlx::query(
            "INSERT INTO chat_tasks (chat_id, launched_by_run_id, tool_use_id, task_id, kind, status, started_at)
             VALUES ('chat-1', ?, ?, ?, ?, ?, ?)",
        )
        .bind(run)
        .bind(format!("toolu_{}", task_id.unwrap_or("none")))
        .bind(task_id)
        .bind(kind)
        .bind(status)
        .bind(started)
        .execute(pool)
        .await
        .unwrap();
    }

    fn ids(edges: &[Edge]) -> Vec<(String, String)> {
        edges.iter().map(|e| (e.child.id(), e.parent.id())).collect()
    }

    #[tokio::test]
    async fn a_run_launched_from_a_chat_points_at_the_turn_that_was_speaking() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now().to_rfc3339();
        insert_run(&pool, "assistant", Some("chat-1"), None, Some("turn-1-session"), "completed", &now).await;
        insert_run(&pool, "real", None, Some("chat-1"), Some("job-session"), "running", &now).await;
        // A turn that came after the launch must not become its parent.
        insert_run(&pool, "assistant", Some("chat-1"), None, Some("turn-2-session"), "running", &now).await;
        let edges = RunLaunchEdges::new(pool).edges(since_10_min()).await.unwrap();
        let got = ids(&edges);
        assert!(got.contains(&("claude-code:job-session".into(), "claude-code:turn-1-session".into())));
        assert!(got.contains(&("codex-cli:job-session".into(), "codex-cli:turn-1-session".into())));
        assert!(!got.iter().any(|(_, parent)| parent.contains("turn-2-session")));
    }

    /// A codex run launched by a chat: the run's `session_id` is the rollout's thread id, so the edge
    /// must end on the very key `codex_cli` sights for that rollout (`sighting.key`).
    #[tokio::test]
    async fn a_codex_run_launched_by_a_chat_has_an_edge_on_the_key_codex_cli_sights() {
        use crate::fleet::codex_cli::{write_minimal_rollout, CodexCli};
        use crate::fleet::source::AgentSource;

        const CODEX_ID: &str = "01a14120-24a4-7bc3-9946-a787ff71b89f";
        let home = tempfile::tempdir().unwrap();
        write_minimal_rollout(home.path(), CODEX_ID);
        let sightings = CodexCli::new(home.path().to_path_buf()).discover(since_10_min()).await.unwrap();
        assert_eq!(sightings.len(), 1, "the rollout is discovered");

        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now().to_rfc3339();
        insert_run(&pool, "assistant", Some("chat-1"), None, Some("turn-session"), "completed", &now).await;
        insert_run(&pool, "real", None, Some("chat-1"), Some(CODEX_ID), "running", &now).await;
        let edges = RunLaunchEdges::new(pool).edges(since_10_min()).await.unwrap();
        assert!(
            edges.contains(&Edge { child: sightings[0].key.clone(), parent: NodeKey::new("claude-code", "turn-session") }),
            "no edge ends on {:?}: {edges:?}",
            sightings[0].key
        );
    }

    #[tokio::test]
    async fn a_launched_run_without_a_session_is_a_daemon_node() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now().to_rfc3339();
        insert_run(&pool, "assistant", Some("chat-1"), None, None, "completed", &now).await;
        let launched = insert_run(&pool, "real", None, Some("chat-1"), None, "running", &now).await;
        let got = ids(&RunLaunchEdges::new(pool).edges(since_10_min()).await.unwrap());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, format!("daemon:run-{launched}"));
        assert!(got[0].1.starts_with("daemon:run-"));
    }

    #[tokio::test]
    async fn untagged_runs_and_chats_with_no_earlier_turn_make_no_edges() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now().to_rfc3339();
        insert_run(&pool, "real", None, None, Some("plain"), "running", &now).await;
        // Tagged with a chat whose turn rows are gone.
        insert_run(&pool, "real", None, Some("ghost-chat"), Some("orphan"), "running", &now).await;
        assert!(RunLaunchEdges::new(pool).edges(since_10_min()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_window_keeps_running_runs_and_drops_old_finished_ones() {
        let pool = crate::testdb::fresh_pool().await;
        let old = "2026-01-01T00:00:00Z";
        let now = Utc::now().to_rfc3339();
        insert_run(&pool, "assistant", Some("chat-1"), None, Some("turn"), "completed", old).await;
        insert_run(&pool, "real", None, Some("chat-1"), Some("still-running"), "running", old).await;
        insert_run(&pool, "real", None, Some("chat-1"), Some("long-done"), "completed", old).await;
        insert_run(&pool, "real", None, Some("chat-1"), Some("fresh"), "completed", &now).await;
        let got = ids(&RunLaunchEdges::new(pool).edges(since_10_min()).await.unwrap());
        assert!(got.iter().any(|(child, _)| child == "claude-code:still-running"));
        assert!(got.iter().any(|(child, _)| child == "claude-code:fresh"));
        assert!(!got.iter().any(|(child, _)| child.contains("long-done")));
    }

    #[tokio::test]
    async fn a_chat_task_is_a_claude_subagent_under_the_run_that_launched_it() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now().to_rfc3339();
        let turn = insert_run(&pool, "assistant", Some("chat-1"), None, Some("turn-session"), "running", &now).await;
        insert_task(&pool, turn, Some("a1b2c3d4e5f60718"), "background_agent", "running", &now).await;
        insert_task(&pool, turn, Some("b1"), "background_bash", "running", &now).await;
        insert_task(&pool, turn, None, "subagent", "completed", &now).await;
        insert_task(&pool, turn, Some("old-agent"), "background_agent", "completed", "2026-01-01T00:00:00.000Z").await;
        let got = ids(&RunLaunchEdges::new(pool).edges(since_10_min()).await.unwrap());
        let tasks: Vec<_> = got.iter().filter(|(child, _)| child.starts_with("claude-code:a1")).collect();
        assert_eq!(tasks.len(), 3, "one edge per candidate key of the launching run");
        assert!(got.contains(&("claude-code:a1b2c3d4e5f60718".into(), "claude-code:turn-session".into())));
        assert!(got.contains(&("claude-code:a1b2c3d4e5f60718".into(), format!("daemon:run-{turn}"))));
        assert!(!got.iter().any(|(child, _)| child == "claude-code:b1" || child.contains("old-agent")));
    }
}
