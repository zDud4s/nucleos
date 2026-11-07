//! Tree-job node wrap-up: ends a node run that already reported, and starts a branch node again
//! when its run ended with a `handoff` report or died without one.
//!
//! The driver owns every other outcome: a node whose run is gone after a `done`, `blocked` or
//! `failed` report is left alone here.
//!
//! A run that died on an API error a later attempt can get past (a 429 rate limit, the service busy
//! or unreachable) is not a death of the node's work: it is started again after a backoff, under a
//! chain and a cap of its own, and spends nothing of the died cap that is there for real deaths.

use chrono::{DateTime, Utc};

use crate::conductor::continuation::with_handoff;
use crate::conductor::sweep::CONTINUATION_STOPPED;
use crate::execution::runs::finalize_termination;
use crate::state::AppState;

use super::decisions;
use super::engine::branch_node_may_close;
use super::model::JOB_TERMINAL_STATE;
use super::open::{DIED_PREFIX, WRAP_PREFIX, latest_launch, relaunch_node, run_is_live};
use super::relaunch::{
    DECISION_PREFIX, TRANSIENT_PREFIX, budget_refusal, refusal_question, successor_key,
};
use super::store;

/// Nodes one tick acts on, oldest first; nodes it leaves alone do not count.
const MAX_NODES_PER_TICK: usize = 20;
const PARKED: &str = "parked";

/// One kind of automatic relaunch: the key prefix of its links, how many may follow one another
/// since the owner's last answer, and what the owner is asked when that many did.
struct Chain {
    prefix: &'static str,
    cap: i64,
    question: fn(&str, i64) -> String,
}

fn ended_question(node_id: &str, cap: i64) -> String {
    format!(
        "Node {node_id} ended {cap} times in a row without finishing; how should the branch go on?"
    )
}

fn transient_question(node_id: &str, cap: i64) -> String {
    format!(
        "Node {node_id} died on a transient API error (a rate limit, or the service busy or \
         unreachable) {cap} times in a row, with a growing wait between tries; how should it go on?"
    )
}

/// The relaunches that follow a `handoff` report.
const WRAPPED: Chain = Chain {
    prefix: WRAP_PREFIX,
    cap: 3,
    question: ended_question,
};
/// The relaunches of a run that died without a report.
const DIED: Chain = Chain {
    prefix: DIED_PREFIX,
    cap: 2,
    question: ended_question,
};
/// The relaunches of a run that died on a transient API error. With the backoff below, sixteen
/// links wait about six hours in all, past a subscription's usage window.
const TRANSIENT: Chain = Chain {
    prefix: TRANSIENT_PREFIX,
    cap: 16,
    question: transient_question,
};
/// The wait after a transient death before its relaunch: the first, doubled per earlier link of
/// the chain, and never more than the last.
const TRANSIENT_FIRST_BACKOFF_SECS: i64 = 60;
const TRANSIENT_MAX_BACKOFF_SECS: i64 = 30 * 60;

type NodeRow = (String, String, Option<String>, Option<String>);

/// One pass over the running nodes of unfinished jobs, then over the roots whose start failed
/// (`root::retry_failed_starts`). A failure on one node is logged and never stops the others. Safe
/// to call again at any time: every relaunch is keyed by the run it follows.
pub async fn tick(state: &AppState) {
    wrap_running_nodes(state).await;
    super::watchdog::sweep(state).await;
    super::root::retry_failed_starts(state).await;
}

async fn wrap_running_nodes(state: &AppState) {
    let rows: Result<Vec<NodeRow>, _> = sqlx::query_as(
        "SELECT n.id, n.tree_job_id, n.run_id, n.handoff_path \
         FROM work_nodes n JOIN tree_jobs j ON j.id = n.tree_job_id \
         WHERE n.status = 'running' AND j.state <> ? \
         ORDER BY n.created_at, n.id",
    )
    .bind(JOB_TERMINAL_STATE)
    .fetch_all(&state.pool)
    .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "tree wrap-up could not list the running nodes");
            return;
        }
    };
    let mut handled = 0;
    for (node_id, job_id, run_id, handoff) in rows {
        if handled >= MAX_NODES_PER_TICK {
            break;
        }
        // A relaunch that failed waits out its backoff (see `watchdog`).
        if super::watchdog::backing_off(&state.pool, &node_id).await {
            continue;
        }
        match wrap_node(state, &node_id, &job_id, run_id, handoff).await {
            Ok(false) => {}
            Ok(true) => {
                handled += 1;
                super::watchdog::record_success(&state.pool, &node_id).await;
            }
            Err(error) => {
                handled += 1;
                tracing::warn!(node = %node_id, %error, "tree wrap-up failed for the node");
                if let Err(error) =
                    super::watchdog::record_failure(&state.pool, &job_id, &node_id, &error).await
                {
                    tracing::warn!(node = %node_id, %error, "watchdog could not record the failure");
                }
            }
        }
    }
}

/// Wraps one node up. `Ok(true)` when it acted (ended a run, relaunched, or stopped the chain).
async fn wrap_node(
    state: &AppState,
    node_id: &str,
    job_id: &str,
    run_id: Option<String>,
    handoff: Option<String>,
) -> Result<bool, String> {
    let pool = &state.pool;
    let Some(job) = store::load(pool, job_id).await.map_err(|e| e.to_string())? else {
        return Ok(false);
    };
    if job.state == JOB_TERMINAL_STATE {
        return Ok(false);
    }
    let branch = job
        .manifest
        .branches
        .values()
        .find(|branch| branch.node_id.as_deref() == Some(node_id));
    match branch {
        // Only an answer starts a parked branch again; a merged or dropped one is the driver's.
        Some(branch) if branch.state == PARKED || branch_node_may_close(branch) => {
            return Ok(false);
        }
        Some(_) => {}
        // A leaf child fails through `reconcile_running`; only the root is wrapped without a branch.
        None if job.root_node_id != node_id => return Ok(false),
        None => {}
    }
    let has_branch = branch.is_some();

    let waiting: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM decisions WHERE node_id = ? AND status = ?")
            .bind(node_id)
            .bind(decisions::STATUS_OPEN)
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;
    if waiting > 0 {
        return Ok(false);
    }

    let Some(run_id) = run_id else {
        // A root whose start failed after its launch was stored: start it again under that key.
        // A start that failed earlier, or a root bound to its branch, is the job of
        // `root::retry_failed_starts`, which this tick runs after the nodes.
        if has_branch {
            return Ok(false);
        }
        let stored = latest_launch(pool, node_id)
            .await
            .map_err(|e| e.to_string())?;
        if stored.is_none() {
            return Ok(false);
        }
        relaunch_node(state, node_id, "none", &format!("root-{node_id}"), None).await?;
        return Ok(true);
    };

    // `tick` listed its rows before this pass, and a relaunch that started meanwhile (the boot one,
    // under another key) has moved the node on: the run it was handed is no longer the node's.
    let current: Option<Option<String>> =
        sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(node_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;
    if current.flatten().as_deref() != Some(run_id.as_str()) {
        return Ok(false);
    }

    let live = run_is_live(pool, &run_id)
        .await
        .map_err(|e| e.to_string())?;
    let report: Option<String> = sqlx::query_scalar(
        "SELECT result FROM tree_transitions WHERE node_id = ? AND transition = 'report' AND key = ?",
    )
    .bind(node_id)
    .bind(&run_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;

    match (live, report.as_deref()) {
        (true, Some("handoff" | "done" | "blocked" | "failed")) => {
            // It reported, so it has nothing left to do; its process may still be running.
            let Ok(number) = run_id.parse::<i64>() else {
                return Ok(false);
            };
            finalize_termination(state, number, "cancelled").await;
            Ok(true)
        }
        (true, _) => Ok(false),
        (false, Some("handoff")) => {
            let suffix = handoff.as_deref().map(|path| with_handoff("", path));
            relaunch_or_stop(state, &job.id, node_id, &run_id, &WRAPPED, suffix).await
        }
        (false, Some(_)) => Ok(false),
        (false, None) => match transient_death(pool, &run_id).await? {
            Some(ended) => relaunch_after_backoff(state, &job.id, node_id, &run_id, ended).await,
            None => relaunch_or_stop(state, &job.id, node_id, &run_id, &DIED, None).await,
        },
    }
}

/// When `run_id` ended `failed` on an API error a later attempt can get past (a 429 rate limit,
/// the service overloaded or unreachable: `runner::failed_on_a_transient_api_error`), the time it
/// ended. Such a death says nothing about the node's work. An end time that does not parse reads
/// as long ago, so the relaunch is not held back by it.
async fn transient_death(
    pool: &sqlx::SqlitePool,
    run_id: &str,
) -> Result<Option<DateTime<Utc>>, String> {
    let Ok(number) = run_id.parse::<i64>() else {
        return Ok(None);
    };
    let row: Option<(Option<String>, Option<String>, String)> = sqlx::query_as(
        "SELECT stdout, completed_at, created_at FROM runs WHERE id = ? AND status = 'failed'",
    )
    .bind(number)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;
    let Some((Some(stdout), completed_at, created_at)) = row else {
        return Ok(None);
    };
    if !crate::execution::runner::failed_on_a_transient_api_error(&stdout) {
        return Ok(None);
    }
    let ended = completed_at.unwrap_or(created_at);
    Ok(Some(
        DateTime::parse_from_rfc3339(&ended)
            .map(|at| at.with_timezone(&Utc))
            .unwrap_or_default(),
    ))
}

/// The wait after a transient death whose chain already has `prior` links.
fn transient_backoff(prior: i64) -> chrono::Duration {
    let doubled = TRANSIENT_FIRST_BACKOFF_SECS.saturating_mul(1_i64 << prior.clamp(0, 20));
    chrono::Duration::seconds(doubled.min(TRANSIENT_MAX_BACKOFF_SECS))
}

/// Starts a node whose run died on a transient API error again, under the `TRANSIENT` chain, once
/// its backoff after `ended` has passed. `Ok(false)` while it waits: the node was left alone.
async fn relaunch_after_backoff(
    state: &AppState,
    job_id: &str,
    node_id: &str,
    dead_run: &str,
    ended: DateTime<Utc>,
) -> Result<bool, String> {
    let key = format!("{}{dead_run}", TRANSIENT.prefix);
    let prior = links_since_answer(&state.pool, node_id, TRANSIENT.prefix, &key).await?;
    if prior < TRANSIENT.cap && Utc::now() < ended + transient_backoff(prior) {
        return Ok(false);
    }
    relaunch_or_stop(state, job_id, node_id, dead_run, &TRANSIENT, None).await
}

/// The links of `prefix` the node has had since the owner's last answer (its latest `decision-`
/// launch), the key being replayed aside. An answer is a fresh allowance, as it is for the driver's
/// fix cap: a node the owner answered after a park is not parked again at its next end.
async fn links_since_answer(
    pool: &sqlx::SqlitePool,
    node_id: &str,
    prefix: &str,
    key: &str,
) -> Result<i64, String> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM tree_launches \
         WHERE node_id = ? AND key LIKE ? AND key <> ? \
           AND seq > COALESCE((SELECT MAX(seq) FROM tree_launches \
                               WHERE node_id = ? AND key LIKE ?), 0)",
    )
    .bind(node_id)
    .bind(format!("{prefix}%"))
    .bind(key)
    .bind(node_id)
    .bind(format!("{DECISION_PREFIX}%"))
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())
}

/// Starts the node again under `<prefix><dead run>`, unless the chain's cap of links already
/// followed the owner's last answer: then it writes the stop line once and asks the owner, parking
/// the branch, or opening the question on a root that tops none (`decisions::park_or_ask`).
async fn relaunch_or_stop(
    state: &AppState,
    job_id: &str,
    node_id: &str,
    dead_run: &str,
    chain: &Chain,
    suffix: Option<String>,
) -> Result<bool, String> {
    let pool = &state.pool;
    let key = format!("{}{dead_run}", chain.prefix);
    let prior = links_since_answer(pool, node_id, chain.prefix, &key).await?;
    if prior < chain.cap {
        if let Some(reason) = budget_refusal(pool, node_id).await {
            // An automatic relaunch spends: past the tree's limit the owner is asked once (the
            // parked branch, or the root's open decision, ends every later pass at `wrap_node`).
            let question = refusal_question(node_id, &reason);
            decisions::park_or_ask(pool, job_id, node_id, &question)
                .await
                .map_err(|e| e.to_string())?;
            return Ok(true);
        }
        // The boot relaunch may have claimed this dead run first: its key is then ours too.
        let key = successor_key(pool, node_id, dead_run, &key)
            .await
            .map_err(|e| e.to_string())?;
        relaunch_node(state, node_id, dead_run, &key, suffix.as_deref()).await?;
        return Ok(true);
    }

    let subject = format!("node:{node_id}");
    let seen: Option<i64> =
        sqlx::query_scalar("SELECT id FROM feed WHERE kind = ? AND subject = ? LIMIT 1")
            .bind(CONTINUATION_STOPPED)
            .bind(&subject)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;
    if seen.is_none() {
        sqlx::query(
            "INSERT INTO feed (kind, summary, run_id, subject, created_at) VALUES (?, ?, NULL, ?, ?)",
        )
        .bind(CONTINUATION_STOPPED)
        .bind("Node ended too many times in a row: its chain stopped.")
        .bind(&subject)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    }
    // A branch is parked; a root that tops no branch is asked on itself, so the job never waits
    // in silence. `park_or_ask` takes the job's lock itself, so none is held here.
    let question = (chain.question)(node_id, chain.cap);
    decisions::park_or_ask(pool, job_id, node_id, &question)
        .await
        .map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::tree_job::decisions;
    use crate::tree_job::model::{Branch, Manifest};
    use crate::tree_job::node_run;
    use crate::tree_job::open::{self, StoredLaunch};
    use crate::tree_job::store::{self, TreeJob};

    const JOB: &str = "wrap-job";
    const NODE: &str = "wrap-node";
    const CREATED: &str = "2026-10-31T00:00:00+00:00";

    async fn state() -> AppState {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(60)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        state
    }

    async fn seed_node(
        state: &AppState,
        branch_state: Option<&str>,
        job_state: &str,
        node_id: &str,
        run_id: Option<i64>,
    ) {
        let mut manifest = Manifest::new(JOB, "feat/wrap", "Wrap the run", "C:/nowhere", &[]);
        manifest.state = job_state.to_owned();
        if let Some(branch_state) = branch_state {
            let mut branch = Branch::new("branch", &["core/src/**".to_owned()], "core", None);
            branch.state = branch_state.to_owned();
            branch.node_id = Some(node_id.to_owned());
            manifest.branches.insert(branch.name.clone(), branch);
        }
        store::save(
            &state.pool,
            &TreeJob {
                id: JOB.to_owned(),
                root_node_id: "wrap-root".to_owned(),
                project_id: None,
                feature_branch: Some("feat/wrap".to_owned()),
                goal: "Wrap the run".to_owned(),
                state: job_state.to_owned(),
                manifest,
                contract: None,
                created_at: CREATED.to_owned(),
                updated_at: CREATED.to_owned(),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, \
                                     tree_job_id, handoff_path, created_at, updated_at) \
             VALUES (?, ?, ?, ?, 'title', 'goal', 'running', ?, ?, ?, ?, ?)",
        )
        .bind(node_id)
        .bind("wrap-root")
        .bind((branch_state.is_none()) as i64)
        .bind(if branch_state.is_some() {
            "orchestrator"
        } else {
            "root"
        })
        .bind(run_id.map(|id| id.to_string()))
        .bind(JOB)
        .bind("C:/state/nodes/wrap/handoff.md")
        .bind(CREATED)
        .bind(CREATED)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn seed_run(state: &AppState, id: i64, status: &str) {
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, created_at) VALUES (?, 'original prompt', ?, ?)",
        )
        .bind(id)
        .bind(status)
        .bind(CREATED)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn seed_launch(state: &AppState, node_id: &str) {
        open::save_launch(
            &state.pool,
            &StoredLaunch {
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

    async fn report(state: &AppState, node_id: &str, run_id: i64, result: &str) {
        node_run::apply(&state.pool, node_id, "report", &run_id.to_string(), result)
            .await
            .unwrap();
    }

    async fn launches(state: &AppState, node_id: &str, prefix: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tree_transitions WHERE node_id = ? AND transition = 'launch' AND key LIKE ?")
            .bind(node_id)
            .bind(format!("{prefix}%"))
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_dead_run_with_a_handoff_report_is_relaunched_under_a_wrap_key_with_the_handoff_sentence()
     {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(41)).await;
        seed_run(&state, 41, "interrupted").await;
        seed_launch(&state, NODE).await;
        report(&state, NODE, 41, "handoff").await;

        tick(&state).await;

        assert_eq!(launches(&state, NODE, "wrap-41").await, 1);
        let prompt: String =
            sqlx::query_scalar("SELECT prompt FROM runs WHERE id <> 41 ORDER BY id DESC LIMIT 1")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(prompt.contains("handoff.md"), "{prompt:?}");
    }

    #[tokio::test]
    async fn a_second_tick_over_the_same_dead_run_launches_nothing() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(42)).await;
        seed_run(&state, 42, "interrupted").await;
        seed_launch(&state, NODE).await;
        report(&state, NODE, 42, "handoff").await;

        tick(&state).await;
        let first = launches(&state, NODE, "wrap-42").await;
        tick(&state).await;

        assert_eq!(first, 1);
        assert_eq!(launches(&state, NODE, "wrap-42").await, first);
    }

    #[tokio::test]
    async fn a_live_run_with_a_handoff_report_is_ended_not_relaunched() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, None).await;
        let run_id = node_run::launch(
            &state,
            NODE,
            "live",
            "original prompt".to_owned(),
            None,
            None,
            "shadow",
        )
        .await
        .unwrap();
        report(&state, NODE, run_id, "handoff").await;

        tick(&state).await;

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "cancelled");
        assert_eq!(launches(&state, NODE, "wrap-").await, 0);
    }

    #[tokio::test]
    async fn the_fourth_handoff_link_of_a_chain_stops_with_one_feed_line_and_parks_the_branch() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(43)).await;
        seed_run(&state, 43, "interrupted").await;
        seed_launch(&state, NODE).await;
        report(&state, NODE, 43, "handoff").await;
        for key in ["wrap-old-1", "wrap-old-2", "wrap-old-3"] {
            prior_link(&state, NODE, key).await;
        }

        tick(&state).await;

        let parked = store::load(&state.pool, JOB)
            .await
            .unwrap()
            .unwrap()
            .manifest
            .branches["branch"]
            .state
            .clone();
        assert_eq!(parked, "parked");
        let feed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM feed WHERE kind = 'node_continuation_stopped' AND subject = ?",
        )
        .bind(format!("node:{NODE}"))
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(feed, 1);
    }

    #[tokio::test]
    async fn a_run_that_died_without_a_report_is_relaunched_twice_and_then_parked() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(44)).await;
        seed_run(&state, 44, "interrupted").await;
        seed_launch(&state, NODE).await;

        tick(&state).await;
        assert_eq!(launches(&state, NODE, "died-44").await, 1);
        let first: i64 =
            sqlx::query_scalar("SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE id = ?")
                .bind(NODE)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        sqlx::query("UPDATE runs SET status = 'interrupted' WHERE id = ?")
            .bind(first)
            .execute(&state.pool)
            .await
            .unwrap();
        tick(&state).await;
        assert_eq!(launches(&state, NODE, "died-").await, 2);
        let second: i64 =
            sqlx::query_scalar("SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE id = ?")
                .bind(NODE)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        sqlx::query("UPDATE runs SET status = 'interrupted' WHERE id = ?")
            .bind(second)
            .execute(&state.pool)
            .await
            .unwrap();
        tick(&state).await;

        let parked = store::load(&state.pool, JOB)
            .await
            .unwrap()
            .unwrap()
            .manifest
            .branches["branch"]
            .state
            .clone();
        assert_eq!(parked, "parked");
    }

    #[tokio::test]
    async fn a_parked_branch_a_merged_branch_and_a_finished_job_are_never_relaunched() {
        let parked_state = state().await;
        seed_node(&parked_state, Some("parked"), "open", NODE, Some(45)).await;
        seed_run(&parked_state, 45, "interrupted").await;
        seed_launch(&parked_state, NODE).await;
        report(&parked_state, NODE, 45, "handoff").await;

        tick(&parked_state).await;

        assert_eq!(launches(&parked_state, NODE, "wrap-").await, 0);

        let merged_state = state().await;
        seed_node(&merged_state, Some("merged"), "open", NODE, Some(48)).await;
        seed_run(&merged_state, 48, "interrupted").await;
        seed_launch(&merged_state, NODE).await;
        report(&merged_state, NODE, 48, "handoff").await;
        tick(&merged_state).await;
        assert_eq!(launches(&merged_state, NODE, "wrap-").await, 0);

        let finished_state = state().await;
        seed_node(
            &finished_state,
            Some("building"),
            "finished",
            NODE,
            Some(49),
        )
        .await;
        seed_run(&finished_state, 49, "interrupted").await;
        seed_launch(&finished_state, NODE).await;
        report(&finished_state, NODE, 49, "handoff").await;
        tick(&finished_state).await;
        assert_eq!(launches(&finished_state, NODE, "wrap-").await, 0);
    }

    #[tokio::test]
    async fn a_done_report_with_a_dead_run_is_left_to_the_driver() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(46)).await;
        seed_run(&state, 46, "interrupted").await;
        seed_launch(&state, NODE).await;
        report(&state, NODE, 46, "done").await;

        tick(&state).await;

        assert_eq!(launches(&state, NODE, "wrap-").await, 0);
        let run_id: String = sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(NODE)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_id, "46");
    }

    #[tokio::test]
    async fn the_root_node_is_relaunched_after_a_handoff_report() {
        let state = state().await;
        seed_node(&state, None, "open", "wrap-root", Some(47)).await;
        seed_run(&state, 47, "interrupted").await;
        seed_launch(&state, "wrap-root").await;
        report(&state, "wrap-root", 47, "handoff").await;

        tick(&state).await;

        assert_eq!(launches(&state, "wrap-root", "wrap-47").await, 1);
    }

    /// A relaunch starts the node from the launch its first start stored, so it carries that launch's
    /// mode and worktree. A node that was first started in plan mode would be started again in plan
    /// mode, and would plan, end, and be wrapped again until the chain's cap parks the branch.
    ///
    /// Goes through the real root start (the route) and the real wrap, so that the launch the wrap
    /// reads is the one `root::start` stored, and no signature is named that GREEN may still change.
    /// The driver's fix and answer relaunches take the same road (`open::relaunch_node` to
    /// `store::launch_recovering`), so this one pin covers them.
    #[tokio::test]
    async fn a_wrap_relaunch_is_not_launched_in_plan_mode() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};
        use crate::execution::runner::Permission;

        let container = space_free_tempdir("tree-wrap-plan-");
        let repo = container.path().join("repo");
        initialize_repo(&repo);
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .expect("git should start");
            assert!(output.status.success(), "git {args:?} failed");
        };
        git(&["branch", "-M", "master"]);
        let workflow = repo.join(".nucleos/workflow");
        std::fs::create_dir_all(&workflow).unwrap();
        std::fs::write(workflow.join("speed.yaml"), "tree:\n  floor_gb: 0\n").unwrap();
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(60)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        let request = Request::builder()
            .method("POST")
            .uri("/trees")
            .header("Authorization", "Bearer test-token")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "id": JOB,
                    "goal": "Wrap the root",
                    "feature_branch": "feat/wrap",
                    "project_root": repo.to_string_lossy().replace('\\', "/"),
                    "project_id": "project-0",
                })
                .to_string(),
            ))
            .unwrap();
        let response = crate::tree_job::http::routes()
            .with_state(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let root = store::load(&state.pool, JOB)
            .await
            .unwrap()
            .unwrap()
            .root_node_id;
        let first: i64 =
            sqlx::query_scalar("SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE id = ?")
                .bind(&root)
                .fetch_one(&state.pool)
                .await
                .expect("the root run is linked to the root node");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while runner.last_permission.lock().unwrap().is_none()
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // The first run ended after a handoff report; the wrap has to start the node again.
        sqlx::query("UPDATE runs SET status = 'interrupted' WHERE id = ?")
            .bind(first)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE work_nodes SET status = 'running' WHERE id = ?")
            .bind(&root)
            .execute(&state.pool)
            .await
            .unwrap();
        report(&state, &root, first, "handoff").await;
        *runner.last_permission.lock().unwrap() = None;

        tick(&state).await;

        assert_eq!(
            launches(&state, &root, "wrap-").await,
            1,
            "the wrap relaunched the root"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let seen = loop {
            let permission = *runner.last_permission.lock().unwrap();
            if permission.is_some() || std::time::Instant::now() >= deadline {
                break permission;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let seen = seen.expect("the relaunched run reached the runner");
        assert_ne!(
            seen,
            Permission::Plan,
            "a relaunch that plans only ends again and burns the chain's cap"
        );
    }

    // ---- the tree budget ---------------------------------------------------------------------------

    /// Gives the tree (root `wrap-root`) a limit of one dollar and charges five to run `run_id`.
    async fn exhaust_budget(state: &AppState, run_id: i64) {
        sqlx::query(
            "INSERT INTO tree_budgets (tree_id, limit_usd, soft_pct, created_at, updated_at) \
             VALUES ('wrap-root', 1.0, 80.0, ?, ?)",
        )
        .bind(CREATED)
        .bind(CREATED)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query("UPDATE runs SET cost_usd = 5.0 WHERE id = ?")
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
    }

    /// The branch's state and the questions of its open decisions.
    async fn parked_and_asked(state: &AppState) -> (String, Vec<String>) {
        let branch = store::load(&state.pool, JOB)
            .await
            .unwrap()
            .unwrap()
            .manifest
            .branches["branch"]
            .state
            .clone();
        let asked =
            sqlx::query_scalar("SELECT question FROM decisions WHERE node_id = ? AND status = ?")
                .bind(NODE)
                .bind(decisions::STATUS_OPEN)
                .fetch_all(&state.pool)
                .await
                .unwrap();
        (branch, asked)
    }

    #[tokio::test]
    async fn wrap_does_not_relaunch_a_silent_node_past_the_budget() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(51)).await;
        seed_run(&state, 51, "interrupted").await;
        seed_launch(&state, NODE).await;
        report(&state, NODE, 51, "handoff").await;
        exhaust_budget(&state, 51).await;

        tick(&state).await;
        tick(&state).await;

        assert_eq!(
            launches(&state, NODE, "wrap-").await,
            0,
            "no paid run was started"
        );
        let (branch, asked) = parked_and_asked(&state).await;
        assert_eq!(branch, "parked");
        assert_eq!(asked.len(), 1, "the owner is told once: {asked:?}");
        assert!(asked[0].to_lowercase().contains("budget"), "{:?}", asked[0]);
    }

    #[tokio::test]
    async fn a_dead_node_is_not_relaunched_past_the_budget() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(52)).await;
        seed_run(&state, 52, "interrupted").await;
        seed_launch(&state, NODE).await;
        exhaust_budget(&state, 52).await;

        tick(&state).await;
        tick(&state).await;

        assert_eq!(
            launches(&state, NODE, "died-").await,
            0,
            "no paid run was started"
        );
        let (branch, asked) = parked_and_asked(&state).await;
        assert_eq!(branch, "parked");
        assert_eq!(asked.len(), 1, "the owner is told once: {asked:?}");
        assert!(asked[0].to_lowercase().contains("budget"), "{:?}", asked[0]);
    }

    // ---- the boot race -----------------------------------------------------------------------------

    #[tokio::test]
    async fn a_sweep_racing_a_boot_relaunch_shares_its_key() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(55)).await;
        seed_run(&state, 55, "interrupted").await;
        seed_launch(&state, NODE).await;

        let key = super::super::relaunch::successor_key(&state.pool, NODE, "55", "resume-55")
            .await
            .unwrap();
        node_run::apply(&state.pool, NODE, "launch", &key, "56")
            .await
            .unwrap();
        wrap_node(&state, NODE, JOB, Some("55".to_owned()), None)
            .await
            .unwrap();

        assert_eq!(launches(&state, NODE, "died-55").await, 0);
        assert_eq!(all_launches(&state, NODE).await, 1);
    }

    #[tokio::test]
    async fn a_boot_relaunch_racing_a_sweep_shares_its_key() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(56)).await;
        seed_run(&state, 56, "interrupted").await;
        seed_launch(&state, NODE).await;

        let key = super::super::relaunch::successor_key(&state.pool, NODE, "56", "died-56")
            .await
            .unwrap();
        node_run::apply(&state.pool, NODE, "launch", &key, "57")
            .await
            .unwrap();
        let boot = crate::tree_job::store::Resumed {
            jobs: vec![JOB.to_owned()],
            ..Default::default()
        };
        open::relaunch_resumed(&state, &boot).await;

        assert_eq!(launches(&state, NODE, "resume-56").await, 0);
        assert_eq!(all_launches(&state, NODE).await, 1);
    }

    /// The first sweep tick fires as `relaunch_resumed` is spawned, so both see a spared node whose
    /// run is dead and neither takes a lock (keys `died-<run>` and `resume-<run>` differ). `tick`
    /// lists its rows once and then acts on that snapshot, so a node `relaunch_resumed` started in
    /// between is relaunched a second time: its fresh run is orphaned, still live and paid for.
    /// The race is replayed deterministically by handing `wrap_node` the run id it listed before.
    #[tokio::test]
    async fn a_dead_node_at_boot_is_relaunched_once() {
        // The boot relaunch wins, then a sweep that listed the node earlier acts on the old run.
        let stale = state().await;
        seed_node(&stale, Some("building"), "open", NODE, Some(53)).await;
        seed_run(&stale, 53, "interrupted").await;
        seed_launch(&stale, NODE).await;
        let boot = crate::tree_job::store::Resumed {
            jobs: vec![JOB.to_owned()],
            ..Default::default()
        };
        open::relaunch_resumed(&stale, &boot).await;
        let listed_before = Some("53".to_owned());
        wrap_node(&stale, NODE, JOB, listed_before, None)
            .await
            .unwrap();
        assert_eq!(
            all_launches(&stale, NODE).await,
            1,
            "boot first, then the stale sweep"
        );

        // The sweep wins, then the boot relaunch looks again: it reads the node afresh.
        let fresh = state().await;
        seed_node(&fresh, Some("building"), "open", NODE, Some(54)).await;
        seed_run(&fresh, 54, "interrupted").await;
        seed_launch(&fresh, NODE).await;
        tick(&fresh).await;
        // The run the sweep started is live, whatever the stub runner does with it next.
        sqlx::query(
            "UPDATE runs SET status = 'running' \
             WHERE id = (SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE id = ?)",
        )
        .bind(NODE)
        .execute(&fresh.pool)
        .await
        .unwrap();
        open::relaunch_resumed(&fresh, &boot).await;
        assert_eq!(
            all_launches(&fresh, NODE).await,
            1,
            "sweep first, then boot"
        );
    }

    /// Every `launch` transition of the node, whatever its key.
    async fn all_launches(state: &AppState, node_id: &str) -> i64 {
        launches(state, node_id, "").await
    }

    // ---- links that count, and links that do not -------------------------------------------------

    /// An earlier relaunch of the node under `key`, as `store::launch_recovering` leaves one: the
    /// stored launch (whose `seq` orders it) and its `launch` transition.
    async fn prior_link(state: &AppState, node_id: &str, key: &str) {
        open::save_launch(
            &state.pool,
            &StoredLaunch {
                node_id: node_id.to_owned(),
                key: key.to_owned(),
                prompt: "original prompt".to_owned(),
                project_id: None,
                cwd: None,
                mode: "shadow".to_owned(),
            },
        )
        .await
        .unwrap();
        node_run::apply(&state.pool, node_id, "launch", key, "99")
            .await
            .unwrap();
    }

    /// The last line of a `claude -p` run the API refused with a 429, as the runner stores it.
    const RATE_LIMITED_STDOUT: &str = r#"{"type":"result","subtype":"success","is_error":true,"terminal_reason":"api_error","api_error_status":429,"result":"API Error: 429"}"#;

    /// A run that ended `failed` on a 429, `ago` before now.
    async fn seed_rate_limited_run(state: &AppState, id: i64, ago: chrono::Duration) {
        let ended = (chrono::Utc::now() - ago).to_rfc3339();
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, stdout, created_at, completed_at) \
             VALUES (?, 'original prompt', 'failed', ?, ?, ?)",
        )
        .bind(id)
        .bind(RATE_LIMITED_STDOUT)
        .bind(CREATED)
        .bind(&ended)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    /// The questions of the node's open decisions.
    async fn open_questions(state: &AppState, node_id: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT question FROM decisions WHERE node_id = ? AND status = ?")
            .bind(node_id)
            .bind(decisions::STATUS_OPEN)
            .fetch_all(&state.pool)
            .await
            .unwrap()
    }

    /// A 429 says "this subscription, right now", not "the node cannot do its work": its relaunch
    /// does not spend the died cap that is there for real deaths, so a node whose died cap is
    /// already spent is still started again.
    #[tokio::test]
    async fn a_rate_limited_death_is_relaunched_without_spending_the_died_cap() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(61)).await;
        seed_launch(&state, NODE).await;
        prior_link(&state, NODE, "died-old-1").await;
        prior_link(&state, NODE, "died-old-2").await;
        seed_rate_limited_run(&state, 61, chrono::Duration::hours(2)).await;

        tick(&state).await;

        assert_eq!(
            launches(&state, NODE, "died-").await,
            2,
            "no died link spent"
        );
        assert_eq!(all_launches(&state, NODE).await, 3, "one new launch");
        let (branch, asked) = parked_and_asked(&state).await;
        assert_eq!(branch, "building", "not parked: {asked:?}");
        assert!(asked.is_empty(), "{asked:?}");
    }

    /// A rate limit lasts a while: the relaunch waits a backoff after the run ended, so the sweep
    /// does not hammer a limit that has not lifted.
    #[tokio::test]
    async fn a_rate_limited_death_waits_for_its_backoff_before_the_relaunch() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(62)).await;
        seed_launch(&state, NODE).await;
        seed_rate_limited_run(&state, 62, chrono::Duration::seconds(1)).await;

        tick(&state).await;
        assert_eq!(
            all_launches(&state, NODE).await,
            0,
            "still inside the backoff"
        );

        sqlx::query("UPDATE runs SET completed_at = ? WHERE id = 62")
            .bind((chrono::Utc::now() - chrono::Duration::minutes(10)).to_rfc3339())
            .execute(&state.pool)
            .await
            .unwrap();
        tick(&state).await;

        assert_eq!(
            all_launches(&state, NODE).await,
            1,
            "relaunched once it passed"
        );
        assert_eq!(launches(&state, NODE, "died-").await, 0);
    }

    /// Still bounded: a node that keeps dying on the limit is parked with a question at last.
    #[tokio::test]
    async fn rate_limited_deaths_past_their_own_cap_park_the_branch_with_a_question() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(63)).await;
        seed_launch(&state, NODE).await;
        for n in 0..16 {
            prior_link(&state, NODE, &format!("transient-old-{n}")).await;
        }
        seed_rate_limited_run(&state, 63, chrono::Duration::hours(2)).await;

        tick(&state).await;
        tick(&state).await;

        assert_eq!(all_launches(&state, NODE).await, 16, "nothing new started");
        let (branch, asked) = parked_and_asked(&state).await;
        assert_eq!(branch, "parked");
        assert_eq!(asked.len(), 1, "asked once: {asked:?}");
        assert!(
            asked[0].to_lowercase().contains("api error"),
            "{:?}",
            asked[0]
        );
    }

    /// The owner's answer is a fresh allowance, as it is for the fix cap: died links from before
    /// the answer (the `decision-` launch) do not count, so the answered node is not parked again
    /// at its first death.
    #[tokio::test]
    async fn an_answer_after_a_died_cap_park_gives_a_fresh_died_allowance() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(64)).await;
        seed_launch(&state, NODE).await;
        prior_link(&state, NODE, "died-old-1").await;
        prior_link(&state, NODE, "died-old-2").await;
        prior_link(&state, NODE, "decision-d-answered").await;
        seed_run(&state, 64, "interrupted").await;

        tick(&state).await;

        assert_eq!(launches(&state, NODE, "died-64").await, 1);
        let (branch, asked) = parked_and_asked(&state).await;
        assert_eq!(branch, "building", "not parked again: {asked:?}");
    }

    #[tokio::test]
    async fn an_answer_after_a_wrap_cap_park_gives_a_fresh_wrap_allowance() {
        let state = state().await;
        seed_node(&state, Some("building"), "open", NODE, Some(65)).await;
        seed_launch(&state, NODE).await;
        for key in [
            "wrap-old-1",
            "wrap-old-2",
            "wrap-old-3",
            "decision-d-answered",
        ] {
            prior_link(&state, NODE, key).await;
        }
        seed_run(&state, 65, "interrupted").await;
        report(&state, NODE, 65, "handoff").await;

        tick(&state).await;

        assert_eq!(launches(&state, NODE, "wrap-65").await, 1);
        let (branch, asked) = parked_and_asked(&state).await;
        assert_eq!(branch, "building", "not parked again: {asked:?}");
    }

    /// A root that tops no branch has nothing to park, but its chain stopping must still reach the
    /// owner as a question, not only as a feed line: otherwise the job waits forever in silence.
    #[tokio::test]
    async fn a_root_without_a_branch_past_its_died_cap_asks_the_owner() {
        let state = state().await;
        seed_node(&state, None, "open", "wrap-root", Some(66)).await;
        seed_run(&state, 66, "interrupted").await;
        seed_launch(&state, "wrap-root").await;
        prior_link(&state, "wrap-root", "died-old-1").await;
        prior_link(&state, "wrap-root", "died-old-2").await;

        tick(&state).await;
        tick(&state).await;

        assert_eq!(launches(&state, "wrap-root", "died-66").await, 0);
        let asked = open_questions(&state, "wrap-root").await;
        assert_eq!(asked.len(), 1, "asked once: {asked:?}");
        let job = store::load(&state.pool, JOB).await.unwrap().unwrap();
        assert!(
            job.manifest.extra.contains_key("root_park"),
            "the root waits on the decision"
        );
    }
}
