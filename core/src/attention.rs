//! The owner-attention brake for autonomous starts.
//!
//! Presence is an explicit, expiring heartbeat from a foreground client, never inferred from API
//! traffic: the shell polls the daemon, so traffic-derived presence would make the owner appear
//! permanently present and silently stop autonomy forever.
//!
//! No recorded heartbeat means the owner is absent and a new run is allowed. This is deliberately
//! not the usual fail-closed posture: a fresh install has heard from no foreground client yet, and
//! treating that valid empty answer as an error would disable every autonomous start on release.
//! An actual read or parse error is different and still fails closed.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::state::RunHandles;

/// A foreground client should refresh its lease well inside this window while the owner interacts.
pub const HEARTBEAT_WINDOW_SECONDS: i64 = 120;

/// Which owner-presence signal prevented a new autonomous start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttentionScope {
    Global,
    Project(String),
}

impl AttentionScope {
    fn db_parts(&self) -> (&'static str, &str) {
        match self {
            Self::Global => ("global", ""),
            Self::Project(project_id) => ("project", project_id),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Global => "global scope".to_string(),
            Self::Project(project_id) => format!("project `{project_id}`"),
        }
    }
}

/// Whether a project may start another autonomous run, judged by recent owner attention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttentionDecision {
    Allow,
    Defer {
        reason: String,
        scope: AttentionScope,
    },
}

/// PURE: whether an explicit heartbeat is still inside its self-clearing presence window.
///
/// A future timestamp remains active. It can happen after a local clock adjustment, and treating it
/// as expired would round a safety brake in the unsafe direction; it still clears once time passes
/// it by a full window.
pub fn heartbeat_is_active(
    last_seen_at: DateTime<Utc>,
    now: DateTime<Utc>,
    window: chrono::Duration,
) -> bool {
    now.signed_duration_since(last_seen_at) < window
}

/// Whether a foreground client is beating right now, globally.
///
/// The web pillar's trust rule (spec §5.2) is a conjunction, and this answers its first half: is
/// somebody looking? A page's text reaches an agent as written only when a person asked for it in
/// the foreground, never when a scheduled run did the asking at three in the morning.
///
/// It lives here rather than in `web.rs` because `attention_heartbeats` is this module's table, and
/// a pillar reaching into another module's SQL is exactly the coupling the module map exists to
/// prevent. It FAILS CLOSED like everything else in this file: an unreadable or unparseable
/// heartbeat answers "nobody is there", which costs fidelity and never safety.
pub async fn owner_is_present(pool: &SqlitePool, now: DateTime<Utc>) -> bool {
    let row = sqlx::query_as::<_, (String,)>(
        "SELECT last_seen_at FROM attention_heartbeats WHERE scope = 'global' AND project_id = ''",
    )
    .fetch_optional(pool)
    .await;

    let Ok(Some((last_seen_at,))) = row else {
        return false;
    };
    let Ok(last_seen_at) = DateTime::parse_from_rfc3339(&last_seen_at) else {
        return false;
    };

    heartbeat_is_active(
        last_seen_at.with_timezone(&Utc),
        now,
        chrono::Duration::seconds(HEARTBEAT_WINDOW_SECONDS),
    )
}

/// Records or refreshes the one heartbeat for `scope`.
pub async fn record_heartbeat(
    pool: &SqlitePool,
    scope: &AttentionScope,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let (scope_type, project_id) = scope.db_parts();
    sqlx::query(
        "INSERT INTO attention_heartbeats (scope, project_id, last_seen_at)
         VALUES (?, ?, ?)
         ON CONFLICT(scope, project_id) DO UPDATE SET last_seen_at = excluded.last_seen_at",
    )
    .bind(scope_type)
    .bind(project_id)
    .bind(now.to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

fn defer(scope: AttentionScope, reason: impl Into<String>) -> AttentionDecision {
    AttentionDecision::Defer {
        reason: reason.into(),
        scope,
    }
}

/// Whether an in-flight run is evidence that the OWNER is here.
///
/// This function used to answer two questions under one name, and only one of them was ever about
/// attention:
///
/// 1. *Is there already a run in flight in this project?* — that is concurrency, and it left. It is
///    `concurrency.rs`'s slot ceiling now, which can say "two" where this could only ever say "one".
/// 2. *Is the owner at the keyboard?* — that stayed, and it is what protects your nights.
///
/// The subtlety that survives the split is the one that made the old shape work by accident: **an
/// assistant turn is evidence of presence.** A run with no `project_id` is a turn you are having
/// with the daemon right now, and it deferred for the wrong reason (concurrency) with exactly the
/// right effect (you are in Telegram, so you are awake). It still defers, now for the stated reason.
///
/// Without that carve-out the split would have been a silent safety regression: send a message at
/// 23:00 and the daemon concludes nobody is there.
async fn in_flight_attention(
    pool: &SqlitePool,
    run_handles: &RunHandles,
) -> Result<Option<(AttentionScope, i64)>, String> {
    // Never hold the process-wide mutex across SQLite awaits. A handle removed after this snapshot
    // can defer at most one scheduler tick; a handle added afterwards is seen on the next tick.
    let run_ids = run_handles
        .lock()
        .map_err(|_| "could not read in-flight run handles: lock poisoned".to_string())?
        .keys()
        .copied()
        .collect::<Vec<_>>();

    for run_id in run_ids {
        let run_project =
            sqlx::query_scalar::<_, Option<String>>("SELECT project_id FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_optional(pool)
                .await
                .map_err(|error| format!("could not resolve in-flight run {run_id}: {error}"))?;

        match run_project {
            Some(None) => return Ok(Some((AttentionScope::Global, run_id))),
            // A run that belongs to a project is autonomous work, whatever project it is. It says
            // the machine is busy, which is the slot ceiling's business, and nothing at all about
            // whether anyone is watching.
            Some(Some(_)) => {}
            None => {
                return Err(format!("in-flight run {run_id} has no durable run record"));
            }
        }
    }

    Ok(None)
}

/// Fails CLOSED on a broken signal read, while allowing the valid empty state where no client has
/// ever posted a heartbeat. An assistant turn in flight is attention too: model work outlives the
/// request that started it, so the live handle map is checked before the expiring heartbeat rows.
///
/// Asks one question, since the split: is the owner here? How many pieces of work a project may run
/// at once is `concurrency.rs`'s, and used to be answered here under the same name.
pub async fn attention_permits_new_run(
    pool: &SqlitePool,
    run_handles: &RunHandles,
    project_id: &str,
    now: DateTime<Utc>,
) -> AttentionDecision {
    match in_flight_attention(pool, run_handles).await {
        Ok(Some((scope, run_id))) => {
            let reason = format!("turn {run_id} is still in flight in {}", scope.label());
            return defer(scope, reason);
        }
        Ok(None) => {}
        Err(reason) => {
            return defer(AttentionScope::Project(project_id.to_string()), reason);
        }
    }

    let rows = match sqlx::query_as::<_, (String, String, String)>(
        "SELECT scope, project_id, last_seen_at
         FROM attention_heartbeats
         WHERE (scope = 'global' AND project_id = '')
            OR (scope = 'project' AND project_id = ?)
         ORDER BY CASE scope WHEN 'global' THEN 0 ELSE 1 END",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            return defer(
                AttentionScope::Project(project_id.to_string()),
                format!("could not read attention heartbeats for project `{project_id}`: {error}"),
            );
        }
    };

    let window = chrono::Duration::seconds(HEARTBEAT_WINDOW_SECONDS);
    for (scope_type, row_project_id, last_seen_at) in rows {
        let scope = match scope_type.as_str() {
            "global" => AttentionScope::Global,
            "project" => AttentionScope::Project(row_project_id),
            _ => {
                return defer(
                    AttentionScope::Project(project_id.to_string()),
                    format!("attention heartbeat has invalid scope `{scope_type}`"),
                );
            }
        };
        let last_seen_at = match DateTime::parse_from_rfc3339(&last_seen_at) {
            Ok(value) => value.with_timezone(&Utc),
            Err(error) => {
                return defer(
                    scope,
                    format!("could not parse attention heartbeat timestamp: {error}"),
                );
            }
        };

        if heartbeat_is_active(last_seen_at, now, window) {
            let reason = format!("owner heartbeat is still active in {}", scope.label());
            return defer(scope, reason);
        }
    }

    AttentionDecision::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

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
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn no_runs() -> RunHandles {
        Arc::new(Mutex::new(HashMap::new()))
    }

    fn timestamp(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[tokio::test]
    async fn no_heartbeat_ever_recorded_allows() {
        let pool = test_pool().await;

        assert_eq!(
            attention_permits_new_run(
                &pool,
                &no_runs(),
                "project-a",
                timestamp("2026-07-29T12:00:00Z"),
            )
            .await,
            AttentionDecision::Allow
        );
    }

    #[tokio::test]
    async fn a_recent_heartbeat_blocks_then_clears_when_the_window_passes() {
        let pool = test_pool().await;
        let scope = AttentionScope::Project("project-a".to_string());
        let seen = timestamp("2026-07-29T12:00:00Z");
        record_heartbeat(&pool, &scope, seen).await.unwrap();

        assert!(matches!(
            attention_permits_new_run(
                &pool,
                &no_runs(),
                "project-a",
                seen + chrono::Duration::seconds(HEARTBEAT_WINDOW_SECONDS - 1),
            )
            .await,
            AttentionDecision::Defer { .. }
        ));
        assert_eq!(
            attention_permits_new_run(
                &pool,
                &no_runs(),
                "project-a",
                seen + chrono::Duration::seconds(HEARTBEAT_WINDOW_SECONDS),
            )
            .await,
            AttentionDecision::Allow
        );
    }

    #[tokio::test]
    async fn a_project_heartbeat_does_not_block_another_project() {
        let pool = test_pool().await;
        let now = timestamp("2026-07-29T12:00:00Z");
        record_heartbeat(
            &pool,
            &AttentionScope::Project("project-a".to_string()),
            now,
        )
        .await
        .unwrap();

        assert!(matches!(
            attention_permits_new_run(&pool, &no_runs(), "project-a", now).await,
            AttentionDecision::Defer { .. }
        ));
        assert_eq!(
            attention_permits_new_run(&pool, &no_runs(), "project-b", now).await,
            AttentionDecision::Allow
        );
    }

    #[tokio::test]
    async fn a_global_heartbeat_blocks_a_project_and_names_the_scope() {
        let pool = test_pool().await;
        let now = timestamp("2026-07-29T12:00:00Z");
        record_heartbeat(&pool, &AttentionScope::Global, now)
            .await
            .unwrap();

        let AttentionDecision::Defer { reason, scope } =
            attention_permits_new_run(&pool, &no_runs(), "project-a", now).await
        else {
            panic!("a global heartbeat must defer");
        };
        assert_eq!(scope, AttentionScope::Global);
        assert!(reason.contains("global scope"), "got: {reason}");
    }

    #[tokio::test]
    async fn a_read_error_defers_and_is_not_the_empty_allow_answer() {
        let pool = test_pool().await;
        pool.close().await;

        let decision = attention_permits_new_run(
            &pool,
            &no_runs(),
            "project-a",
            timestamp("2026-07-29T12:00:00Z"),
        )
        .await;
        let AttentionDecision::Defer { reason, scope } = decision else {
            panic!("an unreadable brake must defer");
        };
        assert_eq!(scope, AttentionScope::Project("project-a".to_string()));
        assert!(reason.contains("could not read"), "got: {reason}");
    }

    /// The split, pinned from both sides — and the second half is the one that matters.
    ///
    /// This function used to answer "is there already a run in flight in this project?" and defer on
    /// it. That is concurrency, and it moved to `concurrency.rs`, which can say "two" where this
    /// could only ever say "one". So autonomous work in a project no longer defers that project.
    ///
    /// What did NOT move is that **an assistant turn is evidence of presence**. A run with no
    /// `project_id` is a conversation you are having with the daemon right now; under the old shape
    /// it deferred for the wrong reason with exactly the right effect. Without this half the split
    /// would be a silent safety regression: send a message at 23:00 and the daemon concludes nobody
    /// is there.
    #[tokio::test]
    async fn a_turn_is_presence_but_a_projects_own_work_is_not() {
        let pool = test_pool().await;
        let now = timestamp("2026-07-29T12:00:00Z");

        let autonomous = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('project-a', 'work', 'running', 'worktree', '2026-07-29T12:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let work = tokio::spawn(std::future::pending::<()>());
        let handles = no_runs();
        handles
            .lock()
            .unwrap()
            .insert(autonomous, work.abort_handle());

        assert_eq!(
            attention_permits_new_run(&pool, &handles, "project-a", now).await,
            AttentionDecision::Allow,
            "a project's own autonomous work is the slot ceiling's business, not attention's"
        );

        // A turn: no project, because it belongs to the conversation rather than to a repository.
        let turn = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('are you there', 'running', 'shadow', '2026-07-29T12:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let talking = tokio::spawn(std::future::pending::<()>());
        handles.lock().unwrap().insert(turn, talking.abort_handle());

        let AttentionDecision::Defer { reason, scope } =
            attention_permits_new_run(&pool, &handles, "project-a", now).await
        else {
            panic!("a turn in flight means somebody is awake");
        };
        assert_eq!(scope, AttentionScope::Global);
        assert!(reason.contains(&turn.to_string()), "got: {reason}");

        work.abort();
        talking.abort();
    }
}
