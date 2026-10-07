//! Context handoffs stay visible instead of rewriting a run's past.
//!
//! `/compact` rewrites history invisibly. Recording a handoff leaves an auditable event and a
//! successor run that a human can inspect, so context pressure never erases how work continued.

/// The run hands off once four fifths of the model's context window is occupied.
const HANDOFF_THRESHOLD_FRACTION: (i64, i64) = (4, 5);

pub fn should_hand_off(fill: i64, context_limit: i64, already_handed_off: bool) -> bool {
    if already_handed_off || context_limit <= 0 {
        return false;
    }

    let (numerator, denominator) = HANDOFF_THRESHOLD_FRACTION;
    fill.saturating_mul(denominator) >= context_limit.saturating_mul(numerator)
}

pub async fn record_handoff(
    pool: &sqlx::SqlitePool,
    run_id: i64,
    successor_run_id: i64,
    fill: i64,
) -> sqlx::Result<()> {
    let mut transaction = pool.begin().await?;

    sqlx::query("UPDATE runs SET successor_run_id = ? WHERE id = ?")
        .bind(successor_run_id)
        .bind(run_id)
        .execute(&mut *transaction)
        .await?;

    sqlx::query(
        "INSERT INTO run_events (run_id, seq, kind, payload, created_at)
         SELECT ?, COALESCE(MAX(seq), -1) + 1, 'context_handoff', ?, ?
         FROM run_events
         WHERE run_id = ?",
    )
    .bind(run_id)
    .bind(
        serde_json::json!({
            "fill": fill,
            "successor_run_id": successor_run_id
        })
        .to_string(),
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(run_id)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await
}

#[cfg(test)]
mod tests {
    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::storage::MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    #[test]
    fn a_handoff_fires_once_at_the_threshold_not_repeatedly() {
        // Policy contract: hand off at 80% of the model's context limit.
        let context_limit = 100_000;
        let threshold_fill = context_limit * 4 / 5;

        assert!(crate::handoff::should_hand_off(
            threshold_fill,
            context_limit,
            false
        ));
        assert!(!crate::handoff::should_hand_off(
            threshold_fill,
            context_limit,
            true
        ));
    }

    #[test]
    fn below_the_threshold_no_handoff_is_proposed() {
        assert!(!crate::handoff::should_hand_off(25_000, 100_000, false));
    }

    #[tokio::test]
    async fn a_handoff_records_an_event_and_links_the_successor() {
        let pool = test_pool().await;
        let run_id = 41_001_i64;
        let successor_run_id = 41_002_i64;
        let fill = 80_000_i64;
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, created_at)
             VALUES (?, 'original', 'completed', 'real', '2026-07-30T12:00:00Z'),
                    (?, 'successor', 'running', 'real', '2026-07-30T12:01:00Z')",
        )
        .bind(run_id)
        .bind(successor_run_id)
        .execute(&pool)
        .await
        .unwrap();

        crate::handoff::record_handoff(&pool, run_id, successor_run_id, fill)
            .await
            .unwrap();

        let events: Vec<(String, String)> = sqlx::query_as(
            "SELECT kind, payload
             FROM run_events
             WHERE run_id = ?",
        )
        .bind(run_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "context_handoff");
        assert!(
            events[0].1.contains(&fill.to_string()),
            "handoff payload must mention the context fill: {}",
            events[0].1
        );

        let linked_successor: Option<i64> =
            sqlx::query_scalar("SELECT successor_run_id FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(linked_successor, Some(successor_run_id));
    }
}
