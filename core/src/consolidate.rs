//! Periodic measurements distilled from durable gate and refusal histories.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, SqliteConnection, SqlitePool};

pub const CONSOLIDATION_INTERVAL: Duration = Duration::from_secs(6 * 3600);
const MIN_OBSERVATIONS: i64 = 2;
const PROMOTION_PROJECTS: usize = 3;
const EPISODIC_VALIDITY_RUNS: i64 = 50;
const CLEARED_WINDOW_RUNS: usize = 5;
const EVIDENCE_LIMIT: usize = 20;

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

#[derive(Debug, FromRow)]
struct DuplicateKey {
    scope_kind: String,
    scope_id: Option<String>,
    fingerprint: String,
}

#[derive(Debug, FromRow)]
struct DuplicateMember {
    id: i64,
    generator: Option<String>,
    observations: i64,
    kind: String,
    title: String,
    body: String,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Outcome {
    Created,
    Remeasured,
    Pending,
    Refused,
    Elsewhere,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PassReport {
    pub created: usize,
    pub remeasured: usize,
    pub pending: usize,
    pub refused: usize,
    pub elsewhere: usize,
    pub successors: usize,
    pub expired: usize,
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
             SELECT DISTINCT project_id FROM proposals
              WHERE kind = 'refused-action' AND project_id IS NOT NULL
             UNION
             SELECT DISTINCT scope_id AS project_id FROM knowledge
              WHERE scope_kind = 'project' AND scope_id IS NOT NULL
                AND source = 'consolidator' AND layer = 'episodic' AND status = 'active'
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
        report.superseded += supersede_contradicted(pool, &project, &window, now).await?;
        report.expired += expire_unconfirmed(pool, &project, now).await?;
    }
    promote_to_machine(pool, now).await?;
    Ok(report)
}

async fn write_measurement(
    pool: &SqlitePool,
    project: &str,
    measurement: &Measurement,
    now: DateTime<Utc>,
) -> sqlx::Result<Outcome> {
    let mut transaction = pool.begin().await?;
    let lives_at_machine_scope: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM knowledge
          WHERE scope_kind = 'machine' AND scope_id IS NULL AND fingerprint = ?
            AND source = 'consolidator' AND layer = 'episodic' AND status = 'active')",
    )
    .bind(&measurement.fingerprint)
    .fetch_one(&mut *transaction)
    .await?;
    if lives_at_machine_scope != 0 {
        transaction.commit().await?;
        return Ok(Outcome::Elsewhere);
    }
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
    insert_measurement(
        &mut transaction,
        ("project", Some(project)),
        measurement,
        now,
        None,
        Some(EPISODIC_VALIDITY_RUNS),
        "measured by the consolidator",
    )
    .await?;
    transaction.commit().await?;
    Ok(Outcome::Created)
}

fn record(report: &mut PassReport, outcome: Outcome) {
    match outcome {
        Outcome::Created => report.created += 1,
        Outcome::Remeasured => report.remeasured += 1,
        Outcome::Pending => report.pending += 1,
        Outcome::Refused => report.refused += 1,
        Outcome::Elsewhere => report.elsewhere += 1,
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
        generator: Some("gate".to_owned()),
        fingerprint,
        observations,
        kind: "memory".to_owned(),
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
        generator: Some("refused-action".to_owned()),
        fingerprint: format!("refused-action:{}", group.tool_name.trim().to_lowercase()),
        observations: group.observations,
        kind: "memory".to_owned(),
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
    scope: (&str, Option<&str>),
    measurement: &Measurement,
    now: DateTime<Utc>,
    supersedes: Option<i64>,
    expires_after_runs: Option<i64>,
    event_note: &str,
) -> sqlx::Result<i64> {
    let now = now.to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO knowledge (layer, scope_kind, scope_id, source, generator, evidence,
             observations, fingerprint, expires_after_runs, last_confirmed_at, kind, title, body,
             status, proposal_id, supersedes, origin_run_id, created_at, activated_at, ended_at)
         VALUES ('episodic', ?, ?, 'consolidator', ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 'active', NULL, ?, NULL, ?, ?, NULL)",
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
    .bind(supersedes)
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
            && write_reversal(pool, project, &row, &window, now, false).await?
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
    supersede_old: bool,
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
        ("project", Some(project)),
        &measurement,
        now,
        Some(refused.id),
        Some(EPISODIC_VALIDITY_RUNS),
        "measured by the consolidator",
    )
    .await?;
    if supersede_old {
        let now = now.to_rfc3339();
        let updated = sqlx::query(
            "UPDATE knowledge SET status = 'superseded', ended_at = ?
              WHERE id = ? AND source = 'consolidator' AND layer = 'episodic'
                AND generator = 'gate' AND status = 'active' AND supersedes IS NULL",
        )
        .bind(&now)
        .bind(refused.id)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            transaction.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
             VALUES (?, 'active', 'superseded', 'the measurement now says the opposite', ?)",
        )
        .bind(refused.id)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
    }
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
        generator: Some("gate".to_owned()),
        fingerprint: refused.fingerprint.clone(),
        observations: 0,
        kind: "memory".to_owned(),
        title: format!("Gate failure no longer recurs: {}", clipped(headline, 80)),
        body: format!(
            "The gate has not failed with this signature in the last {CLEARED_WINDOW_RUNS} gated runs in this project."
        ),
        evidence: serde_json::to_string(&evidence).expect("evidence is serializable"),
    }
}

async fn supersede_contradicted(
    pool: &SqlitePool,
    project: &str,
    window: &[GateRun],
    now: DateTime<Utc>,
) -> sqlx::Result<usize> {
    let active: Vec<RefusedGate> = sqlx::query_as(
        "SELECT id, fingerprint, title FROM knowledge
          WHERE scope_kind = 'project' AND scope_id = ? AND fingerprint IS NOT NULL
            AND source = 'consolidator' AND layer = 'episodic' AND status = 'active'
            AND generator = 'gate' AND supersedes IS NULL ORDER BY id DESC",
    )
    .bind(project)
    .fetch_all(pool)
    .await?;
    let window = as_window(window);
    let mut superseded = 0;
    for row in active {
        if reversed(&window, &row.fingerprint)
            && write_reversal(pool, project, &row, &window, now, true).await?
        {
            superseded += 1;
        }
    }
    Ok(superseded)
}

async fn expire_unconfirmed(
    pool: &SqlitePool,
    project: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<usize> {
    let mut transaction = pool.begin().await?;
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM knowledge
          WHERE scope_kind = 'project' AND scope_id = ? AND source = 'consolidator'
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
              WHERE id = ? AND source = 'consolidator' AND layer = 'episodic'
                AND status = 'active'",
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
            "SELECT id, generator, COALESCE(observations, 0) AS observations,
                    kind, title, body
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
        if members.len() < 2 {
            transaction.commit().await?;
            continue;
        }
        let newest = members.last().expect("duplicate group has a newest member");
        let evidence = members
            .iter()
            .map(|member| Evidence {
                t: "knowledge",
                id: member.id,
            })
            .collect::<Vec<_>>();
        let measurement = Measurement {
            generator: newest.generator.clone(),
            fingerprint: key.fingerprint,
            observations: members.iter().map(|member| member.observations).sum(),
            kind: newest.kind.clone(),
            title: newest.title.clone(),
            body: newest.body.clone(),
            evidence: serde_json::to_string(&evidence).expect("evidence is serializable"),
        };
        let successor = insert_measurement(
            &mut transaction,
            (&key.scope_kind, key.scope_id.as_deref()),
            &measurement,
            now,
            None,
            Some(EPISODIC_VALIDITY_RUNS),
            "measured by the consolidator",
        )
        .await?;
        let now_text = now.to_rfc3339();
        for member in members {
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
            .bind(format!("merged into {successor}"))
            .bind(&now_text)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        merged += 1;
    }
    Ok(merged)
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

    use super::{CONSOLIDATION_INTERVAL, due, merge_duplicates, promote_to_machine, run_pass};
    use crate::knowledge::{
        Declaration, Kind, Known, Scope, approved, failure_signature, for_scope, propose,
    };

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

    /// The public proposal door writes the same fence the consolidator reads.
    #[tokio::test]
    async fn a_proposal_written_by_propose_is_never_proposed_again() {
        let pool = test_pool().await;
        let fingerprint = gate_fingerprint(FAILURE);
        let (knowledge_id, _) = propose(
            &pool,
            Declaration {
                project_id: Some(PROJECT),
                origin_run_id: None,
                kind: Kind::Memory,
                title: "linker",
                body: FAILURE,
                reasoning: "seen twice",
                supersedes: None,
            },
        )
        .await
        .unwrap();

        let stored: Option<String> =
            sqlx::query_scalar("SELECT fingerprint FROM knowledge WHERE id = ?")
                .bind(knowledge_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored.as_deref(), Some(fingerprint.as_str()));

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

    /// When the gate now says the opposite, the old lesson ends and one reversal succeeds it.
    #[tokio::test]
    async fn a_measurement_that_contradicts_an_active_lesson_supersedes_it_once() {
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

        let first = run_pass(&pool, at(6)).await.unwrap();
        assert_eq!(first.superseded, 1);
        let old_row = known(&pool, old).await;
        assert_eq!(old_row["status"], "superseded");
        assert_eq!(old_row["ended_at"], at(6).to_rfc3339());
        let successor =
            sqlx::query("SELECT id, observations, title, body FROM knowledge WHERE supersedes = ?")
                .bind(old)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(successor.get::<i64, _>("observations"), 0);
        assert!(
            successor
                .get::<String, _>("title")
                .starts_with("Gate failure no longer recurs: ")
        );
        assert!(
            successor
                .get::<String, _>("body")
                .contains("last 5 gated runs")
        );
        let event = sqlx::query(
            "SELECT from_status, to_status, note FROM knowledge_events
             WHERE knowledge_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(old)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(event.get::<String, _>("from_status"), "active");
        assert_eq!(event.get::<String, _>("to_status"), "superseded");
        assert_eq!(
            event.get::<String, _>("note"),
            "the measurement now says the opposite"
        );

        run_pass(&pool, at(7)).await.unwrap();
        run_pass(&pool, at(8)).await.unwrap();
        let successors: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM knowledge WHERE supersedes = ?")
                .bind(old)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(successors, 1);
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

        assert_eq!(merge_duplicates(&pool, at(1)).await.unwrap(), 1);

        for id in [first, second] {
            let row = known(&pool, id).await;
            assert_eq!(row["status"], "archived");
            assert_eq!(row["ended_at"], at(1).to_rfc3339());
        }
        let successor = sqlx::query_as::<_, Known>(
            "SELECT * FROM knowledge
             WHERE source = 'consolidator' AND layer = 'episodic' AND status = 'active'
               AND fingerprint = ?",
        )
        .bind(fingerprint)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(successor.observations, Some(5));
        assert_eq!(successor.title, "newest title");
        assert_eq!(successor.body, "newest body");
        assert!(successor.proposal_id.is_none());
        assert!(approved(&successor));
        let mut evidence = serde_json::from_str::<Vec<Value>>(
            successor.evidence.as_deref().expect("merge evidence"),
        )
        .unwrap();
        evidence.sort_by_key(|item| item["id"].as_i64().unwrap());
        assert_eq!(
            evidence,
            vec![
                serde_json::json!({"t": "knowledge", "id": first}),
                serde_json::json!({"t": "knowledge", "id": second}),
            ]
        );
        for id in [first, second] {
            let note: String = sqlx::query_scalar(
                "SELECT note FROM knowledge_events WHERE knowledge_id = ? ORDER BY id DESC LIMIT 1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(note, format!("merged into {}", successor.id));
        }
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

        let report = run_pass(&pool, at(2)).await.unwrap();

        assert_eq!(report.elsewhere, 1);
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
