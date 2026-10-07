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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::{self, Declaration, Kind, Provenance};

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        crate::storage::MIGRATOR.run(&pool).await.unwrap();
        pool
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
