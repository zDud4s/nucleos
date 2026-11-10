//! The stalled-tree detector (autonomy-trees spec B8).
//!
//! The driver's steps have silent paths: `landing_step` and `branch_step` return `Ok(())` when what
//! they wait for never comes, and nothing else watches. A job that is not finished, has no live run
//! on any of its nodes, has no open decision and has not changed for [`STALL_SECS`] is stalled. The
//! detector writes a feed line and walks the B1 ladder (`ladder::Kind::Stalled`):
//!
//! - the first rungs are a nudge: a stale merge backoff is dropped, and the next driver pass looks
//!   at the job again;
//! - then one rescue run (`rescue::attempt`) on the node the job waits for;
//! - then the owner is asked (`decisions::park_or_ask`). Its answer is a fresh allowance.
//!
//! Each action rewrites the job, which restarts the quiet time, so the rungs are `STALL_SECS` apart.

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

use super::decisions;
use super::http::job_lock;
use super::ladder::{self, Kind, Spent, Step};
use super::merge_recovery;
use super::model::{JOB_TERMINAL_STATE, TERMINAL_STATES};
use super::relaunch::run_is_live;
use super::rescue;
use super::store::{self, TreeJob};
use crate::state::AppState;

/// A job that does not change for this long, with nothing running and nobody asked, is stalled.
pub(super) const STALL_SECS: i64 = 30 * 60;
/// The record of the rungs spent, on `manifest.extra`: `{"nudges": n, "at": <updated_at it wrote>}`.
const RECORD_KEY: &str = "stalled";

/// Whether the job has been quiet for [`STALL_SECS`] at `now`.
fn quiet(job: &TreeJob, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(&job.updated_at)
        .is_ok_and(|at| now - at.with_timezone(&Utc) >= chrono::Duration::seconds(STALL_SECS))
}

/// The nudges already spent. A record whose `at` is not the job's `updated_at` was written before
/// the job moved again: that stall is over and the count starts from nothing.
fn nudges_spent(job: &TreeJob) -> i64 {
    let Some(record) = job.manifest.extra.get(RECORD_KEY) else {
        return 0;
    };
    if record["at"].as_str() != Some(job.updated_at.as_str()) {
        return 0;
    }
    record["nudges"].as_i64().unwrap_or(0)
}

/// The node the job waits for: the first branch still in play, else the root.
fn subject_node(job: &TreeJob) -> String {
    job.manifest
        .branches
        .values()
        .filter(|branch| !TERMINAL_STATES.contains(&branch.state.as_str()))
        .find_map(|branch| branch.node_id.clone())
        .unwrap_or_else(|| job.root_node_id.clone())
}

/// Whether any node of the job has a live run.
async fn has_live_run(state: &AppState, job: &TreeJob) -> Result<bool, String> {
    let runs: Vec<Option<String>> =
        sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE tree_job_id = ? OR id = ?")
            .bind(&job.id)
            .bind(&job.root_node_id)
            .fetch_all(&state.pool)
            .await
            .map_err(|e| e.to_string())?;
    for run in runs.into_iter().flatten() {
        if run_is_live(&state.pool, &run)
            .await
            .map_err(|e| e.to_string())?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn has_open_decision(state: &AppState, job: &TreeJob) -> Result<bool, String> {
    let open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM decisions WHERE tree_id = ? AND status = ?",
    )
    .bind(&job.id)
    .bind(decisions::STATUS_OPEN)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(open > 0)
}

/// Looks at one job and, when it is stalled, acts. A failure is logged and never stops the driver.
pub(super) async fn check_job(state: &AppState, job_id: &str) {
    if let Err(error) = check(state, job_id, Utc::now()).await {
        tracing::warn!(job = job_id, %error, "stalled-tree check failed");
    }
}

async fn check(state: &AppState, job_id: &str, now: DateTime<Utc>) -> Result<(), String> {
    let pool = &state.pool;
    let park = {
        let lock = job_lock(job_id);
        let _held = lock.lock().await;
        let Some(mut job) = store::load(pool, job_id).await.map_err(|e| e.to_string())? else {
            return Ok(());
        };
        if job.state == JOB_TERMINAL_STATE
            || !quiet(&job, now)
            || has_live_run(state, &job).await?
            || has_open_decision(state, &job).await?
        {
            return Ok(());
        }
        let spent = nudges_spent(&job);
        let node = subject_node(&job);
        let minutes = STALL_SECS / 60;
        let line = format!(
            "{}: the tree has not moved for {minutes} minutes: nothing runs and nobody is asked (rung {})",
            job.goal,
            spent + 1
        );
        if let Err(error) =
            crate::feed::append(pool, job.project_id.as_deref(), "work_tree_stalled", &line, None, None)
                .await
        {
            tracing::warn!(job = job_id, %error, "could not announce the stalled tree");
        }
        let step = ladder::next_step(
            Kind::Stalled,
            Spent {
                retries: spent,
                ..Spent::default()
            },
        );
        let mut question = None;
        match step {
            Step::Retry { .. } => {
                let names: Vec<String> = job.manifest.branches.keys().cloned().collect();
                for name in names {
                    merge_recovery::clear(&mut job, &name);
                }
                record(&mut job, spent + 1, now);
            }
            Step::Diagnose if rescue_started(state, &node, minutes).await? => {
                record(&mut job, spent + 1, now);
            }
            Step::Diagnose | Step::Park => {
                // The owner's answer is a fresh allowance.
                job.manifest.extra.remove(RECORD_KEY);
                question = Some(format!(
                    "The tree has not moved for {minutes} minutes: nothing runs, nobody is asked, and nudging and a diagnose run did not move it. What should it do?"
                ));
                job.updated_at = now.to_rfc3339_opts(SecondsFormat::Secs, false);
            }
        }
        store::update(pool, &job).await.map_err(|e| e.to_string())?;
        question.map(|question| (node, question))
    };
    if let Some((node, question)) = park {
        decisions::park_or_ask(pool, job_id, &node, &question)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Stamps the job with the rungs spent, so the next stall can tell it is the same one.
fn record(job: &mut TreeJob, nudges: i64, now: DateTime<Utc>) {
    let at = now.to_rfc3339_opts(SecondsFormat::Secs, false);
    job.updated_at.clone_from(&at);
    job.manifest
        .extra
        .insert(RECORD_KEY.to_owned(), json!({ "nudges": nudges, "at": at }));
}

/// The rescue run of the node the job waits for. `false` when the node has no run to follow, the
/// allowance is spent, or the run cannot be started: the owner is asked instead.
async fn rescue_started(state: &AppState, node: &str, minutes: i64) -> Result<bool, String> {
    let dead_run: Option<Option<String>> =
        sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(node)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| e.to_string())?;
    let Some(dead_run) = dead_run.flatten() else {
        return Ok(false);
    };
    let suffix = format!(
        "\n\nThe tree this node belongs to has not moved for {minutes} minutes: nothing runs and nobody is asked. This is a diagnose run: find out what the tree is waiting for (a branch, a merge, a gate, a landing), fix it if you can, and finish the node, or say in your handoff what blocks it."
    );
    match rescue::attempt(state, node, &dead_run, &suffix).await {
        Ok(started) => Ok(started),
        Err(error) => {
            tracing::warn!(node, %error, "could not start the stalled tree's diagnose run");
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree_job::driver::tests::{Fixture, code_branch, root_node_of};

    const JOB: &str = "stall-job";

    fn later(minutes: i64) -> DateTime<Utc> {
        Utc::now() + chrono::Duration::minutes(minutes)
    }

    async fn fresh(fx: &Fixture) {
        let mut job = fx.job(JOB).await;
        job.updated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false);
        store::update(&fx.state.pool, &job).await.unwrap();
    }

    async fn feed_lines(fx: &Fixture) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'work_tree_stalled'")
            .fetch_one(&fx.state.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_job_that_moved_recently_is_not_stalled() {
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![code_branch("api", "green", None)], None)
            .await;
        fresh(&fx).await;

        check(&fx.state, JOB, Utc::now()).await.unwrap();

        assert_eq!(feed_lines(&fx).await, 0);
        assert!(fx.job(JOB).await.manifest.extra.get(RECORD_KEY).is_none());
    }

    #[tokio::test]
    async fn a_live_run_keeps_a_quiet_job_from_being_stalled() {
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![code_branch("api", "fixing", None)], None)
            .await;
        fx.run_live(&root_node_of(JOB)).await;

        check(&fx.state, JOB, Utc::now()).await.unwrap();

        assert_eq!(feed_lines(&fx).await, 0);
    }

    #[tokio::test]
    async fn an_open_decision_keeps_a_quiet_job_from_being_stalled() {
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![code_branch("api", "green", None)], None)
            .await;
        decisions::ask_pending(&fx.state.pool, JOB, &root_node_of(JOB), "which one?")
            .await
            .unwrap();

        check(&fx.state, JOB, Utc::now()).await.unwrap();

        assert_eq!(feed_lines(&fx).await, 0);
    }

    #[tokio::test]
    async fn a_stalled_job_gets_a_feed_line_and_a_nudge_that_restarts_the_quiet_time() {
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![code_branch("api", "green", None)], None)
            .await;
        let mut job = fx.job(JOB).await;
        job.manifest.branches.get_mut("api").unwrap().extra.insert(
            merge_recovery::MERGE_BACKOFF_UNTIL.to_owned(),
            json!(later(60).to_rfc3339()),
        );
        store::update(&fx.state.pool, &job).await.unwrap();

        check(&fx.state, JOB, Utc::now()).await.unwrap();

        assert_eq!(feed_lines(&fx).await, 1);
        let after = fx.job(JOB).await;
        assert_eq!(nudges_spent(&after), 1);
        assert!(
            !merge_recovery::backing_off(&after.manifest.branches["api"].extra),
            "a stale merge backoff is dropped"
        );
        // It moved just now: a second look finds nothing to do.
        check(&fx.state, JOB, Utc::now()).await.unwrap();
        assert_eq!(feed_lines(&fx).await, 1);
    }

    #[tokio::test]
    async fn after_the_nudges_one_diagnose_run_starts_and_then_the_owner_is_asked() {
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![code_branch("api", "green", Some("stall-api"))], None)
            .await;
        fx.add_node(JOB, "stall-api", "code").await;
        fx.save_launch("stall-api").await;
        fx.run_ended("stall-api", None).await;

        let mut at = Utc::now();
        // The ladder's retries are nudges.
        for _ in 0..4 {
            at += chrono::Duration::minutes(STALL_SECS / 60 + 1);
            check(&fx.state, JOB, at).await.unwrap();
        }
        assert_eq!(fx.launches("stall-api", "rescue-").await, 0);

        at += chrono::Duration::minutes(STALL_SECS / 60 + 1);
        check(&fx.state, JOB, at).await.unwrap();
        assert_eq!(fx.launches("stall-api", "rescue-").await, 1);
        assert!(fx.open_questions(JOB, "stall-api").await.is_empty());

        // The diagnose run ended without moving anything: the owner is asked, once.
        fx.relaunched_run_reports("stall-api", "failed").await;
        for _ in 0..2 {
            at += chrono::Duration::minutes(STALL_SECS / 60 + 1);
            check(&fx.state, JOB, at).await.unwrap();
        }
        assert_eq!(fx.launches("stall-api", "rescue-").await, 1);
        assert_eq!(fx.open_questions(JOB, "stall-api").await.len(), 1);
        assert!(feed_lines(&fx).await >= 6);
    }

    #[tokio::test]
    async fn a_finished_job_is_never_stalled() {
        let fx = Fixture::new().await;
        fx.seed_job(JOB, vec![code_branch("api", "merged", None)], None)
            .await;
        let mut job = fx.job(JOB).await;
        job.state = JOB_TERMINAL_STATE.to_owned();
        store::update(&fx.state.pool, &job).await.unwrap();

        check(&fx.state, JOB, Utc::now()).await.unwrap();

        assert_eq!(feed_lines(&fx).await, 0);
    }

    #[test]
    fn a_record_from_an_earlier_stall_counts_for_nothing() {
        let mut job = TreeJob {
            id: "j".to_owned(),
            root_node_id: "r".to_owned(),
            project_id: None,
            feature_branch: None,
            goal: "g".to_owned(),
            state: "open".to_owned(),
            manifest: crate::tree_job::model::Manifest::new("j", "feat/x", "g", "/repo", &[]),
            contract: None,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
        };
        record(&mut job, 2, DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap().into());
        assert_eq!(nudges_spent(&job), 2);
        job.updated_at = "2026-01-01T01:00:00Z".to_owned();
        assert_eq!(nudges_spent(&job), 0);
    }
}
