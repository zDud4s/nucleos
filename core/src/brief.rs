//! The pool-facing half of the knowledge store.
//!
//! `knowledge.rs` makes the selection decision with zero I/O; this module fetches candidates,
//! gives them SQLite's FTS rank, and hands them to that pure selector. It also persists the
//! selector's trace once a run exists. A briefing for work that never becomes a run leaves no
//! trace: `run_knowledge.run_id` is deliberately NOT NULL and references `runs` (D15).

use std::collections::HashMap;

use sqlx::SqlitePool;

use crate::knowledge::{self, Brief, Budget, Context, Scope, Scored};

/// Bounded work on the briefing hot path, as `knowledge::MAX_READ` bounds the candidate fetch.
const MAX_QUERY_TERMS: usize = 64;

/// Briefing traces keep the same 90-day window as `feed::DEFAULT_RETENTION_DAYS` and
/// `council::DEFAULT_COUNCIL_RETENTION_DAYS`, not the transcript's 30 days. The risk in spec
/// section 13.6 is not the volume of one run: the consolidator grows the store by itself, so
/// candidates times runs grows quadratically.
pub const DEFAULT_KNOWLEDGE_TRACE_RETENTION_DAYS: i64 = 90;

/// The trace window, overridable independently from the other hourly retention sweeps.
pub(crate) fn retention_days() -> i64 {
    std::env::var("NUCLEOS_KNOWLEDGE_TRACE_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_KNOWLEDGE_TRACE_RETENTION_DAYS)
}

/// Build an OR expression because an implicit FTS5 AND over a whole node prompt usually matches
/// no row and would leave the FTS signal at zero for every candidate.
fn match_expression(query: &str) -> Option<String> {
    let quoted = crate::search::fts_query(query);
    let mut terms = Vec::new();
    for term in quoted.split(' ') {
        if term.is_empty() || terms.contains(&term) {
            continue;
        }
        terms.push(term);
        if terms.len() == MAX_QUERY_TERMS {
            break;
        }
    }
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

/// Turn SQLite's negative bm25 ranks into one signal on the unit scale for this pass.
fn normalise_fts(bm25: &[Option<f64>]) -> Vec<f64> {
    let raw: Vec<Option<f64>> = bm25
        .iter()
        .map(|rank| rank.filter(|value| value.is_finite()).map(|value| -value))
        .collect();
    if !raw.iter().any(Option::is_some) {
        return vec![0.0; raw.len()];
    }

    let lo = raw
        .iter()
        .map(|value| value.unwrap_or(0.0))
        .fold(f64::INFINITY, f64::min);
    let hi = raw
        .iter()
        .map(|value| value.unwrap_or(0.0))
        .fold(f64::NEG_INFINITY, f64::max);
    if hi == lo {
        return raw
            .iter()
            .map(|value| if value.is_some() { 1.0 } else { 0.0 })
            .collect();
    }

    raw.iter()
        .map(|value| value.map_or(0.0, |value| (value - lo) / (hi - lo)))
        .collect()
}

async fn fts_ranks(pool: &SqlitePool, expression: &str) -> sqlx::Result<HashMap<i64, f64>> {
    let rows: Vec<(i64, f64)> = sqlx::query_as(
        "SELECT rowid, bm25(knowledge_fts)
           FROM knowledge_fts
          WHERE knowledge_fts MATCH ?",
    )
    .bind(expression)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Fetch the context's candidates, add their query-local FTS signal, and select without writing.
#[cfg_attr(not(test), allow(dead_code))] // Task 3.4 calls this from job::spawn_node.
pub async fn of(pool: &SqlitePool, context: &Context, query: &str) -> sqlx::Result<Brief> {
    let scope = context.chain.last().unwrap_or(&Scope::Machine);
    let mut known = knowledge::for_scope(pool, scope).await?;
    let ranks = match match_expression(query) {
        Some(expression) => match fts_ranks(pool, &expression).await {
            Ok(ranks) => ranks,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "knowledge FTS ranking failed; continuing without matches"
                );
                HashMap::new()
            }
        },
        None => HashMap::new(),
    };
    let bm25: Vec<Option<f64>> = known
        .iter()
        .map(|candidate| ranks.get(&candidate.id).copied())
        .collect();
    for (candidate, s_fts) in known.iter_mut().zip(normalise_fts(&bm25)) {
        candidate.s_fts = s_fts;
    }
    Ok(knowledge::select(&known, context, &Budget::default()))
}

/// Persist every candidate, shown or not, so the trace answers why a row lost.
///
/// The timestamp is RFC 3339 because `knowledge::recency` parses that format and treats anything
/// else as never shown; the credit pass also copies this value into `last_shown_at`.
#[cfg_attr(not(test), allow(dead_code))] // Task 3.4 calls this from job::spawn_node.
pub async fn record(
    pool: &SqlitePool,
    run_id: i64,
    item_id: Option<i64>,
    trace: &[Scored],
) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    let at = chrono::Utc::now().to_rfc3339();
    let mut inserted = 0;
    for candidate in trace {
        inserted += sqlx::query(
            "INSERT INTO run_knowledge
               (run_id, knowledge_id, item_id, shown, s_fts, s_scope, s_structure,
                s_recency, s_use, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(run_id)
        .bind(candidate.knowledge_id)
        .bind(item_id)
        .bind(candidate.shown)
        .bind(candidate.s_fts)
        .bind(candidate.s_scope)
        .bind(candidate.s_structure)
        .bind(candidate.s_recency)
        .bind(candidate.s_use)
        .bind(&at)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    tx.commit().await?;
    Ok(inserted)
}

/// Delete briefing explanations past their window without touching learned signal.
///
/// Migration 0143 makes `run_knowledge` the trace, not the signal: counters and recency live on
/// the knowledge row. [`record`] writes `at` as RFC 3339, so computing the cutoff in Rust makes
/// the TEXT comparison exact rather than mixing SQLite's space-separated datetime format with
/// RFC 3339's `T` separator.
pub async fn prune(
    pool: &SqlitePool,
    retain_days: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<u64> {
    if retain_days <= 0 {
        return Ok(0);
    }

    let cutoff = (now - chrono::Duration::days(retain_days)).to_rfc3339();
    Ok(sqlx::query("DELETE FROM run_knowledge WHERE at < ?")
        .bind(cutoff)
        .execute(pool)
        .await?
        .rows_affected())
}

/// What the work that received a briefing ultimately proved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Green,
    NotGreen,
    NoOutcome,
}

/// Map a final run to the signal it contributes to the knowledge it was shown.
pub(crate) fn run_verdict(
    status: &str,
    exit_code: Option<i64>,
    gate_status: Option<&str>,
) -> Option<Verdict> {
    if !crate::runs::TERMINAL_RUN_STATUSES.contains(&status) {
        return None;
    }
    Some(match gate_status {
        Some("passed") => Verdict::Green,
        Some("failed") => Verdict::NotGreen,
        Some(_) => Verdict::NoOutcome,
        None => match status {
            "completed" => Verdict::Green,
            "failed" | "timed_out" if exit_code.is_some() => Verdict::NotGreen,
            _ => Verdict::NoOutcome,
        },
    })
}

/// Map an item state to a verdict without restating which red gates remain retriable.
pub(crate) fn item_verdict(
    state: crate::job::ItemState,
    gate_status: Option<&str>,
    job_ended: bool,
    run: Option<(&str, Option<i64>)>,
) -> Option<Verdict> {
    use crate::job::ItemState;

    let run_without_gate = || {
        run.and_then(|(status, exit_code)| run_verdict(status, exit_code, None))
            .unwrap_or(Verdict::NoOutcome)
    };
    match state {
        ItemState::Passed if gate_status == Some("passed") => Some(Verdict::Green),
        ItemState::Passed | ItemState::Failed => Some(run_without_gate()),
        ItemState::GateFailed => Some(Verdict::NotGreen),
        ItemState::GateErrored
        | ItemState::Cancelled
        | ItemState::Skipped
        | ItemState::Superseded
        | ItemState::Orphaned => Some(Verdict::NoOutcome),
        ItemState::Pending
        | ItemState::Running
        | ItemState::Implemented
        | ItemState::Merging
        | ItemState::Conflicted
        | ItemState::Reverted
        | ItemState::GateRetriable => job_ended.then_some(Verdict::NoOutcome),
    }
}

/// The same defensive ceiling used by the knowledge history walk.
const MAX_CHAIN_HOPS: usize = 50;

/// Follow handoffs and approval resumes to the run that owns the standalone unit's verdict.
async fn chain_end(pool: &SqlitePool, run_id: i64) -> sqlx::Result<i64> {
    let mut current = run_id;
    for _ in 0..MAX_CHAIN_HOPS {
        let (status, successor): (String, Option<i64>) =
            sqlx::query_as("SELECT status, successor_run_id FROM runs WHERE id = ?")
                .bind(current)
                .fetch_one(pool)
                .await?;
        if let Some(successor) = successor {
            current = successor;
            continue;
        }
        if status == "superseded" {
            let resume: Option<i64> = sqlx::query_scalar(
                "SELECT g.run_id
                   FROM action_grants g
                   JOIN proposals p ON p.id = g.proposal_id
                  WHERE p.run_id = ?
                  ORDER BY g.run_id DESC
                  LIMIT 1",
            )
            .bind(current)
            .fetch_optional(pool)
            .await?;
            if let Some(resume) = resume {
                current = resume;
                continue;
            }
        }
        return Ok(current);
    }
    Ok(current)
}

#[derive(Debug, Clone, Copy)]
enum Unit {
    Item(i64),
    Run(i64),
}

type ItemCreditRow = (String, Option<String>, i64, Option<i64>, i64, String);

/// Claim and credit every uncredited trace row for one unit in a single transaction.
///
/// `credited_at`, rather than the trace primary key, makes repeated passes idempotent. Claiming
/// first inside the transaction makes concurrent passes credit once. The counters remain on the
/// knowledge row so pruning may remove the trace without removing the learned signal.
async fn credit_rows(pool: &SqlitePool, unit: Unit, verdict: Verdict) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    let credited_at = chrono::Utc::now().to_rfc3339();
    let claimed: Vec<(i64, bool, String)> = match unit {
        Unit::Item(item_id) => {
            sqlx::query_as(
                "UPDATE run_knowledge
                SET credited_at = ?
              WHERE item_id = ? AND credited_at IS NULL
          RETURNING knowledge_id, shown, at",
            )
            .bind(&credited_at)
            .bind(item_id)
            .fetch_all(&mut *tx)
            .await?
        }
        Unit::Run(run_id) => {
            sqlx::query_as(
                "UPDATE run_knowledge
                SET credited_at = ?
              WHERE run_id = ? AND item_id IS NULL AND credited_at IS NULL
          RETURNING knowledge_id, shown, at",
            )
            .bind(&credited_at)
            .bind(run_id)
            .fetch_all(&mut *tx)
            .await?
        }
    };

    let mut shown_at: HashMap<i64, String> = HashMap::new();
    for (knowledge_id, shown, at) in claimed {
        if !shown {
            continue;
        }
        match shown_at.get_mut(&knowledge_id) {
            Some(latest) if *latest < at => *latest = at,
            Some(_) => {}
            None => {
                shown_at.insert(knowledge_id, at);
            }
        }
    }

    let (outcome, green) = match verdict {
        Verdict::Green => (1, 1),
        Verdict::NotGreen => (1, 0),
        Verdict::NoOutcome => (0, 0),
    };
    let mut updated = 0;
    for (knowledge_id, at) in shown_at {
        updated += sqlx::query(
            "UPDATE knowledge
                SET shown_count = shown_count + 1,
                    outcome_count = outcome_count + ?,
                    green_count = green_count + ?,
                    last_shown_at = CASE
                        WHEN last_shown_at IS NULL OR last_shown_at < ? THEN ?
                        ELSE last_shown_at
                    END
              WHERE id = ?",
        )
        .bind(outcome)
        .bind(green)
        .bind(&at)
        .bind(&at)
        .bind(knowledge_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    tx.commit().await?;
    Ok(updated)
}

async fn credit_item_by_id(pool: &SqlitePool, item_id: i64) -> sqlx::Result<u64> {
    let item: Option<ItemCreditRow> = sqlx::query_as(
        "SELECT i.status, i.gate_status, i.gate_attempts, i.run_id,
                j.gate_retries, j.status
           FROM job_items i
           JOIN jobs j ON j.id = i.job_id
          WHERE i.id = ?",
    )
    .bind(item_id)
    .fetch_optional(pool)
    .await?;
    let Some((status, gate_status, attempts, run_id, retries, job_status)) = item else {
        return Ok(0);
    };

    let run: Option<(String, Option<i64>)> = match run_id {
        Some(run_id) => {
            sqlx::query_as("SELECT status, exit_code FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_optional(pool)
                .await?
        }
        None => None,
    };
    let state = crate::job::item_state_from(&status, attempts, retries);
    let job_ended = crate::job::TERMINAL_STATUSES.contains(&job_status.as_str());
    let verdict = item_verdict(
        state,
        gate_status.as_deref(),
        job_ended,
        run.as_ref()
            .map(|(status, exit_code)| (status.as_str(), *exit_code)),
    );
    match verdict {
        Some(verdict) => credit_rows(pool, Unit::Item(item_id), verdict).await,
        None => Ok(0),
    }
}

/// Credit one job item once its item-level verdict is final.
#[cfg_attr(not(test), allow(dead_code))] // Task 3.4 calls this from job.rs.
pub(crate) async fn credit_item(pool: &SqlitePool, job_id: i64, ordinal: i64) -> sqlx::Result<u64> {
    let item_id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM job_items WHERE job_id = ? AND ordinal = ?")
            .bind(job_id)
            .bind(ordinal)
            .fetch_optional(pool)
            .await?;
    match item_id {
        Some(item_id) => credit_item_by_id(pool, item_id).await,
        None => Ok(0),
    }
}

/// Credit one standalone run by the final run at the end of its handoff or resume chain.
pub(crate) async fn credit_run(pool: &SqlitePool, run_id: i64) -> sqlx::Result<u64> {
    let owner: Option<(Option<i64>,)> = sqlx::query_as("SELECT job_id FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(pool)
        .await?;
    let Some((job_id,)) = owner else {
        return Ok(0);
    };
    if job_id.is_some() {
        return Ok(0);
    }

    let end = chain_end(pool, run_id).await?;
    let row: (String, Option<i64>, Option<String>) =
        sqlx::query_as("SELECT status, exit_code, gate_status FROM runs WHERE id = ?")
            .bind(end)
            .fetch_one(pool)
            .await?;
    match run_verdict(&row.0, row.1, row.2.as_deref()) {
        Some(verdict) => credit_rows(pool, Unit::Run(run_id), verdict).await,
        None => Ok(0),
    }
}

/// Retry every durable, uncredited unit whose verdict is now final.
///
/// This follows the `job_notes.delivered_at IS NULL` queue precedent: because both the verdict and
/// trace are durable, a crash between the verdict and credit is repaired by the next sweep.
#[cfg_attr(not(test), allow(dead_code))] // Task 3.4 calls this from job.rs.
pub(crate) async fn sweep(pool: &SqlitePool) -> sqlx::Result<u64> {
    let item_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT item_id
           FROM run_knowledge
          WHERE credited_at IS NULL AND item_id IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;
    let mut total = 0;
    for item_id in item_ids {
        match credit_item_by_id(pool, item_id).await {
            Ok(credited) => total += credited,
            Err(error) => {
                tracing::warn!(item_id, error = %error, "knowledge item credit failed");
            }
        }
    }

    let run_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT rk.run_id
           FROM run_knowledge rk
           JOIN runs r ON r.id = rk.run_id
          WHERE rk.credited_at IS NULL
            AND rk.item_id IS NULL
            AND r.job_id IS NULL",
    )
    .fetch_all(pool)
    .await?;
    for run_id in run_ids {
        match credit_run(pool, run_id).await {
            Ok(credited) => total += credited,
            Err(error) => {
                tracing::warn!(run_id, error = %error, "knowledge run credit failed");
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::{Row, SqlitePool};

    use super::{
        Verdict, credit_item, credit_run, item_verdict, match_expression, normalise_fts, of, prune,
        record, run_verdict, sweep,
    };
    use crate::job::ItemState;
    use crate::knowledge::{Context, Scope, Scored};

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    async fn seed(
        pool: &SqlitePool,
        scope_kind: &str,
        scope_id: Option<&str>,
        title: &str,
    ) -> sqlx::Result<i64> {
        let result = sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', ?, ?, 'owner', 'memory', ?, 'body', 'active',
                     '2026-08-19T00:00:00+00:00')",
        )
        .bind(scope_kind)
        .bind(scope_id)
        .bind(title)
        .execute(pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    fn scored(knowledge_id: i64, shown: bool) -> Scored {
        Scored {
            knowledge_id,
            shown,
            s_fts: 0.0,
            s_scope: 0.0,
            s_structure: 0.0,
            s_recency: 0.0,
            s_use: 0.0,
            score: 0.0,
        }
    }

    async fn seed_run(
        pool: &SqlitePool,
        status: &str,
        exit_code: Option<i64>,
        gate_status: Option<&str>,
        job_id: Option<i64>,
    ) -> sqlx::Result<i64> {
        let result = sqlx::query(
            "INSERT INTO runs
               (prompt, status, mode, exit_code, gate_status, job_id, created_at)
             VALUES ('p', ?, 'worktree', ?, ?, ?, ?)",
        )
        .bind(status)
        .bind(exit_code)
        .bind(gate_status)
        .bind(job_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    async fn seed_job(pool: &SqlitePool, status: &str, retries: i64) -> sqlx::Result<i64> {
        let result = sqlx::query(
            "INSERT INTO jobs (project_id, project_root, status, max_items, gate_retries, created_at)
             VALUES ('p1', 'C:/tmp', ?, 1, ?, ?)",
        )
        .bind(status)
        .bind(retries)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    async fn seed_item(
        pool: &SqlitePool,
        job_id: i64,
        status: &str,
        gate_status: Option<&str>,
        gate_attempts: i64,
        run_id: Option<i64>,
    ) -> sqlx::Result<i64> {
        let result = sqlx::query(
            "INSERT INTO job_items
               (job_id, ordinal, description, status, gate_status, gate_attempts, run_id)
             VALUES (?, 0, 'item', ?, ?, ?, ?)",
        )
        .bind(job_id)
        .bind(status)
        .bind(gate_status)
        .bind(gate_attempts)
        .bind(run_id)
        .execute(pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    async fn counters(
        pool: &SqlitePool,
        knowledge_id: i64,
    ) -> sqlx::Result<(i64, i64, i64, Option<String>)> {
        sqlx::query_as(
            "SELECT shown_count, outcome_count, green_count, last_shown_at
               FROM knowledge WHERE id = ?",
        )
        .bind(knowledge_id)
        .fetch_one(pool)
        .await
    }

    async fn seed_three(pool: &SqlitePool) -> sqlx::Result<()> {
        seed(pool, "machine", None, "house rule").await?;
        seed(pool, "project", Some("p1"), "zanzibar rollout").await?;
        seed(pool, "project", Some("p1"), "plain note").await?;
        Ok(())
    }

    fn context() -> Context {
        Context {
            chain: vec![Scope::Machine, Scope::Project("p1".into())],
            files: vec![],
            communities: vec![],
            node: None,
            gate: None,
        }
    }

    /// The briefing trace has its own 90-day window: the same as the feed and council sweeps in
    /// the hourly loop, not the transcript's 30 days. What expires is the explanation of a
    /// briefing. Pruning touches no signal because the counters and `last_shown_at` live on the
    /// knowledge row. `prune_transcripts` is the precedent for the shape and the counter-example
    /// for the mechanism: it empties columns and deletes no rows. The row itself is NOT deleted,
    /// and that is the whole design.
    #[tokio::test]
    async fn pruning_the_trace_loses_the_explanation_and_no_signal() -> sqlx::Result<()> {
        let pool = test_pool().await;
        let now = chrono::Utc::now();
        let old_at = (now - chrono::Duration::days(91)).to_rfc3339();
        let recent_at = (now - chrono::Duration::days(1)).to_rfc3339();
        let last_shown_at = (now - chrono::Duration::days(2)).to_rfc3339();
        let measured = seed(&pool, "machine", None, "measured").await?;
        let untouched = seed(&pool, "machine", None, "untouched").await?;
        let run_id = seed_run(&pool, "completed", Some(0), Some("passed"), None).await?;

        sqlx::query(
            "UPDATE knowledge
                SET shown_count = 3, outcome_count = 2, green_count = 1, last_shown_at = ?
              WHERE id = ?",
        )
        .bind(&last_shown_at)
        .bind(measured)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO run_knowledge
               (run_id, knowledge_id, shown, s_fts, s_scope, s_structure, s_recency, s_use, at,
                credited_at)
             VALUES (?, ?, 1, 0.0, 0.0, 0.0, 0.0, 0.0, ?, NULL)",
        )
        .bind(run_id)
        .bind(measured)
        .bind(&old_at)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO run_knowledge
               (run_id, knowledge_id, shown, s_fts, s_scope, s_structure, s_recency, s_use, at,
                credited_at)
             VALUES (?, ?, 0, 0.0, 0.0, 0.0, 0.0, 0.0, ?, ?)",
        )
        .bind(run_id)
        .bind(untouched)
        .bind(&recent_at)
        .bind(&recent_at)
        .execute(&pool)
        .await?;

        let before = counters(&pool, measured).await?;
        assert_eq!(prune(&pool, 90, now).await?, 1);
        let remaining: Vec<(i64, String)> =
            sqlx::query_as("SELECT knowledge_id, at FROM run_knowledge")
                .fetch_all(&pool)
                .await?;
        assert_eq!(remaining, vec![(untouched, recent_at)]);
        assert_eq!(counters(&pool, measured).await?, before);
        let knowledge_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM knowledge")
            .fetch_one(&pool)
            .await?;
        assert_eq!(knowledge_rows, 2);

        assert_eq!(prune(&pool, 0, now).await?, 0);
        let trace_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_knowledge")
            .fetch_one(&pool)
            .await?;
        assert_eq!(trace_rows, 1);
        Ok(())
    }

    #[test]
    fn the_fts_signal_is_min_max_within_one_pass() {
        assert_eq!(normalise_fts(&[None, None]), vec![0.0, 0.0]);
        assert_eq!(
            normalise_fts(&[Some(-4.0), Some(-1.0), None]),
            vec![1.0, 0.25, 0.0]
        );
        assert_eq!(normalise_fts(&[Some(-3.0)]), vec![1.0]);
        assert_eq!(normalise_fts(&[Some(-2.0), Some(-2.0)]), vec![1.0, 1.0]);
        assert_eq!(normalise_fts(&[Some(-4.0), Some(-1.0)]), vec![1.0, 0.0]);
        assert_eq!(normalise_fts(&[Some(f64::NAN), Some(-1.0)]), vec![0.0, 1.0]);

        assert_eq!(match_expression(""), None);
        assert_eq!(match_expression("\"\" \""), None);
        assert_eq!(match_expression("a b a"), Some("\"a\" OR \"b\"".into()));
    }

    #[tokio::test]
    async fn a_briefing_leaves_one_trace_row_per_candidate_with_the_five_signals()
    -> sqlx::Result<()> {
        let pool = test_pool().await;
        seed_three(&pool).await?;
        let now = chrono::Utc::now().to_rfc3339();
        let run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('p', 'running', 'worktree', ?)",
        )
        .bind(now)
        .execute(&pool)
        .await?
        .last_insert_rowid();

        let brief = of(&pool, &context(), "zanzibar please").await?;
        assert_eq!(record(&pool, run_id, None, &brief.trace).await?, 3);

        let rows = sqlx::query(
            "SELECT k.title, rk.shown, rk.s_fts, rk.s_scope, rk.s_structure,
                    rk.s_recency, rk.s_use, rk.at, rk.credited_at, rk.item_id
               FROM run_knowledge rk
               JOIN knowledge k ON k.id = rk.knowledge_id
              WHERE rk.run_id = ?
              ORDER BY rk.knowledge_id",
        )
        .bind(run_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(rows.len(), 3);

        let block = brief.block.as_deref().unwrap_or_default();
        let mut machine_scope = None;
        let mut project_scopes = Vec::new();
        for row in rows {
            let title: String = row.try_get("title")?;
            let shown: bool = row.try_get("shown")?;
            let s_fts: f64 = row.try_get("s_fts")?;
            let s_scope: f64 = row.try_get("s_scope")?;
            let s_structure: f64 = row.try_get("s_structure")?;
            let s_recency: f64 = row.try_get("s_recency")?;
            let s_use: f64 = row.try_get("s_use")?;
            let at: String = row.try_get("at")?;
            let credited_at: Option<String> = row.try_get("credited_at")?;
            let item_id: Option<i64> = row.try_get("item_id")?;

            DateTime::parse_from_rfc3339(&at).expect("trace timestamps are RFC 3339");
            assert!(credited_at.is_none());
            assert!(item_id.is_none());
            assert_eq!(shown, block.contains(&title));
            assert!(s_structure.is_finite());
            assert!(s_recency.is_finite());
            assert!(s_use.is_finite());

            if title == "zanzibar rollout" {
                assert_eq!(s_fts, 1.0);
                project_scopes.push(s_scope);
            } else {
                assert_eq!(s_fts, 0.0);
                if title == "house rule" {
                    machine_scope = Some(s_scope);
                } else {
                    project_scopes.push(s_scope);
                }
            }
        }
        let machine_scope = machine_scope.expect("the machine row is traced");
        assert!(project_scopes.iter().all(|scope| machine_scope < *scope));
        Ok(())
    }

    #[tokio::test]
    async fn a_chat_turn_is_briefed_and_leaves_no_trace() -> sqlx::Result<()> {
        let pool = test_pool().await;
        seed_three(&pool).await?;

        let brief = of(&pool, &context(), "zanzibar").await?;
        assert!(
            brief
                .block
                .as_deref()
                .is_some_and(|block| block.contains("zanzibar rollout"))
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_knowledge")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn a_green_run_credits_the_rows_it_was_shown_and_only_those() -> sqlx::Result<()> {
        let pool = test_pool().await;
        let shown = seed(&pool, "machine", None, "shown").await?;
        let hidden = seed(&pool, "machine", None, "hidden").await?;
        let run_id = seed_run(&pool, "completed", Some(0), Some("passed"), None).await?;
        record(
            &pool,
            run_id,
            None,
            &[scored(shown, true), scored(hidden, false)],
        )
        .await?;

        assert_eq!(credit_run(&pool, run_id).await?, 1);
        let shown_counts = counters(&pool, shown).await?;
        assert_eq!((shown_counts.0, shown_counts.1, shown_counts.2), (1, 1, 1));
        DateTime::parse_from_rfc3339(shown_counts.3.as_deref().unwrap())
            .expect("last_shown_at is RFC 3339");
        assert_eq!(counters(&pool, hidden).await?, (0, 0, 0, None));
        let credited: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM run_knowledge WHERE run_id = ? AND credited_at IS NOT NULL",
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(credited, 2);
        Ok(())
    }

    #[tokio::test]
    async fn a_job_node_credits_by_the_verdict_of_its_item_and_not_of_its_run() -> sqlx::Result<()>
    {
        let pool = test_pool().await;
        let knowledge_id = seed(&pool, "machine", None, "item verdict").await?;
        let job_id = seed_job(&pool, "gate_failed", 0).await?;
        let run_id = seed_run(&pool, "completed", Some(0), Some("passed"), Some(job_id)).await?;
        let item_id = seed_item(
            &pool,
            job_id,
            "gate_failed",
            Some("failed"),
            1,
            Some(run_id),
        )
        .await?;
        record(&pool, run_id, Some(item_id), &[scored(knowledge_id, true)]).await?;

        assert_eq!(credit_run(&pool, run_id).await?, 0);
        assert_eq!(counters(&pool, knowledge_id).await?, (0, 0, 0, None));
        assert_eq!(credit_item(&pool, job_id, 0).await?, 1);
        let credited = counters(&pool, knowledge_id).await?;
        assert_eq!((credited.0, credited.1, credited.2), (1, 1, 0));
        Ok(())
    }

    #[tokio::test]
    async fn a_cancelled_run_leaves_the_rows_it_showed_not_measured_rather_than_failed()
    -> sqlx::Result<()> {
        let pool = test_pool().await;
        let knowledge_id = seed(&pool, "machine", None, "cancelled").await?;
        let run_id = seed_run(&pool, "cancelled", None, None, None).await?;
        record(&pool, run_id, None, &[scored(knowledge_id, true)]).await?;

        assert_eq!(credit_run(&pool, run_id).await?, 1);
        let credited = counters(&pool, knowledge_id).await?;
        assert_eq!((credited.0, credited.1, credited.2), (1, 0, 0));
        let stamp: Option<String> = sqlx::query_scalar(
            "SELECT credited_at FROM run_knowledge WHERE run_id = ? AND knowledge_id = ?",
        )
        .bind(run_id)
        .bind(knowledge_id)
        .fetch_one(&pool)
        .await?;
        assert!(stamp.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn a_second_credit_pass_over_the_same_run_changes_nothing() -> sqlx::Result<()> {
        let pool = test_pool().await;
        let knowledge_id = seed(&pool, "machine", None, "once").await?;
        let run_id = seed_run(&pool, "completed", Some(0), Some("passed"), None).await?;
        record(&pool, run_id, None, &[scored(knowledge_id, true)]).await?;

        assert_eq!(credit_run(&pool, run_id).await?, 1);
        let before: (String, String) = sqlx::query_as(
            "SELECT credited_at, last_shown_at
               FROM run_knowledge JOIN knowledge ON knowledge.id = knowledge_id
              WHERE run_id = ? AND knowledge_id = ?",
        )
        .bind(run_id)
        .bind(knowledge_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(credit_run(&pool, run_id).await?, 0);
        assert_eq!(sweep(&pool).await?, 0);
        let credited = counters(&pool, knowledge_id).await?;
        assert_eq!((credited.0, credited.1, credited.2), (1, 1, 1));
        let after: (String, String) = sqlx::query_as(
            "SELECT credited_at, last_shown_at
               FROM run_knowledge JOIN knowledge ON knowledge.id = knowledge_id
              WHERE run_id = ? AND knowledge_id = ?",
        )
        .bind(run_id)
        .bind(knowledge_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(before, after);
        Ok(())
    }

    #[tokio::test]
    async fn a_row_shown_in_two_attempts_of_one_item_is_credited_once() -> sqlx::Result<()> {
        let pool = test_pool().await;
        let knowledge_id = seed(&pool, "machine", None, "twice").await?;
        let job_id = seed_job(&pool, "completed", 0).await?;
        let first_run = seed_run(&pool, "failed", Some(1), None, Some(job_id)).await?;
        let second_run =
            seed_run(&pool, "completed", Some(0), Some("passed"), Some(job_id)).await?;
        let item_id =
            seed_item(&pool, job_id, "passed", Some("passed"), 1, Some(second_run)).await?;
        record(
            &pool,
            first_run,
            Some(item_id),
            &[scored(knowledge_id, true)],
        )
        .await?;
        record(
            &pool,
            second_run,
            Some(item_id),
            &[scored(knowledge_id, true)],
        )
        .await?;

        assert_eq!(credit_item(&pool, job_id, 0).await?, 1);
        let credited = counters(&pool, knowledge_id).await?;
        assert_eq!((credited.0, credited.1, credited.2), (1, 1, 1));
        let stamps: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM run_knowledge WHERE item_id = ? AND credited_at IS NOT NULL",
        )
        .bind(item_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(stamps, 2);
        Ok(())
    }

    #[tokio::test]
    async fn a_retriable_item_is_not_credited_until_its_verdict_is_final() -> sqlx::Result<()> {
        let pool = test_pool().await;
        let knowledge_id = seed(&pool, "machine", None, "retry").await?;
        let job_id = seed_job(&pool, "running", 1).await?;
        let run_id = seed_run(&pool, "failed", Some(1), None, Some(job_id)).await?;
        let item_id = seed_item(
            &pool,
            job_id,
            "gate_failed",
            Some("failed"),
            1,
            Some(run_id),
        )
        .await?;
        record(&pool, run_id, Some(item_id), &[scored(knowledge_id, true)]).await?;

        assert_eq!(credit_item(&pool, job_id, 0).await?, 0);
        assert_eq!(counters(&pool, knowledge_id).await?, (0, 0, 0, None));
        let stamp: Option<String> =
            sqlx::query_scalar("SELECT credited_at FROM run_knowledge WHERE item_id = ?")
                .bind(item_id)
                .fetch_one(&pool)
                .await?;
        assert!(stamp.is_none());

        sqlx::query("UPDATE job_items SET gate_attempts = 2 WHERE id = ?")
            .bind(item_id)
            .execute(&pool)
            .await?;
        assert_eq!(credit_item(&pool, job_id, 0).await?, 1);
        let credited = counters(&pool, knowledge_id).await?;
        assert_eq!((credited.0, credited.1, credited.2), (1, 1, 0));
        Ok(())
    }

    #[tokio::test]
    async fn a_standalone_run_is_credited_by_the_end_of_its_chain() -> sqlx::Result<()> {
        let pool = test_pool().await;
        let handoff_knowledge = seed(&pool, "machine", None, "handoff").await?;
        let predecessor = seed_run(&pool, "completed", Some(0), None, None).await?;
        let successor = seed_run(&pool, "running", None, None, None).await?;
        sqlx::query("UPDATE runs SET successor_run_id = ? WHERE id = ?")
            .bind(successor)
            .bind(predecessor)
            .execute(&pool)
            .await?;
        record(&pool, predecessor, None, &[scored(handoff_knowledge, true)]).await?;
        assert_eq!(credit_run(&pool, predecessor).await?, 0);
        sqlx::query("UPDATE runs SET status = 'completed', exit_code = 0, gate_status = 'passed' WHERE id = ?")
            .bind(successor)
            .execute(&pool)
            .await?;
        assert_eq!(credit_run(&pool, predecessor).await?, 1);
        let handoff = counters(&pool, handoff_knowledge).await?;
        assert_eq!((handoff.0, handoff.1, handoff.2), (1, 1, 1));

        let resume_knowledge = seed(&pool, "machine", None, "resume").await?;
        let superseded = seed_run(&pool, "superseded", None, None, None).await?;
        let resume = seed_run(&pool, "completed", Some(1), Some("failed"), None).await?;
        record(&pool, superseded, None, &[scored(resume_knowledge, true)]).await?;
        let proposal = sqlx::query(
            "INSERT INTO proposals (kind, status, run_id, reasoning, created_at)
             VALUES ('action-approval', 'approved', ?, 'r', ?)",
        )
        .bind(superseded)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&pool)
        .await?
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO action_grants (run_id, tool_name, proposal_id, created_at)
             VALUES (?, 'Bash', ?, ?)",
        )
        .bind(resume)
        .bind(proposal)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&pool)
        .await?;
        assert_eq!(credit_run(&pool, superseded).await?, 1);
        let resumed = counters(&pool, resume_knowledge).await?;
        assert_eq!((resumed.0, resumed.1, resumed.2), (1, 1, 0));
        Ok(())
    }

    #[tokio::test]
    async fn a_run_the_clock_or_a_failed_launch_ended_is_not_measured() -> sqlx::Result<()> {
        let pool = test_pool().await;
        for (title, status, exit_code, expected) in [
            ("clock", "timed_out", None, (1, 0, 0)),
            ("launch", "failed", None, (1, 0, 0)),
            ("progress", "timed_out", Some(124), (1, 1, 0)),
        ] {
            let knowledge_id = seed(&pool, "machine", None, title).await?;
            let run_id = seed_run(&pool, status, exit_code, None, None).await?;
            record(&pool, run_id, None, &[scored(knowledge_id, true)]).await?;
            assert_eq!(credit_run(&pool, run_id).await?, 1);
            let actual = counters(&pool, knowledge_id).await?;
            assert_eq!((actual.0, actual.1, actual.2), expected);
        }
        Ok(())
    }

    #[test]
    fn the_verdict_of_a_run_follows_the_owners_map() {
        for (status, exit_code, gate_status, expected) in [
            ("completed", Some(0), Some("passed"), Some(Verdict::Green)),
            (
                "completed",
                Some(0),
                Some("failed"),
                Some(Verdict::NotGreen),
            ),
            (
                "completed",
                Some(0),
                Some("errored"),
                Some(Verdict::NoOutcome),
            ),
            ("completed", Some(0), None, Some(Verdict::Green)),
            ("failed", Some(1), None, Some(Verdict::NotGreen)),
            ("timed_out", Some(124), None, Some(Verdict::NotGreen)),
            ("failed", None, None, Some(Verdict::NoOutcome)),
            ("timed_out", None, None, Some(Verdict::NoOutcome)),
            ("cancelled", None, None, Some(Verdict::NoOutcome)),
            ("interrupted", None, None, Some(Verdict::NoOutcome)),
            ("superseded", None, None, Some(Verdict::NoOutcome)),
            ("running", None, None, None),
            ("awaiting_approval", None, Some("passed"), None),
        ] {
            assert_eq!(run_verdict(status, exit_code, gate_status), expected);
        }
    }

    #[test]
    fn the_verdict_of_an_item_is_final_only_when_its_state_is() {
        assert_eq!(
            item_verdict(ItemState::GateRetriable, None, false, None),
            None
        );
        assert_eq!(
            item_verdict(ItemState::GateRetriable, None, true, None),
            Some(Verdict::NoOutcome)
        );
        assert_eq!(
            item_verdict(ItemState::Passed, Some("passed"), false, None),
            Some(Verdict::Green)
        );
        assert_eq!(
            item_verdict(ItemState::Passed, None, false, Some(("completed", Some(0)))),
            Some(Verdict::Green)
        );
        assert_eq!(
            item_verdict(ItemState::GateFailed, Some("failed"), false, None),
            Some(Verdict::NotGreen)
        );
        for state in [
            ItemState::GateErrored,
            ItemState::Skipped,
            ItemState::Superseded,
            ItemState::Orphaned,
            ItemState::Cancelled,
        ] {
            assert_eq!(
                item_verdict(state, None, false, None),
                Some(Verdict::NoOutcome)
            );
        }
        assert_eq!(item_verdict(ItemState::Conflicted, None, false, None), None);
    }
}
