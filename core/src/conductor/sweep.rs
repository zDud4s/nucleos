//! Periodic liveness observations and their one-shot feed reports.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use chrono::{DateTime, Duration, Utc};

use crate::conductor::continuation::{ContinuationError, awaiting_continuation, continue_node};
use crate::conductor::liveness::{Limits, Observation, SubjectStatus, Veredicto, classify};
use crate::execution::runs::{finalize_termination, run_timeout_for_mode};
use crate::feed::Subject;
use crate::state::AppState;

pub const SPAWN_GRACE_SECS: i64 = 60;
pub const TERMINAL_LOOKBACK_HOURS: i64 = 24;
/// The feed kind written when a handoff chain stops; `tree_job::wrap` reads it too.
pub(crate) const CONTINUATION_STOPPED: &str = "node_continuation_stopped";
/// A handed-off node whose continuation was refused for a reason that will not go away by itself.
pub(crate) const CONTINUATION_REFUSED: &str = "node_continuation_refused";

fn refusal_summary(reason: &str) -> String {
    format!("A handed-off node could not be continued, so its tree is waiting: {reason}")
}

/// A source which can be warned, and daemon runs which can also be stopped.
#[derive(Debug, Clone)]
pub struct Observed {
    pub kind: SubjectKind,
    pub project_id: Option<String>,
    pub observation: Observation,
}

#[derive(Debug, Clone)]
pub enum SubjectKind {
    External {
        session_id: String,
    },
    DaemonRun {
        run_id: i64,
        node_id: Option<String>,
    },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub reported: Vec<(String, String)>,
    pub cancelled: Vec<i64>,
    pub marked_nodes: Vec<String>,
}

/// The tail buffer has no mtime. This process-local map gives unchanged tails a stable clock.
type TailClocks = HashMap<(i64, usize), (String, DateTime<Utc>)>;
static TAIL_CLOCKS: LazyLock<Mutex<TailClocks>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub async fn tick(
    state: &AppState,
    observed: &[Observed],
    limits: &Limits,
    now: DateTime<Utc>,
) -> sqlx::Result<SweepReport> {
    let mut report = SweepReport::default();
    for observed in observed {
        // One failing subject (a locked database, say) must not leave the rest of the tick undone:
        // the stalled runs after it still need cancelling.
        if let Err(error) = sweep_one(state, observed, limits, now, &mut report).await {
            tracing::warn!(%error, "liveness sweep could not handle one subject");
        }
    }
    // Before `terminal_feed`, so a node reaped here has its failure announced in this same tick.
    if let Err(error) = crate::tree_job::node_run::reconcile_running(&state.pool).await {
        tracing::warn!(%error, "could not reconcile the running tree nodes");
    }
    // Both ticks log their own failures and return nothing, so neither can fail the sweep.
    crate::tree_job::wrap::tick(state).await;
    crate::tree_job::driver::tick(state).await;
    if let Err(error) = continue_handoffs(state, now, &mut report).await {
        tracing::warn!(%error, "liveness sweep could not continue the handed-off nodes");
    }
    if let Err(error) = terminal_feed(state, now, &mut report).await {
        tracing::warn!(%error, "liveness sweep could not announce the terminal nodes");
    }
    Ok(report)
}

/// One observed subject of `tick`: classify it, report it and, for a dead daemon run, cancel it.
async fn sweep_one(
    state: &AppState,
    observed: &Observed,
    limits: &Limits,
    now: DateTime<Utc>,
    report: &mut SweepReport,
) -> sqlx::Result<()> {
    let verdict = classify(&observed.observation, limits, now);
    let Some(kind) = feed_kind(verdict) else {
        return Ok(());
    };
    let (subject, run_id, node_id, daemon) = match &observed.kind {
        SubjectKind::External { session_id } => {
            (format!("session:{session_id}"), None, None, false)
        }
        SubjectKind::DaemonRun { run_id, node_id } => (
            Subject::Run(*run_id).key(),
            Some(*run_id),
            node_id.clone(),
            true,
        ),
    };
    let episode = observed
        .observation
        .started_at
        .iter()
        .chain(observed.observation.last_transcript_write.iter())
        .cloned()
        .max();
    if insert_feed(state, kind, &subject, run_id, &summary(kind), now, episode).await? {
        report.reported.push((kind.to_owned(), subject));
    }
    if daemon
        && matches!(
            verdict,
            Veredicto::SilentSinceStart | Veredicto::Stalled | Veredicto::Dead
        )
    {
        let run_id = run_id.expect("daemon runs have ids");
        if matches!(verdict, Veredicto::Dead) {
            sqlx::query("UPDATE runs SET status = 'cancelled', completed_at = ? WHERE id = ? AND status = 'running'")
                .bind(now.to_rfc3339()).bind(run_id).execute(&state.pool).await?;
        } else {
            finalize_termination(state, run_id, "cancelled").await;
        }
        report.cancelled.push(run_id);
        if let Some(node_id) = node_id {
            let tree_job: Option<Option<String>> =
                sqlx::query_scalar("SELECT tree_job_id FROM work_nodes WHERE id = ?")
                    .bind(&node_id)
                    .fetch_optional(&state.pool)
                    .await?;
            if tree_job.flatten().is_none() {
                sqlx::query("UPDATE work_nodes SET status = 'failed', summary = ?, closed_at = ?, updated_at = ? WHERE id = ? AND status = 'running'")
                    .bind(format!("Liveness sweep: {}", kind.replace("session_", "")))
                    .bind(now.to_rfc3339()).bind(now.to_rfc3339()).bind(&node_id)
                    .execute(&state.pool).await?;
                report.marked_nodes.push(node_id);
            }
        }
    }
    Ok(())
}

/// Continues every handed-off tree node that needs it. A node without a tree job belongs to an
/// external session and is never picked up here. A refusal stops that node for this tick only: the
/// next tick asks again, which is also how a failed launch is retried.
async fn continue_handoffs(
    state: &AppState,
    now: DateTime<Utc>,
    report: &mut SweepReport,
) -> sqlx::Result<()> {
    // A halted daemon starts nothing; a failed read is treated as halted.
    if crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
    {
        return Ok(());
    }
    for node_id in awaiting_continuation(&state.pool).await? {
        match continue_node(state, &node_id).await {
            Ok(_) | Err(ContinuationError::WorkOver) => {}
            Err(ContinuationError::TooManyContinuations) => {
                let subject = format!("node:{node_id}");
                let summary = "Node handed off too many times in a row: its chain stopped.";
                let inserted = insert_feed(
                    state,
                    CONTINUATION_STOPPED,
                    &subject,
                    None,
                    summary,
                    now,
                    None,
                )
                .await?;
                if inserted {
                    report
                        .reported
                        .push((CONTINUATION_STOPPED.to_owned(), subject));
                }
            }
            // A refusal that asking again will not change (the tree's budget, a guard): without a
            // feed line the tree sits waiting and only a warn every minute says why.
            Err(ContinuationError::Budget(reason) | ContinuationError::Refused(reason)) => {
                tracing::warn!(node = %node_id, %reason, "the handed-off node cannot be continued");
                let subject = format!("node:{node_id}");
                let summary = refusal_summary(&reason);
                if insert_feed(
                    state,
                    CONTINUATION_REFUSED,
                    &subject,
                    None,
                    &summary,
                    now,
                    None,
                )
                .await?
                {
                    report
                        .reported
                        .push((CONTINUATION_REFUSED.to_owned(), subject));
                }
            }
            Err(error) => {
                tracing::warn!(node = %node_id, ?error, "could not continue the handed-off node");
            }
        }
    }
    Ok(())
}

fn feed_kind(verdict: Veredicto) -> Option<&'static str> {
    match verdict {
        Veredicto::SilentSinceStart => Some("session_silent"),
        Veredicto::BlockedOnPrompt => Some("session_blocked_on_prompt"),
        Veredicto::Stalled => Some("session_stalled"),
        Veredicto::Dead => Some("session_dead"),
        Veredicto::Healthy | Veredicto::Waiting(_) => None,
    }
}

fn summary(kind: &str) -> String {
    format!("{kind}; use POST /conductor/resume to continue or inspect the run.")
}

async fn insert_feed(
    state: &AppState,
    kind: &str,
    subject: &str,
    run_id: Option<i64>,
    summary: &str,
    now: DateTime<Utc>,
    episode: Option<DateTime<Utc>>,
) -> sqlx::Result<bool> {
    let exists: Option<String> = sqlx::query_scalar(
        "SELECT created_at FROM feed WHERE kind = ? AND subject = ? ORDER BY id DESC LIMIT 1",
    )
    .bind(kind)
    .bind(subject)
    .fetch_optional(&state.pool)
    .await?;
    if let Some(created_at) = exists {
        let new_episode = episode
            .is_some_and(|anchor| parse_time(&created_at).is_some_and(|created| anchor > created));
        if !new_episode {
            return Ok(false);
        }
    }
    sqlx::query(
        "INSERT INTO feed (kind, summary, run_id, subject, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(kind)
    .bind(summary)
    .bind(run_id)
    .bind(subject)
    .bind(now.to_rfc3339())
    .execute(&state.pool)
    .await?;
    Ok(true)
}

async fn terminal_feed(
    state: &AppState,
    now: DateTime<Utc>,
    report: &mut SweepReport,
) -> sqlx::Result<()> {
    let since = now - Duration::hours(TERMINAL_LOOKBACK_HOURS);
    let runs: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM runs WHERE status = 'timed_out' AND completed_at >= ?")
            .bind(since.to_rfc3339())
            .fetch_all(&state.pool)
            .await?;
    for id in runs {
        let subject = Subject::Run(id).key();
        if insert_feed(
            state,
            "run_timed_out",
            &subject,
            Some(id),
            "Run timed out.",
            now,
            None,
        )
        .await?
        {
            report.reported.push(("run_timed_out".to_owned(), subject));
        }
    }
    let nodes: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, status FROM work_nodes WHERE kind != 'root' AND status IN ('done', 'failed', 'handoff') AND closed_at >= ? \
         AND COALESCE(summary, '') NOT LIKE 'Liveness sweep:%'",
    ).bind(since.to_rfc3339()).fetch_all(&state.pool).await?;
    for (id, status) in nodes {
        if report.marked_nodes.contains(&id) {
            continue;
        }
        let kind = format!("node_{status}");
        let subject = format!("node:{id}");
        if insert_feed(
            state,
            &kind,
            &subject,
            None,
            &format!("Node {status}."),
            now,
            None,
        )
        .await?
        {
            report.reported.push((kind, subject));
        }
    }
    Ok(())
}

pub async fn daemon_observations(state: &AppState) -> sqlx::Result<Vec<Observed>> {
    let now = Utc::now();
    let runs: Vec<(i64, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT id, status, mode, created_at, project_id FROM runs WHERE status = 'running'",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut out = Vec::new();
    for (id, status, mode, created_at, project_id) in runs {
        let started_at = parse_time(&created_at);
        let node_id: Option<String> =
            sqlx::query_scalar("SELECT id FROM work_nodes WHERE run_id = CAST(? AS TEXT) LIMIT 1")
                .bind(id)
                .fetch_optional(&state.pool)
                .await?;
        let running_children: i64 =
            match &node_id {
                Some(node) => sqlx::query_scalar(
                    "SELECT COUNT(*) FROM work_nodes WHERE parent_id = ? AND status = 'running'",
                )
                .bind(node)
                .fetch_one(&state.pool)
                .await?,
                None => 0,
            };
        let alive = state.run_handles.lock().unwrap().contains_key(&id);
        let process_alive = if alive {
            Some(true)
        } else if started_at.is_some_and(|start| now - start <= Duration::seconds(SPAWN_GRACE_SECS))
        {
            None
        } else {
            Some(false)
        };
        let tail = state.run_tails.lock().unwrap().get(&id).cloned();
        let (last_transcript_write, tool_in_flight_until) =
            tail.as_ref().map_or((None, None), |tail| {
                let text = tail.lock().unwrap().clone();
                let written =
                    tail_clock(id, std::sync::Arc::as_ptr(tail) as usize, text.clone(), now);
                (
                    if text.is_empty() { None } else { Some(written) },
                    open_tool_deadline(&text, started_at, state, &mode),
                )
            });
        out.push(Observed {
            kind: SubjectKind::DaemonRun {
                run_id: id,
                node_id,
            },
            project_id,
            observation: Observation {
                process_alive,
                status: (status == "running").then_some(SubjectStatus::Busy),
                started_at,
                last_transcript_write,
                tool_in_flight_until,
                running_children,
                ..Default::default()
            },
        });
    }
    Ok(out)
}

fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn tail_clock(id: i64, pointer: usize, text: String, now: DateTime<Utc>) -> DateTime<Utc> {
    let mut clocks = TAIL_CLOCKS.lock().unwrap();
    let key = (id, pointer);
    match clocks.get_mut(&key) {
        Some((old, time)) if *old == text => *time,
        Some((old, time)) => {
            *old = text;
            *time = now;
            now
        }
        None => {
            clocks.insert(key, (text, now));
            now
        }
    }
}

fn open_tool_deadline(
    tail: &str,
    started_at: Option<DateTime<Utc>>,
    state: &AppState,
    mode: &str,
) -> Option<DateTime<Utc>> {
    let mut open = HashSet::new();
    for line in tail.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        for block in value
            .pointer("/message/content")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            match block.get("type").and_then(serde_json::Value::as_str) {
                Some("tool_use") => {
                    if let Some(id) = block.get("id").and_then(serde_json::Value::as_str) {
                        open.insert(id.to_owned());
                    }
                }
                Some("tool_result") => {
                    if let Some(id) = block.get("tool_use_id").and_then(serde_json::Value::as_str) {
                        open.remove(id);
                    }
                }
                _ => {}
            }
        }
    }
    (!open.is_empty()).then(|| {
        started_at.unwrap_or_else(Utc::now)
            + Duration::from_std(run_timeout_for_mode(state.run_timeout, mode)).unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use chrono::{DateTime, Duration, Utc};

    use super::*;
    use crate::conductor::liveness::{
        Limits, Motivo, Observation, SubjectStatus, Veredicto, classify,
    };
    use crate::execution::runs::spawn_registered;
    use crate::feed::Subject;
    use crate::state::AppState;
    use crate::tree_job::model::{Branch, Manifest};
    use crate::tree_job::store::{TreeJob, save};

    async fn state() -> AppState {
        crate::channels::testkit::state().await
    }

    fn limits() -> Limits {
        Limits::default()
    }

    fn minutes(n: i64) -> Duration {
        Duration::minutes(n)
    }

    fn silent_obs(now: DateTime<Utc>) -> Observation {
        Observation {
            process_alive: Some(true),
            status: Some(SubjectStatus::Busy),
            started_at: Some(now - minutes(10)),
            ..Default::default()
        }
    }

    fn stalled_obs(now: DateTime<Utc>) -> Observation {
        Observation {
            process_alive: Some(true),
            status: Some(SubjectStatus::Busy),
            started_at: Some(now - minutes(60)),
            last_transcript_write: Some(now - minutes(20)),
            ..Default::default()
        }
    }

    fn blocked_obs(now: DateTime<Utc>) -> Observation {
        Observation {
            process_alive: Some(true),
            status: Some(SubjectStatus::Waiting),
            status_since: Some(now - minutes(5)),
            started_at: Some(now - minutes(60)),
            last_transcript_write: Some(now - minutes(6)),
            ..Default::default()
        }
    }

    fn dead_obs(now: DateTime<Utc>) -> Observation {
        Observation {
            process_alive: Some(false),
            status: Some(SubjectStatus::Busy),
            started_at: Some(now - minutes(60)),
            ..Default::default()
        }
    }

    fn external(session_id: &str, observation: Observation) -> Observed {
        Observed {
            kind: SubjectKind::External {
                session_id: session_id.to_owned(),
            },
            project_id: None,
            observation,
        }
    }

    fn daemon(run_id: i64, node_id: Option<&str>, observation: Observation) -> Observed {
        Observed {
            kind: SubjectKind::DaemonRun {
                run_id,
                node_id: node_id.map(str::to_owned),
            },
            project_id: None,
            observation,
        }
    }

    fn run_key(id: i64) -> String {
        Subject::Run(id).key()
    }

    fn session_key(session_id: &str) -> String {
        format!("session:{session_id}")
    }

    fn node_key(node_id: &str) -> String {
        format!("node:{node_id}")
    }

    /// A `runs` row and nothing else: no entry in `run_handles`.
    async fn seed_run(state: &AppState, status: &str, created_at: DateTime<Utc>) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at) VALUES ('x', ?, 'real', ?)",
        )
        .bind(status)
        .bind(created_at.to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// A `running` row with a registered handle parked for as long as the test needs it.
    async fn seed_live_run(state: &AppState) -> i64 {
        let id = seed_run(state, "running", Utc::now()).await;
        spawn_registered(state, id, std::future::pending::<()>());
        id
    }

    /// A live run under an id picked by the test, so whatever keeps the progress clock cannot be
    /// confused by another test's run of the same number.
    async fn seed_live_run_with_id(state: &AppState, id: i64, created_at: DateTime<Utc>) {
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, created_at)
             VALUES (?, 'x', 'running', 'real', ?)",
        )
        .bind(id)
        .bind(created_at.to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap();
        spawn_registered(state, id, std::future::pending::<()>());
    }

    async fn seed_finished_run(state: &AppState, status: &str, completed_at: DateTime<Utc>) -> i64 {
        let id = seed_run(state, status, completed_at).await;
        sqlx::query("UPDATE runs SET completed_at = ? WHERE id = ?")
            .bind(completed_at.to_rfc3339())
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
        id
    }

    async fn seed_node(state: &AppState, id: &str, kind: &str, status: &str, run_id: Option<i64>) {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id,
                                     created_at, updated_at)
             VALUES (?, ?, 0, ?, 'title', 'goal', ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(id)
        .bind(kind)
        .bind(status)
        .bind(run_id.map(|run_id| run_id.to_string()))
        .bind(&now)
        .bind(&now)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn seed_closed_node(
        state: &AppState,
        id: &str,
        kind: &str,
        status: &str,
        closed_at: DateTime<Utc>,
    ) {
        seed_node(state, id, kind, status, None).await;
        sqlx::query("UPDATE work_nodes SET closed_at = ?, updated_at = ? WHERE id = ?")
            .bind(closed_at.to_rfc3339())
            .bind(closed_at.to_rfc3339())
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
    }

    async fn rows(state: &AppState, kind: &str, subject: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = ? AND subject = ?")
            .bind(kind)
            .bind(subject)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn rows_about(state: &AppState, subject: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE subject = ?")
            .bind(subject)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn summary_about(state: &AppState, subject: &str) -> String {
        sqlx::query_scalar("SELECT summary FROM feed WHERE subject = ?")
            .bind(subject)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn run_status(state: &AppState, id: i64) -> String {
        sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    /// status, summary.
    async fn node_row(state: &AppState, id: &str) -> (String, Option<String>) {
        sqlx::query_as("SELECT status, summary FROM work_nodes WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    fn has_handle(state: &AppState, id: i64) -> bool {
        state.run_handles.lock().unwrap().contains_key(&id)
    }

    fn publish_tail(state: &AppState, id: i64, text: &str) -> Arc<Mutex<String>> {
        let buffer = Arc::new(Mutex::new(text.to_owned()));
        state
            .run_tails
            .lock()
            .unwrap()
            .insert(id, Arc::clone(&buffer));
        buffer
    }

    fn observation_of(observed: &[Observed], id: i64) -> &Observed {
        observed
            .iter()
            .find(|o| matches!(&o.kind, SubjectKind::DaemonRun { run_id, .. } if *run_id == id))
            .unwrap_or_else(|| panic!("no observation of run {id}"))
    }

    fn node_of(observed: &Observed) -> Option<&str> {
        match &observed.kind {
            SubjectKind::DaemonRun { node_id, .. } => node_id.as_deref(),
            SubjectKind::External { .. } => panic!("a daemon run was expected"),
        }
    }

    /// Asserts the node ended `failed` with a summary that names the verdict.
    async fn assert_failed_naming(state: &AppState, node: &str, verdict: &str) {
        let (status, summary) = node_row(state, node).await;
        assert_eq!(status, "failed");
        let summary = summary.unwrap_or_default().to_lowercase();
        assert!(
            summary.contains(verdict),
            "the summary should name `{verdict}`, got: {summary}"
        );
    }

    // ----- external sessions -----

    #[tokio::test]
    async fn silent_external_session_reports_once() {
        let state = state().await;
        let now = Utc::now();
        let observed = [external("s1", silent_obs(now))];

        let report = tick(&state, &observed, &limits(), now).await.unwrap();

        let subject = session_key("s1");
        assert_eq!(rows(&state, "session_silent", &subject).await, 1);
        assert_eq!(rows_about(&state, &subject).await, 1);
        assert_eq!(
            report.reported,
            [("session_silent".to_owned(), subject.clone())]
        );
    }

    #[tokio::test]
    async fn second_tick_does_not_repeat_the_report() {
        let state = state().await;
        let now = Utc::now();
        let observed = [external("s1", silent_obs(now))];

        tick(&state, &observed, &limits(), now).await.unwrap();
        let second = tick(&state, &observed, &limits(), now + minutes(1))
            .await
            .unwrap();

        assert_eq!(rows_about(&state, &session_key("s1")).await, 1);
        assert!(second.reported.is_empty());
    }

    /// Each anchor that moves starts a new episode: a restart moves `started_at`, a transcript
    /// that was written again and then stopped moves `last_transcript_write`.
    #[tokio::test]
    async fn episode_ending_and_restarting_reports_again() {
        let state = state().await;
        let t0 = Utc::now();
        let silent = session_key("silent");
        let stalled = session_key("stalled");

        tick(
            &state,
            &[
                external("silent", silent_obs(t0)),
                external("stalled", stalled_obs(t0)),
            ],
            &limits(),
            t0,
        )
        .await
        .unwrap();
        assert_eq!(rows(&state, "session_silent", &silent).await, 1);
        assert_eq!(rows(&state, "session_stalled", &stalled).await, 1);

        // The silent one writes: its episode is over, and nothing is said about that.
        let mut wrote = silent_obs(t0);
        wrote.last_transcript_write = Some(t0 + minutes(1));
        tick(
            &state,
            &[external("silent", wrote)],
            &limits(),
            t0 + minutes(2),
        )
        .await
        .unwrap();
        assert_eq!(rows(&state, "session_silent", &silent).await, 1);

        // It restarts and stays silent; the stalled one wrote again and stopped again.
        let restarted = Observation {
            process_alive: Some(true),
            status: Some(SubjectStatus::Busy),
            started_at: Some(t0 + minutes(20)),
            ..Default::default()
        };
        let mut stalled_again = stalled_obs(t0);
        stalled_again.last_transcript_write = Some(t0 + minutes(5));
        let report = tick(
            &state,
            &[
                external("silent", restarted),
                external("stalled", stalled_again),
            ],
            &limits(),
            t0 + minutes(30),
        )
        .await
        .unwrap();

        assert_eq!(rows(&state, "session_silent", &silent).await, 2);
        assert_eq!(rows(&state, "session_stalled", &stalled).await, 2);
        assert_eq!(report.reported.len(), 2);
    }

    #[tokio::test]
    async fn external_session_is_never_killed() {
        let state = state().await;
        let now = Utc::now();
        let id = seed_live_run(&state).await;
        sqlx::query("UPDATE runs SET session_id = 's1' WHERE id = ?")
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
        seed_node(&state, "n1", "leaf", "running", Some(id)).await;
        sqlx::query("UPDATE work_nodes SET session_id = 's1' WHERE id = 'n1'")
            .execute(&state.pool)
            .await
            .unwrap();

        let report = tick(&state, &[external("s1", stalled_obs(now))], &limits(), now)
            .await
            .unwrap();

        assert!(report.cancelled.is_empty());
        assert!(report.marked_nodes.is_empty());
        assert_eq!(run_status(&state, id).await, "running");
        assert!(has_handle(&state, id));
        assert_eq!(node_row(&state, "n1").await.0, "running");
        assert_eq!(rows(&state, "session_stalled", &session_key("s1")).await, 1);
    }

    #[tokio::test]
    async fn external_dead_session_only_warns() {
        let state = state().await;
        let now = Utc::now();

        let report = tick(&state, &[external("s1", dead_obs(now))], &limits(), now)
            .await
            .unwrap();

        assert!(report.cancelled.is_empty());
        assert!(report.marked_nodes.is_empty());
        assert_eq!(rows(&state, "session_dead", &session_key("s1")).await, 1);
    }

    /// The spec's sentence: "o aviso diz o comando exacto: `POST /conductor/resume` em vez de
    /// `--resume`". Every verdict an external session can get ends up asking the same question.
    #[tokio::test]
    async fn external_warning_names_the_resume_route() {
        let state = state().await;
        let now = Utc::now();
        let observed = [
            external("silent", silent_obs(now)),
            external("blocked", blocked_obs(now)),
            external("stalled", stalled_obs(now)),
            external("dead", dead_obs(now)),
        ];

        tick(&state, &observed, &limits(), now).await.unwrap();

        for session in ["silent", "blocked", "stalled", "dead"] {
            let summary = summary_about(&state, &session_key(session)).await;
            assert!(
                summary.contains("POST /conductor/resume"),
                "{session}: {summary}"
            );
            assert!(!summary.contains("--resume"), "{session}: {summary}");
        }
    }

    #[tokio::test]
    async fn stalled_fires_with_an_unknown_process() {
        let state = state().await;
        let now = Utc::now();
        let mut observation = stalled_obs(now);
        observation.process_alive = None;

        tick(&state, &[external("s1", observation)], &limits(), now)
            .await
            .unwrap();

        assert_eq!(rows(&state, "session_stalled", &session_key("s1")).await, 1);
    }

    #[tokio::test]
    async fn write_older_than_start_is_silent_not_stalled() {
        let state = state().await;
        let now = Utc::now();
        let mut observation = silent_obs(now);
        observation.last_transcript_write = Some(now - minutes(30));

        tick(&state, &[external("s1", observation)], &limits(), now)
            .await
            .unwrap();

        let subject = session_key("s1");
        assert_eq!(rows(&state, "session_silent", &subject).await, 1);
        assert_eq!(rows(&state, "session_stalled", &subject).await, 0);
    }

    #[tokio::test]
    async fn waiting_for_children_is_never_reported() {
        let state = state().await;
        let now = Utc::now();
        let id = seed_live_run(&state).await;
        seed_node(&state, "n1", "orchestrator", "running", Some(id)).await;
        let mut quiet = stalled_obs(now);
        quiet.running_children = 2;
        let observed = [external("s1", quiet.clone()), daemon(id, Some("n1"), quiet)];

        let report = tick(&state, &observed, &limits(), now).await.unwrap();

        assert!(report.reported.is_empty());
        assert!(report.cancelled.is_empty());
        assert!(report.marked_nodes.is_empty());
        assert_eq!(rows_about(&state, &session_key("s1")).await, 0);
        assert_eq!(rows_about(&state, &run_key(id)).await, 0);
        assert_eq!(run_status(&state, id).await, "running");
        assert!(has_handle(&state, id));
    }

    // ----- daemon runs -----

    #[tokio::test]
    async fn daemon_run_stalled_is_cancelled_and_node_marked() {
        let state = state().await;
        let now = Utc::now();
        let id = seed_live_run(&state).await;
        seed_node(&state, "n1", "leaf", "running", Some(id)).await;

        let report = tick(
            &state,
            &[daemon(id, Some("n1"), stalled_obs(now))],
            &limits(),
            now,
        )
        .await
        .unwrap();

        assert_eq!(report.cancelled, [id]);
        assert_eq!(report.marked_nodes, ["n1"]);
        assert_eq!(run_status(&state, id).await, "cancelled");
        assert!(!has_handle(&state, id));
        assert_failed_naming(&state, "n1", "stalled").await;
        assert_eq!(rows(&state, "session_stalled", &run_key(id)).await, 1);
    }

    #[tokio::test]
    async fn daemon_run_silent_is_cancelled_and_node_marked() {
        let state = state().await;
        let now = Utc::now();
        let id = seed_live_run(&state).await;
        seed_node(&state, "n1", "leaf", "running", Some(id)).await;

        let report = tick(
            &state,
            &[daemon(id, Some("n1"), silent_obs(now))],
            &limits(),
            now,
        )
        .await
        .unwrap();

        assert_eq!(report.cancelled, [id]);
        assert_eq!(report.marked_nodes, ["n1"]);
        assert_eq!(run_status(&state, id).await, "cancelled");
        assert!(!has_handle(&state, id));
        assert_failed_naming(&state, "n1", "silent").await;
        assert_eq!(rows(&state, "session_silent", &run_key(id)).await, 1);
    }

    /// `finalize_termination` does nothing without a handle, so this is the path that has to write
    /// the status itself: nobody else ever will.
    #[tokio::test]
    async fn daemon_run_dead_without_a_handle_ends_and_node_marked() {
        let state = state().await;
        let now = Utc::now();
        let id = seed_run(&state, "running", now - minutes(60)).await;
        seed_node(&state, "n1", "leaf", "running", Some(id)).await;
        let observed = [daemon(id, Some("n1"), dead_obs(now))];

        let report = tick(&state, &observed, &limits(), now).await.unwrap();
        tick(&state, &observed, &limits(), now + minutes(1))
            .await
            .unwrap();

        assert_ne!(run_status(&state, id).await, "running");
        assert_eq!(report.marked_nodes, ["n1"]);
        assert_failed_naming(&state, "n1", "dead").await;
        assert_eq!(rows(&state, "session_dead", &run_key(id)).await, 1);
    }

    /// A dialog can still be answered, so this one is a warning and not a kill.
    #[tokio::test]
    async fn daemon_run_blocked_on_prompt_is_reported_not_cancelled() {
        let state = state().await;
        let now = Utc::now();
        let id = seed_live_run(&state).await;
        seed_node(&state, "n1", "leaf", "running", Some(id)).await;
        let observed = [daemon(id, Some("n1"), blocked_obs(now))];

        let first = tick(&state, &observed, &limits(), now).await.unwrap();
        let second = tick(&state, &observed, &limits(), now + minutes(1))
            .await
            .unwrap();

        assert_eq!(
            rows(&state, "session_blocked_on_prompt", &run_key(id)).await,
            1
        );
        assert!(first.cancelled.is_empty() && second.cancelled.is_empty());
        assert!(first.marked_nodes.is_empty() && second.marked_nodes.is_empty());
        assert_eq!(run_status(&state, id).await, "running");
        assert!(has_handle(&state, id));
        assert_eq!(node_row(&state, "n1").await.0, "running");
    }

    /// The node the sweep itself failed must not be announced again by the terminal-node scan, and
    /// the stale observation of a tick still in flight must not repeat the report.
    #[tokio::test]
    async fn daemon_run_report_is_a_single_feed_line() {
        let state = state().await;
        let now = Utc::now();
        let id = seed_live_run(&state).await;
        seed_node(&state, "n1", "leaf", "running", Some(id)).await;
        let observed = [daemon(id, Some("n1"), stalled_obs(now))];

        tick(&state, &observed, &limits(), now).await.unwrap();
        tick(&state, &[], &limits(), now + minutes(1))
            .await
            .unwrap();
        tick(&state, &observed, &limits(), now + minutes(2))
            .await
            .unwrap();

        let lines: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE subject IN (?, ?) OR run_id = ?")
                .bind(run_key(id))
                .bind(node_key("n1"))
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(lines, 1);
    }

    /// The parent's reading of R3 in `tree_job::node_run::reconcile_running`: the top branch node of
    /// a live job closes only when its branch does, so losing a run is no reason to fail it.
    #[tokio::test]
    async fn top_branch_node_of_a_live_tree_job_is_not_marked() {
        let state = state().await;
        let now = Utc::now();
        let created = now.to_rfc3339();
        let mut manifest =
            Manifest::new("job-1", "feat/tree", "Ship it", "C:/Projects/nucleos", &[]);
        manifest.state = "open".to_owned();
        let mut branch = Branch::new("api", &[], "code", None);
        branch.state = "building".to_owned();
        branch.node_id = Some("branch-node".to_owned());
        manifest.branches.insert("api".to_owned(), branch);
        let job = TreeJob {
            id: "job-1".to_owned(),
            root_node_id: "job-1-root".to_owned(),
            project_id: None,
            feature_branch: Some("feat/tree".to_owned()),
            goal: "Ship it".to_owned(),
            state: "open".to_owned(),
            manifest,
            contract: None,
            created_at: created.clone(),
            updated_at: created,
        };
        save(&state.pool, &job).await.unwrap();
        let id = seed_live_run(&state).await;
        seed_node(&state, "branch-node", "leaf", "running", Some(id)).await;
        sqlx::query("UPDATE work_nodes SET tree_job_id = 'job-1' WHERE id = 'branch-node'")
            .execute(&state.pool)
            .await
            .unwrap();

        let report = tick(
            &state,
            &[daemon(id, Some("branch-node"), stalled_obs(now))],
            &limits(),
            now,
        )
        .await
        .unwrap();

        assert_eq!(report.cancelled, [id]);
        assert_eq!(run_status(&state, id).await, "cancelled");
        assert!(report.marked_nodes.is_empty());
        let (status, summary) = node_row(&state, "branch-node").await;
        assert_eq!(status, "running");
        assert_eq!(summary, None);
    }

    /// An open tree job (no branches) and a `running` work node of it linked to `run_id`, the way
    /// `tree_job::node_run::launch` links them: `work_nodes.run_id` is the run's decimal id as TEXT.
    async fn seed_tree_node(state: &AppState, run_id: i64) {
        let created = Utc::now().to_rfc3339();
        let mut manifest =
            Manifest::new("job-1", "feat/tree", "Ship it", "C:/Projects/nucleos", &[]);
        manifest.state = "open".to_owned();
        let job = TreeJob {
            id: "job-1".to_owned(),
            root_node_id: "job-1-root".to_owned(),
            project_id: None,
            feature_branch: Some("feat/tree".to_owned()),
            goal: "Ship it".to_owned(),
            state: "open".to_owned(),
            manifest,
            contract: None,
            created_at: created.clone(),
            updated_at: created,
        };
        save(&state.pool, &job).await.unwrap();
        seed_node(state, "tree-node", "leaf", "running", Some(run_id)).await;
        sqlx::query("UPDATE work_nodes SET tree_job_id = 'job-1' WHERE id = 'tree-node'")
            .execute(&state.pool)
            .await
            .unwrap();
    }

    /// Feed lines about the node or its run, whatever their kind.
    async fn lines_about_tree_node(state: &AppState, run_id: i64) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE subject IN (?, ?) OR run_id = ?")
            .bind(run_key(run_id))
            .bind(node_key("tree-node"))
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    /// `reconcile_running` only ran at boot, and `tick` skips tree nodes on purpose, so a tree node
    /// whose run ended while the daemon lived stayed `running` with nobody told.
    #[tokio::test]
    async fn the_sweep_reaps_a_tree_node_whose_run_ended() {
        let state = state().await;
        let now = Utc::now();
        let run = seed_finished_run(&state, "completed", now - minutes(5)).await;
        seed_tree_node(&state, run).await;

        tick(&state, &[], &Limits::default(), now).await.unwrap();

        assert_ne!(node_row(&state, "tree-node").await.0, "running");
        assert_eq!(lines_about_tree_node(&state, run).await, 1);

        tick(&state, &[], &Limits::default(), Utc::now())
            .await
            .unwrap();

        assert_eq!(lines_about_tree_node(&state, run).await, 1);
    }

    /// Passes already: nothing reaps a node whose run is live. It guards the fix against reaping
    /// too much.
    #[tokio::test]
    async fn the_sweep_leaves_a_tree_node_whose_run_is_alive() {
        let state = state().await;
        let run = seed_live_run(&state).await;
        seed_tree_node(&state, run).await;

        tick(&state, &[], &Limits::default(), Utc::now())
            .await
            .unwrap();

        assert_eq!(node_row(&state, "tree-node").await.0, "running");
        assert_eq!(lines_about_tree_node(&state, run).await, 0);
    }

    // ----- the wrap-up and the driver, from the same tick -----

    /// A state whose runner answers the relaunches a wrap-up starts, as the `wrap` tests' state does.
    async fn runner_state() -> AppState {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(std::time::Duration::from_secs(60)),
            std::time::Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        state
    }

    /// One open tree job whose only branch `api` is in `branch_state`, bound to `node_id` if given.
    async fn seed_branch_job(
        state: &AppState,
        job: &str,
        project_id: Option<&str>,
        root: &str,
        branch_state: &str,
        node_id: Option<&str>,
    ) {
        let created = Utc::now().to_rfc3339();
        let mut manifest = Manifest::new(job, "feat/sweep", "Ship it", root, &[]);
        manifest.state = "open".to_owned();
        let mut branch = Branch::new("api", &["api/**".to_owned()], "backend", None);
        branch.state = branch_state.to_owned();
        branch.git_branch = Some("tree/api".to_owned());
        branch.node_id = node_id.map(str::to_owned);
        manifest.branches.insert(branch.name.clone(), branch);
        save(
            &state.pool,
            &TreeJob {
                id: job.to_owned(),
                root_node_id: format!("{job}-root"),
                project_id: project_id.map(str::to_owned),
                feature_branch: Some("feat/sweep".to_owned()),
                goal: "Ship it".to_owned(),
                state: "open".to_owned(),
                manifest,
                contract: None,
                created_at: created.clone(),
                updated_at: created,
            },
        )
        .await
        .unwrap();
    }

    /// Binds a seeded node to a tree job, with the handoff file its run left.
    async fn bind_to_job(state: &AppState, node_id: &str, job: &str) {
        sqlx::query("UPDATE work_nodes SET tree_job_id = ?, handoff_path = ? WHERE id = ?")
            .bind(job)
            .bind(format!("C:/state/nodes/{node_id}/handoff.md"))
            .bind(node_id)
            .execute(&state.pool)
            .await
            .unwrap();
    }

    /// Stores the launch a relaunch starts again from, as the first start of a node stores it.
    async fn store_first_launch(state: &AppState, node_id: &str) {
        crate::tree_job::open::save_launch(
            &state.pool,
            &crate::tree_job::open::StoredLaunch {
                node_id: node_id.to_owned(),
                key: "initial".to_owned(),
                prompt: "original prompt".to_owned(),
                project_id: None,
                cwd: None,
                mode: "shadow".to_owned(),
            },
        )
        .await
        .unwrap();
    }

    /// The `launch` transitions of a node under exactly `key`.
    async fn launch_rows(state: &AppState, node_id: &str, key: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM tree_transitions WHERE node_id = ? AND transition = 'launch' AND key = ?",
        )
        .bind(node_id)
        .bind(key)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    fn git_in(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git should start");
        assert!(status.success(), "git {args:?} failed");
    }

    fn commit_in(repo: &std::path::Path, file: &str, text: &str, message: &str) {
        std::fs::write(repo.join(file), text).expect("write");
        git_in(repo, &["add", "-A"]);
        git_in(repo, &["commit", "-q", "-m", message]);
    }

    /// `master`; `feat/sweep` one commit ahead of it; `tree/api` one commit ahead of that. The
    /// checkout stands on `feat/sweep`, as the driver's fixtures leave it.
    fn sweep_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let container =
            crate::execution::git_exec::testkit::space_free_tempdir("nucleos-sweep-repo-");
        let repo = container.path().join("repo");
        crate::execution::git_exec::testkit::initialize_repo(&repo);
        git_in(&repo, &["branch", "-M", "master"]);
        git_in(&repo, &["checkout", "-q", "-b", "feat/sweep"]);
        commit_in(&repo, "feature.txt", "from the feature\n", "feature");
        git_in(&repo, &["checkout", "-q", "-b", "tree/api"]);
        commit_in(&repo, "api.txt", "from the branch\n", "api");
        git_in(&repo, &["checkout", "-q", "feat/sweep"]);
        (container, repo)
    }

    /// A branch node that handed off, whose run then died, is the wrap-up's to start again. No
    /// other step of the tick launches anything, so the launch row can only come from `wrap::tick`.
    #[tokio::test]
    async fn the_sweep_runs_the_wrap_relaunch_for_a_handed_off_branch_node() {
        let state = runner_state().await;
        let now = Utc::now();
        let run = seed_run(&state, "interrupted", now - minutes(5)).await;
        seed_node(&state, "branch-node", "orchestrator", "running", Some(run)).await;
        seed_branch_job(
            &state,
            "sweep-wrap",
            None,
            "C:/Projects/nucleos",
            "building",
            Some("branch-node"),
        )
        .await;
        bind_to_job(&state, "branch-node", "sweep-wrap").await;
        store_first_launch(&state, "branch-node").await;
        crate::tree_job::node_run::apply(
            &state.pool,
            "branch-node",
            "report",
            &run.to_string(),
            "handoff",
        )
        .await
        .unwrap();

        tick(&state, &[], &limits(), now).await.unwrap();

        assert_eq!(
            launch_rows(&state, "branch-node", &format!("wrap-{run}")).await,
            1
        );
    }

    /// With the kill switch engaged the sweep must not relaunch a handed-off node.
    #[tokio::test]
    async fn continue_handoffs_does_nothing_while_the_kill_switch_is_engaged() {
        let state = runner_state().await;
        let now = Utc::now();
        let run = seed_run(&state, "interrupted", now - minutes(5)).await;
        seed_node(&state, "halted-parent", "orchestrator", "running", None).await;
        seed_node(&state, "halted-node", "orchestrator", "running", Some(run)).await;
        sqlx::query("UPDATE work_nodes SET parent_id = 'halted-parent' WHERE id = 'halted-node'")
            .execute(&state.pool)
            .await
            .unwrap();
        seed_branch_job(
            &state,
            "sweep-halted",
            None,
            "C:/Projects/nucleos",
            "building",
            None,
        )
        .await;
        bind_to_job(&state, "halted-node", "sweep-halted").await;
        store_first_launch(&state, "halted-node").await;
        sqlx::query("UPDATE work_nodes SET status = 'handoff' WHERE id = 'halted-node'")
            .execute(&state.pool)
            .await
            .unwrap();
        crate::governance::autopilot::set_kill_switch(&state.pool, true)
            .await
            .unwrap();

        let mut report = SweepReport::default();
        continue_handoffs(&state, now, &mut report).await.unwrap();

        let siblings: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM work_nodes WHERE continues_node_id = 'halted-node'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(siblings, 0);
    }

    #[tokio::test]
    async fn a_refused_continuation_reaches_the_feed_once() {
        let state = runner_state().await;
        let now = Utc::now();
        let run = seed_run(&state, "interrupted", now - minutes(5)).await;
        seed_node(&state, "refused-parent", "orchestrator", "running", None).await;
        seed_node(&state, "refused-node", "orchestrator", "running", Some(run)).await;
        sqlx::query("UPDATE work_nodes SET parent_id = 'refused-parent' WHERE id = 'refused-node'")
            .execute(&state.pool)
            .await
            .unwrap();
        seed_branch_job(
            &state,
            "sweep-refused",
            None,
            "C:/Projects/nucleos",
            "building",
            None,
        )
        .await;
        bind_to_job(&state, "refused-node", "sweep-refused").await;
        // No launch is stored, so the continuation is refused and asking again changes nothing.
        sqlx::query("UPDATE work_nodes SET status = 'handoff' WHERE id = 'refused-node'")
            .execute(&state.pool)
            .await
            .unwrap();

        let mut report = SweepReport::default();
        continue_handoffs(&state, now, &mut report).await.unwrap();
        continue_handoffs(&state, now, &mut report).await.unwrap();

        let lines: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = ? AND subject = ?")
                .bind(CONTINUATION_REFUSED)
                .bind("node:refused-node")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(lines, 1, "reported once, not every tick");
    }

    /// A green branch is merged by the driver, and the sweep has to call it: nothing else in the
    /// tick queues a merge. The merge request is the branch's `merge_request_id`.
    #[tokio::test]
    async fn the_sweep_advances_a_green_branch_through_the_driver() {
        let state = runner_state().await;
        let now = Utc::now();
        let (_container, repo) = sweep_repo();
        let root = repo.to_string_lossy().into_owned();
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root, integration_branch)
             VALUES ('alpha', 'active', ?, 'master')",
        )
        .bind(root.as_str())
        .execute(&state.pool)
        .await
        .unwrap();
        let run = seed_run(&state, "interrupted", now - minutes(5)).await;
        seed_node(&state, "api-node", "orchestrator", "running", Some(run)).await;
        seed_branch_job(
            &state,
            "sweep-merge",
            Some("alpha"),
            &root,
            "green",
            Some("api-node"),
        )
        .await;
        bind_to_job(&state, "api-node", "sweep-merge").await;
        crate::tree_job::node_run::apply(
            &state.pool,
            "api-node",
            "report",
            &run.to_string(),
            "done",
        )
        .await
        .unwrap();

        tick(&state, &[], &limits(), now).await.unwrap();

        let job = crate::tree_job::store::load(&state.pool, "sweep-merge")
            .await
            .unwrap()
            .expect("the job is stored");
        let branch = &job.manifest.branches["api"];
        assert_eq!(branch.state, "green", "a queued merge is not a merge yet");
        assert!(
            branch
                .extra
                .get("merge_request_id")
                .and_then(serde_json::Value::as_i64)
                .is_some(),
            "no merge request on the branch: {:?}",
            branch.extra
        );
        let requests: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vcs_requests")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(requests, 1);
    }

    /// A job whose project has no repository on record cannot be advanced. That must not keep the
    /// terminal feed from announcing a node that finished in the same tick.
    #[tokio::test]
    async fn a_failing_driver_does_not_stop_the_terminal_feed() {
        let state = state().await;
        let now = Utc::now();
        seed_branch_job(
            &state,
            "sweep-orphan",
            Some("ghost"),
            "C:/nowhere",
            "green",
            None,
        )
        .await;
        seed_closed_node(&state, "n1", "leaf", "done", now - minutes(5)).await;

        tick(&state, &[], &limits(), now).await.unwrap();

        assert_eq!(rows(&state, "node_done", &node_key("n1")).await, 1);
        let job = crate::tree_job::store::load(&state.pool, "sweep-orphan")
            .await
            .unwrap()
            .expect("the job is stored");
        assert_eq!(job.manifest.branches["api"].state, "green");
    }

    // ----- terminal states nobody announced -----

    #[tokio::test]
    async fn timed_out_run_without_feed_row_gets_one() {
        let state = state().await;
        let now = Utc::now();
        let id = seed_finished_run(&state, "timed_out", now - minutes(60)).await;

        let first = tick(&state, &[], &limits(), now).await.unwrap();
        let second = tick(&state, &[], &limits(), now + minutes(1))
            .await
            .unwrap();

        assert_eq!(rows(&state, "run_timed_out", &run_key(id)).await, 1);
        assert_eq!(first.reported, [("run_timed_out".to_owned(), run_key(id))]);
        assert!(second.reported.is_empty());
    }

    #[tokio::test]
    async fn timed_out_run_with_a_feed_row_is_not_repeated() {
        let state = state().await;
        let now = Utc::now();
        let id = seed_finished_run(&state, "timed_out", now - minutes(60)).await;
        sqlx::query(
            "INSERT INTO feed (kind, summary, run_id, subject, created_at)
             VALUES ('run_timed_out', 'already said', ?, ?, ?)",
        )
        .bind(id)
        .bind(run_key(id))
        .bind((now - minutes(59)).to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap();

        let report = tick(&state, &[], &limits(), now).await.unwrap();

        assert_eq!(rows(&state, "run_timed_out", &run_key(id)).await, 1);
        assert!(report.reported.is_empty());
    }

    /// Without the lookback the first tick after a deploy floods the owner with history.
    #[tokio::test]
    async fn terminal_state_older_than_the_lookback_is_not_announced() {
        let state = state().await;
        let now = Utc::now();
        let old = now - Duration::hours(TERMINAL_LOOKBACK_HOURS + 1);
        let id = seed_finished_run(&state, "timed_out", old).await;
        seed_closed_node(&state, "n1", "leaf", "done", old).await;

        let report = tick(&state, &[], &limits(), now).await.unwrap();

        assert!(report.reported.is_empty());
        assert_eq!(rows_about(&state, &run_key(id)).await, 0);
        assert_eq!(rows_about(&state, &node_key("n1")).await, 0);
    }

    /// Two ticks over a node closed `status`, with nothing observed: exactly one `kind` line.
    async fn assert_one_node_line(status: &str, kind: &str) {
        let state = state().await;
        let now = Utc::now();
        seed_closed_node(&state, "n1", "leaf", status, now - minutes(5)).await;

        let first = tick(&state, &[], &limits(), now).await.unwrap();
        let second = tick(&state, &[], &limits(), now + minutes(1))
            .await
            .unwrap();

        let subject = node_key("n1");
        assert_eq!(rows(&state, kind, &subject).await, 1);
        assert_eq!(rows_about(&state, &subject).await, 1);
        assert_eq!(first.reported, [(kind.to_owned(), subject)]);
        assert!(second.reported.is_empty());
    }

    #[tokio::test]
    async fn node_done_gets_one_feed_line() {
        assert_one_node_line("done", "node_done").await;
    }

    #[tokio::test]
    async fn node_failed_gets_one_feed_line() {
        assert_one_node_line("failed", "node_failed").await;
    }

    #[tokio::test]
    async fn node_handoff_gets_one_feed_line() {
        assert_one_node_line("handoff", "node_handoff").await;
    }

    /// `work_nodes::feed_line` already announces these as `work_tree_finished`.
    #[tokio::test]
    async fn root_node_done_is_left_to_the_work_tree_line() {
        let state = state().await;
        let now = Utc::now();
        seed_closed_node(&state, "done", "root", "done", now - minutes(5)).await;
        seed_closed_node(&state, "failed", "root", "failed", now - minutes(5)).await;

        let report = tick(&state, &[], &limits(), now).await.unwrap();

        assert!(report.reported.is_empty());
        assert_eq!(rows_about(&state, &node_key("done")).await, 0);
        assert_eq!(rows_about(&state, &node_key("failed")).await, 0);
    }

    // ----- daemon_observations -----

    #[tokio::test]
    async fn daemon_observations_marks_a_run_without_a_handle_as_not_alive() {
        let state = state().await;
        let now = Utc::now();
        let orphan = seed_run(
            &state,
            "running",
            now - Duration::seconds(SPAWN_GRACE_SECS + 60),
        )
        .await;
        let finished = seed_finished_run(&state, "completed", now).await;

        let observed = daemon_observations(&state).await.unwrap();

        let observation = &observation_of(&observed, orphan).observation;
        assert_eq!(observation.status, Some(SubjectStatus::Busy));
        assert_eq!(observation.process_alive, Some(false));
        assert!(
            !observed.iter().any(
                |o| matches!(&o.kind, SubjectKind::DaemonRun { run_id, .. } if *run_id == finished)
            ),
            "a finished run is not a subject"
        );
    }

    /// A run's row exists before its handle is registered, so a missing handle on a run this young
    /// says nothing about its process.
    #[tokio::test]
    async fn daemon_observations_gives_a_just_created_run_a_grace_period() {
        let state = state().await;
        let id = seed_run(&state, "running", Utc::now()).await;

        let observed = daemon_observations(&state).await.unwrap();

        let observation = &observation_of(&observed, id).observation;
        assert_eq!(observation.process_alive, None);
        assert_ne!(
            classify(observation, &Limits::default(), Utc::now()),
            Veredicto::Dead
        );
    }

    #[tokio::test]
    async fn daemon_observations_counts_running_children() {
        let state = state().await;
        let id = seed_live_run(&state).await;
        seed_node(&state, "parent", "orchestrator", "running", Some(id)).await;
        for (child, status) in [
            ("c1", "running"),
            ("c2", "running"),
            ("c3", "planned"),
            ("c4", "done"),
        ] {
            seed_node(&state, child, "leaf", status, None).await;
            sqlx::query(
                "UPDATE work_nodes SET parent_id = 'parent', root_id = 'parent' WHERE id = ?",
            )
            .bind(child)
            .execute(&state.pool)
            .await
            .unwrap();
        }

        let observed = daemon_observations(&state).await.unwrap();

        assert_eq!(
            observation_of(&observed, id).observation.running_children,
            2
        );
    }

    /// `work_nodes.run_id` is TEXT and `runs.id` is INTEGER: the match is on the whole decimal id.
    #[tokio::test]
    async fn daemon_observations_links_the_node() {
        let state = state().await;
        let linked = seed_live_run(&state).await;
        let unlinked = seed_live_run(&state).await;
        sqlx::query("UPDATE runs SET project_id = 'proj' WHERE id = ?")
            .bind(linked)
            .execute(&state.pool)
            .await
            .unwrap();
        seed_node(&state, "n1", "leaf", "running", Some(linked)).await;
        // Shares the first digits of a real id and must not be taken for it.
        seed_node(&state, "n2", "leaf", "running", Some(unlinked * 10)).await;

        let observed = daemon_observations(&state).await.unwrap();

        let found = observation_of(&observed, linked);
        assert_eq!(node_of(found), Some("n1"));
        assert_eq!(found.project_id.as_deref(), Some("proj"));
        assert_eq!(node_of(observation_of(&observed, unlinked)), None);
    }

    /// `state.run_tails` is the one live signal a daemon run gives (`runner/claude.rs` mirrors each
    /// stdout line into it). `run_events` is only written once the run ends and `context_fill` has
    /// no time, so a run reading `None` here would look silent and be killed at three minutes.
    #[tokio::test]
    async fn daemon_observations_takes_the_progress_clock_from_the_live_tail() {
        let state = state().await;
        let created = Utc::now() - minutes(60);
        let id = 880_001;
        seed_live_run_with_id(&state, id, created).await;
        publish_tail(&state, id, "{\"type\":\"assistant\"}\n");

        let observed = daemon_observations(&state).await.unwrap();

        let observation = &observation_of(&observed, id).observation;
        assert_eq!(observation.process_alive, Some(true));
        assert_eq!(observation.status, Some(SubjectStatus::Busy));
        assert_eq!(
            observation.started_at.map(|at| at.timestamp()),
            Some(created.timestamp())
        );
        assert!(
            observation
                .last_transcript_write
                .is_some_and(|at| at >= created)
        );
        assert_eq!(
            classify(observation, &limits(), Utc::now()),
            Veredicto::Healthy
        );
    }

    /// The case of the 4 h 20: alive, and not one line since it started.
    #[tokio::test]
    async fn daemon_observations_leaves_an_empty_tail_without_a_write() {
        let state = state().await;
        let id = 880_002;
        seed_live_run_with_id(&state, id, Utc::now() - minutes(60)).await;
        publish_tail(&state, id, "");

        let observed = daemon_observations(&state).await.unwrap();

        let observation = &observation_of(&observed, id).observation;
        assert_eq!(observation.last_transcript_write, None);
        assert_eq!(
            classify(observation, &limits(), Utc::now()),
            Veredicto::SilentSinceStart
        );
    }

    /// The buffer carries no time, so the clock is when it was last seen to change. A clock that
    /// simply read "now" for any non-empty tail would never let a daemon run be `Stalled`.
    #[tokio::test]
    async fn daemon_observations_holds_the_progress_clock_while_the_tail_is_quiet() {
        let state = state().await;
        let id = 880_003;
        seed_live_run_with_id(&state, id, Utc::now() - minutes(60)).await;
        let buffer = publish_tail(&state, id, "first\n");
        let clock = |observed: &[Observed]| {
            observation_of(observed, id)
                .observation
                .last_transcript_write
                .expect("a tail with lines has a write")
        };

        let first_pass = daemon_observations(&state).await.unwrap();
        let first = clock(&first_pass);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let quiet_pass = daemon_observations(&state).await.unwrap();
        assert_eq!(clock(&quiet_pass), first, "nothing was written in between");
        assert_eq!(
            classify(
                &observation_of(&quiet_pass, id).observation,
                &limits(),
                first + minutes(11)
            ),
            Veredicto::Stalled
        );

        buffer.lock().unwrap().push_str("second\n");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let moved_pass = daemon_observations(&state).await.unwrap();
        assert!(clock(&moved_pass) > first, "a new line moves the clock");
    }

    /// One tool call streams nothing while it runs, and the runner exempts an open call from its
    /// silence deadline (`unless_silent`). Without the same exemption here a long `cargo test` is
    /// cancelled at ten minutes. A `worktree` run, because a `real` run's wall clock ends no later
    /// than `stalled_after` from its last write, so it has no such moment to classify at.
    #[tokio::test]
    async fn daemon_observations_holds_a_quiet_run_with_an_open_tool_as_in_flight() {
        let state = state().await;
        let now = Utc::now();
        let id = 880_004;
        seed_live_run_with_id(&state, id, now - minutes(1)).await;
        sqlx::query("UPDATE runs SET mode = 'worktree' WHERE id = ?")
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
        // The lines `track_open_tools` reads: `answered` is closed by its result, `building` is not.
        publish_tail(
            &state,
            id,
            concat!(
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"answered","name":"Read","input":{}}]}}"#,
                "\n",
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"answered","content":"ok"}]}}"#,
                "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"building","name":"Bash","input":{}}]}}"#,
                "\n",
            ),
        );

        let observed = daemon_observations(&state).await.unwrap();

        let observation = &observation_of(&observed, id).observation;
        let until = observation
            .tool_in_flight_until
            .expect("an open tool call has a deadline");
        assert!(until > now, "the deadline is still ahead");
        let written = observation
            .last_transcript_write
            .expect("a tail with lines has a write");
        let moment = written + limits().stalled_after + Duration::seconds(1);
        assert!(moment < until, "a call may outlast `stalled_after`");
        assert_eq!(
            classify(observation, &limits(), moment),
            Veredicto::Waiting(Motivo::ToolInFlight)
        );
    }

    /// A run parked on an approval has no process to hold a handle and may wait for hours. Old on
    /// purpose, so no allowance for a just-created run can be what spares it.
    #[tokio::test]
    async fn daemon_observations_does_not_call_an_awaiting_approval_run_dead() {
        let state = state().await;
        let now = Utc::now();
        let id = seed_run(&state, "awaiting_approval", now - minutes(60)).await;

        let observed = daemon_observations(&state).await.unwrap();

        // Not being observed at all is as good an answer as `Waiting(AwaitingApproval)`.
        for found in observed
            .iter()
            .filter(|o| matches!(&o.kind, SubjectKind::DaemonRun { run_id, .. } if *run_id == id))
        {
            let verdict = classify(&found.observation, &limits(), now);
            assert!(
                !matches!(
                    verdict,
                    Veredicto::Dead | Veredicto::Stalled | Veredicto::SilentSinceStart
                ),
                "an approval wait read as {verdict:?}"
            );
        }
    }
}
