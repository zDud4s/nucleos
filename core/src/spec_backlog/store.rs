//! Spec backlog storage: reads and writes over `specs`, `spec_criteria`, `spec_deps` and
//! `spec_questions`, and the owner's single list of decisions.
//!
//! A function that holds a transaction runs every query on it, never on the pool: the test pool has a
//! single connection.

use std::collections::HashSet;
use std::fmt;

use chrono::{DateTime, Utc};
use rand::RngExt;
use sqlx::SqlitePool;

use super::model::{
    Criterion, CriterionKind, CriterionStatus, Dep, DepSource, NewCriterion, OwnerDecision,
    Question, Spec, SpecKind, SpecStatus, spec_id,
};

#[derive(Debug)]
pub enum StoreError {
    NotFound(String),
    AlreadyDecided(String),
    /// The write was refused because the spec's content moved since the caller read it.
    Stale(String),
    /// A stored value outside its closed vocabulary.
    Invalid(String),
    Db(sqlx::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NotFound(id) => write!(f, "{id} not found"),
            StoreError::AlreadyDecided(id) => write!(f, "{id} was already decided"),
            StoreError::Stale(what) => write!(f, "{what} changed since it was read"),
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
    tree_launched: bool,
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
            tree_launched: self.tree_launched,
            last_audited_sha: self.last_audited_sha,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

const SELECT_SPEC: &str = "SELECT id, project_id, path, title, content_sha, kind, status, \
     status_reason, tree_id, tree_launched, last_audited_sha, created_at, updated_at \
     FROM specs WHERE id = ?";

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
    // IMMEDIATE: this reads before it writes, and a deferred transaction that finds another
    // connection committed in between fails SQLITE_BUSY at once instead of waiting.
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;

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

/// Lists paths in one registry namespace for extraction dependency validation.
pub async fn list_paths_by_project(
    pool: &SqlitePool,
    project_id: Option<&str>,
) -> Result<Vec<String>, StoreError> {
    Ok(
        sqlx::query_scalar("SELECT path FROM specs WHERE project_id IS ? ORDER BY path")
            .bind(project_id)
            .fetch_all(pool)
            .await?,
    )
}

pub async fn set_extraction_result(
    pool: &SqlitePool,
    spec_id: &str,
    kind: SpecKind,
    status: SpecStatus,
    reason: Option<&str>,
) -> Result<(), StoreError> {
    let result = sqlx::query(
        "UPDATE specs SET kind = ?, status = ?, status_reason = ?, updated_at = ? WHERE id = ?",
    )
    .bind(kind.as_str())
    .bind(status.as_str())
    .bind(reason)
    .bind(now())
    .bind(spec_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(StoreError::NotFound(format!("spec {spec_id}")));
    }
    Ok(())
}

/// Records one criterion's review. The write only lands on a criterion extracted from the spec's
/// current content: a spec edited while its reviewer ran is `Stale`, and nothing is overwritten.
pub async fn set_criterion_review(
    pool: &SqlitePool,
    spec_id: &str,
    ordinal: i64,
    status: CriterionStatus,
    evidence: Option<&str>,
    reviewer_run_id: &str,
) -> Result<(), StoreError> {
    let result = sqlx::query(
        "UPDATE spec_criteria SET status = ?, evidence = ?, reviewer_run_id = ?, checked_at = ? \
         WHERE spec_id = ? AND ordinal = ? \
         AND spec_sha = (SELECT content_sha FROM specs WHERE id = ?)",
    )
    .bind(status.as_str())
    .bind(evidence)
    .bind(reviewer_run_id)
    .bind(now())
    .bind(spec_id)
    .bind(ordinal)
    .bind(spec_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(StoreError::Stale(format!("criteria of spec {spec_id}")));
    }
    Ok(())
}

/// Records an audit's outcome, only when the spec still has the content `sha` the audit read.
pub async fn set_audit_result(
    pool: &SqlitePool,
    spec_id: &str,
    status: SpecStatus,
    reason: Option<&str>,
    sha: &str,
) -> Result<(), StoreError> {
    let result = sqlx::query("UPDATE specs SET status = ?, status_reason = ?, last_audited_sha = ?, updated_at = ? WHERE id = ? AND content_sha = ?")
        .bind(status.as_str()).bind(reason).bind(sha).bind(now()).bind(spec_id).bind(sha).execute(pool).await?;
    if result.rows_affected() == 0 {
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM specs WHERE id = ?")
            .bind(spec_id)
            .fetch_optional(pool)
            .await?;
        return Err(match exists {
            Some(_) => StoreError::Stale(format!("spec {spec_id}")),
            None => StoreError::NotFound(format!("spec {spec_id}")),
        });
    }
    Ok(())
}

/// Moves a spec to `status` with the reason, touching nothing else (not `last_audited_sha`).
pub async fn set_status(
    pool: &SqlitePool,
    spec_id: &str,
    status: SpecStatus,
    reason: Option<&str>,
) -> Result<(), StoreError> {
    let result =
        sqlx::query("UPDATE specs SET status = ?, status_reason = ?, updated_at = ? WHERE id = ?")
            .bind(status.as_str())
            .bind(reason)
            .bind(now())
            .bind(spec_id)
            .execute(pool)
            .await?;
    if result.rows_affected() == 0 {
        return Err(StoreError::NotFound(format!("spec {spec_id}")));
    }
    Ok(())
}

/// Records the tree that is working on the spec (the latest one, when a second round replaces it).
/// The new id is not launched yet: `tree_launched` goes back to 0 in the same statement.
pub async fn set_tree_id(
    pool: &SqlitePool,
    spec_id: &str,
    tree_id: &str,
) -> Result<(), StoreError> {
    let result =
        sqlx::query("UPDATE specs SET tree_id = ?, tree_launched = 0, updated_at = ? WHERE id = ?")
            .bind(tree_id)
            .bind(now())
            .bind(spec_id)
            .execute(pool)
            .await?;
    if result.rows_affected() == 0 {
        return Err(StoreError::NotFound(format!("spec {spec_id}")));
    }
    Ok(())
}

/// Records that the launcher accepted the tree named by the spec's `tree_id`.
pub async fn set_tree_launched(pool: &SqlitePool, spec_id: &str) -> Result<(), StoreError> {
    let result = sqlx::query("UPDATE specs SET tree_launched = 1, updated_at = ? WHERE id = ?")
        .bind(now())
        .bind(spec_id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(StoreError::NotFound(format!("spec {spec_id}")));
    }
    Ok(())
}

/// Every spec of one registry namespace (`None` is the namespace without a project), by path.
pub async fn list_specs_by_project(
    pool: &SqlitePool,
    project_id: Option<&str>,
) -> Result<Vec<Spec>, StoreError> {
    let rows: Vec<SpecRow> = sqlx::query_as(
        "SELECT id, project_id, path, title, content_sha, kind, status, status_reason, tree_id, \
         tree_launched, last_audited_sha, created_at, updated_at FROM specs \
         WHERE project_id IS ? ORDER BY path",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(SpecRow::into_spec).collect()
}

/// Lists every spec, regardless of project ownership.
pub async fn list_all_specs(pool: &SqlitePool) -> Result<Vec<Spec>, StoreError> {
    let rows: Vec<SpecRow> = sqlx::query_as(
        "SELECT id, project_id, path, title, content_sha, kind, status, status_reason, tree_id, \
         tree_launched, last_audited_sha, created_at, updated_at FROM specs ORDER BY path",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(SpecRow::into_spec).collect()
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

/// Replaces the dependencies a spec's extraction named with `depends_on`; the owner's own
/// dependencies stay. One transaction, so a failure leaves the previous set.
pub async fn replace_extracted_deps(
    pool: &SqlitePool,
    spec_id: &str,
    depends_on: &[String],
) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM spec_deps WHERE spec_id = ? AND source = ?")
        .bind(spec_id)
        .bind(DepSource::Extracted.as_str())
        .execute(&mut *tx)
        .await?;
    for target in depends_on {
        sqlx::query(
            "INSERT INTO spec_deps (spec_id, depends_on, source) VALUES (?, ?, ?) \
             ON CONFLICT (spec_id, depends_on) DO NOTHING",
        )
        .bind(spec_id)
        .bind(target)
        .bind(DepSource::Extracted.as_str())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
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

/// Makes `questions` the open questions of a spec among those whose text starts with `prefix`, each
/// open once. A text already open keeps its row (the oldest, if an earlier audit left copies), a
/// new one is asked, and any other open row with the prefix becomes `closed`, which takes it off
/// the owner's list. Answered rows and rows without the prefix are not touched.
pub async fn settle_questions(
    pool: &SqlitePool,
    spec_id: &str,
    prefix: &str,
    questions: &[String],
) -> Result<(), StoreError> {
    // IMMEDIATE: this reads before it writes, and a deferred transaction that finds another
    // connection committed in between fails SQLITE_BUSY at once instead of waiting.
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let existing: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id, question, status FROM spec_questions WHERE spec_id = ? \
         AND status IN ('open', 'answered') \
         ORDER BY created_at, id",
    )
    .bind(spec_id)
    .fetch_all(&mut *tx)
    .await?;

    let mut asked: HashSet<&str> = HashSet::new();
    for (id, question, status) in &existing {
        if !question.starts_with(prefix) {
            continue;
        }
        if questions.contains(question) && asked.insert(question.as_str()) {
            continue;
        }
        if status == "open" {
            sqlx::query("UPDATE spec_questions SET status = 'closed' WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
    }
    for question in questions {
        if !asked.insert(question.as_str()) {
            continue;
        }
        sqlx::query(
            "INSERT INTO spec_questions (id, spec_id, question, status, created_at) \
             VALUES (?, ?, ?, 'open', ?)",
        )
        .bind(new_question_id())
        .bind(spec_id)
        .bind(question)
        .bind(now())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn answer_question(
    pool: &SqlitePool,
    question_id: &str,
    answer: &str,
) -> Result<(Question, bool), StoreError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let question: Option<(String, String)> =
        sqlx::query_as("SELECT spec_id, question FROM spec_questions WHERE id = ?")
            .bind(question_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((spec_id, question)) = question else {
        return Err(StoreError::NotFound(format!("question {question_id}")));
    };
    let done = sqlx::query(
        "UPDATE spec_questions SET answer = ?, status = 'answered', answered_at = ? \
         WHERE id = ? AND status = 'open'",
    )
    .bind(answer)
    .bind(now())
    .bind(question_id)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Err(StoreError::AlreadyDecided(format!(
            "question {question_id}"
        )));
    }
    // The spec waits until its last open question is answered: one answer among several settles
    // nothing yet.
    let others_open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM spec_questions WHERE spec_id = ? AND status = 'open'",
    )
    .bind(&spec_id)
    .fetch_one(&mut *tx)
    .await?;
    let reopened = others_open == 0
        && sqlx::query(
            "UPDATE specs SET status = 'discovered', status_reason = NULL, updated_at = ? \
             WHERE id = ? AND status = 'needs_owner'",
        )
        .bind(now())
        .bind(&spec_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
    if reopened {
        // Same transaction as the flip: a crash cannot leave a reopened spec nobody will finish.
        let project_id: Option<String> =
            sqlx::query_scalar("SELECT project_id FROM specs WHERE id = ?")
                .bind(&spec_id)
                .fetch_one(&mut *tx)
                .await?;
        if let Some(project_id) = project_id {
            upsert_finish_intent(&mut tx, &project_id, &now()).await?;
        }
    }
    tx.commit().await?;
    Ok((
        Question {
            id: question_id.to_owned(),
            spec_id,
            question,
            answer: Some(answer.to_owned()),
            status: "answered".to_owned(),
            created_at: String::new(),
            answered_at: None,
        },
        reopened,
    ))
}

async fn upsert_finish_intent(
    conn: &mut sqlx::SqliteConnection,
    project_id: &str,
    created_at: &str,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO spec_finish_intents (project_id, created_at) VALUES (?, ?) \
         ON CONFLICT (project_id) DO UPDATE SET created_at = excluded.created_at",
    )
    .bind(project_id)
    .bind(created_at)
    .execute(conn)
    .await?;
    Ok(())
}

/// Records that `project_id` has finishing to do. Re-writing bumps `created_at`, which is how a
/// request that arrives during a pass is told from the one the pass served.
pub async fn write_finish_intent(pool: &SqlitePool, project_id: &str) -> Result<(), StoreError> {
    write_finish_intent_at(pool, project_id, &now()).await
}

/// `write_finish_intent` with an explicit RFC3339 `created_at`.
pub async fn write_finish_intent_at(
    pool: &SqlitePool,
    project_id: &str,
    created_at: &str,
) -> Result<(), StoreError> {
    let mut conn = pool.acquire().await?;
    upsert_finish_intent(&mut conn, project_id, created_at).await
}

/// Records an intent for `project_id` unless one stands: an existing intent keeps its `created_at`.
pub async fn ensure_finish_intent(
    pool: &SqlitePool,
    project_id: &str,
    created_at: &str,
) -> Result<(), StoreError> {
    sqlx::query("INSERT OR IGNORE INTO spec_finish_intents (project_id, created_at) VALUES (?, ?)")
        .bind(project_id)
        .bind(created_at)
        .execute(pool)
        .await?;
    Ok(())
}

/// Every pending finish intent as `(project_id, created_at)`, ordered by project.
pub async fn finish_intents(pool: &SqlitePool) -> Result<Vec<(String, String)>, StoreError> {
    Ok(
        sqlx::query_as(
            "SELECT project_id, created_at FROM spec_finish_intents ORDER BY project_id",
        )
        .fetch_all(pool)
        .await?,
    )
}

/// Deletes the intent of `project_id`, but only while its `created_at` is still `created_at`: one
/// re-written since is a newer request and stays.
pub async fn delete_finish_intent(
    pool: &SqlitePool,
    project_id: &str,
    created_at: &str,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM spec_finish_intents WHERE project_id = ? AND created_at = ?")
        .bind(project_id)
        .bind(created_at)
        .execute(pool)
        .await?;
    Ok(())
}

/// Lists this audit's answered owner questions, oldest first.
pub async fn answered_audit_questions(
    pool: &SqlitePool,
    spec_id: &str,
    prefix: &str,
) -> Result<Vec<Question>, StoreError> {
    let rows: Vec<QuestionRow> = sqlx::query_as(
        "SELECT id, spec_id, question, answer, status, created_at, answered_at \
         FROM spec_questions WHERE spec_id = ? AND status = 'answered' AND question LIKE ? \
         ORDER BY created_at, id",
    )
    .bind(spec_id)
    .bind(format!("{prefix}%"))
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

/// The owner's single list: every spec question not closed and, when the tree-job `decisions` table
/// exists, every tree decision, oldest first. The migration's view covers the questions, closed
/// ones included, so they are filtered here; the union is here because that view cannot reference
/// a table that may not exist.
pub async fn list_owner_decisions(pool: &SqlitePool) -> Result<Vec<OwnerDecision>, StoreError> {
    let questions: Vec<QuestionRow> = sqlx::query_as(
        "SELECT id, subject AS spec_id, question, answer, status, created_at, answered_at \
         FROM owner_decisions WHERE status <> 'closed'",
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
        let decisions: Vec<DecisionRow> =
            sqlx::query_as("SELECT id, tree_id, question, answer, status, at FROM decisions")
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
        CriterionKind, CriterionStatus, DepSource, NewCriterion, Spec, SpecStatus, spec_id,
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
                assert_eq!(
                    criterion.status,
                    CriterionStatus::Stale,
                    "{}",
                    criterion.text
                );
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

    const ROUNDS: usize = 4;
    const TASKS: usize = 16;

    /// Runs `TASKS` copies of `job` at once, `ROUNDS` times over, and requires every one to be Ok.
    async fn all_ok_concurrently<F, Fut>(job: F)
    where
        F: Fn(usize) -> Fut,
        Fut: std::future::Future<Output = Result<(), StoreError>> + Send + 'static,
    {
        for round in 0..ROUNDS {
            let handles: Vec<_> = (0..TASKS)
                .map(|task| tokio::spawn(job(round * TASKS + task)))
                .collect();
            for handle in handles {
                handle.await.unwrap().unwrap();
            }
        }
    }

    // A deferred transaction that reads and then writes answers SQLITE_BUSY at once, with no
    // busy_timeout wait, once another connection has committed in between.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_upserts_of_one_spec_do_not_lock() {
        let pool = crate::testdb::fresh_shared_pool().await;
        seed(&pool, None, PATH).await;
        all_ok_concurrently(|n| {
            let pool = pool.clone();
            async move {
                upsert_by_path(&pool, None, PATH, "Title", &format!("sha-{n}"))
                    .await
                    .map(|_| ())
            }
        })
        .await;
        pool.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_criteria_replaces_do_not_lock() {
        let pool = crate::testdb::fresh_shared_pool().await;
        let id = seed(&pool, None, PATH).await.id;
        all_ok_concurrently(|n| {
            let pool = pool.clone();
            let id = id.clone();
            async move {
                replace_criteria(
                    &pool,
                    &id,
                    &format!("sha-{n}"),
                    &open_items(&["one", "two"]),
                )
                .await
            }
        })
        .await;
        pool.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_extracted_deps_replaces_do_not_lock() {
        let pool = crate::testdb::fresh_shared_pool().await;
        let id = seed(&pool, None, PATH).await.id;
        all_ok_concurrently(|_| {
            let pool = pool.clone();
            let id = id.clone();
            async move { replace_extracted_deps(&pool, &id, &["other".to_owned()]).await }
        })
        .await;
        pool.close().await;
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
    async fn answer_question_refuses_closed_or_answered_and_keeps_original() {
        let pool = crate::testdb::fresh_pool().await;
        let spec = seed(&pool, None, PATH).await;
        let answered = add_question(&pool, &spec.id, "Which port?").await.unwrap();
        let closed = add_question(&pool, &spec.id, "Which host?").await.unwrap();
        exec(
            &pool,
            "UPDATE spec_questions SET status = 'closed' WHERE id = ?",
            &[closed.as_str()],
        )
        .await;

        answer_question(&pool, &answered, "8080").await.unwrap();
        let again = answer_question(&pool, &answered, "9090").await;
        assert!(
            matches!(again, Err(StoreError::AlreadyDecided(..))),
            "{again:?}"
        );
        let late = answer_question(&pool, &closed, "localhost").await;
        assert!(
            matches!(late, Err(StoreError::AlreadyDecided(..))),
            "{late:?}"
        );
        // An id that was never asked stays distinct from one already decided.
        let unknown = answer_question(&pool, "sq-nope", "x").await;
        assert!(
            matches!(unknown, Err(StoreError::NotFound(..))),
            "{unknown:?}"
        );

        let questions = list_questions(&pool, &spec.id).await.unwrap();
        let kept = questions.iter().find(|q| q.id == answered).unwrap();
        assert_eq!(kept.status, "answered");
        assert_eq!(kept.answer.as_deref(), Some("8080"));
        let untouched = questions.iter().find(|q| q.id == closed).unwrap();
        assert_eq!(untouched.status, "closed");
        assert_eq!(untouched.answer, None);
        assert_eq!(untouched.answered_at, None);
    }

    #[tokio::test]
    async fn settle_questions_preserves_answered_matching_doubt() {
        const PREFIX: &str = "Owner decision needed for criterion: ";
        let pool = crate::testdb::fresh_pool().await;
        let spec = seed(&pool, None, PATH).await;
        let kept_text = format!("{PREFIX}the widget renders");
        let surplus_text = format!("{PREFIX}the widget scrolls");
        let kept = add_question(&pool, &spec.id, &kept_text).await.unwrap();
        let surplus = add_question(&pool, &spec.id, &surplus_text).await.unwrap();
        answer_question(&pool, &kept, "Yes, it renders")
            .await
            .unwrap();

        settle_questions(&pool, &spec.id, PREFIX, std::slice::from_ref(&kept_text))
            .await
            .unwrap();

        // The answered doubt is neither asked again nor rewritten; the surplus open one is closed.
        let questions = list_questions(&pool, &spec.id).await.unwrap();
        assert_eq!(questions.len(), 2, "{questions:?}");
        let answered = questions.iter().find(|q| q.id == kept).unwrap();
        assert_eq!(answered.status, "answered");
        assert_eq!(answered.answer.as_deref(), Some("Yes, it renders"));
        let closed = questions.iter().find(|q| q.id == surplus).unwrap();
        assert_eq!(closed.status, "closed");
        assert!(
            questions.iter().all(|q| q.status != "open"),
            "{questions:?}"
        );
    }

    #[tokio::test]
    async fn answer_flips_a_waiting_spec_to_discovered() {
        let pool = crate::testdb::fresh_pool().await;
        let waiting = seed(&pool, None, PATH).await;
        let working = seed(&pool, None, OTHER_PATH).await;
        let asked = add_question(&pool, &waiting.id, "Which port?")
            .await
            .unwrap();
        let other = add_question(&pool, &working.id, "Which host?")
            .await
            .unwrap();
        set_status(&pool, &waiting.id, SpecStatus::NeedsOwner, Some("asked"))
            .await
            .unwrap();
        set_status(&pool, &working.id, SpecStatus::Queued, None)
            .await
            .unwrap();

        answer_question(&pool, &asked, "8080").await.unwrap();
        answer_question(&pool, &other, "localhost").await.unwrap();

        let after = get_spec(&pool, &waiting.id).await.unwrap().unwrap();
        assert_eq!(after.status, SpecStatus::Discovered);
        // A spec that was not waiting on the owner keeps its status.
        let untouched = get_spec(&pool, &working.id).await.unwrap().unwrap();
        assert_eq!(untouched.status, SpecStatus::Queued);
    }

    #[tokio::test]
    async fn answer_keeps_the_spec_waiting_while_another_question_is_open() {
        let pool = crate::testdb::fresh_pool().await;
        let spec = seed(&pool, None, PATH).await;
        let answered = add_question(&pool, &spec.id, "Which port?").await.unwrap();
        let still_open = add_question(&pool, &spec.id, "Which host?").await.unwrap();
        set_status(&pool, &spec.id, SpecStatus::NeedsOwner, Some("asked"))
            .await
            .unwrap();

        let (_, reopened) = answer_question(&pool, &answered, "8080").await.unwrap();

        assert!(!reopened);
        let after = get_spec(&pool, &spec.id).await.unwrap().unwrap();
        assert_eq!(after.status, SpecStatus::NeedsOwner);
        let questions = list_questions(&pool, &spec.id).await.unwrap();
        assert_eq!(
            questions
                .iter()
                .find(|question| question.id == still_open)
                .unwrap()
                .status,
            "open"
        );
    }

    /// Regression guard: resolving the final open question makes the spec runnable again.
    #[tokio::test]
    async fn answering_the_last_open_question_reopens_the_spec() {
        let pool = crate::testdb::fresh_pool().await;
        let spec = seed(&pool, None, PATH).await;
        let question = add_question(&pool, &spec.id, "Which port?").await.unwrap();
        set_status(&pool, &spec.id, SpecStatus::NeedsOwner, Some("asked"))
            .await
            .unwrap();

        let (_, reopened) = answer_question(&pool, &question, "8080").await.unwrap();

        assert!(reopened);
        let after = get_spec(&pool, &spec.id).await.unwrap().unwrap();
        assert_eq!(after.status, SpecStatus::Discovered);
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
            .map(|row| {
                (
                    row.origin.as_str(),
                    row.id.as_str(),
                    row.created_at.as_str(),
                )
            })
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
