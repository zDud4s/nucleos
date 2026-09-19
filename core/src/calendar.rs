//! The calendar pillar's domain: the events themselves, and the one question they answer.
//!
//! Owns every statement touching `calendar_events` and `calendar_exceptions`, the `/calendar/*`
//! handlers, and two policy questions — [`busy_at`] ("may this person be interrupted right now")
//! and [`next_free_slot`] ("where could an hour of work go"). Expanding a recurrence rule is not
//! here: that is pure, and lives in `recurrence.rs`.
//!
//! **This is not a brake.** `budget.rs`, `wip.rs` and `attention.rs` fail closed, because a brake
//! that fails open is not a brake. The calendar governs a *notification*, and the failure modes are
//! not symmetric: an unwanted ping costs an interruption, a missing one costs a message that never
//! arrived and whose absence nobody notices. So every read here fails OPEN — an unreadable calendar
//! means "not busy", and the notification goes out. See [`busy_at`].
//!
//! It also never touches a triage class. "I am in meetings until 18h" does not make a message less
//! important, it makes me unreachable, and `priority.rs` deliberately has no idea this module
//! exists.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::{
    DateTime, Datelike, Duration, NaiveDateTime, NaiveTime, TimeZone, Timelike, Utc, Weekday,
};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::recurrence::{self, Exception, Freq, Rule};
use crate::state::AppState;

/// How local wall-clock timestamps are stored and accepted. No offset, by design — the zone
/// travels in its own column so that a recurring time keeps its wall clock across a DST change.
pub const LOCAL_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

/// How far ahead [`next_free_slot`] will look before admitting there is no room.
///
/// A bound rather than a search to the end of time: if a fortnight of working hours has no gap for
/// one task, the honest answer is "nowhere soon", not a slot in November.
const SLOT_SEARCH_DAYS: i64 = 14;

/// The granularity proposed slots snap to. Nobody wants a meeting proposed for 14:07.
const SLOT_ALIGNMENT_MINUTES: i64 = 15;

/// When the agent is allowed to propose work, which is NOT the same as when you are busy.
///
/// Busy comes only from real events. This window exists because `next_free_slot` without bounds
/// proposes 03:14 — it says where a proposal may land, and has no say in whether a notification
/// goes out.
#[derive(Debug, Clone)]
pub struct WorkingHours {
    pub start: NaiveTime,
    pub end: NaiveTime,
    pub weekdays: Vec<Weekday>,
}

impl Default for WorkingHours {
    fn default() -> Self {
        Self {
            start: NaiveTime::from_hms_opt(9, 0, 0).expect("09:00 is a valid time"),
            end: NaiveTime::from_hms_opt(18, 0, 0).expect("18:00 is a valid time"),
            weekdays: vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
        }
    }
}

/// The calendar's process-wide settings, resolved once at startup.
///
/// Same shape and same reasoning as `EmailRuntime` and `VoiceRuntime`: read together, changed
/// together, and a `Default` that every test which does not care about calendars can ignore.
#[derive(Debug, Clone)]
pub struct CalendarRuntime {
    /// The zone an event is assumed to be in when the caller does not say.
    pub default_tz: Tz,
    pub working_hours: WorkingHours,
    /// Whether an `action`-class message may file a proposal asking for time.
    ///
    /// **Off by default**, like every other part of this system that acts rather than reports —
    /// the email pillar, the voice pillar and the budget all ship dark for the same reason. The
    /// calendar itself is inert and useful the moment it exists; this switch is the one part that
    /// generates work for a person to review, and a backlog nobody asked for is exactly what
    /// `wip.rs` exists to stop. Turn it on with `propose_time_for_actions: true`.
    pub propose_for_actions: bool,
}

impl Default for CalendarRuntime {
    fn default() -> Self {
        Self {
            default_tz: chrono_tz::UTC,
            working_hours: WorkingHours::default(),
            propose_for_actions: false,
        }
    }
}

/// The zone this machine is set to, or UTC when it cannot be established.
///
/// Asked of the OS rather than defaulted to a name, because a wrong default here is invisible: the
/// times still render, they are just all wrong by a fixed offset, which reads as "the calendar is
/// broken" rather than "the zone is unset".
fn system_tz() -> Tz {
    match iana_time_zone::get_timezone() {
        Ok(name) => name.parse().unwrap_or_else(|_| {
            tracing::warn!(
                name,
                "calendar: the OS reported a zone chrono-tz does not know; using UTC"
            );
            chrono_tz::UTC
        }),
        Err(error) => {
            tracing::warn!(%error, "calendar: could not read the system time zone; using UTC");
            chrono_tz::UTC
        }
    }
}

fn parse_weekday(raw: &str) -> Option<Weekday> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "mon" => Some(Weekday::Mon),
        "tue" => Some(Weekday::Tue),
        "wed" => Some(Weekday::Wed),
        "thu" => Some(Weekday::Thu),
        "fri" => Some(Weekday::Fri),
        "sat" => Some(Weekday::Sat),
        "sun" => Some(Weekday::Sun),
        _ => None,
    }
}

impl CalendarRuntime {
    pub fn from_config(config: &crate::config::CalendarConfig) -> Self {
        let default_tz = if config.default_tz.trim().is_empty() {
            system_tz()
        } else {
            config.default_tz.parse::<Tz>().unwrap_or_else(|_| {
                tracing::warn!(
                    configured = config.default_tz,
                    "calendar: unknown default_tz; falling back to the system zone"
                );
                system_tz()
            })
        };

        let defaults = WorkingHours::default();
        let start = NaiveTime::parse_from_str(&config.working_hours_start, "%H:%M")
            .unwrap_or(defaults.start);
        let end =
            NaiveTime::parse_from_str(&config.working_hours_end, "%H:%M").unwrap_or(defaults.end);
        let weekdays: Vec<Weekday> = config
            .working_weekdays
            .iter()
            .filter_map(|day| parse_weekday(day))
            .collect();

        Self {
            default_tz,
            propose_for_actions: config.propose_time_for_actions,
            working_hours: WorkingHours {
                // A window that ends before it starts would make every slot search fail silently.
                // Reverting to the default is louder than proposing nothing forever.
                start: if start < end { start } else { defaults.start },
                end: if start < end { end } else { defaults.end },
                // An empty list is NOT read as "every day". It is read as a mistake, because the
                // only thing it could otherwise mean is "never propose anything", and a config
                // that silently disables a feature is the failure `notify_classes` was hardened
                // against. An explicit off switch would be its own key.
                weekdays: if weekdays.is_empty() {
                    defaults.weekdays
                } else {
                    weekdays
                },
            },
        }
    }
}

/// One occurrence, joined back to the event that produced it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EventOccurrence {
    pub event_id: i64,
    pub title: String,
    /// `human` or `proposal` — what the agent scheduled must stay distinguishable from what you did.
    pub source: String,
    /// The occurrence's original local start, which is the handle used to cancel or move it.
    pub occurrence_local: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

/// A stored row, before expansion.
struct StoredEvent {
    id: i64,
    title: String,
    source: String,
    rule: Rule,
}

/// The `calendar_events` row exactly as SQLite hands it over, before anything is trusted.
///
/// Every recurrence column arrives as `Option<String>` because that is what the schema holds; they
/// are turned into a [`Rule`] only after each one has been checked, so a bad row can be dropped
/// rather than panicking the read.
#[derive(sqlx::FromRow)]
struct EventRow {
    id: i64,
    title: String,
    source: String,
    starts_at_local: String,
    tz: String,
    duration_minutes: i64,
    freq: Option<String>,
    interval_n: Option<i64>,
    byday: Option<String>,
    until_local: Option<String>,
    count_n: Option<i64>,
}

/// How a caller describes a repeat when creating an event.
///
/// A named struct rather than a tuple because five positional fields, three of them optional, is a
/// call site nobody can read — and swapping `until` for `count` would still compile.
pub struct RecurrenceSpec<'a> {
    pub freq: &'a str,
    pub interval: u32,
    pub byday: Option<&'a str>,
    pub until_local: Option<NaiveDateTime>,
    pub count: Option<u32>,
}

fn parse_local(raw: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(raw, LOCAL_FORMAT).ok()
}

/// Reads every event and its exceptions.
///
/// Loading the whole table is deliberate. A personal calendar is hundreds of rows, and a recurring
/// rule cannot be filtered by start date in SQL without expanding it first — the row for a weekly
/// meeting begun in 2024 is exactly the row a query about next Tuesday needs.
async fn load_events(pool: &sqlx::SqlitePool) -> sqlx::Result<Vec<StoredEvent>> {
    let rows: Vec<EventRow> = sqlx::query_as(
        "SELECT id, title, source, starts_at_local, tz, duration_minutes,
                freq, interval_n, byday, until_local, count_n
           FROM calendar_events",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(
            |EventRow {
                 id,
                 title,
                 source,
                 starts_at_local,
                 tz,
                 duration_minutes,
                 freq,
                 interval_n,
                 byday,
                 until_local,
                 count_n,
             }| {
                // A row that cannot be understood is skipped, not fatal. One unparseable zone —
                // a hand-edited database, a zone dropped by a tzdata update — must not blind the
                // calendar to every other event, the same isolation the email sidecar applies per
                // mailbox.
                let Some(starts_at_local) = parse_local(&starts_at_local) else {
                    tracing::warn!(event_id = id, "calendar: unparseable local start, skipping");
                    return None;
                };
                let Ok(tz) = tz.parse::<Tz>() else {
                    tracing::warn!(event_id = id, tz, "calendar: unknown time zone, skipping");
                    return None;
                };
                Some(StoredEvent {
                    id,
                    title,
                    source,
                    rule: Rule {
                        starts_at_local,
                        duration_minutes,
                        tz,
                        freq: freq.as_deref().and_then(Freq::parse),
                        interval: u32::try_from(interval_n.unwrap_or(1)).unwrap_or(1),
                        byday: byday
                            .as_deref()
                            .map(recurrence::parse_byday)
                            .unwrap_or_default(),
                        until_local: until_local.as_deref().and_then(parse_local),
                        count: count_n.and_then(|count| u32::try_from(count).ok()),
                    },
                })
            },
        )
        .collect())
}

async fn load_exceptions(
    pool: &sqlx::SqlitePool,
    event_id: i64,
) -> sqlx::Result<BTreeMap<NaiveDateTime, Exception>> {
    let rows: Vec<(String, String, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT occurrence_local, kind, moved_to_local, moved_duration_minutes
           FROM calendar_exceptions WHERE event_id = ?",
    )
    .bind(event_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|(occurrence_local, kind, moved_to_local, moved_duration)| {
            let occurrence_local = parse_local(&occurrence_local)?;
            let exception = match kind.as_str() {
                "cancelled" => Exception::Cancelled,
                "moved" => Exception::Moved {
                    to_local: parse_local(moved_to_local.as_deref()?)?,
                    duration_minutes: moved_duration?,
                },
                _ => return None,
            };
            Some((occurrence_local, exception))
        })
        .collect())
}

/// Every occurrence overlapping `[from, to)`, oldest first.
pub async fn occurrences(
    pool: &sqlx::SqlitePool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> sqlx::Result<Vec<EventOccurrence>> {
    let events = load_events(pool).await?;
    let mut found = Vec::new();

    for event in events {
        let exceptions = load_exceptions(pool, event.id).await?;
        for occurrence in recurrence::expand(&event.rule, &exceptions, from, to) {
            found.push(EventOccurrence {
                event_id: event.id,
                title: event.title.clone(),
                source: event.source.clone(),
                occurrence_local: occurrence.occurrence_local.format(LOCAL_FORMAT).to_string(),
                starts_at: occurrence.start,
                ends_at: occurrence.end,
            });
        }
    }

    found.sort_by_key(|occurrence| occurrence.starts_at);
    Ok(found)
}

/// Whether an event is running at `at`.
///
/// **Fails open.** A read error answers `false` — not busy — and the caller notifies. This inverts
/// the rule `budget.rs` and `wip.rs` follow, and the inversion is the point: those guard an agent
/// that could spend money or start work unattended, so their unreadable state must stop things.
/// This guards whether a phone buzzes. Silence caused by a database error is a message that
/// vanished, and nobody notices the notification that never came.
pub async fn busy_at(pool: &sqlx::SqlitePool, at: DateTime<Utc>) -> bool {
    // A one-second window: overlap with [at, at+1s) is the same as "running at `at`", and reusing
    // the overlap logic keeps one definition of what it means for an event to be happening.
    match occurrences(pool, at, at + Duration::seconds(1)).await {
        Ok(found) => !found.is_empty(),
        Err(error) => {
            tracing::warn!(%error, "calendar: busy check failed, treating as free so the notification still goes out");
            false
        }
    }
}

fn within_working_hours(local: NaiveDateTime, hours: &WorkingHours, duration: i64) -> bool {
    hours.weekdays.contains(&local.weekday())
        && local.time() >= hours.start
        && (local + Duration::minutes(duration)).time() <= hours.end
        && (local + Duration::minutes(duration)).date() == local.date()
}

/// The earliest working-hours slot of `duration_minutes` with no event in it, at or after `after`.
///
/// Used only to propose; never to decide whether you are busy. `None` means a fortnight of working
/// hours had no room, which is a real answer and better than proposing a slot in November.
pub async fn next_free_slot(
    pool: &sqlx::SqlitePool,
    runtime: &CalendarRuntime,
    duration_minutes: i64,
    after: DateTime<Utc>,
) -> sqlx::Result<Option<NaiveDateTime>> {
    let tz = runtime.default_tz;
    let hours = &runtime.working_hours;
    let horizon = after + Duration::days(SLOT_SEARCH_DAYS);
    let busy = occurrences(pool, after, horizon).await?;

    let local_now = after.with_timezone(&tz).naive_local();
    // Snap forward so a proposal never lands at 14:07.
    let aligned_minute = ((local_now.time().hour() as i64 * 60 + local_now.time().minute() as i64)
        / SLOT_ALIGNMENT_MINUTES
        + 1)
        * SLOT_ALIGNMENT_MINUTES;
    let mut candidate = local_now
        .date()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        + Duration::minutes(aligned_minute);

    let steps = SLOT_SEARCH_DAYS * 24 * 60 / SLOT_ALIGNMENT_MINUTES;
    for _ in 0..steps {
        if within_working_hours(candidate, hours, duration_minutes) {
            let Some(start) = tz
                .from_local_datetime(&candidate)
                .single()
                .map(|at| at.with_timezone(&Utc))
            else {
                candidate += Duration::minutes(SLOT_ALIGNMENT_MINUTES);
                continue;
            };
            let end = start + Duration::minutes(duration_minutes);
            let clashes = busy
                .iter()
                .any(|occurrence| occurrence.starts_at < end && occurrence.ends_at > start);
            if !clashes && start >= after {
                return Ok(Some(candidate));
            }
        }
        candidate += Duration::minutes(SLOT_ALIGNMENT_MINUTES);
    }

    Ok(None)
}

/// Inserts an event. `source` is `human` for something you created and `proposal` for something an
/// approved proposal did.
#[allow(clippy::too_many_arguments)]
pub async fn insert_event(
    pool: &sqlx::SqlitePool,
    title: &str,
    starts_at_local: NaiveDateTime,
    duration_minutes: i64,
    tz: Tz,
    source: &str,
    source_ref: Option<i64>,
    recurrence: Option<RecurrenceSpec<'_>>,
) -> sqlx::Result<i64> {
    let (freq, interval, byday, until, count) = match recurrence {
        Some(spec) => (
            Some(spec.freq),
            Some(i64::from(spec.interval)),
            spec.byday,
            spec.until_local
                .map(|until| until.format(LOCAL_FORMAT).to_string()),
            spec.count.map(i64::from),
        ),
        None => (None, None, None, None, None),
    };

    sqlx::query_scalar(
        "INSERT INTO calendar_events
             (starts_at_local, tz, duration_minutes, title, source, source_ref,
              freq, interval_n, byday, until_local, count_n, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         RETURNING id",
    )
    .bind(starts_at_local.format(LOCAL_FORMAT).to_string())
    .bind(tz.name())
    .bind(duration_minutes)
    .bind(title)
    .bind(source)
    .bind(source_ref)
    .bind(freq)
    .bind(interval)
    .bind(byday)
    .bind(until)
    .bind(count)
    .bind(Utc::now().to_rfc3339())
    .fetch_one(pool)
    .await
}

// ---------------------------------------------------------------------------------------------
// The `action` class's destination
// ---------------------------------------------------------------------------------------------

/// How long a block proposed for an `action` message lasts, absent anything better to go on.
///
/// A guess, and deliberately a modest one: the person moves or resizes it when approving, and a
/// proposal that asks for too much of the day is one that gets rejected rather than adjusted.
const PROPOSED_ACTION_MINUTES: i64 = 45;

/// How much of a subject line is carried into an event title.
const MAX_TITLE_CHARS: usize = 80;

/// Builds the title an `action` message's block would carry.
///
/// A subject is text a stranger wrote, so it is bounded here rather than trusted. It reaches the
/// database only after a human has read the proposal and approved it — which is the whole reason
/// the agent proposes instead of writing.
fn title_for_action(subject: Option<&str>) -> String {
    let subject = subject.unwrap_or("").trim();
    if subject.is_empty() {
        return "follow up on a message".to_string();
    }
    let trimmed: String = subject.chars().take(MAX_TITLE_CHARS).collect();
    if trimmed.chars().count() < subject.chars().count() {
        format!("{}…", trimmed.trim_end())
    } else {
        trimmed
    }
}

/// Files a proposal to set time aside for an `action` message.
///
/// Returns the proposal id, or `None` when nothing was filed — the feature is off, this message
/// already has one waiting, or the next fortnight of working hours has no room.
pub async fn propose_time_for_action(
    state: &AppState,
    email_id: i64,
    subject: Option<&str>,
) -> sqlx::Result<Option<i64>> {
    if !state.calendar.propose_for_actions {
        return Ok(None);
    }
    if crate::proposals::calendar_proposal_pending_for(&state.pool, email_id).await? {
        return Ok(None);
    }

    let Some(slot) = next_free_slot(
        &state.pool,
        &state.calendar,
        PROPOSED_ACTION_MINUTES,
        Utc::now(),
    )
    .await?
    else {
        tracing::info!(
            email_id,
            "calendar: no free working slot in the next fortnight, so no time was proposed"
        );
        return Ok(None);
    };

    let id = crate::proposals::create_calendar_event(
        &state.pool,
        email_id,
        &title_for_action(subject),
        &slot.format(LOCAL_FORMAT).to_string(),
        PROPOSED_ACTION_MINUTES,
        state.calendar.default_tz.name(),
        "this message needs work but not a reply now, so time is set aside for it",
    )
    .await?;
    Ok(Some(id))
}

#[derive(Debug)]
pub enum DecisionError {
    NotFound,
    NotPending,
    Malformed,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for DecisionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

/// Approving a proposed block: the event and the decision commit together, or neither does.
///
/// One transaction, and the compare-and-set on the proposal's status inside it, exactly as
/// `contacts::approve_merge` does. Two decisions racing means one loses at the status check and
/// rolls back rather than writing a second copy of the same meeting.
pub async fn approve_proposed_event(
    pool: &sqlx::SqlitePool,
    proposal_id: i64,
) -> Result<i64, DecisionError> {
    let proposal = crate::proposals::get(pool, proposal_id)
        .await?
        .ok_or(DecisionError::NotFound)?;
    if proposal.kind != "calendar-event" || proposal.status != "pending" {
        return Err(DecisionError::NotPending);
    }

    let input: serde_json::Value = proposal
        .tool_input
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .ok_or(DecisionError::Malformed)?;
    let (Some(title), Some(starts_at_local), Some(duration), Some(tz)) = (
        input.get("title").and_then(|value| value.as_str()),
        input
            .get("starts_at_local")
            .and_then(|value| value.as_str())
            .and_then(parse_local),
        input
            .get("duration_minutes")
            .and_then(serde_json::Value::as_i64),
        input
            .get("tz")
            .and_then(|value| value.as_str())
            .and_then(|name| name.parse::<Tz>().ok()),
    ) else {
        return Err(DecisionError::Malformed);
    };

    let at = Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;
    let event_id: i64 = sqlx::query_scalar(
        "INSERT INTO calendar_events
             (starts_at_local, tz, duration_minutes, title, source, source_ref, created_at)
         VALUES (?, ?, ?, ?, 'proposal', ?, ?)
         RETURNING id",
    )
    .bind(starts_at_local.format(LOCAL_FORMAT).to_string())
    .bind(tz.name())
    .bind(duration)
    .bind(title)
    .bind(proposal_id)
    .bind(&at)
    .fetch_one(&mut *transaction)
    .await?;

    if !crate::proposals::transition_in_transaction(
        &mut transaction,
        proposal_id,
        "approved",
        "approved by user",
        &at,
    )
    .await?
    {
        return Err(DecisionError::NotPending);
    }

    transaction.commit().await?;
    Ok(event_id)
}

/// Rejecting one leaves no trace on the calendar, which is the point: a refused suggestion is not
/// a cancelled meeting.
pub async fn reject_proposed_event(
    pool: &sqlx::SqlitePool,
    proposal_id: i64,
) -> Result<(), DecisionError> {
    let proposal = crate::proposals::get(pool, proposal_id)
        .await?
        .ok_or(DecisionError::NotFound)?;
    if proposal.kind != "calendar-event" || proposal.status != "pending" {
        return Err(DecisionError::NotPending);
    }
    if !crate::proposals::transition(pool, proposal_id, "rejected", "rejected by user").await? {
        return Err(DecisionError::NotPending);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct WindowQuery {
    from: String,
    to: String,
}

/// `GET /calendar/events?from=&to=` — the occurrences in a window, expanded.
pub async fn list_events(
    State(state): State<AppState>,
    Query(window): Query<WindowQuery>,
) -> impl IntoResponse {
    let (Ok(from), Ok(to)) = (
        DateTime::parse_from_rfc3339(&window.from),
        DateTime::parse_from_rfc3339(&window.to),
    ) else {
        return (StatusCode::BAD_REQUEST, "from and to must be RFC 3339").into_response();
    };

    match occurrences(
        &state.pool,
        from.with_timezone(&Utc),
        to.with_timezone(&Utc),
    )
    .await
    {
        Ok(found) => axum::Json(found).into_response(),
        Err(error) => db_error(error),
    }
}

#[derive(Deserialize)]
pub struct CreateEventRequest {
    title: String,
    starts_at_local: String,
    duration_minutes: i64,
    tz: Option<String>,
    freq: Option<String>,
    interval: Option<u32>,
    byday: Option<String>,
    until_local: Option<String>,
    count: Option<u32>,
}

/// `POST /calendar/events`
pub async fn create_event(
    State(state): State<AppState>,
    axum::Json(request): axum::Json<CreateEventRequest>,
) -> impl IntoResponse {
    if request.title.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "title must not be empty").into_response();
    }
    if request.duration_minutes <= 0 {
        return (StatusCode::BAD_REQUEST, "duration_minutes must be positive").into_response();
    }
    let Some(starts_at_local) = parse_local(&request.starts_at_local) else {
        return (
            StatusCode::BAD_REQUEST,
            "starts_at_local must look like 2026-08-03T09:00:00",
        )
            .into_response();
    };
    let tz = match request.tz.as_deref() {
        None => state.calendar.default_tz,
        Some(raw) => match raw.parse::<Tz>() {
            Ok(tz) => tz,
            Err(_) => return (StatusCode::BAD_REQUEST, "unknown time zone").into_response(),
        },
    };

    // Both a COUNT and an UNTIL is not a stricter rule, it is two answers to one question. Refusing
    // is better than silently picking one and having the series end somewhere nobody chose.
    if request.count.is_some() && request.until_local.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            "a rule may carry count or until_local, not both",
        )
            .into_response();
    }

    let recurrence = match request.freq.as_deref() {
        None => None,
        Some(raw) => {
            if Freq::parse(raw).is_none() {
                return (
                    StatusCode::BAD_REQUEST,
                    "freq must be daily, weekly or monthly",
                )
                    .into_response();
            }
            let until = match request.until_local.as_deref() {
                None => None,
                Some(raw) => match parse_local(raw) {
                    Some(until) => Some(until),
                    None => {
                        return (
                            StatusCode::BAD_REQUEST,
                            "until_local is not a local timestamp",
                        )
                            .into_response();
                    }
                },
            };
            Some(RecurrenceSpec {
                freq: raw,
                interval: request.interval.unwrap_or(1).max(1),
                byday: request.byday.as_deref(),
                until_local: until,
                count: request.count,
            })
        }
    };

    match insert_event(
        &state.pool,
        request.title.trim(),
        starts_at_local,
        request.duration_minutes,
        tz,
        "human",
        None,
        recurrence,
    )
    .await
    {
        Ok(id) => (
            StatusCode::CREATED,
            axum::Json(serde_json::json!({ "id": id })),
        )
            .into_response(),
        Err(error) => db_error(error),
    }
}

/// `DELETE /calendar/events/{id}` — removes the whole series. Exceptions cascade.
pub async fn delete_event(State(state): State<AppState>, Path(id): Path<i64>) -> impl IntoResponse {
    match sqlx::query("DELETE FROM calendar_events WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await
    {
        Ok(result) if result.rows_affected() == 0 => {
            (StatusCode::NOT_FOUND, "no such event").into_response()
        }
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => db_error(error),
    }
}

#[derive(Deserialize)]
pub struct OccurrenceRequest {
    occurrence_local: String,
    to_local: Option<String>,
    duration_minutes: Option<i64>,
}

/// `POST /calendar/events/{id}/cancel` — drops one occurrence, leaving the series intact.
pub async fn cancel_occurrence(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(request): axum::Json<OccurrenceRequest>,
) -> impl IntoResponse {
    let Some(occurrence) = parse_local(&request.occurrence_local) else {
        return (
            StatusCode::BAD_REQUEST,
            "occurrence_local is not a local timestamp",
        )
            .into_response();
    };
    upsert_exception(&state.pool, id, occurrence, "cancelled", None, None).await
}

/// `POST /calendar/events/{id}/move` — relocates one occurrence.
pub async fn move_occurrence(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    axum::Json(request): axum::Json<OccurrenceRequest>,
) -> impl IntoResponse {
    let Some(occurrence) = parse_local(&request.occurrence_local) else {
        return (
            StatusCode::BAD_REQUEST,
            "occurrence_local is not a local timestamp",
        )
            .into_response();
    };
    let Some(to_local) = request.to_local.as_deref().and_then(parse_local) else {
        return (
            StatusCode::BAD_REQUEST,
            "to_local is required to move an occurrence",
        )
            .into_response();
    };
    let duration = request.duration_minutes.unwrap_or(0);
    if duration <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            "duration_minutes must be positive when moving an occurrence",
        )
            .into_response();
    }
    upsert_exception(
        &state.pool,
        id,
        occurrence,
        "moved",
        Some(to_local),
        Some(duration),
    )
    .await
}

async fn upsert_exception(
    pool: &sqlx::SqlitePool,
    event_id: i64,
    occurrence_local: NaiveDateTime,
    kind: &str,
    to_local: Option<NaiveDateTime>,
    duration_minutes: Option<i64>,
) -> axum::response::Response {
    let exists: Result<Option<i64>, _> =
        sqlx::query_scalar("SELECT id FROM calendar_events WHERE id = ?")
            .bind(event_id)
            .fetch_optional(pool)
            .await;
    match exists {
        Ok(None) => return (StatusCode::NOT_FOUND, "no such event").into_response(),
        Err(error) => return db_error(error),
        Ok(Some(_)) => {}
    }

    // Upsert, because moving an occurrence twice must relocate it rather than fail. The identity
    // is the ORIGINAL local start, which is exactly what makes that possible.
    let written = sqlx::query(
        "INSERT INTO calendar_exceptions
             (event_id, occurrence_local, kind, moved_to_local, moved_duration_minutes)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (event_id, occurrence_local) DO UPDATE SET
             kind = excluded.kind,
             moved_to_local = excluded.moved_to_local,
             moved_duration_minutes = excluded.moved_duration_minutes",
    )
    .bind(event_id)
    .bind(occurrence_local.format(LOCAL_FORMAT).to_string())
    .bind(kind)
    .bind(to_local.map(|at| at.format(LOCAL_FORMAT).to_string()))
    .bind(duration_minutes)
    .execute(pool)
    .await;

    match written {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => db_error(error),
    }
}

#[derive(Serialize)]
struct CalendarConfigView {
    default_tz: String,
    working_hours_start: String,
    working_hours_end: String,
    working_weekdays: Vec<String>,
}

/// `GET /calendar/config` — what the shell needs to render times the way the daemon reads them.
pub async fn get_config(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(CalendarConfigView {
        default_tz: state.calendar.default_tz.name().to_string(),
        working_hours_start: state
            .calendar
            .working_hours
            .start
            .format("%H:%M")
            .to_string(),
        working_hours_end: state.calendar.working_hours.end.format("%H:%M").to_string(),
        working_weekdays: state
            .calendar
            .working_hours
            .weekdays
            .iter()
            .map(|weekday| weekday.to_string())
            .collect(),
    })
}

#[derive(Serialize)]
struct BusyView {
    busy: bool,
}

/// `GET /calendar/busy` — the same answer the notifier gets, so it can be checked by hand.
pub async fn get_busy(State(state): State<AppState>) -> impl IntoResponse {
    axum::Json(BusyView {
        busy: busy_at(&state.pool, Utc::now()).await,
    })
}

fn db_error(error: sqlx::Error) -> axum::response::Response {
    tracing::warn!(%error, "calendar: database access failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const LISBON: Tz = chrono_tz::Europe::Lisbon;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .expect("an in-memory database");
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .expect("migrations to apply");
        pool
    }

    fn local(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, LOCAL_FORMAT).expect("a valid local timestamp")
    }

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("a valid instant")
            .with_timezone(&Utc)
    }

    async fn one_off(pool: &sqlx::SqlitePool, starts: &str, minutes: i64) -> i64 {
        insert_event(
            pool,
            "standup",
            local(starts),
            minutes,
            LISBON,
            "human",
            None,
            None,
        )
        .await
        .expect("the event to insert")
    }

    #[tokio::test]
    async fn an_event_is_reported_in_a_window_that_overlaps_it() {
        let pool = test_pool().await;
        one_off(&pool, "2026-08-03T09:00:00", 60).await;

        let found = occurrences(
            &pool,
            utc("2026-08-03T00:00:00Z"),
            utc("2026-08-04T00:00:00Z"),
        )
        .await
        .expect("the read to succeed");

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "standup");
        assert_eq!(found[0].occurrence_local, "2026-08-03T09:00:00");
        assert_eq!(found[0].source, "human");
    }

    #[tokio::test]
    async fn busy_is_true_inside_an_event_and_false_on_either_side() {
        let pool = test_pool().await;
        // 09:00 Lisbon in August is 08:00Z, so the event runs 08:00Z-09:00Z.
        one_off(&pool, "2026-08-03T09:00:00", 60).await;

        assert!(!busy_at(&pool, utc("2026-08-03T07:59:00Z")).await);
        assert!(
            busy_at(&pool, utc("2026-08-03T08:00:00Z")).await,
            "the start instant counts as busy"
        );
        assert!(busy_at(&pool, utc("2026-08-03T08:30:00Z")).await);
        assert!(
            !busy_at(&pool, utc("2026-08-03T09:00:00Z")).await,
            "the end instant does not — an event is a half-open interval, or back-to-back meetings overlap"
        );
    }

    /// The inversion that distinguishes this module from every brake in the tree. A database that
    /// cannot be read must not turn into silence.
    #[tokio::test]
    async fn an_unreadable_calendar_reports_free_so_the_notification_still_goes_out() {
        let pool = test_pool().await;
        one_off(&pool, "2026-08-03T09:00:00", 60).await;
        pool.close().await;

        assert!(
            !busy_at(&pool, utc("2026-08-03T08:30:00Z")).await,
            "mid-event, but unreadable: the answer must be 'free' so the message still arrives"
        );
    }

    #[tokio::test]
    async fn a_cancelled_occurrence_stops_making_you_busy() {
        let pool = test_pool().await;
        let id = insert_event(
            &pool,
            "standup",
            local("2026-08-03T09:00:00"),
            60,
            LISBON,
            "human",
            None,
            Some(RecurrenceSpec {
                freq: "daily",
                interval: 1,
                byday: None,
                until_local: None,
                count: None,
            }),
        )
        .await
        .expect("the event to insert");

        assert!(busy_at(&pool, utc("2026-08-04T08:30:00Z")).await);

        upsert_exception(
            &pool,
            id,
            local("2026-08-04T09:00:00"),
            "cancelled",
            None,
            None,
        )
        .await;

        assert!(!busy_at(&pool, utc("2026-08-04T08:30:00Z")).await);
        assert!(
            busy_at(&pool, utc("2026-08-05T08:30:00Z")).await,
            "the rest of the series is untouched"
        );
    }

    #[tokio::test]
    async fn moving_an_occurrence_twice_relocates_it_rather_than_duplicating_it() {
        let pool = test_pool().await;
        let id = one_off(&pool, "2026-08-03T09:00:00", 60).await;

        upsert_exception(
            &pool,
            id,
            local("2026-08-03T09:00:00"),
            "moved",
            Some(local("2026-08-03T15:00:00")),
            Some(30),
        )
        .await;
        upsert_exception(
            &pool,
            id,
            local("2026-08-03T09:00:00"),
            "moved",
            Some(local("2026-08-03T17:00:00")),
            Some(45),
        )
        .await;

        let found = occurrences(
            &pool,
            utc("2026-08-03T00:00:00Z"),
            utc("2026-08-04T00:00:00Z"),
        )
        .await
        .expect("the read to succeed");

        assert_eq!(found.len(), 1, "one occurrence, moved twice");
        assert_eq!(found[0].starts_at, utc("2026-08-03T16:00:00Z"));
    }

    /// A row nobody can parse must cost only itself.
    #[tokio::test]
    async fn an_event_with_an_unknown_time_zone_is_skipped_without_hiding_the_others() {
        let pool = test_pool().await;
        one_off(&pool, "2026-08-03T09:00:00", 60).await;
        sqlx::query(
            "INSERT INTO calendar_events
                 (starts_at_local, tz, duration_minutes, title, source, created_at)
             VALUES ('2026-08-03T10:00:00', 'Mars/Olympus_Mons', 60, 'broken', 'human', '2026-08-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .expect("the row to insert");

        let found = occurrences(
            &pool,
            utc("2026-08-03T00:00:00Z"),
            utc("2026-08-04T00:00:00Z"),
        )
        .await
        .expect("the read to succeed");

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "standup");
    }

    #[tokio::test]
    async fn a_free_slot_lands_inside_working_hours_and_avoids_a_booked_one() {
        let pool = test_pool().await;
        let runtime = CalendarRuntime {
            default_tz: LISBON,
            working_hours: WorkingHours::default(),
            propose_for_actions: false,
        };
        // Monday 3 August 2026, 09:00-11:00 local is taken.
        one_off(&pool, "2026-08-03T09:00:00", 120).await;

        let slot = next_free_slot(&pool, &runtime, 60, utc("2026-08-03T06:00:00Z"))
            .await
            .expect("the read to succeed")
            .expect("a slot within the fortnight");

        assert_eq!(
            slot,
            local("2026-08-03T11:00:00"),
            "the first aligned hour after the booked block, still inside 09:00-18:00"
        );
    }

    #[tokio::test]
    async fn a_slot_is_never_proposed_outside_working_hours() {
        let pool = test_pool().await;
        let runtime = CalendarRuntime {
            default_tz: LISBON,
            working_hours: WorkingHours::default(),
            propose_for_actions: false,
        };

        // 22:00 Lisbon on a Monday: the next working moment is Tuesday morning.
        let slot = next_free_slot(&pool, &runtime, 60, utc("2026-08-03T21:00:00Z"))
            .await
            .expect("the read to succeed")
            .expect("a slot within the fortnight");

        assert_eq!(slot, local("2026-08-04T09:00:00"));
    }

    #[tokio::test]
    async fn a_slot_skips_the_weekend() {
        let pool = test_pool().await;
        let runtime = CalendarRuntime {
            default_tz: LISBON,
            working_hours: WorkingHours::default(),
            propose_for_actions: false,
        };

        // Friday 7 August 2026 at 18:30 local — after hours, and the weekend is not working time.
        let slot = next_free_slot(&pool, &runtime, 60, utc("2026-08-07T17:30:00Z"))
            .await
            .expect("the read to succeed")
            .expect("a slot within the fortnight");

        assert_eq!(slot, local("2026-08-10T09:00:00"), "the following Monday");
    }

    #[tokio::test]
    async fn a_slot_that_would_run_past_the_working_day_is_not_offered() {
        let pool = test_pool().await;
        let runtime = CalendarRuntime {
            default_tz: LISBON,
            working_hours: WorkingHours::default(),
            propose_for_actions: false,
        };

        // 17:30 local leaves half an hour; a 60-minute task must go to the next day.
        let slot = next_free_slot(&pool, &runtime, 60, utc("2026-08-03T16:30:00Z"))
            .await
            .expect("the read to succeed")
            .expect("a slot within the fortnight");

        assert_eq!(slot, local("2026-08-04T09:00:00"));
    }

    #[tokio::test]
    async fn an_event_created_from_a_proposal_says_so() {
        let pool = test_pool().await;
        insert_event(
            &pool,
            "write the reply",
            local("2026-08-03T14:00:00"),
            45,
            LISBON,
            "proposal",
            Some(7),
            None,
        )
        .await
        .expect("the event to insert");

        let found = occurrences(
            &pool,
            utc("2026-08-03T00:00:00Z"),
            utc("2026-08-04T00:00:00Z"),
        )
        .await
        .expect("the read to succeed");

        assert_eq!(found[0].source, "proposal");
    }

    #[tokio::test]
    async fn the_busy_window_uses_the_zone_the_event_was_written_in() {
        let pool = test_pool().await;
        insert_event(
            &pool,
            "call",
            local("2026-08-03T09:00:00"),
            60,
            chrono_tz::America::New_York,
            "human",
            None,
            None,
        )
        .await
        .expect("the event to insert");

        assert!(
            busy_at(&pool, utc("2026-08-03T13:30:00Z")).await,
            "09:00 in New York in August is 13:00Z"
        );
        assert!(!busy_at(&pool, utc("2026-08-03T08:30:00Z")).await);
    }

    /// Guards the claim the whole storage decision rests on: the wall clock is what recurs.
    #[tokio::test]
    async fn a_weekly_event_keeps_its_wall_clock_across_the_autumn_change() {
        let pool = test_pool().await;
        insert_event(
            &pool,
            "weekly",
            local("2026-10-19T09:00:00"),
            60,
            LISBON,
            "human",
            None,
            Some(RecurrenceSpec {
                freq: "weekly",
                interval: 1,
                byday: None,
                until_local: None,
                count: None,
            }),
        )
        .await
        .expect("the event to insert");

        assert!(
            busy_at(&pool, utc("2026-10-19T08:30:00Z")).await,
            "before the change, 09:00 local is 08:00Z"
        );
        assert!(
            busy_at(&pool, utc("2026-10-26T09:30:00Z")).await,
            "after it, the same 09:00 local is 09:00Z"
        );
        assert!(
            !busy_at(&pool, utc("2026-10-26T08:30:00Z")).await,
            "and the old instant is no longer the meeting"
        );
    }

    #[tokio::test]
    async fn the_utc_offset_helper_is_used_rather_than_assumed() {
        // Pins the assumption the tests above lean on, so a tzdata change fails here first with a
        // clear message instead of failing five busy assertions with confusing ones.
        let at = LISBON
            .with_ymd_and_hms(2026, 8, 3, 9, 0, 0)
            .single()
            .expect("an unambiguous instant");
        assert_eq!(at.with_timezone(&Utc), utc("2026-08-03T08:00:00Z"));
    }

    // ── the `action` class's destination ─────────────────────────────────────────────────────

    async fn state_with(pool: sqlx::SqlitePool, propose: bool) -> AppState {
        AppState {
            token: crate::auth::Token("test-token".into()),
            pool,
            telegram_doctrine: None,
            runner: std::sync::Arc::new(crate::runner::FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            files_root: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: std::sync::Arc::new(crate::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            quota: std::sync::Arc::new(crate::quota::QuotaRuntime::disabled()),
            calendar: std::sync::Arc::new(CalendarRuntime {
                default_tz: LISBON,
                working_hours: WorkingHours::default(),
                propose_for_actions: propose,
            }),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            run_handles: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_messages: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_tails: Default::default(),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    async fn pending_calendar_proposals(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM proposals WHERE kind = 'calendar-event' AND status = 'pending'",
        )
        .fetch_one(pool)
        .await
        .expect("proposals to be countable")
    }

    /// The ship-dark default. Everything in this tree that generates work for a person to review
    /// starts off, and this is the only part of the calendar that does.
    #[tokio::test]
    async fn no_time_is_proposed_while_the_feature_is_off() {
        let state = state_with(test_pool().await, false).await;

        let filed = propose_time_for_action(&state, 1, Some("review the contract"))
            .await
            .expect("the call to succeed");

        assert!(filed.is_none());
        assert_eq!(pending_calendar_proposals(&state.pool).await, 0);
    }

    #[tokio::test]
    async fn an_action_message_gets_one_proposal_however_often_it_is_triaged() {
        let state = state_with(test_pool().await, true).await;

        let first = propose_time_for_action(&state, 7, Some("review the contract"))
            .await
            .expect("the call to succeed");
        let second = propose_time_for_action(&state, 7, Some("review the contract"))
            .await
            .expect("the call to succeed");

        assert!(first.is_some());
        assert!(
            second.is_none(),
            "a second pass over the same message must not file a second proposal"
        );
        assert_eq!(pending_calendar_proposals(&state.pool).await, 1);
    }

    /// Two messages are two pieces of work, so the dedupe must key on the message and not merely
    /// on there being a calendar proposal open.
    #[tokio::test]
    async fn a_different_message_still_gets_its_own_proposal() {
        let state = state_with(test_pool().await, true).await;

        propose_time_for_action(&state, 7, Some("one"))
            .await
            .expect("the call to succeed");
        propose_time_for_action(&state, 8, Some("another"))
            .await
            .expect("the call to succeed");

        assert_eq!(pending_calendar_proposals(&state.pool).await, 2);
    }

    #[tokio::test]
    async fn approving_a_proposal_writes_the_event_and_marks_the_decision() {
        let state = state_with(test_pool().await, true).await;
        let proposal_id = propose_time_for_action(&state, 7, Some("review the contract"))
            .await
            .expect("the call to succeed")
            .expect("a proposal");

        let event_id = approve_proposed_event(&state.pool, proposal_id)
            .await
            .expect("the approval to succeed");

        let (source, source_ref): (String, Option<i64>) =
            sqlx::query_as("SELECT source, source_ref FROM calendar_events WHERE id = ?")
                .bind(event_id)
                .fetch_one(&state.pool)
                .await
                .expect("the event to be readable");
        assert_eq!(source, "proposal");
        assert_eq!(
            source_ref,
            Some(proposal_id),
            "the event points back at the decision that created it"
        );

        let status: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_one(&state.pool)
            .await
            .expect("the proposal to be readable");
        assert_eq!(status, "approved");
    }

    /// The compare-and-set inside the transaction is what makes this safe; without it a double
    /// click would book the same block twice.
    #[tokio::test]
    async fn approving_the_same_proposal_twice_books_only_one_event() {
        let state = state_with(test_pool().await, true).await;
        let proposal_id = propose_time_for_action(&state, 7, Some("review the contract"))
            .await
            .expect("the call to succeed")
            .expect("a proposal");

        approve_proposed_event(&state.pool, proposal_id)
            .await
            .expect("the first approval to succeed");
        let second = approve_proposed_event(&state.pool, proposal_id).await;

        assert!(matches!(second, Err(DecisionError::NotPending)));
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM calendar_events")
            .fetch_one(&state.pool)
            .await
            .expect("events to be countable");
        assert_eq!(events, 1);
    }

    #[tokio::test]
    async fn rejecting_a_proposal_leaves_no_trace_on_the_calendar() {
        let state = state_with(test_pool().await, true).await;
        let proposal_id = propose_time_for_action(&state, 7, Some("review the contract"))
            .await
            .expect("the call to succeed")
            .expect("a proposal");

        reject_proposed_event(&state.pool, proposal_id)
            .await
            .expect("the rejection to succeed");

        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM calendar_events")
            .fetch_one(&state.pool)
            .await
            .expect("events to be countable");
        assert_eq!(
            events, 0,
            "a declined suggestion is not a cancelled meeting"
        );
    }

    #[tokio::test]
    async fn a_proposed_block_lands_inside_working_hours() {
        let state = state_with(test_pool().await, true).await;
        let proposal_id = propose_time_for_action(&state, 7, Some("review the contract"))
            .await
            .expect("the call to succeed")
            .expect("a proposal");

        let event_id = approve_proposed_event(&state.pool, proposal_id)
            .await
            .expect("the approval to succeed");
        let starts_at_local: String =
            sqlx::query_scalar("SELECT starts_at_local FROM calendar_events WHERE id = ?")
                .bind(event_id)
                .fetch_one(&state.pool)
                .await
                .expect("the event to be readable");

        let starts = parse_local(&starts_at_local).expect("a stored local timestamp");
        let hours = WorkingHours::default();
        assert!(hours.weekdays.contains(&starts.weekday()));
        assert!(starts.time() >= hours.start && starts.time() < hours.end);
    }

    #[tokio::test]
    async fn a_long_subject_is_cut_rather_than_carried_whole() {
        let long = "x".repeat(200);
        let title = title_for_action(Some(&long));

        assert!(
            title.chars().count() <= MAX_TITLE_CHARS + 1,
            "plus the ellipsis"
        );
        assert!(title.ends_with('…'));
    }

    #[tokio::test]
    async fn a_message_with_no_subject_still_gets_a_readable_title() {
        assert_eq!(title_for_action(None), "follow up on a message");
        assert_eq!(title_for_action(Some("   ")), "follow up on a message");
    }
}
