//! Capture requests: when a job went wrong, the distiller asks the owner, once, what only they
//! know about it, and holds that job's queue rows until the answer, a dismissal or the deadline.
//! Design: .ai/specs/2026-10-07-pedidos-captura-design.md.
//!
//! The question is a fixed template (no model, spec P5) and is redacted before it is written. An
//! answer is known here only as a note id: composing the note is http.rs's job (spec 5.3).

use crate::distill::Cause;
use chrono::{DateTime, Duration, Local, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

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

/// What the first and the last line of the question say.
pub struct Header<'a> {
    pub project: &'a str,
    pub job_id: i64,
    /// The deadline as the owner reads it, local `HH:MM`.
    pub until: &'a str,
}

/// The one timestamp format of this table: fixed width, so `deadline > now` as text is time order.
pub fn stamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, false)
}

fn cause_label(cause: &str) -> &'static str {
    match cause {
        "job_failed" => "o job falhou",
        "run_exhausted" => "o gate esgotou as tentativas",
        "review_blocking" => "a review bloqueou",
        _ => "algo correu mal",
    }
}

/// The question, from its header and its facts, in the order the facts arrived.
pub fn render(header: &Header, facts: &[Fact]) -> String {
    let mut labels: Vec<&str> = Vec::new();
    for f in facts {
        let label = cause_label(&f.cause);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    let mut out = format!(
        "🧠 {} · job #{} · {}\n",
        header.project,
        header.job_id,
        labels.join(", ")
    );
    for f in facts {
        out.push_str(&f.fact);
        out.push('\n');
    }
    out.push_str("Há alguma coisa que só tu saibas sobre isto?\n");
    out.push_str(&format!(
        "(até às {}; depois o destilador avança) #cap{}",
        header.until, header.job_id
    ));
    out
}

/// Where the wait lives: a row of `schema_meta`, like `distiller.model`, read every tick.
pub const WAIT_SETTING_KEY: &str = "distiller.capture_wait_minutes";
pub const DEFAULT_WAIT_MINUTES: i64 = 120;
/// A week. Beyond it the deadline arithmetic could overflow and panic on every tick, inside the
/// worker's claiming transaction; no owner means to hold a job longer than that.
pub const MAX_WAIT_MINUTES: i64 = 7 * 24 * 60;

/// Minutes a request holds its job. `0` turns requests off; anything unreadable or out of range is
/// the default.
pub async fn wait_minutes(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar::<_, String>("SELECT value FROM schema_meta WHERE key = ?")
        .bind(WAIT_SETTING_KEY)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .and_then(|stored| stored.trim().parse::<i64>().ok())
        .filter(|minutes| (0..=MAX_WAIT_MINUTES).contains(minutes))
        .unwrap_or(DEFAULT_WAIT_MINUTES)
}

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
                "SELECT ordinal, description, gate_attempts, gate_output FROM job_items WHERE id = ?",
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
                        "o item {ordinal} «{short}» esgotou {attempts} tentativas no gate{signature}"
                    )
                }
                None => "um item esgotou as tentativas no gate".to_owned(),
            })
        }
        "review_blocking" => Ok(match row.run_id {
            Some(run) => format!("a review bloqueou o job (run #{run})"),
            None => "a review bloqueou o job".to_owned(),
        }),
        _ => {
            // Not wait_reason: it is only meaningful while a job waits, and would mislead here.
            let status: Option<String> = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
                .bind(row.job_id)
                .fetch_optional(&mut *conn)
                .await?;
            Ok(match status {
                Some(status) => format!("o job terminou em `{status}`"),
                None => "o job falhou".to_owned(),
            })
        }
    }
}

fn until_local(deadline: &str) -> String {
    DateTime::parse_from_rfc3339(deadline)
        .map(|d| d.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_else(|_| "?".to_owned())
}

/// The question exactly as it is stored: rendered, then redacted.
fn written(project: &str, job_id: i64, deadline: &str, facts: &[Fact]) -> String {
    let until = until_local(deadline);
    let header = Header {
        project,
        job_id,
        until: &until,
    };
    crate::redact::redact_secrets(&render(&header, facts))
}

fn facts_json(facts: &[Fact]) -> String {
    serde_json::to_string(facts).unwrap_or_else(|_| "[]".to_owned())
}

/// Open a request for every job with a pending failure row and no request yet, or add a new row's
/// fact to its open request. A closed request is never reopened (spec 3.1). Returns how many
/// requests it opened. Runs inside the worker's claiming transaction.
pub async fn open_due(
    conn: &mut SqliteConnection,
    now: DateTime<Utc>,
    wait_minutes: i64,
) -> sqlx::Result<u32> {
    if wait_minutes <= 0 {
        return Ok(0);
    }
    let rows: Vec<AskingRow> = sqlx::query_as(
        "SELECT id, cause, project_id, job_id, item_id, run_id FROM distill_queue
          WHERE status = ? AND job_id IS NOT NULL AND cause IN (?, ?, ?)
          ORDER BY id",
    )
    .bind(crate::distill::STATUS_PENDING)
    .bind(ASKING_CAUSES[0].as_str())
    .bind(ASKING_CAUSES[1].as_str())
    .bind(ASKING_CAUSES[2].as_str())
    .fetch_all(&mut *conn)
    .await?;

    let mut opened = 0;
    for row in rows {
        let existing: Option<(String, String, String)> =
            sqlx::query_as("SELECT state, causes, deadline FROM capture_requests WHERE job_id = ?")
                .bind(row.job_id)
                .fetch_optional(&mut *conn)
                .await?;
        match existing {
            None => {
                let deadline = stamp(now + Duration::minutes(wait_minutes));
                // Redacted at the source: the fact is stored in `causes` too, not only in the question.
                let fact = crate::redact::redact_secrets(&fact_for(&mut *conn, &row).await?);
                let facts = vec![Fact {
                    row: row.id,
                    cause: row.cause.clone(),
                    fact,
                }];
                let text = written(&row.project_id, row.job_id, &deadline, &facts);
                sqlx::query(
                    "INSERT INTO capture_requests
                         (job_id, project_id, causes, prompt_text, state, deadline, created_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(row.job_id)
                .bind(&row.project_id)
                .bind(facts_json(&facts))
                .bind(&text)
                .bind(STATE_OPEN)
                .bind(&deadline)
                .bind(stamp(now))
                .execute(&mut *conn)
                .await?;
                crate::feed::append_on(
                    &mut *conn,
                    Some(&row.project_id),
                    FEED_KIND,
                    &text,
                    None,
                    Some(&crate::feed::Subject::Job(row.job_id)),
                )
                .await?;
                opened += 1;
            }
            Some((state, causes, deadline)) if state == STATE_OPEN => {
                // Unreadable facts are left as they are: rewriting them from an empty list would
                // drop every earlier fact without a trace.
                let Ok(mut facts) = serde_json::from_str::<Vec<Fact>>(&causes) else {
                    continue;
                };
                if facts.iter().any(|f| f.row == row.id) {
                    continue;
                }
                // Redacted at the source: the fact is stored in `causes` too, not only in the question.
                let fact = crate::redact::redact_secrets(&fact_for(&mut *conn, &row).await?);
                facts.push(Fact {
                    row: row.id,
                    cause: row.cause.clone(),
                    fact,
                });
                let text = written(&row.project_id, row.job_id, &deadline, &facts);
                sqlx::query("UPDATE capture_requests SET causes = ?, prompt_text = ? WHERE job_id = ? AND state = ?")
                    .bind(facts_json(&facts))
                    .bind(&text)
                    .bind(row.job_id)
                    .bind(STATE_OPEN)
                    .execute(&mut *conn)
                    .await?;
            }
            Some(_) => {}
        }
    }
    Ok(opened)
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
);

/// Requests for the shell, nearest deadline first; `seconds_left` is 0 for a closed or due one.
pub async fn list(
    pool: &SqlitePool,
    listing: Listing,
    now: DateTime<Utc>,
) -> sqlx::Result<Vec<CaptureRequest>> {
    let rows: Vec<ListRow> = sqlx::query_as(
        "SELECT job_id, project_id, causes, prompt_text, state, deadline, note_id, created_at, closed_at
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
    fn the_question_is_one_fixed_template() {
        let header = Header {
            project: "web",
            job_id: 7,
            until: "14:30",
        };
        let cases = [
            (
                vec![fact(1, "job_failed", "o job terminou em `failed`")],
                "🧠 web · job #7 · o job falhou\n\
                 o job terminou em `failed`\n\
                 Há alguma coisa que só tu saibas sobre isto?\n\
                 (até às 14:30; depois o destilador avança) #cap7",
            ),
            (
                vec![
                    fact(
                        1,
                        "run_exhausted",
                        "o item 2 «build» esgotou 3 tentativas no gate: `E0425`",
                    ),
                    fact(4, "review_blocking", "a review bloqueou o job (run #9)"),
                ],
                "🧠 web · job #7 · o gate esgotou as tentativas, a review bloqueou\n\
                 o item 2 «build» esgotou 3 tentativas no gate: `E0425`\n\
                 a review bloqueou o job (run #9)\n\
                 Há alguma coisa que só tu saibas sobre isto?\n\
                 (até às 14:30; depois o destilador avança) #cap7",
            ),
            (
                vec![fact(1, "run_exhausted", "a"), fact(2, "run_exhausted", "b")],
                "🧠 web · job #7 · o gate esgotou as tentativas\na\nb\n\
                 Há alguma coisa que só tu saibas sobre isto?\n\
                 (até às 14:30; depois o destilador avança) #cap7",
            ),
        ];
        for (facts, want) in cases {
            assert_eq!(render(&header, &facts), want);
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

    async fn queued(
        pool: &SqlitePool,
        cause: &str,
        job_id: i64,
        item_id: Option<i64>,
        run_id: Option<i64>,
    ) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO distill_queue (cause, project_id, job_id, item_id, run_id, status, attempts, created_at)
             VALUES (?, 'web', ?, ?, ?, 'pending', 0, '2026-10-07T09:00:00+00:00') RETURNING id",
        )
        .bind(cause)
        .bind(job_id)
        .bind(item_id)
        .bind(run_id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn tick(pool: &SqlitePool, now: DateTime<Utc>, wait: i64) {
        let mut tx = pool.begin().await.unwrap();
        open_due(&mut tx, now, wait).await.unwrap();
        expire_due(&mut tx, now).await.unwrap();
        tx.commit().await.unwrap();
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
    async fn a_failed_job_opens_one_request_and_one_feed_line() {
        let pool = pool().await;
        job(&pool, 7, "failed").await;
        queued(&pool, "job_failed", 7, None, None).await;

        tick(&pool, at("2026-10-07T10:00:00+00:00"), 120).await;

        let (state, causes, text, deadline) = request(&pool, 7).await.expect("a request");
        assert_eq!(state, STATE_OPEN);
        assert_eq!(deadline, "2026-10-07T12:00:00+00:00");
        assert!(
            text.contains("o job terminou em `failed`") && text.ends_with("#cap7"),
            "{text}"
        );
        assert_eq!(serde_json::from_str::<Vec<Fact>>(&causes).unwrap().len(), 1);
        let feed: Vec<(String, String)> =
            sqlx::query_as("SELECT kind, summary FROM feed WHERE subject = 'job:7'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(feed, vec![(FEED_KIND.to_owned(), text)]);
    }

    #[tokio::test]
    async fn a_success_cause_or_a_zero_wait_opens_nothing() {
        let pool = pool().await;
        job(&pool, 7, "done").await;
        queued(&pool, "job_landed", 7, None, None).await;
        queued(&pool, "gate_recovered", 7, None, None).await;
        tick(&pool, at("2026-10-07T10:00:00+00:00"), 120).await;
        assert!(request(&pool, 7).await.is_none());

        job(&pool, 8, "failed").await;
        queued(&pool, "job_failed", 8, None, None).await;
        tick(&pool, at("2026-10-07T10:00:00+00:00"), 0).await;
        assert!(request(&pool, 8).await.is_none());
    }

    #[tokio::test]
    async fn a_second_cause_joins_once_however_many_ticks_run() {
        let pool = pool().await;
        job(&pool, 7, "failed").await;
        queued(&pool, "job_failed", 7, None, None).await;
        let now = at("2026-10-07T10:00:00+00:00");
        tick(&pool, now, 120).await;
        // The review fact reads only the queue row's run_id, so no run row is needed.
        queued(&pool, "review_blocking", 7, None, Some(9)).await;
        for _ in 0..3 {
            tick(&pool, now + chrono::Duration::minutes(1), 120).await;
        }
        let (_, causes, text, deadline) = request(&pool, 7).await.unwrap();
        let facts: Vec<Fact> = serde_json::from_str(&causes).unwrap();
        assert_eq!(
            facts.iter().map(|f| f.cause.as_str()).collect::<Vec<_>>(),
            ["job_failed", "review_blocking"]
        );
        assert!(text.contains("a review bloqueou o job (run #9)"), "{text}");
        assert_eq!(
            deadline, "2026-10-07T12:00:00+00:00",
            "joining never moves the deadline"
        );
        let feed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE subject = 'job:7'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(feed, 1, "only the opening is announced");
    }

    #[tokio::test]
    async fn an_exhausted_item_names_its_signature_never_the_raw_output() {
        let pool = pool().await;
        job(&pool, 7, "failed").await;
        let item: i64 = sqlx::query_scalar(
            "INSERT INTO job_items (job_id, ordinal, description, status, gate_attempts, gate_output)
             VALUES (7, 2, 'build the thing', 'gate_failed', 3, ?) RETURNING id",
        )
        .bind("error[E0425]: cannot find value `x` in this scope\n --> src/a.rs:1:1\nlots of raw lines")
        .fetch_one(&pool)
        .await
        .unwrap();
        queued(&pool, "run_exhausted", 7, Some(item), None).await;
        tick(&pool, at("2026-10-07T10:00:00+00:00"), 120).await;
        let (_, _, text, _) = request(&pool, 7).await.unwrap();
        assert!(
            text.contains("o item 2 «build the thing» esgotou 3 tentativas no gate"),
            "{text}"
        );
        assert!(!text.contains("lots of raw lines"), "{text}");
    }

    #[tokio::test]
    async fn a_closed_request_is_never_reopened() {
        let pool = pool().await;
        job(&pool, 7, "failed").await;
        queued(&pool, "job_failed", 7, None, None).await;
        let now = at("2026-10-07T10:00:00+00:00");
        tick(&pool, now, 120).await;
        sqlx::query("UPDATE capture_requests SET state = 'dismissed' WHERE job_id = 7")
            .execute(&pool)
            .await
            .unwrap();
        queued(&pool, "run_exhausted", 7, None, None).await;
        tick(&pool, now, 120).await;
        let (state, causes, _, _) = request(&pool, 7).await.unwrap();
        assert_eq!(state, STATE_DISMISSED);
        assert_eq!(serde_json::from_str::<Vec<Fact>>(&causes).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unreadable_facts_are_never_rewritten_from_nothing() {
        let pool = pool().await;
        job(&pool, 7, "failed").await;
        queued(&pool, "job_failed", 7, None, None).await;
        let now = at("2026-10-07T10:00:00+00:00");
        tick(&pool, now, 120).await;
        sqlx::query("UPDATE capture_requests SET causes = 'not json' WHERE job_id = 7")
            .execute(&pool)
            .await
            .unwrap();
        queued(&pool, "run_exhausted", 7, None, None).await;
        tick(&pool, now, 120).await;
        let (state, causes, _, _) = request(&pool, 7).await.unwrap();
        assert_eq!((state.as_str(), causes.as_str()), (STATE_OPEN, "not json"));
    }

    #[tokio::test]
    async fn the_deadline_expires_a_request_exactly_at_its_instant() {
        let pool = pool().await;
        job(&pool, 7, "failed").await;
        queued(&pool, "job_failed", 7, None, None).await;
        let opened = at("2026-10-07T10:00:00+00:00");
        tick(&pool, opened, 120).await;
        tick(&pool, opened + chrono::Duration::minutes(119), 120).await;
        assert_eq!(request(&pool, 7).await.unwrap().0, STATE_OPEN);
        tick(&pool, opened + chrono::Duration::minutes(120), 120).await;
        assert_eq!(request(&pool, 7).await.unwrap().0, STATE_EXPIRED);
    }

    #[tokio::test]
    async fn a_secret_in_a_fact_never_reaches_the_question() {
        const SECRET: &str = "rk_live_51H8abcdefghijklmnop";
        let pool = pool().await;
        job(&pool, 7, "failed").await;
        let item: i64 = sqlx::query_scalar(
            "INSERT INTO job_items (job_id, ordinal, description, status, gate_attempts)
             VALUES (7, 1, ?, 'gate_failed', 2) RETURNING id",
        )
        .bind(format!("use {SECRET}"))
        .fetch_one(&pool)
        .await
        .unwrap();
        queued(&pool, "run_exhausted", 7, Some(item), None).await;
        tick(&pool, at("2026-10-07T10:00:00+00:00"), 120).await;
        let (_, causes, text, _) = request(&pool, 7).await.unwrap();
        assert!(!text.contains(SECRET), "{text}");
        assert!(
            !causes.contains(SECRET),
            "the stored facts are redacted too: {causes}"
        );
    }

    #[tokio::test]
    async fn the_wait_is_read_from_schema_meta_with_a_default() {
        let pool = pool().await;
        assert_eq!(wait_minutes(&pool).await, DEFAULT_WAIT_MINUTES);
        for (stored, want) in [
            ("0", 0),
            ("45", 45),
            ("-3", DEFAULT_WAIT_MINUTES),
            ("soon", DEFAULT_WAIT_MINUTES),
            ("10080", MAX_WAIT_MINUTES),
            ("999999999999", DEFAULT_WAIT_MINUTES),
        ] {
            sqlx::query("INSERT OR REPLACE INTO schema_meta (key, value) VALUES (?, ?)")
                .bind(WAIT_SETTING_KEY)
                .bind(stored)
                .execute(&pool)
                .await
                .unwrap();
            assert_eq!(wait_minutes(&pool).await, want, "stored {stored}");
        }
    }

    async fn opened(pool: &SqlitePool, job_id: i64) -> DateTime<Utc> {
        job(pool, job_id, "failed").await;
        queued(pool, "job_failed", job_id, None, None).await;
        let now = at("2026-10-07T10:00:00+00:00");
        tick(pool, now, 120).await;
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
}
