//! Tree and land causes of the distiller (spec 2026-11-16-memoria-agentes-captura, F1): how a
//! finished tree, an answered tree decision, a recovered branch and a land of a branch nobody owns
//! reach `distill_queue`, plus the pre-filter and the volume caps that keep the model calls few.

use chrono::{DateTime, Duration, Utc};
use sqlx::{SqliteConnection, SqlitePool};

use super::{Cause, STATUS_PENDING};
use crate::tree_job::store::{self, TreeJob};

/// A store failure as the `sqlx` error the enqueue helpers return.
fn store_error(error: store::StoreError) -> sqlx::Error {
    match error {
        store::StoreError::Db(error) => error,
        other => sqlx::Error::Protocol(other.to_string()),
    }
}

/// The first instant of the UTC day of `now`, as the text `finished_at` is stored in.
fn day_start(now: DateTime<Utc>) -> DateTime<Utc> {
    now.date_naive()
        .and_hms_opt(0, 0, 0)
        .map_or(now, |naive| naive.and_utc())
}

/// The first instant of the UTC day after `now`: when a capped row becomes due again.
pub(super) fn next_day_start(now: DateTime<Utc>) -> DateTime<Utc> {
    day_start(now) + Duration::days(1)
}

/// Whether the manifest holds a non-empty value under `key`.
fn has_extra(job: &TreeJob, key: &str) -> bool {
    job.manifest
        .extra
        .get(key)
        .is_some_and(|value| !value.is_null() && value.as_str().is_none_or(|text| !text.is_empty()))
}

/// Trees (and branches) of one project the distiller reads per UTC day.
pub const DISTILL_TREE_PER_PROJECT_DAY: i64 = 6;
/// How far back the sweep looks for finished trees that never reached the queue.
pub const DISTILL_BACKFILL_DAYS: i64 = 14;
/// Model calls of every cause the distiller makes per UTC day.
pub const DISTILL_MODEL_CALLS_PER_DAY: i64 = 20;
/// Hours a tree waits between two `tree_parked` rows.
pub const TREE_PARKED_COOLOFF_HOURS: i64 = 24;

/// The causes the per-project daily cap counts, next to the branch ones.
pub const TREE_CAUSES: [Cause; 5] = [
    Cause::TreeLanded,
    Cause::TreeFailed,
    Cause::TreeParked,
    Cause::BranchRecovered,
    Cause::BranchLanded,
];

/// Why a finished tree is queued: `TreeFailed` on real evidence of failure, else `TreeLanded`.
pub fn classify_end(job: &TreeJob, bad_nodes: i64) -> Cause {
    let abandoned = job
        .manifest
        .extra
        .get("finish")
        .and_then(|finish| finish.get("status"))
        .and_then(|status| status.as_str())
        == Some("abandoned");
    if abandoned {
        return Cause::TreeFailed;
    }
    if has_extra(job, "land_sha") {
        return Cause::TreeLanded;
    }
    if has_extra(job, "land_failure") || has_extra(job, "land_refusal") || bad_nodes > 0 {
        return Cause::TreeFailed;
    }
    Cause::TreeLanded
}

/// What the gatherer found out about a tree before any model is asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeFacts {
    pub red_failures: usize,
    pub fixing_branches: usize,
    pub answered_decisions: usize,
    pub bad_nodes: usize,
    pub open_item_nodes: usize,
    pub has_diff: bool,
}

/// Whether there is anything to learn from: false closes the row as `nothing: clean run`.
pub fn tree_has_lesson(facts: &TreeFacts) -> bool {
    facts.red_failures > 0
        || facts.fixing_branches > 0
        || facts.answered_decisions > 0
        || facts.bad_nodes > 0
        || facts.open_item_nodes > 0
}

/// What the gathered blocks of a tree say about it, before any model is asked.
pub fn tree_facts(job: &TreeJob, blocks: &super::TreeBlocks) -> TreeFacts {
    // A branch that is `fixing` is mid-repair; a gate entry with a `red` verdict is a repair done.
    let fixing_branches = job
        .manifest
        .branches
        .values()
        .filter(|branch| {
            branch.state == "fixing"
                || branch
                    .gates
                    .iter()
                    .any(|gate| gate.get("verdict").and_then(|v| v.as_str()) == Some("red"))
        })
        .count();
    TreeFacts {
        red_failures: blocks.red_failures.len(),
        fixing_branches,
        answered_decisions: blocks.decisions.len(),
        bad_nodes: blocks.bad_nodes,
        open_item_nodes: blocks.open_item_nodes,
        has_diff: blocks.diff_stat.is_some() || blocks.commits.is_some(),
    }
}

/// The project of a tree, `None` when the tree has none (or is gone).
async fn tree_project(pool: &SqlitePool, tree_id: &str) -> sqlx::Result<Option<String>> {
    let project: Option<Option<String>> =
        sqlx::query_scalar("SELECT project_id FROM tree_jobs WHERE id = ?")
            .bind(tree_id)
            .fetch_optional(pool)
            .await?;
    Ok(project.flatten().filter(|project| !project.is_empty()))
}

/// Queue a finished tree once. `true` when a row was inserted.
pub async fn enqueue_tree_end_in(pool: &SqlitePool, tree_id: &str) -> sqlx::Result<bool> {
    let Some(job) = store::load(pool, tree_id).await.map_err(store_error)? else {
        return Ok(false);
    };
    let Some(project) = job
        .project_id
        .as_deref()
        .filter(|project| !project.is_empty())
    else {
        return Ok(false);
    };
    if job.state != "finished" {
        return Ok(false);
    }
    let bad_nodes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM work_nodes WHERE tree_job_id = ? AND status IN ('failed', 'blocked')",
    )
    .bind(tree_id)
    .fetch_one(pool)
    .await?;
    let cause = classify_end(&job, bad_nodes);
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
             (cause, project_id, tree_id, status, attempts, created_at)
         SELECT ?, ?, ?, ?, 0, ?
          WHERE NOT EXISTS (SELECT 1 FROM distill_queue
                             WHERE tree_id = ? AND cause IN ('tree_landed', 'tree_failed'))",
    )
    .bind(cause.as_str())
    .bind(project)
    .bind(tree_id)
    .bind(STATUS_PENDING)
    .bind(super::now())
    .bind(tree_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(inserted == 1)
}

/// Queue an answered tree decision, at most once per tree per [`TREE_PARKED_COOLOFF_HOURS`].
pub async fn enqueue_tree_parked_in(pool: &SqlitePool, decision_id: &str) -> sqlx::Result<bool> {
    let decision: Option<(Option<String>, Option<String>, String)> =
        sqlx::query_as("SELECT tree_id, node_id, status FROM decisions WHERE id = ?")
            .bind(decision_id)
            .fetch_optional(pool)
            .await?;
    let Some((Some(tree_id), node_id, status)) = decision else {
        return Ok(false);
    };
    if status != "answered" {
        return Ok(false);
    }
    let Some(project) = tree_project(pool, &tree_id).await? else {
        return Ok(false);
    };
    let since = (Utc::now() - Duration::hours(TREE_PARKED_COOLOFF_HOURS)).to_rfc3339();
    let recent: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM distill_queue WHERE cause = 'tree_parked' AND tree_id = ? AND created_at >= ?
          LIMIT 1",
    )
    .bind(&tree_id)
    .bind(since)
    .fetch_optional(pool)
    .await?;
    if recent.is_some() {
        return Ok(false);
    }
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
             (cause, project_id, tree_id, node_id, decision_id, status, attempts, created_at)
         VALUES (?, ?, ?, ?, ?, ?, 0, ?)",
    )
    .bind(Cause::TreeParked.as_str())
    .bind(project)
    .bind(&tree_id)
    .bind(node_id)
    .bind(decision_id)
    .bind(STATUS_PENDING)
    .bind(super::now())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(inserted == 1)
}

/// Queue a tree branch whose worktree failed a verification and later passed one.
pub async fn enqueue_branch_recovered_in(
    pool: &SqlitePool,
    tree_id: &str,
    node_id: &str,
    worktree: &str,
) -> sqlx::Result<bool> {
    let Some(project) = tree_project(pool, tree_id).await? else {
        return Ok(false);
    };
    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status FROM verify_runs WHERE worktree = ? ORDER BY id")
            .bind(worktree)
            .fetch_all(pool)
            .await?;
    let mut failed = false;
    let mut recovered = false;
    for status in &statuses {
        match status.as_str() {
            "failed" => failed = true,
            "passed" if failed => {
                recovered = true;
                break;
            }
            _ => {}
        }
    }
    if !recovered {
        return Ok(false);
    }
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
             (cause, project_id, tree_id, node_id, status, attempts, created_at)
         VALUES (?, ?, ?, ?, ?, 0, ?)",
    )
    .bind(Cause::BranchRecovered.as_str())
    .bind(project)
    .bind(tree_id)
    .bind(node_id)
    .bind(STATUS_PENDING)
    .bind(super::now())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(inserted == 1)
}

/// Queue a succeeded merge that is neither a job's land nor a tree's own merge.
pub async fn enqueue_branch_landed_in(
    conn: &mut SqliteConnection,
    request_id: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
             (cause, project_id, request_id, status, attempts, created_at)
         SELECT ?, v.project_id, v.id, ?, 0, ?
           FROM vcs_requests v
          WHERE v.id = ? AND v.status = 'succeeded' AND v.op = 'merge'
            AND v.project_id <> ''
            AND json_extract(v.args, '$.source') NOT GLOB 'nucleos/job-*'
            AND json_extract(v.args, '$.target') NOT GLOB 'nucleos/*'
            AND json_extract(v.args, '$.source') NOT IN
                (SELECT feature_branch FROM tree_jobs WHERE feature_branch IS NOT NULL)
            AND json_extract(v.args, '$.target') NOT IN
                (SELECT feature_branch FROM tree_jobs WHERE feature_branch IS NOT NULL)",
    )
    .bind(Cause::BranchLanded.as_str())
    .bind(STATUS_PENDING)
    .bind(super::now())
    .bind(request_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Queue the finished trees of the last [`DISTILL_BACKFILL_DAYS`] days that have no row. Returns
/// how many rows were inserted.
pub async fn sweep_trees(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<u64> {
    let since = (now - Duration::days(DISTILL_BACKFILL_DAYS)).to_rfc3339();
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT t.id FROM tree_jobs t
          WHERE t.state = 'finished' AND t.updated_at >= ?
            AND t.project_id IS NOT NULL AND t.project_id <> ''
            AND NOT EXISTS (SELECT 1 FROM distill_queue q
                             WHERE q.tree_id = t.id AND q.cause IN ('tree_landed', 'tree_failed'))
          ORDER BY t.updated_at DESC LIMIT 50",
    )
    .bind(since)
    .fetch_all(pool)
    .await?;
    let mut queued = 0;
    for id in ids {
        if enqueue_tree_end_in(pool, &id).await? {
            queued += 1;
        }
    }
    Ok(queued)
}

/// Model calls made since the start of the UTC day of `now`: rows closed `done` without an error
/// note. A row closed `nothing: ...`, `gone`, `ingest` or `disabled` never asked the model.
pub(super) async fn model_calls_today(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM distill_queue
          WHERE status = 'done' AND error IS NULL AND finished_at >= ?",
    )
    .bind(day_start(now).to_rfc3339())
    .fetch_one(pool)
    .await
}

/// Tree-side rows of `project_id` the model has read since the start of the UTC day of `now`.
pub(super) async fn tree_reads_today(
    pool: &SqlitePool,
    project_id: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM distill_queue
          WHERE project_id = ? AND status = 'done' AND error IS NULL AND finished_at >= ?
            AND cause IN ('tree_landed', 'tree_failed', 'tree_parked', 'branch_recovered',
                          'branch_landed')",
    )
    .bind(project_id)
    .bind(day_start(now).to_rfc3339())
    .fetch_one(pool)
    .await
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, Utc};
    use serde_json::{Value, json};
    use sqlx::SqlitePool;

    use super::{
        TREE_CAUSES, TreeFacts, classify_end, enqueue_branch_landed_in,
        enqueue_branch_recovered_in, enqueue_tree_end_in, enqueue_tree_parked_in, sweep_trees,
        tree_has_lesson,
    };
    use crate::distill::{CAUSES, Cause, claim_next};
    use crate::tree_job::model::Manifest;
    use crate::tree_job::store::{self, TreeJob};

    /// One `distill_queue` row as the tests read it back: cause, tree, node, request, decision.
    type Row = (
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
    );

    // ----- fixtures -----

    /// A tree job in memory; `extra` lands in the manifest next to `finish`, `land_sha` and the like.
    fn tree_job(
        id: &str,
        project: Option<&str>,
        state: &str,
        extra: &[(&str, Value)],
        at: DateTime<Utc>,
    ) -> TreeJob {
        let stamp = at.to_rfc3339();
        let mut manifest = Manifest::new(id, &format!("feat/{id}"), "Ship it", "C:/work", &[]);
        manifest.state = state.to_owned();
        for (key, value) in extra {
            manifest.extra.insert((*key).to_owned(), value.clone());
        }
        TreeJob {
            id: id.to_owned(),
            root_node_id: format!("{id}-root"),
            project_id: project.map(str::to_owned),
            feature_branch: Some(format!("feat/{id}")),
            goal: "Ship it".to_owned(),
            state: state.to_owned(),
            manifest,
            contract: None,
            created_at: stamp.clone(),
            updated_at: stamp,
        }
    }

    async fn seed_tree_at(
        pool: &SqlitePool,
        id: &str,
        project: Option<&str>,
        state: &str,
        extra: &[(&str, Value)],
        at: DateTime<Utc>,
    ) {
        store::save(pool, &tree_job(id, project, state, extra, at))
            .await
            .unwrap();
    }

    /// The tree `job-1`, as of now.
    async fn seed_tree(
        pool: &SqlitePool,
        project: Option<&str>,
        state: &str,
        extra: &[(&str, Value)],
    ) -> String {
        seed_tree_at(pool, "job-1", project, state, extra, Utc::now()).await;
        "job-1".to_owned()
    }

    async fn seed_decision(pool: &SqlitePool, id: &str, tree: &str) {
        sqlx::query(
            "INSERT INTO decisions (id, tree_id, node_id, question, answer, origin, at, status)
             VALUES (?, ?, 'node-api', 'pick A or B', 'Pick A', 'telegram', ?, 'answered')",
        )
        .bind(id)
        .bind(tree)
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_verify(pool: &SqlitePool, worktree: &str, status: &str) {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO verify_runs (project_id, worktree, scope, origin, requested_by, argv,
                                      status, duration_ms, started_at, finished_at)
             VALUES ('alpha', ?, 'full', 'job', 'gate', '[]', ?, 10, ?, ?)",
        )
        .bind(worktree)
        .bind(status)
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_request(
        pool: &SqlitePool,
        op: &str,
        source: &str,
        target: &str,
        status: &str,
    ) -> i64 {
        let args = json!({ "source": source, "target": target }).to_string();
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, status, created_at,
                                       repo_key)
             VALUES (?, ?, 'alpha', 'C:/work', 'run', ?, '2026-11-16T10:00:00Z', 'C:/work')",
        )
        .bind(op)
        .bind(args)
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn rows(pool: &SqlitePool) -> Vec<Row> {
        sqlx::query_as(
            "SELECT cause, tree_id, node_id, request_id, decision_id FROM distill_queue ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    fn landed_extra() -> Vec<(&'static str, Value)> {
        vec![("land_sha", json!("abc123"))]
    }

    // ----- 1, 2: vocabulary and key -----

    #[test]
    fn new_tree_causes_have_stable_wire_names_and_are_in_causes() {
        let expected = [
            (Cause::TreeLanded, "tree_landed"),
            (Cause::TreeFailed, "tree_failed"),
            (Cause::TreeParked, "tree_parked"),
            (Cause::BranchRecovered, "branch_recovered"),
            (Cause::BranchLanded, "branch_landed"),
        ];
        for (cause, wire) in expected {
            assert_eq!(cause.as_str(), wire);
            assert_eq!(Cause::parse(wire), Some(cause));
            assert!(CAUSES.contains(&cause), "{wire} is not in CAUSES");
            assert!(TREE_CAUSES.contains(&cause), "{wire} is not in TREE_CAUSES");
        }
        assert_eq!(CAUSES.len(), 13);
        assert_eq!(TREE_CAUSES.len(), 5);
    }

    #[tokio::test]
    async fn queue_once_key_covers_tree_node_request_and_decision() {
        let pool = crate::testdb::fresh_pool().await;
        // (tree_id, node_id, request_id, decision_id): each differs from the others in one column.
        type Key = (
            Option<&'static str>,
            Option<&'static str>,
            Option<i64>,
            Option<&'static str>,
        );
        let keys: [Key; 5] = [
            (Some("t1"), None, None, None),
            (Some("t2"), None, None, None),
            (Some("t1"), Some("n1"), None, None),
            (Some("t1"), None, Some(7), None),
            (Some("t1"), None, None, Some("d1")),
        ];
        for round in 0..2 {
            for (tree, node, request, decision) in keys {
                sqlx::query(
                    "INSERT OR IGNORE INTO distill_queue
                         (cause, project_id, tree_id, node_id, request_id, decision_id, status,
                          attempts, created_at)
                     VALUES ('tree_landed', 'alpha', ?, ?, ?, ?, 'pending', 0, ?)",
                )
                .bind(tree)
                .bind(node)
                .bind(request)
                .bind(decision)
                .bind(format!("2026-11-16T10:00:0{round}+00:00"))
                .execute(&pool)
                .await
                .unwrap();
            }
        }
        assert_eq!(
            rows(&pool).await.len(),
            5,
            "the second round must be ignored"
        );

        // The claim carries the new columns back.
        let claimed = claim_next(&pool, Utc::now())
            .await
            .unwrap()
            .expect("a pending row");
        assert_eq!(claimed.tree_id.as_deref(), Some("t1"));
        assert_eq!(claimed.node_id, None);
    }

    // ----- 3-6: classify_end -----

    #[test]
    fn classify_end_landed_with_land_sha_is_tree_landed() {
        let job = tree_job(
            "job-1",
            Some("alpha"),
            "finished",
            &landed_extra(),
            Utc::now(),
        );
        let cause = classify_end(&job, 0);
        assert_eq!(cause, Cause::TreeLanded);
        assert_eq!(cause.as_str(), "tree_landed");
        // A land_sha wins over a node that failed on the way.
        assert_eq!(classify_end(&job, 2), Cause::TreeLanded);
    }

    #[test]
    fn classify_end_abandoned_finish_is_tree_failed() {
        let extra = [("finish", json!({ "status": "abandoned" }))];
        let job = tree_job("job-1", Some("alpha"), "finished", &extra, Utc::now());
        let cause = classify_end(&job, 0);
        assert_eq!(cause, Cause::TreeFailed);
        assert_eq!(cause.as_str(), "tree_failed");
    }

    #[test]
    fn classify_end_land_failure_or_failed_node_without_land_sha_is_tree_failed() {
        let failure = [("land_failure", json!("gate red"))];
        let job = tree_job("job-1", Some("alpha"), "finished", &failure, Utc::now());
        assert_eq!(classify_end(&job, 0), Cause::TreeFailed);

        let refusal = [("land_refusal", json!("dirty owner files"))];
        let job = tree_job("job-1", Some("alpha"), "finished", &refusal, Utc::now());
        assert_eq!(classify_end(&job, 0), Cause::TreeFailed);

        let job = tree_job("job-1", Some("alpha"), "finished", &[], Utc::now());
        assert_eq!(classify_end(&job, 1), Cause::TreeFailed);
        assert_eq!(classify_end(&job, 1).as_str(), "tree_failed");
    }

    #[test]
    fn classify_end_held_without_bad_nodes_is_tree_landed() {
        let held = [("landing", json!("held"))];
        let job = tree_job("job-1", Some("alpha"), "finished", &held, Utc::now());
        let cause = classify_end(&job, 0);
        assert_eq!(cause, Cause::TreeLanded);
        assert_eq!(cause.as_str(), "tree_landed");

        let empty = tree_job("job-1", Some("alpha"), "finished", &[], Utc::now());
        assert_eq!(classify_end(&empty, 0).as_str(), "tree_landed");
    }

    // ----- 7-10: enqueue_tree_end_in -----

    #[tokio::test]
    async fn enqueue_tree_end_in_queues_one_tree_landed_row_for_a_landed_tree() {
        let pool = crate::testdb::fresh_pool().await;
        let id = seed_tree(&pool, Some("alpha"), "finished", &landed_extra()).await;

        assert!(enqueue_tree_end_in(&pool, &id).await.unwrap());
        assert!(
            !enqueue_tree_end_in(&pool, &id).await.unwrap(),
            "the second call must not insert"
        );
        assert_eq!(
            rows(&pool).await,
            vec![("tree_landed".to_owned(), Some(id), None, None, None)]
        );
        let project: String = sqlx::query_scalar("SELECT project_id FROM distill_queue")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(project, "alpha");
    }

    #[tokio::test]
    async fn closing_a_finished_tree_twice_leaves_one_row() {
        let pool = crate::testdb::fresh_pool().await;
        let id = seed_tree(&pool, Some("alpha"), "finished", &landed_extra()).await;

        crate::tree_job::obsolete::close_tree_logged(&pool, &id, "r").await;
        crate::tree_job::obsolete::close_tree_logged(&pool, &id, "r").await;

        assert_eq!(
            rows(&pool).await,
            vec![("tree_landed".to_owned(), Some(id), None, None, None)]
        );
    }

    #[tokio::test]
    async fn an_abandoned_tree_queues_tree_failed() {
        let pool = crate::testdb::fresh_pool().await;
        let extra = [("finish", json!({ "status": "abandoned" }))];
        let id = seed_tree(&pool, Some("alpha"), "finished", &extra).await;

        assert!(enqueue_tree_end_in(&pool, &id).await.unwrap());
        assert_eq!(
            rows(&pool).await,
            vec![("tree_failed".to_owned(), Some(id), None, None, None)]
        );
    }

    #[tokio::test]
    async fn a_tree_without_project_queues_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        seed_tree_at(
            &pool,
            "no-project",
            None,
            "finished",
            &landed_extra(),
            Utc::now(),
        )
        .await;
        seed_tree_at(
            &pool,
            "blank",
            Some(""),
            "finished",
            &landed_extra(),
            Utc::now(),
        )
        .await;
        seed_tree_at(
            &pool,
            "with-project",
            Some("alpha"),
            "finished",
            &landed_extra(),
            Utc::now(),
        )
        .await;

        assert!(!enqueue_tree_end_in(&pool, "no-project").await.unwrap());
        assert!(!enqueue_tree_end_in(&pool, "blank").await.unwrap());
        assert!(!enqueue_tree_end_in(&pool, "missing").await.unwrap());
        // Control: the same call queues a tree that has a project.
        assert!(enqueue_tree_end_in(&pool, "with-project").await.unwrap());
        assert_eq!(
            rows(&pool).await,
            vec![(
                "tree_landed".to_owned(),
                Some("with-project".to_owned()),
                None,
                None,
                None
            )]
        );
    }

    // ----- 11-12: parked and recovered -----

    #[tokio::test]
    async fn tree_parked_is_queued_per_decision_and_at_most_once_per_24h() {
        let pool = crate::testdb::fresh_pool().await;
        let id = seed_tree(&pool, Some("alpha"), "open", &[]).await;
        seed_decision(&pool, "d-1", &id).await;
        seed_decision(&pool, "d-2", &id).await;

        assert!(enqueue_tree_parked_in(&pool, "d-1").await.unwrap());
        assert!(
            !enqueue_tree_parked_in(&pool, "d-1").await.unwrap(),
            "the same decision is one row"
        );
        assert!(
            !enqueue_tree_parked_in(&pool, "d-2").await.unwrap(),
            "a second decision of the tree within 24 hours is held back"
        );
        assert_eq!(
            rows(&pool).await,
            vec![(
                "tree_parked".to_owned(),
                Some(id.clone()),
                Some("node-api".to_owned()),
                None,
                Some("d-1".to_owned())
            )]
        );

        let aged = (Utc::now() - Duration::hours(25)).to_rfc3339();
        sqlx::query("UPDATE distill_queue SET created_at = ?")
            .bind(aged)
            .execute(&pool)
            .await
            .unwrap();
        assert!(enqueue_tree_parked_in(&pool, "d-2").await.unwrap());
        assert_eq!(rows(&pool).await.len(), 2);
    }

    #[tokio::test]
    async fn branch_recovered_needs_a_failed_then_passed_verify_run_of_the_branch_worktree() {
        let pool = crate::testdb::fresh_pool().await;
        let id = seed_tree(&pool, Some("alpha"), "open", &[]).await;

        // wt-only-pass: green from the start. wt-only-fail: never recovered.
        // wt-pass-fail: went green, then red. wt-other: its red belongs to another worktree.
        seed_verify(&pool, "wt-only-pass", "passed").await;
        seed_verify(&pool, "wt-only-fail", "failed").await;
        seed_verify(&pool, "wt-pass-fail", "passed").await;
        seed_verify(&pool, "wt-pass-fail", "failed").await;
        seed_verify(&pool, "wt-other", "passed").await;
        seed_verify(&pool, "wt-fixed", "failed").await;
        seed_verify(&pool, "wt-fixed", "passed").await;

        for worktree in ["wt-only-pass", "wt-only-fail", "wt-pass-fail", "wt-other"] {
            assert!(
                !enqueue_branch_recovered_in(&pool, &id, "node-a", worktree)
                    .await
                    .unwrap(),
                "{worktree} must not queue"
            );
        }
        assert!(rows(&pool).await.is_empty());

        assert!(
            enqueue_branch_recovered_in(&pool, &id, "node-a", "wt-fixed")
                .await
                .unwrap()
        );
        assert!(
            !enqueue_branch_recovered_in(&pool, &id, "node-a", "wt-fixed")
                .await
                .unwrap(),
            "one row per tree and node"
        );
        assert_eq!(
            rows(&pool).await,
            vec![(
                "branch_recovered".to_owned(),
                Some(id),
                Some("node-a".to_owned()),
                None,
                None
            )]
        );
    }

    // ----- 13-14: branch_landed -----

    #[tokio::test]
    async fn branch_landed_is_queued_for_a_non_job_non_tree_merge() {
        let pool = crate::testdb::fresh_pool().await;
        let request = seed_request(&pool, "merge", "feature/x", "main", "succeeded").await;

        let mut conn = pool.acquire().await.unwrap();
        enqueue_branch_landed_in(&mut conn, request).await.unwrap();
        enqueue_branch_landed_in(&mut conn, request).await.unwrap();
        drop(conn);

        assert_eq!(
            rows(&pool).await,
            vec![("branch_landed".to_owned(), None, None, Some(request), None)]
        );
        let project: String = sqlx::query_scalar("SELECT project_id FROM distill_queue")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(project, "alpha");
    }

    #[tokio::test]
    async fn branch_landed_skips_job_branches_and_tree_feature_branches() {
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query(
            "INSERT INTO jobs (id, project_id, project_root, status, max_items, gate_retries, created_at)
             VALUES (7, 'alpha', 'C:/work', 'failed', 3, 0, '2026-11-16T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        // The feature branch of this tree is `feat/job-1`.
        seed_tree(&pool, Some("alpha"), "open", &[]).await;

        let skipped = [
            seed_request(&pool, "merge", "nucleos/job-7", "main", "succeeded").await,
            seed_request(&pool, "merge", "feat/job-1", "main", "succeeded").await,
            // A tree's branch merging into its feature branch is not a `--land`.
            seed_request(&pool, "merge", "feature/y", "feat/job-1", "succeeded").await,
            // The target must be outside `nucleos/`.
            seed_request(&pool, "merge", "feature/y", "nucleos/job-9", "succeeded").await,
            seed_request(&pool, "merge", "feature/y", "main", "failed").await,
            seed_request(&pool, "revert", "feature/y", "main", "succeeded").await,
        ];
        let kept = seed_request(&pool, "merge", "feature/z", "main", "succeeded").await;

        let mut conn = pool.acquire().await.unwrap();
        for request in skipped {
            enqueue_branch_landed_in(&mut conn, request).await.unwrap();
        }
        drop(conn);
        assert!(rows(&pool).await.is_empty(), "nothing is queued for these");
        let mut conn = pool.acquire().await.unwrap();
        enqueue_branch_landed_in(&mut conn, kept).await.unwrap();
        drop(conn);

        assert_eq!(
            rows(&pool).await,
            vec![("branch_landed".to_owned(), None, None, Some(kept), None)]
        );
    }

    // ----- 15: the pre-filter -----

    #[test]
    fn tree_has_lesson_truth_table() {
        assert!(!tree_has_lesson(&TreeFacts::default()));
        // A diff alone is not a lesson: a clean run closes as `nothing: clean run`.
        assert!(!tree_has_lesson(&TreeFacts {
            has_diff: true,
            ..TreeFacts::default()
        }));

        let signals = [
            TreeFacts {
                red_failures: 1,
                ..TreeFacts::default()
            },
            TreeFacts {
                fixing_branches: 1,
                ..TreeFacts::default()
            },
            TreeFacts {
                answered_decisions: 1,
                ..TreeFacts::default()
            },
            TreeFacts {
                bad_nodes: 1,
                ..TreeFacts::default()
            },
            TreeFacts {
                open_item_nodes: 1,
                ..TreeFacts::default()
            },
        ];
        for facts in signals {
            assert!(tree_has_lesson(&facts), "{facts:?} carries a lesson");
        }
        assert!(tree_has_lesson(&TreeFacts {
            red_failures: 2,
            fixing_branches: 1,
            answered_decisions: 3,
            bad_nodes: 1,
            open_item_nodes: 1,
            has_diff: true,
        }));
    }

    // ----- 16: the sweep -----

    #[tokio::test]
    async fn sweep_trees_queues_finished_trees_without_a_row_within_14_days_only() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now();
        let recent = now - Duration::days(2);
        let old = now - Duration::days(30);
        let finish =
            |at: DateTime<Utc>, status: &str| json!({ "status": status, "at": at.to_rfc3339() });

        let mut landed = landed_extra();
        landed.push(("finish", finish(recent, "landed")));
        seed_tree_at(
            &pool,
            "a-landed",
            Some("alpha"),
            "finished",
            &landed,
            recent,
        )
        .await;
        let abandoned = [("finish", finish(recent, "abandoned"))];
        seed_tree_at(
            &pool,
            "b-abandoned",
            Some("alpha"),
            "finished",
            &abandoned,
            recent,
        )
        .await;
        // Already has a row: the sweep leaves it alone.
        seed_tree_at(
            &pool,
            "c-queued",
            Some("alpha"),
            "finished",
            &landed,
            recent,
        )
        .await;
        sqlx::query(
            "INSERT OR IGNORE INTO distill_queue (cause, project_id, tree_id, status, attempts, created_at)
             VALUES ('tree_landed', 'alpha', 'c-queued', 'done', 0, ?)",
        )
        .bind(recent.to_rfc3339())
        .execute(&pool)
        .await
        .unwrap();
        // Too old, and still open.
        let mut old_landed = landed_extra();
        old_landed.push(("finish", finish(old, "landed")));
        seed_tree_at(&pool, "d-old", Some("alpha"), "finished", &old_landed, old).await;
        seed_tree_at(&pool, "e-open", Some("alpha"), "open", &[], recent).await;

        assert_eq!(sweep_trees(&pool, now).await.unwrap(), 2);
        // The sweep's insertion order is its own business: compare by tree.
        let mut got = rows(&pool).await;
        got.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(
            got,
            vec![
                (
                    "tree_landed".to_owned(),
                    Some("a-landed".to_owned()),
                    None,
                    None,
                    None
                ),
                (
                    "tree_failed".to_owned(),
                    Some("b-abandoned".to_owned()),
                    None,
                    None,
                    None
                ),
                (
                    "tree_landed".to_owned(),
                    Some("c-queued".to_owned()),
                    None,
                    None,
                    None
                ),
            ]
        );
        assert_eq!(
            sweep_trees(&pool, now).await.unwrap(),
            0,
            "the sweep is idempotent"
        );
    }

    // ----- 17: the daily model-call cap -----

    async fn seed_done(pool: &SqlitePool, job_id: i64, error: Option<&str>, at: DateTime<Utc>) {
        sqlx::query(
            "INSERT INTO distill_queue (cause, project_id, job_id, status, attempts, error, created_at,
                                        finished_at)
             VALUES ('job_landed', 'alpha', ?, 'done', 1, ?, ?, ?)",
        )
        .bind(job_id)
        .bind(error)
        .bind(at.to_rfc3339())
        .bind(at.to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_pending(pool: &SqlitePool, job_id: i64) {
        sqlx::query(
            "INSERT INTO distill_queue (cause, project_id, job_id, status, attempts, created_at)
             VALUES ('job_landed', 'alpha', ?, 'pending', 0, '2026-11-16T00:00:00+00:00')",
        )
        .bind(job_id)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn claim_next_returns_nothing_after_20_model_calls_today() {
        let now = Utc::now();

        let capped = crate::testdb::fresh_pool().await;
        for job in 0..20 {
            seed_done(&capped, job, None, now).await;
        }
        seed_pending(&capped, 100).await;
        assert!(claim_next(&capped, now).await.unwrap().is_none());
        let status: String =
            sqlx::query_scalar("SELECT status FROM distill_queue WHERE job_id = 100")
                .fetch_one(&capped)
                .await
                .unwrap();
        assert_eq!(status, "pending", "the capped row waits for tomorrow");

        // 19 calls, plus rows that never called the model and rows from another day: still room.
        let open = crate::testdb::fresh_pool().await;
        for job in 0..19 {
            seed_done(&open, job, None, now).await;
        }
        for job in 20..25 {
            seed_done(&open, job, Some("nothing: clean run"), now).await;
        }
        seed_done(&open, 30, None, now - Duration::days(2)).await;
        seed_pending(&open, 100).await;
        let row = claim_next(&open, now).await.unwrap().expect("a due row");
        assert_eq!(row.job_id, Some(100));
    }
}
