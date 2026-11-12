//! `work_nodes`: an `EdgeSource` over the `work_nodes` table (migration 0216; spec 2026-10-20 §4,
//! item 4). A work node's `parent_id` says "this unit of delegated work belongs under that one";
//! the fleet tree gets an edge when both ends resolve to nodes some source sights, even when they
//! are two processes of two harnesses with nothing in common on disk.
//!
//! **How a work node becomes a `NodeKey`.** The table has no harness column, and `POST /nodes`
//! only records `session_id`/`chat_id` (the route sets `run_id` to `None`; the column and
//! `OpenArgs.run_id` exist). So a bare session id could belong to Claude Code or to Codex. The
//! rule, kept free of heuristics:
//!
//! - `session_id` s: candidates `claude-code:s` and `codex-cli:s` (a uuid is sighted under at most one).
//! - `run_id` n: the run's `runs.session_id` gives the same two candidates, plus `daemon:run-n`
//!   (the key `daemon_runs` uses for a run without a native adapter or session id).
//! - neither (both blank), but `chat_id` c: the chat's latest `mode='assistant'` turn at or before
//!   the node's creation, resolved as a run.
//!
//! Every child candidate is paired with every parent candidate; fusion drops the pairs whose ends
//! no source sights (contract rule 3). Pairs where both ends are the same node are not emitted
//! (an orchestrator's children usually run in its own session).
//!
//! **One query per pass** (contract amendment 12): the run sessions and the chat turns are read by
//! `LEFT JOIN`s in the query that reads the node pairs, never by a query per row.

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::{DateTime, Duration};
use sqlx::SqlitePool;

use super::model::{NodeKey, Timestamp};
use super::source::{Edge, EdgeSource, SourceResult};

/// The native harnesses a bare session id may belong to (contract §2 labels).
const SESSION_HARNESSES: [&str; 2] = ["claude-code", "codex-cli"];
/// The label `daemon_runs` gives runs that have no native adapter or no session id.
const DAEMON_HARNESS: &str = "daemon";
/// The most rows one edge query reads. The table is a bounded tree per task; this is a ceiling
/// against a runaway table, not a page size.
pub(super) const MAX_EDGE_ROWS: i64 = 2000;

pub(super) fn session_keys(session_id: &str) -> Vec<NodeKey> {
    SESSION_HARNESSES
        .iter()
        .map(|harness| NodeKey::new(harness, session_id))
        .collect()
}

/// Every key `daemon_runs` could have sighted run `run_id` under, given its `runs.session_id`.
pub(super) fn run_keys(run_id: i64, session_id: Option<&str>) -> Vec<NodeKey> {
    let mut keys = session_id
        .map(str::trim)
        .filter(|session| !session.is_empty())
        .map(session_keys)
        .unwrap_or_default();
    keys.push(NodeKey::new(DAEMON_HARNESS, &format!("run-{run_id}")));
    keys
}

/// Whether a stored RFC 3339 stamp (the house text format; `Z` and `+00:00` both occur) is at or
/// after `since`. An unparseable stamp counts as inside: dropping an edge over a bad date would
/// hide a real parent.
pub(super) fn in_window(stamp: &str, since: Timestamp) -> bool {
    DateTime::parse_from_rfc3339(stamp)
        .map(|at| at >= since)
        .unwrap_or(true)
}

/// A lower bound for SQL text comparison, one hour before `since`: text order is exact only up to
/// the sub-second/offset suffix, so SQL over-selects and `in_window` decides.
pub(super) fn coarse_bound(since: Timestamp) -> String {
    (since - Duration::hours(1)).to_rfc3339()
}

fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|text| !text.is_empty())
}

fn dedup_keys(keys: &mut Vec<NodeKey>) {
    let mut seen = HashSet::new();
    keys.retain(|key| seen.insert(key.clone()));
}

/// A work node and its parent, side by side (one row per child that has a parent), with the run
/// sessions and chat turns of both ends already joined in (contract amendment 12).
#[derive(sqlx::FromRow)]
struct PairRow {
    status: String,
    updated_at: String,
    c_session: Option<String>,
    c_run: Option<String>,
    /// `runs.session_id` of the run named by `c_run` (NULL when blank, unparseable or no such run).
    c_run_session: Option<String>,
    /// The chat's speaking turn, only looked up when the node has neither session nor run.
    c_turn: Option<i64>,
    c_turn_session: Option<String>,
    p_session: Option<String>,
    p_run: Option<String>,
    p_run_session: Option<String>,
    p_turn: Option<i64>,
    p_turn_session: Option<String>,
}

/// One end of an edge as the query read it.
struct End<'a> {
    session: Option<&'a str>,
    run: Option<&'a str>,
    run_session: Option<&'a str>,
    turn: Option<i64>,
    turn_session: Option<&'a str>,
}

/// Candidate keys for one end of an edge (see the module doc). Pure: every database read already
/// happened in the pair query.
fn node_keys(end: End<'_>) -> Vec<NodeKey> {
    let mut keys = Vec::new();
    if let Some(session) = present(end.session) {
        keys.extend(session_keys(session));
    }
    if let Some(run_id) = present(end.run).and_then(|text| text.parse::<i64>().ok()) {
        keys.extend(run_keys(run_id, end.run_session));
    }
    // `turn` is NULL unless the node had neither session nor run, so this only fires for chat-only nodes.
    if keys.is_empty()
        && let Some(turn) = end.turn
    {
        keys.extend(run_keys(turn, end.turn_session));
    }
    dedup_keys(&mut keys);
    keys
}

impl PairRow {
    fn child_keys(&self) -> Vec<NodeKey> {
        node_keys(End {
            session: self.c_session.as_deref(),
            run: self.c_run.as_deref(),
            run_session: self.c_run_session.as_deref(),
            turn: self.c_turn,
            turn_session: self.c_turn_session.as_deref(),
        })
    }

    fn parent_keys(&self) -> Vec<NodeKey> {
        node_keys(End {
            session: self.p_session.as_deref(),
            run: self.p_run.as_deref(),
            run_session: self.p_run_session.as_deref(),
            turn: self.p_turn,
            turn_session: self.p_turn_session.as_deref(),
        })
    }
}

/// `EdgeSource` over `work_nodes.parent_id`. One query per pass: the run sessions and chat turns of
/// both ends are `LEFT JOIN`ed (the `MAX(id)` turn subquery is served by `runs_assistant_by_chat`), so
/// the cost does not grow with the number of rows beyond the `MAX_EDGE_ROWS` ceiling.
pub struct WorkNodesEdges {
    pool: SqlitePool,
}

impl WorkNodesEdges {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl EdgeSource for WorkNodesEdges {
    fn name(&self) -> &'static str {
        "work_nodes"
    }

    /// Edges for children that are still `planned`/`running` or were touched at or after `since`.
    async fn edges(&self, since: Timestamp) -> SourceResult<Vec<Edge>> {
        let rows: Vec<PairRow> = sqlx::query_as(
            "SELECT c.status AS status, c.updated_at AS updated_at,
                    c.session_id AS c_session, c.run_id AS c_run, cr.session_id AS c_run_session,
                    ct.id AS c_turn, ct.session_id AS c_turn_session,
                    p.session_id AS p_session, p.run_id AS p_run, pr.session_id AS p_run_session,
                    pt.id AS p_turn, pt.session_id AS p_turn_session
               FROM work_nodes c
               JOIN work_nodes p ON p.id = c.parent_id
               LEFT JOIN runs cr ON cr.id = CAST(NULLIF(TRIM(c.run_id), '') AS INTEGER)
               LEFT JOIN runs pr ON pr.id = CAST(NULLIF(TRIM(p.run_id), '') AS INTEGER)
               LEFT JOIN runs ct ON ct.id = (
                    SELECT MAX(t.id) FROM runs t
                     WHERE t.chat_id = c.chat_id AND t.mode = 'assistant' AND t.created_at <= c.created_at
                       AND COALESCE(TRIM(c.session_id), '') = '' AND COALESCE(TRIM(c.run_id), '') = '')
               LEFT JOIN runs pt ON pt.id = (
                    SELECT MAX(t.id) FROM runs t
                     WHERE t.chat_id = p.chat_id AND t.mode = 'assistant' AND t.created_at <= p.created_at
                       AND COALESCE(TRIM(p.session_id), '') = '' AND COALESCE(TRIM(p.run_id), '') = '')
              WHERE c.status IN ('planned', 'running') OR c.updated_at >= ?
              ORDER BY c.created_at DESC
              LIMIT ?",
        )
        .bind(coarse_bound(since))
        .bind(MAX_EDGE_ROWS)
        .fetch_all(&self.pool)
        .await?;

        let mut edges = Vec::new();
        let mut seen = HashSet::new();
        for row in rows {
            let open = matches!(row.status.as_str(), "planned" | "running");
            if !open && !in_window(&row.updated_at, since) {
                continue;
            }
            let (children, parents) = (row.child_keys(), row.parent_keys());
            for child in &children {
                for parent in &parents {
                    if child != parent
                        && child.native_id != parent.native_id
                        && seen.insert((child.clone(), parent.clone()))
                    {
                        edges.push(Edge {
                            child: child.clone(),
                            parent: parent.clone(),
                        });
                    }
                }
            }
        }
        Ok(edges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn labels(keys: &[NodeKey]) -> Vec<String> {
        keys.iter().map(NodeKey::id).collect()
    }

    #[test]
    fn a_bare_session_id_is_a_candidate_for_every_native_harness() {
        assert_eq!(
            labels(&session_keys("s1")),
            ["claude-code:s1", "codex-cli:s1"]
        );
    }

    #[test]
    fn a_run_is_its_session_candidates_plus_the_daemon_run_key() {
        assert_eq!(
            labels(&run_keys(7, Some("s1"))),
            ["claude-code:s1", "codex-cli:s1", "daemon:run-7"]
        );
        assert_eq!(labels(&run_keys(7, None)), ["daemon:run-7"]);
        assert_eq!(labels(&run_keys(7, Some(""))), ["daemon:run-7"]);
    }

    #[test]
    fn the_window_keeps_unparseable_timestamps_and_drops_old_ones() {
        let since: Timestamp = "2026-10-20T10:00:00Z".parse().unwrap();
        assert!(in_window("2026-10-20T10:00:00.5+00:00", since));
        assert!(in_window("2026-10-20T11:00:00Z", since));
        assert!(!in_window("2026-10-20T09:59:59Z", since));
        assert!(
            in_window("garbage", since),
            "a bad stamp degrades to kept, never to lost edges"
        );
    }

    #[test]
    fn the_coarse_sql_bound_is_an_hour_before_since() {
        let since: Timestamp = "2026-10-20T10:00:00Z".parse().unwrap();
        assert!(coarse_bound(since).starts_with("2026-10-20T09:00:00"));
    }

    struct NodeSpec<'a> {
        id: &'a str,
        parent: Option<&'a str>,
        status: &'a str,
        session: Option<&'a str>,
        run: Option<&'a str>,
        chat: Option<&'a str>,
        stamp: &'a str,
    }

    impl<'a> NodeSpec<'a> {
        fn new(id: &'a str) -> Self {
            Self {
                id,
                parent: None,
                status: "running",
                session: None,
                run: None,
                chat: None,
                stamp: "",
            }
        }
    }

    async fn insert(pool: &SqlitePool, spec: NodeSpec<'_>) {
        let now = Utc::now().to_rfc3339();
        let stamp = if spec.stamp.is_empty() {
            now.as_str()
        } else {
            spec.stamp
        };
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, parent_id, depth, kind, title, goal, status,
                                     session_id, run_id, chat_id, created_at, updated_at)
             VALUES (?, 'root', ?, ?, 'leaf', 't', 'g', ?, ?, ?, ?, ?, ?)",
        )
        .bind(spec.id)
        .bind(spec.parent)
        .bind(i64::from(spec.parent.is_some()))
        .bind(spec.status)
        .bind(spec.session)
        .bind(spec.run)
        .bind(spec.chat)
        .bind(stamp)
        .bind(stamp)
        .execute(pool)
        .await
        .unwrap();
    }

    fn since_10_min() -> Timestamp {
        Utc::now() - Duration::minutes(10)
    }

    fn pair(child: &str, parent: &str) -> Edge {
        let split = |id: &str| {
            let (harness, native) = id.split_once(':').unwrap();
            NodeKey::new(harness, native)
        };
        Edge {
            child: split(child),
            parent: split(parent),
        }
    }

    #[tokio::test]
    async fn a_child_in_another_harness_session_gets_a_cross_harness_edge() {
        let pool = crate::testdb::fresh_pool().await;
        insert(
            &pool,
            NodeSpec {
                session: Some("claude-sess"),
                ..NodeSpec::new("root")
            },
        )
        .await;
        insert(
            &pool,
            NodeSpec {
                parent: Some("root"),
                session: Some("codex-sess"),
                ..NodeSpec::new("leaf")
            },
        )
        .await;
        let edges = WorkNodesEdges::new(pool)
            .edges(since_10_min())
            .await
            .unwrap();
        // 2 child candidates x 2 parent candidates; fusion keeps the one both ends of which are sighted.
        assert_eq!(edges.len(), 4);
        assert!(edges.contains(&pair("codex-cli:codex-sess", "claude-code:claude-sess")));
        assert!(edges.contains(&pair("claude-code:codex-sess", "codex-cli:claude-sess")));
    }

    #[tokio::test]
    async fn a_child_sharing_its_parents_session_adds_no_self_edge() {
        let pool = crate::testdb::fresh_pool().await;
        insert(
            &pool,
            NodeSpec {
                session: Some("s"),
                ..NodeSpec::new("root")
            },
        )
        .await;
        insert(
            &pool,
            NodeSpec {
                parent: Some("root"),
                session: Some("s"),
                ..NodeSpec::new("leaf")
            },
        )
        .await;
        assert!(
            WorkNodesEdges::new(pool)
                .edges(since_10_min())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn roots_and_nodes_with_no_identity_make_no_edges() {
        let pool = crate::testdb::fresh_pool().await;
        insert(
            &pool,
            NodeSpec {
                session: Some("s"),
                ..NodeSpec::new("root")
            },
        )
        .await;
        insert(
            &pool,
            NodeSpec {
                parent: Some("root"),
                ..NodeSpec::new("anonymous")
            },
        )
        .await;
        insert(
            &pool,
            NodeSpec {
                parent: Some("root"),
                session: Some("  "),
                ..NodeSpec::new("blank")
            },
        )
        .await;
        assert!(
            WorkNodesEdges::new(pool)
                .edges(since_10_min())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_run_id_resolves_through_the_run_and_a_sessionless_run_is_a_daemon_node() {
        let pool = crate::testdb::fresh_pool().await;
        let with_session = sqlx::query("INSERT INTO runs (prompt, status, mode, created_at, session_id) VALUES ('p','running','real',?, 'cs')")
            .bind(Utc::now().to_rfc3339()).execute(&pool).await.unwrap().last_insert_rowid();
        let sessionless = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at) VALUES ('p','running','real',?)",
        )
        .bind(Utc::now().to_rfc3339())
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        insert(
            &pool,
            NodeSpec {
                session: Some("parent-s"),
                ..NodeSpec::new("root")
            },
        )
        .await;
        let a = with_session.to_string();
        let b = sessionless.to_string();
        insert(
            &pool,
            NodeSpec {
                parent: Some("root"),
                run: Some(a.as_str()),
                ..NodeSpec::new("a")
            },
        )
        .await;
        insert(
            &pool,
            NodeSpec {
                parent: Some("root"),
                run: Some(b.as_str()),
                ..NodeSpec::new("b")
            },
        )
        .await;
        let edges = WorkNodesEdges::new(pool)
            .edges(since_10_min())
            .await
            .unwrap();
        assert!(edges.contains(&pair("claude-code:cs", "claude-code:parent-s")));
        assert!(edges.contains(&pair("codex-cli:cs", "codex-cli:parent-s")));
        assert!(edges.contains(&pair(
            &format!("daemon:run-{sessionless}"),
            "claude-code:parent-s"
        )));
    }

    #[tokio::test]
    async fn a_chat_only_node_resolves_to_the_chats_latest_turn_before_it() {
        let pool = crate::testdb::fresh_pool().await;
        let turn = |at: &'static str, session: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query("INSERT INTO runs (prompt, status, mode, created_at, session_id, chat_id) VALUES ('p','completed','assistant',?,?, 'chat-1')")
                    .bind(at).bind(session).execute(&pool).await.unwrap().last_insert_rowid()
            }
        };
        turn("2026-10-20T10:00:00+00:00", "old-session").await;
        turn("2026-10-20T10:05:00+00:00", "speaking-session").await;
        turn("2026-10-20T10:30:00+00:00", "later-session").await;
        let stamp = "2026-10-20T10:10:00+00:00";
        insert(
            &pool,
            NodeSpec {
                chat: Some("chat-1"),
                stamp,
                ..NodeSpec::new("root")
            },
        )
        .await;
        insert(
            &pool,
            NodeSpec {
                parent: Some("root"),
                session: Some("kid"),
                stamp,
                ..NodeSpec::new("leaf")
            },
        )
        .await;
        let since: Timestamp = "2026-10-20T09:00:00Z".parse().unwrap();
        let edges = WorkNodesEdges::new(pool).edges(since).await.unwrap();
        assert!(edges.contains(&pair("claude-code:kid", "claude-code:speaking-session")));
        assert!(
            !edges
                .iter()
                .any(|e| e.parent.native_id == "later-session"
                    || e.parent.native_id == "old-session")
        );
    }

    #[tokio::test]
    async fn the_retention_window_keeps_open_nodes_and_drops_old_closed_ones() {
        let pool = crate::testdb::fresh_pool().await;
        let old = "2026-01-01T00:00:00+00:00";
        insert(
            &pool,
            NodeSpec {
                session: Some("p"),
                stamp: old,
                ..NodeSpec::new("root")
            },
        )
        .await;
        insert(
            &pool,
            NodeSpec {
                parent: Some("root"),
                session: Some("open-kid"),
                stamp: old,
                ..NodeSpec::new("open")
            },
        )
        .await;
        insert(
            &pool,
            NodeSpec {
                parent: Some("root"),
                session: Some("done-kid"),
                status: "done",
                stamp: old,
                ..NodeSpec::new("closed")
            },
        )
        .await;
        let edges = WorkNodesEdges::new(pool)
            .edges(since_10_min())
            .await
            .unwrap();
        assert!(
            edges.iter().any(|e| e.child.native_id == "open-kid"),
            "a running node outlives the window"
        );
        assert!(!edges.iter().any(|e| e.child.native_id == "done-kid"));
    }

    /// The edge must end on the very key `codex_cli` sights for the same session: a rollout is
    /// written, `CodexCli::discover` sights it, and a work node whose `session_id` is the rollout's
    /// id (parent: a Claude Code session) must yield an edge whose `child` equals `sighting.key`.
    #[tokio::test]
    async fn an_edge_child_is_the_key_codex_cli_sights_for_that_session() {
        use crate::fleet::codex_cli::{CodexCli, write_minimal_rollout};
        use crate::fleet::source::AgentSource;

        const CODEX_ID: &str = "01a14120-24a4-7bc3-9946-a787ff71b89f";
        let home = tempfile::tempdir().unwrap();
        write_minimal_rollout(home.path(), CODEX_ID);
        let sightings = CodexCli::new(home.path().to_path_buf())
            .discover(since_10_min())
            .await
            .unwrap();
        assert_eq!(sightings.len(), 1, "the rollout is discovered");

        let pool = crate::testdb::fresh_pool().await;
        insert(
            &pool,
            NodeSpec {
                session: Some("claude-sess"),
                ..NodeSpec::new("root")
            },
        )
        .await;
        insert(
            &pool,
            NodeSpec {
                parent: Some("root"),
                session: Some(CODEX_ID),
                ..NodeSpec::new("leaf")
            },
        )
        .await;
        let edges = WorkNodesEdges::new(pool)
            .edges(since_10_min())
            .await
            .unwrap();
        assert!(
            edges.contains(&Edge {
                child: sightings[0].key.clone(),
                parent: NodeKey::new("claude-code", "claude-sess")
            }),
            "no edge ends on {:?}: {edges:?}",
            sightings[0].key
        );
    }
}
