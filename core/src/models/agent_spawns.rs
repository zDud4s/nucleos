//! The record of subagent spawns and how each one ended: what the `agents` router surface decided,
//! and the evidence escalation and the cost report read back. A failed or re-dispatched task is the
//! signal that the next spawn of the same description should start one rung higher.
//!
//! Nothing here fails the hook: callers log and carry on, a missing row only means less evidence.

use sqlx::SqlitePool;
use std::collections::HashSet;

use super::agent_route::{Rung, rung_of_model_id};

/// Descriptions at or above this word overlap are the same task dispatched again.
const SIMILAR_JACCARD: f64 = 0.8;

/// How a spawn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnOutcome {
    Ok,
    Error,
}

impl SpawnOutcome {
    fn as_str(self) -> &'static str {
        match self {
            SpawnOutcome::Ok => "ok",
            SpawnOutcome::Error => "error",
        }
    }
}

/// A spawn about to be recorded.
#[derive(Debug, Clone)]
pub struct NewSpawn {
    pub session_id: String,
    pub subagent_type: String,
    pub description: String,
    pub model: Option<String>,
    pub source: &'static str,
    pub mode: &'static str,
    pub advised_model: Option<String>,
    pub route_decision_id: Option<String>,
    pub failed: Vec<String>,
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn word_set(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether two descriptions read as the same task: word sets with Jaccard overlap of at least 0.8.
pub fn similar(a: &str, b: &str) -> bool {
    let (a, b) = (word_set(a), word_set(b));
    if a.is_empty() || b.is_empty() {
        return a.is_empty() && b.is_empty();
    }
    let both = a.intersection(&b).count() as f64;
    let either = a.union(&b).count() as f64;
    both / either >= SIMILAR_JACCARD
}

fn rung_of(model: &str) -> Option<Rung> {
    Rung::parse_alias(&model.to_ascii_lowercase()).or_else(|| rung_of_model_id(model))
}

/// Records a spawn and returns its row id.
pub async fn record_spawn(pool: &SqlitePool, spawn: &NewSpawn) -> Result<i64, sqlx::Error> {
    record_spawn_at(pool, spawn, unix_now()).await
}

async fn record_spawn_at(
    pool: &SqlitePool,
    spawn: &NewSpawn,
    now: i64,
) -> Result<i64, sqlx::Error> {
    let failed = serde_json::to_string(&spawn.failed).unwrap_or_else(|_| "[]".into());
    let done = sqlx::query(
        "INSERT INTO agent_spawns (session_id, subagent_type, description, model, source, mode,
                                   advised_model, route_decision_id, failed, spawned_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&spawn.session_id)
    .bind(&spawn.subagent_type)
    .bind(&spawn.description)
    .bind(&spawn.model)
    .bind(spawn.source)
    .bind(spawn.mode)
    .bind(&spawn.advised_model)
    .bind(&spawn.route_decision_id)
    .bind(failed)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(done.last_insert_rowid())
}

/// Links an outcome to the most recent still-open spawn of the session (of `agent_type` when given).
/// A failure only warns: the hook that reports this must never be held up by it.
pub async fn record_outcome(
    pool: &SqlitePool,
    session_id: &str,
    agent_id: Option<&str>,
    agent_type: Option<&str>,
    outcome: SpawnOutcome,
    cost_usd: Option<f64>,
) -> bool {
    record_outcome_at(
        pool,
        session_id,
        agent_id,
        agent_type,
        outcome,
        cost_usd,
        unix_now(),
    )
    .await
}

async fn record_outcome_at(
    pool: &SqlitePool,
    session_id: &str,
    agent_id: Option<&str>,
    agent_type: Option<&str>,
    outcome: SpawnOutcome,
    cost_usd: Option<f64>,
    now: i64,
) -> bool {
    let _ = agent_id; // Claude Code's id is not known at spawn time, so it cannot key the link.
    let done = sqlx::query(
        "UPDATE agent_spawns SET ended_at = ?, outcome = ?, cost_usd = ?
         WHERE id = (SELECT id FROM agent_spawns
                     WHERE session_id = ? AND ended_at IS NULL
                       AND (? IS NULL OR subagent_type = ?)
                     ORDER BY spawned_at DESC, id DESC LIMIT 1)",
    )
    .bind(now)
    .bind(outcome.as_str())
    .bind(cost_usd)
    .bind(session_id)
    .bind(agent_type)
    .bind(agent_type)
    .execute(pool)
    .await;
    match done {
        Ok(r) => r.rows_affected() > 0,
        Err(e) => {
            tracing::warn!("agent spawn outcome not recorded: {e}");
            false
        }
    }
}

/// The rungs that already failed this task in this session: spawns of a near-identical description
/// that ended in error, or that were dispatched again afterwards.
pub async fn recent_failures(
    pool: &SqlitePool,
    session_id: &str,
    description: &str,
    window_secs: i64,
) -> Vec<Rung> {
    recent_failures_at(pool, session_id, description, window_secs, unix_now()).await
}

async fn recent_failures_at(
    pool: &SqlitePool,
    session_id: &str,
    description: &str,
    window_secs: i64,
    now: i64,
) -> Vec<Rung> {
    let rows: Vec<(Option<String>, Option<String>, Option<String>)> = match sqlx::query_as(
        "SELECT description, model, outcome FROM agent_spawns
         WHERE session_id = ? AND spawned_at >= ? ORDER BY spawned_at, id",
    )
    .bind(session_id)
    .bind(now - window_secs)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("agent spawn failures not read: {e}");
            return Vec::new();
        }
    };
    let same: Vec<_> = rows
        .iter()
        .filter(|(d, _, _)| similar(d.as_deref().unwrap_or(""), description))
        .collect();
    let mut out = Vec::new();
    for (pos, (_, model, outcome)) in same.iter().enumerate() {
        let redispatched = pos + 1 < same.len();
        if (outcome.as_deref() == Some("error") || redispatched)
            && let Some(rung) = model.as_deref().and_then(rung_of)
            && !out.contains(&rung)
        {
            out.push(rung);
        }
    }
    out
}

/// Marks spawns that never reported an outcome and are older than `older_than_secs` as `lost`, so a
/// dead agent is not "in progress" for ever. Returns how many.
pub async fn sweep_lost(pool: &SqlitePool, older_than_secs: i64) -> u64 {
    sweep_lost_at(pool, older_than_secs, unix_now()).await
}

async fn sweep_lost_at(pool: &SqlitePool, older_than_secs: i64, now: i64) -> u64 {
    sqlx::query(
        "UPDATE agent_spawns SET outcome = 'lost', ended_at = ?
         WHERE ended_at IS NULL AND spawned_at < ?",
    )
    .bind(now)
    .bind(now - older_than_secs)
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::agent_route::{SOURCE_LADDER, SOURCE_ROUTER};

    const NOW: i64 = 1_800_000_000;
    const WINDOW: i64 = 600;

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    fn spawn(session: &str, description: &str, model: &str) -> NewSpawn {
        NewSpawn {
            session_id: session.into(),
            subagent_type: "worker".into(),
            description: description.into(),
            model: Some(model.into()),
            source: SOURCE_LADDER,
            mode: "shadow",
            advised_model: None,
            route_decision_id: None,
            failed: Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_spawn_and_its_outcome_land_in_one_row() {
        let pool = pool().await;
        let mut s = spawn("s1", "find the retry code", "haiku");
        s.source = SOURCE_ROUTER;
        s.advised_model = Some("haiku".into());
        record_spawn_at(&pool, &s, NOW).await.unwrap();
        assert!(
            record_outcome_at(
                &pool,
                "s1",
                None,
                Some("worker"),
                SpawnOutcome::Ok,
                Some(0.5),
                NOW + 5
            )
            .await
        );
        let row: (i64, String, f64, String, Option<String>) = sqlx::query_as(
            "SELECT ended_at, outcome, cost_usd, source, advised_model FROM agent_spawns",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            row,
            (
                NOW + 5,
                "ok".into(),
                0.5,
                "router".into(),
                Some("haiku".into())
            )
        );
        let rows: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM agent_spawns")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows.0, 1);
    }

    #[tokio::test]
    async fn an_errored_agent_is_a_failure_for_the_next_request() {
        let pool = pool().await;
        record_spawn_at(&pool, &spawn("s1", "fix the parser", "sonnet"), NOW)
            .await
            .unwrap();
        record_outcome_at(&pool, "s1", None, None, SpawnOutcome::Error, None, NOW + 10).await;
        let got = recent_failures_at(&pool, "s1", "fix the parser", WINDOW, NOW + 20).await;
        assert_eq!(got, vec![Rung::Sonnet]);
    }

    #[tokio::test]
    async fn a_near_identical_redispatch_counts_as_the_first_having_failed() {
        let pool = pool().await;
        record_spawn_at(
            &pool,
            &spawn("s1", "fix the parser for nested lists", "haiku"),
            NOW,
        )
        .await
        .unwrap();
        record_spawn_at(
            &pool,
            &spawn("s1", "Fix the parser for nested lists.", "sonnet"),
            NOW + 30,
        )
        .await
        .unwrap();
        let got = recent_failures_at(
            &pool,
            "s1",
            "fix the parser for nested lists",
            WINDOW,
            NOW + 40,
        )
        .await;
        // Only the first spawn failed: the redispatch is the attempt in flight, not a failure.
        assert_eq!(got, vec![Rung::Haiku]);
    }

    #[tokio::test]
    async fn a_different_description_or_session_is_not_a_failure() {
        let pool = pool().await;
        record_spawn_at(&pool, &spawn("s1", "fix the parser", "sonnet"), NOW)
            .await
            .unwrap();
        record_outcome_at(&pool, "s1", None, None, SpawnOutcome::Error, None, NOW + 1).await;
        assert!(
            recent_failures_at(&pool, "s2", "fix the parser", WINDOW, NOW + 2)
                .await
                .is_empty()
        );
        assert!(
            recent_failures_at(&pool, "s1", "write the release notes", WINDOW, NOW + 2)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn failures_outside_the_window_are_forgotten() {
        let pool = pool().await;
        record_spawn_at(&pool, &spawn("s1", "fix the parser", "sonnet"), NOW)
            .await
            .unwrap();
        record_outcome_at(&pool, "s1", None, None, SpawnOutcome::Error, None, NOW + 1).await;
        assert!(
            recent_failures_at(&pool, "s1", "fix the parser", WINDOW, NOW + WINDOW + 5)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_spawn_with_no_outcome_for_too_long_is_not_counted_as_open_forever() {
        let pool = pool().await;
        record_spawn_at(&pool, &spawn("s1", "fix the parser", "sonnet"), NOW)
            .await
            .unwrap();
        let later = NOW + WINDOW + 5;
        assert!(
            recent_failures_at(&pool, "s1", "fix the parser", WINDOW, later)
                .await
                .is_empty()
        );
        assert_eq!(sweep_lost_at(&pool, WINDOW, later).await, 1);
        let outcome: (String,) = sqlx::query_as("SELECT outcome FROM agent_spawns")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(outcome.0, "lost");
        assert_eq!(sweep_lost_at(&pool, WINDOW, later).await, 0);
    }

    #[test]
    fn similarity_is_word_overlap_not_substring() {
        assert!(similar("Fix the parser!", "fix the parser"));
        assert!(!similar("fix the parser", "fix the lexer"));
        assert!(!similar("fix", ""));
    }
}
