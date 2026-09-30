//! The pool-facing half of the knowledge store.
//!
//! `knowledge.rs` makes the selection decision with zero I/O; this module fetches candidates,
//! gives them SQLite's FTS rank, and hands them to that pure selector. It also persists the
//! selector's trace once a run exists. A briefing for work that never becomes a run leaves no
//! trace: `run_knowledge.run_id` is deliberately NOT NULL and references `runs` (D15).

use std::collections::HashMap;

use sqlx::SqlitePool;

use crate::knowledge::{self, Brief, Budget, Context, Scope, Scored};

/// Bound the query expression; `knowledge::MAX_READ` separately bounds candidates and rank reads.
const MAX_QUERY_TERMS: usize = 64;

/// The most knowledge rows one explicit recall returns.
pub const RECALL_LIMIT: usize = 10;

/// One approved answer returned by explicit recall.
#[derive(serde::Serialize)]
pub struct Recalled {
    pub id: i64,
    pub layer: String,
    pub kind: String,
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub source: String,
    pub observations: Option<i64>,
    pub evidence: Option<String>,
    pub title: String,
    pub body: String,
}

/// At the 30-second job tick, 64 units of each kind is 7,680 per hour, well above the rate at
/// which CLI-backed items and runs can finish. Each unit touches at most `knowledge::MAX_READ`
/// trace rows, so one pass also has a fixed counter-write ceiling.
const SWEEP_BATCH: i64 = 64;

/// Item statuses that are final without consulting their job. This mirrors `item_verdict`; the
/// `the_sweep_selects_exactly_the_items_item_verdict_calls_final` agreement test keeps the SQL
/// prefilter and the Rust verdict rule aligned.
const FINAL_ITEM_STATUSES: [&str; 7] = [
    "passed",
    "failed",
    crate::job::STATUS_CANCELLED,
    "gate_errored",
    crate::job::STATUS_SKIPPED,
    crate::job::STATUS_SUPERSEDED,
    "orphaned",
];

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

async fn fts_ranks(
    pool: &SqlitePool,
    expression: &str,
    candidates: &[i64],
) -> sqlx::Result<HashMap<i64, f64>> {
    if candidates.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders = vec!["?"; candidates.len()].join(", ");
    // SAFETY: only literal `?` placeholders are interpolated, as in `knowledge::for_scope`.
    let sql = sqlx::AssertSqlSafe(format!(
        "SELECT rowid, bm25(knowledge_fts)
           FROM knowledge_fts
          WHERE knowledge_fts MATCH ?
            AND rowid IN ({placeholders})
          ORDER BY bm25(knowledge_fts)
          LIMIT ?"
    ));
    let mut query = sqlx::query_as(sql).bind(expression);
    for candidate in candidates {
        query = query.bind(candidate);
    }
    let rows: Vec<(i64, f64)> = query
        .bind(knowledge::MAX_READ as i64)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().collect())
}

/// Recall only D12-approved (`active`) knowledge in the daemon-selected scope.
///
/// `live` rows arrive ONLY through the automatic briefing, with a floor of one item in the last
/// scope group, so if node 1 leaves five facts, node 2 is guaranteed one and may not see the
/// others. That is deliberate. Errors propagate here because the HTTP handler decides how they are
/// reported.
pub async fn recall(
    pool: &SqlitePool,
    scope: &Scope,
    query: &str,
    layer: Option<knowledge::Layer>,
) -> sqlx::Result<Vec<Recalled>> {
    let Some(expression) = match_expression(query) else {
        return Ok(Vec::new());
    };
    let mut known = knowledge::for_scope(pool, scope).await?;
    known.retain(|row| {
        row.layer != knowledge::Layer::Working.as_str()
            && layer.is_none_or(|layer| row.layer == layer.as_str())
            && knowledge::approved(row)
    });
    let candidate_ids: Vec<i64> = known.iter().map(|row| row.id).collect();
    let ranks = fts_ranks(pool, &expression, &candidate_ids).await?;
    known.retain(|row| ranks.contains_key(&row.id));
    known.sort_by(|left, right| {
        ranks[&left.id]
            .total_cmp(&ranks[&right.id])
            .then_with(|| left.id.cmp(&right.id))
    });

    Ok(known
        .into_iter()
        .take(RECALL_LIMIT)
        .map(|row| Recalled {
            id: row.id,
            layer: row.layer,
            kind: row.kind,
            scope_kind: row.scope_kind,
            scope_id: row.scope_id,
            source: row.source,
            observations: row.observations,
            evidence: row.evidence,
            title: row.title,
            body: row.body,
        })
        .collect())
}

/// Fetch the context's candidates, add their query-local FTS signal, and select without writing.
pub async fn of(pool: &SqlitePool, context: &Context, query: &str) -> sqlx::Result<Brief> {
    let scope = context.chain.last().unwrap_or(&Scope::Machine);
    let mut known = knowledge::for_scope(pool, scope).await?;
    let candidate_ids: Vec<i64> = known.iter().map(|candidate| candidate.id).collect();
    let ranks = match match_expression(query) {
        Some(expression) => match fts_ranks(pool, &expression, &candidate_ids).await {
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

/// A layer that cannot be read is a reason to say so, never a reason to refuse to start the node.
/// Every context outside `spawn_node` reads through this one best-effort helper.
pub async fn for_prompt(
    pool: &SqlitePool,
    context: &Context,
    query: &str,
    site: &'static str,
) -> Option<Brief> {
    match of(pool, context, query).await {
        Ok(briefing) => Some(briefing),
        Err(error) => {
            tracing::warn!(site, %error, "could not read what is known; continuing without it");
            None
        }
    }
}

/// Persist every candidate, shown or not, so the trace answers why a row lost.
///
/// The timestamp is RFC 3339 because `knowledge::recency` parses that format and treats anything
/// else as never shown; the credit pass also copies this value into `last_shown_at`.
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

/// Map an item state and its stored gate/run evidence to a final verdict.
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
        // Owner decision: credit must not depend on which side of a replan the sweep runs.
        // Replans supersede only gate_failed/failed items, so NULL keeps the prior run verdict.
        ItemState::Superseded => Some(match gate_status {
            Some("passed") => Verdict::Green,
            Some("failed") => Verdict::NotGreen,
            Some(_) => Verdict::NoOutcome,
            None => run_without_gate(),
        }),
        ItemState::GateErrored
        | ItemState::Cancelled
        | ItemState::Skipped
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

/// Retry a bounded batch of durable, uncredited units whose verdict is now final.
///
/// This follows the `job_notes.delivered_at IS NULL` queue precedent: because both the verdict and
/// trace are durable, a crash between the verdict and credit is repaired by the next sweep. Only
/// final units take a batch slot, and final units are visited oldest first by their monotonic ids.
/// The item prefilter exactly mirrors `item_verdict`. The run prefilter uses the unit run's own
/// terminal status; a terminal handoff or approval run whose chain tail is still live can therefore
/// take a slot until that tail ends, when `credit_run` follows the chain and credits it.
async fn sweep_at_most(pool: &SqlitePool, batch: i64) -> sqlx::Result<u64> {
    let final_item_placeholders = vec!["?"; FINAL_ITEM_STATUSES.len()].join(", ");
    let terminal_job_placeholders = vec!["?"; crate::job::TERMINAL_STATUSES.len()].join(", ");
    let mut item_query = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT rk.item_id
           FROM run_knowledge rk
           JOIN job_items i ON i.id = rk.item_id
           JOIN jobs j ON j.id = i.job_id
          WHERE rk.credited_at IS NULL
            AND rk.item_id IS NOT NULL
            AND (i.status IN ({final_item_placeholders})
                 OR (i.status = 'gate_failed' AND i.gate_attempts > j.gate_retries)
                 OR j.status IN ({terminal_job_placeholders}))
          ORDER BY rk.item_id
          LIMIT ?"
    )));
    for status in FINAL_ITEM_STATUSES {
        item_query = item_query.bind(status);
    }
    for status in crate::job::TERMINAL_STATUSES {
        item_query = item_query.bind(status);
    }
    let item_ids: Vec<i64> = item_query.bind(batch).fetch_all(pool).await?;
    let mut total = 0;
    for item_id in item_ids {
        match credit_item_by_id(pool, item_id).await {
            Ok(credited) => total += credited,
            Err(error) => {
                tracing::warn!(item_id, error = %error, "knowledge item credit failed");
            }
        }
    }

    let terminal_run_placeholders = vec!["?"; crate::runs::TERMINAL_RUN_STATUSES.len()].join(", ");
    let mut run_query = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT rk.run_id
           FROM run_knowledge rk
           JOIN runs r ON r.id = rk.run_id
          WHERE rk.credited_at IS NULL
            AND rk.item_id IS NULL
            AND r.job_id IS NULL
            AND r.status IN ({terminal_run_placeholders})
          ORDER BY rk.run_id
          LIMIT ?"
    )));
    for status in crate::runs::TERMINAL_RUN_STATUSES {
        run_query = run_query.bind(status);
    }
    let run_ids: Vec<i64> = run_query.bind(batch).fetch_all(pool).await?;
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

pub(crate) async fn sweep(pool: &SqlitePool) -> sqlx::Result<u64> {
    sweep_at_most(pool, SWEEP_BATCH).await
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, path::Path};

    use chrono::DateTime;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::{Row, SqlitePool};

    use super::{
        SWEEP_BATCH, Verdict, credit_item, credit_run, fts_ranks, item_verdict, match_expression,
        normalise_fts, of, prune, record, run_verdict, sweep, sweep_at_most,
    };
    use crate::job::ItemState;
    use crate::knowledge::{Context, Scope, Scored};

    const BRIEFED_CONTEXTS: &[&str] =
        &["assistant.rs", "council.rs", "job.rs", "runs.rs", "team.rs"];
    const UNBRIEFED_LAUNCHERS: &[(&str, &str)] = &[(
        "map_intent.rs",
        "map derivation is deliberately unbriefed because it creates the map that later scopes briefing",
    )];

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

    #[tokio::test]
    async fn the_trace_prune_reads_by_age_through_an_index() -> sqlx::Result<()> {
        let pool = test_pool().await;
        let plan = sqlx::query("EXPLAIN QUERY PLAN DELETE FROM run_knowledge WHERE at < ?")
            .bind("2026-01-01T00:00:00+00:00")
            .fetch_all(&pool)
            .await?;

        assert!(plan.iter().any(|row| {
            row.try_get::<String, _>("detail")
                .is_ok_and(|detail| detail.contains("idx_run_knowledge_at"))
        }));
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
    async fn the_fts_rank_read_is_bounded_to_the_candidates() -> sqlx::Result<()> {
        let pool = test_pool().await;
        let a = seed(&pool, "project", Some("p1"), "zanzibar rollout").await?;
        let _b = seed(&pool, "project", Some("p2"), "zanzibar elsewhere").await?;

        let ranks = fts_ranks(&pool, "\"zanzibar\"", &[a]).await?;
        assert_eq!(ranks.keys().copied().collect::<Vec<_>>(), vec![a]);
        assert!(fts_ranks(&pool, "\"zanzibar\"", &[]).await?.is_empty());
        Ok(())
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
    async fn a_sweep_pass_credits_at_most_its_batch_and_the_next_finishes_the_rest()
    -> sqlx::Result<()> {
        let pool = test_pool().await;

        let dead_knowledge = seed(&pool, "machine", None, "dead job node").await?;
        let dead_job = seed_job(&pool, "running", 0).await?;
        let dead_run = seed_run(&pool, "completed", Some(0), None, Some(dead_job)).await?;
        record(&pool, dead_run, None, &[scored(dead_knowledge, true)]).await?;

        let mut item_ids = Vec::new();
        for title in ["item one", "item two", "item three"] {
            let knowledge_id = seed(&pool, "machine", None, title).await?;
            let job_id = seed_job(&pool, "completed", 0).await?;
            let run_id =
                seed_run(&pool, "completed", Some(0), Some("passed"), Some(job_id)).await?;
            let item_id =
                seed_item(&pool, job_id, "passed", Some("passed"), 0, Some(run_id)).await?;
            record(&pool, run_id, Some(item_id), &[scored(knowledge_id, true)]).await?;
            item_ids.push(item_id);
        }

        let mut standalone_run_ids = Vec::new();
        for title in ["run one", "run two", "run three"] {
            let knowledge_id = seed(&pool, "machine", None, title).await?;
            let run_id = seed_run(&pool, "completed", Some(0), Some("passed"), None).await?;
            record(&pool, run_id, None, &[scored(knowledge_id, true)]).await?;
            standalone_run_ids.push(run_id);
        }

        assert_eq!(sweep_at_most(&pool, 2).await?, 4);
        for item_id in &item_ids[..2] {
            let stamp: Option<String> =
                sqlx::query_scalar("SELECT credited_at FROM run_knowledge WHERE item_id = ?")
                    .bind(item_id)
                    .fetch_one(&pool)
                    .await?;
            assert!(stamp.is_some());
        }
        for run_id in &standalone_run_ids[..2] {
            let stamp: Option<String> =
                sqlx::query_scalar("SELECT credited_at FROM run_knowledge WHERE run_id = ?")
                    .bind(run_id)
                    .fetch_one(&pool)
                    .await?;
            assert!(stamp.is_some());
        }
        let last_item_stamp: Option<String> =
            sqlx::query_scalar("SELECT credited_at FROM run_knowledge WHERE item_id = ?")
                .bind(item_ids[2])
                .fetch_one(&pool)
                .await?;
        assert!(last_item_stamp.is_none());
        let last_run_stamp: Option<String> =
            sqlx::query_scalar("SELECT credited_at FROM run_knowledge WHERE run_id = ?")
                .bind(standalone_run_ids[2])
                .fetch_one(&pool)
                .await?;
        assert!(last_run_stamp.is_none());
        let dead_stamp: Option<String> =
            sqlx::query_scalar("SELECT credited_at FROM run_knowledge WHERE run_id = ?")
                .bind(dead_run)
                .fetch_one(&pool)
                .await?;
        assert!(dead_stamp.is_none());

        assert_eq!(sweep_at_most(&pool, 2).await?, 2);
        assert_eq!(sweep_at_most(&pool, 2).await?, 0);
        assert_eq!(counters(&pool, dead_knowledge).await?, (0, 0, 0, None));
        Ok(())
    }

    #[tokio::test]
    async fn non_final_units_take_no_slot_in_a_sweep_pass() -> sqlx::Result<()> {
        let pool = test_pool().await;
        let stuck = seed(&pool, "machine", None, "stuck").await?;

        for _ in 0..(SWEEP_BATCH + 1) {
            let job_id = seed_job(&pool, "waiting", 1).await?;
            let run_id = seed_run(&pool, "completed", Some(0), None, Some(job_id)).await?;
            let item_id = seed_item(
                &pool,
                job_id,
                "gate_failed",
                Some("failed"),
                1,
                Some(run_id),
            )
            .await?;
            record(&pool, run_id, Some(item_id), &[scored(stuck, true)]).await?;
        }
        for _ in 0..(SWEEP_BATCH + 1) {
            let run_id = seed_run(&pool, "awaiting_approval", None, None, None).await?;
            record(&pool, run_id, None, &[scored(stuck, true)]).await?;
        }

        let final_item_knowledge = seed(&pool, "machine", None, "final item").await?;
        let final_job = seed_job(&pool, "completed", 1).await?;
        let final_item_run =
            seed_run(&pool, "completed", Some(0), Some("passed"), Some(final_job)).await?;
        let final_item = seed_item(
            &pool,
            final_job,
            "passed",
            Some("passed"),
            1,
            Some(final_item_run),
        )
        .await?;
        record(
            &pool,
            final_item_run,
            Some(final_item),
            &[scored(final_item_knowledge, true)],
        )
        .await?;

        let final_run_knowledge = seed(&pool, "machine", None, "final run").await?;
        let final_run = seed_run(&pool, "completed", Some(0), Some("passed"), None).await?;
        record(&pool, final_run, None, &[scored(final_run_knowledge, true)]).await?;

        assert_eq!(sweep(&pool).await?, 2);
        let final_item_counters = counters(&pool, final_item_knowledge).await?;
        assert_eq!(
            (
                final_item_counters.0,
                final_item_counters.1,
                final_item_counters.2
            ),
            (1, 1, 1)
        );
        assert!(final_item_counters.3.is_some());
        let final_run_counters = counters(&pool, final_run_knowledge).await?;
        assert_eq!(
            (
                final_run_counters.0,
                final_run_counters.1,
                final_run_counters.2
            ),
            (1, 1, 1)
        );
        assert!(final_run_counters.3.is_some());
        assert_eq!(counters(&pool, stuck).await?, (0, 0, 0, None));
        let uncredited: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM run_knowledge WHERE credited_at IS NULL")
                .fetch_one(&pool)
                .await?;
        assert_eq!(uncredited, 2 * (SWEEP_BATCH + 1));
        Ok(())
    }

    #[tokio::test]
    async fn the_sweep_selects_exactly_the_items_item_verdict_calls_final() -> sqlx::Result<()> {
        let pool = test_pool().await;
        let knowledge_id = seed(&pool, "machine", None, "agreement").await?;
        let statuses = [
            "pending",
            "running",
            "implemented",
            "merging",
            "conflicted",
            "reverted",
            "gate_failed",
            "passed",
            "failed",
            "cancelled",
            "gate_errored",
            "skipped",
            "superseded",
            "orphaned",
            "bogus",
        ];
        let mut expected = BTreeSet::new();
        let mut all = BTreeSet::new();

        for status in statuses {
            for (gate_attempts, gate_retries) in [(1, 1), (2, 1)] {
                for job_status in ["running", "completed"] {
                    let job_id = seed_job(&pool, job_status, gate_retries).await?;
                    let item_id =
                        seed_item(&pool, job_id, status, None, gate_attempts, None).await?;
                    let run_id = seed_run(&pool, "completed", Some(0), None, Some(job_id)).await?;
                    record(&pool, run_id, Some(item_id), &[scored(knowledge_id, true)]).await?;
                    all.insert(item_id);

                    let state = crate::job::item_state_from(status, gate_attempts, gate_retries);
                    let job_ended = crate::job::TERMINAL_STATUSES.contains(&job_status);
                    if item_verdict(state, None, job_ended, None).is_some() {
                        expected.insert(item_id);
                    }
                }
            }
        }

        sweep_at_most(&pool, 10_000).await?;
        let credited: BTreeSet<i64> = sqlx::query_scalar(
            "SELECT DISTINCT item_id
               FROM run_knowledge
              WHERE item_id IS NOT NULL AND credited_at IS NOT NULL",
        )
        .fetch_all(&pool)
        .await?
        .into_iter()
        .collect();

        assert_eq!(credited, expected);
        assert!(!expected.is_empty());
        assert_ne!(expected, all);
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
    async fn a_superseded_item_is_credited_as_its_stored_verdict_says_whichever_side_of_the_replan_it_is_swept()
    -> sqlx::Result<()> {
        for (item_status, gate_status, gate_attempts, (run_status, exit_code)) in [
            ("gate_failed", Some("failed"), 1, ("failed", Some(1))),
            ("failed", None, 0, ("failed", Some(1))),
        ] {
            for superseded_first in [false, true] {
                let pool = test_pool().await;
                let job_id = seed_job(&pool, "running", 0).await?;
                let run_id = seed_run(&pool, run_status, exit_code, None, Some(job_id)).await?;
                let item_id = seed_item(
                    &pool,
                    job_id,
                    item_status,
                    gate_status,
                    gate_attempts,
                    Some(run_id),
                )
                .await?;
                let knowledge_id = seed(&pool, "machine", None, item_status).await?;
                record(&pool, run_id, Some(item_id), &[scored(knowledge_id, true)]).await?;

                if superseded_first {
                    sqlx::query("UPDATE job_items SET status = 'superseded' WHERE id = ?")
                        .bind(item_id)
                        .execute(&pool)
                        .await?;
                }

                sweep(&pool).await?;
                let credited = counters(&pool, knowledge_id).await?;
                assert_eq!((credited.0, credited.1, credited.2), (1, 1, 0));
            }
        }
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
        assert_eq!(
            item_verdict(ItemState::Superseded, Some("passed"), false, None),
            Some(Verdict::Green)
        );
        assert_eq!(
            item_verdict(ItemState::Superseded, Some("failed"), false, None),
            Some(Verdict::NotGreen)
        );
        assert_eq!(
            item_verdict(ItemState::Superseded, Some("errored"), false, None),
            Some(Verdict::NoOutcome)
        );
        assert_eq!(
            item_verdict(
                ItemState::Superseded,
                None,
                false,
                Some(("failed", Some(1)))
            ),
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

    /// `BRIEFED_CONTEXTS` is a constant: this test fails the day a new context appears outside
    /// it. It is a list rather than discovery because the limits below make discovery incomplete.
    ///
    /// Launches do have a marker, `crate::runner::RunRequest {`, so this scan can find them. The
    /// append channel has no marker at all because it is ordinary `String` concatenation. The
    /// house gives that channel a marker by making `brief` the only module that produces the block:
    /// only `brief.rs` calls `knowledge::select`, and only `knowledge.rs` writes `PREAMBLE`. Without
    /// that decision this test is blind in exactly the direction from which `spawn_node` came.
    ///
    /// Its limits are deliberate and named. (a) Its granularity is the file, so a second launcher
    /// inside an already listed file is invisible. That is true today of `council::run_local_seat`,
    /// team's `local_agent::run_turn` branch, and `assistant::spawn_local_turn`/`errand_turn`: they
    /// build prompts and are deliberately not briefed. (b) It reads only `core/src/*.rs`, not
    /// subdirectories or other crates such as `shell/src-tauri` and `sidecars/`. (c) A launcher that
    /// does not spell `crate::runner::RunRequest {` (for example, imported `RunRequest {` or a
    /// builder) is invisible. (d) `map_seam.rs` is the nearest relative and a warning, not a
    /// precedent: it enforces the inverse direction. It derives `Seam::uncalled` on every read but
    /// never asserts it, because "Served routes no call here reaches -- and no screen is not
    /// nothing." A static scan does not see who calls over HTTP.
    #[test]
    fn no_context_builds_a_prompt_for_an_agent_without_going_through_the_one_producer() {
        let built_in = Path::new(env!("CARGO_MANIFEST_DIR"));
        let running_in = std::env::current_dir().expect("the working directory must be readable");
        assert_eq!(
            built_in,
            running_in.as_path(),
            "this test binary was compiled in {} and is running in {} -- a shared target directory \
             handed this checkout a binary built somewhere else, so this scan would read the other \
             checkout's sources. Touch this file to force a rebuild.",
            built_in.display(),
            running_in.display(),
        );

        let mut sources = fs::read_dir(running_in.join("src"))
            .expect("core source directory must be readable")
            .map(|entry| entry.expect("core source entry must be readable").path())
            .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("rs"))
            .map(|path| {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("core source file names must be UTF-8")
                    .to_owned();
                let source = fs::read_to_string(&path).expect("core source file must be readable");
                let source = source.replace("\r\n", "\n");
                let production = source
                    .split_once("\n#[cfg(test)]\nmod ")
                    .map_or(source.as_str(), |(production, _)| production)
                    .to_owned();
                (name, production)
            })
            .collect::<Vec<_>>();
        sources.sort_by(|left, right| left.0.cmp(&right.0));

        let expected = BRIEFED_CONTEXTS
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<BTreeSet<_>>();
        let callers = sources
            .iter()
            .filter(|(name, source)| {
                name != "brief.rs"
                    && (source.contains("brief::of(") || source.contains("brief::for_prompt("))
            })
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>();
        if let Some(file) = callers.difference(&expected).next() {
            panic!(
                "{file} calls brief::of/brief::for_prompt but is absent from BRIEFED_CONTEXTS; add the context to the constant"
            );
        }
        if let Some(file) = expected.difference(&callers).next() {
            panic!(
                "{file} is in BRIEFED_CONTEXTS but no longer calls brief::of/brief::for_prompt; remove the stale entry or restore briefing"
            );
        }

        let launchers = sources
            .iter()
            .filter(|(_, source)| source.contains("crate::runner::RunRequest {"))
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>();
        let unbriefed = UNBRIEFED_LAUNCHERS
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect::<BTreeSet<_>>();
        for file in &launchers {
            if !expected.contains(file) && !unbriefed.contains(file) {
                panic!(
                    "{file} builds a RunRequest and is in neither BRIEFED_CONTEXTS nor UNBRIEFED_LAUNCHERS: brief it through brief::for_prompt, or list it as unbriefed with the reason"
                );
            }
        }
        for (file, reason) in UNBRIEFED_LAUNCHERS {
            assert!(
                launchers.contains(*file),
                "{file} is stale in UNBRIEFED_LAUNCHERS ({reason}); remove it or restore the RunRequest launcher"
            );
        }

        for (file, source) in &sources {
            if file != "brief.rs"
                && (source.contains("knowledge::select(") || source.contains("knowledge::render("))
            {
                panic!(
                    "{file} produces a knowledge block outside brief.rs; route it through brief::of or brief::for_prompt"
                );
            }
            if file != "knowledge.rs"
                && source.contains("Earlier work on this project left the notes below")
            {
                panic!(
                    "{file} writes the knowledge preamble outside knowledge.rs; keep PREAMBLE's only definition in knowledge.rs"
                );
            }
        }
    }
}
