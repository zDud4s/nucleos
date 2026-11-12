//! Knowledge history, evidence tagging, and findings.

use super::*;

/// One decision in a row's life, as a person reads it back.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Event {
    pub id: i64,
    pub from_status: Option<String>,
    pub to_status: String,
    pub note: Option<String>,
    pub at: String,
}

/// What this says, what it said before, and what replaced it — the reviewable history the whole
/// store is for, in one answer.
///
/// One call and not three, because the three are only useful together: "revert this" is a decision
/// a person makes by reading the text that would come back, and a screen that made them fetch it
/// separately is a screen where they revert without having read it.
#[derive(Debug, Serialize)]
pub struct History {
    pub known: Known,
    pub events: Vec<Event>,
    /// Newest first: what this one replaced, then what THAT replaced, back to the first text.
    pub replaced: Vec<Known>,
    /// What replaced this one, if a person has approved a successor.
    pub replaced_by: Option<Known>,
}

/// How far back a chain is read before the walk stops and says no more.
pub(super) const MAX_CHAIN: usize = 50;

/// Read one row, its own decisions, and the chain on both sides of it.
pub async fn history(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<History>> {
    let Some(known) = fetch(pool, id).await? else {
        return Ok(None);
    };

    let events = sqlx::query_as::<_, Event>(
        "SELECT id, from_status, to_status, note, at
           FROM knowledge_events WHERE knowledge_id = ? ORDER BY id",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;

    let mut replaced: Vec<Known> = Vec::new();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::from([id]);
    let mut next = known.supersedes;
    while let Some(previous) = next {
        // No path through this module can write a cycle — a predecessor must already exist, so
        // links only ever point backwards — but this is a loop over data, and a loop over data that
        // trusts it terminates until the day the data is wrong, and then it hangs the daemon
        // holding the connection instead of returning a poor answer.
        if !seen.insert(previous) || replaced.len() >= MAX_CHAIN {
            break;
        }
        let Some(row) = fetch(pool, previous).await? else {
            break;
        };
        next = row.supersedes;
        replaced.push(row);
    }

    // The newest successor, because a chain forked by two proposals approved out of order is a
    // thing SQLite will happily store and a person should still be able to read.
    let replaced_by = sqlx::query_as::<_, Known>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM knowledge WHERE supersedes = ? ORDER BY id DESC LIMIT 1"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(Some(History {
        known,
        events,
        replaced,
        replaced_by,
    }))
}

pub(super) async fn fetch(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<Known>> {
    sqlx::query_as::<_, Known>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM knowledge WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// The complete vocabulary a run may use to point at the source of a finding.
pub const EVIDENCE_TAGS: [&str; 8] = [
    "job",
    "run",
    "job_item",
    "proposal",
    "knowledge",
    "project",
    "command",
    "gate",
];

pub(super) fn known_element(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(tag) = object.get("t").and_then(serde_json::Value::as_str) else {
        return false;
    };
    if !EVIDENCE_TAGS.contains(&tag) {
        return false;
    }
    match object.get("id") {
        Some(serde_json::Value::Number(id)) => id.as_u64().is_some_and(|id| id > 0),
        Some(serde_json::Value::String(id)) => !id.is_empty() && id.chars().count() <= 120,
        _ => false,
    }
}

/// Keep only shaped, known referents and serialise them in the daemon's own JSON form.
pub fn tagged_evidence(raw: &serde_json::Value) -> Option<String> {
    struct Tagged<'a>(&'a serde_json::Map<String, serde_json::Value>);

    impl Serialize for Tagged<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            use serde::ser::SerializeMap;

            let mut clean = serializer.serialize_map(Some(self.0.len()))?;
            clean.serialize_entry("t", self.0.get("t").expect("known element has a tag"))?;
            clean.serialize_entry("id", self.0.get("id").expect("known element has an id"))?;
            for (key, value) in self.0 {
                if key != "t" && key != "id" {
                    clean.serialize_entry(key, value)?;
                }
            }
            clean.end()
        }
    }

    let kept: Vec<_> = raw
        .as_array()?
        .iter()
        .filter(|element| known_element(element))
        .filter_map(|element| {
            let object = element.as_object()?;
            Some(Tagged(object))
        })
        .collect();
    if kept.is_empty() {
        return None;
    }
    let normalised = serde_json::to_string(&kept).ok()?;
    debug_assert!(evidence_is_tagged(&normalised));
    Some(normalised)
}

/// Whether stored evidence still has at least one shaped, known referent.
pub fn evidence_is_tagged(stored: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(stored)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .is_some_and(|elements| elements.iter().any(known_element))
}

/// Why a run's attempted working-layer finding was refused.
#[derive(Debug)]
pub enum FindingError {
    EmptyFact,
    FactTooLong,
    NoEvidence,
    EvidenceTooLong,
    NoJob,
    JobEnded,
    TooMany,
    Db(sqlx::Error),
}

impl std::fmt::Display for FindingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FindingError::EmptyFact => write!(formatter, "a finding needs a fact"),
            FindingError::FactTooLong => write!(
                formatter,
                "a finding may be at most {PER_ITEM_CHARS} characters"
            ),
            FindingError::NoEvidence => write!(formatter, "a finding needs tagged evidence"),
            FindingError::EvidenceTooLong => write!(
                formatter,
                "a finding's evidence may be at most {MAX_EVIDENCE_CHARS} characters"
            ),
            FindingError::NoJob => write!(formatter, "the run does not belong to a job"),
            FindingError::JobEnded => write!(formatter, "the run's job has ended"),
            FindingError::TooMany => write!(
                formatter,
                "the job already has {MAX_LIVE_FINDINGS_PER_JOB} live findings"
            ),
            FindingError::Db(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for FindingError {}

impl From<sqlx::Error> for FindingError {
    fn from(error: sqlx::Error) -> Self {
        FindingError::Db(error)
    }
}

pub(super) const MAX_LIVE_FINDINGS_PER_JOB: i64 = 20;
/// About 3.3x the 600-character fact cap, leaving room for dozens of
/// `{"t":"run","id":N}` referents whose ids are already capped at 120 characters. Measured on
/// the normalised form because that is what is stored.
pub const MAX_EVIDENCE_CHARS: usize = 2_000;

/// Leave one evidenced fact for the next node of the caller's own live job.
pub async fn note_finding(
    pool: &SqlitePool,
    run_id: i64,
    fact: &str,
    evidence: &serde_json::Value,
) -> Result<i64, FindingError> {
    if fact.trim().is_empty() {
        return Err(FindingError::EmptyFact);
    }
    if fact.chars().count() > PER_ITEM_CHARS {
        return Err(FindingError::FactTooLong);
    }
    let evidence = tagged_evidence(evidence).ok_or(FindingError::NoEvidence)?;
    if evidence.chars().count() > MAX_EVIDENCE_CHARS {
        return Err(FindingError::EvidenceTooLong);
    }

    let Some((job_id, _project_id)) = sqlx::query_as::<_, (Option<i64>, Option<String>)>(
        "SELECT job_id, project_id FROM runs WHERE id = ?",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?
    else {
        return Err(FindingError::NoJob);
    };
    let Some(job_id) = job_id else {
        return Err(FindingError::NoJob);
    };
    let Some(job_status) = sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_optional(pool)
        .await?
    else {
        return Err(FindingError::JobEnded);
    };
    if crate::execution::job::TERMINAL_STATUSES.contains(&job_status.as_str()) {
        return Err(FindingError::JobEnded);
    }

    let title = clip(fact.lines().next().unwrap_or_default(), 80);
    let now = chrono::Utc::now().to_rfc3339();
    let job_scope = job_id.to_string();
    let inserted = sqlx::query(
        "INSERT INTO knowledge
           (layer, status, scope_kind, scope_id, source, generator, evidence, observations,
            fingerprint, kind, title, body, proposal_id, origin_run_id, created_at, activated_at)
         SELECT
           'working', 'live', 'job', ?, 'run', NULL, ?, NULL,
           NULL, 'memory', ?, ?, NULL, ?, ?, NULL
          WHERE (SELECT COUNT(*) FROM knowledge
                  WHERE scope_kind = 'job' AND scope_id = ? AND status = 'live') < ?",
    )
    .bind(&job_scope)
    .bind(evidence)
    .bind(title)
    .bind(fact)
    .bind(run_id)
    .bind(now)
    .bind(&job_scope)
    .bind(MAX_LIVE_FINDINGS_PER_JOB)
    .execute(pool)
    .await?;
    if inserted.rows_affected() == 0 {
        Err(FindingError::TooMany)
    } else {
        Ok(inserted.last_insert_rowid())
    }
}
