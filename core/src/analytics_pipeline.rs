//! Pipeline and autopilot health for the analytics page (spec 2026-10-17-analytics-pipeline).
//!
//! Read side: one report per scope and window, built block by block; a block that fails becomes
//! `{"error": ...}` and the others still come (spec D5). Write side lives in `sampler` (slot
//! samples) and in `verify::postgate::record_transition` (colour changes).

pub mod gate;
pub mod landing;
pub mod sampler;
pub mod stats;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::analytics::{Scope, Span, Window};

/// A block, or why it could not be computed (spec D5): nine good blocks and one error beat an
/// empty screen.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Block<T> {
    Ready(T),
    Failed { error: &'static str },
}

/// The one error code a block answers today. The detail goes to the log, never to the client.
pub const BLOCK_FAILED: &str = "query_failed";

pub(crate) fn block<T>(name: &'static str, result: sqlx::Result<T>) -> Block<T> {
    match result {
        Ok(value) => Block::Ready(value),
        Err(error) => {
            tracing::warn!(%error, block = name, "analytics pipeline: block failed");
            Block::Failed {
                error: BLOCK_FAILED,
            }
        }
    }
}

/// Since when the two new records hold data (spec §7.1, §8 "sem histórico").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct History {
    /// When recording began: the transitions migration's own `installed_on`, so "no red since"
    /// reads as zero red seconds and not as "no history" (open decision 2). Falls back to the
    /// first transition.
    pub postgate_since: Option<String>,
    /// The oldest sample still on record, daily or raw.
    pub slots_since: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PipelineReport {
    pub scope: &'static str,
    pub project_id: Option<String>,
    pub days: i64,
    pub timezone: &'static str,
    pub now: String,
    pub window: Span,
    pub previous: Span,
    pub history: History,
    pub landing: Block<landing::Landing>,
    pub gate: Block<gate::Gate>,
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

async fn postgate_since(pool: &SqlitePool) -> sqlx::Result<Option<String>> {
    // sqlx names a migration after its file, underscores read as spaces. A database without
    // sqlx's table (a hand-built test schema) falls through to the first transition.
    let installed: Option<String> = sqlx::query_scalar(
        "SELECT installed_on FROM _sqlx_migrations WHERE description = 'postgate transitions'",
    )
    .fetch_optional(pool)
    .await
    .unwrap_or(None);
    let first: Option<String> = match installed {
        Some(at) => Some(at),
        None => {
            sqlx::query_scalar("SELECT MIN(at) FROM postgate_transitions")
                .fetch_one(pool)
                .await?
        }
    };
    Ok(first.as_deref().and_then(stats::parse_at).map(rfc3339))
}

async fn slots_since(pool: &SqlitePool) -> sqlx::Result<Option<String>> {
    let daily: Option<String> = sqlx::query_scalar("SELECT MIN(day) FROM slot_samples_daily")
        .fetch_one(pool)
        .await?;
    let raw: Option<String> = sqlx::query_scalar("SELECT MIN(sampled_at) FROM slot_samples")
        .fetch_one(pool)
        .await?;
    let daily = daily
        .as_deref()
        .and_then(stats::day_of)
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .map(|midnight| midnight.and_utc());
    let raw = raw.as_deref().and_then(stats::parse_at);
    let earliest = match (daily, raw) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    Ok(earliest.map(rfc3339))
}

/// A history date that cannot be read is absent and logged, never a reason to fail the report.
async fn history(pool: &SqlitePool) -> History {
    fn or_none(what: &str, result: sqlx::Result<Option<String>>) -> Option<String> {
        result.unwrap_or_else(|error| {
            tracing::warn!(%error, "analytics pipeline: could not read when {what} began");
            None
        })
    }
    History {
        postgate_since: or_none("the post-merge gate history", postgate_since(pool).await),
        slots_since: or_none("the slot history", slots_since(pool).await),
    }
}

/// Everything the Pipeline section shows for one scope and window. Infallible: each block
/// carries its own error. `now` is a parameter so a test states its clock.
pub async fn build(
    pool: &SqlitePool,
    scope: Scope<'_>,
    window: Window,
    now: DateTime<Utc>,
) -> PipelineReport {
    let project = scope.project();
    PipelineReport {
        scope: if project.is_some() {
            "project"
        } else {
            "global"
        },
        project_id: project.map(str::to_owned),
        days: window.days,
        timezone: "UTC",
        now: rfc3339(now),
        window: window.span(),
        previous: window.previous().span(),
        history: history(pool).await,
        landing: block("landing", landing::read(pool, project, window, now).await),
        gate: block("gate", gate::read(pool, project, window).await),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::analytics::tests::now;

    pub(crate) async fn report(
        pool: &sqlx::SqlitePool,
        scope: Scope<'_>,
        days: i64,
    ) -> serde_json::Value {
        let window = Window::new(days, now()).unwrap();
        let built = build(pool, scope, window, now()).await;
        serde_json::to_value(built).unwrap()
    }

    /// The envelope is F1's (spec §7.1): scope, days, UTC, the two inclusive spans, and history.
    #[tokio::test]
    async fn the_envelope_matches_the_first_phase_and_dates_the_history() {
        let pool = crate::testdb::fresh_pool().await;
        let global = report(&pool, Scope::Global, 7).await;
        assert_eq!(global["scope"], "global");
        assert!(global["project_id"].is_null());
        assert_eq!(global["days"], 7);
        assert_eq!(global["timezone"], "UTC");
        assert_eq!(global["now"], "2026-10-17T14:03:11Z");
        assert_eq!(global["window"]["from"], "2026-10-11");
        assert_eq!(global["window"]["to"], "2026-10-17");
        assert_eq!(global["previous"]["to"], "2026-10-10");
        assert_eq!(global["landing"]["total"], 0);
        assert!(global["history"]["slots_since"].is_null(), "no sample yet");

        sqlx::query(
            "INSERT INTO slot_samples (sampled_at, project_id, in_use, slot_limit)
             VALUES ('2026-10-16T10:00:00Z', NULL, 0, 4)",
        )
        .execute(&pool)
        .await
        .unwrap();
        // A test schema without sqlx's table falls back to the first transition; one row makes
        // the assertion hold on either branch (production always has the table).
        sqlx::query(
            "INSERT INTO postgate_transitions (project_id, at, to_state)
             VALUES ('p', '2026-10-16T09:00:00Z', 'red')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let project = report(&pool, Scope::Project("p"), 30).await;
        assert_eq!(project["scope"], "project");
        assert_eq!(project["project_id"], "p");
        assert_eq!(project["history"]["slots_since"], "2026-10-16T10:00:00Z");
        // Recording started when the transitions migration ran, whether or not any red happened.
        assert!(
            project["history"]["postgate_since"].is_string(),
            "{project}"
        );
    }

    /// `Block<T>` serialises as the block itself or as `{"error": code}` (spec §7.1, D5).
    #[test]
    fn a_block_is_its_value_or_an_error_object() {
        let ready: Block<i64> = Block::Ready(3);
        let failed: Block<i64> = Block::Failed {
            error: BLOCK_FAILED,
        };
        assert_eq!(serde_json::to_value(ready).unwrap(), serde_json::json!(3));
        assert_eq!(
            serde_json::to_value(failed).unwrap(),
            serde_json::json!({ "error": "query_failed" })
        );
    }

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
