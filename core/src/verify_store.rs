//! Storage for the `verify` tool: the ticket (`verify_requests`) and the green-unit cache
//! (`verify_cache`).
//!
//! A request keeps what was asked and what the planner decided; the live state of each unit stays
//! in `verify_runs` and is read by `run_id`, so the ticket's status is derived and never stored.
//! The cache only ever holds green rows, which is what makes a hit safe to reuse.

use serde::{Deserialize, Serialize};
use sqlx::{Row as _, SqlitePool};

use crate::verify_runs;

/// One unit of a plan, as the ticket keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PlannedUnit {
    /// `None` is the project's `gate_command`.
    pub group: Option<String>,
    /// Empty when `skipped` is `Some`.
    pub argv: Vec<String>,
    /// "paths: a, b" | "full sweep: x" | "unclaimed paths" | "gate_command".
    pub why: String,
    pub fingerprint: Option<String>,
    pub cacheable: bool,
    /// The `verify_runs` row: queued or joined, or the `skipped_cached` record.
    pub run_id: Option<i64>,
    /// The `verify_runs` row whose green is reused.
    pub cached_from: Option<i64>,
    /// Why the unit will not run at all.
    pub skipped: Option<String>,
}

pub(crate) struct NewRequest<'a> {
    pub project_id: &'a str,
    pub worktree: &'a str,
    pub kind: &'a str,
    pub scope: &'a str,
    pub base: Option<&'a str>,
    pub priority: i64,
    pub caller: &'a str,
    pub note: Option<&'a str>,
    pub unclaimed: &'a [String],
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RequestRow {
    pub id: i64,
    pub project_id: String,
    pub worktree: String,
    pub kind: String,
    pub scope: String,
    pub base: Option<String>,
    pub priority: i64,
    pub caller: String,
    pub note: Option<String>,
    pub unclaimed: Vec<String>,
    pub plan: Vec<PlannedUnit>,
    pub created_at: String,
}

pub(crate) struct CacheKey<'a> {
    pub project_id: &'a str,
    pub group: &'a str,
    pub kind: &'a str,
    pub argv: &'a [String],
    pub fingerprint: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CacheHit {
    pub run_id: i64,
    pub duration_ms: Option<i64>,
}

pub(crate) const DAY_MS: i64 = 86_400_000;

/// The argv as `verify_runs.argv` stores it; the same encoding `verify_runs::enqueue` uses, so a
/// cache key and a queued row compare equal.
pub(crate) fn argv_json(argv: &[String]) -> String {
    serde_json::to_string(argv).unwrap_or_else(|_| "[]".into())
}

/// Oldest `created_ms` still fresh. Saturating so a huge `cache_days` cannot overflow.
fn cutoff_ms(now_ms: i64, max_age_days: u64) -> i64 {
    let age = i64::try_from(max_age_days)
        .unwrap_or(i64::MAX)
        .saturating_mul(DAY_MS);
    now_ms.saturating_sub(age)
}

pub(crate) async fn insert_request(
    pool: &SqlitePool,
    request: &NewRequest<'_>,
) -> sqlx::Result<i64> {
    let unclaimed = serde_json::to_string(request.unclaimed).unwrap_or_else(|_| "[]".into());
    let id = sqlx::query(
        "INSERT INTO verify_requests (project_id, worktree, kind, scope, base, priority, caller, \
         note, unclaimed, created_at, plan) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, '[]')",
    )
    .bind(request.project_id)
    .bind(request.worktree)
    .bind(request.kind)
    .bind(request.scope)
    .bind(request.base)
    .bind(request.priority)
    .bind(request.caller)
    .bind(request.note)
    .bind(unclaimed)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?
    .last_insert_rowid();
    Ok(id)
}

pub(crate) async fn set_plan(pool: &SqlitePool, id: i64, plan: &[PlannedUnit]) -> sqlx::Result<()> {
    let plan = serde_json::to_string(plan).unwrap_or_else(|_| "[]".into());
    sqlx::query("UPDATE verify_requests SET plan = ? WHERE id = ?")
        .bind(plan)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub(crate) async fn get_request(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<RequestRow>> {
    let Some(row) = sqlx::query(
        "SELECT id, project_id, worktree, kind, scope, base, priority, caller, note, unclaimed, \
         plan, created_at FROM verify_requests WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let unclaimed: String = row.try_get("unclaimed")?;
    let plan: String = row.try_get("plan")?;
    Ok(Some(RequestRow {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        worktree: row.try_get("worktree")?,
        kind: row.try_get("kind")?,
        scope: row.try_get("scope")?,
        base: row.try_get("base")?,
        priority: row.try_get("priority")?,
        caller: row.try_get("caller")?,
        note: row.try_get("note")?,
        // A column that does not parse is an empty list rather than a ticket nobody can read.
        unclaimed: serde_json::from_str(&unclaimed).unwrap_or_default(),
        plan: serde_json::from_str(&plan).unwrap_or_default(),
        created_at: row.try_get("created_at")?,
    }))
}

/// Records the tree a request measured. Write-once: a request that already holds one keeps it,
/// and the caller learns that by `false`.
pub(crate) async fn set_measured_tree(
    pool: &SqlitePool,
    id: i64,
    tree: &str,
) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE verify_requests SET measured_tree = ? WHERE id = ? AND measured_tree IS NULL",
    )
    .bind(tree)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// The tree request `id` recorded, or `None` when it never did (or the request does not exist).
/// Only tests read it back one request at a time; production asks `requests_measuring` by tree.
#[cfg(test)]
pub(crate) async fn measured_tree(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<String>> {
    let tree: Option<Option<String>> =
        sqlx::query_scalar("SELECT measured_tree FROM verify_requests WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(tree.flatten())
}

/// The `test`/`scope` requests of `project_id` that recorded exactly `tree`, newest first, with the
/// base each diffed from. Capped, because only the newest passed one is ever wanted.
pub(crate) async fn requests_measuring(
    pool: &SqlitePool,
    project_id: &str,
    tree: &str,
) -> sqlx::Result<Vec<(i64, String)>> {
    sqlx::query_as(
        "SELECT id, base FROM verify_requests WHERE project_id = ? AND measured_tree = ? \
         AND kind = 'test' AND scope = 'scope' AND base IS NOT NULL ORDER BY id DESC LIMIT 20",
    )
    .bind(project_id)
    .bind(tree)
    .fetch_all(pool)
    .await
}

pub(crate) async fn cache_lookup(
    pool: &SqlitePool,
    key: &CacheKey<'_>,
    now_ms: i64,
    max_age_days: u64,
) -> sqlx::Result<Option<CacheHit>> {
    let row = sqlx::query(
        "SELECT run_id, duration_ms FROM verify_cache WHERE project_id = ? AND group_name = ? \
         AND kind = ? AND argv = ? AND fingerprint = ? AND created_ms >= ?",
    )
    .bind(key.project_id)
    .bind(key.group)
    .bind(key.kind)
    .bind(argv_json(key.argv))
    .bind(key.fingerprint)
    .bind(cutoff_ms(now_ms, max_age_days))
    .fetch_optional(pool)
    .await?;
    row.map(|row| {
        Ok(CacheHit {
            run_id: row.try_get("run_id")?,
            duration_ms: row.try_get("duration_ms")?,
        })
    })
    .transpose()
}

pub(crate) async fn cache_store(
    pool: &SqlitePool,
    key: &CacheKey<'_>,
    run_id: i64,
    duration_ms: Option<i64>,
    now_ms: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO verify_cache (project_id, group_name, kind, argv, fingerprint, \
         run_id, duration_ms, created_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(key.project_id)
    .bind(key.group)
    .bind(key.kind)
    .bind(argv_json(key.argv))
    .bind(key.fingerprint)
    .bind(run_id)
    .bind(duration_ms)
    .bind(now_ms)
    .execute(pool)
    .await?;
    Ok(())
}

/// Drops entries older than the freshness window; returns how many went.
pub(crate) async fn cache_prune(
    pool: &SqlitePool,
    now_ms: i64,
    max_age_days: u64,
) -> sqlx::Result<u64> {
    let result = sqlx::query("DELETE FROM verify_cache WHERE created_ms < ?")
        .bind(cutoff_ms(now_ms, max_age_days))
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Writes the `skipped_cached` record a cache hit leaves in `verify_runs`, so the log shows the
/// unit was considered and why it did not run.
pub(crate) async fn record_cache_hit(
    pool: &SqlitePool,
    project_id: &str,
    worktree: &str,
    scope: &str,
    request_id: i64,
    key: &CacheKey<'_>,
) -> sqlx::Result<i64> {
    let argv = argv_json(key.argv);
    let now = chrono::Utc::now().to_rfc3339();
    verify_runs::record(
        pool,
        &verify_runs::Row {
            project_id: Some(project_id),
            worktree,
            sha: None,
            scope,
            origin: verify_runs::ORIGIN_VERIFY,
            origin_id: Some(request_id),
            ordinal: None,
            requested_by: scope,
            group_name: Some(key.group),
            kind: Some(key.kind),
            argv: &argv,
            fingerprint: Some(key.fingerprint),
            status: verify_runs::STATUS_SKIPPED_CACHED,
            exit_code: None,
            duration_ms: 0,
            started_at: &now,
            finished_at: &now,
            output_tail: None,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

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

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    fn key<'a>(argv: &'a [String], fingerprint: &'a str) -> CacheKey<'a> {
        CacheKey {
            project_id: "alpha",
            group: "core",
            kind: "test",
            argv,
            fingerprint,
        }
    }

    fn unit() -> PlannedUnit {
        PlannedUnit {
            group: Some("core".into()),
            argv: argv(&["run-tests", "core"]),
            why: "paths: core/a.rs".into(),
            fingerprint: Some("fp1:abc".into()),
            cacheable: true,
            run_id: Some(4),
            cached_from: None,
            skipped: None,
        }
    }

    #[tokio::test]
    async fn a_request_round_trips_with_its_plan() {
        let pool = test_pool().await;
        let unclaimed = vec!["docs/x.md".to_string(), "misc".to_string()];
        let id = insert_request(
            &pool,
            &NewRequest {
                project_id: "alpha",
                worktree: "/w/alpha",
                kind: "test",
                scope: "scope",
                base: Some("main"),
                priority: 5,
                caller: "run:9",
                note: Some("a note"),
                unclaimed: &unclaimed,
            },
        )
        .await
        .unwrap();
        let fresh = get_request(&pool, id).await.unwrap().unwrap();
        assert_eq!(fresh.plan, Vec::<PlannedUnit>::new());
        assert_eq!(fresh.unclaimed, unclaimed);

        let skipped = PlannedUnit {
            group: None,
            argv: Vec::new(),
            why: "gate_command".into(),
            fingerprint: None,
            cacheable: false,
            run_id: None,
            cached_from: Some(2),
            skipped: Some("no gate".into()),
        };
        let plan = vec![unit(), skipped];
        set_plan(&pool, id, &plan).await.unwrap();

        let row = get_request(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.id, id);
        assert_eq!(row.project_id, "alpha");
        assert_eq!(row.worktree, "/w/alpha");
        assert_eq!(row.kind, "test");
        assert_eq!(row.scope, "scope");
        assert_eq!(row.base.as_deref(), Some("main"));
        assert_eq!(row.priority, 5);
        assert_eq!(row.caller, "run:9");
        assert_eq!(row.note.as_deref(), Some("a note"));
        assert_eq!(row.unclaimed, unclaimed);
        assert_eq!(row.plan, plan);
        assert!(!row.created_at.is_empty());
    }

    #[tokio::test]
    async fn an_unknown_request_reads_as_none() {
        let pool = test_pool().await;
        assert_eq!(get_request(&pool, 999).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_stored_green_is_found_by_its_whole_key() {
        let pool = test_pool().await;
        let args = argv(&["run-tests", "-p", "core"]);
        cache_store(&pool, &key(&args, "fp1:a"), 11, Some(1500), 1_000)
            .await
            .unwrap();
        let hit = cache_lookup(&pool, &key(&args, "fp1:a"), 2_000, 7)
            .await
            .unwrap();
        assert_eq!(
            hit,
            Some(CacheHit {
                run_id: 11,
                duration_ms: Some(1500)
            })
        );
    }

    #[tokio::test]
    async fn changing_any_part_of_the_key_misses() {
        let pool = test_pool().await;
        let args = argv(&["run-tests"]);
        cache_store(&pool, &key(&args, "fp1:a"), 11, None, 1_000)
            .await
            .unwrap();
        let other_argv = argv(&["run-tests", "--release"]);
        let variants = [
            CacheKey {
                project_id: "beta",
                ..key(&args, "fp1:a")
            },
            CacheKey {
                group: "shell",
                ..key(&args, "fp1:a")
            },
            CacheKey {
                kind: "check",
                ..key(&args, "fp1:a")
            },
            key(&other_argv, "fp1:a"),
            key(&args, "fp1:b"),
        ];
        for variant in &variants {
            assert_eq!(
                cache_lookup(&pool, variant, 2_000, 7).await.unwrap(),
                None,
                "{} {} {} {:?} {}",
                variant.project_id,
                variant.group,
                variant.kind,
                variant.argv,
                variant.fingerprint
            );
        }
        assert!(
            cache_lookup(&pool, &key(&args, "fp1:a"), 2_000, 7)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn an_expired_green_is_not_reused() {
        let pool = test_pool().await;
        let args = argv(&["run-tests"]);
        let k = key(&args, "fp1:a");
        cache_store(&pool, &k, 11, None, 0).await.unwrap();
        // Exactly at the edge of the window it is still fresh.
        assert!(
            cache_lookup(&pool, &k, 7 * DAY_MS, 7)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            cache_lookup(&pool, &k, 7 * DAY_MS + 1, 7).await.unwrap(),
            None
        );
        assert!(
            cache_lookup(&pool, &k, 7 * DAY_MS + 1, 8)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn storing_twice_keeps_one_row_with_the_latest_run() {
        let pool = test_pool().await;
        let args = argv(&["run-tests"]);
        let k = key(&args, "fp1:a");
        cache_store(&pool, &k, 11, Some(10), 1_000).await.unwrap();
        cache_store(&pool, &k, 12, Some(20), 2_000).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM verify_cache")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(
            cache_lookup(&pool, &k, 3_000, 7).await.unwrap(),
            Some(CacheHit {
                run_id: 12,
                duration_ms: Some(20)
            })
        );
    }

    #[tokio::test]
    async fn prune_removes_only_expired_entries() {
        let pool = test_pool().await;
        let args = argv(&["run-tests"]);
        let old = key(&args, "fp1:old");
        let fresh = key(&args, "fp1:fresh");
        let now = 30 * DAY_MS;
        cache_store(&pool, &old, 1, None, now - 8 * DAY_MS)
            .await
            .unwrap();
        cache_store(&pool, &fresh, 2, None, now - DAY_MS)
            .await
            .unwrap();
        assert_eq!(cache_prune(&pool, now, 7).await.unwrap(), 1);
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM verify_cache")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 1);
        assert!(cache_lookup(&pool, &fresh, now, 7).await.unwrap().is_some());
        assert_eq!(cache_prune(&pool, now, 7).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_cache_hit_is_recorded_as_skipped_cached() {
        let pool = test_pool().await;
        let args = argv(&["run-tests", "-p", "core"]);
        let k = key(&args, "fp1:a");
        let id = record_cache_hit(&pool, "alpha", "/w/alpha", "own", 42, &k)
            .await
            .unwrap();
        #[allow(clippy::type_complexity)]
        let row: (
            Option<String>,
            String,
            String,
            Option<i64>,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
            String,
            Option<i64>,
            i64,
        ) = sqlx::query_as(
            "SELECT project_id, worktree, origin, origin_id, group_name, kind, argv, fingerprint, \
             status, exit_code, duration_ms FROM verify_runs WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0.as_deref(), Some("alpha"));
        assert_eq!(row.1, "/w/alpha");
        assert_eq!(row.2, verify_runs::ORIGIN_VERIFY);
        assert_eq!(row.3, Some(42));
        assert_eq!(row.4.as_deref(), Some("core"));
        assert_eq!(row.5.as_deref(), Some("test"));
        assert_eq!(row.6, argv_json(&args));
        assert_eq!(row.7.as_deref(), Some("fp1:a"));
        assert_eq!(row.8, verify_runs::STATUS_SKIPPED_CACHED);
        assert_eq!(row.9, None);
        assert_eq!(row.10, 0);
    }

    #[tokio::test]
    async fn argv_json_matches_what_the_queue_stores() {
        let pool = test_pool().await;
        let args = argv(&["sh", "-c", "echo \"hi\" && exit 0"]);
        let submitted = verify_runs::enqueue(
            &pool,
            &verify_runs::Request {
                project_id: Some("alpha".into()),
                worktree: "/w/alpha".into(),
                scope: "own".into(),
                origin: verify_runs::ORIGIN_VERIFY.into(),
                origin_id: Some(1),
                requested_by: "own".into(),
                group_name: Some("core".into()),
                kind: Some("test".into()),
                argv: args.clone(),
                fingerprint: Some("fp1:a".into()),
                priority: 0,
                weight: 1,
                timeout_ms: 1000,
            },
            0,
        )
        .await
        .unwrap();
        let stored: String = sqlx::query_scalar("SELECT argv FROM verify_runs WHERE id = ?")
            .bind(submitted.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, argv_json(&args));
        assert_eq!(argv_json(&[]), "[]");
    }
}
