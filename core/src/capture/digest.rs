//! The daily digest of capture questions (spec 2026-10-22 B7, section 7).

pub fn render(day: &str, lines: &[String], more: usize, until: &str) -> String {
    let mut out = format!("Daily questions · {day} · {} to answer\n", lines.len() + more);
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    if more > 0 {
        out.push_str(&format!("and {more} more in the Brain\n"));
    }
    out.push_str(&format!(
        "Reply to this message (when only one is open) or send /cap <job> <answer>. Open until {until}."
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn the_digest_text_matches_the_template() {
        let lines = vec![
            "#7 · api · job #7 — the job ended in `failed`. What went wrong that the logs would not show?".to_owned(),
            "#9 · api · job #9 — a. b?".to_owned(),
        ];
        assert_eq!(
            render("2026-10-22", &lines, 0, "2026-10-27"),
            "Daily questions · 2026-10-22 · 2 to answer\n\
             #7 · api · job #7 — the job ended in `failed`. What went wrong that the logs would not show?\n\
             #9 · api · job #9 — a. b?\n\
             Reply to this message (when only one is open) or send /cap <job> <answer>. Open until 2026-10-27."
        );
        let text = render("2026-10-22", &lines, 3, "2026-10-27");
        assert!(text.starts_with("Daily questions · 2026-10-22 · 5 to answer\n"), "{text}");
        assert!(text.contains("\nand 3 more in the Brain\n"), "{text}");
    }
}
