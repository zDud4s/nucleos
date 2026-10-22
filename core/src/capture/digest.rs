//! The daily digest of capture questions (spec 2026-10-22 B7, section 7).

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    #[tokio::test]
    async fn one_digest_per_local_day_and_requests_carry_the_digest_id() {
        let pool = pool().await;
        for (day, ids) in [("2026-10-07", "[7]"), ("2026-10-08", "[8]")] {
            sqlx::query("INSERT INTO capture_digests (day, job_ids, created_at) VALUES (?, ?, 't')")
                .bind(day)
                .bind(ids)
                .execute(&pool)
                .await
                .unwrap();
        }
        let again = sqlx::query(
            "INSERT INTO capture_digests (day, job_ids, created_at) VALUES ('2026-10-07', '[9]', 't')",
        )
        .execute(&pool)
        .await;
        assert!(again.is_err(), "a second digest for the same day must be refused");

        sqlx::query(
            "INSERT INTO capture_requests (job_id, project_id, causes, prompt_text, state, deadline, created_at)
             VALUES (7, 'p', '[]', 'q', 'open', 'd', 't')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let digest_id: Option<i64> =
            sqlx::query_scalar("SELECT digest_id FROM capture_requests WHERE job_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(digest_id, None, "a request opened without a digest has none");
    }
}
