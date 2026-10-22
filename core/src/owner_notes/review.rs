//! The owner's weekly review of the second brain: what was written this ISO week, what is still
//! unlinked, a few old notes put back in front of the owner, and how many proposals wait.
//!
//! One read (`weekly_review`, also served by `GET /owner-notes/review`) and one push
//! (`send_weekly_if_due`): the first tick at or after Monday 09:00 local time of an ISO week sends
//! one message through the channel outbox, and a marker in `schema_meta` keeps it to one per week
//! across restarts. The clock is always passed in, so the tests never wait for a Monday.
//!
//! Boundary (second-brain §5): this is the owner-notes module, so it may read `owner_notes*`. The
//! message goes to the owner's own channel and the text of a note is never logged.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Utc};
use sqlx::SqlitePool;

use super::OwnerNote;
use crate::channels::contract::{ConversationKey, OutPart};
use crate::channels::outbox::{self, Class, NewAction};
use crate::channels::registry::RoutingState;
use crate::channels::{access, routing};
use crate::state::AppState;

/// `schema_meta` key holding the ISO week (`2026-W46`) the last review was sent for.
const SENT_WEEK_KEY: &str = "owner_notes.review_sent_week";

/// Hour of the Monday, local time, from which that week's review is due.
const DUE_HOUR: u32 = 9;

/// A note counts as resurfaceable once nobody touched it for this long, measured at the start of
/// the week so the candidates (and so the picks) do not move while the week goes by.
const RESURFACE_AFTER_DAYS: i64 = 30;

/// Notes put back in front of the owner each week.
const RESURFACED_MAX: usize = 3;

/// Unlinked notes listed at most.
const UNLINKED_MAX: i64 = 10;

/// Longest first line of a note quoted in the message, in characters.
const LINE_MAX: usize = 80;

/// How often the loop looks at the clock.
const TICK: std::time::Duration = std::time::Duration::from_secs(5 * 60);

const NOTE_SELECT: &str =
    "SELECT id, note_text, origin, state, created_at, updated_at FROM owner_notes";

/// The review of one ISO week, as `GET /owner-notes/review` serialises it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Review {
    /// Monday 00:00 local time, rfc3339.
    pub week_start: String,
    /// The next Monday 00:00 local time, rfc3339 (exclusive end of the week).
    pub week_end: String,
    /// Active notes written during the week, newest first.
    pub created: Vec<OwnerNote>,
    /// Active notes with no link at all, newest first, at most ten.
    pub unlinked: Vec<OwnerNote>,
    /// How many active notes have no link at all, uncapped (`unlinked` stops at ten).
    pub unlinked_total: i64,
    /// Up to three active notes untouched for 30 days, fixed for the whole week.
    pub resurfaced: Vec<OwnerNote>,
    /// Knowledge rows waiting for the owner's decision.
    pub waiting_proposals: i64,
}

/// A wall-clock hour of `date` in the local zone. A time that does not exist (a daylight-saving
/// gap) moves to the first hour that does.
fn local_at(date: NaiveDate, hour: u32) -> DateTime<Local> {
    let base = date
        .and_hms_opt(hour, 0, 0)
        .expect("an hour below 24 is a valid time");
    for extra in 0..4 {
        if let Some(moment) = Local
            .from_local_datetime(&(base + Duration::hours(extra)))
            .earliest()
        {
            return moment;
        }
    }
    Local.from_utc_datetime(&base)
}

/// The Monday of the ISO week `now` falls in.
fn monday_of(now: DateTime<Local>) -> NaiveDate {
    let today = now.date_naive();
    today - Duration::days(i64::from(today.weekday().num_days_from_monday()))
}

/// `[Monday 00:00, next Monday 00:00)` of the ISO week of `now`, in local time.
pub fn week_bounds(now: DateTime<Local>) -> (DateTime<Local>, DateTime<Local>) {
    let monday = monday_of(now);
    (local_at(monday, 0), local_at(monday + Duration::days(7), 0))
}

/// `2026-W46`: the ISO year and week, which is also what the sent marker stores.
fn week_key(now: DateTime<Local>) -> String {
    let week = now.iso_week();
    format!("{}-W{:02}", week.year(), week.week())
}

/// The moment from which the review of the week of `now` is due.
fn due_at(now: DateTime<Local>) -> DateTime<Local> {
    local_at(monday_of(now), DUE_HOUR)
}

/// A timestamp the way the notes store theirs (`Utc::now().to_rfc3339()`), so the comparisons in
/// SQL are between strings of one shape.
fn stamp(moment: DateTime<Local>) -> String {
    moment.with_timezone(&Utc).to_rfc3339()
}

/// SplitMix64 finaliser: a fixed arithmetic scramble, the same on every build and platform (a
/// `DefaultHasher` is neither promised nor stable).
fn mix(value: u64) -> u64 {
    let mut x = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The rank of a note in the pick of one ISO week; the lowest ranks are resurfaced.
fn pick_rank(year: i32, week: u32, id: i64) -> u64 {
    let week_number = u64::from(year.unsigned_abs()) * 100 + u64::from(week);
    mix(mix(week_number) ^ id.unsigned_abs())
}

/// The review for the ISO week of `now`.
pub async fn weekly_review(pool: &SqlitePool, now: DateTime<Local>) -> sqlx::Result<Review> {
    let (start, end) = week_bounds(now);
    let (start_utc, end_utc) = (stamp(start), stamp(end));

    let created: Vec<OwnerNote> = sqlx::query_as(note_query(
        " WHERE state = 'active' AND created_at >= ? AND created_at < ? \
         ORDER BY created_at DESC, id DESC",
    ))
    .bind(&start_utc)
    .bind(&end_utc)
    .fetch_all(pool)
    .await?;

    let unlinked: Vec<OwnerNote> = sqlx::query_as(note_query(
        " WHERE state = 'active' AND NOT EXISTS \
         (SELECT 1 FROM owner_note_links WHERE owner_note_links.note_id = owner_notes.id) \
         ORDER BY created_at DESC, id DESC LIMIT ?",
    ))
    .bind(UNLINKED_MAX)
    .fetch_all(pool)
    .await?;

    let unlinked_total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM owner_notes WHERE state = 'active' AND NOT EXISTS \
         (SELECT 1 FROM owner_note_links WHERE owner_note_links.note_id = owner_notes.id)",
    )
    .fetch_one(pool)
    .await?;

    let resurfaced = resurfaced(pool, start, now).await?;

    let waiting_proposals: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM knowledge WHERE status = 'proposed'")
            .fetch_one(pool)
            .await?;

    Ok(Review {
        week_start: start.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        week_end: end.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        created,
        unlinked,
        unlinked_total,
        resurfaced,
        waiting_proposals,
    })
}

/// `NOTE_SELECT` followed by a fixed tail. The tails are literals in this file, never input, which
/// is what makes the assertion safe.
fn note_query(tail: &'static str) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(format!("{NOTE_SELECT}{tail}"))
}

/// Active notes whose last edit is older than 30 days before the week began, ranked by a hash of
/// the ISO week and the id: the same picks all week, different ones the next.
async fn resurfaced(
    pool: &SqlitePool,
    week_start: DateTime<Local>,
    now: DateTime<Local>,
) -> sqlx::Result<Vec<OwnerNote>> {
    let cutoff = stamp(week_start - Duration::days(RESURFACE_AFTER_DAYS));
    let candidates: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM owner_notes WHERE state = 'active' AND updated_at < ?")
            .bind(&cutoff)
            .fetch_all(pool)
            .await?;

    let week = now.iso_week();
    let mut ranked: Vec<(u64, i64)> = candidates
        .into_iter()
        .map(|id| (pick_rank(week.year(), week.week(), id), id))
        .collect();
    ranked.sort_unstable();
    ranked.truncate(RESURFACED_MAX);

    let mut picked = Vec::with_capacity(ranked.len());
    for (_, id) in ranked {
        let note: Option<OwnerNote> = sqlx::query_as(note_query(" WHERE id = ?"))
            .bind(id)
            .fetch_optional(pool)
            .await?;
        picked.extend(note);
    }
    Ok(picked)
}

/// The first line of a note, trimmed, cut to `LINE_MAX` characters (an ellipsis counts in them).
fn first_line(note_text: &str) -> String {
    let line = note_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    if line.chars().count() <= LINE_MAX {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(LINE_MAX - 1).collect();
    cut.push('\u{2026}');
    cut
}

/// The message the owner receives: counts, the first line of each resurfaced note, and where to
/// look. English, like the rest of the channel strings.
pub fn weekly_message(review: &Review) -> String {
    let mut message = format!(
        "Your weekly review\n\
         New notes this week: {}\n\
         Notes without a link: {}\n\
         Proposals waiting for you: {}",
        review.created.len(),
        review.unlinked_total,
        review.waiting_proposals,
    );
    if !review.resurfaced.is_empty() {
        message.push_str("\n\nWorth another look:");
        for note in &review.resurfaced {
            message.push_str("\n- ");
            message.push_str(&first_line(&note.note_text));
        }
    }
    message.push_str("\n\nOpen the Brain page to go through them.");
    message
}

/// The ISO week the last review was sent for, if one ever was.
async fn sent_week(pool: &SqlitePool) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = ?")
        .bind(SENT_WEEK_KEY)
        .fetch_optional(pool)
        .await
}

/// Sends this week's review when it is due and not sent yet. Returns whether it enqueued one.
///
/// Due: at or after Monday 09:00 local of the current ISO week, with a different week in the
/// marker. Nothing is sent, and no marker written, while the primary channel has no destination
/// (the next tick tries again). The message and the marker commit together, so a crash between
/// them neither loses the review nor sends it twice. The marker is claimed first inside that
/// transaction, so a second daemon on the same database finds it taken and sends nothing. A week
/// with nothing to report (no new, unlinked or old notes, no proposals) writes the marker and
/// sends nothing.
pub async fn send_weekly_if_due(state: &AppState, now: DateTime<Local>) -> sqlx::Result<bool> {
    let pool = &state.pool;
    if now < due_at(now) {
        return Ok(false);
    }
    let week = week_key(now);
    if sent_week(pool).await?.as_deref() == Some(week.as_str()) {
        return Ok(false);
    }
    if routing::routing_state(state).await? != RoutingState::Ok {
        return Ok(false);
    }
    let Some(primary) = access::primary(pool).await? else {
        return Ok(false);
    };
    let Some(home) = access::home_conversation(pool, primary).await? else {
        return Ok(false);
    };

    let review = weekly_review(pool, now).await?;
    let note_text = weekly_message(&review);

    // Claim the week first, as the transaction's opening write: of two daemons on one database
    // only one changes the marker, and only that one enqueues.
    let mut tx = pool.begin().await?;
    let claimed = sqlx::query(
        "INSERT INTO schema_meta (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value \
         WHERE value IS NOT excluded.value",
    )
    .bind(SENT_WEEK_KEY)
    .bind(&week)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if claimed != 1 {
        tx.rollback().await?;
        return Ok(false);
    }
    if review.created.is_empty()
        && review.unlinked_total == 0
        && review.resurfaced.is_empty()
        && review.waiting_proposals == 0
    {
        // An empty review is not worth a message; the week is still marked as done.
        tx.commit().await?;
        return Ok(false);
    }
    outbox::enqueue(
        &mut tx,
        &ConversationKey::new(primary, home),
        NewAction::Send {
            reply_to: None,
            parts: vec![OutPart::Text { text: note_text }],
            buttons: None,
            silent: false,
        },
        Class::Notification,
        None,
        now.with_timezone(&Utc),
    )
    .await?;
    tx.commit().await?;
    outbox::wake(state, primary);

    tracing::info!(
        week = %week,
        created = review.created.len(),
        unlinked = review.unlinked_total,
        resurfaced = review.resurfaced.len(),
        waiting_proposals = review.waiting_proposals,
        "weekly review sent"
    );
    Ok(true)
}

/// Looks at the clock every five minutes (the first look is at startup) and sends the review when
/// it is due. Spawned once by the daemon.
pub async fn run_weekly_loop(state: AppState) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if let Err(error) = send_weekly_if_due(&state, Local::now()).await {
            tracing::warn!(%error, "the weekly review could not be sent");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;

    use super::*;
    use crate::channels::registry::ChannelConfig;
    use crate::channels::{ChannelsRuntime, routing as channel_routing, testkit};

    const HOME: &str = "-100";

    fn local(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("an unambiguous local time")
    }

    /// Monday 2026-11-09 is the start of ISO week 46, clear of every daylight-saving change.
    fn wednesday() -> DateTime<Local> {
        local(2026, 11, 11, 12, 0)
    }

    /// A state whose primary is `telegram`, configured, with a home when `with_home`.
    async fn ready(with_home: bool) -> AppState {
        let mut state = testkit::state().await;
        let mut configs = HashMap::new();
        configs.insert(
            "telegram",
            ChannelConfig {
                doctrine: None,
                projects: BTreeMap::new(),
                stream_edits: None,
            },
        );
        state.channels = Arc::new(ChannelsRuntime::with_configs(configs));
        state
            .channels
            .presence
            .set_secrets_present("telegram", Vec::new());
        sqlx::query("UPDATE channels_global SET primary_channel = 'telegram'")
            .execute(&state.pool)
            .await
            .unwrap();
        if with_home {
            sqlx::query(
                "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
                 conversation_kind, home, created_at) VALUES ('telegram', '42', ?, 'group', 1, 't')",
            )
            .bind(HOME)
            .execute(&state.pool)
            .await
            .unwrap();
        }
        let mut conn = state.pool.acquire().await.unwrap();
        channel_routing::seed(&mut conn, "telegram").await.unwrap();
        drop(conn);
        state
    }

    async fn note(
        pool: &SqlitePool,
        text: &str,
        state: &str,
        created: DateTime<Local>,
        updated: DateTime<Local>,
    ) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO owner_notes (note_text, origin, state, created_at, updated_at) \
             VALUES (?, 'shell', ?, ?, ?) RETURNING id",
        )
        .bind(text)
        .bind(state)
        .bind(stamp(created))
        .bind(stamp(updated))
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn link(pool: &SqlitePool, note_id: i64) {
        sqlx::query(
            "INSERT INTO owner_note_links (note_id, link_type, target_kind, target_ref, created_at) \
             VALUES (?, 'relates', 'job', '1', 't')",
        )
        .bind(note_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn old_notes(pool: &SqlitePool, count: usize) -> Vec<i64> {
        let long_ago = local(2026, 6, 1, 10, 0);
        let mut ids = Vec::new();
        for n in 0..count {
            ids.push(note(pool, &format!("old {n}"), "active", long_ago, long_ago).await);
        }
        ids
    }

    /// One active note written this week, so the review has something to say.
    async fn seed_note(state: &AppState) {
        let tuesday = local(2026, 11, 10, 10, 0);
        note(&state.pool, "seed", "active", tuesday, tuesday).await;
    }

    async fn outbox_texts(state: &AppState) -> Vec<(String, String)> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT conversation, payload FROM channel_outbox ORDER BY id")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        rows.into_iter()
            .map(|(conversation, payload)| {
                let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
                (
                    conversation,
                    payload["parts"][0]["text"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    async fn marker(state: &AppState) -> Option<String> {
        sent_week(&state.pool).await.unwrap()
    }

    #[test]
    fn the_week_runs_from_monday_midnight_to_the_next_monday() {
        let (start, end) = week_bounds(wednesday());
        assert_eq!(start, local(2026, 11, 9, 0, 0));
        assert_eq!(end, local(2026, 11, 16, 0, 0));

        // Sunday night still belongs to the week that began on Monday the 9th.
        assert_eq!(week_bounds(local(2026, 11, 15, 23, 59)).0, start);
        // Monday midnight opens the next one.
        assert_eq!(week_bounds(local(2026, 11, 16, 0, 0)).0, end);
        assert_eq!(week_bounds(local(2026, 11, 9, 0, 0)).0, start);
    }

    #[test]
    fn the_week_key_is_the_iso_year_and_week() {
        assert_eq!(week_key(wednesday()), "2026-W46");
        // 2027-01-01 is a Friday of ISO week 53 of 2026.
        assert_eq!(week_key(local(2027, 1, 1, 12, 0)), "2026-W53");
    }

    #[test]
    fn the_first_line_is_cut_to_eighty_characters() {
        assert_eq!(first_line("\n  hello  \nsecond"), "hello");
        let long = "x".repeat(200);
        let cut = first_line(&long);
        assert_eq!(cut.chars().count(), 80);
        assert!(cut.ends_with('\u{2026}'));
        assert_eq!(first_line(&"y".repeat(80)).chars().count(), 80);
    }

    #[test]
    fn the_message_has_the_counts_the_lines_and_a_pointer() {
        let mut review = Review {
            week_start: String::new(),
            week_end: String::new(),
            created: Vec::new(),
            unlinked: Vec::new(),
            unlinked_total: 25,
            resurfaced: Vec::new(),
            waiting_proposals: 4,
        };
        let plain = weekly_message(&review);
        assert!(plain.contains("Proposals waiting for you: 4"));
        // The count is the total, not the length of the capped list.
        assert!(plain.contains("Notes without a link: 25"));
        assert!(plain.contains("Brain page"));
        assert!(!plain.contains("Worth another look"));

        review.resurfaced.push(OwnerNote {
            id: 1,
            note_text: "first line\nsecond line".into(),
            origin: "shell".into(),
            state: "active".into(),
            created_at: "t".into(),
            updated_at: "t".into(),
        });
        let message = weekly_message(&review);
        assert!(message.contains("Worth another look:\n- first line"));
        assert!(!message.contains("second line"));
    }

    #[tokio::test]
    async fn the_review_counts_the_week_the_unlinked_the_old_and_the_proposals() {
        let state = testkit::state().await;
        let pool = state.pool.clone();
        let tuesday = local(2026, 11, 10, 10, 0);
        let recent = local(2026, 11, 2, 10, 0);
        let long_ago = local(2026, 6, 1, 10, 0);

        let this_week = note(&pool, "this week", "active", tuesday, tuesday).await;
        let last_week = note(&pool, "last week", "active", recent, recent).await;
        let next_week = note(
            &pool,
            "next week",
            "active",
            local(2026, 11, 16, 0, 0),
            tuesday,
        )
        .await;
        let archived = note(&pool, "archived", "archived", tuesday, tuesday).await;
        let linked = note(&pool, "linked", "active", long_ago, long_ago).await;
        link(&pool, linked).await;
        let old = note(&pool, "old", "active", long_ago, long_ago).await;
        // Written long ago but edited this month: not resurfaced.
        let edited = note(&pool, "edited", "active", long_ago, recent).await;
        let archived_old = note(&pool, "archived old", "archived", long_ago, long_ago).await;

        sqlx::query(
            "INSERT INTO knowledge (layer, scope_kind, source, kind, title, body, status, created_at) \
             VALUES ('semantic', 'machine', 'owner', 'fact', 't', 'b', ?, 't')",
        )
        .bind("proposed")
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO knowledge (layer, scope_kind, source, kind, title, body, status, created_at) \
             VALUES ('semantic', 'machine', 'owner', 'fact', 't', 'b', ?, 't')",
        )
        .bind("active")
        .execute(&pool)
        .await
        .unwrap();

        let review = weekly_review(&pool, wednesday()).await.unwrap();
        let ids = |notes: &[OwnerNote]| notes.iter().map(|n| n.id).collect::<Vec<_>>();

        assert_eq!(ids(&review.created), vec![this_week]);
        assert!(!ids(&review.created).contains(&archived));
        assert!(!ids(&review.created).contains(&next_week));

        let unlinked = ids(&review.unlinked);
        assert!(unlinked.contains(&this_week) && unlinked.contains(&last_week));
        assert!(unlinked.contains(&old) && unlinked.contains(&edited));
        assert!(!unlinked.contains(&linked));
        assert!(!unlinked.contains(&archived) && !unlinked.contains(&archived_old));

        let mut resurfaced = ids(&review.resurfaced);
        resurfaced.sort_unstable();
        assert_eq!(resurfaced, vec![linked, old]);

        assert_eq!(review.waiting_proposals, 1);
        let (start, end) = week_bounds(wednesday());
        assert_eq!(
            review.week_start,
            start.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
        );
        assert_eq!(
            review.week_end,
            end.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
        );
    }

    #[tokio::test]
    async fn unlinked_notes_stop_at_ten() {
        let state = testkit::state().await;
        let pool = state.pool.clone();
        let tuesday = local(2026, 11, 10, 10, 0);
        for n in 0..14 {
            note(&pool, &format!("n{n}"), "active", tuesday, tuesday).await;
        }
        let review = weekly_review(&pool, wednesday()).await.unwrap();
        assert_eq!(review.unlinked.len(), 10);
        assert_eq!(review.unlinked_total, 14);
        assert_eq!(review.created.len(), 14);
    }

    #[tokio::test]
    async fn the_sent_message_counts_every_unlinked_note_not_just_the_listed_ten() {
        let state = ready(true).await;
        let tuesday = local(2026, 11, 10, 10, 0);
        for n in 0..14 {
            note(&state.pool, &format!("n{n}"), "active", tuesday, tuesday).await;
        }
        assert!(send_weekly_if_due(&state, wednesday()).await.unwrap());
        let sent = outbox_texts(&state).await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].1.contains("Notes without a link: 14"));
    }

    #[tokio::test]
    async fn two_calls_in_one_week_enqueue_one_row() {
        let state = ready(true).await;
        seed_note(&state).await;

        let first = send_weekly_if_due(&state, wednesday()).await.unwrap();
        let second = send_weekly_if_due(&state, wednesday()).await.unwrap();
        assert!(first);
        assert!(!second);
        assert_eq!(outbox_texts(&state).await.len(), 1);
        assert_eq!(marker(&state).await.as_deref(), Some("2026-W46"));
    }

    #[tokio::test]
    async fn a_taken_marker_enqueues_nothing_and_stays_untouched() {
        // Another daemon got there first: the marker already names this week, so a call sends
        // nothing and leaves it as it was. (The in-transaction claim itself, which closes the
        // window after the early read, is the same statement the first call of the test above runs.)
        let state = ready(true).await;
        seed_note(&state).await;
        sqlx::query("INSERT INTO schema_meta (key, value) VALUES (?, '2026-W46')")
            .bind(SENT_WEEK_KEY)
            .execute(&state.pool)
            .await
            .unwrap();
        assert!(!send_weekly_if_due(&state, wednesday()).await.unwrap());
        assert!(outbox_texts(&state).await.is_empty());
        assert_eq!(marker(&state).await.as_deref(), Some("2026-W46"));
    }

    #[tokio::test]
    async fn an_empty_week_writes_the_marker_and_sends_nothing() {
        let state = ready(true).await;
        assert!(!send_weekly_if_due(&state, wednesday()).await.unwrap());
        assert!(outbox_texts(&state).await.is_empty());
        assert_eq!(marker(&state).await.as_deref(), Some("2026-W46"));

        // A note written afterwards waits for the next week's review.
        seed_note(&state).await;
        assert!(
            !send_weekly_if_due(&state, local(2026, 11, 13, 10, 0))
                .await
                .unwrap()
        );
        assert!(outbox_texts(&state).await.is_empty());
        assert!(
            send_weekly_if_due(&state, local(2026, 11, 16, 9, 0))
                .await
                .unwrap()
        );
        assert_eq!(outbox_texts(&state).await.len(), 1);
    }

    #[tokio::test]
    async fn the_resurfaced_notes_are_three_and_stable_within_a_week() {
        let state = testkit::state().await;
        let pool = state.pool.clone();
        old_notes(&pool, 9).await;

        let ids = |review: &Review| review.resurfaced.iter().map(|n| n.id).collect::<Vec<_>>();
        let monday = weekly_review(&pool, local(2026, 11, 9, 0, 0))
            .await
            .unwrap();
        let sunday = weekly_review(&pool, local(2026, 11, 15, 23, 59))
            .await
            .unwrap();
        assert_eq!(monday.resurfaced.len(), 3);
        assert_eq!(ids(&monday), ids(&sunday));

        // Other weeks pick other notes, or the pick would not be a rotation at all.
        let mut picks = vec![ids(&monday)];
        for day in [16, 23, 30] {
            let later = weekly_review(&pool, local(2026, 11, day, 12, 0))
                .await
                .unwrap();
            picks.push(ids(&later));
        }
        picks.sort();
        picks.dedup();
        assert!(picks.len() > 1, "four weeks in a row picked the same notes");
    }

    #[tokio::test]
    async fn nothing_is_sent_before_monday_nine() {
        let state = ready(true).await;
        seed_note(&state).await;
        assert!(
            !send_weekly_if_due(&state, local(2026, 11, 9, 8, 59))
                .await
                .unwrap()
        );
        assert!(outbox_texts(&state).await.is_empty());
        assert_eq!(marker(&state).await, None);

        assert!(
            send_weekly_if_due(&state, local(2026, 11, 9, 9, 0))
                .await
                .unwrap()
        );
        assert_eq!(outbox_texts(&state).await.len(), 1);
    }

    #[tokio::test]
    async fn the_review_goes_once_per_week_to_the_home_conversation() {
        let state = ready(true).await;
        old_notes(&state.pool, 1).await;

        assert!(
            send_weekly_if_due(&state, local(2026, 11, 9, 9, 5))
                .await
                .unwrap()
        );
        let sent = outbox_texts(&state).await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, HOME);
        assert!(sent[0].1.contains("Your weekly review"));
        assert!(sent[0].1.contains("- old 0"));
        assert_eq!(marker(&state).await.as_deref(), Some("2026-W46"));

        // The same week, later: nothing more.
        assert!(
            !send_weekly_if_due(&state, local(2026, 11, 9, 9, 10))
                .await
                .unwrap()
        );
        assert!(!send_weekly_if_due(&state, wednesday()).await.unwrap());
        assert!(
            !send_weekly_if_due(&state, local(2026, 11, 15, 23, 59))
                .await
                .unwrap()
        );
        assert_eq!(outbox_texts(&state).await.len(), 1);

        // The next week's Monday sends again.
        assert!(
            send_weekly_if_due(&state, local(2026, 11, 16, 9, 0))
                .await
                .unwrap()
        );
        assert_eq!(outbox_texts(&state).await.len(), 2);
        assert_eq!(marker(&state).await.as_deref(), Some("2026-W47"));
    }

    #[tokio::test]
    async fn a_restart_does_not_resend() {
        let state = ready(true).await;
        seed_note(&state).await;
        assert!(send_weekly_if_due(&state, wednesday()).await.unwrap());

        // A restarted daemon has the same database and nothing else: the marker alone decides.
        let restarted = state.clone();
        assert!(!send_weekly_if_due(&restarted, wednesday()).await.unwrap());
        assert!(
            !send_weekly_if_due(&restarted, local(2026, 11, 12, 8, 0))
                .await
                .unwrap()
        );
        assert_eq!(outbox_texts(&state).await.len(), 1);
    }

    #[tokio::test]
    async fn no_channel_means_no_message_and_no_marker() {
        // No primary at all.
        let bare = testkit::state().await;
        assert!(!send_weekly_if_due(&bare, wednesday()).await.unwrap());
        assert_eq!(marker(&bare).await, None);
        assert!(outbox_texts(&bare).await.is_empty());

        // A primary without a home conversation.
        let homeless = ready(false).await;
        assert!(!send_weekly_if_due(&homeless, wednesday()).await.unwrap());
        assert_eq!(marker(&homeless).await, None);
        assert!(outbox_texts(&homeless).await.is_empty());
    }

    #[tokio::test]
    async fn a_home_that_appears_later_gets_the_review_that_week() {
        let state = ready(false).await;
        seed_note(&state).await;
        assert!(!send_weekly_if_due(&state, wednesday()).await.unwrap());

        sqlx::query(
            "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
             conversation_kind, home, created_at) VALUES ('telegram', '42', ?, 'group', 1, 't')",
        )
        .bind(HOME)
        .execute(&state.pool)
        .await
        .unwrap();
        assert!(
            send_weekly_if_due(&state, local(2026, 11, 12, 10, 0))
                .await
                .unwrap()
        );
        assert_eq!(outbox_texts(&state).await.len(), 1);
    }
}
