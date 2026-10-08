//! What an unattended worktree run did by itself that the `verify` tool exists for.
//!
//! F2b step 1 (spec 2026-10-05 §9, metric 7): one `verification_observations` row per
//! verification command a run executed on its own, and `deviation`, those rows over the `verify`
//! units of the same project in a window. Observe only — recording is best effort and never
//! decides anything.

use sqlx::SqlitePool;

use crate::verify_guard::Hit;
use crate::verify_runs::ORIGIN_VERIFY;

/// One verification command a run executed itself, as the hook saw it.
pub struct Observation<'a> {
    pub project_id: &'a str,
    pub run_id: i64,
    /// The shadow decision the same call produced, when there is one.
    pub shadow_decision_id: Option<i64>,
    pub tool_name: &'a str,
    pub command: &'a str,
    pub hit: &'a Hit,
    /// What the classifier decided about the call; recorded, never changed here.
    pub decision: &'a str,
}

/// Appends one observation and returns its row id. Append-only: a repeat is a second row.
pub async fn record(pool: &SqlitePool, o: &Observation<'_>) -> sqlx::Result<i64> {
    let done = sqlx::query(
        "INSERT INTO verification_observations (project_id, run_id, shadow_decision_id, tool_name, \
         command, kind, name, segment, decision, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(o.project_id)
    .bind(o.run_id)
    .bind(o.shadow_decision_id)
    .bind(o.tool_name)
    .bind(o.command)
    .bind(o.hit.kind.as_str())
    .bind(&o.hit.name)
    .bind(&o.hit.segment)
    .bind(o.decision)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(done.last_insert_rowid())
}

/// Metric 7 over a window: observations against the `verify` units of the same project.
#[derive(Debug, Clone, PartialEq)]
pub struct Deviation {
    /// Verification commands runs executed themselves.
    pub observed: i64,
    /// Units the `verify` tool ran for the project, one per `verify_runs` row it produced.
    pub verify_units: i64,
    /// `observed / verify_units`; `None` when there are no units, where it would divide by zero.
    pub ratio: Option<f64>,
}

/// The deviation for `project_id` from `since` (inclusive) to now.
///
/// Every timestamp compared here is a chrono `to_rfc3339()` string in UTC, so the text order of
/// two of them is their time order and `>=` on the column is a window.
pub async fn deviation(
    pool: &SqlitePool,
    project_id: &str,
    since: &str,
) -> sqlx::Result<Deviation> {
    let observed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM verification_observations WHERE project_id = ? AND created_at >= ?",
    )
    .bind(project_id)
    .bind(since)
    .fetch_one(pool)
    .await?;
    // A unit belongs to the window of the ticket that asked for it, and only `verify` rows carry a
    // ticket id in `origin_id` (a gate row's points at a run).
    let verify_units: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM verify_runs vr JOIN verify_requests rq ON vr.origin_id = rq.id \
         WHERE vr.origin = ? AND rq.project_id = ? AND rq.created_at >= ?",
    )
    .bind(ORIGIN_VERIFY)
    .bind(project_id)
    .bind(since)
    .fetch_one(pool)
    .await?;
    let ratio = (verify_units > 0).then(|| observed as f64 / verify_units as f64);
    Ok(Deviation {
        observed,
        verify_units,
        ratio,
    })
}

#[cfg(test)]
mod tests {
    use super::{Deviation, Observation, deviation, record};
    use crate::verify_guard::{Hit, Kind};
    use crate::verify_runs::{self, ORIGIN_RUN, ORIGIN_VERIFY};

    fn a_hit() -> Hit {
        Hit {
            kind: Kind::Tool,
            name: "cargo".to_string(),
            segment: "cargo test -p nucleos-core".to_string(),
        }
    }

    /// Records one observation for `project` and returns its row id.
    async fn observe(pool: &sqlx::SqlitePool, project: &str, hit: &Hit) -> i64 {
        record(
            pool,
            &Observation {
                project_id: project,
                run_id: 7,
                shadow_decision_id: None,
                tool_name: "Bash",
                command: "time cargo test -p nucleos-core",
                hit,
                decision: "allow",
            },
        )
        .await
        .unwrap()
    }

    /// Moves an observation to a chosen instant, so a window has something to exclude.
    async fn stamp_observation(pool: &sqlx::SqlitePool, id: i64, created_at: &str) {
        sqlx::query("UPDATE verification_observations SET created_at = ? WHERE id = ?")
            .bind(created_at)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// A `verify_requests` ticket for `project`, created at `created_at`.
    async fn request(pool: &sqlx::SqlitePool, project: &str, created_at: &str) -> i64 {
        sqlx::query(
            "INSERT INTO verify_requests (project_id, worktree, kind, scope, priority, caller, \
             created_at) VALUES (?, '/w', 'test', 'changed', 1, 'agent', ?)",
        )
        .bind(project)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// A `verify_runs` unit of `origin`, pointing at `origin_id`.
    async fn unit(pool: &sqlx::SqlitePool, project: &str, origin: &str, origin_id: i64) {
        verify_runs::record(
            pool,
            &verify_runs::Row {
                project_id: Some(project),
                worktree: "/w",
                sha: None,
                scope: verify_runs::SCOPE_FULL,
                origin,
                origin_id: Some(origin_id),
                ordinal: None,
                requested_by: verify_runs::REQUESTED_BY_GATE,
                group_name: None,
                kind: None,
                argv: "cargo test",
                fingerprint: None,
                status: verify_runs::STATUS_PASSED,
                exit_code: Some(0),
                duration_ms: 1,
                started_at: "2026-10-05T10:00:00+00:00",
                finished_at: "2026-10-05T10:00:01+00:00",
                output_tail: None,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn an_observation_is_recorded_with_what_the_hit_found() {
        let pool = crate::testdb::fresh_pool().await;
        let hit = a_hit();
        let id = record(
            &pool,
            &Observation {
                project_id: "p1",
                run_id: 42,
                shadow_decision_id: Some(9),
                tool_name: "Bash",
                command: "time cargo test -p nucleos-core",
                hit: &hit,
                decision: "allow",
            },
        )
        .await
        .unwrap();

        type Stored = (
            String,
            i64,
            Option<i64>,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
        );
        let row: Stored = sqlx::query_as(
            "SELECT project_id, run_id, shadow_decision_id, tool_name, command, kind, name, \
             segment, decision, created_at FROM verification_observations WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "p1");
        assert_eq!(row.1, 42);
        assert_eq!(row.2, Some(9));
        assert_eq!(row.3, "Bash");
        assert_eq!(row.4, "time cargo test -p nucleos-core");
        assert_eq!(row.5, hit.kind.as_str());
        assert_eq!(row.6, "cargo");
        assert_eq!(row.7, "cargo test -p nucleos-core");
        assert_eq!(row.8, "allow");
        // `deviation` compares this as text, so it must be the rfc3339 form every other row uses.
        assert!(chrono::DateTime::parse_from_rfc3339(&row.9).is_ok());

        // A second call is a second row: observing is append-only and never merges.
        let again = observe(&pool, "p1", &hit).await;
        assert_ne!(again, id);
    }

    #[tokio::test]
    async fn the_window_and_the_project_bound_what_deviation_counts() {
        let pool = crate::testdb::fresh_pool().await;
        let hit = a_hit();
        let before = observe(&pool, "p1", &hit).await;
        stamp_observation(&pool, before, "2026-10-01T09:59:59+00:00").await;
        let inside = observe(&pool, "p1", &hit).await;
        stamp_observation(&pool, inside, "2026-10-01T10:00:00+00:00").await;
        let other_project = observe(&pool, "p2", &hit).await;
        stamp_observation(&pool, other_project, "2026-10-02T10:00:00+00:00").await;

        let d: Deviation = deviation(&pool, "p1", "2026-10-01T10:00:00+00:00")
            .await
            .unwrap();
        assert_eq!(d.observed, 1, "only the in-window row of this project");
        assert_eq!(d.verify_units, 0);
    }

    #[tokio::test]
    async fn only_verify_units_of_this_project_inside_the_window_are_counted() {
        let pool = crate::testdb::fresh_pool().await;
        let since = "2026-10-01T10:00:00+00:00";
        let old = request(&pool, "p1", "2026-09-30T10:00:00+00:00").await;
        let inside = request(&pool, "p1", "2026-10-02T10:00:00+00:00").await;
        let other = request(&pool, "p2", "2026-10-02T10:00:00+00:00").await;

        // Two units under one in-window ticket, and one each under the tickets that must not count.
        unit(&pool, "p1", ORIGIN_VERIFY, inside).await;
        unit(&pool, "p1", ORIGIN_VERIFY, inside).await;
        unit(&pool, "p1", ORIGIN_VERIFY, old).await;
        unit(&pool, "p2", ORIGIN_VERIFY, other).await;
        // A gate row whose origin_id happens to equal the ticket's id is not a `verify` unit.
        unit(&pool, "p1", ORIGIN_RUN, inside).await;

        let d = deviation(&pool, "p1", since).await.unwrap();
        assert_eq!(d.verify_units, 2);
        assert_eq!(d.observed, 0);
        assert_eq!(d.ratio, Some(0.0));
    }

    #[tokio::test]
    async fn the_ratio_is_observed_over_verify_units_and_absent_without_units() {
        let pool = crate::testdb::fresh_pool().await;
        let hit = a_hit();
        let since = "2000-01-01T00:00:00+00:00";

        // Observations but no `verify` unit: a ratio would divide by zero, so there is none.
        for _ in 0..3 {
            observe(&pool, "p1", &hit).await;
        }
        let none = deviation(&pool, "p1", since).await.unwrap();
        assert_eq!((none.observed, none.verify_units), (3, 0));
        assert_eq!(none.ratio, None);

        let ticket = request(&pool, "p1", "2026-10-02T10:00:00+00:00").await;
        unit(&pool, "p1", ORIGIN_VERIFY, ticket).await;
        unit(&pool, "p1", ORIGIN_VERIFY, ticket).await;
        let some = deviation(&pool, "p1", since).await.unwrap();
        assert_eq!((some.observed, some.verify_units), (3, 2));
        assert_eq!(some.ratio, Some(1.5));
    }
}
