//! Where a distilled `knowledge` row came from: its `distill_cause`, which `knowledge::Known` does not carry, served read-only to /learned.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use sqlx::SqlitePool;

/// The cause a distilled row was written for (`job_landed`, `job_failed`, ...), by row id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, sqlx::FromRow)]
pub struct DistillCause {
    pub id: i64,
    pub cause: String,
}

/// Every distilled row that records a cause, newest first. Owner and consolidator rows carry no
/// `distill_cause` and a distilled row written before the column existed has none either, so
/// neither is listed.
pub async fn causes(pool: &SqlitePool) -> sqlx::Result<Vec<DistillCause>> {
    sqlx::query_as::<_, DistillCause>(
        "SELECT id, distill_cause AS cause FROM knowledge
         WHERE source = 'distiller' AND distill_cause IS NOT NULL
         ORDER BY id DESC LIMIT 2000",
    )
    .fetch_all(pool)
    .await
}

/// `GET /distill/causes`. A database error is a bare 500 and the log carries the error only,
/// never a row's text.
pub async fn get_distill_causes(
    State(state): State<crate::state::AppState>,
) -> Result<Json<Vec<DistillCause>>, StatusCode> {
    match causes(&state.pool).await {
        Ok(rows) => Ok(Json(rows)),
        Err(error) => {
            tracing::warn!(%error, "listing distill causes failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// A row the distiller judged a near-duplicate of an older one, by row id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, sqlx::FromRow)]
pub struct NearDuplicate {
    pub id: i64,
    pub of_id: i64,
}

/// Every row with a `near_duplicate:<id>` event, newest event first, so a row noted twice lists
/// its latest pointer first. An event whose note carries no usable id is skipped.
pub async fn near_duplicates(pool: &SqlitePool) -> sqlx::Result<Vec<NearDuplicate>> {
    let prefix = crate::knowledge::NOTE_NEAR_DUPLICATE;
    sqlx::query_as::<_, NearDuplicate>(
        "SELECT knowledge_id AS id, CAST(substr(note, ?) AS INTEGER) AS of_id
         FROM knowledge_events
         WHERE substr(note, 1, ?) = ? AND CAST(substr(note, ?) AS INTEGER) > 0
         ORDER BY id DESC LIMIT 2000",
    )
    .bind(prefix.len() as i64 + 1)
    .bind(prefix.len() as i64)
    .bind(prefix)
    .bind(prefix.len() as i64 + 1)
    .fetch_all(pool)
    .await
}

/// `GET /distill/duplicates`. Same error policy as `get_distill_causes`.
pub async fn get_near_duplicates(
    State(state): State<crate::state::AppState>,
) -> Result<Json<Vec<NearDuplicate>>, StatusCode> {
    match near_duplicates(&state.pool).await {
        Ok(rows) => Ok(Json(rows)),
        Err(error) => {
            tracing::warn!(%error, "listing near-duplicates failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::{self, Declaration, Kind, Provenance};

    async fn test_pool() -> sqlx::SqlitePool {
        crate::testdb::fresh_pool().await
    }

    /// /learned marks a row "possible duplicate of #N" from the event `distill` writes; an event
    /// of any other note, and a malformed near-duplicate note, must not produce a mark.
    #[tokio::test]
    async fn a_near_duplicate_event_lists_the_row_it_points_to() {
        let pool = test_pool().await;
        let declaration = |title| Declaration {
            project_id: Some("p"),
            origin_run_id: None,
            kind: Kind::Memory,
            title,
            body: "b",
            reasoning: "r",
            supersedes: None,
        };

        let mut tx = pool.begin().await.unwrap();
        let older =
            knowledge::record_distilled(&mut tx, &declaration("older"), &Provenance::default())
                .await
                .unwrap();
        let newer =
            knowledge::record_distilled(&mut tx, &declaration("newer"), &Provenance::default())
                .await
                .unwrap();
        knowledge::note_near_duplicate_in(&mut tx, newer, older)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
             VALUES (?, 'proposed', 'proposed', 'near_duplicate:x', 'now')",
        )
        .bind(older)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(
            near_duplicates(&pool).await.unwrap(),
            vec![NearDuplicate {
                id: newer,
                of_id: older,
            }]
        );
    }

    /// The listing is for /learned's "distilled from job #N, because ..." line, so it carries
    /// the distiller's rows and nobody else's: an owner row has no cause and must not appear.
    #[tokio::test]
    async fn only_distilled_rows_are_listed_with_their_cause() {
        let pool = test_pool().await;

        let mut tx = pool.begin().await.unwrap();
        let distilled_id = knowledge::record_distilled(
            &mut tx,
            &Declaration {
                project_id: Some("p"),
                origin_run_id: None,
                kind: Kind::Memory,
                title: "t",
                body: "b",
                reasoning: "r",
                supersedes: None,
            },
            &Provenance {
                distill_cause: Some("job_failed"),
                evidence: Some(r#"[{"t":"job","id":7},{"t":"run","id":3}]"#),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        knowledge::propose_in_with(
            &mut tx,
            Declaration {
                project_id: Some("p"),
                origin_run_id: None,
                kind: Kind::Memory,
                title: "an owner's note",
                body: "written by hand",
                reasoning: "r",
                supersedes: None,
            },
            &Provenance::default(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(
            causes(&pool).await.unwrap(),
            vec![DistillCause {
                id: distilled_id,
                cause: "job_failed".to_string(),
            }]
        );
    }
}
