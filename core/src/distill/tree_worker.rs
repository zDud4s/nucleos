//! The worker half of the tree and land causes (spec 2026-11-16-memoria-agentes-captura, F1): what
//! happens to a claimed `tree_landed`, `tree_failed`, `tree_parked`, `branch_recovered` or
//! `branch_landed` row once `process_one_with` hands it over.
//!
//! RED stage: `process` is a stub, so the tests of this file fail at run time. The GREEN packet
//! replaces the body.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use super::QueueRow;
use crate::embed::Embedder;
use crate::map::intent::Extractor;

/// Distil one tree-side row: close it as `nothing: ...` when there is nothing to learn, defer it
/// when the project's daily cap is spent, fail it as `gone` when its subject was deleted, else ask
/// the model and write what it answers.
#[allow(dead_code)]
pub(super) async fn process(
    _pool: &SqlitePool,
    _asked: Extractor<'_>,
    _embedder: Option<&dyn Embedder>,
    _row: &QueueRow,
    _now: DateTime<Utc>,
) {
    todo!()
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, TimeZone, Utc};
    use serde_json::{Value, json};
    use sqlx::SqlitePool;

    use crate::distill::{claim_next, process_one, tick};
    use crate::execution::runner::{FakeCommandRunner, RunOutcome};
    use crate::map::intent::Extractor;
    use crate::tree_job::model::Manifest;
    use crate::tree_job::store::{self, TreeJob};

    // ----- fixtures -----

    /// A runner that answers `stdout` once. `last_prompt` stays `None` when the model is not asked.
    fn answering(stdout: &str) -> FakeCommandRunner {
        FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(RunOutcome {
                exit_code: 0,
                stdout: stdout.to_owned(),
                stderr: String::new(),
                session_id: None,
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
                model_requested: false,
            })),
            ..Default::default()
        }
    }

    fn noon() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 12, 0, 0).unwrap()
    }

    /// A finished tree of project `alpha`; `extra` lands in the manifest.
    async fn seed_tree(pool: &SqlitePool, id: &str, extra: &[(&str, Value)], at: DateTime<Utc>) {
        let stamp = at.to_rfc3339();
        let mut manifest = Manifest::new(id, &format!("feat/{id}"), "Ship it", "C:/work", &[]);
        manifest.state = "finished".to_owned();
        for (key, value) in extra {
            manifest.extra.insert((*key).to_owned(), value.clone());
        }
        let job = TreeJob {
            id: id.to_owned(),
            root_node_id: format!("{id}-root"),
            project_id: Some("alpha".to_owned()),
            feature_branch: Some(format!("feat/{id}")),
            goal: "Ship it".to_owned(),
            state: "finished".to_owned(),
            manifest,
            contract: None,
            created_at: stamp.clone(),
            updated_at: stamp,
        };
        store::save(pool, &job).await.unwrap();
    }

    async fn seed_work_node(pool: &SqlitePool, id: &str, tree: &str, status: &str) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, parent_id, depth, kind, title, goal, status,
                                     tree_job_id, created_at, updated_at)
             VALUES (?, ?, NULL, 1, 'orchestrator', 'Branch', 'goal', ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(format!("{tree}-root"))
        .bind(status)
        .bind(tree)
        .bind("2026-11-16T10:00:00+00:00")
        .bind("2026-11-16T10:00:00+00:00")
        .execute(pool)
        .await
        .unwrap();
    }

    /// A pending queue row of a tree, written by hand so a test controls cause, `not_before` and
    /// `created_at`. Returns its id.
    async fn seed_tree_row(
        pool: &SqlitePool,
        cause: &str,
        tree_id: &str,
        not_before: Option<&str>,
        created_at: &str,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO distill_queue (cause, project_id, tree_id, status, attempts, not_before,
                                        created_at)
             VALUES (?, 'alpha', ?, 'pending', 0, ?, ?)",
        )
        .bind(cause)
        .bind(tree_id)
        .bind(not_before)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// (status, attempts, not_before, error) of one queue row.
    async fn queue_state(
        pool: &SqlitePool,
        id: i64,
    ) -> (String, i64, Option<String>, Option<String>) {
        sqlx::query_as("SELECT status, attempts, not_before, error FROM distill_queue WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Claim the due row and run it through the real dispatch.
    async fn claim_and_process(pool: &SqlitePool, runner: &FakeCommandRunner, at: DateTime<Utc>) {
        let row = claim_next(pool, at)
            .await
            .unwrap()
            .expect("a due tree row the claim accepts");
        process_one(pool, Extractor::Cli(runner), row, at).await;
    }

    fn prompt_of(runner: &FakeCommandRunner) -> Option<String> {
        runner.last_prompt.lock().unwrap().clone()
    }

    // ----- 43, 44: nothing to learn, no model call -----

    #[tokio::test]
    async fn a_clean_tree_closes_nothing_clean_run_without_calling_the_model() {
        let pool = crate::testdb::fresh_pool().await;
        let now = noon();
        seed_tree(&pool, "job-1", &[("land_sha", json!("abc123"))], now).await;
        let row = seed_tree_row(&pool, "tree_landed", "job-1", None, &now.to_rfc3339()).await;
        let runner = answering("[]");

        claim_and_process(&pool, &runner, now).await;

        assert_eq!(prompt_of(&runner), None, "a clean run never reaches the model");
        let (status, _, _, error) = queue_state(&pool, row).await;
        assert_eq!(status, "done");
        assert_eq!(error.as_deref(), Some("nothing: clean run"));
    }

    #[tokio::test]
    async fn a_held_empty_tree_closes_nothing_clean_run() {
        let pool = crate::testdb::fresh_pool().await;
        let now = noon();
        seed_tree(&pool, "job-1", &[("landing", json!("held"))], now).await;
        let row = seed_tree_row(&pool, "tree_landed", "job-1", None, &now.to_rfc3339()).await;
        let runner = answering("[]");

        claim_and_process(&pool, &runner, now).await;

        assert_eq!(prompt_of(&runner), None);
        let (status, _, _, error) = queue_state(&pool, row).await;
        assert_eq!(status, "done");
        assert_eq!(error.as_deref(), Some("nothing: clean run"));
    }

    // ----- 45: a failed tree teaches -----

    #[tokio::test]
    async fn a_failed_tree_asks_the_model_and_writes_a_trial_learning() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now();
        let extra = [("finish", json!({ "status": "abandoned" }))];
        seed_tree(&pool, "job-1", &extra, now).await;
        seed_work_node(&pool, "node-api", "job-1", "failed").await;
        let row = seed_tree_row(&pool, "tree_failed", "job-1", None, &now.to_rfc3339()).await;
        let runner = answering(
            r#"[{"layer":"procedural","title":"Fix the foo gate","body":"Cause X; do Y","files":[]}]"#,
        );

        claim_and_process(&pool, &runner, now).await;

        let prompt = prompt_of(&runner).expect("a failed tree reaches the model");
        assert!(
            prompt.contains("Why this was queued: tree_failed"),
            "the prompt names the cause: {prompt}"
        );
        let (status, _, _, error) = queue_state(&pool, row).await;
        assert_eq!(status, "done", "error: {error:?}");
        assert_eq!(error, None);

        let learnings: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT title, evidence, distill_cause FROM knowledge WHERE scope_id = 'alpha'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(learnings.len(), 1, "one learning: {learnings:?}");
        let (title, evidence, cause) = &learnings[0];
        assert_eq!(title, "Fix the foo gate");
        assert_eq!(cause.as_deref(), Some("tree_failed"));
        let evidence: Vec<Value> = serde_json::from_str(evidence).unwrap();
        assert!(
            evidence
                .iter()
                .any(|e| e["t"] == "tree" && e["id"] == "job-1"),
            "the tree is evidence: {evidence:?}"
        );
    }

    // ----- 46: the per-project daily cap defers -----

    #[tokio::test]
    async fn the_seventh_tree_of_a_project_day_is_deferred_to_the_next_utc_day() {
        let pool = crate::testdb::fresh_pool().await;
        let now = noon();
        for n in 1..=6 {
            sqlx::query(
                "INSERT INTO distill_queue (cause, project_id, tree_id, status, attempts, created_at,
                                            finished_at)
                 VALUES ('tree_landed', 'alpha', ?, 'done', 0, ?, ?)",
            )
            .bind(format!("old-{n}"))
            .bind((now - Duration::hours(4)).to_rfc3339())
            .bind((now - Duration::hours(3)).to_rfc3339())
            .execute(&pool)
            .await
            .unwrap();
        }
        let extra = [("finish", json!({ "status": "abandoned" }))];
        seed_tree(&pool, "job-7", &extra, now).await;
        seed_work_node(&pool, "node-api", "job-7", "failed").await;
        let row = seed_tree_row(&pool, "tree_failed", "job-7", None, &now.to_rfc3339()).await;
        let runner = answering("[]");

        claim_and_process(&pool, &runner, now).await;

        assert_eq!(prompt_of(&runner), None, "a deferred row does not call the model");
        let (status, attempts, not_before, error) = queue_state(&pool, row).await;
        assert_eq!(status, "pending", "error: {error:?}");
        assert_eq!(attempts, 0, "a deferral is not an attempt");
        let midnight = Utc.with_ymd_and_hms(2026, 11, 17, 0, 0, 0).unwrap();
        let not_before = DateTime::parse_from_rfc3339(&not_before.expect("a not_before"))
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(not_before, midnight);
        assert!(
            claim_next(&pool, now).await.unwrap().is_none(),
            "the deferred row is not due the same day"
        );

        // The next UTC day the cap is fresh and the row is processed.
        claim_and_process(&pool, &runner, midnight).await;
        assert!(prompt_of(&runner).is_some(), "the next day it reaches the model");
        let (status, _, _, error) = queue_state(&pool, row).await;
        assert_eq!(status, "done", "error: {error:?}");
    }

    // ----- 47: stale rows -----

    #[tokio::test]
    async fn a_tree_row_deferred_for_14_days_closes_nothing_stale() {
        let pool = crate::testdb::fresh_pool().await;
        let now = noon();
        let extra = [("finish", json!({ "status": "abandoned" }))];
        seed_tree(&pool, "job-1", &extra, now).await;
        seed_work_node(&pool, "node-api", "job-1", "failed").await;
        let old = (now - Duration::days(15)).to_rfc3339();
        let row = seed_tree_row(&pool, "tree_failed", "job-1", Some(&old), &old).await;
        let runner = answering("[]");

        claim_and_process(&pool, &runner, now).await;

        assert_eq!(prompt_of(&runner), None, "a stale row does not call the model");
        let (status, _, _, error) = queue_state(&pool, row).await;
        assert_eq!(status, "done");
        assert_eq!(error.as_deref(), Some("nothing: stale"));
    }

    // ----- 48: the tree is gone -----

    #[tokio::test]
    async fn a_deleted_tree_fails_the_row_as_gone() {
        let pool = crate::testdb::fresh_pool().await;
        let now = noon();
        let row = seed_tree_row(&pool, "tree_landed", "deleted", None, &now.to_rfc3339()).await;
        let runner = answering("[]");

        claim_and_process(&pool, &runner, now).await;

        assert_eq!(prompt_of(&runner), None);
        let (status, _, _, error) = queue_state(&pool, row).await;
        assert_eq!(status, "failed");
        assert!(
            error.as_deref().is_some_and(|e| e.starts_with("gone")),
            "error: {error:?}"
        );
    }

    // ----- 49: a land with nothing to read -----

    #[tokio::test]
    async fn a_branch_landed_row_without_diff_or_commits_closes_nothing_no_diff() {
        let pool = crate::testdb::fresh_pool().await;
        let now = noon();
        // `C:/work` is not a git repository and the request has no `result_sha`: no diff, no commits.
        let request = sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, status, created_at,
                                       repo_key)
             VALUES ('merge', ?, 'alpha', 'C:/work', 'run', 'succeeded', ?, 'C:/work')",
        )
        .bind(json!({ "source": "feature/x", "target": "main" }).to_string())
        .bind(now.to_rfc3339())
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let row = sqlx::query(
            "INSERT INTO distill_queue (cause, project_id, request_id, status, attempts, created_at)
             VALUES ('branch_landed', 'alpha', ?, 'pending', 0, ?)",
        )
        .bind(request)
        .bind(now.to_rfc3339())
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let runner = answering("[]");

        claim_and_process(&pool, &runner, now).await;

        assert_eq!(prompt_of(&runner), None, "no diff, no model call");
        let (status, _, _, error) = queue_state(&pool, row).await;
        assert_eq!(status, "done");
        assert_eq!(error.as_deref(), Some("nothing: no diff"));
    }

    // ----- 50: the tick sweeps -----

    #[tokio::test]
    async fn tick_sweeps_finished_trees_into_the_queue() {
        let pool = crate::testdb::fresh_pool().await;
        seed_tree(&pool, "job-1", &[("land_sha", json!("abc123"))], Utc::now()).await;
        let runner = answering("[]");

        tick(&pool, Extractor::Cli(&runner), None).await;

        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT cause, status FROM distill_queue WHERE tree_id = 'job-1'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![("tree_landed".to_owned(), "done".to_owned())],
            "the sweep queued the finished tree and the same tick processed it"
        );
    }
}
