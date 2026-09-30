//! Periodic measurements distilled from durable gate and refusal histories.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, SqliteConnection, SqlitePool};

pub const CONSOLIDATION_INTERVAL: Duration = Duration::from_secs(6 * 3600);
const MIN_OBSERVATIONS: i64 = 2;
const EPISODIC_VALIDITY_RUNS: i64 = 50;
const CLEARED_WINDOW_RUNS: usize = 5;
const EVIDENCE_LIMIT: usize = 20;

/// The first retention tick consolidates immediately; later ticks wait for the pass interval.
pub fn due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| now.duration_since(last) >= CONSOLIDATION_INTERVAL)
}

#[derive(Clone, Debug)]
struct Measurement {
    generator: &'static str,
    fingerprint: String,
    observations: i64,
    title: String,
    body: String,
    evidence: String,
}

#[derive(Clone, Debug, FromRow)]
struct GateRun {
    id: i64,
    gate_status: String,
    gate_output: Option<String>,
}

#[derive(Clone, Debug)]
struct WindowRun {
    id: i64,
    fingerprint: Option<String>,
}

#[derive(Debug, FromRow)]
struct RefusalGroup {
    tool_name: String,
    observations: i64,
    first_at: String,
    last_at: String,
}

#[derive(Debug, FromRow)]
struct RefusedGate {
    id: i64,
    fingerprint: String,
    title: String,
}

#[derive(Serialize)]
struct Evidence {
    t: &'static str,
    id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Outcome {
    Created,
    Remeasured,
    Pending,
    Refused,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PassReport {
    pub created: usize,
    pub remeasured: usize,
    pub pending: usize,
    pub refused: usize,
    pub successors: usize,
    pub skipped_busy: usize,
}

impl PassReport {
    pub fn is_quiet(&self) -> bool {
        *self == Self::default()
    }
}

pub async fn run_pass(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<PassReport> {
    let projects: Vec<String> = sqlx::query_scalar(
        "SELECT project_id FROM (
             SELECT DISTINCT project_id FROM runs
              WHERE project_id IS NOT NULL AND gate_status IS NOT NULL
             UNION
             SELECT DISTINCT project_id FROM proposals
              WHERE kind = 'refused-action' AND project_id IS NOT NULL
         ) ORDER BY project_id",
    )
    .fetch_all(pool)
    .await?;
    let mut report = PassReport::default();
    for project in projects {
        if project_is_busy(pool, &project).await? {
            report.skipped_busy += 1;
            continue;
        }
        let window = gate_window(pool, &project).await?;
        for measurement in gate_measurements(&window) {
            record(
                &mut report,
                write_measurement(pool, &project, &measurement, now).await?,
            );
        }
        for measurement in refusal_measurements(pool, &project).await? {
            record(
                &mut report,
                write_measurement(pool, &project, &measurement, now).await?,
            );
        }
        report.successors += write_reversals(pool, &project, &window, now).await?;
    }
    Ok(report)
}

async fn write_measurement(
    pool: &SqlitePool,
    project: &str,
    measurement: &Measurement,
    now: DateTime<Utc>,
) -> sqlx::Result<Outcome> {
    let mut transaction = pool.begin().await?;
    if let Some(id) =
        active_measurement(&mut transaction, project, &measurement.fingerprint).await?
    {
        let now = now.to_rfc3339();
        sqlx::query(
            "UPDATE knowledge
                SET observations = ?, title = ?, body = ?, evidence = ?, last_confirmed_at = ?
              WHERE id = ? AND source = 'consolidator' AND layer = 'episodic'
                AND status = 'active' AND supersedes IS NULL",
        )
        .bind(measurement.observations)
        .bind(&measurement.title)
        .bind(&measurement.body)
        .bind(&measurement.evidence)
        .bind(now)
        .bind(id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        return Ok(Outcome::Remeasured);
    }
    if let Some(outcome) =
        blocked_outcome(&mut transaction, project, &measurement.fingerprint).await?
    {
        transaction.commit().await?;
        return Ok(outcome);
    }
    insert_measurement(&mut transaction, project, measurement, now, None).await?;
    transaction.commit().await?;
    Ok(Outcome::Created)
}

fn record(report: &mut PassReport, outcome: Outcome) {
    match outcome {
        Outcome::Created => report.created += 1,
        Outcome::Remeasured => report.remeasured += 1,
        Outcome::Pending => report.pending += 1,
        Outcome::Refused => report.refused += 1,
    }
}

async fn project_is_busy(pool: &SqlitePool, project: &str) -> sqlx::Result<bool> {
    let placeholders = std::iter::repeat_n("?", crate::job::LIVE_STATUSES.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM jobs WHERE project_id = ? AND status IN ({placeholders}))
             OR EXISTS(SELECT 1 FROM runs WHERE project_id = ? AND status = 'running')"
    );
    // `AssertSqlSafe`, audited: only one `?` per member of the constant status list is
    // interpolated. Project and status values are bound below.
    let mut query = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql)).bind(project);
    for status in crate::job::LIVE_STATUSES {
        query = query.bind(status);
    }
    Ok(query.bind(project).fetch_one(pool).await? != 0)
}

async fn gate_window(pool: &SqlitePool, project: &str) -> sqlx::Result<Vec<GateRun>> {
    sqlx::query_as(
        "SELECT id, gate_status, gate_output FROM runs
          WHERE project_id = ? AND gate_status IN ('passed', 'failed', 'errored')
          ORDER BY id DESC LIMIT 200",
    )
    .bind(project)
    .fetch_all(pool)
    .await
}

fn gate_measurements(window: &[GateRun]) -> Vec<Measurement> {
    let mut groups: BTreeMap<String, (String, Vec<i64>)> = BTreeMap::new();
    for run in window {
        if !matches!(run.gate_status.as_str(), "failed" | "errored") {
            continue;
        }
        let Some(signature) = run
            .gate_output
            .as_deref()
            .and_then(crate::knowledge::failure_signature)
        else {
            continue;
        };
        let key = format!("gate:{}", signature.fingerprint);
        let group = groups
            .entry(key)
            .or_insert((signature.headline, Vec::new()));
        group.1.push(run.id);
    }
    groups
        .into_iter()
        .filter(|(_, (_, ids))| ids.len() as i64 >= MIN_OBSERVATIONS)
        .map(|(fingerprint, (headline, ids))| {
            gate_measurement(window.len(), fingerprint, headline, ids)
        })
        .collect()
}

fn gate_measurement(
    total: usize,
    fingerprint: String,
    headline: String,
    ids: Vec<i64>,
) -> Measurement {
    let observations = ids.len() as i64;
    let evidence = ids
        .into_iter()
        .take(EVIDENCE_LIMIT)
        .map(|id| Evidence { t: "run", id })
        .collect::<Vec<_>>();
    Measurement {
        generator: "gate",
        fingerprint,
        observations,
        title: format!("Gate keeps failing: {}", clipped(&headline, 80)),
        body: format!(
            "The gate failed with this signature in {observations} of the last {total} gated runs in this project: {headline}"
        ),
        evidence: serde_json::to_string(&evidence).expect("evidence is serializable"),
    }
}

async fn refusal_measurements(pool: &SqlitePool, project: &str) -> sqlx::Result<Vec<Measurement>> {
    let groups: Vec<RefusalGroup> = sqlx::query_as(
        "SELECT tool_name, COUNT(*) AS observations, MIN(created_at) AS first_at,
                MAX(created_at) AS last_at
           FROM proposals
          WHERE kind = 'refused-action' AND project_id = ?
            AND tool_name IS NOT NULL AND tool_name <> ''
          GROUP BY tool_name HAVING COUNT(*) >= 2 ORDER BY tool_name",
    )
    .bind(project)
    .fetch_all(pool)
    .await?;
    let mut measurements = Vec::with_capacity(groups.len());
    for group in groups {
        measurements.push(refusal_measurement(pool, project, group).await?);
    }
    Ok(measurements)
}

async fn refusal_measurement(
    pool: &SqlitePool,
    project: &str,
    group: RefusalGroup,
) -> sqlx::Result<Measurement> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM proposals
          WHERE kind = 'refused-action' AND project_id = ? AND tool_name = ?
          ORDER BY id DESC LIMIT ?",
    )
    .bind(project)
    .bind(&group.tool_name)
    .bind(EVIDENCE_LIMIT as i64)
    .fetch_all(pool)
    .await?;
    let evidence = ids
        .into_iter()
        .map(|id| Evidence { t: "proposal", id })
        .collect::<Vec<_>>();
    Ok(Measurement {
        generator: "refused-action",
        fingerprint: format!("refused-action:{}", group.tool_name.trim().to_lowercase()),
        observations: group.observations,
        title: format!("Refused here: {}", group.tool_name),
        body: format!(
            "Asking for {} in this project was refused {} times between {} and {}.",
            group.tool_name, group.observations, group.first_at, group.last_at
        ),
        evidence: serde_json::to_string(&evidence).expect("evidence is serializable"),
    })
}

async fn active_measurement(
    connection: &mut SqliteConnection,
    project: &str,
    fingerprint: &str,
) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT id FROM knowledge
          WHERE scope_kind = 'project' AND scope_id = ? AND fingerprint = ?
            AND source = 'consolidator' AND layer = 'episodic' AND status = 'active'
            AND supersedes IS NULL ORDER BY id DESC LIMIT 1",
    )
    .bind(project)
    .bind(fingerprint)
    .fetch_optional(connection)
    .await
}

async fn blocked_outcome(
    connection: &mut SqliteConnection,
    project: &str,
    fingerprint: &str,
) -> sqlx::Result<Option<Outcome>> {
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM knowledge
          WHERE scope_kind = 'project' AND scope_id = ? AND fingerprint = ?
            AND status IN ('proposed', 'rejected', 'reverted')
          ORDER BY CASE status WHEN 'proposed' THEN 0 ELSE 1 END, id DESC LIMIT 1",
    )
    .bind(project)
    .bind(fingerprint)
    .fetch_optional(connection)
    .await?;
    Ok(match status.as_deref() {
        Some("proposed") => Some(Outcome::Pending),
        Some("rejected" | "reverted") => Some(Outcome::Refused),
        _ => None,
    })
}

async fn insert_measurement(
    connection: &mut SqliteConnection,
    project: &str,
    measurement: &Measurement,
    now: DateTime<Utc>,
    supersedes: Option<i64>,
) -> sqlx::Result<i64> {
    let now = now.to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO knowledge (layer, scope_kind, scope_id, source, generator, evidence,
             observations, fingerprint, expires_after_runs, last_confirmed_at, kind, title, body,
             status, proposal_id, supersedes, origin_run_id, created_at, activated_at, ended_at)
         VALUES ('episodic', 'project', ?, 'consolidator', ?, ?, ?, ?, ?, ?, 'memory', ?, ?,
                 'active', NULL, ?, NULL, ?, ?, NULL)",
    )
    .bind(project)
    .bind(measurement.generator)
    .bind(&measurement.evidence)
    .bind(measurement.observations)
    .bind(&measurement.fingerprint)
    .bind(EPISODIC_VALIDITY_RUNS)
    .bind(&now)
    .bind(&measurement.title)
    .bind(&measurement.body)
    .bind(supersedes)
    .bind(&now)
    .bind(&now)
    .execute(&mut *connection)
    .await?;
    let id = result.last_insert_rowid();
    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'active', 'measured by the consolidator', ?)",
    )
    .bind(id)
    .bind(now)
    .execute(connection)
    .await?;
    Ok(id)
}

fn as_window(window: &[GateRun]) -> Vec<WindowRun> {
    window
        .iter()
        .map(|run| WindowRun {
            id: run.id,
            fingerprint: run
                .gate_output
                .as_deref()
                .filter(|_| matches!(run.gate_status.as_str(), "failed" | "errored"))
                .and_then(crate::knowledge::failure_signature)
                .map(|signature| format!("gate:{}", signature.fingerprint)),
        })
        .collect()
}

fn reversed(window: &[WindowRun], fingerprint: &str) -> bool {
    window.len() >= CLEARED_WINDOW_RUNS
        && window
            .iter()
            .take(CLEARED_WINDOW_RUNS)
            .all(|run| run.fingerprint.as_deref() != Some(fingerprint))
}

async fn write_reversals(
    pool: &SqlitePool,
    project: &str,
    window: &[GateRun],
    now: DateTime<Utc>,
) -> sqlx::Result<usize> {
    let refused: Vec<RefusedGate> = sqlx::query_as(
        "SELECT id, fingerprint, title FROM knowledge
          WHERE scope_kind = 'project' AND scope_id = ? AND fingerprint IS NOT NULL
            AND status IN ('rejected', 'reverted') AND source = 'consolidator'
            AND layer = 'episodic' AND generator = 'gate' AND supersedes IS NULL
          ORDER BY id DESC",
    )
    .bind(project)
    .fetch_all(pool)
    .await?;
    let window = as_window(window);
    let mut written = 0;
    for row in refused {
        if reversed(&window, &row.fingerprint)
            && write_reversal(pool, project, &row, &window, now).await?
        {
            written += 1;
        }
    }
    Ok(written)
}

async fn write_reversal(
    pool: &SqlitePool,
    project: &str,
    refused: &RefusedGate,
    window: &[WindowRun],
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let mut transaction = pool.begin().await?;
    let exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM knowledge
          WHERE scope_kind = 'project' AND scope_id = ? AND fingerprint = ?
            AND supersedes IS NOT NULL AND status IN ('proposed', 'active'))",
    )
    .bind(project)
    .bind(&refused.fingerprint)
    .fetch_one(&mut *transaction)
    .await?;
    if exists != 0 {
        transaction.commit().await?;
        return Ok(false);
    }
    let measurement = reversal_measurement(refused, window);
    insert_measurement(
        &mut transaction,
        project,
        &measurement,
        now,
        Some(refused.id),
    )
    .await?;
    transaction.commit().await?;
    Ok(true)
}

fn reversal_measurement(refused: &RefusedGate, window: &[WindowRun]) -> Measurement {
    let headline = refused
        .title
        .strip_prefix("Gate keeps failing: ")
        .unwrap_or(&refused.title);
    let evidence = window
        .iter()
        .take(CLEARED_WINDOW_RUNS)
        .map(|run| Evidence {
            t: "run",
            id: run.id,
        })
        .collect::<Vec<_>>();
    Measurement {
        generator: "gate",
        fingerprint: refused.fingerprint.clone(),
        observations: 0,
        title: format!("Gate failure no longer recurs: {}", clipped(headline, 80)),
        body: format!(
            "The gate has not failed with this signature in the last {CLEARED_WINDOW_RUNS} gated runs in this project."
        ),
        evidence: serde_json::to_string(&evidence).expect("evidence is serializable"),
    }
}

fn clipped(value: &str, width: usize) -> String {
    value.chars().take(width).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use chrono::{DateTime, TimeZone, Utc};
    use serde_json::Value;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::{Row, SqlitePool};

    use super::{CONSOLIDATION_INTERVAL, due, run_pass};
    use crate::knowledge::{Known, failure_signature};

    const PROJECT: &str = "nucleos";
    const FAILURE: &str = "error: build failed because the linker refused output.exe";

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 30, hour, 0, 0)
            .single()
            .unwrap()
    }

    async fn seed_run(
        pool: &SqlitePool,
        project: &str,
        status: &str,
        gate_status: Option<&str>,
        gate_output: Option<&str>,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, gate_status, gate_output, created_at)
             VALUES (?, 'test prompt', ?, ?, ?, '2026-09-30T00:00:00+00:00')",
        )
        .bind(project)
        .bind(status)
        .bind(gate_status)
        .bind(gate_output)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn seed_refusal(pool: &SqlitePool, project: &str, tool: &str) -> i64 {
        sqlx::query(
            "INSERT INTO proposals
               (kind, status, project_id, tool_name, reasoning, created_at)
             VALUES ('refused-action', 'pending', ?, ?, 'policy refused it',
                     '2026-09-30T00:00:00+00:00')",
        )
        .bind(project)
        .bind(tool)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_knowledge(
        pool: &SqlitePool,
        layer: &str,
        scope_kind: &str,
        scope_id: &str,
        source: &str,
        generator: Option<&str>,
        fingerprint: &str,
        status: &str,
        observations: Option<i64>,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, generator, observations, fingerprint,
                kind, title, body, status, created_at, activated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'memory', 'seeded title', 'seeded body', ?,
                     '2026-09-29T00:00:00+00:00', '2026-09-29T00:00:00+00:00')",
        )
        .bind(layer)
        .bind(scope_kind)
        .bind(scope_id)
        .bind(source)
        .bind(generator)
        .bind(observations)
        .bind(fingerprint)
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn known(pool: &SqlitePool, id: i64) -> Value {
        let row = sqlx::query_as::<_, Known>("SELECT * FROM knowledge WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
        serde_json::to_value(row).unwrap()
    }

    fn gate_fingerprint(output: &str) -> String {
        format!("gate:{}", failure_signature(output).unwrap().fingerprint)
    }

    /// A measured exception may bypass approval only while every production write is fenced to it.
    #[tokio::test]
    async fn the_consolidator_has_no_way_to_create_anything_but_a_measurement() {
        let pool = test_pool().await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        seed_refusal(&pool, PROJECT, "Bash").await;
        seed_refusal(&pool, PROJECT, "Bash").await;

        let report = run_pass(&pool, at(1)).await.unwrap();
        assert_eq!(report.created, 2);
        let rows = sqlx::query(
            "SELECT layer, source, status, observations, generator
             FROM knowledge WHERE source = 'consolidator' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(row.get::<String, _>("layer"), "episodic");
            assert_eq!(row.get::<String, _>("source"), "consolidator");
            assert_eq!(row.get::<String, _>("status"), "active");
            assert!(row.get::<Option<i64>, _>("observations").is_some());
            assert!(matches!(
                row.get::<String, _>("generator").as_str(),
                "gate" | "refused-action"
            ));
        }

        let built_in = Path::new(env!("CARGO_MANIFEST_DIR"));
        let running_in = std::env::current_dir().expect("the working directory must be readable");
        assert_eq!(
            built_in,
            running_in.as_path(),
            "this test binary was compiled in {} and is running in {} -- a shared target directory handed this checkout a binary built somewhere else, so this scan would read the other checkout's sources. Touch this file to force a rebuild.",
            built_in.display(),
            running_in.display(),
        );
        let source = fs::read_to_string(running_in.join("src/consolidate.rs"))
            .expect("consolidate source must be readable")
            .replace("\r\n", "\n");
        let production = source
            .split_once("\n#[cfg(test)]")
            .map_or(source.as_str(), |(production, _)| production);
        assert_eq!(production.matches("INSERT INTO knowledge ").count(), 1);
        assert!(!production.contains("DELETE FROM knowledge"));
        let updates = production
            .split(';')
            .filter(|statement| statement.contains("UPDATE knowledge"))
            .collect::<Vec<_>>();
        assert!(!updates.is_empty());
        for update in updates {
            assert!(update.contains("source = 'consolidator'"), "{update}");
            assert!(update.contains("layer = 'episodic'"), "{update}");
        }
    }

    /// Consolidation may add its own measured row but may never mutate a person's assertion.
    #[tokio::test]
    async fn the_consolidator_cannot_touch_what_a_person_wrote() {
        let pool = test_pool().await;
        let fingerprint = gate_fingerprint(FAILURE);
        let owner = seed_knowledge(
            &pool,
            "semantic",
            "project",
            PROJECT,
            "owner",
            None,
            &fingerprint,
            "active",
            None,
        )
        .await;
        sqlx::query(
            "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
             VALUES (?, 'proposed', 'active', 'owner approved it',
                     '2026-09-29T00:00:00+00:00')",
        )
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
        let before = known(&pool, owner).await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;

        run_pass(&pool, at(1)).await.unwrap();
        run_pass(&pool, at(2)).await.unwrap();

        assert_eq!(known(&pool, owner).await, before);
        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM knowledge_events WHERE knowledge_id = ?")
                .bind(owner)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(events, 1);
    }

    /// Working knowledge belongs to its job and is outside the consolidator's write authority.
    #[tokio::test]
    async fn the_consolidator_never_writes_to_a_working_row() {
        let pool = test_pool().await;
        let fingerprint = gate_fingerprint(FAILURE);
        let job_row = seed_knowledge(
            &pool,
            "working",
            "job",
            "41",
            "run",
            None,
            &fingerprint,
            "live",
            None,
        )
        .await;
        let project_row = seed_knowledge(
            &pool,
            "working",
            "project",
            PROJECT,
            "run",
            None,
            &fingerprint,
            "live",
            None,
        )
        .await;
        let before_job = known(&pool, job_row).await;
        let before_project = known(&pool, project_row).await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;

        run_pass(&pool, at(1)).await.unwrap();
        run_pass(&pool, at(2)).await.unwrap();

        assert_eq!(known(&pool, job_row).await, before_job);
        assert_eq!(known(&pool, project_row).await, before_project);
    }

    /// Idempotence means a later count re-measures the original row instead of duplicating it.
    #[tokio::test]
    async fn two_passes_over_the_same_measurement_leave_one_row_with_the_newer_count() {
        let pool = test_pool().await;
        let fingerprint = gate_fingerprint(FAILURE);
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        run_pass(&pool, at(1)).await.unwrap();
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;

        let second = run_pass(&pool, at(2)).await.unwrap();

        assert_eq!(second.remeasured, 1);
        let rows = sqlx::query(
            "SELECT observations, last_confirmed_at FROM knowledge
             WHERE scope_kind = 'project' AND scope_id = ? AND fingerprint = ?",
        )
        .bind(PROJECT)
        .bind(fingerprint)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64, _>("observations"), 3);
        assert_eq!(
            rows[0].get::<String, _>("last_confirmed_at"),
            at(2).to_rfc3339()
        );
    }

    /// A proposal already waiting for a person is the one door for that fingerprint.
    #[tokio::test]
    async fn a_pending_proposal_is_never_proposed_again() {
        let pool = test_pool().await;
        let fingerprint = gate_fingerprint(FAILURE);
        seed_knowledge(
            &pool,
            "semantic",
            "project",
            PROJECT,
            "run",
            None,
            &fingerprint,
            "proposed",
            None,
        )
        .await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;

        let report = run_pass(&pool, at(1)).await.unwrap();

        assert_eq!(report.pending, 1);
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge WHERE scope_kind = 'project' AND scope_id = ? AND fingerprint = ?",
        )
        .bind(PROJECT)
        .bind(fingerprint)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }

    /// Rejected and reverted are both explicit refusals, so neither may silently return.
    #[tokio::test]
    async fn neither_a_rejection_nor_a_withdrawal_comes_back() {
        let pool = test_pool().await;
        let first = "error: alpha failed its assertion";
        let second = "error: beta could not link its binary";
        for (output, status) in [(first, "rejected"), (second, "reverted")] {
            seed_knowledge(
                &pool,
                "episodic",
                "project",
                PROJECT,
                "consolidator",
                Some("gate"),
                &gate_fingerprint(output),
                status,
                Some(3),
            )
            .await;
            for _ in 0..3 {
                seed_run(&pool, PROJECT, "completed", Some("failed"), Some(output)).await;
            }
        }

        let report = run_pass(&pool, at(1)).await.unwrap();

        assert_eq!(report.refused, 2);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM knowledge")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2);
    }

    /// A cleared gate signature may have one measured successor, but never be resurrected in place.
    #[tokio::test]
    async fn a_measurement_that_reverses_writes_one_successor_and_never_two() {
        let pool = test_pool().await;
        let fingerprint = gate_fingerprint(FAILURE);
        let refused = seed_knowledge(
            &pool,
            "episodic",
            "project",
            PROJECT,
            "consolidator",
            Some("gate"),
            &fingerprint,
            "rejected",
            Some(2),
        )
        .await;
        for _ in 0..5 {
            seed_run(&pool, PROJECT, "completed", Some("passed"), None).await;
        }

        let first = run_pass(&pool, at(1)).await.unwrap();
        assert_eq!(first.successors, 1);
        let successor = sqlx::query(
            "SELECT id, fingerprint, observations, body FROM knowledge WHERE supersedes = ?",
        )
        .bind(refused)
        .fetch_one(&pool)
        .await
        .unwrap();
        let successor_id = successor.get::<i64, _>("id");
        assert_eq!(successor.get::<String, _>("fingerprint"), fingerprint);
        assert_eq!(successor.get::<i64, _>("observations"), 0);
        let unchanged_body = successor.get::<String, _>("body");

        run_pass(&pool, at(2)).await.unwrap();
        run_pass(&pool, at(3)).await.unwrap();
        let rows = sqlx::query(
            "SELECT id, observations, body FROM knowledge WHERE supersedes = ? ORDER BY id",
        )
        .bind(refused)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64, _>("id"), successor_id);
        assert_eq!(rows[0].get::<i64, _>("observations"), 0);
        assert_eq!(rows[0].get::<String, _>("body"), unchanged_body);

        let short_project = "only-four";
        let short_refused = seed_knowledge(
            &pool,
            "episodic",
            "project",
            short_project,
            "consolidator",
            Some("gate"),
            &fingerprint,
            "reverted",
            Some(2),
        )
        .await;
        for _ in 0..4 {
            seed_run(&pool, short_project, "completed", Some("passed"), None).await;
        }
        run_pass(&pool, at(4)).await.unwrap();
        let short_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM knowledge WHERE supersedes = ?")
                .bind(short_refused)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(short_count, 0);
    }

    /// Gate identity survives both historical worktree layouts while M counts every gated run.
    #[tokio::test]
    async fn the_gate_generator_counts_n_of_m_across_two_worktree_layouts() {
        let pool = test_pool().await;
        let legacy = r#"error: failed to remove file `C:\Projects\nucleos-worktrees\nucleos\run-900372\core\target\debug\nucleos-core.exe`: Acesso negado. (os error 5)"#;
        let nested = r#"error: failed to remove file `C:\Projects\nucleos\.nucleos\worktrees\run-900446\core\target\debug\nucleos-core.exe`: Acesso negado. (os error 5)"#;
        for output in [legacy, nested, legacy, nested] {
            seed_run(&pool, PROJECT, "completed", Some("failed"), Some(output)).await;
        }
        for _ in 0..3 {
            seed_run(&pool, PROJECT, "completed", Some("passed"), None).await;
        }
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some("")).await;

        run_pass(&pool, at(1)).await.unwrap();

        let row = sqlx::query(
            "SELECT observations, body, evidence FROM knowledge WHERE generator = 'gate'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<i64, _>("observations"), 4);
        assert!(row.get::<String, _>("body").contains("4 of the last 8"));
        let evidence: Value = serde_json::from_str(&row.get::<String, _>("evidence")).unwrap();
        let evidence = evidence.as_array().unwrap();
        assert_eq!(evidence.len(), 4);
        assert!(evidence.iter().all(|item| item["t"] == "run"));
    }

    /// Repeated refusals teach one lesson per tool; a singleton and an unscoped refusal teach none.
    #[tokio::test]
    async fn the_refused_action_generator_writes_one_lesson_per_tool() {
        let pool = test_pool().await;
        for _ in 0..19 {
            seed_refusal(&pool, PROJECT, "Bash").await;
        }
        seed_refusal(&pool, PROJECT, "Edit").await;
        sqlx::query(
            "INSERT INTO proposals
               (kind, status, project_id, tool_name, reasoning, created_at)
             VALUES ('refused-action', 'pending', NULL, 'Bash', 'unscoped refusal',
                     '2026-09-30T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        run_pass(&pool, at(1)).await.unwrap();

        let rows = sqlx::query(
            "SELECT observations, fingerprint FROM knowledge WHERE generator = 'refused-action'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get::<i64, _>("observations"), 19);
        assert_eq!(
            rows[0].get::<String, _>("fingerprint"),
            "refused-action:bash"
        );
    }

    /// A project in flight is not stable history, so the whole project waits for a later pass.
    #[tokio::test]
    async fn a_project_with_a_running_job_is_left_alone() {
        let pool = test_pool().await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        let job = sqlx::query(
            "INSERT INTO jobs
               (project_id, project_root, status, max_items, created_at)
             VALUES (?, 'C:/Projects/nucleos', 'implementing', 4,
                     '2026-09-30T00:00:00+00:00')",
        )
        .bind(PROJECT)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let busy = run_pass(&pool, at(1)).await.unwrap();
        assert_eq!(busy.skipped_busy, 1);
        let empty: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM knowledge")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(empty, 0);

        sqlx::query("UPDATE jobs SET status = 'completed' WHERE id = ?")
            .bind(job)
            .execute(&pool)
            .await
            .unwrap();
        let settled = run_pass(&pool, at(2)).await.unwrap();
        assert_eq!(settled.created, 1);
    }

    /// The six-hour clock runs once at startup and never early after a completed pass.
    #[test]
    fn the_pass_is_due_on_start_and_then_every_interval() {
        let now = Instant::now();
        assert!(due(None, now));
        assert!(!due(Some(now), now));
        assert!(due(Some(now), now + CONSOLIDATION_INTERVAL));
        assert!(!due(
            Some(now),
            now + CONSOLIDATION_INTERVAL - Duration::from_secs(1)
        ));
    }
}
