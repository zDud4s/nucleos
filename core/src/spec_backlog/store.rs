//! Spec backlog storage: reads and writes over `specs`, `spec_criteria`, `spec_deps` and
//! `spec_questions`, and the owner's single list of decisions.
//!
//! A function that holds a transaction runs every query on it, never on the pool: the test pool has a
//! single connection.

use std::fmt;

use chrono::{DateTime, Utc};
use rand::RngExt;
use sqlx::SqlitePool;

use super::model::{
    spec_id, Criterion, CriterionKind, CriterionStatus, Dep, DepSource, NewCriterion,
    OwnerDecision, Question, Spec, SpecKind, SpecStatus,
};

#[derive(Debug)]
pub enum StoreError {
    NotFound(String),
    /// A stored value outside its closed vocabulary.
    Invalid(String),
    Db(sqlx::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NotFound(id) => write!(f, "{id} not found"),
            StoreError::Invalid(what) => write!(f, "invalid stored value: {what}"),
            StoreError::Db(error) => write!(f, "database error: {error}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<sqlx::Error> for StoreError {
    fn from(error: sqlx::Error) -> Self {
        StoreError::Db(error)
    }
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

#[derive(sqlx::FromRow)]
struct SpecRow {
    id: String,
    project_id: Option<String>,
    path: String,
    title: String,
    content_sha: String,
    kind: String,
    status: String,
    status_reason: Option<String>,
    tree_id: Option<String>,
    last_audited_sha: Option<String>,
    created_at: String,
    updated_at: String,
}

impl SpecRow {
    fn into_spec(self) -> Result<Spec, StoreError> {
        Ok(Spec {
            kind: SpecKind::parse(&self.kind)
                .ok_or_else(|| StoreError::Invalid(format!("spec kind {:?}", self.kind)))?,
            status: SpecStatus::parse(&self.status)
                .ok_or_else(|| StoreError::Invalid(format!("spec status {:?}", self.status)))?,
            id: self.id,
            project_id: self.project_id,
            path: self.path,
            title: self.title,
            content_sha: self.content_sha,
            status_reason: self.status_reason,
            tree_id: self.tree_id,
            last_audited_sha: self.last_audited_sha,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

const SELECT_SPEC: &str = "SELECT id, project_id, path, title, content_sha, kind, status, \
     status_reason, tree_id, last_audited_sha, created_at, updated_at FROM specs WHERE id = ?";

pub async fn get_spec(pool: &SqlitePool, id: &str) -> Result<Option<Spec>, StoreError> {
    let row: Option<SpecRow> = sqlx::query_as(SELECT_SPEC)
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(SpecRow::into_spec).transpose()
}

/// Registers a spec file or refreshes it. The id is derived from project and path, so a second call
/// finds the same row (also for a NULL project, where the unique index sees every row as distinct).
/// A changed `content_sha` sends the spec back to `discovered` and marks its criteria `stale`
/// (their own `spec_sha` stays, naming the version they were extracted from); an unchanged one
/// leaves status and criteria alone. `created_at`, `tree_id` and `last_audited_sha` are never touched.
pub async fn upsert_by_path(
    pool: &SqlitePool,
    project_id: Option<&str>,
    path: &str,
    title: &str,
    content_sha: &str,
) -> Result<Spec, StoreError> {
    let id = spec_id(project_id, path);
    let stamp = now();
    let mut tx = pool.begin().await?;

    let existing: Option<String> = sqlx::query_scalar("SELECT content_sha FROM specs WHERE id = ?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?;
    match existing {
        None => {
            sqlx::query(
                "INSERT INTO specs (id, project_id, path, title, content_sha, kind, status, \
                 created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'unknown', 'discovered', ?, ?)",
            )
            .bind(&id)
            .bind(project_id)
            .bind(path.replace('\\', "/"))
            .bind(title)
            .bind(content_sha)
            .bind(&stamp)
            .bind(&stamp)
            .execute(&mut *tx)
            .await?;
        }
        Some(stored) if stored != content_sha => {
            sqlx::query(
                "UPDATE specs SET title = ?, content_sha = ?, status = 'discovered', \
                 updated_at = ? WHERE id = ?",
            )
            .bind(title)
            .bind(content_sha)
            .bind(&stamp)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE spec_criteria SET status = 'stale' WHERE spec_id = ?")
                .bind(&id)
                .execute(&mut *tx)
                .await?;
        }
        Some(_) => {
            sqlx::query("UPDATE specs SET title = ?, updated_at = ? WHERE id = ?")
                .bind(title)
                .bind(&stamp)
                .bind(&id)
                .execute(&mut *tx)
                .await?;
        }
    }

    let row: SpecRow = sqlx::query_as(SELECT_SPEC)
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    row.into_spec()
}

#[derive(sqlx::FromRow)]
struct CriterionRow {
    id: i64,
    spec_id: String,
    ordinal: i64,
    text: String,
    kind: String,
    verify_hint: Option<String>,
    status: String,
    evidence: Option<String>,
    reviewer_run_id: Option<String>,
    spec_sha: String,
    checked_at: Option<String>,
}

impl CriterionRow {
    fn into_criterion(self) -> Result<Criterion, StoreError> {
        Ok(Criterion {
            kind: CriterionKind::parse(&self.kind)
                .ok_or_else(|| StoreError::Invalid(format!("criterion kind {:?}", self.kind)))?,
            status: CriterionStatus::parse(&self.status).ok_or_else(|| {
                StoreError::Invalid(format!("criterion status {:?}", self.status))
            })?,
            id: self.id,
            spec_id: self.spec_id,
            ordinal: self.ordinal,
            text: self.text,
            verify_hint: self.verify_hint,
            evidence: self.evidence,
            reviewer_run_id: self.reviewer_run_id,
            spec_sha: self.spec_sha,
            checked_at: self.checked_at,
        })
    }
}

/// Replaces all of a spec's criteria in one transaction, numbering from 1 and stamping each with
/// `spec_sha`. A failure part-way leaves the previous criteria in place.
pub async fn replace_criteria(
    pool: &SqlitePool,
    spec_id: &str,
    spec_sha: &str,
    items: &[NewCriterion],
) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM spec_criteria WHERE spec_id = ?")
        .bind(spec_id)
        .execute(&mut *tx)
        .await?;
    for (index, item) in items.iter().enumerate() {
        sqlx::query(
            "INSERT INTO spec_criteria (spec_id, ordinal, text, kind, verify_hint, status, \
             spec_sha) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(spec_id)
        .bind(index as i64 + 1)
        .bind(&item.text)
        .bind(item.kind.as_str())
        .bind(&item.verify_hint)
        .bind(item.status.as_str())
        .bind(spec_sha)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn list_criteria(pool: &SqlitePool, spec_id: &str) -> Result<Vec<Criterion>, StoreError> {
    let rows: Vec<CriterionRow> = sqlx::query_as(
        "SELECT id, spec_id, ordinal, text, kind, verify_hint, status, evidence, reviewer_run_id, \
         spec_sha, checked_at FROM spec_criteria WHERE spec_id = ? ORDER BY ordinal",
    )
    .bind(spec_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(CriterionRow::into_criterion).collect()
}

/// Records that `spec_id` depends on `depends_on`. The pair is the key: a repeat, under either
/// source, changes nothing.
pub async fn add_dep(
    pool: &SqlitePool,
    spec_id: &str,
    depends_on: &str,
    source: DepSource,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO spec_deps (spec_id, depends_on, source) VALUES (?, ?, ?) \
         ON CONFLICT (spec_id, depends_on) DO NOTHING",
    )
    .bind(spec_id)
    .bind(depends_on)
    .bind(source.as_str())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_deps(pool: &SqlitePool, spec_id: &str) -> Result<Vec<Dep>, StoreError> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT spec_id, depends_on, source FROM spec_deps WHERE spec_id = ? ORDER BY depends_on",
    )
    .bind(spec_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(spec_id, depends_on, source)| {
            Ok(Dep {
                source: DepSource::parse(&source)
                    .ok_or_else(|| StoreError::Invalid(format!("dep source {source:?}")))?,
                spec_id,
                depends_on,
            })
        })
        .collect()
}

#[derive(sqlx::FromRow)]
struct QuestionRow {
    id: String,
    spec_id: String,
    question: String,
    answer: Option<String>,
    status: String,
    created_at: String,
    answered_at: Option<String>,
}

fn new_question_id() -> String {
    let bytes = rand::rng().random::<[u8; 8]>();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("sq-{hex}")
}

/// Puts a question to the owner and returns its id. Asking the same text twice makes two questions.
pub async fn add_question(
    pool: &SqlitePool,
    spec_id: &str,
    question: &str,
) -> Result<String, StoreError> {
    let id = new_question_id();
    sqlx::query(
        "INSERT INTO spec_questions (id, spec_id, question, status, created_at) \
         VALUES (?, ?, ?, 'open', ?)",
    )
    .bind(&id)
    .bind(spec_id)
    .bind(question)
    .bind(now())
    .execute(pool)
    .await?;
    Ok(id)
}

pub async fn answer_question(
    pool: &SqlitePool,
    question_id: &str,
    answer: &str,
) -> Result<(), StoreError> {
    let done = sqlx::query(
        "UPDATE spec_questions SET answer = ?, status = 'answered', answered_at = ? WHERE id = ?",
    )
    .bind(answer)
    .bind(now())
    .bind(question_id)
    .execute(pool)
    .await?;
    if done.rows_affected() == 0 {
        return Err(StoreError::NotFound(format!("question {question_id}")));
    }
    Ok(())
}

pub async fn list_questions(pool: &SqlitePool, spec_id: &str) -> Result<Vec<Question>, StoreError> {
    let rows: Vec<QuestionRow> = sqlx::query_as(
        "SELECT id, spec_id, question, answer, status, created_at, answered_at \
         FROM spec_questions WHERE spec_id = ? ORDER BY created_at, id",
    )
    .bind(spec_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Question {
            id: row.id,
            spec_id: row.spec_id,
            question: row.question,
            answer: row.answer,
            status: row.status,
            created_at: row.created_at,
            answered_at: row.answered_at,
        })
        .collect())
}

#[derive(sqlx::FromRow)]
struct DecisionRow {
    id: String,
    tree_id: Option<String>,
    question: String,
    answer: Option<String>,
    status: String,
    at: String,
}

/// The owner's single list: every spec question and, when the tree-job `decisions` table exists,
/// every tree decision, oldest first. The migration's view covers the questions; the union is here
/// because that view cannot reference a table that may not exist.
pub async fn list_owner_decisions(pool: &SqlitePool) -> Result<Vec<OwnerDecision>, StoreError> {
    let questions: Vec<QuestionRow> = sqlx::query_as(
        "SELECT id, subject AS spec_id, question, answer, status, created_at, answered_at \
         FROM owner_decisions",
    )
    .fetch_all(pool)
    .await?;
    let mut rows: Vec<OwnerDecision> = questions
        .into_iter()
        .map(|row| OwnerDecision {
            origin: "spec_question".to_owned(),
            id: row.id,
            subject: row.spec_id,
            question: row.question,
            answer: row.answer,
            status: row.status,
            created_at: row.created_at,
            answered_at: row.answered_at,
        })
        .collect();

    let has_decisions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'decisions'",
    )
    .fetch_one(pool)
    .await?;
    if has_decisions > 0 {
        let decisions: Vec<DecisionRow> = sqlx::query_as(
            "SELECT id, tree_id, question, answer, status, at FROM decisions",
        )
        .fetch_all(pool)
        .await?;
        rows.extend(decisions.into_iter().map(|row| OwnerDecision {
            origin: "tree_decision".to_owned(),
            id: row.id,
            subject: row.tree_id.unwrap_or_default(),
            question: row.question,
            answer: row.answer,
            status: row.status,
            created_at: row.at,
            answered_at: None,
        }));
    }

    // Parsed instants first, so differing fractional-second or offset spellings still order by time.
    rows.sort_by_cached_key(|row| {
        (
            DateTime::parse_from_rfc3339(&row.created_at)
                .ok()
                .map(|at| at.with_timezone(&Utc)),
            row.created_at.clone(),
            row.id.clone(),
        )
    });
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_backlog::model::{
        spec_id, CriterionKind, CriterionStatus, DepSource, NewCriterion, Spec, SpecStatus,
    };
    use sqlx::SqlitePool;

    const PATH: &str = ".ai/specs/2026-01-01-alpha.md";
    const OTHER_PATH: &str = ".ai/specs/2026-01-02-beta.md";
    /// Every upsert path must hold for both: the unique index over a nullable `project_id` treats
    /// NULLs as distinct, so it catches nothing in the `None` case.
    const PROJECTS: [Option<&str>; 2] = [Some("proj-1"), None];
    const OLD: &str = "2020-01-01T00:00:00+00:00";

    async fn exec(pool: &SqlitePool, sql: &'static str, binds: &[&str]) {
        let mut query = sqlx::query(sql);
        for value in binds {
            query = query.bind((*value).to_owned());
        }
        query.execute(pool).await.unwrap();
    }

    async fn count(pool: &SqlitePool, sql: &'static str) -> i64 {
        sqlx::query_scalar::<_, i64>(sql)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn open_items(texts: &[&str]) -> Vec<NewCriterion> {
        texts
            .iter()
            .map(|text| NewCriterion {
                text: (*text).to_owned(),
                kind: CriterionKind::Behavior,
                verify_hint: None,
                status: CriterionStatus::Open,
            })
            .collect()
    }

    async fn seed(pool: &SqlitePool, project: Option<&str>, path: &str) -> Spec {
        upsert_by_path(pool, project, path, "Title", "sha-1")
            .await
            .unwrap()
    }

    async fn criteria_texts(pool: &SqlitePool, spec: &str) -> Vec<String> {
        list_criteria(pool, spec)
            .await
            .unwrap()
            .into_iter()
            .map(|criterion| criterion.text)
            .collect()
    }

    async fn mark_first_criterion_met(pool: &SqlitePool, spec: &str) {
        exec(
            pool,
            "UPDATE spec_criteria SET status = 'met', evidence = 'seen in run r1', \
             reviewer_run_id = 'r1', checked_at = '2026-10-30T10:00:00+00:00' \
             WHERE spec_id = ? AND ordinal = 1",
            &[spec],
        )
        .await;
    }

    async fn source_of(pool: &SqlitePool, spec: &str, target: &str) -> Vec<DepSource> {
        list_deps(pool, spec)
            .await
            .unwrap()
            .into_iter()
            .filter(|dep| dep.spec_id == spec && dep.depends_on == target)
            .map(|dep| dep.source)
            .collect()
    }

    async fn recreate_decisions(pool: &SqlitePool) {
        exec(pool, "DROP TABLE IF EXISTS decisions", &[]).await;
        exec(
            pool,
            "CREATE TABLE decisions (id TEXT PRIMARY KEY, tree_id TEXT, node_id TEXT, \
             question TEXT NOT NULL, answer TEXT, origin TEXT, at TEXT NOT NULL, \
             status TEXT NOT NULL)",
            &[],
        )
        .await;
    }

    #[tokio::test]
    async fn upsert_by_path_keeps_id() {
        for project in PROJECTS {
            let pool = crate::testdb::fresh_pool().await;
            let expected_id = spec_id(project, PATH);
            assert!(get_spec(&pool, &expected_id).await.unwrap().is_none());

            let first = upsert_by_path(&pool, project, PATH, "Alpha", "sha-1")
                .await
                .unwrap();
            assert_eq!(first.id, expected_id);
            assert_eq!(first.project_id.as_deref(), project);
            assert_eq!(first.path, PATH);
            assert_eq!(first.content_sha, "sha-1");
            assert_eq!(first.status, SpecStatus::Discovered);

            // Backdated, so a delete-and-insert cannot pass by landing on the same clock reading.
            exec(
                &pool,
                "UPDATE specs SET created_at = ? WHERE id = ?",
                &[OLD, first.id.as_str()],
            )
            .await;

            for sha in ["sha-1", "sha-2"] {
                let again = upsert_by_path(&pool, project, PATH, "Alpha", sha)
                    .await
                    .unwrap();
                assert_eq!(again.id, first.id, "{project:?} {sha}");
                assert_eq!(again.created_at, OLD, "{project:?} {sha}");
                assert_eq!(again.content_sha, sha, "{project:?} {sha}");
            }

            assert_eq!(count(&pool, "SELECT COUNT(*) FROM specs").await, 1);
            let stored = get_spec(&pool, &first.id).await.unwrap().unwrap();
            assert_eq!(stored.created_at, OLD);
            assert_eq!(stored.content_sha, "sha-2");
        }
    }

    #[tokio::test]
    async fn same_path_in_two_projects_are_two_specs() {
        let pool = crate::testdb::fresh_pool().await;
        let one = seed(&pool, Some("proj-1"), PATH).await;
        let two = seed(&pool, Some("proj-2"), PATH).await;
        let none = seed(&pool, None, PATH).await;
        assert_ne!(one.id, two.id);
        assert_ne!(one.id, none.id);
        assert_ne!(two.id, none.id);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM specs").await, 3);

        // A change in one project reaches neither the other project nor the project-less row.
        upsert_by_path(&pool, Some("proj-1"), PATH, "Title", "sha-2")
            .await
            .unwrap();
        for (id, project, sha) in [
            (&one.id, Some("proj-1"), "sha-2"),
            (&two.id, Some("proj-2"), "sha-1"),
            (&none.id, None, "sha-1"),
        ] {
            let stored = get_spec(&pool, id).await.unwrap().unwrap();
            assert_eq!(stored.project_id.as_deref(), project);
            assert_eq!(stored.content_sha, sha, "{project:?}");
        }
    }

    #[tokio::test]
    async fn changed_content_sha_marks_criteria_stale() {
        for project in PROJECTS {
            let pool = crate::testdb::fresh_pool().await;
            let spec = seed(&pool, project, PATH).await;
            let other = seed(&pool, project, OTHER_PATH).await;
            replace_criteria(&pool, &spec.id, "sha-1", &open_items(&["first", "second"]))
                .await
                .unwrap();
            replace_criteria(&pool, &other.id, "sha-1", &open_items(&["untouched"]))
                .await
                .unwrap();
            mark_first_criterion_met(&pool, &spec.id).await;
            exec(&pool, "UPDATE specs SET status = 'audited'", &[]).await;

            let updated = upsert_by_path(&pool, project, PATH, "Title", "sha-2")
                .await
                .unwrap();
            assert_eq!(updated.id, spec.id);
            assert_eq!(updated.content_sha, "sha-2");
            assert_eq!(updated.status, SpecStatus::Discovered);
            let stored = get_spec(&pool, &spec.id).await.unwrap().unwrap();
            assert_eq!(stored.status, SpecStatus::Discovered);

            let criteria = list_criteria(&pool, &spec.id).await.unwrap();
            assert_eq!(criteria.len(), 2);
            for criterion in &criteria {
                assert_eq!(criterion.status, CriterionStatus::Stale, "{}", criterion.text);
                assert_eq!(criterion.spec_sha, "sha-1", "{}", criterion.text);
            }

            // The change belongs to this spec only.
            let bystander = get_spec(&pool, &other.id).await.unwrap().unwrap();
            assert_eq!(bystander.status, SpecStatus::Audited);
            let untouched = list_criteria(&pool, &other.id).await.unwrap();
            assert_eq!(untouched.len(), 1);
            assert_eq!(untouched[0].status, CriterionStatus::Open);
        }
    }

    #[tokio::test]
    async fn unchanged_content_sha_keeps_status_and_criteria() {
        for project in PROJECTS {
            let pool = crate::testdb::fresh_pool().await;
            let spec = seed(&pool, project, PATH).await;
            replace_criteria(&pool, &spec.id, "sha-1", &open_items(&["first", "second"]))
                .await
                .unwrap();
            mark_first_criterion_met(&pool, &spec.id).await;
            exec(
                &pool,
                "UPDATE specs SET status = 'audited', status_reason = 'all met' WHERE id = ?",
                &[spec.id.as_str()],
            )
            .await;

            let again = upsert_by_path(&pool, project, PATH, "Title", "sha-1")
                .await
                .unwrap();
            assert_eq!(again.status, SpecStatus::Audited);
            assert_eq!(again.status_reason.as_deref(), Some("all met"));

            let criteria = list_criteria(&pool, &spec.id).await.unwrap();
            assert_eq!(criteria.len(), 2);
            assert_eq!(criteria[0].status, CriterionStatus::Met);
            assert_eq!(criteria[0].evidence.as_deref(), Some("seen in run r1"));
            assert_eq!(criteria[0].reviewer_run_id.as_deref(), Some("r1"));
            assert_eq!(criteria[1].status, CriterionStatus::Open);
        }
    }

    #[tokio::test]
    async fn upsert_keeps_tree_and_audit_fields() {
        for project in PROJECTS {
            let pool = crate::testdb::fresh_pool().await;
            let spec = seed(&pool, project, PATH).await;
            exec(
                &pool,
                "UPDATE specs SET tree_id = 'tree-1', last_audited_sha = 'sha-0' WHERE id = ?",
                &[spec.id.as_str()],
            )
            .await;

            for sha in ["sha-1", "sha-2"] {
                let again = upsert_by_path(&pool, project, PATH, "Title", sha)
                    .await
                    .unwrap();
                assert_eq!(again.tree_id.as_deref(), Some("tree-1"), "{sha}");
                assert_eq!(again.last_audited_sha.as_deref(), Some("sha-0"), "{sha}");
            }
        }
    }

    #[tokio::test]
    async fn criteria_replace_is_atomic() {
        let pool = crate::testdb::fresh_pool().await;
        let spec = seed(&pool, None, PATH).await;
        replace_criteria(&pool, &spec.id, "sha-1", &open_items(&["keep-1", "keep-2"]))
            .await
            .unwrap();
        let before = list_criteria(&pool, &spec.id).await.unwrap();
        exec(
            &pool,
            "CREATE TRIGGER spec_criteria_boom BEFORE INSERT ON spec_criteria \
             WHEN NEW.text = 'boom' BEGIN SELECT RAISE(ABORT, 'boom'); END",
            &[],
        )
        .await;

        let failed = replace_criteria(
            &pool,
            &spec.id,
            "sha-2",
            &open_items(&["new-1", "boom", "new-3"]),
        )
        .await;
        assert!(failed.is_err());

        let after = list_criteria(&pool, &spec.id).await.unwrap();
        assert_eq!(after.len(), before.len());
        for (kept, old) in after.iter().zip(&before) {
            assert_eq!(kept.id, old.id);
            assert_eq!(kept.ordinal, old.ordinal);
            assert_eq!(kept.text, old.text);
            assert_eq!(kept.spec_sha, "sha-1");
        }
        assert_eq!(criteria_texts(&pool, &spec.id).await, ["keep-1", "keep-2"]);

        // The failure leaves no transaction open behind it: the next replace goes through.
        replace_criteria(&pool, &spec.id, "sha-2", &open_items(&["new-1"]))
            .await
            .unwrap();
        assert_eq!(criteria_texts(&pool, &spec.id).await, ["new-1"]);
    }

    #[tokio::test]
    async fn replace_criteria_numbers_from_one_and_clears_stale() {
        let pool = crate::testdb::fresh_pool().await;
        let spec = seed(&pool, None, PATH).await;
        let other = seed(&pool, None, OTHER_PATH).await;
        replace_criteria(&pool, &other.id, "sha-1", &open_items(&["bystander"]))
            .await
            .unwrap();
        replace_criteria(&pool, &spec.id, "sha-1", &open_items(&["a", "b", "c"]))
            .await
            .unwrap();
        let ordinals: Vec<i64> = list_criteria(&pool, &spec.id)
            .await
            .unwrap()
            .iter()
            .map(|criterion| criterion.ordinal)
            .collect();
        assert_eq!(ordinals, [1, 2, 3]);

        // The list follows the ordinal, not the insertion order.
        exec(
            &pool,
            "UPDATE spec_criteria SET ordinal = 10 - ordinal WHERE spec_id = ?",
            &[spec.id.as_str()],
        )
        .await;
        assert_eq!(criteria_texts(&pool, &spec.id).await, ["c", "b", "a"]);

        upsert_by_path(&pool, None, PATH, "Title", "sha-2")
            .await
            .unwrap();
        let extracted = [
            NewCriterion {
                text: "x".to_owned(),
                kind: CriterionKind::Test,
                verify_hint: Some("cargo test x".to_owned()),
                status: CriterionStatus::Open,
            },
            NewCriterion {
                text: "y".to_owned(),
                kind: CriterionKind::Decision,
                verify_hint: None,
                status: CriterionStatus::Doubt,
            },
        ];
        replace_criteria(&pool, &spec.id, "sha-2", &extracted)
            .await
            .unwrap();

        let criteria = list_criteria(&pool, &spec.id).await.unwrap();
        assert_eq!(criteria.len(), 2);
        assert_eq!(criteria[0].ordinal, 1);
        assert_eq!(criteria[0].spec_id, spec.id);
        assert_eq!(criteria[0].text, "x");
        assert_eq!(criteria[0].kind, CriterionKind::Test);
        assert_eq!(criteria[0].verify_hint.as_deref(), Some("cargo test x"));
        assert_eq!(criteria[0].status, CriterionStatus::Open);
        assert_eq!(criteria[1].ordinal, 2);
        assert_eq!(criteria[1].text, "y");
        assert_eq!(criteria[1].kind, CriterionKind::Decision);
        assert_eq!(criteria[1].verify_hint, None);
        assert_eq!(criteria[1].status, CriterionStatus::Doubt);
        for criterion in &criteria {
            assert_eq!(criterion.spec_sha, "sha-2");
            assert_eq!(criterion.evidence, None);
            assert_eq!(criterion.reviewer_run_id, None);
            assert_eq!(criterion.checked_at, None);
        }
        assert_eq!(
            count(
                &pool,
                "SELECT COUNT(*) FROM spec_criteria WHERE status = 'stale'"
            )
            .await,
            0
        );

        // Only this spec's rows were replaced.
        assert_eq!(criteria_texts(&pool, &other.id).await, ["bystander"]);
    }

    #[tokio::test]
    async fn deps_roundtrip() {
        let pool = crate::testdb::fresh_pool().await;
        let a = seed(&pool, None, ".ai/specs/a.md").await.id;
        let b = seed(&pool, None, ".ai/specs/b.md").await.id;
        let c = seed(&pool, None, ".ai/specs/c.md").await.id;
        add_dep(&pool, &a, &b, DepSource::Extracted).await.unwrap();
        add_dep(&pool, &a, &c, DepSource::Owner).await.unwrap();

        assert_eq!(list_deps(&pool, &a).await.unwrap().len(), 2);
        assert_eq!(source_of(&pool, &a, &b).await, [DepSource::Extracted]);
        assert_eq!(source_of(&pool, &a, &c).await, [DepSource::Owner]);

        // The edge has a direction.
        assert!(list_deps(&pool, &b).await.unwrap().is_empty());

        // The same pair again changes nothing.
        add_dep(&pool, &a, &b, DepSource::Extracted).await.unwrap();
        assert_eq!(list_deps(&pool, &a).await.unwrap().len(), 2);
        assert_eq!(source_of(&pool, &a, &b).await, [DepSource::Extracted]);

        // The same pair under the other source still neither errors nor duplicates. Which source
        // wins is not pinned, so it is not asserted.
        add_dep(&pool, &a, &b, DepSource::Owner).await.unwrap();
        assert_eq!(list_deps(&pool, &a).await.unwrap().len(), 2);
        assert_eq!(source_of(&pool, &a, &b).await.len(), 1);
    }

    #[tokio::test]
    async fn questions_roundtrip() {
        let pool = crate::testdb::fresh_pool().await;
        let spec = seed(&pool, None, PATH).await;
        let other = seed(&pool, None, OTHER_PATH).await;

        let asked = add_question(&pool, &spec.id, "Which port?").await.unwrap();
        let pending = add_question(&pool, &spec.id, "Which port?").await.unwrap();
        let elsewhere = add_question(&pool, &other.id, "Which host?").await.unwrap();
        assert_ne!(asked, pending);

        let questions = list_questions(&pool, &spec.id).await.unwrap();
        assert_eq!(questions.len(), 2);
        let open = questions.iter().find(|q| q.id == asked).unwrap();
        assert_eq!(open.spec_id, spec.id);
        assert_eq!(open.question, "Which port?");
        assert_eq!(open.answer, None);
        assert_eq!(open.status, "open");
        assert_eq!(open.answered_at, None);
        assert!(chrono::DateTime::parse_from_rfc3339(&open.created_at).is_ok());
        let created_at = open.created_at.clone();

        answer_question(&pool, &asked, "8080").await.unwrap();

        let questions = list_questions(&pool, &spec.id).await.unwrap();
        let answered = questions.iter().find(|q| q.id == asked).unwrap();
        assert_eq!(answered.answer.as_deref(), Some("8080"));
        assert_eq!(answered.status, "answered");
        assert_eq!(answered.created_at, created_at);
        let answered_at = answered.answered_at.as_deref().unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(answered_at).is_ok());

        // Answering one question leaves its neighbours alone.
        let neighbour = questions.iter().find(|q| q.id == pending).unwrap();
        assert_eq!(neighbour.status, "open");
        assert_eq!(neighbour.answer, None);
        let others = list_questions(&pool, &other.id).await.unwrap();
        assert_eq!(others.len(), 1);
        assert_eq!(others[0].id, elsewhere);
        assert_eq!(others[0].status, "open");
    }

    #[tokio::test]
    async fn list_owner_decisions_merges_spec_questions_and_decisions_when_table_exists() {
        let pool = crate::testdb::fresh_pool().await;
        recreate_decisions(&pool).await;
        let spec = seed(&pool, None, PATH).await;

        let q1 = add_question(&pool, &spec.id, "First?").await.unwrap();
        let q2 = add_question(&pool, &spec.id, "Third?").await.unwrap();
        answer_question(&pool, &q2, "yes").await.unwrap();
        let (t10, t11, t12, t13) = (
            "2026-10-30T10:00:00+00:00",
            "2026-10-30T11:00:00+00:00",
            "2026-10-30T12:00:00+00:00",
            "2026-10-30T13:00:00+00:00",
        );
        let set_created = "UPDATE spec_questions SET created_at = ? WHERE id = ?";
        exec(&pool, set_created, &[t10, q1.as_str()]).await;
        exec(&pool, set_created, &[t12, q2.as_str()]).await;
        // `origin` here is the table's own column, a different thing from the merged list's origin.
        exec(
            &pool,
            "INSERT INTO decisions (id, tree_id, node_id, question, answer, origin, at, status) \
             VALUES ('d-1', 'tree-1', 'node-1', 'Second?', NULL, 'agent', ?, 'open')",
            &[t11],
        )
        .await;
        exec(
            &pool,
            "INSERT INTO decisions (id, tree_id, node_id, question, answer, origin, at, status) \
             VALUES ('d-2', NULL, NULL, 'Fourth?', 'no', 'agent', ?, 'answered')",
            &[t13],
        )
        .await;

        let rows = list_owner_decisions(&pool).await.unwrap();

        // Interleaved on purpose: neither table first would give this order.
        let order: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|row| (row.origin.as_str(), row.id.as_str(), row.created_at.as_str()))
            .collect();
        assert_eq!(
            order,
            [
                ("spec_question", q1.as_str(), t10),
                ("tree_decision", "d-1", t11),
                ("spec_question", q2.as_str(), t12),
                ("tree_decision", "d-2", t13),
            ]
        );

        assert_eq!(rows[0].subject, spec.id);
        assert_eq!(rows[0].question, "First?");
        assert_eq!(rows[0].answer, None);
        assert_eq!(rows[0].status, "open");
        assert_eq!(rows[0].answered_at, None);

        assert_eq!(rows[1].subject, "tree-1");
        assert_eq!(rows[1].question, "Second?");
        assert_eq!(rows[1].answer, None);
        assert_eq!(rows[1].status, "open");
        assert_eq!(rows[1].answered_at, None);

        assert_eq!(rows[2].subject, spec.id);
        assert_eq!(rows[2].question, "Third?");
        assert_eq!(rows[2].answer.as_deref(), Some("yes"));
        assert_eq!(rows[2].status, "answered");
        assert!(rows[2].answered_at.is_some());

        // A decision with no tree is not lost; its subject may be empty, so that is not asserted.
        assert_eq!(rows[3].question, "Fourth?");
        assert_eq!(rows[3].answer.as_deref(), Some("no"));
        assert_eq!(rows[3].status, "answered");
        assert_eq!(rows[3].answered_at, None);
    }

    #[tokio::test]
    async fn list_owner_decisions_without_decisions_table() {
        let pool = crate::testdb::fresh_pool().await;
        exec(&pool, "DROP TABLE IF EXISTS decisions", &[]).await;
        assert!(list_owner_decisions(&pool).await.unwrap().is_empty());

        let spec = seed(&pool, None, PATH).await;
        let later = add_question(&pool, &spec.id, "Later?").await.unwrap();
        let earlier = add_question(&pool, &spec.id, "Earlier?").await.unwrap();
        exec(
            &pool,
            "UPDATE spec_questions SET created_at = ? WHERE id = ?",
            &[OLD, earlier.as_str()],
        )
        .await;

        let rows = list_owner_decisions(&pool).await.unwrap();
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, [earlier.as_str(), later.as_str()]);
        for row in &rows {
            assert_eq!(row.origin, "spec_question");
            assert_eq!(row.subject, spec.id);
            assert_eq!(row.status, "open");
        }
    }
}
