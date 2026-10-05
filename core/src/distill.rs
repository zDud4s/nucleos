//! The distiller: a durable queue of closed jobs and runs whose text is worth reading once, and
//! the worker that turns each one into project-scoped learnings.
//!
//! Design source of truth: `.ai/specs/2026-10-05-destilador-design.md` (§3 is the queue schema,
//! §4 the causes and the worker). Phase A, packet P1 covers the queue, the cause vocabulary and
//! the in-transaction enqueue helpers; nothing here reads a job's text yet.

// Wired in P5: until the job loop and the vcs queue call the helpers, only the tests do.
#![cfg_attr(not(test), allow(dead_code))]

use sqlx::SqliteConnection;

/// Why a job or run was queued. The spelling is what `distill_queue.cause` holds; the
/// `distill_review_blocking` trigger writes `review_blocking` as a SQL literal, which a test pins
/// to [`Cause::ReviewBlocking`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cause {
    JobLanded,
    JobFailed,
    GateRecovered,
    ReviewBlocking,
    RunExhausted,
}

pub const CAUSES: [Cause; 5] = [
    Cause::JobLanded,
    Cause::JobFailed,
    Cause::GateRecovered,
    Cause::ReviewBlocking,
    Cause::RunExhausted,
];

impl Cause {
    pub fn as_str(self) -> &'static str {
        match self {
            Cause::JobLanded => "job_landed",
            Cause::JobFailed => "job_failed",
            Cause::GateRecovered => "gate_recovered",
            Cause::ReviewBlocking => "review_blocking",
            Cause::RunExhausted => "run_exhausted",
        }
    }

    pub fn parse(s: &str) -> Option<Cause> {
        CAUSES.iter().copied().find(|c| c.as_str() == s)
    }
}

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_DONE: &str = "done";
pub const STATUS_FAILED: &str = "failed";

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Queue a job that ended without completing. Callers pass `&mut *tx` so the row rides the
/// retirement's own transaction.
pub async fn enqueue_job_ending_in(
    conn: &mut SqliteConnection,
    job_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
             (cause, project_id, job_id, item_id, run_id, status, attempts, created_at)
         SELECT ?, project_id, id, NULL, NULL, 'pending', 0, ? FROM jobs WHERE id = ?",
    )
    .bind(Cause::JobFailed.as_str())
    .bind(now())
    .bind(job_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Queue the item verdict just written for `(job_id, ordinal)`: a pass after at least one red
/// gate is a recovery, and a `gate_failed` item whose reds exceed the job's retries is
/// exhausted. Anything else queues nothing.
pub async fn enqueue_item_verdict_in(
    conn: &mut SqliteConnection,
    job_id: i64,
    ordinal: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
             (cause, project_id, job_id, item_id, run_id, status, attempts, created_at)
         SELECT ?, j.project_id, j.id, i.id, i.run_id, 'pending', 0, ?
           FROM job_items i JOIN jobs j ON j.id = i.job_id
          WHERE i.job_id = ? AND i.ordinal = ?
            AND i.gate_status = 'passed' AND i.gate_attempts >= 1",
    )
    .bind(Cause::GateRecovered.as_str())
    .bind(now())
    .bind(job_id)
    .bind(ordinal)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
             (cause, project_id, job_id, item_id, run_id, status, attempts, created_at)
         SELECT ?, j.project_id, j.id, i.id, i.run_id, 'pending', 0, ?
           FROM job_items i JOIN jobs j ON j.id = i.job_id
          WHERE i.job_id = ? AND i.ordinal = ?
            AND i.status = 'gate_failed' AND i.gate_attempts > j.gate_retries",
    )
    .bind(Cause::RunExhausted.as_str())
    .bind(now())
    .bind(job_id)
    .bind(ordinal)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Queue a land: a succeeded merge of a job's own branch (`nucleos/job-<id>`, same project) into
/// a branch outside `nucleos/`.
pub async fn enqueue_landed_in(
    conn: &mut SqliteConnection,
    vcs_request_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
             (cause, project_id, job_id, item_id, run_id, status, attempts, created_at)
         SELECT ?, j.project_id, j.id, NULL, NULL, 'pending', 0, ?
           FROM vcs_requests v
           JOIN jobs j ON j.project_id = v.project_id
                      AND json_extract(v.args, '$.source') = 'nucleos/job-' || j.id
          WHERE v.id = ? AND v.status = 'succeeded' AND v.op = 'merge'
            AND json_extract(v.args, '$.target') NOT GLOB 'nucleos/*'",
    )
    .bind(Cause::JobLanded.as_str())
    .bind(now())
    .bind(vcs_request_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CAUSES, Cause, enqueue_item_verdict_in, enqueue_job_ending_in, enqueue_landed_in,
    };
    use sqlx::SqlitePool;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    /// A job row in a status that is not live, so two jobs of one project never meet
    /// `one_live_job_per_project`.
    async fn seed_job(pool: &SqlitePool, id: i64, project: &str, gate_retries: i64) {
        sqlx::query(
            "INSERT INTO jobs (id, project_id, project_root, status, max_items, gate_retries, created_at)
             VALUES (?, ?, 'C:/work', 'failed', 3, ?, '2026-10-05T00:00:00Z')",
        )
        .bind(id)
        .bind(project)
        .bind(gate_retries)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_item(
        pool: &SqlitePool,
        job_id: i64,
        ordinal: i64,
        status: &str,
        gate_status: &str,
        gate_attempts: i64,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, gate_status, gate_attempts)
             VALUES (?, ?, 'do the thing', ?, ?, ?)",
        )
        .bind(job_id)
        .bind(ordinal)
        .bind(status)
        .bind(gate_status)
        .bind(gate_attempts)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn seed_run(
        pool: &SqlitePool,
        project: &str,
        status: &str,
        job_id: Option<i64>,
        stage: Option<&str>,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, created_at, job_id, stage)
             VALUES (?, 'go', ?, '2026-10-05T00:00:00Z', ?, ?)",
        )
        .bind(project)
        .bind(status)
        .bind(job_id)
        .bind(stage)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn seed_merge(
        pool: &SqlitePool,
        project: &str,
        op: &str,
        source: &str,
        target: &str,
        status: &str,
    ) -> i64 {
        let args = serde_json::json!({ "op": op, "source": source, "target": target }).to_string();
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, status, created_at)
             VALUES (?, ?, ?, 'C:/work', 'job', ?, '2026-10-05T00:00:00Z')",
        )
        .bind(op)
        .bind(args)
        .bind(project)
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn queued(pool: &SqlitePool, cause: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM distill_queue WHERE cause = ?")
            .bind(cause)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn queued_total(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM distill_queue")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// The unique index treats a missing id as -1, so the same cause for the same subject is one
    /// row whether the ids are set or NULL — SQLite alone would call every NULL distinct.
    #[tokio::test]
    async fn the_same_cause_is_queued_once() {
        let pool = test_pool().await;
        seed_job(&pool, 1, "alpha", 0).await;

        let mut conn = pool.acquire().await.unwrap();
        enqueue_job_ending_in(&mut *conn, 1).await.unwrap();
        enqueue_job_ending_in(&mut *conn, 1).await.unwrap();
        drop(conn);
        assert_eq!(
            queued(&pool, Cause::JobFailed.as_str()).await,
            1,
            "the same job ending was queued twice"
        );

        // NULL job, item and run: the expression index is what makes these collide.
        for _ in 0..2 {
            sqlx::query(
                "INSERT OR IGNORE INTO distill_queue (cause, project_id, status, attempts, created_at)
                 VALUES ('job_landed', 'alpha', 'pending', 0, '2026-10-05T00:00:00Z')",
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        assert_eq!(
            queued(&pool, "job_landed").await,
            1,
            "NULL ids defeated the once-only index"
        );

        // A different cause for the same job is a different fact and is kept.
        sqlx::query(
            "INSERT OR IGNORE INTO distill_queue (cause, project_id, job_id, status, attempts, created_at)
             VALUES ('job_landed', 'alpha', 1, 'pending', 0, '2026-10-05T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(queued(&pool, "job_landed").await, 2);
        assert_eq!(queued_total(&pool).await, 3);
    }

    /// The trigger rides on the run's own terminal write, so every site that fails a review run
    /// queues it without being edited, and none of the quiet cases may break that write.
    #[tokio::test]
    async fn a_review_that_fails_is_queued_by_the_same_write() {
        let pool = test_pool().await;
        seed_job(&pool, 7, "alpha", 0).await;

        let review = seed_run(&pool, "alpha", "running", Some(7), Some("review")).await;
        let res = sqlx::query("UPDATE runs SET status = 'failed' WHERE id = ?")
            .bind(review)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(res.rows_affected(), 1, "the run's own write must succeed");

        let rows: Vec<(String, String, Option<i64>, Option<i64>, Option<i64>, String)> =
            sqlx::query_as(
                "SELECT cause, project_id, job_id, item_id, run_id, status FROM distill_queue",
            )
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "a failed review queued {} rows", rows.len());
        let (cause, project, job_id, item_id, run_id, status) = &rows[0];
        assert_eq!(cause, Cause::ReviewBlocking.as_str());
        assert_eq!(project, "alpha", "the queue row carries the job's project");
        assert_eq!(*job_id, Some(7));
        assert_eq!(*item_id, None);
        assert_eq!(*run_id, Some(review));
        assert_eq!(status, "pending");

        // Quiet cases: each must leave the queue at exactly one row and its own UPDATE must succeed.
        let implement = seed_run(&pool, "alpha", "running", Some(7), Some("implement")).await;
        let passing = seed_run(&pool, "alpha", "running", Some(7), Some("review")).await;
        let jobless = seed_run(&pool, "alpha", "running", None, Some("review")).await;
        for (id, to) in [
            (implement, "failed"), // not a review
            (passing, "done"),     // a review that did not fail
            (review, "failed"),    // already failed: OLD.status is 'failed'
            (jobless, "failed"),   // a review with no job to name
        ] {
            let res = sqlx::query("UPDATE runs SET status = ? WHERE id = ?")
                .bind(to)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            assert_eq!(res.rows_affected(), 1, "the update of run {id} must succeed");
        }
        assert_eq!(
            queued_total(&pool).await,
            1,
            "a non-review, non-failed, already-failed or job-less run was queued"
        );
    }

    /// The trigger's SQL cannot call Rust, so its cause is a literal that a test pins to the enum.
    #[tokio::test]
    async fn the_review_trigger_speaks_the_cause_vocabulary() {
        let pool = test_pool().await;
        let sql: Option<String> = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'distill_review_blocking'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        let sql = sql.expect("the distill_review_blocking trigger is missing from the schema");
        let literal = format!("'{}'", Cause::ReviewBlocking.as_str());
        assert!(
            sql.contains(&literal),
            "the trigger does not write {literal}, the Rust spelling of the cause"
        );

        let spellings: std::collections::HashSet<&str> =
            CAUSES.iter().map(|c| c.as_str()).collect();
        assert_eq!(CAUSES.len(), 5);
        assert_eq!(spellings.len(), 5, "two causes share a spelling");
        for expected in [
            "job_landed",
            "job_failed",
            "gate_recovered",
            "review_blocking",
            "run_exhausted",
        ] {
            assert!(spellings.contains(expected), "{expected} is not a cause");
        }
    }

    /// `gate_attempts` counts RED gates: a pass after one red is a recovery, and an item is
    /// exhausted only once its reds exceed the job's retries.
    #[tokio::test]
    async fn an_item_verdict_queues_recovery_or_exhaustion_and_nothing_else() {
        let pool = test_pool().await;
        seed_job(&pool, 3, "alpha", 1).await;

        // The job ending is queued with the job's project.
        let mut conn = pool.acquire().await.unwrap();
        enqueue_job_ending_in(&mut *conn, 3).await.unwrap();
        drop(conn);
        let ending: Vec<(String, i64)> = sqlx::query_as(
            "SELECT project_id, job_id FROM distill_queue WHERE cause = 'job_failed'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(ending, vec![("alpha".to_string(), 3)]);

        let recovered = seed_item(&pool, 3, 1, "done", "passed", 1).await;
        let exhausted = seed_item(&pool, 3, 2, "gate_failed", "failed", 2).await;
        let first_time = seed_item(&pool, 3, 3, "done", "passed", 0).await;
        let still_retrying = seed_item(&pool, 3, 4, "gate_failed", "failed", 1).await;

        let mut conn = pool.acquire().await.unwrap();
        for ordinal in 1..=4 {
            enqueue_item_verdict_in(&mut *conn, 3, ordinal).await.unwrap();
        }
        // Writing the same verdict again is not a second cause.
        enqueue_item_verdict_in(&mut *conn, 3, 1).await.unwrap();
        enqueue_item_verdict_in(&mut *conn, 3, 2).await.unwrap();
        drop(conn);

        let items_for = |cause: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (Option<i64>, String)>(
                    "SELECT item_id, project_id FROM distill_queue WHERE cause = ?",
                )
                .bind(cause)
                .fetch_all(&pool)
                .await
                .unwrap()
            }
        };
        assert_eq!(
            items_for(Cause::GateRecovered.as_str()).await,
            vec![(Some(recovered), "alpha".to_string())],
            "only the item that passed after a red recovers"
        );
        assert_eq!(
            items_for(Cause::RunExhausted.as_str()).await,
            vec![(Some(exhausted), "alpha".to_string())],
            "only the item whose reds exceed gate_retries is exhausted"
        );
        for untouched in [first_time, still_retrying] {
            let n: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM distill_queue WHERE item_id = ?")
                    .bind(untouched)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(n, 0, "item {untouched} queued something it should not");
        }
    }

    /// A land's success is a merge of the job's own branch into a branch a person owns; a merge
    /// between `nucleos/` branches, a feature branch, or a request that did not succeed is not.
    #[tokio::test]
    async fn only_a_landed_job_branch_is_queued_as_landed() {
        let pool = test_pool().await;
        seed_job(&pool, 5, "alpha", 0).await;
        seed_job(&pool, 6, "bravo", 0).await;

        let landed = seed_merge(&pool, "alpha", "merge", "nucleos/job-5", "master", "succeeded").await;
        let not_succeeded =
            seed_merge(&pool, "alpha", "merge", "nucleos/job-5", "master", "failed").await;
        let feature = seed_merge(&pool, "alpha", "merge", "feat/x", "master", "succeeded").await;
        let into_nucleos =
            seed_merge(&pool, "alpha", "merge", "nucleos/job-5", "nucleos/staging", "succeeded")
                .await;
        let not_a_merge =
            seed_merge(&pool, "alpha", "rebase", "nucleos/job-5", "master", "succeeded").await;
        // Job 6 belongs to another project, so a request of `alpha` naming it matches nothing.
        let foreign = seed_merge(&pool, "alpha", "merge", "nucleos/job-6", "master", "succeeded").await;

        let mut conn = pool.acquire().await.unwrap();
        for id in [not_succeeded, feature, into_nucleos, not_a_merge, foreign] {
            enqueue_landed_in(&mut *conn, id).await.unwrap();
        }
        drop(conn);
        assert_eq!(
            queued_total(&pool).await,
            0,
            "something that did not land was queued as landed"
        );

        let mut conn = pool.acquire().await.unwrap();
        enqueue_landed_in(&mut *conn, landed).await.unwrap();
        enqueue_landed_in(&mut *conn, landed).await.unwrap();
        drop(conn);
        let rows: Vec<(String, String, Option<i64>)> =
            sqlx::query_as("SELECT cause, project_id, job_id FROM distill_queue")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![(
                Cause::JobLanded.as_str().to_string(),
                "alpha".to_string(),
                Some(5)
            )],
            "a landed job branch is queued once, with its job and project"
        );
    }
}
