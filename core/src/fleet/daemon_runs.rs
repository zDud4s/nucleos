//! The base network of the live tree (spec §4, adapter 1): everything the daemon launches,
//! whatever its runner, is a node here, and this is the only source that can stop one.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::FleetEnv;
use super::fuse::DAEMON_RUNS_SOURCE;
use super::model::{Cursor, NodeKey, NodeState, Timestamp, TranscriptPage, clean_label};
use super::source::{
    AgentSource, LinesRead, NodeRef, Offset, Sighting, SourceError, SourceResult, Usage,
};
use super::tail::project_of;
use stream::FINAL_DONE;

pub mod stream;

#[derive(sqlx::FromRow)]
struct RunRow {
    id: i64,
    project_id: Option<String>,
    cwd: Option<String>,
    status: String,
    session_id: Option<String>,
    cost_usd: Option<f64>,
    created_at: String,
    completed_at: Option<String>,
    mode: String,
    job_id: Option<i64>,
    runner: Option<String>,
    model: Option<String>,
    context_fill: Option<i64>,
    prompt_head: Option<String>,
}

// The SQL cut sits far beyond the 80-char label so that whitespace collapsing away can never
// leave a token cut mid-way before `clean_label` redacts it (amendment 18).
const DISCOVER_SQL: &str = "SELECT id, project_id, cwd, status, session_id, cost_usd, created_at,
        completed_at, mode, job_id, runner, model, context_fill, substr(prompt, 1, 2000) AS prompt_head
     FROM runs
     WHERE status IN ('running', 'awaiting_approval') OR COALESCE(completed_at, created_at) >= ?
     ORDER BY id DESC LIMIT 500";

pub struct DaemonRuns {
    pool: sqlx::SqlitePool,
    run_tails: crate::state::RunTails,
    /// Rebuilt on every `discover`, so it only ever holds runs inside the retention window.
    keys: Mutex<HashMap<NodeKey, i64>>,
    /// When a live run last wrote a line, as `read_lines` saw it.
    activity: Mutex<HashMap<i64, Timestamp>>,
}

impl DaemonRuns {
    pub fn new(env: &FleetEnv) -> Self {
        Self {
            pool: env.pool.clone(),
            run_tails: env.run_tails.clone(),
            keys: Mutex::new(HashMap::new()),
            activity: Mutex::new(HashMap::new()),
        }
    }

    fn run_of(&self, key: &NodeKey) -> Option<i64> {
        self.keys.lock().ok()?.get(key).copied()
    }

    fn sighting(&self, row: RunRow, since: Timestamp) -> Option<Sighting> {
        let started_at = parse(&row.created_at)?;
        let state = state_of(&row.status);
        let terminal = !matches!(state, NodeState::Busy | NodeState::Idle);
        let ended = row.completed_at.as_deref().and_then(parse);
        if terminal && ended.unwrap_or(started_at) < since {
            return None;
        }
        let (key, harness_label) = key_of(row.runner.as_deref(), row.session_id.as_deref(), row.id);
        let last_activity_at = if terminal {
            ended.unwrap_or(started_at)
        } else {
            self.activity
                .lock()
                .ok()
                .and_then(|seen| seen.get(&row.id).copied())
                .unwrap_or(started_at)
        };
        Some(Sighting {
            key,
            parent: None,
            harness_label: harness_label.to_owned(),
            model: row.model.clone(),
            role: role_of(&row),
            label: label_of(&row),
            project: row
                .cwd
                .as_deref()
                .and_then(project_of)
                .or_else(|| row.project_id.clone()),
            state,
            started_at,
            // Contract amendments 3 and 15: a finished run is sighted with `ended_at` set.
            ended_at: terminal.then_some(ended.unwrap_or(started_at)),
            last_activity_at,
            run_id: Some(row.id),
            can_stop: !terminal,
            has_lines: true,
            has_transcript: false,
            usage: Some(Usage {
                context_tokens: row.context_fill,
                cost_usd: row.cost_usd,
            }),
        })
    }
}

/// Spec §3, "raiz que é run do daemon": any terminal status is `Done` (the last line says which),
/// only a run the daemon found orphaned (`interrupted`, written by the startup reconciliation) is
/// `Lost`, and a run waiting for the owner's approval is `Idle`.
pub(crate) fn state_of(status: &str) -> NodeState {
    match status {
        "running" => NodeState::Busy,
        "awaiting_approval" => NodeState::Idle,
        "interrupted" => NodeState::Lost,
        _ => NodeState::Done,
    }
}

/// The runner-to-harness mapping lives here and nowhere else. `claude` and `codex` runs that have
/// a session id are keyed under the harness that also writes that session on disk, so the harness
/// adapter and this source sight ONE node. An unrouted run (`runner` NULL) ran on the daemon's
/// primary runner, Claude unless `primary_runner: codex` is configured.
pub(crate) fn key_of(
    runner: Option<&str>,
    session_id: Option<&str>,
    run_id: i64,
) -> (NodeKey, &'static str) {
    let native = match runner {
        Some("codex") => Some("codex-cli"),
        Some("claude") | None => Some("claude-code"),
        Some(_) => None,
    };
    match (native, session_id.filter(|id| !id.is_empty())) {
        (Some(harness), Some(session)) => (NodeKey::new(harness, session), harness),
        _ => (NodeKey::new("daemon", &format!("run-{run_id}")), "daemon"),
    }
}

fn role_of(row: &RunRow) -> Option<String> {
    if row.job_id.is_some() {
        Some("job".to_owned())
    } else if row.mode == "assistant" {
        Some("chat".to_owned())
    } else {
        None
    }
}

fn label_of(row: &RunRow) -> String {
    // Collapse, redact, then cut (contract amendment 18): a token straddling character 80 never
    // survives half-redacted.
    let label = clean_label(row.prompt_head.as_deref().unwrap_or(""), 80);
    if label.is_empty() {
        format!("run {}", row.id)
    } else {
        label
    }
}

#[async_trait]
impl AgentSource for DaemonRuns {
    fn name(&self) -> &'static str {
        DAEMON_RUNS_SOURCE
    }

    /// Nothing on disk to watch: the hub polls a source that returns an empty list.
    fn watch_paths(&self) -> Vec<std::path::PathBuf> {
        Vec::new()
    }

    async fn discover(&self, since: Timestamp) -> SourceResult<Vec<Sighting>> {
        let rows = sqlx::query_as::<_, RunRow>(DISCOVER_SQL)
            .bind(since.to_rfc3339())
            .fetch_all(&self.pool)
            .await?;
        // Two runs can carry one session id (a resumed session). One key is one node, so keep a
        // single sighting per key: the live run, else the highest id. `keys` is built from the
        // survivors only, so a read never targets the shadowed run.
        let mut by_key: HashMap<NodeKey, Sighting> = HashMap::new();
        for sighting in rows.into_iter().filter_map(|row| self.sighting(row, since)) {
            match by_key.get(&sighting.key) {
                Some(kept) if !prefers(&sighting, kept) => {}
                _ => {
                    by_key.insert(sighting.key.clone(), sighting);
                }
            }
        }
        let mut sightings: Vec<Sighting> = by_key.into_values().collect();
        sightings.sort_by_key(|s| std::cmp::Reverse(s.run_id));
        let mut keys = self
            .keys
            .lock()
            .map_err(|_| SourceError::msg("the run key map is poisoned"))?;
        keys.clear();
        keys.extend(
            sightings
                .iter()
                .filter_map(|s| s.run_id.map(|id| (s.key.clone(), id))),
        );
        Ok(sightings)
    }

    fn node_ref(&self, key: &NodeKey) -> Option<NodeRef> {
        self.run_of(key).map(|_| NodeRef {
            key: key.clone(),
            path: None,
        })
    }

    async fn read_lines(&self, node: &NodeRef, from: Offset) -> SourceResult<LinesRead> {
        let run_id = self
            .run_of(&node.key)
            .ok_or_else(|| SourceError::msg(format!("unknown daemon run {}", node.key.id())))?;
        if let Some((raw, offset)) = stream::tail_lines(&self.run_tails, run_id, from) {
            let parsed = stream::parse_lines(&raw);
            let last_activity_at = (!raw.is_empty()).then(Utc::now);
            if let (Some(at), Ok(mut seen)) = (last_activity_at, self.activity.lock()) {
                seen.insert(run_id, at);
            }
            return Ok(LinesRead {
                lines: parsed.lines,
                offset,
                usage: parsed.usage,
                last_activity_at,
            });
        }
        // No live buffer: either the run has not started writing, or it ended.
        let (status, stdout_tail): (String, Option<String>) =
            sqlx::query_as("SELECT status, substr(stdout, -8192) FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&self.pool)
                .await?;
        let live = matches!(state_of(&status), NodeState::Busy | NodeState::Idle);
        if live || from == FINAL_DONE {
            return Ok(LinesRead {
                lines: Vec::new(),
                offset: from,
                usage: None,
                last_activity_at: None,
            });
        }
        Ok(LinesRead {
            lines: vec![stream::final_line(
                &status,
                stdout_tail.as_deref().unwrap_or(""),
            )],
            offset: FINAL_DONE,
            usage: None,
            last_activity_at: None,
        })
    }

    async fn transcript_page(
        &self,
        _: &NodeRef,
        _: Option<Cursor>,
    ) -> SourceResult<TranscriptPage> {
        // `has_transcript` is false, so the hub never asks. A run's full output stays in the run
        // detail screen (`run_id` opens it).
        Err(SourceError::msg("daemon runs keep no full transcript"))
    }
}

/// Whether `new` should replace `kept` as the one sighting of their shared key: a live run beats a
/// finished one, and between equals the higher run id (the newer run) wins.
fn prefers(new: &Sighting, kept: &Sighting) -> bool {
    let live = |s: &Sighting| matches!(s.state, NodeState::Busy | NodeState::Idle);
    (live(new), new.run_id) > (live(kept), kept.run_id)
}

fn parse(text: &str) -> Option<Timestamp> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|when| when.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::model::{TermKind, TermLine};
    use chrono::Duration;
    use std::sync::Arc;

    async fn env() -> (FleetEnv, tempfile::TempDir) {
        let pool = crate::testdb::fresh_pool().await;
        let home = tempfile::tempdir().unwrap();
        (
            FleetEnv {
                pool,
                home: home.path().to_path_buf(),
                run_tails: Default::default(),
            },
            home,
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert(
        pool: &sqlx::SqlitePool,
        status: &str,
        session: Option<&str>,
        runner: Option<&str>,
        created: Timestamp,
        completed: Option<Timestamp>,
        stdout: Option<&str>,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, session_id, runner, created_at,
                               completed_at, stdout)
             VALUES ('acme', 'C:\\Projects\\acme', 'fix the parser please', ?, ?, ?, ?, ?, ?)",
        )
        .bind(status)
        .bind(session)
        .bind(runner)
        .bind(created.to_rfc3339())
        .bind(completed.map(|c| c.to_rfc3339()))
        .bind(stdout)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    #[test]
    fn statuses_map_to_states() {
        for (status, state) in [
            ("running", NodeState::Busy),
            ("awaiting_approval", NodeState::Idle),
            ("completed", NodeState::Done),
            ("failed", NodeState::Done),
            ("timed_out", NodeState::Done),
            ("cancelled", NodeState::Done),
            ("superseded", NodeState::Done),
            ("interrupted", NodeState::Lost),
        ] {
            assert_eq!(state_of(status), state, "{status}");
        }
    }

    #[test]
    fn runner_and_session_decide_the_key() {
        let claude = |r, s| key_of(r, s, 9);
        assert_eq!(
            claude(Some("claude"), Some("s1")),
            (NodeKey::new("claude-code", "s1"), "claude-code")
        );
        assert_eq!(
            claude(Some("codex"), Some("s1")),
            (NodeKey::new("codex-cli", "s1"), "codex-cli")
        );
        assert_eq!(
            claude(None, Some("s1")).0,
            NodeKey::new("claude-code", "s1")
        );
        assert_eq!(
            claude(Some("claude"), None),
            (NodeKey::new("daemon", "run-9"), "daemon")
        );
        assert_eq!(
            claude(Some("claude"), Some("")).0,
            NodeKey::new("daemon", "run-9")
        );
        assert_eq!(
            claude(Some("ollama"), Some("s1")).0,
            NodeKey::new("daemon", "run-9")
        );
    }

    #[tokio::test]
    async fn discover_sights_live_runs_and_recent_ones_and_skips_old_ones() {
        let (env, _home) = env().await;
        let now = Utc::now();
        let live = insert(
            &env.pool,
            "running",
            Some("s1"),
            Some("claude"),
            now - Duration::minutes(2),
            None,
            None,
        )
        .await;
        let waiting = insert(
            &env.pool,
            "awaiting_approval",
            Some("s2"),
            None,
            now - Duration::minutes(3),
            None,
            None,
        )
        .await;
        let done = insert(
            &env.pool,
            "completed",
            Some("s3"),
            Some("codex"),
            now - Duration::minutes(6),
            Some(now - Duration::minutes(2)),
            None,
        )
        .await;
        let orphan = insert(
            &env.pool,
            "interrupted",
            None,
            None,
            now - Duration::minutes(8),
            Some(now - Duration::minutes(3)),
            None,
        )
        .await;
        let _old = insert(
            &env.pool,
            "completed",
            Some("s5"),
            None,
            now - Duration::hours(2),
            Some(now - Duration::hours(1)),
            None,
        )
        .await;
        let source = DaemonRuns::new(&env);

        let sightings = source.discover(now - Duration::minutes(10)).await.unwrap();

        let by_id = |id: i64| {
            sightings
                .iter()
                .find(|s| s.run_id == Some(id))
                .expect("sighted")
        };
        assert_eq!(sightings.len(), 4);
        let live = by_id(live);
        assert_eq!(live.key, NodeKey::new("claude-code", "s1"));
        assert_eq!(live.state, NodeState::Busy);
        assert!(live.can_stop && live.has_lines && !live.has_transcript);
        assert_eq!(live.ended_at, None);
        assert_eq!(live.project.as_deref(), Some("acme"));
        assert_eq!(live.label, "fix the parser please");
        assert_eq!(by_id(waiting).state, NodeState::Idle);
        let done = by_id(done);
        assert_eq!(done.key, NodeKey::new("codex-cli", "s3"));
        assert_eq!(done.state, NodeState::Done);
        assert!(!done.can_stop);
        assert!(done.ended_at.is_some());
        let orphan = by_id(orphan);
        assert_eq!(orphan.state, NodeState::Lost);
        assert_eq!(
            orphan.key,
            NodeKey::new("daemon", &format!("run-{}", orphan.run_id.unwrap()))
        );
        assert_eq!(orphan.harness_label, "daemon");
    }

    #[tokio::test]
    async fn runs_sharing_a_session_id_collapse_to_one_sighting_per_key() {
        let (env, _home) = env().await;
        let now = Utc::now();
        // A resumed session writes a second `runs` row under the same session id.
        let live = insert(
            &env.pool,
            "running",
            Some("shared"),
            Some("claude"),
            now - Duration::minutes(5),
            None,
            None,
        )
        .await;
        let _newer_done = insert(
            &env.pool,
            "completed",
            Some("shared"),
            Some("claude"),
            now - Duration::minutes(4),
            Some(now - Duration::minutes(1)),
            None,
        )
        .await;
        let _older_done = insert(
            &env.pool,
            "completed",
            Some("again"),
            Some("claude"),
            now - Duration::minutes(6),
            Some(now - Duration::minutes(5)),
            None,
        )
        .await;
        let newest_done = insert(
            &env.pool,
            "failed",
            Some("again"),
            Some("claude"),
            now - Duration::minutes(3),
            Some(now - Duration::minutes(2)),
            None,
        )
        .await;
        let source = DaemonRuns::new(&env);

        let sightings = source.discover(now - Duration::minutes(10)).await.unwrap();

        assert_eq!(sightings.len(), 2, "one sighting per key: {sightings:?}");
        let shared = sightings
            .iter()
            .find(|s| s.key == NodeKey::new("claude-code", "shared"))
            .unwrap();
        assert_eq!(
            shared.run_id,
            Some(live),
            "the live run beats a newer finished one"
        );
        assert_eq!(shared.state, NodeState::Busy);
        let again = sightings
            .iter()
            .find(|s| s.key == NodeKey::new("claude-code", "again"))
            .unwrap();
        assert_eq!(
            again.run_id,
            Some(newest_done),
            "with no live run the highest id wins"
        );
        // The handle follows the survivor, so a read never targets the shadowed run.
        assert_eq!(source.run_of(&shared.key), Some(live));
        assert_eq!(source.run_of(&again.key), Some(newest_done));
    }

    #[tokio::test]
    async fn a_finished_run_reads_one_last_line_once() {
        let (env, _home) = env().await;
        let now = Utc::now();
        let stdout = "{\"type\":\"result\",\"result\":\"All green\"}
";
        let table = [
            ("completed", TermLine::new(TermKind::Final, "All green")),
            ("failed", TermLine::new(TermKind::Error, "failed")),
            ("timed_out", TermLine::new(TermKind::Error, "timed_out")),
            ("cancelled", TermLine::new(TermKind::Error, "cancelled")),
            ("interrupted", TermLine::new(TermKind::Error, "interrupted")),
        ];
        for (status, _) in &table {
            let session = format!("sess-{status}");
            let (created, ended) = (now - Duration::minutes(5), now - Duration::minutes(1));
            insert(
                &env.pool,
                status,
                Some(&session),
                Some("claude"),
                created,
                Some(ended),
                Some(stdout),
            )
            .await;
        }
        let source = DaemonRuns::new(&env);
        source.discover(now - Duration::minutes(10)).await.unwrap();

        for (status, expected) in table {
            let key = NodeKey::new("claude-code", &format!("sess-{status}"));
            let node = source.node_ref(&key).expect("known run");
            let read = source.read_lines(&node, Offset::TAIL).await.unwrap();
            assert_eq!(read.lines, vec![expected], "{status}");
            assert_eq!(read.offset, FINAL_DONE);
            let again = source.read_lines(&node, FINAL_DONE).await.unwrap();
            assert!(
                again.lines.is_empty(),
                "{status} delivers its last line once"
            );
        }
    }

    #[tokio::test]
    async fn a_running_run_is_read_from_its_live_buffer_incrementally() {
        let (env, _home) = env().await;
        let now = Utc::now();
        let id = insert(
            &env.pool,
            "running",
            Some("live"),
            Some("claude"),
            now,
            None,
            None,
        )
        .await;
        let buffer = Arc::new(Mutex::new(
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hello\"}],\"usage\":{\"input_tokens\":7}}}\n{\"type\":\"assis".to_owned(),
        ));
        env.run_tails.lock().unwrap().insert(id, buffer.clone());
        let source = DaemonRuns::new(&env);
        source.discover(now - Duration::minutes(10)).await.unwrap();
        let node = source
            .node_ref(&NodeKey::new("claude-code", "live"))
            .unwrap();

        let first = source.read_lines(&node, Offset::TAIL).await.unwrap();
        assert_eq!(first.lines, vec![TermLine::new(TermKind::Text, "hello")]);
        assert_eq!(first.usage.as_ref().and_then(|u| u.context_tokens), Some(7));
        assert!(first.last_activity_at.is_some());

        buffer.lock().unwrap().push_str("tant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Bash\",\"input\":{\"command\":\"ls\"}}]}}\n");
        let second = source.read_lines(&node, first.offset).await.unwrap();
        assert_eq!(second.lines, vec![TermLine::new(TermKind::Tool, "Bash ls")]);

        let quiet = source.read_lines(&node, second.offset).await.unwrap();
        assert!(quiet.lines.is_empty());
        assert_eq!(quiet.offset, second.offset);
        assert_eq!(quiet.last_activity_at, None);
    }

    #[tokio::test]
    async fn a_running_run_without_a_buffer_yet_reads_nothing_and_keeps_its_offset() {
        let (env, _home) = env().await;
        let now = Utc::now();
        insert(
            &env.pool,
            "running",
            Some("early"),
            Some("claude"),
            now,
            None,
            None,
        )
        .await;
        let source = DaemonRuns::new(&env);
        source.discover(now - Duration::minutes(10)).await.unwrap();
        let node = source
            .node_ref(&NodeKey::new("claude-code", "early"))
            .unwrap();
        let read = source.read_lines(&node, Offset::TAIL).await.unwrap();
        assert!(read.lines.is_empty());
        assert_eq!(read.offset, Offset::TAIL);
    }

    #[tokio::test]
    async fn an_unknown_key_has_no_handle_and_cannot_be_read() {
        let (env, _home) = env().await;
        let source = DaemonRuns::new(&env);
        let key = NodeKey::new("claude-code", "nope");
        assert!(source.node_ref(&key).is_none());
        let node = NodeRef { key, path: None };
        assert!(source.read_lines(&node, Offset::TAIL).await.is_err());
    }

    #[tokio::test]
    async fn a_token_in_the_prompt_never_reaches_the_label_even_across_the_cut() {
        let (env, _home) = env().await;
        let now = Utc::now();
        // The token starts at character 70 and would end at 110; the label is cut at 80.
        let token = format!("ghp_{}", "A1b2C3d4E5".repeat(4).get(..36).unwrap());
        let prompt = format!("{} {token} and more", "x".repeat(69));
        sqlx::query(
            "INSERT INTO runs (project_id, cwd, prompt, status, session_id, runner, created_at)
             VALUES ('acme', 'C:\\Projects\\acme', ?, 'running', 's-secret', 'claude', ?)",
        )
        .bind(&prompt)
        .bind(now.to_rfc3339())
        .execute(&env.pool)
        .await
        .unwrap();
        let source = DaemonRuns::new(&env);

        let sightings = source.discover(now - Duration::minutes(10)).await.unwrap();

        let label = &sightings[0].label;
        assert!(
            !label.contains("ghp_") && !label.contains(&token[4..14]),
            "{label}"
        );
        assert!(label.chars().count() <= 80);
    }

    #[tokio::test]
    async fn the_registry_carries_the_daemon_source() {
        let (env, _home) = env().await;
        let (agents, _) = crate::fleet::sources(&env);
        assert!(agents.iter().any(|a| a.name() == DAEMON_RUNS_SOURCE));
    }
}
