//! Periodic measurements distilled from durable gate histories.
//!
//! The consolidator never creates a lesson from a count. It reinforces a row that already has a
//! cause, merges exact duplicates into the oldest one, expires what nobody reconfirms and promotes
//! measurements repeated across projects.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, SqliteConnection, SqlitePool};

pub const CONSOLIDATION_INTERVAL: Duration = Duration::from_secs(6 * 3600);
const MIN_OBSERVATIONS: i64 = 2;
const PROMOTION_PROJECTS: usize = 3;
const EVIDENCE_LIMIT: usize = 20;
const CLEANUP_FLAG: &str = "consolidator.cleanup_v1";

/// The first retention tick consolidates immediately; later ticks wait for the pass interval.
pub fn due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|last| now.duration_since(last) >= CONSOLIDATION_INTERVAL)
}

#[derive(Clone, Debug)]
struct Measurement {
    generator: Option<String>,
    fingerprint: String,
    observations: i64,
    kind: String,
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

#[derive(Debug, FromRow)]
struct DuplicateKey {
    scope_kind: String,
    scope_id: Option<String>,
    fingerprint: String,
}

#[derive(Debug, FromRow)]
struct DuplicateMember {
    id: i64,
    observations: i64,
}

#[derive(Debug, FromRow)]
struct PromotionMember {
    id: i64,
    scope_id: String,
    generator: Option<String>,
    observations: i64,
    title: String,
    body: String,
}

#[derive(Serialize)]
struct Evidence {
    t: &'static str,
    id: i64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PassReport {
    /// Rows with a cause reinforced in place.
    pub remeasured: usize,
    /// Always 0: no path writes a successor row any more.
    pub successors: usize,
    pub expired: usize,
    /// Always 0: a contradicted row is left as it is.
    pub superseded: usize,
    pub merged: usize,
    pub skipped_busy: usize,
}

impl PassReport {
    pub fn is_quiet(&self) -> bool {
        *self == Self::default()
    }
}

pub async fn run_pass(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<PassReport> {
    let mut report = PassReport {
        merged: merge_duplicates(pool, now).await?,
        ..PassReport::default()
    };
    let projects: Vec<String> = sqlx::query_scalar(
        "SELECT project_id FROM (
             SELECT DISTINCT project_id FROM runs
              WHERE project_id IS NOT NULL AND gate_status IS NOT NULL
             UNION
             SELECT DISTINCT scope_id AS project_id FROM knowledge
              WHERE scope_kind = 'project' AND scope_id IS NOT NULL
                AND (source = 'consolidator' OR source = 'distiller')
                AND layer = 'episodic' AND status = 'active'
         ) ORDER BY project_id",
    )
    .fetch_all(pool)
    .await?;
    for project in projects {
        if project_is_busy(pool, &project).await? {
            report.skipped_busy += 1;
            continue;
        }
        let window = gate_window(pool, &project).await?;
        report.remeasured += reinforce_gate_signatures(pool, &project, &window, now).await?;
        report.expired += expire_unconfirmed(pool, &project, now).await?;
    }
    promote_to_machine(pool, now).await?;
    Ok(report)
}

/// The consolidator as the retention loop (`execution::runs::run_retention_loop`, in
/// `nucleos-core`) runs it: `main.rs` hands one in, since that loop cannot name this module.
pub struct Consolidator;

#[async_trait::async_trait]
impl crate::execution::runs::Consolidation for Consolidator {
    async fn step_if_due(&self, pool: &SqlitePool, now: DateTime<Utc>, last: &mut Option<Instant>) {
        if !due(*last, Instant::now()) {
            return;
        }
        *last = Some(Instant::now());
        match archive_countless_rows(pool, now).await {
            Ok(0) => {}
            Ok(archived) => {
                tracing::info!(archived, "consolidate: archived count rows without a cause");
            }
            Err(error) => tracing::warn!(%error, "consolidate: cleanup failed"),
        }
        match run_pass(pool, now).await {
            Ok(report) if report.is_quiet() => {}
            Ok(report) => tracing::info!(
                remeasured = report.remeasured,
                merged = report.merged,
                expired = report.expired,
                skipped_busy = report.skipped_busy,
                "consolidate: pass finished"
            ),
            Err(error) => tracing::warn!(%error, "consolidate: pass failed"),
        }
    }
}

async fn project_is_busy(pool: &SqlitePool, project: &str) -> sqlx::Result<bool> {
    let placeholders = std::iter::repeat_n("?", crate::execution::job::LIVE_STATUSES.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM jobs WHERE project_id = ? AND status IN ({placeholders}))
             OR EXISTS(SELECT 1 FROM runs WHERE project_id = ? AND status = 'running')"
    );
    // `AssertSqlSafe`, audited: only one `?` per member of the constant status list is
    // interpolated. Project and status values are bound below.
    let mut query = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql)).bind(project);
    for status in crate::execution::job::LIVE_STATUSES {
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

/// Failed gate runs of the window grouped by raw failure signature, keeping the signatures that
/// repeat: fingerprint to run ids, newest first.
fn repeated_signatures(window: &[GateRun]) -> BTreeMap<String, Vec<i64>> {
    let mut groups: BTreeMap<String, Vec<i64>> = BTreeMap::new();
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
        groups
            .entry(signature.fingerprint)
            .or_default()
            .push(run.id);
    }
    groups.retain(|_, ids| ids.len() as i64 >= MIN_OBSERVATIONS);
    groups
}

/// A repeated gate signature raises a row somebody already explained (its `cause_key` is the raw
/// fingerprint) and never creates one: observations go up, the runs become evidence, the row is
/// reconfirmed.
async fn reinforce_gate_signatures(
    pool: &SqlitePool,
    project: &str,
    window: &[GateRun],
    now: DateTime<Utc>,
) -> sqlx::Result<usize> {
    let now = now.to_rfc3339();
    let mut reinforced = 0;
    for (fingerprint, run_ids) in repeated_signatures(window) {
        let mut transaction = pool.begin().await?;
        let rows: Vec<(i64, Option<i64>, Option<String>)> = sqlx::query_as(
            "SELECT id, observations, evidence FROM knowledge
              WHERE scope_kind = 'project' AND scope_id = ? AND cause_key = ?
                AND source IN ('consolidator', 'distiller')
                AND status IN ('active', 'proposed')
              ORDER BY id",
        )
        .bind(project)
        .bind(&fingerprint)
        .fetch_all(&mut *transaction)
        .await?;
        for (id, observations, evidence) in rows {
            let mut items: Vec<serde_json::Value> = evidence
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or_default();
            for run_id in &run_ids {
                let item = serde_json::json!({"t": "run", "id": run_id});
                if items.len() < EVIDENCE_LIMIT && !items.contains(&item) {
                    items.push(item);
                }
            }
            let observed = observations.unwrap_or(0).max(run_ids.len() as i64);
            let updated = sqlx::query(
                "UPDATE knowledge
                    SET observations = ?, evidence = ?, last_confirmed_at = ?
                  WHERE id = ? AND cause_key = ?
                    AND source IN ('consolidator', 'distiller')
                    AND status IN ('active', 'proposed')",
            )
            .bind(observed)
            .bind(serde_json::to_string(&items).expect("evidence is serializable"))
            .bind(&now)
            .bind(id)
            .bind(&fingerprint)
            .execute(&mut *transaction)
            .await?;
            reinforced += updated.rows_affected() as usize;
        }
        transaction.commit().await?;
    }
    Ok(reinforced)
}

async fn insert_measurement(
    connection: &mut SqliteConnection,
    scope: (&str, Option<&str>),
    measurement: &Measurement,
    now: DateTime<Utc>,
    expires_after_runs: Option<i64>,
    event_note: &str,
) -> sqlx::Result<i64> {
    let now = now.to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO knowledge (layer, scope_kind, scope_id, source, generator, evidence,
             observations, fingerprint, expires_after_runs, last_confirmed_at, kind, title, body,
             status, proposal_id, supersedes, origin_run_id, created_at, activated_at, ended_at)
         VALUES ('episodic', ?, ?, 'consolidator', ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 'active', NULL, NULL, NULL, ?, ?, NULL)",
    )
    .bind(scope.0)
    .bind(scope.1)
    .bind(&measurement.generator)
    .bind(&measurement.evidence)
    .bind(measurement.observations)
    .bind(&measurement.fingerprint)
    .bind(expires_after_runs)
    .bind(&now)
    .bind(&measurement.kind)
    .bind(&measurement.title)
    .bind(&measurement.body)
    .bind(&now)
    .bind(&now)
    .execute(&mut *connection)
    .await?;
    let id = result.last_insert_rowid();
    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'active', ?, ?)",
    )
    .bind(id)
    .bind(event_note)
    .bind(now)
    .execute(connection)
    .await?;
    Ok(id)
}


/// Exact duplicates collapse into the oldest row: it absorbs the observations and the others are
/// archived, so the surviving id never changes and no successor is inserted.
async fn merge_duplicates(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<usize> {
    let keys: Vec<DuplicateKey> = sqlx::query_as(
        "SELECT scope_kind, scope_id, fingerprint FROM knowledge
          WHERE source = 'consolidator' AND layer = 'episodic' AND status = 'active'
            AND fingerprint IS NOT NULL
          GROUP BY scope_kind, scope_id, fingerprint HAVING COUNT(*) > 1
          ORDER BY scope_kind, scope_id, fingerprint",
    )
    .fetch_all(pool)
    .await?;
    let mut merged = 0;
    for key in keys {
        if key.scope_kind == "project"
            && let Some(project) = key.scope_id.as_deref()
            && project_is_busy(pool, project).await?
        {
            continue;
        }
        let mut transaction = pool.begin().await?;
        let members: Vec<DuplicateMember> = sqlx::query_as(
            "SELECT id, COALESCE(observations, 0) AS observations
               FROM knowledge
              WHERE scope_kind = ? AND scope_id IS ? AND fingerprint = ?
                AND source = 'consolidator' AND layer = 'episodic' AND status = 'active'
              ORDER BY id",
        )
        .bind(&key.scope_kind)
        .bind(&key.scope_id)
        .bind(&key.fingerprint)
        .fetch_all(&mut *transaction)
        .await?;
        let Some((oldest, rest)) = members.split_first() else {
            transaction.commit().await?;
            continue;
        };
        if rest.is_empty() {
            transaction.commit().await?;
            continue;
        }
        let evidence = rest
            .iter()
            .map(|member| Evidence {
                t: "knowledge",
                id: member.id,
            })
            .collect::<Vec<_>>();
        let observations: i64 = members.iter().map(|member| member.observations).sum();
        let now_text = now.to_rfc3339();
        let updated = sqlx::query(
            "UPDATE knowledge SET observations = ?, evidence = ?, last_confirmed_at = ?
              WHERE id = ? AND source = 'consolidator' AND layer = 'episodic'
                AND status = 'active'",
        )
        .bind(observations)
        .bind(serde_json::to_string(&evidence).expect("evidence is serializable"))
        .bind(&now_text)
        .bind(oldest.id)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() == 0 {
            transaction.commit().await?;
            continue;
        }
        for member in rest {
            let updated = sqlx::query(
                "UPDATE knowledge SET status = 'archived', ended_at = ?
                  WHERE id = ? AND source = 'consolidator' AND layer = 'episodic'
                    AND status = 'active'",
            )
            .bind(&now_text)
            .bind(member.id)
            .execute(&mut *transaction)
            .await?;
            if updated.rows_affected() == 0 {
                continue;
            }
            sqlx::query(
                "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
                 VALUES (?, 'active', 'archived', ?, ?)",
            )
            .bind(member.id)
            .bind(format!("merged into {}", oldest.id))
            .bind(&now_text)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        merged += 1;
    }
    Ok(merged)
}

/// One-shot cleanup of the count rows the consolidator used to write (gate and refused-action
/// generators, episodic layer): they are archived, one event each, nothing is deleted. The
/// `schema_meta` flag makes a second call a no-op, so a row written later is never swept.
pub(super) async fn archive_countless_rows(
    pool: &SqlitePool,
    now: DateTime<Utc>,
) -> sqlx::Result<usize> {
    let mut transaction = pool.begin().await?;
    let done: Option<String> = sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = ?")
        .bind(CLEANUP_FLAG)
        .fetch_optional(&mut *transaction)
        .await?;
    if done.is_some() {
        transaction.commit().await?;
        return Ok(0);
    }
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, status FROM knowledge
          WHERE source = 'consolidator' AND generator IN ('gate', 'refused-action')
            AND layer = 'episodic' AND status IN ('active', 'proposed')
          ORDER BY id",
    )
    .fetch_all(&mut *transaction)
    .await?;
    let now_text = now.to_rfc3339();
    let mut archived = 0;
    for (id, from_status) in rows {
        let updated = sqlx::query(
            "UPDATE knowledge SET status = 'archived', ended_at = ?
              WHERE id = ? AND source = 'consolidator' AND layer = 'episodic'
                AND status IN ('active', 'proposed')",
        )
        .bind(&now_text)
        .bind(id)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() == 0 {
            continue;
        }
        sqlx::query(
            "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
             VALUES (?, ?, 'archived', 'cleanup: consolidator count without cause', ?)",
        )
        .bind(id)
        .bind(from_status)
        .bind(&now_text)
        .execute(&mut *transaction)
        .await?;
        archived += 1;
    }
    sqlx::query("INSERT OR REPLACE INTO schema_meta (key, value) VALUES (?, ?)")
        .bind(CLEANUP_FLAG)
        .bind(&now_text)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(archived)
}

async fn expire_unconfirmed(
    pool: &SqlitePool,
    project: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<usize> {
    let mut transaction = pool.begin().await?;
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM knowledge
          WHERE scope_kind = 'project' AND scope_id = ?
            AND (source = 'consolidator' OR source = 'distiller')
            AND layer = 'episodic' AND status = 'active' AND expires_after_runs IS NOT NULL
            AND last_confirmed_at IS NOT NULL
            AND (SELECT COUNT(*) FROM runs
                  WHERE project_id = ? AND created_at > knowledge.last_confirmed_at)
                >= expires_after_runs
          ORDER BY id",
    )
    .bind(project)
    .bind(project)
    .fetch_all(&mut *transaction)
    .await?;
    let now = now.to_rfc3339();
    let mut expired = 0;
    for id in ids {
        let updated = sqlx::query(
            "UPDATE knowledge SET status = 'expired', ended_at = ?
              WHERE id = ? AND (source = 'consolidator' OR source = 'distiller')
                AND layer = 'episodic' AND status = 'active'",
        )
        .bind(&now)
        .bind(id)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() == 0 {
            continue;
        }
        sqlx::query(
            "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
             VALUES (?, 'active', 'expired', 'no longer confirmed by the measurement', ?)",
        )
        .bind(id)
        .bind(&now)
        .execute(&mut *transaction)
        .await?;
        expired += 1;
    }
    transaction.commit().await?;
    Ok(expired)
}

/// Promote measurements repeated across independent projects into the inherited machine scope.
pub async fn promote_to_machine(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<u64> {
    let fingerprints: Vec<String> = sqlx::query_scalar(
        "SELECT fingerprint FROM knowledge
          WHERE source = 'consolidator' AND layer = 'episodic'
            AND scope_kind = 'project' AND status = 'active' AND fingerprint IS NOT NULL
          GROUP BY fingerprint HAVING COUNT(DISTINCT scope_id) >= ?
          ORDER BY fingerprint",
    )
    .bind(PROMOTION_PROJECTS as i64)
    .fetch_all(pool)
    .await?;
    let mut promoted = 0;
    for fingerprint in fingerprints {
        let mut transaction = pool.begin().await?;
        let fenced: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM knowledge
              WHERE scope_kind = 'machine' AND scope_id IS NULL AND fingerprint = ?
                AND status IN ('proposed', 'active', 'rejected', 'reverted'))",
        )
        .bind(&fingerprint)
        .fetch_one(&mut *transaction)
        .await?;
        if fenced != 0 {
            transaction.commit().await?;
            continue;
        }
        let contributors: Vec<PromotionMember> = sqlx::query_as(
            "SELECT id, scope_id, generator, COALESCE(observations, 0) AS observations,
                    title, body
               FROM knowledge
              WHERE source = 'consolidator' AND layer = 'episodic'
                AND scope_kind = 'project' AND status = 'active' AND fingerprint = ?
                AND scope_id IS NOT NULL
              ORDER BY id",
        )
        .bind(&fingerprint)
        .fetch_all(&mut *transaction)
        .await?;
        let project_count = contributors
            .iter()
            .map(|row| row.scope_id.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        if project_count < PROMOTION_PROJECTS {
            transaction.commit().await?;
            continue;
        }
        let newest = contributors
            .last()
            .expect("a promotable fingerprint has contributors");
        if !contributors
            .iter()
            .all(|row| row.generator.as_deref() == newest.generator.as_deref())
        {
            let ids = contributors.iter().map(|row| row.id).collect::<Vec<_>>();
            tracing::warn!(contributor_ids = ?ids, "not promoting measurements with different generators");
            transaction.commit().await?;
            continue;
        }
        let observations = contributors.iter().map(|row| row.observations).sum::<i64>();
        let evidence = contributors
            .iter()
            .map(|row| Evidence {
                t: "knowledge",
                id: row.id,
            })
            .collect::<Vec<_>>();
        let measurement = Measurement {
            generator: newest.generator.clone(),
            fingerprint,
            observations,
            kind: "memory".to_owned(),
            title: newest.title.clone(),
            body: format!(
                "Measured in {project_count} projects, {observations} observations in all: {}",
                newest.body
            ),
            evidence: serde_json::to_string(&evidence).expect("evidence is serializable"),
        };
        let successor = insert_measurement(
            &mut transaction,
            ("machine", None),
            &measurement,
            now,
            None,
            &format!("promoted from {project_count} projects"),
        )
        .await?;
        let now_text = now.to_rfc3339();
        for contributor in contributors {
            let updated = sqlx::query(
                "UPDATE knowledge SET status = 'archived', ended_at = ?
                  WHERE id = ? AND source = 'consolidator' AND layer = 'episodic'
                    AND status = 'active'",
            )
            .bind(&now_text)
            .bind(contributor.id)
            .execute(&mut *transaction)
            .await?;
            if updated.rows_affected() == 0 {
                continue;
            }
            sqlx::query(
                "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
                 VALUES (?, 'active', 'archived', ?, ?)",
            )
            .bind(contributor.id)
            .bind(format!("promoted to machine scope as {successor}"))
            .bind(&now_text)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        promoted += 1;
    }
    Ok(promoted)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, Instant};

    use chrono::{DateTime, TimeZone, Utc};
    use serde_json::Value;

    use sqlx::{Row, SqlitePool};

    use super::{
        CONSOLIDATION_INTERVAL, archive_countless_rows, due, merge_duplicates, promote_to_machine,
        run_pass,
    };
    use crate::knowledge::{Known, Scope, approved, failure_signature, for_scope};

    const PROJECT: &str = "nucleos";
    const FAILURE: &str = "error: build failed because the linker refused output.exe";

    async fn test_pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
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

    async fn seed_run_at(
        pool: &SqlitePool,
        project: &str,
        gate_status: Option<&str>,
        gate_output: Option<&str>,
        created_at: DateTime<Utc>,
    ) -> i64 {
        let id = seed_run(pool, project, "completed", gate_status, gate_output).await;
        sqlx::query("UPDATE runs SET created_at = ? WHERE id = ?")
            .bind(created_at.to_rfc3339())
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        id
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

    async fn seed_machine_knowledge(pool: &SqlitePool, fingerprint: &str, status: &str) -> i64 {
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, generator, observations, fingerprint,
                kind, title, body, status, created_at, activated_at)
             VALUES ('episodic', 'machine', NULL, 'consolidator', 'gate', 6, ?,
                     'memory', 'machine title', 'machine body', ?,
                     '2026-09-29T00:00:00+00:00', '2026-09-29T00:00:00+00:00')",
        )
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

    /// The raw signature fingerprint: slice 1 matches `cause_key` against it (plan assumption).
    fn raw_fingerprint(output: &str) -> String {
        failure_signature(output).unwrap().fingerprint
    }

    async fn set_cause_key(pool: &SqlitePool, id: i64, cause_key: &str) -> sqlx::Result<()> {
        sqlx::query("UPDATE knowledge SET cause_key = ? WHERE id = ?")
            .bind(cause_key)
            .bind(id)
            .execute(pool)
            .await
            .map(|_| ())
    }

    async fn knowledge_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM knowledge")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// `(name, declared type, notnull, default)` of every column of `table`.
    async fn columns(pool: &SqlitePool, table: &str) -> Vec<(String, String, i64, Option<String>)> {
        sqlx::query(
            "SELECT name, type, \"notnull\" AS not_null, dflt_value FROM pragma_table_info(?)",
        )
        .bind(table)
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.get::<String, _>("name"),
                row.get::<String, _>("type"),
                row.get::<i64, _>("not_null"),
                row.get::<Option<String>, _>("dflt_value"),
            )
        })
        .collect()
    }

    #[allow(clippy::too_many_arguments)]
    async fn queue_row(
        pool: &SqlitePool,
        job_id: Option<i64>,
        item_id: Option<i64>,
        run_id: Option<i64>,
        tree_id: Option<&str>,
        node_id: Option<&str>,
        request_id: Option<i64>,
        decision_id: Option<&str>,
    ) -> u64 {
        sqlx::query(
            "INSERT OR IGNORE INTO distill_queue
               (cause, project_id, job_id, item_id, run_id, tree_id, node_id, request_id,
                decision_id, status, attempts, created_at)
             VALUES ('run_exhausted', 'nucleos', ?, ?, ?, ?, ?, ?, ?, 'pending', 0,
                     '2026-09-30T00:00:00+00:00')",
        )
        .bind(job_id)
        .bind(item_id)
        .bind(run_id)
        .bind(tree_id)
        .bind(node_id)
        .bind(request_id)
        .bind(decision_id)
        .execute(pool)
        .await
        .unwrap()
        .rows_affected()
    }

    /// Slice 1 lays every capture column of the spec, inert: nobody writes them yet.
    #[tokio::test]
    async fn migration_0316_adds_the_capture_columns_with_inert_defaults() {
        let pool = test_pool().await;
        let existing = seed_knowledge(
            &pool,
            "episodic",
            "project",
            PROJECT,
            "consolidator",
            Some("gate"),
            "gate:before-the-columns",
            "active",
            Some(2),
        )
        .await;

        let knowledge = columns(&pool, "knowledge").await;
        let find = |table: &[(String, String, i64, Option<String>)], name: &str| {
            table
                .iter()
                .find(|column| column.0 == name)
                .cloned()
                .unwrap_or_else(|| panic!("column {name} is missing"))
        };
        let cause_key = find(&knowledge, "cause_key");
        assert_eq!(cause_key.1, "TEXT");
        assert_eq!(cause_key.2, 0);
        assert_eq!(cause_key.3, None);
        let trial = find(&knowledge, "trial");
        assert_eq!(trial.1, "INTEGER");
        assert_eq!(trial.2, 1);
        assert_eq!(trial.3.as_deref(), Some("0"));
        let trial_started_at = find(&knowledge, "trial_started_at");
        assert_eq!(trial_started_at.1, "TEXT");
        assert_eq!(trial_started_at.2, 0);

        let queue = columns(&pool, "distill_queue").await;
        for (name, kind) in [
            ("tree_id", "TEXT"),
            ("node_id", "TEXT"),
            ("request_id", "INTEGER"),
            ("decision_id", "TEXT"),
        ] {
            let column = find(&queue, name);
            assert_eq!(column.1, kind, "{name}");
            assert_eq!(column.2, 0, "{name} must be nullable");
            assert_eq!(column.3, None, "{name}");
        }

        let row = sqlx::query(
            "SELECT trial, cause_key, trial_started_at FROM knowledge WHERE id = ?",
        )
        .bind(existing)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<i64, _>("trial"), 0);
        assert!(row.get::<Option<String>, _>("cause_key").is_none());
        assert!(row.get::<Option<String>, _>("trial_started_at").is_none());
    }

    /// The unique key of the queue tells subjects apart by every id, and still ignores a repeat.
    #[tokio::test]
    async fn distill_queue_once_tells_apart_trees_nodes_requests_and_decisions() {
        let pool = test_pool().await;

        assert_eq!(queue_row(&pool, None, None, None, None, None, None, None).await, 1);
        assert_eq!(
            queue_row(&pool, None, None, None, Some("tree-1"), None, None, None).await,
            1
        );
        assert_eq!(
            queue_row(&pool, None, None, None, Some("tree-2"), None, None, None).await,
            1
        );
        assert_eq!(
            queue_row(&pool, None, None, None, None, Some("node-1"), None, None).await,
            1
        );
        assert_eq!(
            queue_row(&pool, None, None, None, None, None, Some(7), None).await,
            1
        );
        assert_eq!(
            queue_row(&pool, None, None, None, None, None, None, Some("decision-1")).await,
            1
        );
        // An exact repeat of any of them is ignored.
        assert_eq!(queue_row(&pool, None, None, None, None, None, None, None).await, 0);
        assert_eq!(
            queue_row(&pool, None, None, None, Some("tree-1"), None, None, None).await,
            0
        );
        assert_eq!(
            queue_row(&pool, None, None, None, None, None, Some(7), None).await,
            0
        );
        assert_eq!(
            queue_row(&pool, None, None, None, None, None, None, Some("decision-1")).await,
            0
        );

        // The old shape (job / item / run) dedups exactly as before.
        assert_eq!(
            queue_row(&pool, Some(1), Some(2), Some(3), None, None, None, None).await,
            1
        );
        assert_eq!(
            queue_row(&pool, Some(1), Some(2), Some(3), None, None, None, None).await,
            0
        );
        assert_eq!(
            queue_row(&pool, Some(1), None, Some(3), None, None, None, None).await,
            1
        );

        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM distill_queue")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(total, 8);
    }

    /// Only a live row (active or proposed) holds its cause key; the machine scope dedups too.
    #[tokio::test]
    async fn a_live_cause_key_is_unique_per_scope_and_dead_rows_do_not_count() {
        let pool = test_pool().await;
        let seed = |scope_id: &'static str, status: &'static str, fingerprint: &'static str| {
            let pool = pool.clone();
            async move {
                seed_knowledge(
                    &pool,
                    "semantic",
                    "project",
                    scope_id,
                    "distiller",
                    None,
                    fingerprint,
                    status,
                    None,
                )
                .await
            }
        };
        let live = seed(PROJECT, "active", "title:live").await;
        set_cause_key(&pool, live, "cause-1").await.unwrap();

        // NULL cause keys never collide.
        seed(PROJECT, "active", "title:null-a").await;
        seed(PROJECT, "active", "title:null-b").await;

        // Dead twins with the same key do not count.
        for status in ["archived", "expired", "reverted"] {
            let twin = seed(PROJECT, status, "title:twin").await;
            set_cause_key(&pool, twin, "cause-1").await.unwrap();
        }
        // The same key in another scope is another cause.
        let other = seed("other-project", "active", "title:other").await;
        set_cause_key(&pool, other, "cause-1").await.unwrap();

        // A second live row with the same scope and key is refused, active or proposed.
        let second = seed(PROJECT, "active", "title:second").await;
        assert!(set_cause_key(&pool, second, "cause-1").await.is_err());
        let proposed = seed(PROJECT, "proposed", "title:proposed").await;
        assert!(set_cause_key(&pool, proposed, "cause-1").await.is_err());

        // A dead twin cannot be brought back to life next to a live row.
        let dead: i64 = sqlx::query_scalar(
            "SELECT id FROM knowledge WHERE cause_key = 'cause-1' AND status = 'archived'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            sqlx::query("UPDATE knowledge SET status = 'active' WHERE id = ?")
                .bind(dead)
                .execute(&pool)
                .await
                .is_err()
        );

        // Machine scope has scope_id NULL: the pair must dedupe all the same.
        let machine_a = seed_machine_knowledge(&pool, "gate:machine-a", "active").await;
        let machine_b = seed_machine_knowledge(&pool, "gate:machine-b", "active").await;
        set_cause_key(&pool, machine_a, "machine-cause").await.unwrap();
        assert!(set_cause_key(&pool, machine_b, "machine-cause").await.is_err());
    }

    /// A gate signature nobody explained is not a lesson: the consolidator no longer writes it.
    #[tokio::test]
    async fn a_repeated_gate_signature_without_a_row_that_has_a_cause_writes_nothing() {
        let pool = test_pool().await;
        for _ in 0..3 {
            seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        }
        // A row with the same fingerprint but no cause_key does not count as having a cause.
        let bare = seed_knowledge(
            &pool,
            "semantic",
            "project",
            PROJECT,
            "distiller",
            None,
            &gate_fingerprint(FAILURE),
            "active",
            Some(1),
        )
        .await;
        let before = known(&pool, bare).await;

        let report = run_pass(&pool, at(1)).await.unwrap();

        assert_eq!(report.remeasured, 0);
        assert_eq!(knowledge_count(&pool).await, 1);
        assert_eq!(known(&pool, bare).await, before);
    }

    /// F2: a row with a cause is reinforced in place; the id never changes and nothing is inserted.
    #[tokio::test]
    async fn a_repeated_gate_signature_with_a_cause_key_row_raises_observations_and_keeps_the_id() {
        let pool = test_pool().await;
        let cause = seed_knowledge(
            &pool,
            "semantic",
            "project",
            PROJECT,
            "distiller",
            None,
            "title:explained failure",
            "active",
            Some(1),
        )
        .await;
        set_cause_key(&pool, cause, &raw_fingerprint(FAILURE))
            .await
            .unwrap();
        // An owner row that was refused keeps its cause key but is dead: never reinforced.
        let other = seed_knowledge(
            &pool,
            "semantic",
            "project",
            PROJECT,
            "owner",
            None,
            "title:owner row",
            "rejected",
            Some(1),
        )
        .await;
        set_cause_key(&pool, other, &raw_fingerprint(FAILURE))
            .await
            .unwrap();
        let owner_before = known(&pool, other).await;
        let first_run = seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;

        let first = run_pass(&pool, at(1)).await.unwrap();
        assert_eq!(first.remeasured, 1);
        let row = known(&pool, cause).await;
        assert_eq!(row["observations"], 2);
        assert_eq!(row["last_confirmed_at"], at(1).to_rfc3339());
        assert_eq!(row["status"], "active");
        let evidence: Vec<Value> =
            serde_json::from_str(row["evidence"].as_str().expect("evidence is written")).unwrap();
        assert_eq!(evidence.len(), 2);
        assert!(evidence.iter().all(|item| item["t"] == "run"));
        assert!(evidence.contains(&serde_json::json!({"t": "run", "id": first_run})));

        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        let second = run_pass(&pool, at(2)).await.unwrap();
        assert_eq!(second.remeasured, 1);
        let row = known(&pool, cause).await;
        assert_eq!(row["observations"], 3);
        assert_eq!(row["last_confirmed_at"], at(2).to_rfc3339());
        let evidence: Vec<Value> =
            serde_json::from_str(row["evidence"].as_str().unwrap()).unwrap();
        assert_eq!(evidence.len(), 3, "an evidence item is never added twice");

        assert_eq!(knowledge_count(&pool).await, 2);
        assert_eq!(known(&pool, other).await, owner_before);
    }

    /// F3: a merge pass keeps the oldest id, so two passes never churn a new successor id.
    #[tokio::test]
    async fn two_merge_passes_over_duplicates_keep_the_oldest_id_and_create_no_new_row() {
        let pool = test_pool().await;
        let mut ids = Vec::new();
        for observations in [2, 3, 4] {
            ids.push(
                seed_knowledge(
                    &pool,
                    "episodic",
                    "project",
                    PROJECT,
                    "consolidator",
                    Some("gate"),
                    "gate:duplicate-twice",
                    "active",
                    Some(observations),
                )
                .await,
            );
        }
        let before = knowledge_count(&pool).await;

        assert_eq!(merge_duplicates(&pool, at(1)).await.unwrap(), 1);
        assert_eq!(merge_duplicates(&pool, at(2)).await.unwrap(), 0);

        assert_eq!(knowledge_count(&pool).await, before);
        let oldest = known(&pool, ids[0]).await;
        assert_eq!(oldest["status"], "active");
        assert_eq!(oldest["observations"], 9);
        assert_eq!(oldest["last_confirmed_at"], at(1).to_rfc3339());
        for id in &ids[1..] {
            assert_eq!(known(&pool, *id).await["status"], "archived");
        }
    }

    /// F4: a refused action is no longer turned into a count row.
    #[tokio::test]
    async fn refused_action_proposals_write_no_knowledge() {
        let pool = test_pool().await;
        for _ in 0..19 {
            seed_refusal(&pool, PROJECT, "Bash").await;
        }
        seed_refusal(&pool, PROJECT, "Edit").await;

        let report = run_pass(&pool, at(1)).await.unwrap();

        assert_eq!(knowledge_count(&pool).await, 0);
        assert!(report.is_quiet(), "{report:?}");
    }

    /// F5: the one-shot cleanup archives the countless rows and nothing else.
    #[tokio::test]
    async fn the_cleanup_archives_consolidator_rows_and_leaves_the_distiller_rows_alone() {
        let pool = test_pool().await;
        let mut count_rows = Vec::new();
        for (generator, fingerprint, status) in [
            ("gate", "gate:aaaa", "active"),
            ("gate", "gate:bbbb", "proposed"),
            ("refused-action", "refused-action:bash", "active"),
        ] {
            count_rows.push((
                seed_knowledge(
                    &pool,
                    "episodic",
                    "project",
                    PROJECT,
                    "consolidator",
                    Some(generator),
                    fingerprint,
                    status,
                    Some(7),
                )
                .await,
                status,
            ));
        }
        let machine = seed_machine_knowledge(&pool, "gate:machine", "active").await;
        let mut distiller = Vec::new();
        for index in 0..4 {
            let id = seed_knowledge(
                &pool,
                "semantic",
                "project",
                PROJECT,
                "distiller",
                None,
                &format!("title:distilled {index}"),
                "active",
                None,
            )
            .await;
            sqlx::query("UPDATE knowledge SET shown_count = ? WHERE id = ?")
                .bind(10 + index)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            distiller.push(id);
        }
        let reverted = seed_knowledge(
            &pool,
            "episodic",
            "project",
            PROJECT,
            "consolidator",
            Some("gate"),
            "gate:owner-reverted",
            "reverted",
            Some(5),
        )
        .await;
        let distiller_before = {
            let mut rows = Vec::new();
            for id in &distiller {
                rows.push(known(&pool, *id).await);
            }
            rows
        };
        let reverted_before = known(&pool, reverted).await;

        let archived = archive_countless_rows(&pool, at(1)).await.unwrap();

        assert_eq!(archived, 4);
        let active_left: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge
             WHERE source = 'consolidator' AND generator IN ('gate', 'refused-action')
               AND status IN ('active', 'proposed')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(active_left, 0);
        for (id, from_status) in count_rows
            .into_iter()
            .chain(std::iter::once((machine, "active")))
        {
            let row = known(&pool, id).await;
            assert_eq!(row["status"], "archived");
            assert_eq!(row["ended_at"], at(1).to_rfc3339());
            let event = sqlx::query(
                "SELECT from_status, to_status, note FROM knowledge_events
                 WHERE knowledge_id = ? ORDER BY id DESC LIMIT 1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(event.get::<String, _>("from_status"), from_status);
            assert_eq!(event.get::<String, _>("to_status"), "archived");
            assert_eq!(
                event.get::<String, _>("note"),
                "cleanup: consolidator count without cause"
            );
        }
        for (id, before) in distiller.iter().zip(distiller_before) {
            assert_eq!(known(&pool, *id).await, before);
        }
        assert_eq!(known(&pool, reverted).await, reverted_before);
        let distiller_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge_events WHERE knowledge_id IN (?, ?, ?, ?)",
        )
        .bind(distiller[0])
        .bind(distiller[1])
        .bind(distiller[2])
        .bind(distiller[3])
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(distiller_events, 0);
    }

    /// F5 is idempotent through its schema_meta flag, and archiving deletes nothing.
    #[tokio::test]
    async fn the_cleanup_twice_writes_no_second_event_and_run_knowledge_still_resolves() {
        let pool = test_pool().await;
        let row = seed_knowledge(
            &pool,
            "episodic",
            "project",
            PROJECT,
            "consolidator",
            Some("gate"),
            "gate:cleaned-once",
            "active",
            Some(6),
        )
        .await;
        let run = seed_run(&pool, PROJECT, "completed", Some("passed"), None).await;
        sqlx::query(
            "INSERT INTO run_knowledge
               (run_id, knowledge_id, shown, s_fts, s_scope, s_structure, s_recency, s_use, at)
             VALUES (?, ?, 1, 0.5, 0.5, 0.5, 0.5, 0.5, '2026-09-30T00:00:00+00:00')",
        )
        .bind(run)
        .bind(row)
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(archive_countless_rows(&pool, at(1)).await.unwrap(), 1);
        let flag: Option<String> = sqlx::query_scalar(
            "SELECT value FROM schema_meta WHERE key = 'consolidator.cleanup_v1'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert!(flag.is_some());
        let events_after_first: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM knowledge_events WHERE knowledge_id = ?")
                .bind(row)
                .fetch_one(&pool)
                .await
                .unwrap();

        // A row that appears after the first cleanup is not swept again: the flag is the gate.
        let later = seed_knowledge(
            &pool,
            "episodic",
            "project",
            PROJECT,
            "consolidator",
            Some("gate"),
            "gate:written-later",
            "active",
            Some(6),
        )
        .await;
        assert_eq!(archive_countless_rows(&pool, at(2)).await.unwrap(), 0);

        let events_after_second: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM knowledge_events WHERE knowledge_id = ?")
                .bind(row)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(events_after_first, 1);
        assert_eq!(events_after_second, events_after_first);
        assert_eq!(known(&pool, row).await["ended_at"], at(1).to_rfc3339());
        assert_eq!(known(&pool, later).await["status"], "active");

        // Nothing was deleted: the briefing trace of the archived row still joins its knowledge.
        let joined: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM run_knowledge rk
               JOIN knowledge k ON k.id = rk.knowledge_id
              WHERE rk.run_id = ? AND k.status = 'archived'",
        )
        .bind(run)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(joined, 1);
    }

    /// F6: failures painted in colour write no escape byte anywhere in the knowledge table.
    #[tokio::test]
    async fn a_pass_over_ansi_coloured_gate_failures_writes_no_escape_bytes() {
        let pool = test_pool().await;
        let coloured =
            "\x1b[1m\x1b[31merror\x1b[0m: build failed because the \x1b[33mlinker\x1b[0m refused output.exe";
        let plain = "error: build failed because the linker refused output.exe";
        // A row with a cause is reinforced from coloured output.
        let cause = seed_knowledge(
            &pool,
            "semantic",
            "project",
            PROJECT,
            "distiller",
            None,
            "title:coloured failure",
            "active",
            Some(1),
        )
        .await;
        set_cause_key(&pool, cause, &raw_fingerprint(plain))
            .await
            .unwrap();
        for _ in 0..3 {
            seed_run(&pool, PROJECT, "completed", Some("failed"), Some(coloured)).await;
        }

        run_pass(&pool, at(1)).await.unwrap();

        assert_eq!(known(&pool, cause).await["observations"], 3);
        assert_eq!(
            gate_fingerprint(coloured),
            gate_fingerprint(plain),
            "the signature ignores the colour"
        );
        let escaped: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge
             WHERE instr(title, char(27)) > 0 OR instr(body, char(27)) > 0
                OR instr(COALESCE(fingerprint, ''), char(27)) > 0
                OR instr(COALESCE(evidence, ''), char(27)) > 0",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(escaped, 0);
        assert_eq!(knowledge_count(&pool).await, 1);
    }

    /// Owner decision on open point 4: reversing a gate row the owner refused writes no count-only
    /// successor either.
    #[tokio::test]
    async fn an_owner_reversal_of_a_gate_row_writes_no_count_only_successor() {
        let pool = test_pool().await;
        let fingerprint = gate_fingerprint(FAILURE);
        let mut refused = Vec::new();
        for (project, status) in [("alpha", "rejected"), ("bravo", "reverted")] {
            refused.push(
                seed_knowledge(
                    &pool,
                    "episodic",
                    "project",
                    project,
                    "consolidator",
                    Some("gate"),
                    &fingerprint,
                    status,
                    Some(2),
                )
                .await,
            );
            for _ in 0..5 {
                seed_run(&pool, project, "completed", Some("passed"), None).await;
            }
        }
        let before = knowledge_count(&pool).await;

        let report = run_pass(&pool, at(1)).await.unwrap();
        run_pass(&pool, at(2)).await.unwrap();

        assert_eq!(report.successors, 0);
        assert_eq!(knowledge_count(&pool).await, before);
        for id in refused {
            let successors: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM knowledge WHERE supersedes = ?")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(successors, 0);
        }
    }

    /// A measured exception may bypass approval only while every production write is fenced to it.
    #[tokio::test]
    async fn the_consolidator_has_no_way_to_create_anything_but_a_measurement() {
        let pool = test_pool().await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        seed_refusal(&pool, PROJECT, "Bash").await;
        seed_refusal(&pool, PROJECT, "Bash").await;

        // The consolidator is a reinforcer now: a pass over repeated failures and refusals
        // creates no row of its own.
        run_pass(&pool, at(1)).await.unwrap();
        assert_eq!(knowledge_count(&pool).await, 0);

        let built_in = Path::new(env!("CARGO_MANIFEST_DIR"));
        let running_in = std::env::current_dir().expect("the working directory must be readable");
        assert_eq!(
            built_in,
            running_in.as_path(),
            "this test binary was compiled in {} and is running in {} -- a shared target directory handed this checkout a binary built somewhere else, so this scan would read the other checkout's sources. Touch this file to force a rebuild.",
            built_in.display(),
            running_in.display(),
        );
        let source = fs::read_to_string(running_in.join("../../src/consolidate.rs"))
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
            let flat = update.split_whitespace().collect::<String>();
            if update.contains("cause_key = ?") {
                // The one reinforcement statement: it may only raise a row that has a cause and
                // is the consolidator's or the distiller's, never an owner's.
                assert!(
                    flat.contains("sourceIN('consolidator','distiller')"),
                    "{update}"
                );
                assert!(!flat.contains("source='owner'"), "{update}");
                assert!(flat.contains("statusIN('active','proposed')"), "{update}");
                continue;
            }
            if update.contains("source = 'distiller'") {
                // The distiller's rows may be touched here only to expire them -- never to
                // renew, merge or rewrite what the distiller wrote.
                assert!(update.contains("status = 'expired'"), "{update}");
            } else {
                assert!(update.contains("source = 'consolidator'"), "{update}");
            }
            assert!(update.contains("layer = 'episodic'"), "{update}");
        }
    }

    /// A distilled episodic that nobody reconfirmed ages out like a measurement does, in a project
    /// the consolidator never wrote to; a distilled rule never ages out.
    #[tokio::test]
    async fn a_distilled_episode_nobody_reconfirms_expires_and_a_distilled_rule_does_not() {
        let pool = test_pool().await;
        let episode = seed_knowledge(
            &pool,
            "episodic",
            "project",
            PROJECT,
            "distiller",
            None,
            "title:an old episode",
            "active",
            None,
        )
        .await;
        let rule = seed_knowledge(
            &pool,
            "semantic",
            "project",
            PROJECT,
            "distiller",
            None,
            "title:an old rule",
            "active",
            None,
        )
        .await;
        for id in [episode, rule] {
            sqlx::query(
                "UPDATE knowledge SET expires_after_runs = 1, last_confirmed_at = ? WHERE id = ?",
            )
            .bind(at(1).to_rfc3339())
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }
        for _ in 0..3 {
            seed_run_at(&pool, PROJECT, Some("passed"), None, at(2)).await;
        }

        let report = run_pass(&pool, at(3)).await.unwrap();

        assert_eq!(report.expired, 1);
        let episode = known(&pool, episode).await;
        assert_eq!(episode["status"], "expired");
        assert!(!episode["ended_at"].is_null());
        let rule = known(&pool, rule).await;
        assert_eq!(rule["status"], "active");
        assert!(rule["ended_at"].is_null());
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

        assert_eq!(report.remeasured, 0);
        assert_eq!(knowledge_count(&pool).await, 2);
    }

    /// A cleared gate signature writes no successor and is never resurrected in place.
    #[tokio::test]
    async fn a_measurement_that_reverses_writes_no_successor() {
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

        let before = knowledge_count(&pool).await;
        let first = run_pass(&pool, at(1)).await.unwrap();
        assert_eq!(first.successors, 0);
        run_pass(&pool, at(2)).await.unwrap();
        run_pass(&pool, at(3)).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM knowledge WHERE supersedes = ?")
            .bind(refused)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
        assert_eq!(knowledge_count(&pool).await, before);
        assert_eq!(known(&pool, refused).await["status"], "rejected");

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
        let cause = seed_knowledge(
            &pool,
            "semantic",
            "project",
            PROJECT,
            "distiller",
            None,
            "title:remove file denied",
            "active",
            Some(1),
        )
        .await;
        set_cause_key(&pool, cause, &raw_fingerprint(legacy))
            .await
            .unwrap();
        for output in [legacy, nested, legacy, nested] {
            seed_run(&pool, PROJECT, "completed", Some("failed"), Some(output)).await;
        }
        for _ in 0..3 {
            seed_run(&pool, PROJECT, "completed", Some("passed"), None).await;
        }
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some("")).await;

        run_pass(&pool, at(1)).await.unwrap();

        assert_eq!(knowledge_count(&pool).await, 1);
        let row = sqlx::query("SELECT observations, evidence FROM knowledge WHERE id = ?")
            .bind(cause)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.get::<i64, _>("observations"), 4);
        let evidence: Value = serde_json::from_str(&row.get::<String, _>("evidence")).unwrap();
        let evidence = evidence.as_array().unwrap();
        assert_eq!(evidence.len(), 4);
        assert!(evidence.iter().all(|item| item["t"] == "run"));
    }

    /// A project in flight is not stable history, so the whole project waits for a later pass.
    #[tokio::test]
    async fn a_project_with_a_running_job_is_left_alone() {
        let pool = test_pool().await;
        let cause = seed_knowledge(
            &pool,
            "semantic",
            "project",
            PROJECT,
            "distiller",
            None,
            "title:linker refused",
            "active",
            Some(1),
        )
        .await;
        set_cause_key(&pool, cause, &raw_fingerprint(FAILURE))
            .await
            .unwrap();
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        seed_run(&pool, PROJECT, "completed", Some("failed"), Some(FAILURE)).await;
        let job = sqlx::query(
            "INSERT INTO jobs
               (project_id, project_root, status, max_items, created_at)
             VALUES (?, 'C:/Projects/nucleos', 'running', 4,
                     '2026-09-30T00:00:00+00:00')",
        )
        .bind(PROJECT)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let busy = run_pass(&pool, at(1)).await.unwrap();
        assert_eq!(busy.skipped_busy, 1);
        assert_eq!(known(&pool, cause).await["observations"], 1);

        sqlx::query("UPDATE jobs SET status = 'completed' WHERE id = ?")
            .bind(job)
            .execute(&pool)
            .await
            .unwrap();
        let settled = run_pass(&pool, at(2)).await.unwrap();
        assert_eq!(settled.remeasured, 1);
        assert_eq!(known(&pool, cause).await["observations"], 2);
    }

    /// An episodic measurement expires only after its full run window passes without confirmation.
    #[tokio::test]
    async fn an_episodic_row_nobody_confirms_expires_after_its_window_of_runs() {
        let pool = test_pool().await;
        let id = seed_knowledge(
            &pool,
            "episodic",
            "project",
            PROJECT,
            "consolidator",
            Some("refused-action"),
            "refused-action:Bash",
            "active",
            Some(2),
        )
        .await;
        sqlx::query(
            "UPDATE knowledge SET expires_after_runs = 3, last_confirmed_at = ? WHERE id = ?",
        )
        .bind(at(1).to_rfc3339())
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        seed_run_at(&pool, PROJECT, Some("passed"), None, at(2)).await;
        seed_run_at(&pool, PROJECT, Some("passed"), None, at(3)).await;

        let before_window = run_pass(&pool, at(4)).await.unwrap();
        assert_eq!(before_window.expired, 0);
        assert_eq!(known(&pool, id).await["status"], "active");

        seed_run_at(&pool, PROJECT, Some("passed"), None, at(4)).await;
        let at_window = run_pass(&pool, at(5)).await.unwrap();
        assert_eq!(at_window.expired, 1);
        let row = known(&pool, id).await;
        assert_eq!(row["status"], "expired");
        assert_eq!(row["ended_at"], at(5).to_rfc3339());
        let event = sqlx::query(
            "SELECT from_status, to_status, note FROM knowledge_events
             WHERE knowledge_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(event.get::<String, _>("from_status"), "active");
        assert_eq!(event.get::<String, _>("to_status"), "expired");
        assert_eq!(
            event.get::<String, _>("note"),
            "no longer confirmed by the measurement"
        );
    }

    /// A measurement refreshed earlier in the same pass starts a new expiry window.
    #[tokio::test]
    async fn a_row_the_measurement_reconfirms_does_not_expire() {
        let pool = test_pool().await;
        let fingerprint = gate_fingerprint(FAILURE);
        let id = seed_knowledge(
            &pool,
            "episodic",
            "project",
            PROJECT,
            "consolidator",
            Some("gate"),
            &fingerprint,
            "active",
            Some(2),
        )
        .await;
        sqlx::query(
            "UPDATE knowledge SET expires_after_runs = 3, last_confirmed_at = ? WHERE id = ?",
        )
        .bind(at(1).to_rfc3339())
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        // Only a row that has a cause is reinforced (and so reconfirmed) by the gate now.
        set_cause_key(&pool, id, &raw_fingerprint(FAILURE))
            .await
            .unwrap();
        for hour in 2..=4 {
            seed_run_at(&pool, PROJECT, Some("failed"), Some(FAILURE), at(hour)).await;
        }

        let report = run_pass(&pool, at(5)).await.unwrap();

        assert_eq!(report.remeasured, 1);
        assert_eq!(report.expired, 0);
        let row = known(&pool, id).await;
        assert_eq!(row["status"], "active");
        assert_eq!(row["last_confirmed_at"], at(5).to_rfc3339());
    }

    /// Assertions and procedures do not become false merely because newer runs exist.
    #[tokio::test]
    async fn a_fact_a_person_wrote_never_expires_by_age() {
        let pool = test_pool().await;
        let rows = [
            ("semantic", "owner", None),
            ("procedural", "run", None),
            ("semantic", "consolidator", Some("gate")),
        ];
        let mut ids = Vec::new();
        for (index, (layer, source, generator)) in rows.into_iter().enumerate() {
            let id = seed_knowledge(
                &pool,
                layer,
                "project",
                PROJECT,
                source,
                generator,
                &format!("old-fact-{index}"),
                "active",
                Some(1),
            )
            .await;
            sqlx::query(
                "UPDATE knowledge SET expires_after_runs = 1, last_confirmed_at = ? WHERE id = ?",
            )
            .bind(at(1).to_rfc3339())
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
            ids.push(id);
        }
        for _ in 0..10 {
            seed_run_at(&pool, PROJECT, Some("passed"), None, at(2)).await;
        }

        let report = run_pass(&pool, at(3)).await.unwrap();

        assert_eq!(report.expired, 0);
        for id in ids {
            let row = known(&pool, id).await;
            assert_eq!(row["status"], "active");
            assert!(row["ended_at"].is_null());
        }
    }

    /// When the gate now says the opposite, no count-only successor is written; the lesson is
    /// simply not reinforced and ages out through its normal lifecycle.
    #[tokio::test]
    async fn a_measurement_that_contradicts_an_active_lesson_writes_no_successor() {
        let pool = test_pool().await;
        let fingerprint = gate_fingerprint(FAILURE);
        let old = seed_knowledge(
            &pool,
            "episodic",
            "project",
            PROJECT,
            "consolidator",
            Some("gate"),
            &fingerprint,
            "active",
            Some(3),
        )
        .await;
        for hour in 1..=5 {
            seed_run_at(&pool, PROJECT, Some("passed"), None, at(hour)).await;
        }

        let before = knowledge_count(&pool).await;
        let first = run_pass(&pool, at(6)).await.unwrap();
        assert_eq!(first.superseded, 0);
        assert_eq!(first.successors, 0);
        let old_row = known(&pool, old).await;
        assert_eq!(old_row["status"], "active");
        assert!(old_row["ended_at"].is_null());
        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge_events WHERE knowledge_id = ? AND to_status = 'superseded'",
        )
        .bind(old)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(events, 0);

        run_pass(&pool, at(7)).await.unwrap();
        run_pass(&pool, at(8)).await.unwrap();
        let successors: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM knowledge WHERE supersedes = ?")
                .bind(old)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(successors, 0);
        assert_eq!(knowledge_count(&pool).await, before);
    }

    /// A matching fingerprint never gives the consolidator authority over an owner's row.
    #[tokio::test]
    async fn the_consolidator_does_not_supersede_a_row_the_owner_wrote() {
        let pool = test_pool().await;
        let fingerprint = gate_fingerprint(FAILURE);
        let owner = seed_knowledge(
            &pool,
            "episodic",
            "project",
            PROJECT,
            "owner",
            Some("gate"),
            &fingerprint,
            "active",
            Some(3),
        )
        .await;
        let before = known(&pool, owner).await;
        for hour in 1..=5 {
            seed_run_at(&pool, PROJECT, Some("passed"), None, at(hour)).await;
        }

        let report = run_pass(&pool, at(6)).await.unwrap();

        assert_eq!(report.superseded, 0);
        assert_eq!(known(&pool, owner).await, before);
        let successors: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM knowledge WHERE supersedes = ?")
                .bind(owner)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(successors, 0);
    }

    /// Exact duplicate measurements merge, while merely similar authority or layers stay separate.
    #[tokio::test]
    async fn merging_near_duplicates_sums_observations_and_archives_the_originals() {
        let pool = test_pool().await;
        let fingerprint = "gate:duplicate";
        let first = seed_knowledge(
            &pool,
            "episodic",
            "project",
            PROJECT,
            "consolidator",
            Some("gate"),
            fingerprint,
            "active",
            Some(2),
        )
        .await;
        let second = seed_knowledge(
            &pool,
            "episodic",
            "project",
            PROJECT,
            "consolidator",
            Some("gate"),
            fingerprint,
            "active",
            Some(3),
        )
        .await;
        sqlx::query(
            "UPDATE knowledge SET title = 'newest title', body = 'newest body' WHERE id = ?",
        )
        .bind(second)
        .execute(&pool)
        .await
        .unwrap();
        let owner = seed_knowledge(
            &pool,
            "episodic",
            "project",
            PROJECT,
            "owner",
            Some("gate"),
            fingerprint,
            "active",
            Some(8),
        )
        .await;
        let semantic_first = seed_knowledge(
            &pool,
            "semantic",
            "project",
            PROJECT,
            "consolidator",
            Some("gate"),
            "semantic-duplicate",
            "active",
            Some(1),
        )
        .await;
        let semantic_second = seed_knowledge(
            &pool,
            "semantic",
            "project",
            PROJECT,
            "consolidator",
            Some("gate"),
            "semantic-duplicate",
            "active",
            Some(1),
        )
        .await;

        let rows_before = knowledge_count(&pool).await;

        assert_eq!(merge_duplicates(&pool, at(1)).await.unwrap(), 1);

        // No successor row: the oldest id survives and absorbs the others.
        assert_eq!(knowledge_count(&pool).await, rows_before);
        let survivor = sqlx::query_as::<_, Known>("SELECT * FROM knowledge WHERE id = ?")
            .bind(first)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(survivor.status, "active");
        assert_eq!(survivor.observations, Some(5));
        assert_ne!(survivor.title, "newest title");
        assert!(survivor.ended_at.is_none());
        assert_eq!(
            survivor.last_confirmed_at.as_deref(),
            Some(at(1).to_rfc3339().as_str())
        );
        assert!(survivor.proposal_id.is_none());
        assert!(approved(&survivor));
        let evidence = serde_json::from_str::<Vec<Value>>(
            survivor.evidence.as_deref().expect("merge evidence"),
        )
        .unwrap();
        assert_eq!(
            evidence,
            vec![serde_json::json!({"t": "knowledge", "id": second})]
        );
        let row = known(&pool, second).await;
        assert_eq!(row["status"], "archived");
        assert_eq!(row["ended_at"], at(1).to_rfc3339());
        let note: String = sqlx::query_scalar(
            "SELECT note FROM knowledge_events WHERE knowledge_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(second)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(note, format!("merged into {first}"));
        assert_eq!(known(&pool, owner).await["status"], "active");
        assert_eq!(known(&pool, semantic_first).await["status"], "active");
        assert_eq!(known(&pool, semantic_second).await["status"], "active");
    }

    /// Promotion requires three projects and counts only the consolidator's measurements.
    #[tokio::test]
    async fn a_promotion_needs_three_distinct_projects_and_counts_only_measurements() {
        let pool = test_pool().await;
        let fingerprint = "gate:three-projects";
        let first = seed_knowledge(
            &pool,
            "episodic",
            "project",
            "alpha",
            "consolidator",
            Some("gate"),
            fingerprint,
            "active",
            Some(2),
        )
        .await;
        let second = seed_knowledge(
            &pool,
            "episodic",
            "project",
            "bravo",
            "consolidator",
            Some("gate"),
            fingerprint,
            "active",
            Some(3),
        )
        .await;

        assert_eq!(promote_to_machine(&pool, at(1)).await.unwrap(), 0);

        let third = seed_knowledge(
            &pool,
            "episodic",
            "project",
            "charlie",
            "consolidator",
            Some("gate"),
            fingerprint,
            "active",
            Some(4),
        )
        .await;
        sqlx::query(
            "UPDATE knowledge SET title = 'newest title', body = 'newest body' WHERE id = ?",
        )
        .bind(third)
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(promote_to_machine(&pool, at(2)).await.unwrap(), 1);

        let promoted = sqlx::query_as::<_, Known>(
            "SELECT * FROM knowledge
             WHERE scope_kind = 'machine' AND scope_id IS NULL AND fingerprint = ?",
        )
        .bind(fingerprint)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(promoted.observations, Some(9));
        assert_eq!(promoted.title, "newest title");
        assert_eq!(
            promoted.body,
            "Measured in 3 projects, 9 observations in all: newest body"
        );
        assert!(promoted.expires_after_runs.is_none());
        let evidence = serde_json::from_str::<Vec<Value>>(
            promoted.evidence.as_deref().expect("promotion evidence"),
        )
        .unwrap();
        assert_eq!(
            evidence,
            vec![
                serde_json::json!({"t": "knowledge", "id": first}),
                serde_json::json!({"t": "knowledge", "id": second}),
                serde_json::json!({"t": "knowledge", "id": third}),
            ]
        );
        for id in [first, second, third] {
            assert_eq!(known(&pool, id).await["status"], "archived");
            let note: String = sqlx::query_scalar(
                "SELECT note FROM knowledge_events WHERE knowledge_id = ? ORDER BY id DESC LIMIT 1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(
                note,
                format!("promoted to machine scope as {}", promoted.id)
            );
        }
        let promoted_note: String = sqlx::query_scalar(
            "SELECT note FROM knowledge_events WHERE knowledge_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(promoted.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(promoted_note, "promoted from 3 projects");

        let foreign = test_pool().await;
        for (project, source) in [
            ("alpha", "run"),
            ("bravo", "run"),
            ("charlie", "run"),
            ("delta", "owner"),
        ] {
            seed_knowledge(
                &foreign,
                "episodic",
                "project",
                project,
                source,
                Some("gate"),
                fingerprint,
                "active",
                Some(2),
            )
            .await;
        }
        assert_eq!(promote_to_machine(&foreign, at(2)).await.unwrap(), 0);
        let machine_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge WHERE scope_kind = 'machine' AND fingerprint = ?",
        )
        .bind(fingerprint)
        .fetch_one(&foreign)
        .await
        .unwrap();
        assert_eq!(machine_rows, 0);
    }

    /// A machine lesson prevents the next project pass from recreating its archived contributor.
    #[tokio::test]
    async fn a_promoted_lesson_is_not_written_again_per_project() {
        let pool = test_pool().await;
        let fingerprint = gate_fingerprint(FAILURE);
        for project in ["alpha", "bravo", "charlie"] {
            seed_knowledge(
                &pool,
                "episodic",
                "project",
                project,
                "consolidator",
                Some("gate"),
                &fingerprint,
                "active",
                Some(2),
            )
            .await;
        }
        assert_eq!(promote_to_machine(&pool, at(1)).await.unwrap(), 1);
        let machine_id: i64 = sqlx::query_scalar(
            "SELECT id FROM knowledge
             WHERE scope_kind = 'machine' AND scope_id IS NULL AND fingerprint = ?",
        )
        .bind(&fingerprint)
        .fetch_one(&pool)
        .await
        .unwrap();
        let before = known(&pool, machine_id).await;
        seed_run(&pool, "delta", "completed", Some("failed"), Some(FAILURE)).await;
        seed_run(&pool, "delta", "completed", Some("failed"), Some(FAILURE)).await;

        run_pass(&pool, at(2)).await.unwrap();

        let delta_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge
             WHERE scope_kind = 'project' AND scope_id = 'delta' AND fingerprint = ?",
        )
        .bind(&fingerprint)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(delta_rows, 0);
        assert_eq!(known(&pool, machine_id).await, before);
    }

    /// A rejected machine lesson fences the fingerprint from another promotion attempt.
    #[tokio::test]
    async fn a_refused_machine_lesson_is_not_promoted_again() {
        let pool = test_pool().await;
        let fingerprint = "gate:refused-machine";
        seed_machine_knowledge(&pool, fingerprint, "rejected").await;
        let mut contributors = Vec::new();
        for project in ["alpha", "bravo", "charlie"] {
            contributors.push(
                seed_knowledge(
                    &pool,
                    "episodic",
                    "project",
                    project,
                    "consolidator",
                    Some("gate"),
                    fingerprint,
                    "active",
                    Some(2),
                )
                .await,
            );
        }

        assert_eq!(promote_to_machine(&pool, at(1)).await.unwrap(), 0);
        for id in contributors {
            assert_eq!(known(&pool, id).await["status"], "active");
        }
        let machine_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge WHERE scope_kind = 'machine' AND fingerprint = ?",
        )
        .bind(fingerprint)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(machine_rows, 1);
    }

    /// A promoted machine lesson is inherited and approved in a project that did not contribute.
    #[tokio::test]
    async fn a_promoted_lesson_reaches_a_node_in_any_project() {
        let pool = test_pool().await;
        let fingerprint = "gate:inherited-machine";
        for project in ["alpha", "bravo", "charlie"] {
            seed_knowledge(
                &pool,
                "episodic",
                "project",
                project,
                "consolidator",
                Some("gate"),
                fingerprint,
                "active",
                Some(2),
            )
            .await;
        }
        assert_eq!(promote_to_machine(&pool, at(1)).await.unwrap(), 1);

        let rows = for_scope(&pool, &Scope::Project("delta".to_owned()))
            .await
            .unwrap();
        let promoted = rows
            .iter()
            .find(|row| row.fingerprint.as_deref() == Some(fingerprint))
            .expect("the machine lesson should be inherited by another project");
        assert_eq!(promoted.scope_kind, "machine");
        assert!(approved(promoted));
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
