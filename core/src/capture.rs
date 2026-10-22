//! Capture requests: when a job went wrong, the distiller asks the owner, once, what only they
//! know about it. The questions go out in a daily digest (`digest`) and stay answerable for a few
//! days; they never hold the distiller's queue.
//! Design: .ai/specs/2026-10-07-pedidos-captura-design.md.
//!
//! The question is a fixed template (no model, spec P5) and is redacted before it is written. An
//! answer is known here only as a note id: composing the note is http.rs's job (spec 5.3).

use crate::distill::Cause;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

pub mod digest;

pub const STATE_OPEN: &str = "open";
pub const STATE_ANSWERED: &str = "answered";
pub const STATE_DISMISSED: &str = "dismissed";
pub const STATE_EXPIRED: &str = "expired";

/// The feed kind a new request is announced with; the Telegram sidecar sends it without a prefix.
pub const FEED_KIND: &str = "capture_requested";

/// The causes that ask the owner (spec P3): only when something went wrong.
pub const ASKING_CAUSES: [Cause; 3] =
    [Cause::JobFailed, Cause::RunExhausted, Cause::ReviewBlocking];

/// One queue row's contribution to a request: which row, which cause, and its line of fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub row: i64,
    pub cause: String,
    pub fact: String,
}

/// The one timestamp format of this table: fixed width, so `deadline > now` as text is time order.
pub fn stamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// The retired wait setting (minutes). Only read as a fallback by [`answer_days`]: a stored `0`
/// keeps requests off.
const OLD_WAIT_KEY: &str = "distiller.capture_wait_minutes";

pub const ANSWER_DAYS_KEY: &str = "capture.answer_days";
pub const DIGEST_HOUR_KEY: &str = "capture.digest_hour";
pub const DEFAULT_ANSWER_DAYS: i64 = 5;
pub const MAX_ANSWER_DAYS: i64 = 30;
pub const DEFAULT_DIGEST_HOUR: i64 = 18;

async fn meta(pool: &SqlitePool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM schema_meta WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

/// Days a request stays answerable. `0` turns requests off. Without the new key the old
/// `distiller.capture_wait_minutes` is read as a fallback: `0` stays off, any other value is the default.
pub async fn answer_days(pool: &SqlitePool) -> i64 {
    if let Some(days) = meta(pool, ANSWER_DAYS_KEY).await.and_then(|v| v.trim().parse::<i64>().ok()) {
        return if (0..=MAX_ANSWER_DAYS).contains(&days) { days } else { DEFAULT_ANSWER_DAYS };
    }
    match meta(pool, OLD_WAIT_KEY).await.and_then(|v| v.trim().parse::<i64>().ok()) {
        Some(0) => 0,
        _ => DEFAULT_ANSWER_DAYS,
    }
}

pub async fn digest_hour(pool: &SqlitePool) -> u32 {
    meta(pool, DIGEST_HOUR_KEY)
        .await
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|h| (0..=23).contains(h))
        .unwrap_or(DEFAULT_DIGEST_HOUR) as u32
}

#[derive(Debug)]
pub enum ConfigError {
    OutOfRange,
    Db(sqlx::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::OutOfRange => write!(f, "answer_days is 0 to 30 and digest_hour 0 to 23"),
            ConfigError::Db(error) => write!(f, "{error}"),
        }
    }
}

impl From<sqlx::Error> for ConfigError {
    fn from(error: sqlx::Error) -> Self {
        ConfigError::Db(error)
    }
}

/// Store the answer window and/or the digest hour; both are validated before either is written.
pub async fn set_config(
    pool: &SqlitePool,
    answer_days: Option<i64>,
    digest_hour: Option<i64>,
    now: DateTime<Utc>,
) -> Result<(), ConfigError> {
    if answer_days.is_some_and(|d| !(0..=MAX_ANSWER_DAYS).contains(&d))
        || digest_hour.is_some_and(|h| !(0..=23).contains(&h))
    {
        return Err(ConfigError::OutOfRange);
    }
    let mut tx = pool.begin().await?;
    for (key, value) in [(ANSWER_DAYS_KEY, answer_days), (DIGEST_HOUR_KEY, digest_hour)] {
        if let Some(value) = value {
            sqlx::query(
                "INSERT INTO schema_meta (key, value) VALUES (?, ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )
            .bind(key)
            .bind(value.to_string())
            .execute(&mut *tx)
            .await?;
        }
    }
    // Zero days holds nothing, so a request still open would ask a question nobody waits on:
    // close them as dismissed, the way the owner would have.
    if answer_days == Some(0) {
        sqlx::query("UPDATE capture_requests SET state = ?, closed_at = ? WHERE state = ?")
            .bind(STATE_DISMISSED)
            .bind(stamp(now))
            .bind(STATE_OPEN)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub fn question_for(cause: &str) -> &'static str {
    match cause {
        "run_exhausted" => "Is there something about this item or the gate that only you know?",
        "review_blocking" => "Was the review right, or is there a rule of yours behind it?",
        _ => "What went wrong that the logs would not show?",
    }
}

/// One job's line in the digest: ids, project, its facts in arrival order, then the first cause's question.
pub fn question_line(project: &str, job_id: i64, facts: &[Fact]) -> String {
    let said: Vec<&str> = facts.iter().map(|f| f.fact.as_str()).collect();
    let ask = question_for(facts.first().map_or("", |f| f.cause.as_str()));
    format!("#{job_id} · {project} · job #{job_id} — {}. {ask}", said.join("; "))
}

/// Queue rows created before this UTC second never ask; see migration 0173. Absent: all may ask.
pub const FROM_KEY: &str = "distiller.capture_from";

#[derive(sqlx::FromRow)]
struct AskingRow {
    id: i64,
    cause: String,
    project_id: String,
    job_id: i64,
    item_id: Option<i64>,
    run_id: Option<i64>,
}

const DESCRIPTION_CHARS: usize = 60;

/// The one line of fact a queue row contributes. Read live, like the distiller's dossier: the
/// queue stores ids only. Never the raw gate output, never a review's stdout.
async fn fact_for(conn: &mut SqliteConnection, row: &AskingRow) -> sqlx::Result<String> {
    match row.cause.as_str() {
        "run_exhausted" => {
            let item: Option<(i64, String, i64, Option<String>)> = sqlx::query_as(
                "SELECT ordinal, description, gate_attempts, gate_output FROM job_nodes WHERE id = ?",
            )
            .bind(row.item_id)
            .fetch_optional(&mut *conn)
            .await?;
            Ok(match item {
                Some((ordinal, description, attempts, output)) => {
                    // One line, always: a newline would move the question and the #cap mark.
                    let short: String = description
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(DESCRIPTION_CHARS)
                        .collect();
                    let signature = output
                        .as_deref()
                        .and_then(crate::knowledge::failure_signature)
                        .map(|s| format!(": `{}`", s.headline))
                        .unwrap_or_default();
                    format!(
                        "item {ordinal} «{short}» used up {attempts} gate attempts{signature}"
                    )
                }
                None => "an item used up its gate attempts".to_owned(),
            })
        }
        "review_blocking" => Ok(match row.run_id {
            Some(run) => format!("the review blocked the job (run #{run})"),
            None => "the review blocked the job".to_owned(),
        }),
        _ => {
            // Not wait_reason: it is only meaningful while a job waits, and would mislead here.
            let status: Option<String> = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
                .bind(row.job_id)
                .fetch_optional(&mut *conn)
                .await?;
            Ok(match status {
                Some(status) => format!("the job ended in `{status}`"),
                None => "the job failed".to_owned(),
            })
        }
    }
}

fn facts_json(facts: &[Fact]) -> String {
    serde_json::to_string(facts).unwrap_or_else(|_| "[]".to_owned())
}

/// Close every open request whose deadline has come. `<= now` here is the exact complement of the
/// claim's `deadline > now`, so no instant both holds and expires a request.
pub async fn expire_due(conn: &mut SqliteConnection, now: DateTime<Utc>) -> sqlx::Result<u64> {
    let now = stamp(now);
    Ok(sqlx::query(
        "UPDATE capture_requests SET state = ?, closed_at = ? WHERE state = ? AND deadline <= ?",
    )
    .bind(STATE_EXPIRED)
    .bind(&now)
    .bind(STATE_OPEN)
    .bind(&now)
    .execute(&mut *conn)
    .await?
    .rows_affected())
}

#[derive(Debug)]
pub enum CaptureError {
    NotFound,
    Closed,
    Db(sqlx::Error),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::NotFound => f.write_str("no capture request for this job"),
            CaptureError::Closed => f.write_str("the capture request is already closed"),
            CaptureError::Db(error) => write!(f, "{error}"),
        }
    }
}

impl From<sqlx::Error> for CaptureError {
    fn from(error: sqlx::Error) -> Self {
        CaptureError::Db(error)
    }
}

/// Close an open request as answered by `note_id`. False when there was no open request: a late
/// answer keeps its note (http.rs made it) and changes nothing here.
pub async fn close_answered_in(
    conn: &mut SqliteConnection,
    job_id: i64,
    note_id: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let changed = sqlx::query(
        "UPDATE capture_requests SET state = ?, note_id = ?, closed_at = ? WHERE job_id = ? AND state = ?",
    )
    .bind(STATE_ANSWERED)
    .bind(note_id)
    .bind(stamp(now))
    .bind(job_id)
    .bind(STATE_OPEN)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok(changed == 1)
}

/// The owner has nothing to add: release the job now.
pub async fn dismiss(
    pool: &SqlitePool,
    job_id: i64,
    now: DateTime<Utc>,
) -> Result<(), CaptureError> {
    let changed = sqlx::query(
        "UPDATE capture_requests SET state = ?, closed_at = ? WHERE job_id = ? AND state = ?",
    )
    .bind(STATE_DISMISSED)
    .bind(stamp(now))
    .bind(job_id)
    .bind(STATE_OPEN)
    .execute(pool)
    .await?
    .rows_affected();
    if changed == 1 {
        return Ok(());
    }
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT job_id FROM capture_requests WHERE job_id = ?")
            .bind(job_id)
            .fetch_optional(pool)
            .await?;
    Err(if exists.is_some() {
        CaptureError::Closed
    } else {
        CaptureError::NotFound
    })
}

/// How many times an owner's answer or dismissal is written before a busy database is reported.
/// Each attempt already waits the pool's 10s busy_timeout, so two attempts stay under the Telegram
/// sidecar's 30s client timeout: a retry that outlived the client would save an answer the owner
/// was told was lost, and the owner's second try would then be a late duplicate.
pub const BUSY_ATTEMPTS: u32 = 2;

/// The pause before a retry, so it does not land on the same writer's next statement.
pub const BUSY_PAUSE: std::time::Duration = std::time::Duration::from_millis(500);

// The busy-database helpers live in the shared storage layer now, because writes that end a run
// need them too; re-exported so the callers here and in `http/knowledge.rs` keep their path.
pub use nucleos_base::storage::{is_busy, retry_busy};

/// Why an owner's answer to a capture request was not recorded.
#[derive(Debug)]
pub enum AnswerError {
    /// No capture request exists for the job.
    NotFound,
    /// The answer had no text.
    Empty,
    /// The origin is not one a note may carry.
    UnknownOrigin,
    Db(sqlx::Error),
    /// A note error that an answer cannot meet in practice, kept for the log.
    Internal(String),
}

impl std::fmt::Display for AnswerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("no capture request for this job"),
            Self::Empty => formatter.write_str("an answer needs some text"),
            Self::UnknownOrigin => formatter.write_str("unknown origin"),
            Self::Db(error) => write!(formatter, "database error: {error}"),
            Self::Internal(detail) => write!(formatter, "internal error: {detail}"),
        }
    }
}

/// The owner's answer becomes a note linked to the job; it also closes the request when it is
/// still open and schedules a new reading of the job. A late answer keeps its note too. Returns the
/// note's id and whether the owner's reading was scheduled.
pub async fn answer(
    pool: &SqlitePool,
    job_id: i64,
    text: &str,
    origin: &str,
) -> Result<(i64, bool), AnswerError> {
    let exists = sqlx::query_scalar::<_, i64>("SELECT 1 FROM capture_requests WHERE job_id = ?")
        .bind(job_id)
        .fetch_optional(pool)
        .await
        .map_err(AnswerError::Db)?
        .is_some();
    if !exists {
        return Err(AnswerError::NotFound);
    }
    // A busy database is retried: the owner typed this once, and another writer holding the lock
    // past busy_timeout is no reason to lose it.
    retry_busy(
        BUSY_ATTEMPTS,
        BUSY_PAUSE,
        |error: &AnswerError| matches!(error, AnswerError::Db(db) if is_busy(db)),
        || answer_step(pool, job_id, text, origin),
    )
    .await
}

/// One attempt at an owner's answer: the note, its link and the request's close commit together or
/// not at all, so a retry after a busy database never finds half of the previous try. Returns the
/// note's id and whether the owner's reading was scheduled.
async fn answer_step(
    pool: &SqlitePool,
    job_id: i64,
    text: &str,
    origin: &str,
) -> Result<(i64, bool), AnswerError> {
    let mut tx = pool.begin().await.map_err(AnswerError::Db)?;
    // The one call this module makes into the owner's notes (sanctioned by the guard in
    // `owner_notes.rs`); its error converts through the `From` impl that lives beside `NoteError`.
    let note_id =
        crate::owner_notes::create_linked_to_job_in(&mut tx, text, origin, job_id).await?;
    close_answered_in(&mut tx, job_id, note_id, chrono::Utc::now())
        .await
        .map_err(AnswerError::Db)?;
    let requeued = crate::distill::enqueue_answered_in(&mut tx, job_id, note_id)
        .await
        .map_err(AnswerError::Db)?;
    tx.commit().await.map_err(AnswerError::Db)?;
    Ok((note_id, requeued))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    Open,
    All,
}

impl Listing {
    pub fn parse(s: &str) -> Option<Listing> {
        match s {
            "open" => Some(Listing::Open),
            "all" => Some(Listing::All),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CaptureRequest {
    pub job_id: i64,
    pub project_id: String,
    pub causes: Vec<String>,
    pub prompt_text: String,
    pub state: String,
    pub deadline: String,
    pub seconds_left: i64,
    pub note_id: Option<i64>,
    pub created_at: String,
    pub closed_at: Option<String>,
    pub digest_id: Option<i64>,
}

type ListRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    Option<i64>,
    String,
    Option<String>,
    Option<i64>,
);

/// Requests for the shell, nearest deadline first; `seconds_left` is 0 for a closed or due one.
pub async fn list(
    pool: &SqlitePool,
    listing: Listing,
    now: DateTime<Utc>,
) -> sqlx::Result<Vec<CaptureRequest>> {
    let rows: Vec<ListRow> = sqlx::query_as(
        "SELECT job_id, project_id, causes, prompt_text, state, deadline, note_id, created_at, closed_at,
                digest_id
           FROM capture_requests WHERE (? OR state = ?) ORDER BY deadline, job_id",
    )
    .bind(listing == Listing::All)
    .bind(STATE_OPEN)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                job_id,
                project_id,
                causes,
                prompt_text,
                state,
                deadline,
                note_id,
                created_at,
                closed_at,
                digest_id,
            )| {
                let seconds_left = if state == STATE_OPEN {
                    DateTime::parse_from_rfc3339(&deadline)
                        .map(|d| (d.with_timezone(&Utc) - now).num_seconds().max(0))
                        .unwrap_or(0)
                } else {
                    0
                };
                let causes = serde_json::from_str::<Vec<Fact>>(&causes)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|f| f.cause)
                    .collect();
                CaptureRequest {
                    job_id,
                    project_id,
                    causes,
                    prompt_text,
                    state,
                    deadline,
                    seconds_left,
                    note_id,
                    created_at,
                    closed_at,
                    digest_id,
                }
            },
        )
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(row: i64, cause: &str, line: &str) -> Fact {
        Fact {
            row,
            cause: cause.to_owned(),
            fact: line.to_owned(),
        }
    }

    #[test]
    fn a_stamp_has_a_fixed_width_so_text_order_is_time_order() {
        let t = chrono::DateTime::parse_from_rfc3339("2026-10-07T10:00:00.123456+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(stamp(t), "2026-10-07T10:00:00+00:00");
        assert!(stamp(t) < stamp(t + chrono::Duration::seconds(1)));
    }

    #[test]
    fn only_failure_causes_ask() {
        use crate::distill::Cause;
        assert_eq!(
            ASKING_CAUSES,
            [Cause::JobFailed, Cause::RunExhausted, Cause::ReviewBlocking]
        );
    }

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn job(pool: &SqlitePool, id: i64, status: &str) {
        sqlx::query(
            "INSERT INTO jobs (id, project_id, project_root, status, max_items, created_at)
             VALUES (?, 'web', '/tmp/web', ?, 3, '2026-10-07T09:00:00+00:00')",
        )
        .bind(id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn request(pool: &SqlitePool, job_id: i64) -> Option<(String, String, String, String)> {
        sqlx::query_as(
            "SELECT state, causes, prompt_text, deadline FROM capture_requests WHERE job_id = ?",
        )
        .bind(job_id)
        .fetch_optional(pool)
        .await
        .unwrap()
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[tokio::test]
    async fn the_answer_window_and_hour_default_to_5_days_and_18() {
        let pool = pool().await;
        assert_eq!((answer_days(&pool).await, digest_hour(&pool).await), (5, 18));
    }

    #[tokio::test]
    async fn a_legacy_zero_wait_keeps_requests_off_and_any_other_value_gives_the_default() {
        let pool = pool().await;
        set_meta(&pool, OLD_WAIT_KEY, "0").await;
        assert_eq!(answer_days(&pool).await, 0, "an owner who turned requests off keeps them off");
        set_meta(&pool, OLD_WAIT_KEY, "30").await;
        assert_eq!(answer_days(&pool).await, 5, "minutes are not converted into days");
        set_meta(&pool, ANSWER_DAYS_KEY, "9").await;
        assert_eq!(answer_days(&pool).await, 9, "the new key wins over the old one");
        set_meta(&pool, ANSWER_DAYS_KEY, "31").await;
        assert_eq!(answer_days(&pool).await, 5, "out of range reads as the default");
    }

    #[tokio::test]
    async fn the_config_is_set_within_bounds_and_zero_days_dismisses_the_open_requests() {
        let pool = pool().await;
        let now = at("2026-10-07T10:00:00+00:00");
        assert!(matches!(set_config(&pool, Some(31), None, now).await, Err(ConfigError::OutOfRange)));
        assert!(matches!(set_config(&pool, None, Some(24), now).await, Err(ConfigError::OutOfRange)));
        set_config(&pool, Some(7), Some(9), now).await.unwrap();
        assert_eq!((answer_days(&pool).await, digest_hour(&pool).await), (7, 9));

        sqlx::query(
            "INSERT INTO capture_requests (job_id, project_id, causes, prompt_text, state, deadline, created_at)
             VALUES (7, 'p', '[]', 'q', 'open', '2999-01-01T00:00:00+00:00', 't')",
        )
        .execute(&pool)
        .await
        .unwrap();
        set_config(&pool, Some(0), None, now).await.unwrap();
        assert_eq!(request(&pool, 7).await.unwrap().0, STATE_DISMISSED);
    }

    #[test]
    fn the_question_is_fixed_per_cause_and_the_line_is_one_line() {
        let line = question_line(
            "api",
            9,
            &[fact(1, "run_exhausted", "item 2 «add retries» used up 3 gate attempts: `error[E0382]`")],
        );
        assert_eq!(
            line,
            "#9 · api · job #9 — item 2 «add retries» used up 3 gate attempts: `error[E0382]`. Is there something about this item or the gate that only you know?"
        );
        assert_eq!(
            question_line("api", 7, &[fact(1, "job_failed", "the job ended in `failed`")]),
            "#7 · api · job #7 — the job ended in `failed`. What went wrong that the logs would not show?"
        );
        assert!(question_for("review_blocking")
            .contains("Was the review right, or is there a rule of yours behind it?"));
        assert!(!line.contains('\n'));
    }

    #[tokio::test]
    async fn expire_due_closes_requests_without_any_digest_running() {
        let pool = pool().await;
        sqlx::query(
            "INSERT INTO capture_requests (job_id, project_id, causes, prompt_text, state, deadline, created_at)
             VALUES (7, 'web', '[]', 'q', 'open', '2026-10-07T12:00:00+00:00', 't')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        assert_eq!(
            expire_due(&mut conn, at("2026-10-07T11:59:59+00:00"))
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            expire_due(&mut conn, at("2026-10-07T12:00:00+00:00"))
                .await
                .unwrap(),
            1
        );
        drop(conn);
        assert_eq!(request(&pool, 7).await.unwrap().0, STATE_EXPIRED);
    }

    async fn set_meta(pool: &SqlitePool, key: &str, value: &str) {
        sqlx::query("INSERT OR REPLACE INTO schema_meta (key, value) VALUES (?, ?)")
            .bind(key)
            .bind(value)
            .execute(pool)
            .await
            .unwrap();
    }

    /// An open request for `job_id`, as a digest would have left it, answerable for two hours.
    async fn opened(pool: &SqlitePool, job_id: i64) -> DateTime<Utc> {
        job(pool, job_id, "failed").await;
        let now = at("2026-10-07T10:00:00+00:00");
        sqlx::query(
            "INSERT INTO capture_requests (job_id, project_id, causes, prompt_text, state, deadline, created_at)
             VALUES (?, 'web', ?, 'q', ?, ?, ?)",
        )
        .bind(job_id)
        .bind(facts_json(&[fact(1, "job_failed", "the job ended in `failed`")]))
        .bind(STATE_OPEN)
        .bind(stamp(now + chrono::Duration::minutes(120)))
        .bind(stamp(now))
        .execute(pool)
        .await
        .unwrap();
        now
    }

    #[tokio::test]
    async fn an_answer_closes_an_open_request_and_leaves_a_closed_one_alone() {
        let pool = pool().await;
        let now = opened(&pool, 7).await;
        let mut tx = pool.begin().await.unwrap();
        assert!(close_answered_in(&mut tx, 7, 31, now).await.unwrap());
        assert!(
            !close_answered_in(&mut tx, 7, 32, now).await.unwrap(),
            "a late answer changes nothing"
        );
        assert!(!close_answered_in(&mut tx, 99, 33, now).await.unwrap());
        tx.commit().await.unwrap();
        let (state, note): (String, Option<i64>) =
            sqlx::query_as("SELECT state, note_id FROM capture_requests WHERE job_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((state.as_str(), note), (STATE_ANSWERED, Some(31)));
    }

    #[tokio::test]
    async fn capture_answer_creates_a_linked_note_and_closes_the_request() {
        let pool = pool().await;
        opened(&pool, 7).await;
        let (note_id, requeued) = answer(&pool, 7, "only I knew this", "shell").await.unwrap();
        assert!(requeued);
        let (state, note): (String, Option<i64>) =
            sqlx::query_as("SELECT state, note_id FROM capture_requests WHERE job_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((state.as_str(), note), (STATE_ANSWERED, Some(note_id)));
        let (_, late) = answer(&pool, 7, "a second thought", "shell").await.unwrap();
        assert!(late, "a late answer keeps its note and rides the row already pending");
    }

    #[tokio::test]
    async fn capture_answer_unknown_job_is_not_found() {
        let pool = pool().await;
        assert!(matches!(
            answer(&pool, 99, "text", "shell").await,
            Err(AnswerError::NotFound)
        ));
    }

    #[tokio::test]
    async fn capture_answer_empty_text_is_empty() {
        let pool = pool().await;
        opened(&pool, 7).await;
        assert!(matches!(
            answer(&pool, 7, "   ", "shell").await,
            Err(AnswerError::Empty)
        ));
        assert_eq!(request(&pool, 7).await.unwrap().0, STATE_OPEN);
    }

    #[tokio::test]
    async fn capture_answer_unknown_origin_is_refused() {
        let pool = pool().await;
        opened(&pool, 7).await;
        assert!(matches!(
            answer(&pool, 7, "text", "carrier-pigeon").await,
            Err(AnswerError::UnknownOrigin)
        ));
        assert_eq!(request(&pool, 7).await.unwrap().0, STATE_OPEN);
    }

    #[tokio::test]
    async fn dismissing_twice_or_an_unknown_job_is_refused() {
        let pool = pool().await;
        let now = opened(&pool, 7).await;
        assert!(dismiss(&pool, 7, now).await.is_ok());
        assert!(matches!(
            dismiss(&pool, 7, now).await,
            Err(CaptureError::Closed)
        ));
        assert!(matches!(
            dismiss(&pool, 99, now).await,
            Err(CaptureError::NotFound)
        ));
        assert_eq!(request(&pool, 7).await.unwrap().0, STATE_DISMISSED);
    }

    #[tokio::test]
    async fn the_list_shows_open_requests_with_their_time_left() {
        let pool = pool().await;
        let now = opened(&pool, 7).await;
        opened(&pool, 8).await;
        dismiss(&pool, 8, now).await.unwrap();

        let open = list(&pool, Listing::Open, now + chrono::Duration::minutes(30))
            .await
            .unwrap();
        assert_eq!(open.iter().map(|r| r.job_id).collect::<Vec<_>>(), [7]);
        assert_eq!(open[0].seconds_left, 90 * 60);
        assert_eq!(open[0].causes, ["job_failed"]);

        let all = list(&pool, Listing::All, now).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all.iter().find(|r| r.job_id == 8).unwrap().seconds_left, 0);
    }

    /// A real SQLITE_BUSY: one connection holds the write lock, a second with no busy_timeout writes.
    async fn a_busy_error() -> sqlx::Error {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use sqlx::{Connection, Executor};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("busy.db");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let mut holder = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        holder.execute("CREATE TABLE t (x INTEGER)").await.unwrap();
        holder.execute("BEGIN IMMEDIATE").await.unwrap();
        holder.execute("INSERT INTO t VALUES (1)").await.unwrap();
        let other = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.busy_timeout(std::time::Duration::ZERO))
            .await
            .unwrap();
        sqlx::query("INSERT INTO t VALUES (2)")
            .execute(&other)
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn a_locked_database_is_busy_and_other_errors_are_not() {
        assert!(is_busy(&a_busy_error().await));
        assert!(!is_busy(&sqlx::Error::RowNotFound));
        let pool = crate::testdb::fresh_pool().await;
        let syntax = sqlx::query("SELEC 1").execute(&pool).await.unwrap_err();
        assert!(!is_busy(&syntax));
    }

    #[tokio::test]
    async fn a_busy_write_is_retried_once_and_then_passes() {
        let busy = a_busy_error().await;
        let mut first = Some(busy);
        let mut calls = 0;
        let done: Result<i64, sqlx::Error> =
            retry_busy(BUSY_ATTEMPTS, std::time::Duration::ZERO, is_busy, || {
                calls += 1;
                let outcome = match first.take() {
                    Some(error) => Err(error),
                    None => Ok(7),
                };
                async move { outcome }
            })
            .await;
        assert_eq!(done.unwrap(), 7);
        assert_eq!(calls, 2);
    }

    #[tokio::test]
    async fn a_write_still_busy_after_its_attempts_reports_the_busy_error() {
        let mut calls = 0;
        let mut errors = vec![
            a_busy_error().await,
            a_busy_error().await,
            a_busy_error().await,
        ];
        let done: Result<(), sqlx::Error> =
            retry_busy(BUSY_ATTEMPTS, std::time::Duration::ZERO, is_busy, || {
                calls += 1;
                let error = errors.pop().unwrap();
                async move { Err(error) }
            })
            .await;
        assert!(is_busy(&done.unwrap_err()));
        assert_eq!(calls, BUSY_ATTEMPTS);
    }

    #[tokio::test]
    async fn any_other_error_is_not_retried() {
        let mut calls = 0;
        let done: Result<(), sqlx::Error> =
            retry_busy(BUSY_ATTEMPTS, std::time::Duration::ZERO, is_busy, || {
                calls += 1;
                async { Err(sqlx::Error::RowNotFound) }
            })
            .await;
        assert!(matches!(done, Err(sqlx::Error::RowNotFound)));
        assert_eq!(calls, 1);
    }
}
