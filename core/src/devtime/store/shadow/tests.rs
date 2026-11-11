//! Tests of the shadow log's retention prune (the rest of the shadow tests sit in `shadow.rs`).

use super::*;

async fn insert(
    pool: &SqlitePool,
    key: &str,
    state: &str,
    resolved_at: Option<&str>,
    decided: &str,
) {
    sqlx::query(
        "INSERT INTO devtime_shadow (action, rule_id, project_id, session_id, subject_key, mode, \
         decision, template_id, rule_version, action_version, thresholds_hash, rules_version, \
         decided_at, window_end_at, outcome_state, resolved_at) \
         VALUES ('inject', 'D3', 'p', 's', ?, 'shadow', 'fire', 't', 1, 1, 'h', 'rv', ?, ?, ?, ?)",
    )
    .bind(key)
    .bind(decided)
    .bind(decided)
    .bind(state)
    .bind(resolved_at)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn prune_closed_before_deletes_only_old_closed_rows() {
    let pool = crate::testdb::fresh_pool().await;
    let old = "2026-01-01T00:00:00.000Z";
    let new = "2026-10-01T00:00:00.000Z";
    let cutoff = "2026-06-01T00:00:00.000Z";
    insert(&pool, "old-resolved", "resolved", Some(old), old).await;
    insert(&pool, "old-expired", "expired", Some(old), old).await;
    insert(&pool, "old-void-no-resolved-at", "void", None, old).await;
    insert(&pool, "old-pending", "pending", None, old).await;
    insert(&pool, "new-resolved", "resolved", Some(new), old).await;
    insert(&pool, "new-void", "void", None, new).await;

    assert_eq!(prune_closed_before(&pool, cutoff).await.unwrap(), 3);
    let left: Vec<String> =
        sqlx::query_scalar("SELECT subject_key FROM devtime_shadow ORDER BY subject_key")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(left, ["new-resolved", "new-void", "old-pending"]);
    assert_eq!(prune_closed_before(&pool, cutoff).await.unwrap(), 0);
}
