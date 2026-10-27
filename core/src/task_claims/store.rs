//! SQL, the compare-and-swap and `is_live` (spec task-claims section 1).
//!
//! Every function that decides who owns a task runs in `BEGIN IMMEDIATE`: the write lock is taken up
//! front, so two claimers cannot both read "free" and both write. Inside a transaction only `&mut tx`
//! is used -- never the pool. Every time goes through `execution::wave::stamp` and is compared as TEXT.
//!
//! **Expiry is lazy.** Nothing is rewritten when a lease lapses; `is_live` is the one helper every
//! consumer (the claim, `task_list`, the verify, the land gate, the postgate, the hooks) asks, so an
//! expired claim counts as free everywhere.

#![allow(unused_imports)] // until the write functions of Tasks 5-9 use them; removed in Task 9

use super::{
    ClaimOutcome, Closer, FailedProbe, Holder, HolderKind, NewTask, ProbeStart, Status, TaskKey, TaskKind,
    TaskRow, PROBE_LEASE_SECONDS, SESSION_LEASE_SECONDS,
};
use crate::execution::wave::stamp;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};

#[derive(sqlx::FromRow)]
struct RawRow {
    id: i64,
    project_id: String,
    task_key: String,
    kind: String,
    title: String,
    detail: String,
    guard_paths: String,
    hint_paths: String,
    status: String,
    holder_kind: Option<String>,
    holder_id: Option<String>,
    holder_worktree: Option<String>,
    lease_until: Option<String>,
    checked_sha: Option<String>,
    probe_lease_until: Option<String>,
    created_at: String,
    claimed_at: Option<String>,
    closed_at: Option<String>,
    close_note: Option<String>,
}

fn corrupt(column: &str, detail: impl std::fmt::Display) -> sqlx::Error {
    sqlx::Error::Decode(format!("task_claims.{column}: {detail}").into())
}

fn json_list(column: &str, text: &str) -> sqlx::Result<Vec<String>> {
    serde_json::from_str(text).map_err(|error| corrupt(column, error))
}

fn decode(raw: RawRow) -> sqlx::Result<TaskRow> {
    let holder_kind = match raw.holder_kind.as_deref() {
        None => None,
        Some(text) => Some(HolderKind::parse(text).ok_or_else(|| corrupt("holder_kind", text))?),
    };
    Ok(TaskRow {
        id: raw.id,
        project_id: raw.project_id,
        key: TaskKey(raw.task_key),
        kind: TaskKind::parse(&raw.kind).ok_or_else(|| corrupt("kind", &raw.kind))?,
        title: raw.title,
        detail: serde_json::from_str(&raw.detail).map_err(|error| corrupt("detail", error))?,
        guard_paths: json_list("guard_paths", &raw.guard_paths)?,
        hint_paths: json_list("hint_paths", &raw.hint_paths)?,
        status: Status::parse(&raw.status).ok_or_else(|| corrupt("status", &raw.status))?,
        holder_kind,
        holder_id: raw.holder_id,
        holder_worktree: raw.holder_worktree,
        lease_until: raw.lease_until,
        checked_sha: raw.checked_sha,
        probe_lease_until: raw.probe_lease_until,
        created_at: raw.created_at,
        claimed_at: raw.claimed_at,
        closed_at: raw.closed_at,
        close_note: raw.close_note,
    })
}

pub(super) async fn row_by_id(conn: &mut SqliteConnection, id: i64) -> sqlx::Result<Option<TaskRow>> {
    sqlx::query_as::<_, RawRow>("SELECT * FROM task_claims WHERE id = ?")
        .bind(id)
        .fetch_optional(conn)
        .await?
        .map(decode)
        .transpose()
}

/// The row holding the unique-index slot for `(project, key)`: `probing`, `open` or `claimed`.
async fn unresolved_by_key(
    conn: &mut SqliteConnection,
    project_id: &str,
    key: &TaskKey,
) -> sqlx::Result<Option<TaskRow>> {
    sqlx::query_as::<_, RawRow>(
        "SELECT * FROM task_claims
         WHERE project_id = ? AND task_key = ? AND status IN ('probing','open','claimed')",
    )
    .bind(project_id)
    .bind(key.as_str())
    .fetch_optional(conn)
    .await?
    .map(decode)
    .transpose()
}

/// Whether a path `changed` is the guarded path or lies below it.
pub(super) fn paths_overlap(guard: &str, changed: &str) -> bool {
    let guard = guard.trim_end_matches('/');
    changed == guard || changed.strip_prefix(guard).is_some_and(|rest| rest.starts_with('/'))
}

// ---- liveness ------------------------------------------------------------------------------------

/// Is the claim alive at `now`? See the module note: the one question every consumer asks.
///
/// - `session` and `verify`/`probing`: `lease_until >= now` (the edge counts as live).
/// - `run` / `job`: the run or job is still active in its table.
/// - `postgate`: the project's `postgate_state` is still red.
/// - `open` (no holder) and closed rows: never.
pub async fn is_live(pool: &SqlitePool, row: &TaskRow, now: DateTime<Utc>) -> sqlx::Result<bool> {
    let mut conn = pool.acquire().await?;
    is_live_on(&mut conn, row, now).await
}

async fn is_live_on(conn: &mut SqliteConnection, row: &TaskRow, now: DateTime<Utc>) -> sqlx::Result<bool> {
    let leased = || {
        let at = stamp(now);
        row.lease_until.as_deref().is_some_and(|until| until >= at.as_str())
    };
    match row.status {
        Status::Open | Status::Done | Status::Abandoned => Ok(false),
        Status::Probing => Ok(leased()),
        Status::Claimed => match (row.holder_kind, row.holder_id.as_deref()) {
            (Some(HolderKind::Session | HolderKind::Verify), _) => Ok(leased()),
            (Some(HolderKind::Run), Some(id)) => run_is_live(conn, id).await,
            (Some(HolderKind::Job), Some(id)) => job_is_live(conn, id).await,
            (Some(HolderKind::Postgate), Some(project_id)) => project_is_red(conn, project_id).await,
            _ => Ok(false),
        },
    }
}

/// Spelled out, not assembled from `concurrency::LIVE_RUN_STATUSES`: sqlx refuses SQL built at run
/// time. `a_run_claim_is_live_only_while_its_run_is_active` walks the constant, so the two cannot drift.
async fn run_is_live(conn: &mut SqliteConnection, id: &str) -> sqlx::Result<bool> {
    let Ok(id) = id.parse::<i64>() else { return Ok(false) };
    let found: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM runs WHERE id = ? AND status IN ('running','awaiting_approval'))",
    )
    .bind(id)
    .fetch_one(conn)
    .await?;
    Ok(found != 0)
}

/// The statuses of `execution::job::store::LIVE_STATUSES`, spelled out for the same reason.
async fn job_is_live(conn: &mut SqliteConnection, id: &str) -> sqlx::Result<bool> {
    let Ok(id) = id.parse::<i64>() else { return Ok(false) };
    let found: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM jobs WHERE id = ? AND status IN ('running','awaiting_approval','waiting'))",
    )
    .bind(id)
    .fetch_one(conn)
    .await?;
    Ok(found != 0)
}

/// Red as the postgate itself reads it (`postgate::read_before`): a non-empty `red_groups` list.
async fn project_is_red(conn: &mut SqliteConnection, project_id: &str) -> sqlx::Result<bool> {
    let found: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM postgate_state WHERE project_id = ? AND red_groups NOT IN ('', '[]'))",
    )
    .bind(project_id)
    .fetch_one(conn)
    .await?;
    Ok(found != 0)
}

// ---- reads ---------------------------------------------------------------------------------------

/// The unresolved rows of a project, in id order, without the dead probes.
async fn live_rows_on(
    conn: &mut SqliteConnection,
    project_id: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Vec<TaskRow>> {
    let raws = sqlx::query_as::<_, RawRow>(
        "SELECT * FROM task_claims
         WHERE project_id = ? AND status IN ('probing','open','claimed') ORDER BY id",
    )
    .bind(project_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut rows = Vec::with_capacity(raws.len());
    for raw in raws {
        let row = decode(raw)?;
        if row.status != Status::Probing || is_live_on(conn, &row, now).await? {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// The project's unresolved tasks: `open`, `claimed` (also with a dead holder: the task is still
/// undone) and `probing` with a live lease. Whether a `claimed` row has a live owner is `is_live`.
pub async fn list_live(pool: &SqlitePool, project_id: &str, now: DateTime<Utc>) -> sqlx::Result<Vec<TaskRow>> {
    let mut conn = pool.acquire().await?;
    live_rows_on(&mut conn, project_id, now).await
}

/// The unresolved task with this key, if any (a dead probe does not count).
pub async fn live_by_key(
    pool: &SqlitePool,
    project_id: &str,
    key: &TaskKey,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<TaskRow>> {
    let mut conn = pool.acquire().await?;
    let Some(row) = unresolved_by_key(&mut conn, project_id, key).await? else { return Ok(None) };
    if row.status == Status::Probing && !is_live_on(&mut conn, &row, now).await? {
        return Ok(None);
    }
    Ok(Some(row))
}

/// The claims that gate a land: `claimed`, with a live holder, whose `guard_paths` cover some `changed`
/// file. `open` and `probing` rows never gate (spec section 1).
pub async fn guarding(
    pool: &SqlitePool,
    project_id: &str,
    changed: &[String],
    now: DateTime<Utc>,
) -> sqlx::Result<Vec<TaskRow>> {
    let mut conn = pool.acquire().await?;
    let mut hits = Vec::new();
    for row in live_rows_on(&mut conn, project_id, now).await? {
        let covers = row
            .guard_paths
            .iter()
            .any(|guard| changed.iter().any(|file| paths_overlap(guard, file)));
        if row.status == Status::Claimed && covers && is_live_on(&mut conn, &row, now).await? {
            hits.push(row);
        }
    }
    Ok(hits)
}

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    pub(super) fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 22, 10, 0, 0).unwrap()
    }

    pub(super) fn secs(n: i64) -> Duration {
        Duration::seconds(n)
    }

    pub(super) async fn test_pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    pub(super) fn session(id: &str) -> Holder {
        Holder { kind: HolderKind::Session, id: id.into(), worktree: format!("/wt/{id}") }
    }

    pub(super) fn task(key: &str) -> NewTask {
        NewTask {
            key: super::super::keys::normalize(key, std::path::Path::new("/repo")).unwrap(),
            kind: TaskKind::Declared,
            title: format!("fix {key}"),
            detail: serde_json::json!({}),
            guard_paths: vec![],
            hint_paths: vec![],
        }
    }

    pub(super) async fn row(pool: &SqlitePool, id: i64) -> TaskRow {
        let mut conn = pool.acquire().await.unwrap();
        row_by_id(&mut conn, id).await.unwrap().unwrap()
    }

    /// `(event, holder_id, note)` in order.
    pub(super) async fn events(pool: &SqlitePool, id: i64) -> Vec<(String, Option<String>, String)> {
        sqlx::query_as("SELECT event, holder_id, note FROM task_claim_events WHERE task_id = ? ORDER BY id")
            .bind(id)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// A row written straight into the table, so liveness is tested without the claim code.
    #[derive(Default)]
    pub(super) struct Seed<'a> {
        pub key: &'a str,
        pub status: &'a str,
        pub holder: Option<(&'a str, &'a str)>,
        pub lease: Option<DateTime<Utc>>,
        pub guard: &'a [&'a str],
    }

    pub(super) async fn seed(pool: &SqlitePool, seed: Seed<'_>) -> i64 {
        sqlx::query(
            "INSERT INTO task_claims (project_id, task_key, kind, title, status, holder_kind, holder_id,
                                      holder_worktree, lease_until, guard_paths, created_at)
             VALUES ('p', ?, 'incident', 't', ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(seed.key)
        .bind(seed.status)
        .bind(seed.holder.map(|h| h.0))
        .bind(seed.holder.map(|h| h.1))
        .bind(seed.holder.map(|_| "/wt/a"))
        .bind(seed.lease.map(stamp))
        .bind(serde_json::to_string(seed.guard).unwrap())
        .bind(stamp(t0()))
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    #[test]
    fn stamps_are_fixed_width_so_text_order_is_time_order() {
        let early = stamp(t0());
        let later = stamp(t0() + Duration::microseconds(1));
        let much_later = stamp(t0() + Duration::days(400));
        assert_eq!(early.len(), later.len());
        assert_eq!(early.len(), much_later.len());
        assert!(early < later && later < much_later);
    }

    #[test]
    fn a_guard_path_covers_itself_and_everything_below_it() {
        assert!(paths_overlap("core/a.rs", "core/a.rs"));
        assert!(paths_overlap("core/src", "core/src/a.rs"));
        assert!(paths_overlap("core/src/", "core/src/a.rs"));
        assert!(!paths_overlap("core/src", "core/src2/a.rs"));
        assert!(!paths_overlap("core/a.rs", "core"));
    }

    #[tokio::test]
    async fn a_session_claim_is_live_until_its_lease_runs_out() {
        let pool = test_pool().await;
        let id = seed(&pool, Seed {
            key: "file:a.rs", status: "claimed", holder: Some(("session", "s1")),
            lease: Some(t0() + secs(60)), ..Seed::default()
        }).await;
        let held = row(&pool, id).await;
        assert!(is_live(&pool, &held, t0()).await.unwrap());
        assert!(is_live(&pool, &held, t0() + secs(60)).await.unwrap(), "the lease edge is still live");
        assert!(!is_live(&pool, &held, t0() + secs(61)).await.unwrap());
    }

    #[tokio::test]
    async fn a_run_claim_is_live_only_while_its_run_is_active() {
        let pool = test_pool().await;
        for status in crate::execution::concurrency::LIVE_RUN_STATUSES {
            let run_id = sqlx::query("INSERT INTO runs (prompt, status, created_at) VALUES ('x', ?, 't')")
                .bind(status).execute(&pool).await.unwrap().last_insert_rowid();
            let id = seed(&pool, Seed {
                key: &format!("file:{status}.rs"), status: "claimed",
                holder: Some(("run", &run_id.to_string())), ..Seed::default()
            }).await;
            let held = row(&pool, id).await;
            assert!(is_live(&pool, &held, t0()).await.unwrap(), "a run '{status}' keeps its claim");
            sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
                .bind(run_id).execute(&pool).await.unwrap();
            assert!(!is_live(&pool, &held, t0()).await.unwrap(), "a finished run does not");
        }
        let id = seed(&pool, Seed {
            key: "file:odd.rs", status: "claimed", holder: Some(("run", "not-a-number")), ..Seed::default()
        }).await;
        assert!(!is_live(&pool, &row(&pool, id).await, t0()).await.unwrap(), "a holder id that names no run is dead");
    }

    #[tokio::test]
    async fn a_job_claim_is_live_only_while_its_job_is_active() {
        let pool = test_pool().await;
        // Iterates the real constant, like the run test, so a status added there is covered here.
        for status in crate::execution::job::LIVE_STATUSES {
            let job_id = sqlx::query(
                "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
                 VALUES ('p', '/r', ?, 1, 't')",
            ).bind(status).execute(&pool).await.unwrap().last_insert_rowid();
            let id = seed(&pool, Seed {
                key: &format!("file:{status}.rs"), status: "claimed",
                holder: Some(("job", &job_id.to_string())), ..Seed::default()
            }).await;
            let held = row(&pool, id).await;
            assert!(is_live(&pool, &held, t0()).await.unwrap(), "a job '{status}' keeps its claim");
            sqlx::query("UPDATE jobs SET status = 'completed' WHERE id = ?")
                .bind(job_id).execute(&pool).await.unwrap();
            assert!(!is_live(&pool, &held, t0()).await.unwrap());
        }
    }

    #[tokio::test]
    async fn a_postgate_claim_is_live_only_while_the_project_is_red() {
        let pool = test_pool().await;
        let id = seed(&pool, Seed {
            key: "master-red", status: "claimed", holder: Some(("postgate", "p")), ..Seed::default()
        }).await;
        let held = row(&pool, id).await;
        assert!(!is_live(&pool, &held, t0()).await.unwrap(), "no postgate_state row: not red");
        sqlx::query("INSERT INTO postgate_state (project_id, target, red_groups) VALUES ('p', 'main', '[\"core\"]')")
            .execute(&pool).await.unwrap();
        assert!(is_live(&pool, &held, t0()).await.unwrap());
        sqlx::query("UPDATE postgate_state SET red_groups = '[]' WHERE project_id = 'p'")
            .execute(&pool).await.unwrap();
        assert!(!is_live(&pool, &held, t0()).await.unwrap(), "back to green");
    }

    #[tokio::test]
    async fn open_and_closed_rows_are_never_live_and_a_probe_lives_by_its_lease() {
        let pool = test_pool().await;
        for status in ["open", "done", "abandoned"] {
            let id = seed(&pool, Seed { key: &format!("file:{status}.rs"), status, ..Seed::default() }).await;
            assert!(!is_live(&pool, &row(&pool, id).await, t0()).await.unwrap(), "{status}");
        }
        let probe = seed(&pool, Seed {
            key: "master-red", status: "probing", holder: Some(("verify", "ticket-1")),
            lease: Some(t0() + secs(600)), ..Seed::default()
        }).await;
        let probing = row(&pool, probe).await;
        assert!(is_live(&pool, &probing, t0() + secs(600)).await.unwrap());
        assert!(!is_live(&pool, &probing, t0() + secs(601)).await.unwrap());
    }

    #[tokio::test]
    async fn the_index_allows_one_unresolved_task_per_key_probing_included() {
        let pool = test_pool().await;
        seed(&pool, Seed { key: "master-red", status: "probing", holder: Some(("verify", "t")), ..Seed::default() }).await;
        let second = sqlx::query(
            "INSERT INTO task_claims (project_id, task_key, kind, title, status, created_at)
             VALUES ('p', 'master-red', 'incident', 't', 'open', 't')",
        ).execute(&pool).await;
        assert!(second.is_err(), "a second unresolved master-red in the same project is refused");
        sqlx::query(
            "INSERT INTO task_claims (project_id, task_key, kind, title, status, created_at)
             VALUES ('other', 'master-red', 'incident', 't', 'open', 't')",
        ).execute(&pool).await.expect("another project may have its own");
        sqlx::query("UPDATE task_claims SET status = 'done' WHERE project_id = 'p'")
            .execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO task_claims (project_id, task_key, kind, title, status, created_at)
             VALUES ('p', 'master-red', 'incident', 't', 'open', 't')",
        ).execute(&pool).await.expect("a closed task leaves the index");
    }

    #[tokio::test]
    async fn list_live_keeps_unresolved_tasks_and_hides_dead_probes_and_other_projects() {
        let pool = test_pool().await;
        let open = seed(&pool, Seed { key: "file:a.rs", status: "open", ..Seed::default() }).await;
        let expired = seed(&pool, Seed {
            key: "file:b.rs", status: "claimed", holder: Some(("session", "s1")),
            lease: Some(t0() - secs(1)), ..Seed::default()
        }).await;
        seed(&pool, Seed { key: "file:c.rs", status: "done", ..Seed::default() }).await;
        seed(&pool, Seed {
            key: "master-red", status: "probing", holder: Some(("verify", "t")),
            lease: Some(t0() - secs(1)), ..Seed::default()
        }).await;
        let live = list_live(&pool, "p", t0()).await.unwrap();
        let ids: Vec<i64> = live.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![open, expired], "an expired claim is still an unresolved task; a dead probe is not");
        assert!(list_live(&pool, "elsewhere", t0()).await.unwrap().is_empty());

        let key = TaskKey::master_red();
        assert!(live_by_key(&pool, "p", &key, t0()).await.unwrap().is_none(), "the dead probe is not an incident");
        let found = live_by_key(&pool, "p", &super::super::keys::normalize("file:a.rs", std::path::Path::new("/r")).unwrap(), t0())
            .await.unwrap();
        assert_eq!(found.map(|r| r.id), Some(open));
    }

    #[tokio::test]
    async fn guarding_returns_only_live_claimed_rows_whose_guard_paths_meet_the_change() {
        let pool = test_pool().await;
        let live_guard = seed(&pool, Seed {
            key: "master-red", status: "claimed", holder: Some(("session", "s1")),
            lease: Some(t0() + secs(60)), guard: &["core/src/a.rs", "core/tests"], ..Seed::default()
        }).await;
        seed(&pool, Seed {
            key: "file:expired.rs", status: "claimed", holder: Some(("session", "s2")),
            lease: Some(t0() - secs(1)), guard: &["core/src/a.rs"], ..Seed::default()
        }).await;
        seed(&pool, Seed { key: "file:open.rs", status: "open", guard: &["core/src/a.rs"], ..Seed::default() }).await;
        let changed = |paths: &[&str]| paths.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();

        let hit = guarding(&pool, "p", &changed(&["core/src/a.rs", "README.md"]), t0()).await.unwrap();
        assert_eq!(hit.iter().map(|r| r.id).collect::<Vec<_>>(), vec![live_guard]);
        let below = guarding(&pool, "p", &changed(&["core/tests/x.rs"]), t0()).await.unwrap();
        assert_eq!(below.len(), 1, "a guarded directory covers the files below it");
        assert!(guarding(&pool, "p", &changed(&["docs/x.md"]), t0()).await.unwrap().is_empty());
    }
}
