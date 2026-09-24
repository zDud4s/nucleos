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

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::{Row, SqlitePool};

    use super::{match_expression, normalise_fts, of, record};
    use crate::knowledge::{Context, Scope};

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
}
