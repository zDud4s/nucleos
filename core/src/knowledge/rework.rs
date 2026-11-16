//! Rework records turned into knowledge, with no model (spec 2026-11-16-memoria-agentes-captura,
//! section 5, F2).
//!
//! A `rework` ledger record carries a controller convention (`payload.root_cause`, `fix` and
//! `lesson`). This module normalises it (pure), keys it by cause, and writes one episodic trial row
//! per cause, reinforcing it when the cause recurs in another task.

use chrono::{DateTime, Utc};
use sha2::Digest;
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

/// A payload string, cleaned of ANSI and trimmed; `None` when absent, not text or empty.
fn clean_text(value: Option<&serde_json::Value>) -> Option<String> {
    let text = value?.as_str()?;
    let cleaned = super::signature::strip_ansi(text).trim().to_owned();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// The first `TITLE_CLIP` characters of `text`, trimmed, with no ellipsis.
fn clip_title(text: &str) -> String {
    text.chars().take(TITLE_CLIP).collect::<String>().trim().to_owned()
}

/// **Pure.** The candidate a rework record carries, `None` when it carries none.
pub fn rework_candidate(record: &crate::workflow_records::Record) -> Option<Candidate> {
    if record.kind != "rework" {
        return None;
    }
    let payload: serde_json::Value = serde_json::from_str(&record.payload).ok()?;
    let object = payload.as_object()?;
    let root_cause = clean_text(object.get("root_cause"))?;
    let fix = clean_text(object.get("fix"))?;
    let caught_by = record
        .caught_by
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let decomposition =
        object.get("kind").and_then(serde_json::Value::as_str) == Some("decomposition");
    if decomposition && caught_by == Some("budget") {
        return None;
    }

    let title = clean_text(object.get("lesson"))
        .map(|lesson| clip_title(&lesson))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| clip_title(&root_cause));
    let mut body = format!("Cause: {root_cause}\nFix: {fix}");
    if let Some(catcher) = caught_by {
        body.push_str("\nCaught by: ");
        body.push_str(catcher);
        if let Some(phase) = record
            .phase
            .as_deref()
            .map(str::trim)
            .filter(|phase| !phase.is_empty())
        {
            body.push_str(&format!(" ({phase})"));
        }
    }
    Some(Candidate {
        title,
        body,
        cause_key: cause_key(&root_cause),
    })
}

/// **Pure.** The identity of a cause: the hex digest of its normalised text.
pub fn cause_key(root_cause: &str) -> String {
    let normalised = super::signature::normalise_line(&root_cause.to_lowercase());
    let text = normalised.split_whitespace().collect::<Vec<_>>().join(" ");
    super::signature::hex16(&sha2::Sha256::digest(text.as_bytes()))
}

/// A live row of a cause: id, layer, status, evidence.
type LiveRow = (i64, String, String, Option<String>);

async fn live_row_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    project: &str,
    key: &str,
) -> sqlx::Result<Option<LiveRow>> {
    sqlx::query_as(
        "SELECT id, layer, status, evidence FROM knowledge
          WHERE scope_kind = 'project' AND scope_id = ? AND cause_key = ?
            AND status IN ('active', 'proposed')
          ORDER BY id DESC LIMIT 1",
    )
    .bind(project)
    .bind(key)
    .fetch_optional(&mut **tx)
    .await
}

/// The newest retired row of a cause that still blocks a new lesson, if any.
async fn tombstoned_cause_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    project: &str,
    key: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<i64>> {
    let cutoff = (now - chrono::Duration::days(super::TRIAL_COOLOFF_DAYS)).to_rfc3339();
    sqlx::query_scalar(
        "SELECT id FROM knowledge
          WHERE scope_kind = 'project' AND scope_id = ? AND cause_key = ?
            AND (status IN ('reverted', 'rejected', 'superseded')
                 OR (status = 'expired' AND ended_at > ?))
          ORDER BY id DESC LIMIT 1",
    )
    .bind(project)
    .bind(key)
    .bind(cutoff)
    .fetch_optional(&mut **tx)
    .await
}

/// Turn the rework record `record_id` into knowledge, or reinforce what it repeats.
pub async fn capture_rework(pool: &SqlitePool, record_id: i64) -> sqlx::Result<Captured> {
    let record: Option<crate::workflow_records::Record> = sqlx::query_as(
        "SELECT id, project_id, worktree, kind, task_id, phase, caught_by, created_at, payload
           FROM workflow_records WHERE id = ?",
    )
    .bind(record_id)
    .fetch_optional(pool)
    .await?;
    let Some(record) = record else {
        return Ok(Captured::Skipped);
    };
    let Some(candidate) = rework_candidate(&record) else {
        return Ok(Captured::Skipped);
    };
    if record.project_id.is_empty() || record.project_id == "unknown" {
        return Ok(Captured::Skipped);
    }

    let now = Utc::now();
    let evidence = serde_json::json!([{"t": "record", "id": record.id}]).to_string();
    let mut tx = pool.begin().await?;
    let outcome = capture_in(&mut tx, &record, &candidate, &evidence, now).await?;
    tx.commit().await?;
    Ok(outcome)
}

async fn capture_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    record: &crate::workflow_records::Record,
    candidate: &Candidate,
    evidence: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Captured> {
    let project = record.project_id.as_str();
    if let Some(dead) = tombstoned_cause_in(tx, project, &candidate.cause_key, now).await? {
        super::note_tombstone_skip_in(tx, dead).await?;
        return Ok(Captured::Skipped);
    }
    if let Some(live) = live_row_in(tx, project, &candidate.cause_key).await? {
        return reinforce_in(tx, record, live, evidence).await;
    }
    if let Some(id) = insert_row_in(tx, project, candidate, evidence, now).await? {
        return Ok(Captured::Written(id));
    }
    // Lost the race to a live row of the same cause: reinforce it instead.
    match live_row_in(tx, project, &candidate.cause_key).await? {
        Some(live) => reinforce_in(tx, record, live, evidence).await,
        None => Ok(Captured::Skipped),
    }
}

/// Merge a repeat into the live row; another task's repeat also counts and may promote it.
async fn reinforce_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    record: &crate::workflow_records::Record,
    live: LiveRow,
    evidence: &str,
) -> sqlx::Result<Captured> {
    let (id, layer, status, stored) = live;
    let seen = stored.clone().unwrap_or_else(|| "[]".to_owned());
    // Idempotent: a record already in the evidence has been counted.
    let already: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM json_each(?)
          WHERE json_extract(value, '$.t') = 'record' AND json_extract(value, '$.id') = ?",
    )
    .bind(&seen)
    .bind(record.id)
    .fetch_one(&mut **tx)
    .await?;
    if already > 0 {
        return Ok(Captured::Skipped);
    }
    let same_task: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workflow_records
          WHERE project_id = ? AND task_id = ?
            AND id IN (SELECT json_extract(value, '$.id') FROM json_each(?)
                        WHERE json_extract(value, '$.t') = 'record')",
    )
    .bind(&record.project_id)
    .bind(&record.task_id)
    .bind(&seen)
    .fetch_one(&mut **tx)
    .await?;

    super::propose::reconfirm_row_in(tx, id, &layer, &status, stored, evidence).await?;
    if same_task > 0 {
        return Ok(Captured::Reinforced(id));
    }
    sqlx::query("UPDATE knowledge SET observations = COALESCE(observations, 0) + 1 WHERE id = ?")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    let observations: i64 =
        sqlx::query_scalar("SELECT COALESCE(observations, 0) FROM knowledge WHERE id = ?")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    if observations >= 2 && super::promote_layer_in(tx, id, super::Layer::Procedural).await? {
        return Ok(Captured::Promoted(id));
    }
    Ok(Captured::Reinforced(id))
}

/// Capture every rework record newer than the watermark, at most 200 per call. Returns how many
/// records produced or reinforced a row.
pub async fn sweep_rework(pool: &SqlitePool, _now: DateTime<Utc>) -> sqlx::Result<usize> {
    let mark: i64 = sqlx::query_scalar::<_, String>("SELECT value FROM schema_meta WHERE key = ?")
        .bind(REWORK_WATERMARK)
        .fetch_optional(pool)
        .await?
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM workflow_records WHERE kind = 'rework' AND id > ? ORDER BY id LIMIT 200",
    )
    .bind(mark)
    .fetch_all(pool)
    .await?;

    let mut upto = mark;
    let mut captured = 0;
    for id in ids {
        match capture_rework(pool, id).await {
            Ok(Captured::Skipped) => {}
            Ok(_) => captured += 1,
            Err(error) => {
                tracing::warn!(%error, record = id, "could not capture a rework record");
                break;
            }
        }
        upto = id;
    }
    if upto > mark {
        sqlx::query("INSERT OR REPLACE INTO schema_meta (key, value) VALUES (?, ?)")
            .bind(REWORK_WATERMARK)
            .bind(upto.to_string())
            .execute(pool)
            .await?;
    }
    Ok(captured)
}

/// Insert the row for a new cause; `None` when a live row of that cause already exists.
async fn insert_row_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    project: &str,
    candidate: &Candidate,
    evidence: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<i64>> {
    let at = now.to_rfc3339();
    let fingerprint = super::title_fingerprint(&candidate.title);
    let result = sqlx::query(
        "INSERT INTO knowledge
           (layer, scope_kind, scope_id, source, kind, title, body, fingerprint, evidence,
            distill_cause, cause_key, observations, status, trial, trial_started_at,
            created_at, activated_at, last_confirmed_at)
         VALUES ('episodic', 'project', ?, 'distiller', 'memory', ?, ?, ?, ?, 'rework', ?, 1,
                 'active', 1, ?, ?, ?, ?)
         ON CONFLICT DO NOTHING",
    )
    .bind(project)
    .bind(&candidate.title)
    .bind(&candidate.body)
    .bind(fingerprint)
    .bind(evidence)
    .bind(&candidate.cause_key)
    .bind(&at)
    .bind(&at)
    .bind(&at)
    .bind(&at)
    .execute(&mut **tx)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    let id = result.last_insert_rowid();
    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'active', 'created', ?)",
    )
    .bind(id)
    .bind(&at)
    .execute(&mut **tx)
    .await?;
    Ok(Some(id))
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
