//! Rework records turned into knowledge, with no model (spec 2026-11-16-memoria-agentes-captura,
//! section 5, F2).
//!
//! A `rework` ledger record carries a controller convention (`payload.root_cause`, `fix` and
//! `lesson`). This module normalises it (pure), keys it by cause, and writes one episodic trial row
//! per cause, reinforcing it when the cause recurs in another task.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

/// The `distill_cause` every row written here carries.
pub const REWORK_CAUSE: &str = "rework";
/// The `schema_meta` key holding the highest `workflow_records.id` already swept.
pub const REWORK_WATERMARK: &str = "rework.captured_upto";
/// The longest a title taken from a root cause may be, in characters.
pub const TITLE_CLIP: usize = 80;

/// What a rework record says, ready to become a row.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub title: String,
    pub body: String,
    pub cause_key: String,
}

/// What `capture_rework` did with one record.
#[derive(Debug, PartialEq)]
pub enum Captured {
    /// A new row, by id.
    Written(i64),
    /// An existing row of the same cause was reinforced.
    Reinforced(i64),
    /// An existing row of the same cause was reinforced and moved to the procedural layer.
    Promoted(i64),
    /// Nothing was written.
    Skipped,
}

/// **Pure.** The candidate a rework record carries, `None` when it carries none.
pub fn rework_candidate(_record: &crate::workflow_records::Record) -> Option<Candidate> {
    None
}

/// **Pure.** The identity of a cause: the hex digest of its normalised text.
pub fn cause_key(_root_cause: &str) -> String {
    String::new()
}

/// Turn the rework record `record_id` into knowledge, or reinforce what it repeats.
pub async fn capture_rework(_pool: &SqlitePool, _record_id: i64) -> sqlx::Result<Captured> {
    Ok(Captured::Skipped)
}

/// Capture every rework record newer than the watermark, at most 200 per call. Returns how many
/// records produced or reinforced a row.
pub async fn sweep_rework(_pool: &SqlitePool, _now: DateTime<Utc>) -> sqlx::Result<usize> {
    Ok(0)
}

/// Insert the row for a new cause; `None` when a live row of that cause already exists.
#[allow(dead_code)]
async fn insert_row_in(
    _tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    _project: &str,
    _candidate: &Candidate,
    _evidence: &str,
    _now: DateTime<Utc>,
) -> sqlx::Result<Option<i64>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_records::{NewRecord, Record};
    use serde_json::{Value, json};

    const PROJECT: &str = "alpha";

    /// An in-memory record for the pure tests.
    fn rework_record(payload: Value) -> Record {
        Record {
            id: 1,
            project_id: PROJECT.to_owned(),
            worktree: None,
            kind: "rework".to_owned(),
            task_id: "t1".to_owned(),
            phase: Some("implement".to_owned()),
            caught_by: Some("review".to_owned()),
            created_at: "2026-11-16T00:00:00+00:00".to_owned(),
            payload: payload.to_string(),
        }
    }

    async fn seed_record(
        pool: &SqlitePool,
        project: &str,
        task: &str,
        caught_by: &str,
        payload: Value,
    ) -> i64 {
        crate::workflow_records::insert(
            pool,
            &NewRecord {
                project_id: project.to_owned(),
                worktree: None,
                kind: "rework".to_owned(),
                task_id: task.to_owned(),
                phase: Some("implement".to_owned()),
                caught_by: Some(caught_by.to_owned()),
                payload: payload.to_string(),
            },
        )
        .await
        .unwrap()
    }

    /// A rework record through plain `insert` (no hook), with its cause, fix and extra payload keys.
    async fn seed_rework(
        pool: &SqlitePool,
        project: &str,
        task: &str,
        root_cause: &str,
        fix: &str,
        extra: &[(&str, Value)],
    ) -> i64 {
        let mut payload = json!({"root_cause": root_cause, "fix": fix});
        for (key, value) in extra {
            payload[*key] = value.clone();
        }
        seed_record(pool, project, task, "review", payload).await
    }

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn knowledge_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM knowledge")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn live_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge
              WHERE cause_key IS NOT NULL AND status IN ('active', 'proposed')",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn notes_of(pool: &SqlitePool, id: i64) -> Vec<String> {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT note FROM knowledge_events WHERE knowledge_id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .flatten()
        .collect()
    }

    /// A dead row for a cause, as an earlier trial or the owner left it.
    async fn plant_dead(pool: &SqlitePool, cause: &str, status: &str, ended_at: &str) -> i64 {
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, cause_key,
                distill_cause, created_at, ended_at)
             VALUES ('episodic', 'project', ?, 'distiller', 'memory', 'Old lesson', 'b', ?, ?,
                     'rework', '2026-01-01T00:00:00+00:00', ?)",
        )
        .bind(PROJECT)
        .bind(status)
        .bind(cause_key(cause))
        .bind(ended_at)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    // ---- 1-5: the pure half ----

    #[test]
    fn rework_candidate_needs_root_cause_and_fix_after_trim_and_ansi() {
        let both = rework_candidate(&rework_record(json!({
            "root_cause": "  the linker held the exe ",
            "fix": "retry after the lock clears"
        })));
        assert!(both.is_some(), "a cause and a fix are enough");

        let ansi_fix = json!({"root_cause": "a cause", "fix": "\u{1b}[31m  \u{1b}[0m"});
        assert_eq!(rework_candidate(&rework_record(ansi_fix)), None);
        let blank_cause = json!({"root_cause": "   ", "fix": "a fix"});
        assert_eq!(rework_candidate(&rework_record(blank_cause)), None);
        let no_fix = json!({"root_cause": "a cause"});
        assert_eq!(rework_candidate(&rework_record(no_fix)), None);
        let not_text = json!({"root_cause": 7, "fix": "a fix"});
        assert_eq!(rework_candidate(&rework_record(not_text)), None);

        let mut metric = rework_record(json!({"root_cause": "a cause", "fix": "a fix"}));
        metric.kind = "metric".to_owned();
        assert_eq!(rework_candidate(&metric), None, "only a rework counts");
    }

    #[test]
    fn rework_candidate_skips_budget_decomposition_and_fixless_rework() {
        let mut record = rework_record(json!({
            "kind": "decomposition",
            "root_cause": "a session ran past the ceiling",
            "fix": "split the task"
        }));
        record.caught_by = Some("budget".to_owned());
        assert_eq!(rework_candidate(&record), None, "the pair is skipped");

        // Positive control: the same record without the decomposition kind is a candidate.
        let mut plain = rework_record(json!({
            "root_cause": "a session ran past the ceiling",
            "fix": "split the task"
        }));
        plain.caught_by = Some("budget".to_owned());
        assert!(
            rework_candidate(&plain).is_some(),
            "caught_by budget alone is not skipped"
        );

        // Only the pair is skipped: a decomposition caught by a person is a candidate.
        let mut reviewed = rework_record(json!({
            "kind": "decomposition",
            "root_cause": "a session ran past the ceiling",
            "fix": "split the task"
        }));
        reviewed.caught_by = Some("review".to_owned());
        assert!(rework_candidate(&reviewed).is_some());

        let fixless = rework_record(json!({"root_cause": "a cause"}));
        assert_eq!(rework_candidate(&fixless), None);
    }

    #[test]
    fn rework_candidate_title_is_the_lesson_else_the_root_cause_clipped_to_80() {
        let with_lesson = rework_candidate(&rework_record(json!({
            "root_cause": "a cause",
            "fix": "a fix",
            "lesson": "  \u{1b}[1mAlways reread the plan\u{1b}[0m  "
        })))
        .unwrap();
        assert_eq!(with_lesson.title, "Always reread the plan");

        let long_cause = "c".repeat(120);
        let clipped = rework_candidate(&rework_record(json!({
            "root_cause": long_cause,
            "fix": "a fix"
        })))
        .unwrap();
        assert_eq!(clipped.title, "c".repeat(TITLE_CLIP));

        let blank_lesson = rework_candidate(&rework_record(json!({
            "root_cause": "a short cause",
            "fix": "a fix",
            "lesson": "   "
        })))
        .unwrap();
        assert_eq!(blank_lesson.title, "a short cause");
    }

    #[test]
    fn rework_candidate_body_is_cause_fix_and_catcher_with_phase() {
        let record = rework_record(json!({"root_cause": "the cause", "fix": "the fix"}));
        let body = rework_candidate(&record).unwrap().body;
        assert_eq!(
            body,
            "Cause: the cause\nFix: the fix\nCaught by: review (implement)"
        );

        let mut phaseless = rework_record(json!({"root_cause": "the cause", "fix": "the fix"}));
        phaseless.phase = None;
        let body = rework_candidate(&phaseless).unwrap().body;
        assert_eq!(body, "Cause: the cause\nFix: the fix\nCaught by: review");
    }

    #[test]
    fn cause_key_ignores_case_spacing_numbers_and_worktree_paths_but_not_the_cause() {
        let key = cause_key("The gate failed in run-12 after 3.5s at src/a.rs:10:5");
        assert_eq!(key.len(), 16, "a hex16 digest: {key:?}");
        assert_eq!(
            key,
            cause_key("the   GATE failed in RUN-99 after 120ms at src/a.rs:77:1"),
            "case, spacing and numbers are not the cause"
        );
        assert_eq!(
            cause_key("it broke at C:\\wt\\run-5\\src\\a.rs"),
            cause_key("it broke at /tmp/job-9/src/a.rs"),
            "the worktree path is not the cause"
        );
        assert_ne!(
            key,
            cause_key("The linker held the exe in run-12 after 3.5s"),
            "another cause is another key"
        );
    }

    #[test]
    fn the_workflow_doc_names_the_rework_convention() {
        let doc = include_str!("../../assets/nucleos-dev/.nucleos/workflow/workflow.md");
        let line = doc
            .lines()
            .find(|line| line.starts_with("Metrics, rework, intent"))
            .expect("the ledger line of the workflow doc");
        for word in ["root_cause", "fix", "lesson"] {
            assert!(line.contains(word), "the rework line must name {word}");
        }
    }

    // ---- 7-9: the writer ----

    type Row = (
        String,
        Option<String>,
        String,
        String,
        i64,
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<String>,
        Option<String>,
    );

    async fn row(pool: &SqlitePool, id: i64) -> Row {
        sqlx::query_as(
            "SELECT source, distill_cause, layer, status, trial, trial_started_at,
                    expires_after_runs, observations, cause_key, evidence
               FROM knowledge WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_rework_with_cause_and_fix_writes_one_episodic_trial_row_without_a_model() {
        let pool = pool().await;
        let record = seed_rework(&pool, PROJECT, "t1", "the linker held the exe", "retry", &[]).await;

        let Captured::Written(id) = capture_rework(&pool, record).await.unwrap() else {
            panic!("a rework with a cause and a fix is written");
        };

        let (source, cause, layer, status, trial, started, expires, observations, key, evidence) =
            row(&pool, id).await;
        assert_eq!(source, "distiller");
        assert_eq!(cause.as_deref(), Some(REWORK_CAUSE));
        assert_eq!(layer, "episodic");
        assert_eq!(status, "active");
        assert_eq!(trial, 1);
        assert!(started.is_some(), "the trial clock is running");
        assert_eq!(expires, None, "the trial sweep owns its expiry");
        assert_eq!(observations, Some(1));
        assert_eq!(key.as_deref(), Some(cause_key("the linker held the exe").as_str()));
        let evidence: Value = serde_json::from_str(&evidence.unwrap()).unwrap();
        assert_eq!(evidence, json!([{"t": "record", "id": record}]));
        assert_eq!(notes_of(&pool, id).await, vec!["created".to_owned()]);

        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM distill_queue")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(queued, 0, "no model, so nothing is queued for one");
    }

    #[tokio::test]
    async fn a_second_rework_of_the_same_cause_from_another_task_reinforces_and_promotes_to_procedural()
     {
        let pool = pool().await;
        let first = seed_rework(&pool, PROJECT, "t1", "The gate failed in run-3", "retry", &[]).await;
        let Captured::Written(id) = capture_rework(&pool, first).await.unwrap() else {
            panic!("the first sighting is written");
        };
        sqlx::query("UPDATE knowledge SET trial_started_at = '2026-01-01T00:00:00+00:00' WHERE id = ?")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();

        let second = seed_rework(&pool, PROJECT, "t2", "the gate FAILED in run-8", "retry", &[]).await;
        assert_eq!(
            capture_rework(&pool, second).await.unwrap(),
            Captured::Promoted(id)
        );

        let (_, _, layer, status, trial, started, _, observations, _, evidence) =
            row(&pool, id).await;
        assert_eq!(observations, Some(2));
        assert_eq!(layer, "procedural");
        assert_eq!(status, "active");
        assert_eq!(trial, 1, "still on trial");
        assert!(
            started.as_deref().is_some_and(|at| at > "2026-01-01T00:00:00+00:00"),
            "the trial clock restarted: {started:?}"
        );
        let evidence: Value = serde_json::from_str(&evidence.unwrap()).unwrap();
        assert_eq!(
            evidence,
            json!([{"t": "record", "id": first}, {"t": "record", "id": second}])
        );
        let notes = notes_of(&pool, id).await;
        assert!(notes.iter().any(|note| note == "reconfirmed"), "{notes:?}");
        assert!(
            notes.iter().any(|note| note.contains("episodic->procedural")),
            "{notes:?}"
        );
        assert_eq!(live_rows(&pool).await, 1, "one live row per cause");
    }

    #[tokio::test]
    async fn a_second_rework_from_the_same_task_reinforces_without_counting() {
        let pool = pool().await;
        let first = seed_rework(&pool, PROJECT, "t1", "a recurring cause", "a fix", &[]).await;
        let Captured::Written(id) = capture_rework(&pool, first).await.unwrap() else {
            panic!("the first sighting is written");
        };
        let second = seed_rework(&pool, PROJECT, "t1", "a recurring cause", "a fix", &[]).await;
        assert_eq!(
            capture_rework(&pool, second).await.unwrap(),
            Captured::Reinforced(id)
        );

        let (_, _, layer, _, trial, _, _, observations, _, evidence) = row(&pool, id).await;
        assert_eq!(observations, Some(1), "the same task is not a recurrence");
        assert_eq!(layer, "episodic");
        assert_eq!(trial, 1);
        let evidence: Value = serde_json::from_str(&evidence.unwrap()).unwrap();
        assert_eq!(
            evidence,
            json!([{"t": "record", "id": first}, {"t": "record", "id": second}])
        );
    }

    #[tokio::test]
    async fn inserting_the_same_cause_twice_does_not_violate_the_unique_index() {
        let pool = pool().await;
        let candidate = Candidate {
            title: "A cause".to_owned(),
            body: "Cause: x\nFix: y\nCaught by: review".to_owned(),
            cause_key: cause_key("a cause"),
        };
        let evidence = r#"[{"t":"record","id":1}]"#;
        let mut tx = pool.begin().await.unwrap();
        let first = insert_row_in(&mut tx, PROJECT, &candidate, evidence, Utc::now())
            .await
            .unwrap();
        assert!(first.is_some(), "the first insert writes the row");
        let second = insert_row_in(&mut tx, PROJECT, &candidate, evidence, Utc::now())
            .await
            .unwrap();
        assert_eq!(second, None, "the second is a conflict, not an error");
        tx.commit().await.unwrap();
        assert_eq!(knowledge_rows(&pool).await, 1);
    }

    // ---- 11-13: what is not written ----

    #[tokio::test]
    async fn a_decomposition_or_fixless_rework_writes_nothing() {
        let pool = pool().await;
        let decomposition = seed_record(
            &pool,
            PROJECT,
            "t1",
            "budget",
            json!({"kind": "decomposition", "root_cause": "too long", "fix": "split"}),
        )
        .await;
        let fixless = seed_rework(&pool, PROJECT, "t2", "a cause without a fix", "  ", &[]).await;
        assert_eq!(
            capture_rework(&pool, decomposition).await.unwrap(),
            Captured::Skipped
        );
        assert_eq!(capture_rework(&pool, fixless).await.unwrap(), Captured::Skipped);
        assert_eq!(knowledge_rows(&pool).await, 0);

        // Positive control: a complete rework in the same database is written.
        let complete = seed_rework(&pool, PROJECT, "t3", "a real cause", "a real fix", &[]).await;
        assert!(matches!(
            capture_rework(&pool, complete).await.unwrap(),
            Captured::Written(_)
        ));
        assert_eq!(knowledge_rows(&pool).await, 1);
    }

    #[tokio::test]
    async fn a_cause_with_a_reverted_or_a_recent_expired_row_writes_nothing() {
        let pool = pool().await;
        let recent = (Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        let reverted = plant_dead(&pool, "cause a", "reverted", "2026-01-02T00:00:00+00:00").await;
        let expired = plant_dead(&pool, "cause b", "expired", &recent).await;

        let blocked_a = seed_rework(&pool, PROJECT, "t1", "cause a", "a fix", &[]).await;
        let blocked_b = seed_rework(&pool, PROJECT, "t2", "cause b", "a fix", &[]).await;
        assert_eq!(capture_rework(&pool, blocked_a).await.unwrap(), Captured::Skipped);
        assert_eq!(capture_rework(&pool, blocked_b).await.unwrap(), Captured::Skipped);
        assert_eq!(knowledge_rows(&pool).await, 2, "only the two planted rows");
        for dead in [reverted, expired] {
            let notes = notes_of(&pool, dead).await;
            assert!(
                notes.iter().any(|note| note == "skipped: tombstone"),
                "the dead row says it blocked a lesson: {notes:?}"
            );
        }

        // Positive control: an unrelated cause is written.
        let free = seed_rework(&pool, PROJECT, "t3", "cause c", "a fix", &[]).await;
        assert!(matches!(
            capture_rework(&pool, free).await.unwrap(),
            Captured::Written(_)
        ));
    }

    #[tokio::test]
    async fn an_expired_row_older_than_the_cooloff_does_not_block_a_new_one() {
        let pool = pool().await;
        let old = (Utc::now() - chrono::Duration::days(45)).to_rfc3339();
        let dead = plant_dead(&pool, "an old cause", "expired", &old).await;

        let record = seed_rework(&pool, PROJECT, "t1", "an old cause", "a fix", &[]).await;
        let Captured::Written(id) = capture_rework(&pool, record).await.unwrap() else {
            panic!("an expired row past the cool-off does not block");
        };
        assert_ne!(id, dead);
        let status: String = sqlx::query_scalar("SELECT status FROM knowledge WHERE id = ?")
            .bind(dead)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "expired");
        assert_eq!(live_rows(&pool).await, 1);
    }

    // ---- 14: the sweep ----

    #[tokio::test]
    async fn sweeping_twice_neither_duplicates_nor_double_counts() {
        let pool = pool().await;
        seed_rework(&pool, PROJECT, "t1", "cause x", "a fix", &[]).await;
        seed_rework(&pool, PROJECT, "t2", "cause x", "a fix", &[]).await;
        let last = seed_rework(&pool, PROJECT, "t3", "cause y", "a fix", &[]).await;

        let first = sweep_rework(&pool, Utc::now()).await.unwrap();
        assert_eq!(first, 3, "every record produced or reinforced a row");
        assert_eq!(live_rows(&pool).await, 2);
        let counts = |key: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<i64>>(
                    "SELECT observations FROM knowledge WHERE cause_key = ?",
                )
                .bind(key)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        assert_eq!(counts(cause_key("cause x")).await, Some(2));
        assert_eq!(counts(cause_key("cause y")).await, Some(1));
        let watermark = || async {
            sqlx::query_scalar::<_, String>("SELECT value FROM schema_meta WHERE key = ?")
                .bind(REWORK_WATERMARK)
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        assert_eq!(watermark().await, last.to_string());

        assert_eq!(sweep_rework(&pool, Utc::now()).await.unwrap(), 0);
        assert_eq!(live_rows(&pool).await, 2);
        assert_eq!(counts(cause_key("cause x")).await, Some(2));

        // A record written later is picked up by the next sweep.
        let later = seed_rework(&pool, PROJECT, "t4", "cause x", "a fix", &[]).await;
        assert_eq!(sweep_rework(&pool, Utc::now()).await.unwrap(), 1);
        assert_eq!(counts(cause_key("cause x")).await, Some(3));
        assert_eq!(live_rows(&pool).await, 2);
        assert_eq!(watermark().await, later.to_string());
    }
}
