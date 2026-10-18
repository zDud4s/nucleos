//! Pipeline and autopilot health for the analytics page (spec 2026-10-17-analytics-pipeline).
//!
//! Read side: one report per scope and window, built block by block; a block that fails becomes
//! `{"error": ...}` and the others still come (spec D5). Write side lives in `sampler` (slot
//! samples) and in `verify::postgate::record_transition` (colour changes).

#[cfg(test)]
mod tests {
    /// Spec §10: the three history tables exist with the columns and constraints it names.
    #[tokio::test]
    async fn the_history_tables_take_the_spec_columns() {
        let pool = crate::testdb::fresh_pool().await;

        sqlx::query(
            "INSERT INTO slot_samples (sampled_at, project_id, in_use, slot_limit)
             VALUES ('2026-10-17T10:00:00Z', NULL, 1, 4), ('2026-10-17T10:00:00Z', 'p', 1, 2)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // The house is '' in the daily table, never NULL, so it can sit in the primary key.
        sqlx::query(
            "INSERT INTO slot_samples_daily (day, samples, avg_in_use, max_in_use, slot_limit, at_limit_samples)
             VALUES ('2026-10-16', 1440, 0.5, 2, 4, 10)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let house: String = sqlx::query_scalar("SELECT project_id FROM slot_samples_daily")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(house, "");
        let twice = sqlx::query(
            "INSERT INTO slot_samples_daily (day, project_id, samples, avg_in_use, max_in_use, slot_limit, at_limit_samples)
             VALUES ('2026-10-16', '', 1, 0, 0, 4, 0)",
        )
        .execute(&pool)
        .await;
        assert!(twice.is_err(), "one daily row per (day, scope)");

        sqlx::query("INSERT INTO postgate_transitions (project_id, at, to_state) VALUES ('p', '2026-10-17T10:00:00Z', 'red')")
            .execute(&pool)
            .await
            .unwrap();
        let (groups, reverted): (String, i64) =
            sqlx::query_as("SELECT red_groups, reverted FROM postgate_transitions")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((groups.as_str(), reverted), ("[]", 0));
        let amber = sqlx::query("INSERT INTO postgate_transitions (project_id, at, to_state) VALUES ('p', '2026-10-17T10:00:00Z', 'amber')")
            .execute(&pool)
            .await;
        assert!(amber.is_err(), "to_state is red or green only");
    }
}
