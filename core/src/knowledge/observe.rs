//! Conversation observations: the write door and the age sweep. An observation is an `observed`
//! row in the `observation` scope, which no prompt reader selects. Spec 2026-10-22 B8, B11, B14.

use super::*;
use chrono::{DateTime, Utc};

/// How long an observation lives before the sweep deletes it (spec B14).
pub const OBSERVATION_MAX_AGE_DAYS: i64 = 60;

/// Record a conversation observation: episodic, the distiller's, `observed` (never `active`), in the
/// `observation` scope. `about` stays NULL on purpose (spec B11: only the pass proves it). Reaches no
/// prompt: `select::approved`/`admitted` accept `active` only and both prompt readers filter by scope.
pub async fn record_observation_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    scope_id: &str,
    kind: Kind,
    title: &str,
    body: &str,
    provenance: &Provenance<'_>,
) -> sqlx::Result<i64> {
    let now = Utc::now().to_rfc3339();
    let id = sqlx::query(
        "INSERT INTO knowledge
           (layer, scope_kind, scope_id, source, kind, title, body, fingerprint, evidence,
            points_at, distill_cause, status, created_at)
         VALUES ('episodic', 'observation', ?, 'distiller', ?, ?, ?, ?, ?, ?, ?, 'observed', ?)",
    )
    .bind(scope_id)
    .bind(kind.as_str())
    .bind(title)
    .bind(body)
    .bind(provenance.fingerprint)
    .bind(provenance.evidence)
    .bind(provenance.points_at)
    .bind(provenance.distill_cause)
    .bind(&now)
    .execute(&mut **tx)
    .await?
    .last_insert_rowid();

    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'observed', 'created', ?)",
    )
    .bind(id)
    .bind(&now)
    .execute(&mut **tx)
    .await?;

    Ok(id)
}

/// Delete observations older than [`OBSERVATION_MAX_AGE_DAYS`], with their vectors, embed-failure
/// marks and events. Needs no embedder. Returns the `knowledge` rows deleted.
pub async fn sweep_observations(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<u64> {
    let cutoff = (now - chrono::Duration::days(OBSERVATION_MAX_AGE_DAYS)).to_rfc3339();
    let stale = "SELECT id FROM knowledge WHERE scope_kind = 'observation' \
                 AND status IN ('observed','archived') AND created_at < ?1";
    let mut tx = pool.begin().await?;
    for table in [
        "knowledge_embeddings",
        "knowledge_embed_failures",
        "knowledge_events",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {table} WHERE knowledge_id IN ({stale})"
        )))
            .bind(&cutoff)
            .execute(&mut *tx)
            .await?;
    }
    let removed = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM knowledge WHERE id IN ({stale})"
    )))
        .bind(&cutoff)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_observation_is_observed_in_the_observation_scope_with_one_event() {
        let pool = crate::testdb::fresh_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let id = record_observation_in(
            &mut tx,
            "alpha:chat:5",
            Kind::Memory,
            "On 2026-10-20 the owner preferred tabs",
            "body",
            &Provenance {
                distill_cause: Some("chat_exchanges"),
                evidence: Some(r#"[{"t":"chat","id":7}]"#),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        #[allow(clippy::type_complexity)]
        let row: (
            String,
            String,
            Option<String>,
            String,
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<i64>,
        ) = sqlx::query_as(
            "SELECT layer, scope_kind, scope_id, source, status, about, expires_after_runs, distill_cause, proposal_id FROM knowledge WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            row,
            (
                "episodic".into(),
                "observation".into(),
                Some("alpha:chat:5".into()),
                "distiller".into(),
                "observed".into(),
                None,
                None,
                Some("chat_exchanges".into()),
                None
            )
        );
        let events: Vec<(Option<String>, String, String)> = sqlx::query_as(
            "SELECT from_status, to_status, note FROM knowledge_events WHERE knowledge_id = ?",
        )
        .bind(id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(events, vec![(None, "observed".into(), "created".into())]);
    }

    async fn observation_aged(
        pool: &SqlitePool,
        scope_id: &str,
        status: &str,
        at: DateTime<Utc>,
    ) -> i64 {
        let id = sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('episodic', 'observation', ?, 'distiller', 'memory', 't', 'b', ?, ?)",
        )
        .bind(scope_id)
        .bind(status)
        .bind(at.to_rfc3339())
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        sqlx::query(
            "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
             VALUES (?, NULL, ?, 'created', ?)",
        )
        .bind(id)
        .bind(status)
        .bind(at.to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn project_row_aged(pool: &SqlitePool, project: &str, at: DateTime<Utc>) -> i64 {
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('episodic', 'project', ?, 'distiller', 'memory', 't', 'b', 'active', ?)",
        )
        .bind(project)
        .bind(at.to_rfc3339())
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn vector_and_failure(pool: &SqlitePool, id: i64) {
        let mut conn = pool.acquire().await.unwrap();
        crate::embed::store_in(&mut conn, id, "m", &[1.0, 0.0])
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO knowledge_embed_failures (knowledge_id, model, failed_at)
             VALUES (?, 'm', '2026-10-01T00:00:00+00:00')",
        )
        .bind(id)
        .execute(&mut *conn)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn the_sweep_deletes_observations_over_sixty_days_old_through_all_four_tables() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now();
        let old = observation_aged(&pool, "alpha:chat:1", "observed", now - chrono::Duration::days(61)).await;
        let fresh = observation_aged(&pool, "alpha:chat:1", "observed", now - chrono::Duration::days(59)).await;
        let archived = observation_aged(&pool, "alpha:chat:2", "archived", now - chrono::Duration::days(70)).await;
        let lesson = project_row_aged(&pool, "alpha", now - chrono::Duration::days(100)).await;
        for id in [old, archived] {
            vector_and_failure(&pool, id).await;
        }

        let removed = sweep_observations(&pool, now).await.unwrap();
        assert_eq!(removed, 2);
        let left: Vec<i64> = sqlx::query_scalar("SELECT id FROM knowledge ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(left, vec![fresh.min(lesson), fresh.max(lesson)]);
        for table in [
            "knowledge_embeddings",
            "knowledge_embed_failures",
            "knowledge_events",
        ] {
            let orphans: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT COUNT(*) FROM {table} WHERE knowledge_id IN ({old}, {archived})"
            )))
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(orphans, 0, "{table}");
        }
    }
}
