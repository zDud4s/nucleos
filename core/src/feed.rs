use serde::Serialize;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct FeedEntry {
    pub id: i64,
    pub project_id: Option<String>,
    pub kind: String,
    pub summary: String,
    pub run_id: Option<i64>,
    /// What this line is about, as a [`Subject::key`] — `job:57`, `run:900598` — or `None`.
    ///
    /// Serialized as `null` rather than omitted: a reader grouping lines into stories has to tell
    /// "this line has no subject" from "this daemon predates subjects", and only an explicit `null`
    /// beside the other fields says the first.
    pub subject: Option<String>,
    pub created_at: String,
}

/// The thing a feed line is about, so lines about the same thing can be read together.
///
/// An enum rather than a free string at the call sites, because the key is a contract with a reader
/// that groups on it by exact match: `job:57` and `Job:57` are two stories, and a typo in one writer
/// would split a job in half with nothing failing. The vocabulary lives here, spelled once.
///
/// The ids are each family's own primary key, never a number read out of a summary. Councils and
/// team runs are keyed by UUID, which is why those two carry a `String`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Job(i64),
    Run(i64),
    Council(String),
    TeamRun(String),
    Vcs(i64),
}

impl Subject {
    /// `<kind>:<id>`, lowercase — the spelling the `subject` column stores.
    pub fn key(&self) -> String {
        match self {
            Subject::Job(id) => format!("job:{id}"),
            Subject::Run(id) => format!("run:{id}"),
            Subject::Council(id) => format!("council:{id}"),
            Subject::TeamRun(id) => format!("team_run:{id}"),
            Subject::Vcs(id) => format!("vcs:{id}"),
        }
    }
}

/// The subject of a line about run `run_id`: the job or team run it is a node of, else the run.
///
/// A job's item runs are chapters of the job's story, not stories of their own — "run 900598 failed
/// its gate" is read as "job 57's item failed its gate", and keying it by the run would put it in a
/// row nobody opened. The same holds for a department's runs and their team run.
///
/// Never an error. A feed line is best-effort, and a lookup that fails (or finds no row) still has
/// an exact answer to give: the run itself. That can put a job's line under its run, which the
/// reader shows as a row of one; refusing to write the line would show nothing.
pub async fn run_subject(pool: &sqlx::SqlitePool, run_id: i64) -> Subject {
    let owner: Option<(Option<i64>, Option<String>)> =
        sqlx::query_as("SELECT job_id, team_run_id FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    match owner {
        Some((Some(job_id), _)) => Subject::Job(job_id),
        Some((None, Some(team_run_id))) => Subject::TeamRun(team_run_id),
        _ => Subject::Run(run_id),
    }
}

/// The feed scope to search.
///
/// `Global` means the machine's own lines and nobody else's — `project_id IS NULL`. It is
/// deliberately not a merge of every scope, which is what `All` is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedScope {
    All,
    Global,
    Project(String),
}

#[derive(Debug, Clone)]
pub struct SearchFilter {
    pub scope: FeedScope,
    pub q: Option<String>,
    pub kind: Option<String>,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    pub limit: i64,
}

use crate::search::escape_like;

/// Writes one line.
///
/// `subject` is a required argument, not a sibling entry point, although most call sites pass
/// `None`: the question "what is this line about" has to be answered by every writer, and a
/// parameter is what makes a new writer answer it; a subject left out by accident is a line the
/// replay cannot group.
pub async fn append(
    pool: &sqlx::SqlitePool,
    project_id: Option<&str>,
    kind: &str,
    summary: &str,
    run_id: Option<i64>,
    subject: Option<&Subject>,
) -> sqlx::Result<i64> {
    append_on(
        pool.acquire().await?.as_mut(),
        project_id,
        kind,
        summary,
        run_id,
        subject,
    )
    .await
}

/// The same append against a caller-supplied connection, so a writer that must be atomic with its
/// feed entry can run both inside one transaction. Feed SQL stays in this module either way.
pub async fn append_on(
    conn: &mut sqlx::SqliteConnection,
    project_id: Option<&str>,
    kind: &str,
    summary: &str,
    run_id: Option<i64>,
    subject: Option<&Subject>,
) -> sqlx::Result<i64> {
    let created_at = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO feed (project_id, kind, summary, run_id, subject, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(kind)
    .bind(summary)
    .bind(run_id)
    .bind(subject.map(Subject::key))
    .bind(created_at)
    .execute(conn)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Returns one feed scope newest-first. `None` selects the machine's own rows (`project_id IS NULL`),
/// not a merge of every project's feed.
pub async fn list_feed(
    pool: &sqlx::SqlitePool,
    project_id: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<FeedEntry>> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, FeedEntry>(
                "SELECT id, project_id, kind, summary, run_id, subject, created_at
                 FROM feed WHERE project_id = ? ORDER BY id DESC LIMIT ?",
            )
            .bind(project_id)
            .bind(limit)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, FeedEntry>(
                "SELECT id, project_id, kind, summary, run_id, subject, created_at
                 FROM feed WHERE project_id IS NULL
                 ORDER BY id DESC LIMIT ?",
            )
            .bind(limit)
            .fetch_all(pool)
            .await
        }
    }
}

/// The newest `kind` row for `run_id` whose summary starts with `summary_prefix`, or `None`.
///
/// For a writer that reports the same condition on every pass and needs to ask what it said last
/// time. It exists because a feed row is a notification the Telegram sidecar forwards unread, so
/// "don't repeat yourself" has to be decided before the row is written, not filtered afterwards.
pub async fn latest_summary(
    pool: &sqlx::SqlitePool,
    kind: &str,
    run_id: Option<i64>,
    summary_prefix: &str,
) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT summary FROM feed
         WHERE kind = ? AND run_id IS ? AND summary LIKE ? ESCAPE '\\'
         ORDER BY id DESC LIMIT 1",
    )
    .bind(kind)
    .bind(run_id)
    .bind(format!("{}%", escape_like(summary_prefix)))
    .fetch_optional(pool)
    .await
}

/// Aggregated feed across EVERY scope (global NULL rows + all projects), newest-first, honoring `limit`.
/// Distinct from `list_feed(None)`, which returns only global (`project_id IS NULL`) rows.
pub async fn list_all(pool: &sqlx::SqlitePool, limit: i64) -> sqlx::Result<Vec<FeedEntry>> {
    sqlx::query_as::<_, FeedEntry>(
        "SELECT id, project_id, kind, summary, run_id, subject, created_at
         FROM feed ORDER BY id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Searches feed entries newest-first without changing the meaning of the global scope.
pub async fn search(
    pool: &sqlx::SqlitePool,
    filter: &SearchFilter,
) -> sqlx::Result<Vec<FeedEntry>> {
    let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT id, project_id, kind, summary, run_id, subject, created_at FROM feed WHERE 1 = 1",
    );

    match &filter.scope {
        FeedScope::All => {}
        FeedScope::Global => {
            query.push(" AND project_id IS NULL");
        }
        FeedScope::Project(project_id) => {
            query.push(" AND project_id = ").push_bind(project_id);
        }
    }
    if let Some(kind) = &filter.kind {
        query.push(" AND kind = ").push_bind(kind);
    }
    if let Some(q) = &filter.q {
        // Both, joined by OR, exactly as `runs::search` does it. The index answers by word, which is
        // what makes an entry findable without recalling its phrasing; LIKE answers by substring,
        // which is what still finds `nucleos-core` for somebody who types `leos-co`. Dropping either
        // loses searches the other cannot do.
        let fts = crate::search::fts_query(q);
        query
            .push(" AND (summary LIKE ")
            .push_bind(format!("%{}%", escape_like(q)))
            .push(" ESCAPE '\\'");
        if fts.is_empty() {
            // `MATCH ''` is an error rather than an empty result, so a query with no searchable
            // words has to contribute a clause that is merely false.
            query.push(" OR 0");
        } else {
            query
                .push(" OR id IN (SELECT rowid FROM feed_fts WHERE feed_fts MATCH ")
                .push_bind(fts)
                .push(")");
        }
        query.push(")");
    }
    if let Some(since) = &filter.since {
        query
            .push(" AND created_at >= ")
            .push_bind(since.to_rfc3339());
    }
    if let Some(until) = &filter.until {
        query
            .push(" AND created_at <= ")
            .push_bind(until.to_rfc3339());
    }
    query
        .push(" ORDER BY created_at DESC, id DESC LIMIT ")
        .push_bind(filter.limit);

    query.build_query_as::<FeedEntry>().fetch_all(pool).await
}

/// The most lines one timeline answer carries.
///
/// A ceiling rather than a page size: the reader asks for a window of time, not for N lines, and a
/// normal day is a few hundred. Five thousand is a day on which something looped — and on that day
/// the answer still arrives, cut and saying so, instead of a month of a runaway writer serialized
/// into one response.
pub const TIMELINE_MAX: i64 = 5000;

/// The widest window a timeline may ask for, in days.
///
/// A month is the widest view the page offers. Without a bound, `since=1970` is a full scan of
/// ninety days of feed ordered in memory, and the cap above would only decide how much of it is
/// thrown away afterwards.
pub const TIMELINE_WINDOW_MAX_DAYS: i64 = 31;

/// One window of the feed, oldest first.
#[derive(Debug, Serialize)]
pub struct Timeline {
    pub entries: Vec<FeedEntry>,
    /// More lines matched than [`TIMELINE_MAX`]; `entries` is the newest of them.
    pub truncated: bool,
}

/// Why a window is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowError {
    UntilBeforeSince,
    TooWide,
}

/// Whether `since..until` (or `since..now`, open-ended) is a window a timeline answers.
///
/// Pure, and separate from [`timeline`], so the refusal is decided before any SQL runs and can be
/// tested at the boundary without a clock: exactly [`TIMELINE_WINDOW_MAX_DAYS`] is inside.
pub fn timeline_window(
    since: chrono::DateTime<chrono::Utc>,
    until: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), WindowError> {
    if let Some(until) = until
        && until < since
    {
        return Err(WindowError::UntilBeforeSince);
    }
    if until.unwrap_or(now) - since > chrono::Duration::days(TIMELINE_WINDOW_MAX_DAYS) {
        return Err(WindowError::TooWide);
    }
    Ok(())
}

/// Every scope's lines with `since <= created_at <= until` and `id > after_id`, oldest first.
///
/// Across every owner — machine and project — because this is the page that shows the day
/// whole; the scoped readers above are still where a single owner's lines come from.
///
/// When more than `cap` match it is the NEWEST `cap` that come back, still ascending, with
/// `truncated` set. That is why the query reads newest-first and the result is reversed here: the
/// reader opened the page to see what just happened, so if something has to be dropped it is the
/// start of the window. Asking for one row past the cap is how "exactly the cap" and "more than
/// the cap" are told apart without a second COUNT query.
///
/// Bounds go through `to_rfc3339` for the reason `search` does: `created_at` is compared as TEXT,
/// and a bound spelled `Z` against rows spelled `+00:00` is wrong for every row in its own second.
pub async fn timeline(
    pool: &sqlx::SqlitePool,
    since: chrono::DateTime<chrono::Utc>,
    until: Option<chrono::DateTime<chrono::Utc>>,
    after_id: Option<i64>,
    cap: i64,
) -> sqlx::Result<Timeline> {
    let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT id, project_id, kind, summary, run_id, subject, created_at FROM feed
         WHERE created_at >= ",
    );
    query.push_bind(since.to_rfc3339());
    if let Some(until) = until {
        query
            .push(" AND created_at <= ")
            .push_bind(until.to_rfc3339());
    }
    if let Some(after_id) = after_id {
        query.push(" AND id > ").push_bind(after_id);
    }
    query
        .push(" ORDER BY created_at DESC, id DESC LIMIT ")
        .push_bind(cap.saturating_add(1));

    let mut entries = query.build_query_as::<FeedEntry>().fetch_all(pool).await?;
    let truncated = entries.len() as i64 > cap;
    entries.truncate(usize::try_from(cap).unwrap_or(0));
    entries.reverse();
    Ok(Timeline { entries, truncated })
}

/// Where the owner stopped reading the feed.
///
/// `through` is a feed id; `through_created_at` is the time it stands for, read from the nearest line
/// at or below it rather than from that exact id, because the line itself may have been pruned — and
/// `None` when every line that far back is gone. All three are `None` before anything was marked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Seen {
    pub through: Option<i64>,
    pub through_created_at: Option<String>,
    pub seen_at: Option<String>,
}

/// The read marker as it stands.
pub async fn seen(pool: &sqlx::SqlitePool) -> sqlx::Result<Seen> {
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT through, seen_at FROM feed_seen WHERE scope = 'global'")
            .fetch_optional(pool)
            .await?;
    let Some((through, seen_at)) = row else {
        return Ok(Seen {
            through: None,
            through_created_at: None,
            seen_at: None,
        });
    };
    let through_created_at =
        sqlx::query_scalar("SELECT created_at FROM feed WHERE id <= ? ORDER BY id DESC LIMIT 1")
            .bind(through)
            .fetch_optional(pool)
            .await?;
    Ok(Seen {
        through: Some(through),
        through_created_at,
        seen_at: Some(seen_at),
    })
}

/// Moves the read marker to `through`, forward only, and never past the newest line.
///
/// Forward only because two windows can be open on the same feed, and the one that read less must
/// not unread what the other read. Never past the newest line because a marker ahead of the feed
/// would pre-read lines that do not exist yet — the next ones written would arrive already seen.
/// On an empty feed that clamp is 0, which is below every id AUTOINCREMENT will ever hand out.
///
/// One statement, so two windows marking at once cannot interleave a read and a write and lose the
/// higher of the two. `seen_at` moves on every call, including one that leaves `through` where it
/// was: it records when somebody last looked, not what they found. A negative `through` is the
/// caller's to refuse; here it simply cannot lower anything.
pub async fn mark_seen(
    pool: &sqlx::SqlitePool,
    through: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<Seen> {
    sqlx::query(
        "INSERT INTO feed_seen (scope, through, seen_at)
         VALUES ('global', MAX(0, MIN(?, COALESCE((SELECT MAX(id) FROM feed), 0))), ?)
         ON CONFLICT (scope) DO UPDATE SET
             through = MAX(feed_seen.through, excluded.through),
             seen_at = excluded.seen_at",
    )
    .bind(through)
    .bind(now.to_rfc3339())
    .execute(pool)
    .await?;
    seen(pool).await
}

/// How long an activity entry stays readable.
///
/// Longer than a run keeps its transcript, deliberately: an entry is one short line, and "what was
/// this daemon doing in June" is a question people actually ask. It still needs a bound — every run,
/// every gate, every worktree collected and every startup recovery writes one, for ever.
pub const DEFAULT_RETENTION_DAYS: i64 = 90;

/// The window, overridable the same way `runs` and `worktree` allow theirs to be.
pub fn retention_days() -> i64 {
    std::env::var("NUCLEOS_FEED_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_RETENTION_DAYS)
}

/// Removes activity entries past the window. Returns how many went.
pub async fn prune(
    pool: &sqlx::SqlitePool,
    retain_days: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<u64> {
    if retain_days <= 0 {
        return Ok(0);
    }
    // RFC 3339 in Rust rather than SQLite's `datetime()`, for the reason `web::prune` sets out at
    // length: the two spellings are compared as TEXT and disagree inside the cutoff's own day.
    let cutoff = (now - chrono::Duration::days(retain_days)).to_rfc3339();
    let result = sqlx::query("DELETE FROM feed WHERE created_at < ?")
        .bind(&cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::{
        FeedScope, SearchFilter, Seen, Subject, TIMELINE_MAX, Timeline, WindowError, append,
        latest_summary, list_all, list_feed, mark_seen, prune, run_subject, search, seen, timeline,
        timeline_window,
    };

    /// A search that filters on nothing but the scope, so a scope test is about the scope.
    fn scoped(scope: FeedScope) -> SearchFilter {
        SearchFilter {
            scope,
            q: None,
            kind: None,
            since: None,
            until: None,
            limit: 50,
        }
    }

    fn query(q: &str) -> SearchFilter {
        SearchFilter {
            scope: FeedScope::All,
            q: Some(q.to_string()),
            kind: None,
            since: None,
            until: None,
            limit: 50,
        }
    }

    /// What the index buys over the substring match that was here alone: a word out of the middle,
    /// in any order, without recalling how the line was phrased.
    #[tokio::test]
    async fn an_entry_is_found_by_its_words_in_any_order() {
        let pool = test_pool().await;
        append(
            &pool,
            None,
            "run_completed",
            "worktree collected for nucleos-core",
            None,
            None,
        )
        .await
        .unwrap();

        for q in ["collected", "nucleos-core collected", "COLLECTED"] {
            assert_eq!(
                search(&pool, &query(q)).await.unwrap().len(),
                1,
                "{q:?} found nothing"
            );
        }
        assert!(search(&pool, &query("absent")).await.unwrap().is_empty());
    }

    /// Substring search has to survive the index arriving beside it: `leos-co` is not a word and
    /// FTS5 will never match it, which is exactly why the OR keeps LIKE in the query.
    #[tokio::test]
    async fn a_mid_word_fragment_still_matches() {
        let pool = test_pool().await;
        append(
            &pool,
            None,
            "run_completed",
            "built nucleos-core",
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(search(&pool, &query("leos-co")).await.unwrap().len(), 1);
    }

    /// The reason this migration carries a delete trigger at all. `feed::prune` runs hourly, and an
    /// index that keeps the terms of a pruned entry is the bug the previous commit on this branch
    /// was written to fix, one table over.
    #[tokio::test]
    async fn a_pruned_entry_leaves_the_index_with_it() {
        let pool = test_pool().await;
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-08T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        sqlx::query(
            "INSERT INTO feed (project_id, kind, summary, run_id, created_at)
             VALUES (NULL, 'run_completed', 'aardvark ate the invoice', NULL, '2026-01-01T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(search(&pool, &query("aardvark")).await.unwrap().len(), 1);
        assert_eq!(prune(&pool, 90, now).await.unwrap(), 1);

        let indexed: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feed_fts WHERE feed_fts MATCH 'aardvark'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            indexed, 0,
            "the terms of a pruned entry stayed searchable in the index"
        );
    }

    /// A query of nothing but punctuation reaches `MATCH` as the empty string, which errors instead
    /// of returning nothing. The `OR 0` branch is what keeps that a query rather than a 500.
    #[tokio::test]
    async fn a_query_with_no_searchable_words_is_answered_not_refused() {
        let pool = test_pool().await;
        append(
            &pool,
            None,
            "run_completed",
            "something happened",
            None,
            None,
        )
        .await
        .unwrap();

        assert!(search(&pool, &query("\"\"\"")).await.unwrap().is_empty());
    }

    /// Entries past the window go; the rest stay, and a second pass finds nothing left to do.
    #[tokio::test]
    async fn feed_entries_past_the_window_go_and_the_rest_stay() {
        let pool = test_pool().await;
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-08T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        for created_at in [
            // Two the wrong side of a 90-day cutoff, and one a second inside it. The boundary case
            // is the one that matters: `created_at` is RFC 3339 and SQLite's `datetime()` is not,
            // and comparing the two as TEXT spares a day's worth of rows on every sweep for ever.
            "2026-01-01T00:00:00+00:00",
            "2026-05-10T11:59:59+00:00",
            "2026-05-10T12:00:01+00:00",
        ] {
            sqlx::query(
                "INSERT INTO feed (project_id, kind, summary, run_id, created_at)
                 VALUES (NULL, 'run_completed', 'something happened', NULL, ?)",
            )
            .bind(created_at)
            .execute(&pool)
            .await
            .unwrap();
        }

        assert_eq!(prune(&pool, 90, now).await.unwrap(), 2);
        assert_eq!(list_all(&pool, 100).await.unwrap().len(), 1);
        assert_eq!(
            prune(&pool, 90, now).await.unwrap(),
            0,
            "a sweep with nothing to do must say so, or the log reports work every hour for ever"
        );
    }

    /// Zero is not a retention policy, it is a typo that empties the whole activity log.
    #[tokio::test]
    async fn a_zero_or_negative_feed_window_prunes_nothing() {
        let pool = test_pool().await;
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-08T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        append(&pool, None, "run_completed", "ancient", None, None)
            .await
            .unwrap();
        sqlx::query("UPDATE feed SET created_at = '2000-01-01T00:00:00+00:00'")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(prune(&pool, 0, now).await.unwrap(), 0);
        assert_eq!(prune(&pool, -1, now).await.unwrap(), 0);
        assert_eq!(list_all(&pool, 100).await.unwrap().len(), 1);
    }

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::storage::MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn append_and_list_round_trip() {
        let pool = test_pool().await;

        let id = append(
            &pool,
            Some("project-a"),
            "run_interrupted",
            "run interrupted during startup recovery",
            Some(7),
            None,
        )
        .await
        .unwrap();

        let entries = list_feed(&pool, Some("project-a"), 50).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, id);
        assert_eq!(entries[0].project_id.as_deref(), Some("project-a"));
        assert_eq!(entries[0].kind, "run_interrupted");
        assert_eq!(
            entries[0].summary,
            "run interrupted during startup recovery"
        );
        assert_eq!(entries[0].run_id, Some(7));
        chrono::DateTime::parse_from_rfc3339(&entries[0].created_at).unwrap();
    }

    /// The key is the contract the replay groups on, so its spelling is pinned here family by
    /// family: lowercase kind, a colon, the family's own id.
    #[test]
    fn every_subject_is_spelled_kind_colon_id() {
        let keys: Vec<String> = [
            Subject::Job(57),
            Subject::Run(900598),
            Subject::Council("c-1".into()),
            Subject::TeamRun("t-1".into()),
            Subject::Vcs(21),
        ]
        .iter()
        .map(Subject::key)
        .collect();
        assert_eq!(
            keys,
            [
                "job:57",
                "run:900598",
                "council:c-1",
                "team_run:t-1",
                "vcs:21"
            ]
        );
    }

    /// Written, read back by every reader, and serialized as a string — or as an explicit `null`
    /// when the writer had none, never as a missing field.
    #[tokio::test]
    async fn a_subject_is_stored_read_back_and_serialized_null_when_absent() {
        let pool = test_pool().await;
        let about = append(
            &pool,
            Some("p"),
            "job_started",
            "job 57 started",
            None,
            Some(&Subject::Job(57)),
        )
        .await
        .unwrap();
        let bare = append(&pool, None, "config_written", "written", None, None)
            .await
            .unwrap();

        let entries = list_all(&pool, 50).await.unwrap();
        let by_id = |id: i64| entries.iter().find(|entry| entry.id == id).unwrap();
        assert_eq!(by_id(about).subject.as_deref(), Some("job:57"));
        assert_eq!(by_id(bare).subject, None);
        assert_eq!(
            list_feed(&pool, Some("p"), 50).await.unwrap()[0]
                .subject
                .as_deref(),
            Some("job:57")
        );
        assert_eq!(
            search(&pool, &scoped(FeedScope::All)).await.unwrap()[1]
                .subject
                .as_deref(),
            Some("job:57")
        );

        let about_json = serde_json::to_value(by_id(about)).unwrap();
        assert_eq!(about_json["subject"], "job:57");
        let bare_json = serde_json::to_value(by_id(bare)).unwrap();
        assert!(
            bare_json.as_object().unwrap().contains_key("subject"),
            "a line without a subject still carries the field: {bare_json}"
        );
        assert!(bare_json["subject"].is_null());
    }

    /// The timeline is the reader the replay is built on, so it has to carry the key too.
    #[tokio::test]
    async fn a_timeline_carries_each_lines_subject() {
        let pool = test_pool().await;
        let since = chrono::Utc::now() - chrono::Duration::hours(1);
        append(
            &pool,
            None,
            "run_retry",
            "retrying",
            Some(900598),
            Some(&Subject::Run(900598)),
        )
        .await
        .unwrap();
        append(&pool, None, "web.read", "read a page", None, None)
            .await
            .unwrap();
        let found = timeline(&pool, since, None, None, TIMELINE_MAX)
            .await
            .unwrap();
        let subjects: Vec<Option<&str>> = found
            .entries
            .iter()
            .map(|entry| entry.subject.as_deref())
            .collect();
        assert_eq!(subjects, [Some("run:900598"), None]);
    }

    /// A job's node and a department's run are chapters of their owner's story; anything else, and
    /// a run the table does not know, is its own.
    #[tokio::test]
    async fn a_runs_subject_is_its_job_or_team_run_before_itself() {
        let pool = test_pool().await;
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&pool)
            .await
            .unwrap();
        for (id, job_id, team_run_id) in [
            (1_i64, Some(57_i64), None::<&str>),
            (2, None, Some("t-1")),
            (3, None, None),
        ] {
            sqlx::query(
                "INSERT INTO runs (id, cwd, prompt, status, created_at, job_id, team_run_id)
                 VALUES (?, '.', 'p', 'running', '2026-01-01T00:00:00+00:00', ?, ?)",
            )
            .bind(id)
            .bind(job_id)
            .bind(team_run_id)
            .execute(&pool)
            .await
            .unwrap();
        }
        assert_eq!(run_subject(&pool, 1).await, Subject::Job(57));
        assert_eq!(run_subject(&pool, 2).await, Subject::TeamRun("t-1".into()));
        assert_eq!(run_subject(&pool, 3).await, Subject::Run(3));
        assert_eq!(run_subject(&pool, 404).await, Subject::Run(404));
    }

    /// The prefix is a Windows worktree path in practice, so the backslashes are the point: they
    /// must survive the LIKE escaping as literal characters, not turn into the escape itself.
    #[tokio::test]
    async fn latest_summary_matches_a_prefix_with_backslashes_and_ignores_other_runs() {
        let pool = test_pool().await;
        let prefix = r"failed to remove worktree C:\Projects\nucleos-worktrees\run-1:";
        let older = format!("{prefix} first failure");
        let newer = format!("{prefix} second failure");
        append(
            &pool,
            Some("p"),
            "worktree_gc_failed",
            &older,
            Some(1),
            None,
        )
        .await
        .unwrap();
        append(
            &pool,
            Some("p"),
            "worktree_gc_failed",
            &newer,
            Some(1),
            None,
        )
        .await
        .unwrap();
        append(
            &pool,
            Some("p"),
            "worktree_gc_failed",
            "unrelated",
            Some(2),
            None,
        )
        .await
        .unwrap();
        append(&pool, Some("p"), "worktree_removed", &newer, Some(1), None)
            .await
            .unwrap();

        let found = latest_summary(&pool, "worktree_gc_failed", Some(1), prefix)
            .await
            .unwrap();
        assert_eq!(found.as_deref(), Some(newer.as_str()));
        assert_eq!(
            latest_summary(&pool, "worktree_gc_failed", Some(3), prefix)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            latest_summary(&pool, "worktree_gc_failed", None, prefix)
                .await
                .unwrap(),
            None,
            "a NULL run id must not match rows that have one"
        );
    }

    #[tokio::test]
    async fn global_and_project_feeds_are_isolated() {
        let pool = test_pool().await;
        append(&pool, None, "global", "global summary", None, None)
            .await
            .unwrap();
        append(
            &pool,
            Some("project-a"),
            "project",
            "project summary",
            None,
            None,
        )
        .await
        .unwrap();
        append(
            &pool,
            Some("project-b"),
            "project",
            "other project summary",
            None,
            None,
        )
        .await
        .unwrap();

        let global = list_feed(&pool, None, 50).await.unwrap();
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].project_id, None);
        assert_eq!(global[0].summary, "global summary");

        let project = list_feed(&pool, Some("project-a"), 50).await.unwrap();
        assert_eq!(project.len(), 1);
        assert_eq!(project[0].project_id.as_deref(), Some("project-a"));
        assert_eq!(project[0].summary, "project summary");
    }

    /// The aggregate keeps aggregating: `All` still means everything.
    ///
    /// `Global` narrowing and `All` narrowing with it would be the same bug in the other direction
    /// — a machine-wide view that quietly stopped showing a whole class of work.
    #[tokio::test]
    async fn the_aggregate_carries_every_scope() {
        let pool = test_pool().await;
        append(
            &pool,
            None,
            "kill_switch",
            "the stop was released",
            None,
            None,
        )
        .await
        .unwrap();
        append(
            &pool,
            Some("project-a"),
            "run_completed",
            "a run",
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(list_all(&pool, 50).await.unwrap().len(), 2);
        assert_eq!(
            search(&pool, &scoped(FeedScope::All)).await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn list_feed_returns_newest_first_and_honors_limit() {
        let pool = test_pool().await;
        let first = append(&pool, None, "event", "first", None, None)
            .await
            .unwrap();
        let second = append(&pool, None, "event", "second", None, None)
            .await
            .unwrap();
        let third = append(&pool, None, "event", "third", None, None)
            .await
            .unwrap();

        let entries = list_feed(&pool, None, 2).await.unwrap();
        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![third, second]
        );
        assert!(first < second);
    }

    #[tokio::test]
    async fn list_all_merges_every_scope_newest_first() {
        let pool = test_pool().await;
        let global = append(&pool, None, "global", "global summary", None, None)
            .await
            .unwrap();
        let project_a = append(
            &pool,
            Some("project-a"),
            "project",
            "project a summary",
            None,
            None,
        )
        .await
        .unwrap();
        let project_b = append(
            &pool,
            Some("project-b"),
            "project",
            "project b summary",
            None,
            None,
        )
        .await
        .unwrap();

        let entries = list_all(&pool, 50).await.unwrap();
        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![project_b, project_a, global]
        );
    }

    #[tokio::test]
    async fn list_all_honors_limit() {
        let pool = test_pool().await;
        let first = append(&pool, None, "event", "first", None, None)
            .await
            .unwrap();
        let second = append(&pool, Some("project-a"), "event", "second", None, None)
            .await
            .unwrap();
        let third = append(&pool, Some("project-b"), "event", "third", None, None)
            .await
            .unwrap();

        let entries = list_all(&pool, 2).await.unwrap();
        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![third, second]
        );
        assert!(first < second);
    }

    #[tokio::test]
    async fn list_feed_keeps_global_and_project_scopes_isolated() {
        let pool = test_pool().await;
        append(&pool, None, "global", "global summary", None, None)
            .await
            .unwrap();
        append(
            &pool,
            Some("project-a"),
            "project",
            "project a summary",
            None,
            None,
        )
        .await
        .unwrap();
        append(
            &pool,
            Some("project-b"),
            "project",
            "project b summary",
            None,
            None,
        )
        .await
        .unwrap();

        let global = list_feed(&pool, None, 50).await.unwrap();
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].project_id, None);
        assert_eq!(global[0].summary, "global summary");

        let project_a = list_feed(&pool, Some("project-a"), 50).await.unwrap();
        assert_eq!(project_a.len(), 1);
        assert_eq!(project_a[0].project_id.as_deref(), Some("project-a"));
        assert_eq!(project_a[0].summary, "project a summary");
    }

    async fn insert_entry(
        pool: &sqlx::SqlitePool,
        project_id: Option<&str>,
        kind: &str,
        summary: &str,
        created_at: &str,
    ) -> i64 {
        sqlx::query("INSERT INTO feed (project_id, kind, summary, created_at) VALUES (?, ?, ?, ?)")
            .bind(project_id)
            .bind(kind)
            .bind(summary)
            .bind(created_at)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid()
    }

    fn search_filter(scope: FeedScope) -> SearchFilter {
        SearchFilter {
            scope,
            q: None,
            kind: None,
            since: None,
            until: None,
            limit: 50,
        }
    }

    #[tokio::test]
    async fn search_filters_scope_kind_and_summary() {
        let pool = test_pool().await;
        let matching = insert_entry(
            &pool,
            Some("project-a"),
            "worktree_run_completed",
            "Autopilot completed the March worktree run",
            "2026-03-10T12:00:00+00:00",
        )
        .await;
        insert_entry(
            &pool,
            Some("project-a"),
            "shadow_run_completed",
            "Autopilot completed a shadow run",
            "2026-03-11T12:00:00+00:00",
        )
        .await;
        insert_entry(
            &pool,
            Some("project-b"),
            "worktree_run_completed",
            "Autopilot completed the March worktree run",
            "2026-03-12T12:00:00+00:00",
        )
        .await;
        insert_entry(
            &pool,
            None,
            "worktree_run_completed",
            "Autopilot completed the March worktree run",
            "2026-03-13T12:00:00+00:00",
        )
        .await;

        let mut filter = search_filter(FeedScope::Project("project-a".into()));
        filter.kind = Some("worktree_run_completed".into());
        filter.q = Some("March worktree".into());
        let entries = search(&pool, &filter).await.unwrap();

        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            [matching]
        );
    }

    #[tokio::test]
    async fn search_uses_inclusive_normalized_time_bounds_and_orders_newest_first() {
        let pool = test_pool().await;
        insert_entry(
            &pool,
            None,
            "event",
            "before the window",
            "2026-02-28T23:59:59+00:00",
        )
        .await;
        let first = insert_entry(
            &pool,
            None,
            "event",
            "start of window",
            "2026-03-01T00:00:00+00:00",
        )
        .await;
        let second = insert_entry(
            &pool,
            None,
            "event",
            "end of window",
            "2026-03-31T23:59:59+00:00",
        )
        .await;
        insert_entry(
            &pool,
            None,
            "event",
            "after the window",
            "2026-04-01T00:00:00+00:00",
        )
        .await;

        let mut filter = search_filter(FeedScope::Global);
        filter.since = Some(
            chrono::DateTime::parse_from_rfc3339("2026-03-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        filter.until = Some(
            chrono::DateTime::parse_from_rfc3339("2026-03-31T23:59:59Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        let entries = search(&pool, &filter).await.unwrap();

        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            [second, first]
        );
    }

    #[tokio::test]
    async fn search_escapes_like_wildcards_and_honors_limit() {
        let pool = test_pool().await;
        let percent = insert_entry(
            &pool,
            None,
            "event",
            "Autopilot spent 50% of its budget",
            "2026-03-03T00:00:00+00:00",
        )
        .await;
        insert_entry(
            &pool,
            None,
            "event",
            "Autopilot spent half of its budget",
            "2026-03-04T00:00:00+00:00",
        )
        .await;

        let mut filter = search_filter(FeedScope::All);
        filter.q = Some("%".into());
        let entries = search(&pool, &filter).await.unwrap();
        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            [percent]
        );

        filter.q = None;
        filter.limit = 1;
        let entries = search(&pool, &filter).await.unwrap();
        assert_eq!(entries.len(), 1);
    }

    fn at(value: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn ids(timeline: &Timeline) -> Vec<i64> {
        timeline.entries.iter().map(|entry| entry.id).collect()
    }

    /// The window is inclusive at both ends, carries every scope, and reads oldest
    /// first — the opposite of every other reader here, because a day is read from its morning.
    #[tokio::test]
    async fn a_timeline_is_every_scope_inside_the_window_oldest_first() {
        let pool = test_pool().await;
        insert_entry(&pool, None, "event", "before", "2026-03-01T08:59:59+00:00").await;
        // Written in the same instant and out of id order with the line after them: the ordering is
        // `created_at` first, and `id` only breaks a tie.
        let late = insert_entry(&pool, None, "event", "late", "2026-03-01T12:00:00+00:00").await;
        let start = insert_entry(
            &pool,
            Some("project-a"),
            "event",
            "start",
            "2026-03-01T09:00:00+00:00",
        )
        .await;
        let tie = insert_entry(&pool, None, "event", "tie", "2026-03-01T12:00:00+00:00").await;
        let middle =
            insert_entry(&pool, None, "event", "middle", "2026-03-01T10:00:00+00:00").await;
        let end = insert_entry(&pool, None, "event", "end", "2026-03-01T18:00:00+00:00").await;
        insert_entry(&pool, None, "event", "after", "2026-03-01T18:00:01+00:00").await;

        let found = timeline(
            &pool,
            at("2026-03-01T09:00:00Z"),
            Some(at("2026-03-01T18:00:00Z")),
            None,
            TIMELINE_MAX,
        )
        .await
        .unwrap();

        assert_eq!(ids(&found), [start, middle, late, tie, end]);
        assert!(!found.truncated);
    }

    /// No `until` is "up to now", and `after_id` is how a reader that already holds the window
    /// asks only for what arrived since — by id, so a line with a skewed clock is not lost.
    #[tokio::test]
    async fn a_timeline_without_until_runs_to_the_end_and_after_id_skips_what_was_read() {
        let pool = test_pool().await;
        let first = insert_entry(&pool, None, "event", "one", "2026-03-01T09:00:00+00:00").await;
        let second = insert_entry(&pool, None, "event", "two", "2026-03-02T09:00:00+00:00").await;
        let third = insert_entry(&pool, None, "event", "three", "2026-03-03T09:00:00+00:00").await;

        let since = at("2026-03-01T00:00:00Z");
        let all = timeline(&pool, since, None, None, TIMELINE_MAX)
            .await
            .unwrap();
        assert_eq!(ids(&all), [first, second, third]);

        let newer = timeline(&pool, since, None, Some(first), TIMELINE_MAX)
            .await
            .unwrap();
        assert_eq!(ids(&newer), [second, third]);
    }

    /// Past the cap it is the NEWEST lines that come back, still oldest first, and the answer says
    /// it was cut. The morning is what gets dropped because the evening is what somebody opened the
    /// page to see; exactly the cap is not a cut.
    #[tokio::test]
    async fn past_the_cap_the_newest_lines_come_back_and_the_answer_says_so() {
        let pool = test_pool().await;
        let mut written = Vec::new();
        for hour in 10..15 {
            written.push(
                insert_entry(
                    &pool,
                    None,
                    "event",
                    "line",
                    &format!("2026-03-01T{hour}:00:00+00:00"),
                )
                .await,
            );
        }
        let since = at("2026-03-01T00:00:00Z");

        let cut = timeline(&pool, since, None, None, 3).await.unwrap();
        assert_eq!(ids(&cut), written[2..]);
        assert!(cut.truncated);

        let whole = timeline(&pool, since, None, None, 5).await.unwrap();
        assert_eq!(ids(&whole), written);
        assert!(!whole.truncated, "exactly the cap is everything, not a cut");
    }

    #[test]
    fn a_window_is_refused_backwards_or_wider_than_a_month() {
        let now = at("2026-03-31T00:00:00Z");
        let since = at("2026-03-01T00:00:00Z");

        assert_eq!(timeline_window(since, None, now), Ok(()));
        assert_eq!(
            timeline_window(since, Some(at("2026-04-01T00:00:00Z")), now),
            Ok(()),
            "31 days to the second is inside"
        );
        assert_eq!(
            timeline_window(since, Some(at("2026-04-01T00:00:01Z")), now),
            Err(WindowError::TooWide)
        );
        assert_eq!(
            timeline_window(since, None, at("2026-04-01T00:00:01Z")),
            Err(WindowError::TooWide),
            "an open window is measured to now"
        );
        assert_eq!(
            timeline_window(since, Some(at("2026-02-28T00:00:00Z")), now),
            Err(WindowError::UntilBeforeSince)
        );
    }

    #[tokio::test]
    async fn nothing_has_been_seen_on_a_fresh_database() {
        let pool = test_pool().await;
        assert_eq!(
            seen(&pool).await.unwrap(),
            Seen {
                through: None,
                through_created_at: None,
                seen_at: None,
            }
        );
    }

    /// A second window that had read less must not move the marker back and unread what the first
    /// one read. The time still moves: it records when somebody last looked, not what they found.
    #[tokio::test]
    async fn the_marker_only_moves_forward() {
        let pool = test_pool().await;
        let first = insert_entry(&pool, None, "event", "one", "2026-03-01T09:00:00+00:00").await;
        let second = insert_entry(&pool, None, "event", "two", "2026-03-01T10:00:00+00:00").await;

        let marked = mark_seen(&pool, second, at("2026-03-02T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(marked.through, Some(second));
        assert_eq!(
            marked.through_created_at.as_deref(),
            Some("2026-03-01T10:00:00+00:00")
        );

        let behind = mark_seen(&pool, first, at("2026-03-03T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(behind.through, Some(second));
        assert_eq!(
            behind.seen_at.as_deref(),
            Some(at("2026-03-03T00:00:00Z").to_rfc3339().as_str())
        );
        assert_eq!(seen(&pool).await.unwrap(), behind);
    }

    /// A marker past the newest line would silently pre-read lines that do not exist yet.
    #[tokio::test]
    async fn a_marker_past_the_newest_line_stops_at_it() {
        let pool = test_pool().await;
        let newest = insert_entry(&pool, None, "event", "one", "2026-03-01T09:00:00+00:00").await;

        let marked = mark_seen(&pool, newest + 1000, at("2026-03-02T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(marked.through, Some(newest));

        let later = insert_entry(&pool, None, "event", "two", "2026-03-02T09:00:00+00:00").await;
        assert!(later > newest);
        assert_eq!(seen(&pool).await.unwrap().through, Some(newest));
    }

    /// The marker is an id, and the id's line may be gone — pruned, or never written because ids
    /// skip. The time it stands for is the nearest line at or below it, and none at all when the
    /// retention sweep has taken everything that far back.
    #[tokio::test]
    async fn the_marker_is_dated_by_the_nearest_line_at_or_below_it() {
        let pool = test_pool().await;
        let old = insert_entry(&pool, None, "event", "old", "2026-01-01T09:00:00+00:00").await;
        let kept = insert_entry(&pool, None, "event", "kept", "2026-03-01T09:00:00+00:00").await;
        let gone = insert_entry(&pool, None, "event", "gone", "2026-03-01T10:00:00+00:00").await;
        let newest = insert_entry(&pool, None, "event", "new", "2026-03-01T11:00:00+00:00").await;
        mark_seen(&pool, gone, at("2026-03-02T00:00:00Z"))
            .await
            .unwrap();

        sqlx::query("DELETE FROM feed WHERE id = ?")
            .bind(gone)
            .execute(&pool)
            .await
            .unwrap();
        let marker = seen(&pool).await.unwrap();
        assert_eq!(marker.through, Some(gone));
        assert_eq!(
            marker.through_created_at.as_deref(),
            Some("2026-03-01T09:00:00+00:00"),
            "line {kept} is the nearest one at or below the marker"
        );

        sqlx::query("DELETE FROM feed WHERE id IN (?, ?)")
            .bind(old)
            .bind(kept)
            .execute(&pool)
            .await
            .unwrap();
        let pruned = seen(&pool).await.unwrap();
        assert_eq!(pruned.through, Some(gone));
        assert_eq!(pruned.through_created_at, None);
        assert!(newest > gone);
    }

    /// Migration `0153` removes what the errands feature left in the database. It is reached by
    /// stopping the chain at 148, seeding every shape the feature wrote, and finishing the chain,
    /// because a fresh database has none of these rows and the migration's `DELETE`s would never run.
    #[tokio::test]
    async fn migration_0153_removes_what_errands_left_in_the_database() {
        let pool = crate::testdb::pool_migrated_through(148).await;
        let at = "2026-09-01T00:00:00Z";

        sqlx::query(
            "INSERT INTO errands (id, name, chat_key, folder, created_at) VALUES (1, 'e', 'k', 'f', ?)",
        )
        .bind(at)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO errand_rules (errand_id, name, cron, prompt, last_fired_at, created_at)              VALUES (1, 'r', '* * * * *', 'p', ?, ?)",
        )
        .bind(at)
        .bind(at)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO errand_artifacts (errand_id, path, created_at) VALUES (1, 'a', ?)",
        )
        .bind(at)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO feed (id, kind, summary, created_at, errand_id, subject)              VALUES (1, 'errand_note', 'about an errand', ?, 1, 'errand:1')",
        )
        .bind(at)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO feed (id, kind, summary, created_at) VALUES (2, 'machine', 'kept', ?)",
        )
        .bind(at)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO proposals (id, kind, reasoning, created_at, errand_id)              VALUES (1, 'action-approval', 'refused', ?, 1)",
        )
        .bind(at)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO knowledge (id, layer, scope_kind, scope_id, source, kind, title, body, status, created_at)              VALUES (1, 'semantic', 'errand', '1', 'owner', 'memory', 't', 'b', 'active', ?)",
        )
        .bind(at)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO knowledge_events (knowledge_id, to_status, at) VALUES (1, 'active', ?)",
        )
        .bind(at)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO notify_policy (scope, selector, enabled, updated_at)              VALUES ('family', 'errand_', 1, ?)",
        )
        .bind(at)
        .execute(&pool)
        .await
        .unwrap();

        crate::testdb::apply_migrations_after(&pool, 148).await;

        for table in ["errands", "errand_rules", "errand_artifacts"] {
            let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name = ?")
                .bind(table)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(left, 0, "table {table} must be gone");
        }
        for table in ["feed", "proposals"] {
            let column: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM pragma_table_info(?) WHERE name = 'errand_id'",
            )
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(column, 0, "{table} must have no errand_id column");
        }

        let feed: Vec<i64> = sqlx::query_scalar("SELECT id FROM feed ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(feed, vec![2], "only the machine line stays");
        let proposals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM proposals")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            proposals, 1,
            "a proposal is a refused-action record and stays"
        );
        let knowledge: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM knowledge WHERE scope_kind = 'errand'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(knowledge, 0);
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM knowledge_events")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(events, 0);
        let policy: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notify_policy")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(policy, 0);
    }
}
