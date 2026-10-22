//! The daily digest of capture questions (spec 2026-10-22 B7, section 7). No model: the text is a
//! fixed template.

use super::{
    ASKING_CAUSES, AskingRow, FROM_KEY, Fact, STATE_OPEN, fact_for, facts_json, question_line, stamp,
};
use chrono::{DateTime, Duration, TimeZone, Timelike, Utc};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::BTreeMap;

pub const FEED_KIND: &str = "capture_digest";
/// Questions spelled out in the text; the rest are counted and stay answerable in the Brain.
pub const MAX_QUESTIONS: usize = 8;

pub async fn run_if_due(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<Option<i64>> {
    run_if_due_in(pool, now, &chrono::Local).await
}

/// Open the day's digest when the local hour has come, requests are on, and there is something new
/// to ask. The key is the local date, not 24 hours. Returns the digest id.
pub async fn run_if_due_in<Tz>(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    tz: &Tz,
) -> sqlx::Result<Option<i64>>
where
    Tz: TimeZone,
    Tz::Offset: std::fmt::Display,
{
    let days = super::answer_days(pool).await;
    let local = now.with_timezone(tz);
    if days == 0 || local.hour() < super::digest_hour(pool).await {
        return Ok(None);
    }
    let day = local.format("%Y-%m-%d").to_string();
    let done: Option<i64> = sqlx::query_scalar("SELECT 1 FROM capture_digests WHERE day = ?")
        .bind(&day)
        .fetch_optional(pool)
        .await?;
    if done.is_some() {
        return Ok(None);
    }
    let until = (now + Duration::days(days))
        .with_timezone(tz)
        .format("%Y-%m-%d")
        .to_string();
    // IMMEDIATE, like `claim_next`: the candidate read and the writes are one unit.
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let jobs = candidates(&mut tx, now, days).await?;
    if jobs.is_empty() {
        tx.rollback().await?;
        return Ok(None);
    }
    let ids: Vec<i64> = jobs.iter().map(|(job, _)| *job).collect();
    let digest_id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO capture_digests (day, job_ids, created_at) VALUES (?, ?, ?)
         ON CONFLICT(day) DO NOTHING RETURNING id",
    )
    .bind(&day)
    .bind(serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_owned()))
    .bind(stamp(now))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(digest_id) = digest_id else {
        tx.rollback().await?;
        return Ok(None);
    };

    let deadline = stamp(now + Duration::days(days));
    let mut lines = Vec::new();
    for (job, rows) in &jobs {
        let mut facts = Vec::new();
        for row in rows {
            // Redacted at the source: the fact is stored in `causes` too, not only in the text.
            let fact =
                crate::governance::redact::redact_secrets(&fact_for(&mut tx, row).await?);
            facts.push(Fact {
                row: row.id,
                cause: row.cause.clone(),
                fact,
            });
        }
        let line = crate::governance::redact::redact_secrets(&question_line(
            &rows[0].project_id,
            *job,
            &facts,
        ));
        sqlx::query(
            "INSERT INTO capture_requests
                 (job_id, project_id, causes, prompt_text, state, deadline, created_at, digest_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(job)
        .bind(&rows[0].project_id)
        .bind(facts_json(&facts))
        .bind(&line)
        .bind(STATE_OPEN)
        .bind(&deadline)
        .bind(stamp(now))
        .bind(digest_id)
        .execute(&mut *tx)
        .await?;
        lines.push(line);
    }
    let more = lines.len().saturating_sub(MAX_QUESTIONS);
    lines.truncate(MAX_QUESTIONS);
    let text = render(&day, &lines, more, &until);
    // No project: the digest crosses projects, so it belongs to the main conversation.
    crate::feed::append_on(
        &mut tx,
        None,
        FEED_KIND,
        &text,
        None,
        Some(&crate::feed::Subject::CaptureDigest(digest_id)),
    )
    .await?;
    tx.commit().await?;
    Ok(Some(digest_id))
}

/// The jobs to ask about, newest first (by their newest queue row), each with its asking rows in
/// arrival order. A job qualifies by a queue row of an asking cause inside the answer window, in any
/// queue state, and no request of any state yet. The `capture_from` marker (migration 0173) keeps
/// still-pending rows queued before it from asking.
async fn candidates(
    conn: &mut SqliteConnection,
    now: DateTime<Utc>,
    days: i64,
) -> sqlx::Result<Vec<(i64, Vec<AskingRow>)>> {
    let from: Option<String> = sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = ?")
        .bind(FROM_KEY)
        .fetch_optional(&mut *conn)
        .await?;
    let cutoff = (now - Duration::days(days))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();
    let rows: Vec<AskingRow> = sqlx::query_as(
        "SELECT q.id, q.cause, q.project_id, q.job_id, q.item_id, q.run_id FROM distill_queue q
          WHERE q.job_id IS NOT NULL AND q.cause IN (?1, ?2, ?3)
            AND substr(q.created_at, 1, 19) > ?4
            AND (?5 IS NULL OR substr(q.created_at, 1, 19) > ?5)
            AND NOT EXISTS (SELECT 1 FROM capture_requests c WHERE c.job_id = q.job_id)
          ORDER BY q.id",
    )
    .bind(ASKING_CAUSES[0].as_str())
    .bind(ASKING_CAUSES[1].as_str())
    .bind(ASKING_CAUSES[2].as_str())
    .bind(cutoff)
    .bind(from)
    .fetch_all(&mut *conn)
    .await?;
    let mut by_job: BTreeMap<i64, Vec<AskingRow>> = BTreeMap::new();
    for row in rows {
        by_job.entry(row.job_id).or_default().push(row);
    }
    let mut jobs: Vec<(i64, Vec<AskingRow>)> = by_job.into_iter().collect();
    jobs.sort_by_key(|(_, rows)| std::cmp::Reverse(rows.last().map_or(0, |row| row.id)));
    Ok(jobs)
}

pub fn render(day: &str, lines: &[String], more: usize, until: &str) -> String {
    let mut out = format!("Daily questions · {day} · {} to answer\n", lines.len() + more);
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    if more > 0 {
        out.push_str(&format!("and {more} more in the Brain\n"));
    }
    out.push_str(&format!(
        "Reply to this message (when only one is open) or send /cap <job> <answer>. Open until {until}."
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use chrono_tz::Europe::Lisbon;
    use sqlx::SqlitePool;

    async fn ask(pool: &SqlitePool, job: i64, created: &str) {
        sqlx::query(
            "INSERT OR IGNORE INTO jobs (id, project_id, project_root, status, max_items, created_at)
             VALUES (?, 'web', '/tmp/web', 'failed', 3, '2026-10-07T09:00:00+00:00')",
        )
        .bind(job)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO distill_queue (cause, project_id, job_id, status, attempts, created_at)
             VALUES ('job_failed', 'web', ?, 'pending', 0, ?)",
        )
        .bind(job)
        .bind(created)
        .execute(pool)
        .await
        .unwrap();
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    async fn digests(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM capture_digests")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    // 2026-10-07 is summer time in Lisbon (UTC+1): 17:30Z is 18:30 local.
    #[tokio::test]
    async fn no_digest_without_new_requests_before_the_hour_or_with_requests_off() {
        let pool = pool().await;
        assert_eq!(
            run_if_due_in(&pool, at("2026-10-07T17:30:00+00:00"), &Lisbon)
                .await
                .unwrap(),
            None
        );
        ask(&pool, 7, "2026-10-07T09:00:00+00:00").await;
        // 17:59 local is before the hour.
        assert_eq!(
            run_if_due_in(&pool, at("2026-10-07T16:59:00+00:00"), &Lisbon)
                .await
                .unwrap(),
            None
        );
        crate::capture::set_config(&pool, Some(0), None, at("2026-10-07T10:00:00+00:00"))
            .await
            .unwrap();
        assert_eq!(
            run_if_due_in(&pool, at("2026-10-07T17:30:00+00:00"), &Lisbon)
                .await
                .unwrap(),
            None
        );
        assert_eq!(digests(&pool).await, 0);
    }

    #[tokio::test]
    async fn one_digest_opens_one_request_per_job_and_one_feed_line() {
        let pool = pool().await;
        ask(&pool, 7, "2026-10-07T09:00:00+00:00").await;
        ask(&pool, 9, "2026-10-07T10:00:00+00:00").await;
        let id = run_if_due_in(&pool, at("2026-10-07T17:30:00+00:00"), &Lisbon)
            .await
            .unwrap()
            .unwrap();

        let (day, job_ids): (String, String) =
            sqlx::query_as("SELECT day, job_ids FROM capture_digests WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((day.as_str(), job_ids.as_str()), ("2026-10-07", "[9,7]"));
        let rows: Vec<(i64, String, String, Option<i64>)> = sqlx::query_as(
            "SELECT job_id, state, deadline, digest_id FROM capture_requests ORDER BY job_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (7, "open".into(), "2026-10-12T17:30:00+00:00".into(), Some(id)),
                (9, "open".into(), "2026-10-12T17:30:00+00:00".into(), Some(id)),
            ]
        );
        let feed: Vec<(String, Option<String>, Option<String>, String)> =
            sqlx::query_as("SELECT kind, project_id, subject, summary FROM feed")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(feed.len(), 1);
        assert_eq!(
            (feed[0].0.as_str(), feed[0].1.as_deref(), feed[0].2.as_deref()),
            ("capture_digest", None, Some(format!("capture_digest:{id}").as_str()))
        );
        assert!(
            feed[0].3.starts_with("Daily questions · 2026-10-07 · 2 to answer\n"),
            "{}",
            feed[0].3
        );
        assert!(feed[0].3.ends_with("Open until 2026-10-12."), "{}", feed[0].3);
    }

    #[tokio::test]
    async fn a_second_run_the_same_day_does_nothing_even_with_new_requests() {
        let pool = pool().await;
        ask(&pool, 7, "2026-10-07T09:00:00+00:00").await;
        assert!(
            run_if_due_in(&pool, at("2026-10-07T17:30:00+00:00"), &Lisbon)
                .await
                .unwrap()
                .is_some()
        );
        ask(&pool, 8, "2026-10-07T17:00:00+00:00").await;
        assert_eq!(
            run_if_due_in(&pool, at("2026-10-07T21:00:00+00:00"), &Lisbon)
                .await
                .unwrap(),
            None
        );
        assert_eq!(digests(&pool).await, 1);
    }

    /// The key is the local date, not 24 hours: a 23-hour day (spring forward, 2026-03-29) and a
    /// 25-hour day (fall back, 2026-10-25) each fire exactly once.
    #[tokio::test]
    async fn a_day_of_23_or_25_hours_fires_once() {
        let pool = pool().await;
        // Spring forward: 18:30 local on the 29th is 17:30Z; 23:30 local is 22:30Z, same date.
        ask(&pool, 1, "2026-03-28T12:00:00+00:00").await;
        assert!(
            run_if_due_in(&pool, at("2026-03-29T17:30:00+00:00"), &Lisbon)
                .await
                .unwrap()
                .is_some()
        );
        ask(&pool, 2, "2026-03-29T18:00:00+00:00").await;
        assert_eq!(
            run_if_due_in(&pool, at("2026-03-29T22:30:00+00:00"), &Lisbon)
                .await
                .unwrap(),
            None
        );
        // The next date fires 24h later.
        assert!(
            run_if_due_in(&pool, at("2026-03-30T17:30:00+00:00"), &Lisbon)
                .await
                .unwrap()
                .is_some()
        );
        // Fall back: 17:30Z on the 25th is 17:30 local (before the hour), 18:30Z is 18:30 local.
        ask(&pool, 3, "2026-10-24T12:00:00+00:00").await;
        assert_eq!(
            run_if_due_in(&pool, at("2026-10-25T17:30:00+00:00"), &Lisbon)
                .await
                .unwrap(),
            None
        );
        assert!(
            run_if_due_in(&pool, at("2026-10-25T18:30:00+00:00"), &Lisbon)
                .await
                .unwrap()
                .is_some()
        );
        ask(&pool, 4, "2026-10-25T19:00:00+00:00").await;
        assert_eq!(
            run_if_due_in(&pool, at("2026-10-25T19:30:00+00:00"), &Lisbon)
                .await
                .unwrap(),
            None
        );
        assert_eq!(digests(&pool).await, 3);
    }

    #[tokio::test]
    async fn the_window_the_state_the_marker_and_existing_requests_choose_the_jobs() {
        let pool = pool().await;
        let now = at("2026-10-07T17:30:00+00:00");
        ask(&pool, 1, "2026-10-01T09:00:00+00:00").await; // older than 5 days: out
        // in, whatever its queue state (after the marker, which is written below but read at run time)
        ask(&pool, 2, "2026-10-06T14:00:00+00:00").await;
        sqlx::query("UPDATE distill_queue SET status = 'done' WHERE job_id = 2")
            .execute(&pool)
            .await
            .unwrap();
        ask(&pool, 3, "2026-10-06T09:00:00+00:00").await; // already has a request: out
        sqlx::query(
            "INSERT INTO capture_requests (job_id, project_id, causes, prompt_text, state, deadline, created_at)
             VALUES (3, 'web', '[]', 'q', 'expired', 'd', 't')",
        )
        .execute(&pool)
        .await
        .unwrap();
        ask(&pool, 4, "2026-10-06T09:00:00").await; // before the marker: out
        sqlx::query(
            "INSERT INTO schema_meta (key, value) VALUES ('distiller.capture_from', '2026-10-06T12:00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        ask(&pool, 5, "2026-10-06T13:00:00+00:00").await; // after the marker: in
        let id = run_if_due_in(&pool, now, &Lisbon).await.unwrap().unwrap();
        let ids: String = sqlx::query_scalar("SELECT job_ids FROM capture_digests WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(ids, "[5,2]");
    }

    #[tokio::test]
    async fn the_text_lists_the_eight_most_recent_and_counts_the_rest() {
        let pool = pool().await;
        for job in 1..=10 {
            ask(&pool, job, "2026-10-07T09:00:00+00:00").await;
        }
        let id = run_if_due_in(&pool, at("2026-10-07T17:30:00+00:00"), &Lisbon)
            .await
            .unwrap()
            .unwrap();
        let text: String = sqlx::query_scalar("SELECT summary FROM feed WHERE subject = ?")
            .bind(format!("capture_digest:{id}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(text.contains("· 10 to answer"), "{text}");
        assert!(text.contains("#10 · web") && text.contains("#3 · web"), "{text}");
        assert!(
            !text.contains("#2 · web") && text.contains("and 2 more in the Brain"),
            "{text}"
        );
        let open: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM capture_requests WHERE digest_id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(open, 10, "the requests beyond the text stay answerable");
    }

    /// Seed job 7 with one exhausted item (`gate_output` as given) and its `run_exhausted` queue row.
    async fn exhausted(pool: &SqlitePool, description: &str, output: Option<&str>) {
        sqlx::query(
            "INSERT INTO jobs (id, project_id, project_root, status, max_items, created_at)
             VALUES (7, 'web', '/tmp/web', 'failed', 3, '2026-10-07T09:00:00+00:00')",
        )
        .execute(pool)
        .await
        .unwrap();
        let item: i64 = sqlx::query_scalar(
            "INSERT INTO job_nodes (job_id, node_id, ordinal, description, status, gate_attempts, gate_output)
             VALUES (7, 'implement', 2, ?, 'gate_failed', 3, ?) RETURNING id",
        )
        .bind(description)
        .bind(output)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO distill_queue (cause, project_id, job_id, item_id, status, attempts, created_at)
             VALUES ('run_exhausted', 'web', 7, ?, 'pending', 0, '2026-10-07T09:00:00+00:00')",
        )
        .bind(item)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_secret_in_a_fact_never_reaches_the_digest() {
        const SECRET: &str = "rk_live_51H8abcdefghijklmnop";
        let pool = pool().await;
        exhausted(&pool, &format!("use {SECRET}"), None).await;
        let id = run_if_due_in(&pool, at("2026-10-07T17:30:00+00:00"), &Lisbon)
            .await
            .unwrap()
            .unwrap();
        let summary: String = sqlx::query_scalar("SELECT summary FROM feed WHERE subject = ?")
            .bind(format!("capture_digest:{id}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        let (causes, text): (String, String) =
            sqlx::query_as("SELECT causes, prompt_text FROM capture_requests WHERE job_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(!summary.contains(SECRET), "{summary}");
        assert!(!text.contains(SECRET), "{text}");
        assert!(
            !causes.contains(SECRET),
            "the stored facts are redacted too: {causes}"
        );
    }

    #[tokio::test]
    async fn an_exhausted_item_names_its_signature_never_the_raw_output() {
        let pool = pool().await;
        exhausted(
            &pool,
            "build the thing",
            Some("error[E0425]: cannot find value `x` in this scope\n --> src/a.rs:1:1\nlots of raw lines"),
        )
        .await;
        run_if_due_in(&pool, at("2026-10-07T17:30:00+00:00"), &Lisbon)
            .await
            .unwrap()
            .unwrap();
        let text: String =
            sqlx::query_scalar("SELECT prompt_text FROM capture_requests WHERE job_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            text.contains("item 2 «build the thing» used up 3 gate attempts: `"),
            "{text}"
        );
        assert!(!text.contains("lots of raw lines"), "{text}");
    }

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    #[tokio::test]
    async fn one_digest_per_local_day_and_requests_carry_the_digest_id() {
        let pool = pool().await;
        for (day, ids) in [("2026-10-07", "[7]"), ("2026-10-08", "[8]")] {
            sqlx::query("INSERT INTO capture_digests (day, job_ids, created_at) VALUES (?, ?, 't')")
                .bind(day)
                .bind(ids)
                .execute(&pool)
                .await
                .unwrap();
        }
        let again = sqlx::query(
            "INSERT INTO capture_digests (day, job_ids, created_at) VALUES ('2026-10-07', '[9]', 't')",
        )
        .execute(&pool)
        .await;
        assert!(again.is_err(), "a second digest for the same day must be refused");

        sqlx::query(
            "INSERT INTO capture_requests (job_id, project_id, causes, prompt_text, state, deadline, created_at)
             VALUES (7, 'p', '[]', 'q', 'open', 'd', 't')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let digest_id: Option<i64> =
            sqlx::query_scalar("SELECT digest_id FROM capture_requests WHERE job_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(digest_id, None, "a request opened without a digest has none");
    }

    #[test]
    fn the_digest_text_matches_the_template() {
        let lines = vec![
            "#7 · api · job #7 — the job ended in `failed`. What went wrong that the logs would not show?".to_owned(),
            "#9 · api · job #9 — a. b?".to_owned(),
        ];
        assert_eq!(
            render("2026-10-22", &lines, 0, "2026-10-27"),
            "Daily questions · 2026-10-22 · 2 to answer\n\
             #7 · api · job #7 — the job ended in `failed`. What went wrong that the logs would not show?\n\
             #9 · api · job #9 — a. b?\n\
             Reply to this message (when only one is open) or send /cap <job> <answer>. Open until 2026-10-27."
        );
        let text = render("2026-10-22", &lines, 3, "2026-10-27");
        assert!(text.starts_with("Daily questions · 2026-10-22 · 5 to answer\n"), "{text}");
        assert!(text.contains("\nand 3 more in the Brain\n"), "{text}");
    }
}
