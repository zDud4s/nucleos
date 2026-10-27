//! The `calendar_week` cause: one row per ISO week, read from the owner's calendar and written as
//! week-scoped observations (spec 2026-10-22 section 9, Phase 7).
//!
//! The dossier holds the owner's own event titles, hours and durations (only `human` events, never
//! an agent's proposal) and, at most, the names of people the owner both titled an event with and
//! has written to. It holds no notes and no theme: this file reads the calendar and the contact
//! roster and nothing else of the owner's.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, Utc};
use sqlx::SqlitePool;

use super::{Cause, QueueRow, STATUS_DONE, STATUS_RUNNING, ask, fail};
use crate::integrations::calendar::{self, EventOccurrence};
use crate::map::intent::Extractor;

/// How far back and forward the window reads, in days.
const WINDOW_BACK_DAYS: i64 = 28;
const WINDOW_FORWARD_DAYS: i64 = 14;
/// The most observations one row may write.
const MAX_OBS_PER_ROW: usize = 8;

/// One line per `human` occurrence: `Mon 07:00, 60 min, weekly: Gym`. The day and hour come from
/// the local time the event is lived in, never from the UTC instant. `freq` maps an event id to its
/// recurrence word.
pub(super) fn week_lines(
    occurrences: &[EventOccurrence],
    freq: &HashMap<i64, String>,
) -> Vec<String> {
    occurrences
        .iter()
        .filter(|o| o.source == "human")
        .map(|o| {
            let local = NaiveDateTime::parse_from_str(&o.occurrence_local, calendar::LOCAL_FORMAT);
            let when = local
                .map(|l| l.format("%a %H:%M").to_string())
                .unwrap_or_else(|_| "?".into());
            let minutes = (o.ends_at - o.starts_at).num_minutes();
            let rhythm = freq
                .get(&o.event_id)
                .map(|f| format!(", {f}"))
                .unwrap_or_default();
            format!("{when}, {minutes} min{rhythm}: {}", o.title)
        })
        .collect()
}

/// The queue's `item_id` of an ISO week: year * 100 + week.
pub(super) fn week_item_id(day: NaiveDate) -> i64 {
    let w = day.iso_week();
    i64::from(w.year()) * 100 + i64::from(w.week())
}

/// The observation scope of the ISO week `day` falls in: `week:2026-W43`.
pub(super) fn week_scope_id(day: NaiveDate) -> String {
    let w = day.iso_week();
    format!("week:{}-W{:02}", w.year(), w.week())
}

/// The scope id of a queued week, from its `item_id` (so a retry in a later week still labels the
/// week it was queued for).
fn week_scope_of_item(item_id: i64) -> String {
    format!("week:{}-W{:02}", item_id / 100, item_id % 100)
}

/// `true` when `name` occurs in `title` as a whole word, ignoring case. Word characters are
/// `char::is_alphanumeric`; the name is never turned into a pattern.
fn names_word(title: &str, name: &str) -> bool {
    let title = title.to_lowercase();
    let name = name.trim().to_lowercase();
    if name.is_empty() {
        return false;
    }
    title.match_indices(&name).any(|(start, _)| {
        let before = title[..start].chars().next_back();
        let after = title[start + name.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// The names of correspondents the owner has written to and also named in an event title. Names
/// only, alphabetical: never an address, a count or a line of mail.
pub(super) async fn contact_marks(
    pool: &SqlitePool,
    titles: &[String],
) -> sqlx::Result<Vec<String>> {
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT display_name FROM contact_addresses
          WHERE outbound_ever = 1 AND display_name IS NOT NULL AND TRIM(display_name) <> ''
          ORDER BY display_name",
    )
    .fetch_all(pool)
    .await?;
    Ok(names
        .into_iter()
        .filter(|name| titles.iter().any(|t| names_word(t, name)))
        .collect())
}

/// The one contacts line of the dossier: the fixed phrase and the names.
pub(super) fn contact_line(marks: &[String]) -> String {
    format!("You write to: {}", marks.join(", "))
}

/// The dossier of a week: calendar lines and the contact mark, each scrubbed. No notes block.
pub(super) fn week_dossier(lines: &[String], marks: &[String]) -> String {
    let mut out = format!("## Calendar\n{}", super::scrub(&lines.join("\n")));
    if !marks.is_empty() {
        out.push_str(&format!(
            "\n\n## Contacts\n{}",
            super::scrub(&contact_line(marks))
        ));
    }
    out
}

/// What a week's dossier is built from, and the titles the citation guard checks against.
pub(super) struct WeekInputs {
    pub lines: Vec<String>,
    pub marks: Vec<String>,
    pub titles: Vec<String>,
}

/// Read the window `[now - 28 d, now + 14 d)`. `None` when it holds no `human` occurrence.
pub(super) async fn gather_week(
    pool: &SqlitePool,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<WeekInputs>> {
    let found = calendar::occurrences(
        pool,
        now - chrono::Duration::days(WINDOW_BACK_DAYS),
        now + chrono::Duration::days(WINDOW_FORWARD_DAYS),
    )
    .await?;
    let human: Vec<EventOccurrence> = found.into_iter().filter(|o| o.source == "human").collect();
    if human.is_empty() {
        return Ok(None);
    }
    let ids: Vec<i64> = human.iter().map(|o| o.event_id).collect();
    let freq = calendar::recurrence_of(pool, &ids).await?;
    let titles: Vec<String> = human.iter().map(|o| o.title.clone()).collect();
    let marks = contact_marks(pool, &titles).await?;
    Ok(Some(WeekInputs {
        lines: week_lines(&human, &freq),
        marks,
        titles,
    }))
}

/// Queue this ISO week's row if the source is on and the week has none yet. `true` when a row was
/// added; the `distill_queue_once` index turns the second attempt into a no-op.
pub(super) async fn enqueue_week_if_due(
    pool: &SqlitePool,
    now: DateTime<Local>,
) -> sqlx::Result<bool> {
    if !crate::distill::sources::enabled(pool, crate::distill::sources::CALENDAR_KEY).await {
        return Ok(false);
    }
    let queued = sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
             (cause, project_id, job_id, item_id, run_id, status, attempts, created_at)
         VALUES (?, '', NULL, ?, NULL, 'pending', 0, ?)",
    )
    .bind(Cause::CalendarWeek.as_str())
    .bind(week_item_id(now.date_naive()))
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(queued.rows_affected() > 0)
}

/// Close a `calendar_week` row as done, with a category in `error` when it did nothing.
async fn close(pool: &SqlitePool, row: &QueueRow, error: Option<&str>, now: DateTime<Utc>) {
    let written = sqlx::query(
        "UPDATE distill_queue SET status = ?, finished_at = ?, error = ?
          WHERE id = ? AND status = ?",
    )
    .bind(STATUS_DONE)
    .bind(now.to_rfc3339())
    .bind(error)
    .bind(row.id)
    .bind(STATUS_RUNNING)
    .execute(pool)
    .await;
    if let Err(e) = written {
        tracing::warn!(
            "distillation: could not close calendar queue row {}: {e}",
            row.id
        );
    }
}

/// Write the episodic items as `observed` rows of the week and close the queue row, in one
/// transaction. Returns `(written, dropped)`: a `semantic` or `procedural` item is dropped, since a
/// week alone proves no lesson.
async fn write_observations(
    pool: &SqlitePool,
    row: &QueueRow,
    scope_id: &str,
    items: &[super::Item],
    titles: &[String],
    now: DateTime<Utc>,
) -> sqlx::Result<(usize, usize)> {
    let sources: Vec<&str> = titles.iter().map(String::as_str).collect();
    let evidence = serde_json::json!([{
        "t": "week",
        "id": scope_id.strip_prefix("week:").unwrap_or(scope_id),
    }])
    .to_string();
    let mut tx = pool.begin().await?;
    let (mut written, mut dropped) = (0usize, 0usize);
    for item in items {
        if item.layer != crate::knowledge::Layer::Episodic {
            dropped += 1;
            continue;
        }
        if written >= MAX_OBS_PER_ROW {
            dropped += 1;
            continue;
        }
        let Some(fingerprint) = crate::knowledge::title_fingerprint(&item.title) else {
            dropped += 1;
            continue;
        };
        let already: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge
              WHERE scope_kind = 'observation' AND scope_id = ? AND fingerprint = ?",
        )
        .bind(scope_id)
        .bind(&fingerprint)
        .fetch_one(&mut *tx)
        .await?;
        if already > 0 {
            continue;
        }
        let provenance = crate::knowledge::Provenance {
            source: Some("distiller"),
            distill_cause: Some(Cause::CalendarWeek.as_str()),
            evidence: Some(&evidence),
            fingerprint: Some(&fingerprint),
            layer: Some(crate::knowledge::Layer::Episodic),
            about: None,
            ..Default::default()
        };
        let id = crate::knowledge::record_observation_in(
            &mut tx,
            scope_id,
            crate::knowledge::Kind::Memory,
            &item.title,
            &item.body,
            &provenance,
        )
        .await?;
        written += 1;
        if crate::governance::quote_guard::quotes(
            &format!("{}\n{}", item.title, item.body),
            &sources,
        ) {
            crate::knowledge::note_quoted_owner_text_in(&mut tx, id).await?;
        }
    }
    sqlx::query(
        "UPDATE distill_queue SET status = ?, finished_at = ?, error = NULL
          WHERE id = ? AND status = ?",
    )
    .bind(STATUS_DONE)
    .bind(now.to_rfc3339())
    .bind(row.id)
    .bind(STATUS_RUNNING)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((written, dropped))
}

/// Distil one claimed `calendar_week` row. A source that is off, or a week without a `human`
/// event, closes the row `done` without asking the model.
pub(super) async fn process(
    pool: &SqlitePool,
    asked: Extractor<'_>,
    row: QueueRow,
    now: DateTime<Utc>,
) {
    if !crate::distill::sources::enabled(pool, crate::distill::sources::CALENDAR_KEY).await {
        close(pool, &row, Some("disabled"), now).await;
        return;
    }
    let inputs = match gather_week(pool, now).await {
        Ok(Some(inputs)) => inputs,
        Ok(None) => {
            close(pool, &row, None, now).await;
            return;
        }
        Err(_) => {
            fail(pool, &row, "db", "reading the calendar failed", now).await;
            return;
        }
    };
    let dossier = week_dossier(&inputs.lines, &inputs.marks);
    let answer = match ask(
        asked,
        super::extraction_prompt(Cause::CalendarWeek, &dossier),
        &super::standing_for(Cause::CalendarWeek),
        super::items_format(),
    )
    .await
    {
        Ok(answer) => answer,
        Err(error) => {
            fail(pool, &row, "model", &format!("{:?}", error.kind()), now).await;
            return;
        }
    };
    let Ok(items) = super::parse_items(&answer) else {
        fail(pool, &row, "parse", "the answer is not a JSON array", now).await;
        return;
    };
    let scope_id = match row.item_id {
        Some(item_id) => week_scope_of_item(item_id),
        None => week_scope_id(now.with_timezone(&Local).date_naive()),
    };
    match write_observations(pool, &row, &scope_id, &items, &inputs.titles, now).await {
        Ok((written, dropped)) => tracing::info!(
            "distillation: queue row {} (calendar_week): parsed {}, written {written}, dropped {dropped}",
            row.id,
            items.len()
        ),
        Err(_) => fail(pool, &row, "db", "writing the observations failed", now).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn occ(
        title: &str,
        source: &str,
        local: &str,
        start: &str,
        minutes: i64,
        event_id: i64,
    ) -> EventOccurrence {
        let starts_at = start.parse::<DateTime<Utc>>().unwrap();
        EventOccurrence {
            event_id,
            title: title.into(),
            source: source.into(),
            occurrence_local: local.into(),
            starts_at,
            ends_at: starts_at + chrono::Duration::minutes(minutes),
        }
    }

    #[test]
    fn human_events_count_and_agent_proposals_do_not() {
        let rows = week_lines(
            &[
                occ(
                    "Gym",
                    "human",
                    "2026-10-20T07:00:00",
                    "2026-10-20T06:00:00Z",
                    60,
                    1,
                ),
                occ(
                    "Agent slot",
                    "proposal",
                    "2026-10-20T10:00:00",
                    "2026-10-20T09:00:00Z",
                    30,
                    2,
                ),
            ],
            &HashMap::new(),
        );
        assert_eq!(rows.len(), 1);
        assert!(rows[0].contains("Gym"));
        assert!(!rows.iter().any(|r| r.contains("Agent slot")));
    }

    #[test]
    fn day_and_hour_come_from_the_local_time_not_from_utc() {
        // 00:30 local on Monday 2026-10-26 is 23:30Z on Sunday 2026-10-25 (UTC+1).
        let rows = week_lines(
            &[occ(
                "Night shift",
                "human",
                "2026-10-26T00:30:00",
                "2026-10-25T23:30:00Z",
                90,
                1,
            )],
            &HashMap::new(),
        );
        assert!(rows[0].starts_with("Mon 00:30"), "got {:?}", rows[0]);
        assert!(rows[0].contains("90 min"));
    }

    #[test]
    fn a_recurring_event_is_marked_weekly_from_calendar_events() {
        let mut freq = HashMap::new();
        freq.insert(1_i64, "weekly".to_owned());
        let rows = week_lines(
            &[occ(
                "Gym",
                "human",
                "2026-10-20T07:00:00",
                "2026-10-20T06:00:00Z",
                60,
                1,
            )],
            &freq,
        );
        assert!(rows[0].contains("weekly"));
    }

    #[test]
    fn the_week_key_packs_the_iso_year_and_week() {
        assert_eq!(
            week_item_id(NaiveDate::from_ymd_opt(2026, 10, 21).unwrap()),
            202643
        );
        assert_eq!(
            week_scope_id(NaiveDate::from_ymd_opt(2026, 10, 21).unwrap()),
            "week:2026-W43"
        );
        // 2027-01-01 belongs to ISO week 53 of 2026.
        assert_eq!(
            week_item_id(NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()),
            202653
        );
        assert_eq!(week_scope_of_item(202643), "week:2026-W43");
    }

    async fn add_contact(pool: &SqlitePool, name: &str, outbound: bool) {
        let contact_id = sqlx::query(
            "INSERT INTO contacts (display_name, created_at) VALUES (?, '2026-10-01T00:00:00Z')",
        )
        .bind(name)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO contact_addresses
                 (address, contact_id, first_seen, last_seen, outbound_ever, display_name)
             VALUES (?, ?, '2026-10-01T00:00:00Z', '2026-10-01T00:00:00Z', ?, ?)",
        )
        .bind(format!(
            "{}@example.test",
            name.to_lowercase().replace(' ', ".")
        ))
        .bind(contact_id)
        .bind(i64::from(outbound))
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_contact_is_marked_only_when_the_owner_titled_an_event_with_the_name_and_wrote_to_them()
     {
        let pool = crate::testdb::fresh_pool().await;
        add_contact(&pool, "Ana Silva", true).await;
        add_contact(&pool, "Bruno", false).await;
        add_contact(&pool, "Carla", true).await;
        let marks = contact_marks(
            &pool,
            &[
                "Lunch with Ana Silva".to_owned(),
                "Call Bruno".to_owned(),
                "Tanaka review".to_owned(),
            ],
        )
        .await
        .unwrap();
        assert_eq!(marks, vec!["Ana Silva".to_owned()]);
    }

    #[tokio::test]
    async fn the_name_must_be_a_whole_word_and_nothing_but_the_name_leaves() {
        let pool = crate::testdb::fresh_pool().await;
        add_contact(&pool, "Ana", true).await;
        assert!(
            contact_marks(&pool, &["Banana split".to_owned()])
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            contact_marks(&pool, &["ana's birthday".to_owned()])
                .await
                .unwrap(),
            vec!["Ana".to_owned()]
        );
        // The dossier line has the name and the fixed phrase only: no address, count or mail text.
        assert_eq!(contact_line(&["Ana".to_owned()]), "You write to: Ana");
    }

    #[tokio::test]
    async fn the_calendar_source_is_on_when_the_key_is_absent_and_off_only_for_a_stored_zero() {
        let pool = crate::testdb::fresh_pool().await;
        use crate::distill::sources::{CALENDAR_KEY, enabled, set};
        assert!(enabled(&pool, CALENDAR_KEY).await);
        set(&pool, CALENDAR_KEY, false).await.unwrap();
        assert!(!enabled(&pool, CALENDAR_KEY).await);
        set(&pool, CALENDAR_KEY, true).await.unwrap();
        assert!(enabled(&pool, CALENDAR_KEY).await);
    }

    async fn queue_rows(pool: &SqlitePool) -> Vec<(String, i64, String)> {
        sqlx::query_as("SELECT cause, item_id, project_id FROM distill_queue")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_first_tick_of_a_week_queues_one_calendar_week_row_and_the_second_does_not() {
        let pool = crate::testdb::fresh_pool().await;
        let wed = Local.with_ymd_and_hms(2026, 10, 21, 9, 0, 0).unwrap();
        assert!(enqueue_week_if_due(&pool, wed).await.unwrap());
        assert!(
            !enqueue_week_if_due(&pool, wed + chrono::Duration::hours(3))
                .await
                .unwrap()
        );
        assert_eq!(
            queue_rows(&pool).await,
            vec![("calendar_week".to_owned(), 202643, String::new())]
        );
        let next = wed + chrono::Duration::days(7);
        assert!(enqueue_week_if_due(&pool, next).await.unwrap());
    }

    fn fake_answering(stdout: &str) -> crate::execution::runner::FakeCommandRunner {
        crate::execution::runner::FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(crate::execution::runner::RunOutcome {
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
        Utc.with_ymd_and_hms(2026, 10, 21, 12, 0, 0).unwrap()
    }

    /// A running `calendar_week` row for ISO week 43 of 2026, as `claim_next` would hand it over.
    async fn running_week_row(pool: &SqlitePool) -> QueueRow {
        let id = sqlx::query(
            "INSERT INTO distill_queue
                 (cause, project_id, job_id, item_id, run_id, status, attempts, created_at)
             VALUES ('calendar_week', '', NULL, 202643, NULL, 'running', 0, '2026-10-21T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        QueueRow {
            id,
            cause: "calendar_week".into(),
            project_id: String::new(),
            job_id: None,
            item_id: Some(202643),
            run_id: None,
            chat_id: None,
            attempts: 0,
        }
    }

    async fn queue_state(pool: &SqlitePool, id: i64) -> (String, Option<String>) {
        sqlx::query_as("SELECT status, error FROM distill_queue WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn event(pool: &SqlitePool, title: &str, source: &str, start: DateTime<Utc>) {
        calendar::insert_event(
            pool,
            title,
            start.naive_utc(),
            60,
            chrono_tz::UTC,
            source,
            None,
            None,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn the_window_is_minus_28_to_plus_14_days_human_only_and_an_empty_week_closes_done_without_the_model()
     {
        let pool = crate::testdb::fresh_pool().await;
        let day = chrono::Duration::days;
        event(&pool, "Too old", "human", noon() - day(30)).await;
        event(&pool, "Past", "human", noon() - day(20)).await;
        event(&pool, "Soon", "human", noon() + day(10)).await;
        event(&pool, "Too far", "human", noon() + day(20)).await;
        event(&pool, "Agent slot", "proposal", noon() - day(5)).await;
        let inputs = gather_week(&pool, noon())
            .await
            .unwrap()
            .expect("two events");
        assert_eq!(inputs.titles, vec!["Past".to_owned(), "Soon".to_owned()]);

        // A fresh database has no human event: done, empty error, the model never asked.
        let empty = crate::testdb::fresh_pool().await;
        let row = running_week_row(&empty).await;
        let id = row.id;
        let runner = fake_answering("[]");
        super::super::process_one_with(&empty, Extractor::Cli(&runner), None, row, noon()).await;
        assert_eq!(queue_state(&empty, id).await, ("done".to_owned(), None));
        assert!(
            runner.last_prompt.lock().unwrap().is_none(),
            "the model was asked"
        );
    }

    #[tokio::test]
    async fn a_disabled_source_queues_nothing_and_a_queued_row_closes_done_disabled() {
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query(
            "INSERT INTO schema_meta (key, value) VALUES ('distiller.calendar_sources', '0')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let wed = Local.with_ymd_and_hms(2026, 10, 21, 9, 0, 0).unwrap();
        assert!(!enqueue_week_if_due(&pool, wed).await.unwrap());
        assert!(queue_rows(&pool).await.is_empty());

        event(&pool, "Gym", "human", noon()).await;
        let row = running_week_row(&pool).await;
        let id = row.id;
        let runner = fake_answering("[]");
        super::super::process_one_with(&pool, Extractor::Cli(&runner), None, row, noon()).await;
        assert_eq!(
            queue_state(&pool, id).await,
            ("done".to_owned(), Some("disabled".to_owned()))
        );
        assert!(
            runner.last_prompt.lock().unwrap().is_none(),
            "the model was asked"
        );
    }

    #[tokio::test]
    async fn the_output_becomes_observations_scoped_to_the_week_and_never_active_knowledge() {
        let pool = crate::testdb::fresh_pool().await;
        event(&pool, "Gym", "human", noon()).await;
        let row = running_week_row(&pool).await;
        let id = row.id;
        let runner = fake_answering(
            r#"[
              {"layer":"episodic","title":"Gym on Wednesday mornings","body":"The owner trains weekly."},
              {"layer":"episodic","title":"Short sessions","body":"Sessions last an hour."},
              {"layer":"semantic","title":"The owner is sporty","body":"A lesson one week cannot prove."}
            ]"#,
        );
        super::super::process_one_with(&pool, Extractor::Cli(&runner), None, row, noon()).await;
        assert_eq!(queue_state(&pool, id).await, ("done".to_owned(), None));
        let rows: Vec<(String, String, String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT scope_kind, scope_id, status, about, distill_cause FROM knowledge",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2, "the semantic item is dropped: {rows:?}");
        for (kind, scope, status, about, cause) in rows {
            assert_eq!(
                (kind.as_str(), scope.as_str(), status.as_str()),
                ("observation", "week:2026-W43", "observed")
            );
            assert_eq!((about, cause.as_deref()), (None, Some("calendar_week")));
        }
    }

    #[test]
    fn the_dossier_has_titles_and_the_contact_mark_but_no_notes_block() {
        let d = week_dossier(
            &["Mon 07:00, 60 min, weekly: Gym".to_owned()],
            &["Ana".to_owned()],
        );
        assert!(d.contains("Gym") && d.contains("You write to: Ana"));
        assert!(!d.contains("owner context"));
    }

    #[test]
    fn week_rs_names_neither_the_notes_nor_the_theme_tables() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/distill/week.rs"),
        )
        .unwrap();
        let production = crate::source_scan::production_code(&src);
        assert!(!production.contains(&["owner", "note"].join("_")));
        assert!(!production.contains("theme_members"));
    }
}
