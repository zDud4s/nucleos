//! The worker half of the tree and land causes (spec 2026-11-16-memoria-agentes-captura, F1): what
//! happens to a claimed `tree_landed`, `tree_failed`, `tree_parked`, `branch_recovered` or
//! `branch_landed` row once `process_one_with` hands it over.

use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;

use super::tree::{
    DISTILL_BACKFILL_DAYS, DISTILL_TREE_PER_PROJECT_DAY, next_day_start, tree_facts,
    tree_has_lesson, tree_reads_today,
};
use super::{
    Cause, DOSSIER_CEILING, Item, QueueRow, STATUS_FAILED, STATUS_PENDING, STATUS_RUNNING, Subject,
    TreeBlocks, Written, ask, close_row, embed_items, extraction_prompt, fail, items_format,
    parse_items, standing_for, tree_dossier, tree_gather, write_one_item_at,
};
use crate::embed::Embedder;
use crate::map::intent::Extractor;

/// Close a row as `done`, keeping `note` (`nothing: ...`) as the reason no model was asked.
async fn close(pool: &SqlitePool, row: &QueueRow, now: DateTime<Utc>, note: &str) {
    let closed = async {
        let mut tx = pool.begin().await?;
        close_row(&mut tx, row.id, now, Some(note)).await?;
        tx.commit().await
    }
    .await;
    if let Err(error) = closed {
        tracing::warn!(%error, "distillation: could not close tree row {}", row.id);
    }
}

/// Fail a row whose subject was deleted: nothing a retry can fix.
async fn gone(pool: &SqlitePool, row: &QueueRow, now: DateTime<Utc>, message: &str) {
    let written = sqlx::query(
        "UPDATE distill_queue SET status = ?, error = ?, finished_at = ?
          WHERE id = ? AND status = ?",
    )
    .bind(STATUS_FAILED)
    .bind(message)
    .bind(now.to_rfc3339())
    .bind(row.id)
    .bind(STATUS_RUNNING)
    .execute(pool)
    .await;
    if let Err(error) = written {
        tracing::warn!(%error, "distillation: could not fail tree row {}", row.id);
    }
}

/// Put a capped row back in line for the start of the next UTC day. `attempts` stays: a deferral
/// is not a failed try.
async fn defer(pool: &SqlitePool, row: &QueueRow, now: DateTime<Utc>) {
    let written = sqlx::query(
        "UPDATE distill_queue SET status = ?, not_before = ? WHERE id = ? AND status = ?",
    )
    .bind(STATUS_PENDING)
    .bind(next_day_start(now).to_rfc3339())
    .bind(row.id)
    .bind(STATUS_RUNNING)
    .execute(pool)
    .await;
    if let Err(error) = written {
        tracing::warn!(%error, "distillation: could not defer tree row {}", row.id);
    }
}

/// Whether the row has waited longer than the sweep's backfill window.
async fn is_stale(pool: &SqlitePool, row: &QueueRow, now: DateTime<Utc>) -> bool {
    let created: Option<String> =
        sqlx::query_scalar("SELECT created_at FROM distill_queue WHERE id = ?")
            .bind(row.id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    created
        .and_then(|text| DateTime::parse_from_rfc3339(&text).ok())
        .is_some_and(|at| at.with_timezone(&Utc) < now - Duration::days(DISTILL_BACKFILL_DAYS))
}

/// What a row reads: its blocks, and what to name as evidence.
struct Gathered {
    blocks: TreeBlocks,
    lesson: bool,
    /// The `{"t":..,"id":..}` element that names the subject.
    subject: serde_json::Value,
    /// Why a row without a lesson closes.
    nothing: &'static str,
    /// The text the reasoning names.
    label: String,
}

/// Read the blocks of the row's subject. `Ok(None)` means the subject is gone.
async fn gather(
    pool: &SqlitePool,
    row: &QueueRow,
    cause: Cause,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<Gathered>> {
    if cause == Cause::BranchLanded {
        let Some(request_id) = row.request_id else {
            return Ok(None);
        };
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM vcs_requests WHERE id = ?")
            .bind(request_id)
            .fetch_optional(pool)
            .await?;
        if exists.is_none() {
            return Ok(None);
        }
        let blocks =
            tree_gather::gather_request_blocks(pool, request_id, &row.project_id, now).await?;
        let lesson = blocks.diff_stat.is_some() || blocks.commits.is_some();
        return Ok(Some(Gathered {
            blocks,
            lesson,
            subject: serde_json::json!({"t": "request", "id": request_id}),
            nothing: "nothing: no diff",
            label: format!("request {request_id}"),
        }));
    }
    let Some(tree_id) = row.tree_id.as_deref() else {
        return Ok(None);
    };
    let job = match crate::tree_job::store::load(pool, tree_id).await {
        Ok(Some(job)) => job,
        Ok(None) => return Ok(None),
        Err(crate::tree_job::store::StoreError::Db(error)) => return Err(error),
        // A manifest nobody can read is as good as gone for the distiller.
        Err(_) => return Ok(None),
    };
    let blocks = tree_gather::gather_blocks(pool, &job, row.node_id.as_deref(), now).await?;
    let lesson = tree_has_lesson(&tree_facts(&job, &blocks));
    Ok(Some(Gathered {
        blocks,
        lesson,
        subject: serde_json::json!({"t": "tree", "id": tree_id}),
        nothing: "nothing: clean run",
        label: format!("tree {tree_id}"),
    }))
}

/// Write the extracted items and close the row, all in one transaction. Returns how many items
/// were recorded or proposed.
#[allow(clippy::too_many_arguments)]
async fn write(
    pool: &SqlitePool,
    row: &QueueRow,
    cause: Cause,
    gathered: &Gathered,
    runs: &[i64],
    verify_runs: &[i64],
    items: &[Item],
    vectors: &[Option<Vec<f32>>],
    model: Option<&str>,
    now: DateTime<Utc>,
) -> sqlx::Result<usize> {
    let mut evidence_list = vec![gathered.subject.clone()];
    evidence_list.extend(
        runs.iter()
            .map(|r| serde_json::json!({"t": "run", "id": r})),
    );
    evidence_list.extend(
        verify_runs
            .iter()
            .map(|r| serde_json::json!({"t": "verify_run", "id": r})),
    );
    let evidence = serde_json::Value::Array(evidence_list).to_string();
    let reasoning = format!("distilled from {} ({})", gathered.label, cause.as_str());
    let subject = Subject {
        cause,
        project_id: &row.project_id,
        evidence: &evidence,
        reasoning: &reasoning,
        // No owner text rides these rows.
        sources: &[],
        model,
    };
    let mut tx = pool.begin().await?;
    let mut proposed_any = false;
    let mut kept = 0usize;
    for (index, item) in items.iter().enumerate() {
        let vector = vectors.get(index).and_then(|v| v.as_deref());
        match write_one_item_at(&mut tx, &subject, item, vector, now).await? {
            Written::Skipped => {}
            Written::Recorded => kept += 1,
            Written::Proposed => {
                proposed_any = true;
                kept += 1;
            }
        }
    }
    close_row(&mut tx, row.id, now, None).await?;
    tx.commit().await?;
    // Only after the commit: a waker must never see a proposal the database does not hold yet.
    if proposed_any {
        nucleos_base::wake::bump(nucleos_base::wake::Topic::Proposals);
    }
    Ok(kept)
}

/// Distil one tree-side row: close it as `nothing: ...` when there is nothing to learn, defer it
/// when the project's daily cap is spent, fail it as `gone` when its subject was deleted, else ask
/// the model and write what it answers.
pub(super) async fn process(
    pool: &SqlitePool,
    asked: Extractor<'_>,
    embedder: Option<&dyn Embedder>,
    row: &QueueRow,
    now: DateTime<Utc>,
) {
    let Some(cause) = Cause::parse(&row.cause) else {
        fail(pool, row, "vocabulary", "unknown cause", now).await;
        return;
    };
    if is_stale(pool, row, now).await {
        close(pool, row, now, "nothing: stale").await;
        return;
    }
    if row.project_id.is_empty() {
        close(pool, row, now, "nothing: no project").await;
        return;
    }
    match tree_reads_today(pool, &row.project_id, now).await {
        Ok(read) if read >= DISTILL_TREE_PER_PROJECT_DAY => {
            defer(pool, row, now).await;
            return;
        }
        Ok(_) => {}
        Err(_) => {
            fail(pool, row, "db", "counting the day's reads failed", now).await;
            return;
        }
    }
    let gathered = match gather(pool, row, cause, now).await {
        Ok(Some(gathered)) => gathered,
        Ok(None) => {
            gone(pool, row, now, "gone: the tree no longer exists").await;
            return;
        }
        Err(_) => {
            fail(pool, row, "db", "reading the tree failed", now).await;
            return;
        }
    };
    if !gathered.lesson {
        close(pool, row, now, gathered.nothing).await;
        return;
    }
    let built = tree_dossier(&gathered.blocks, DOSSIER_CEILING);
    let answer = match ask(
        asked,
        extraction_prompt(cause, &built.text),
        &standing_for(cause),
        items_format(),
    )
    .await
    {
        Ok(answer) => answer,
        Err(error) => {
            fail(pool, row, "model", &format!("{:?}", error.kind()), now).await;
            return;
        }
    };
    let items = match parse_items(&answer) {
        Ok(items) => items,
        Err(_) => {
            fail(pool, row, "parse", "the answer is not a JSON array", now).await;
            return;
        }
    };
    let vectors = embed_items(embedder, &items).await;
    let model = embedder.map(|e| e.model());
    match write(
        pool,
        row,
        cause,
        &gathered,
        &built.runs,
        &built.verify_runs,
        &items,
        &vectors,
        model,
        now,
    )
    .await
    {
        Ok(written) => tracing::info!(
            "distillation: tree row {} ({}): parsed {}, written {written}",
            row.id,
            cause.as_str(),
            items.len()
        ),
        Err(_) => fail(pool, row, "db", "writing the learnings failed", now).await,
    }
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

        assert_eq!(
            prompt_of(&runner),
            None,
            "a clean run never reaches the model"
        );
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

    #[tokio::test]
    async fn a_tree_lesson_cites_a_failed_verification_as_verify_run() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now();
        let extra = [("finish", json!({ "status": "abandoned" }))];
        seed_tree(&pool, "job-1", &extra, now).await;
        seed_work_node(&pool, "node-api", "job-1", "failed").await;
        let mut job = store::load(&pool, "job-1").await.unwrap().unwrap();
        let mut branch = crate::tree_job::model::Branch::new("api", &[], "leaf", None);
        branch.worktree = Some("C:/wt/api".to_owned());
        job.manifest.branches.insert("api".to_owned(), branch);
        store::update(&pool, &job).await.unwrap();
        let stamp = now.to_rfc3339();
        let verify_id = sqlx::query(
            "INSERT INTO verify_runs (project_id, worktree, scope, origin, requested_by, argv,
                                      status, duration_ms, started_at, finished_at, output_tail)
             VALUES ('alpha', 'C:/wt/api', 'full', 'job', 'gate', '[]', 'failed', 10, ?, ?,
                     'error: gate failed on api')",
        )
        .bind(&stamp)
        .bind(&stamp)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        seed_tree_row(&pool, "tree_failed", "job-1", None, &stamp).await;
        let runner = answering(
            r#"[{"layer":"procedural","title":"Fix the api gate","body":"Cause X; do Y","files":[]}]"#,
        );

        claim_and_process(&pool, &runner, now).await;

        let evidence: String =
            sqlx::query_scalar("SELECT evidence FROM knowledge WHERE title = 'Fix the api gate'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let evidence: Vec<Value> = serde_json::from_str(&evidence).unwrap();
        assert!(
            evidence
                .iter()
                .any(|e| e["t"] == "verify_run" && e["id"] == verify_id),
            "the failed verification is cited as verify_run: {evidence:?}"
        );
        assert!(
            !evidence
                .iter()
                .any(|e| e["t"] == "run" && e["id"] == verify_id),
            "and not as a run: {evidence:?}"
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

        assert_eq!(
            prompt_of(&runner),
            None,
            "a deferred row does not call the model"
        );
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
        assert!(
            prompt_of(&runner).is_some(),
            "the next day it reaches the model"
        );
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

        assert_eq!(
            prompt_of(&runner),
            None,
            "a stale row does not call the model"
        );
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

        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT cause, status FROM distill_queue WHERE tree_id = 'job-1'")
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
