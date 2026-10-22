//! The AI workflow's ledgers, kept by the daemon (F4): the `workflow_records` table, the
//! `POST /workflow/records` route the `record` MCP tool calls, and the reads that join a task's
//! rework to the step that caught it and to the verifications it ran.
//!
//! A workflow bundle used to append JSONL to `.ai/local/ledgers/` from its own scripts. That state
//! lived in one checkout, no dashboard could read it, and nothing tied a rework to the gate that
//! caught it. Here every record names its task (`task_id`) and, for a rework, what caught it
//! (`caught_by`); the rest of what a script logged travels as `payload`, a JSON object the daemon
//! stores without interpreting, so a bundle can change what it records without a migration.

use std::time::Instant;

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::SqlitePool;

use crate::auth::Scope;
use crate::execution::git_exec::OPERATION_TIMEOUT;
use crate::state::AppState;
use crate::verify::runs::ORIGIN_VERIFY;
use crate::verify::{Caller, resolve_worktree};

pub const KIND_METRIC: &str = "metric";
pub const KIND_REWORK: &str = "rework";
pub const KIND_INTENT: &str = "intent";
pub const KIND_EVENT: &str = "event";

/// The closed vocabulary of `kind`, the same four values the migration's CHECK allows.
pub const KINDS: &[&str] = &[KIND_METRIC, KIND_REWORK, KIND_INTENT, KIND_EVENT];

/// The largest `payload` accepted, in bytes of its JSON text. A ledger row is a handful of fields
/// and a note; anything this size is a transcript or a diff pasted in by mistake, and the table is
/// read whole by the dashboard, so it is refused rather than truncated into something misleading.
pub const PAYLOAD_LIMIT: usize = 16 * 1024;

/// The longest `task_id`, `phase` or `caught_by`. They are identifiers to group by, not prose.
pub const LABEL_LIMIT: usize = 200;

/// Which step caught each rework, per task: rework rows grouped by `caught_by` and `phase`.
pub const REWORK_BY_CATCHER_SQL: &str = "SELECT task_id, caught_by, phase, COUNT(*) AS reworks \
     FROM workflow_records \
     WHERE project_id = ? AND kind = 'rework' \
     GROUP BY task_id, caught_by, phase \
     ORDER BY task_id, reworks DESC, caught_by, phase";

/// How many verifications each task ran: `verify_runs` grouped by `task_id`. `requests` counts the
/// `verify` calls (a call's units share its `origin_id`, the request id), `units` the rows those
/// calls queued or answered from the cache. Only `verify`-origin rows carry a task.
pub const VERIFICATIONS_PER_TASK_SQL: &str = "SELECT task_id, COUNT(DISTINCT origin_id) AS requests, COUNT(*) AS units \
     FROM verify_runs \
     WHERE project_id = ? AND origin = ? AND task_id IS NOT NULL \
     GROUP BY task_id \
     ORDER BY task_id";

/// The body of `POST /workflow/records`.
///
/// No `project_id`: the project is the one the worktree belongs to, resolved exactly as `verify`
/// resolves it, so a model can never file a record under a project it named itself.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordArgs {
    pub kind: String,
    pub task_id: String,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub caught_by: Option<String>,
    pub payload: Map<String, Value>,
    /// Required for the owner; a job node may omit it, and may only name its own.
    #[serde(default)]
    pub worktree: Option<String>,
}

/// One validated record, ready to insert.
#[derive(Debug, Clone, PartialEq)]
pub struct NewRecord {
    pub project_id: String,
    pub worktree: Option<String>,
    pub kind: String,
    pub task_id: String,
    pub phase: Option<String>,
    pub caught_by: Option<String>,
    /// The payload's JSON text, already measured against `PAYLOAD_LIMIT`.
    pub payload: String,
}

/// One stored record, as `for_task` reads it back.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct Record {
    pub id: i64,
    pub project_id: String,
    pub worktree: Option<String>,
    pub kind: String,
    pub task_id: String,
    pub phase: Option<String>,
    pub caught_by: Option<String>,
    pub created_at: String,
    pub payload: String,
}

/// One line of `REWORK_BY_CATCHER_SQL`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct ReworkCatch {
    pub task_id: String,
    pub caught_by: Option<String>,
    pub phase: Option<String>,
    pub reworks: i64,
}

/// One line of `VERIFICATIONS_PER_TASK_SQL`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct TaskVerifications {
    pub task_id: String,
    pub requests: i64,
    pub units: i64,
}

/// What the route answers once a record is stored.
#[derive(Debug, Clone, Serialize)]
pub struct Recorded {
    pub id: i64,
    pub project_id: String,
    pub worktree: Option<String>,
    pub kind: String,
    pub task_id: String,
}

/// A label trimmed, with blank meaning absent, and refused above `LABEL_LIMIT`.
fn label(name: &str, value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > LABEL_LIMIT {
        return Err(format!("{name} is longer than {LABEL_LIMIT} characters"));
    }
    Ok(Some(value.to_owned()))
}

/// **Pure.** Checks what the caller sent and builds the row, for a project already resolved.
///
/// Runs before the worktree is resolved, so a malformed record is refused without a git process.
pub fn validate(
    args: &RecordArgs,
    project_id: &str,
    worktree: Option<&str>,
) -> Result<NewRecord, String> {
    let kind = args.kind.trim();
    if !KINDS.contains(&kind) {
        return Err(format!(
            "kind must be one of {}, not {:?}",
            KINDS.join(", "),
            args.kind
        ));
    }
    let task_id = label("task_id", Some(args.task_id.as_str()))?
        .ok_or_else(|| "task_id is required and may not be blank".to_owned())?;
    let phase = label("phase", args.phase.as_deref())?;
    let caught_by = label("caught_by", args.caught_by.as_deref())?;
    if kind == KIND_REWORK && caught_by.is_none() {
        return Err(
            "caught_by is required on a rework record: name the step that caught it".to_owned(),
        );
    }
    let payload = serde_json::to_string(&args.payload).map_err(|error| error.to_string())?;
    if payload.len() > PAYLOAD_LIMIT {
        return Err(format!(
            "payload is {} bytes of JSON; the limit is {PAYLOAD_LIMIT}",
            payload.len()
        ));
    }
    Ok(NewRecord {
        project_id: project_id.to_owned(),
        worktree: worktree.map(str::to_owned),
        kind: kind.to_owned(),
        task_id,
        phase,
        caught_by,
        payload,
    })
}

/// Stores one validated record and returns its id.
pub async fn insert(pool: &SqlitePool, record: &NewRecord) -> sqlx::Result<i64> {
    let id = sqlx::query(
        "INSERT INTO workflow_records \
         (project_id, worktree, kind, task_id, phase, caught_by, created_at, payload) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&record.project_id)
    .bind(&record.worktree)
    .bind(&record.kind)
    .bind(&record.task_id)
    .bind(&record.phase)
    .bind(&record.caught_by)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(&record.payload)
    .execute(pool)
    .await?
    .last_insert_rowid();
    Ok(id)
}

/// Every record of one task in one project, oldest first.
pub async fn for_task(
    pool: &SqlitePool,
    project_id: &str,
    task_id: &str,
) -> sqlx::Result<Vec<Record>> {
    sqlx::query_as(
        "SELECT id, project_id, worktree, kind, task_id, phase, caught_by, created_at, payload \
         FROM workflow_records WHERE project_id = ? AND task_id = ? ORDER BY id",
    )
    .bind(project_id)
    .bind(task_id)
    .fetch_all(pool)
    .await
}

/// Which step caught each rework of a project's tasks (`REWORK_BY_CATCHER_SQL`).
pub async fn rework_by_catcher(
    pool: &SqlitePool,
    project_id: &str,
) -> sqlx::Result<Vec<ReworkCatch>> {
    sqlx::query_as(REWORK_BY_CATCHER_SQL)
        .bind(project_id)
        .fetch_all(pool)
        .await
}

/// How many verifications each of a project's tasks ran (`VERIFICATIONS_PER_TASK_SQL`).
pub async fn verifications_per_task(
    pool: &SqlitePool,
    project_id: &str,
) -> sqlx::Result<Vec<TaskVerifications>> {
    sqlx::query_as(VERIFICATIONS_PER_TASK_SQL)
        .bind(project_id)
        .bind(ORIGIN_VERIFY)
        .fetch_all(pool)
        .await
}

/// Validates, resolves the caller's worktree and project as `verify` does, and stores the record.
///
/// `validate` runs before the worktree is resolved, with no project yet, so a malformed body is
/// refused before any git process starts; the row it built is then completed with what the daemon
/// resolved, never with anything the caller said.
pub async fn record(
    pool: &SqlitePool,
    caller: Caller,
    args: &RecordArgs,
) -> Result<Recorded, (StatusCode, String)> {
    let mut row = validate(args, "", None).map_err(|message| (StatusCode::BAD_REQUEST, message))?;
    let deadline = Instant::now() + OPERATION_TIMEOUT;
    let (root, project_id) = resolve_worktree(pool, caller, args.worktree.as_deref(), deadline)
        .await
        .map_err(|error| (error.status(), error.message().to_owned()))?;
    row.project_id = project_id;
    row.worktree = Some(root.to_string_lossy().into_owned());
    let id = insert(pool, &row).await.map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not store the record: {error}"),
        )
    })?;
    Ok(Recorded {
        id,
        project_id: row.project_id,
        worktree: row.worktree,
        kind: row.kind,
        task_id: row.task_id,
    })
}

/// `POST /workflow/records`: one ledger record for the caller's own worktree.
///
/// The same keys as `verify` reach it — the owner and one autonomous run — because a record is
/// filed against a worktree the caller may verify, and a run is confined to its own.
pub async fn post_record(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(args): Json<RecordArgs>,
) -> Result<Json<Recorded>, (StatusCode, String)> {
    let caller = Caller::from_scope(&scope).ok_or_else(|| {
        (
            StatusCode::FORBIDDEN,
            "this key cannot record workflow state".to_owned(),
        )
    })?;
    record(&state.pool, caller, &args).await.map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(kind: &str, task_id: &str) -> RecordArgs {
        RecordArgs {
            kind: kind.to_owned(),
            task_id: task_id.to_owned(),
            phase: None,
            caught_by: None,
            payload: Map::new(),
            worktree: None,
        }
    }

    fn rework(task_id: &str, phase: &str, caught_by: &str) -> NewRecord {
        let mut payload = Map::new();
        payload.insert("class".to_owned(), Value::from("spec_gap"));
        validate(
            &RecordArgs {
                phase: Some(phase.to_owned()),
                caught_by: Some(caught_by.to_owned()),
                payload,
                ..args(KIND_REWORK, task_id)
            },
            "alpha",
            Some("/w/alpha"),
        )
        .unwrap()
    }

    #[test]
    fn the_four_kinds_are_accepted_and_nothing_else() {
        for kind in KINDS {
            let sent = RecordArgs {
                caught_by: Some("review".to_owned()),
                ..args(kind, "t-1")
            };
            assert!(validate(&sent, "alpha", None).is_ok(), "{kind}");
        }
        for kind in ["", "metrics", "Rework", "todo"] {
            let error = validate(&args(kind, "t-1"), "alpha", None).unwrap_err();
            assert!(error.contains("kind must be one of"), "{kind}: {error}");
        }
    }

    #[test]
    fn a_rework_without_its_catcher_is_refused() {
        let error = validate(&args(KIND_REWORK, "t-1"), "alpha", None).unwrap_err();
        assert!(error.contains("caught_by is required"), "{error}");
        let blank = RecordArgs {
            caught_by: Some("  ".to_owned()),
            ..args(KIND_REWORK, "t-1")
        };
        assert!(validate(&blank, "alpha", None).is_err());
        assert!(validate(&args(KIND_METRIC, "t-1"), "alpha", None).is_ok());
    }

    #[test]
    fn a_blank_task_id_is_refused_and_blank_labels_are_absent() {
        assert!(validate(&args(KIND_EVENT, "  "), "alpha", None).is_err());
        let row = validate(
            &RecordArgs {
                phase: Some(" ".to_owned()),
                caught_by: Some(" gate ".to_owned()),
                ..args(KIND_EVENT, " t-1 ")
            },
            "alpha",
            None,
        )
        .unwrap();
        assert_eq!(row.task_id, "t-1");
        assert_eq!(row.phase, None);
        assert_eq!(row.caught_by.as_deref(), Some("gate"));
        assert!(
            validate(
                &RecordArgs {
                    phase: Some("p".repeat(LABEL_LIMIT + 1)),
                    ..args(KIND_EVENT, "t-1")
                },
                "alpha",
                None,
            )
            .is_err()
        );
    }

    /// The cap is on the payload's JSON text: one byte over is refused, the limit itself is not.
    #[test]
    fn a_payload_over_the_limit_is_refused() {
        // `{"note":"..."}` is 11 bytes of framing around the string.
        let fits = "x".repeat(PAYLOAD_LIMIT - 11);
        let mut payload = Map::new();
        payload.insert("note".to_owned(), Value::from(fits.clone()));
        let at_limit = RecordArgs {
            payload: payload.clone(),
            ..args(KIND_METRIC, "t-1")
        };
        assert_eq!(
            validate(&at_limit, "alpha", None).unwrap().payload.len(),
            PAYLOAD_LIMIT
        );

        payload.insert("note".to_owned(), Value::from(format!("{fits}x")));
        let over = RecordArgs {
            payload,
            ..args(KIND_METRIC, "t-1")
        };
        let error = validate(&over, "alpha", None).unwrap_err();
        assert!(error.contains("limit"), "{error}");
    }

    #[tokio::test]
    async fn a_record_is_stored_and_read_back_by_task() {
        let pool = crate::testdb::fresh_pool().await;
        let row = rework("t-1", "execute", "gate");
        let id = insert(&pool, &row).await.unwrap();
        insert(&pool, &rework("t-2", "execute", "review"))
            .await
            .unwrap();

        let stored = for_task(&pool, "alpha", "t-1").await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, id);
        assert_eq!(stored[0].kind, KIND_REWORK);
        assert_eq!(stored[0].caught_by.as_deref(), Some("gate"));
        assert_eq!(stored[0].worktree.as_deref(), Some("/w/alpha"));
        assert_eq!(stored[0].payload, r#"{"class":"spec_gap"}"#);
        assert!(
            chrono::DateTime::parse_from_rfc3339(&stored[0].created_at).is_ok(),
            "{}",
            stored[0].created_at
        );
        assert!(for_task(&pool, "beta", "t-1").await.unwrap().is_empty());
    }

    /// The schema refuses a kind the Rust side would have: the CHECK is the last line of defence.
    #[tokio::test]
    async fn the_schema_refuses_an_unknown_kind() {
        let pool = crate::testdb::fresh_pool().await;
        let mut row = rework("t-1", "execute", "gate");
        row.kind = "todo".to_owned();
        assert!(insert(&pool, &row).await.is_err());
    }

    /// The acceptance queries: which step caught each rework, and how many verifications each
    /// task ran, over fixtures in two projects so the project filter is exercised too.
    #[tokio::test]
    async fn the_acceptance_queries_join_rework_and_verification_to_the_task() {
        let pool = crate::testdb::fresh_pool().await;
        for row in [
            rework("t-1", "execute", "gate"),
            rework("t-1", "execute", "gate"),
            rework("t-1", "review", "reviewer"),
            rework("t-2", "execute", "test_one"),
        ] {
            insert(&pool, &row).await.unwrap();
        }
        // Not rework, and not this project: neither may show up.
        let mut event = rework("t-1", "execute", "gate");
        event.kind = KIND_EVENT.to_owned();
        insert(&pool, &event).await.unwrap();
        let mut elsewhere = rework("t-1", "execute", "gate");
        elsewhere.project_id = "beta".to_owned();
        insert(&pool, &elsewhere).await.unwrap();

        assert_eq!(
            rework_by_catcher(&pool, "alpha").await.unwrap(),
            [
                ReworkCatch {
                    task_id: "t-1".to_owned(),
                    caught_by: Some("gate".to_owned()),
                    phase: Some("execute".to_owned()),
                    reworks: 2,
                },
                ReworkCatch {
                    task_id: "t-1".to_owned(),
                    caught_by: Some("reviewer".to_owned()),
                    phase: Some("review".to_owned()),
                    reworks: 1,
                },
                ReworkCatch {
                    task_id: "t-2".to_owned(),
                    caught_by: Some("test_one".to_owned()),
                    phase: Some("execute".to_owned()),
                    reworks: 1,
                },
            ]
        );

        // (project, origin, request id, task): t-1 made two calls, one of two units; t-2 one call;
        // a gate row and an untagged call carry no task; beta's row is another project's.
        for (project, origin, request, task) in [
            ("alpha", ORIGIN_VERIFY, 10, Some("t-1")),
            ("alpha", ORIGIN_VERIFY, 10, Some("t-1")),
            ("alpha", ORIGIN_VERIFY, 11, Some("t-1")),
            ("alpha", ORIGIN_VERIFY, 12, Some("t-2")),
            ("alpha", ORIGIN_VERIFY, 13, None),
            ("alpha", "job_item", 14, Some("t-1")),
            ("beta", ORIGIN_VERIFY, 15, Some("t-1")),
        ] {
            sqlx::query(
                "INSERT INTO verify_runs (project_id, worktree, scope, origin, origin_id, \
                 requested_by, argv, status, task_id) \
                 VALUES (?, '/w', 'own', ?, ?, 'own', '[]', 'passed', ?)",
            )
            .bind(project)
            .bind(origin)
            .bind(request)
            .bind(task)
            .execute(&pool)
            .await
            .unwrap();
        }

        assert_eq!(
            verifications_per_task(&pool, "alpha").await.unwrap(),
            [
                TaskVerifications {
                    task_id: "t-1".to_owned(),
                    requests: 2,
                    units: 3,
                },
                TaskVerifications {
                    task_id: "t-2".to_owned(),
                    requests: 1,
                    units: 1,
                },
            ]
        );
    }

    /// A malformed record is refused before the worktree is looked at: no runs row, no project and
    /// no git repository exist here, and the answer is still the validation error.
    #[tokio::test]
    async fn a_malformed_record_is_refused_before_the_worktree_is_resolved() {
        let pool = crate::testdb::fresh_pool().await;
        let (status, message) = record(&pool, Caller::Run(1), &args("todo", "t-1"))
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(message.contains("kind"), "{message}");
    }

    /// A run with no working directory has no worktree to file against, as with `verify`.
    #[tokio::test]
    async fn a_run_without_a_worktree_is_refused() {
        let pool = crate::testdb::fresh_pool().await;
        let (status, _) = record(&pool, Caller::Run(4242), &args(KIND_EVENT, "t-1"))
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflow_records")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, 0);
    }

    /// The owner names a registered worktree; the project is the daemon's answer for it, and the
    /// record carries the resolved root rather than the path as typed.
    #[tokio::test]
    async fn the_owner_records_against_the_project_its_worktree_belongs_to() {
        let pool = crate::testdb::fresh_pool().await;
        let repo = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["init", "-q", "-b", "main"])
            .status()
            .unwrap();
        assert!(status.success());
        let root = repo.path().to_string_lossy().into_owned();
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) \
             VALUES ('alpha', 'active', ?)",
        )
        .bind(&root)
        .execute(&pool)
        .await
        .unwrap();

        let recorded = record(
            &pool,
            Caller::Owner,
            &RecordArgs {
                worktree: Some(root.clone()),
                caught_by: Some("gate".to_owned()),
                ..args(KIND_REWORK, "t-9")
            },
        )
        .await
        .unwrap();
        assert_eq!(recorded.project_id, "alpha");
        assert_eq!(recorded.task_id, "t-9");
        let stored = for_task(&pool, "alpha", "t-9").await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].caught_by.as_deref(), Some("gate"));

        // The owner must name the worktree: there is no "own" one to fall back on.
        let (status, _) = record(&pool, Caller::Owner, &args(KIND_EVENT, "t-9"))
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
