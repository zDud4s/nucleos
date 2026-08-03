use serde::{Deserialize, Serialize};

/// What was asked for, as data.
///
/// Typed rather than a command string on purpose: a string would have to be parsed, and parsing
/// shell is the surface `classifier.rs` exists to keep closed. The daemon builds every argv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Merge { source: String, target: String },
}

impl Op {
    pub fn kind(&self) -> &'static str {
        match self {
            Op::Merge { .. } => "merge",
        }
    }

    pub fn to_args(&self) -> String {
        serde_json::to_string(self).expect("an Op is always serializable")
    }

    /// `kind` is the column, `args` the JSON payload. They are stored apart so the queue can be
    /// filtered by operation without parsing every row, which means they can also disagree — so
    /// the parse is checked against the column rather than trusted.
    pub fn from_stored(kind: &str, args: &str) -> Result<Self, String> {
        let parsed: Self = serde_json::from_str(args).map_err(|error| error.to_string())?;
        if parsed.kind() != kind {
            return Err(format!("stored op column {kind} disagrees with its payload"));
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    // Task 7 adds `use std::time::Duration;` when it first needs it — adding it now would warn as
    // an unused import on every run from here to Task 6.

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn insert(pool: &sqlx::SqlitePool, project: &str, status: &str) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, status, created_at)
             VALUES ('merge', '{}', ?, 'C:/repo', 'human', ?, '2026-08-02T00:00:00Z')",
        )
        .bind(project)
        .bind(status)
        .execute(pool)
        .await
        .map(|_| ())
    }

    /// Exclusivity is the database's job, not a Mutex's: a Mutex does not survive a daemon restart
    /// and this index does. Asserted sequentially on purpose — the constraint is what is under
    /// test, and the pool helper is `max_connections(1)`, so a "concurrent" version would prove
    /// less and flake more.
    #[tokio::test]
    async fn only_one_request_may_run_per_repository() {
        let pool = test_pool().await;

        insert(&pool, "alpha", "running").await.expect("the first running request is allowed");

        let second = insert(&pool, "alpha", "running").await;
        assert!(second.is_err(), "a second running request for the same repository must be rejected");

        insert(&pool, "beta", "running")
            .await
            .expect("a different repository is not blocked by alpha's running request");

        for _ in 0..3 {
            insert(&pool, "alpha", "queued")
                .await
                .expect("queued requests are not limited — only running is");
        }
    }

    /// Round-tripping through the stored form is the point: the row is the contract between the
    /// submitting process and the worker, which may be a daemon restart apart.
    #[test]
    fn an_operation_round_trips_through_its_stored_form() {
        let op = Op::Merge { source: "feat/x".into(), target: "master".into() };
        let back = Op::from_stored(op.kind(), &op.to_args()).expect("a stored operation must parse back");
        assert_eq!(back, op);
    }

    #[test]
    fn an_unknown_operation_is_refused_rather_than_guessed() {
        assert!(Op::from_stored("rm_rf", "{}").is_err());
    }

    /// The column and the payload can disagree — a row edited by hand, or a bug that wrote one
    /// without the other. Trusting the payload would let a `merge` row execute as something else the
    /// moment a second variant exists.
    ///
    /// NOTE: with a single variant this refusal comes from serde's unknown-tag error, not from the
    /// `kind` comparison — every payload that parses at all is a `Merge`, so that branch is
    /// unreachable by construction today. The guard is written now because the moment Chunk 4 adds
    /// `Push` it stops being unreachable and starts being the thing that prevents a merge row from
    /// executing as a push. **Chunk 4 must add the case that actually covers it:**
    /// `Op::from_stored("push", <a merge payload>)`.
    #[test]
    fn a_payload_that_contradicts_its_column_is_refused() {
        assert!(Op::from_stored("merge", r#"{"op":"rm_rf"}"#).is_err());
    }
}
