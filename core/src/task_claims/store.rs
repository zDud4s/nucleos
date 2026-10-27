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

// ---- writes: claim --------------------------------------------------------------------------------

async fn begin(pool: &SqlitePool) -> sqlx::Result<sqlx::Transaction<'_, sqlx::Sqlite>> {
    pool.begin_with("BEGIN IMMEDIATE").await
}

fn lease_for(holder: &Holder, now: DateTime<Utc>) -> Option<String> {
    (holder.kind == HolderKind::Session).then(|| stamp(now + Duration::seconds(SESSION_LEASE_SECONDS)))
}

/// Appends one event inside a transaction the caller already holds. `who` is the holder the event is
/// ABOUT (for a takeover, the previous owner).
async fn record_event_on(
    conn: &mut SqliteConnection,
    task_id: i64,
    event: &str,
    who: Option<&Holder>,
    note: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO task_claim_events (task_id, event, holder_kind, holder_id, holder_worktree, at, note)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(task_id)
    .bind(event)
    .bind(who.map(|h| h.kind.as_str()))
    .bind(who.map(|h| h.id.as_str()))
    .bind(who.map(|h| h.worktree.as_str()))
    .bind(stamp(now))
    .bind(note)
    .execute(conn)
    .await?;
    Ok(())
}

/// Public, pool-level twin of `record_event_on`: opens its own `BEGIN IMMEDIATE` transaction. Chunk 3
/// writes the `override_land` event through it (there is no separate `record_override_land`).
pub async fn record_event(
    pool: &SqlitePool,
    task_id: i64,
    event: &str,
    who: Option<&Holder>,
    note: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let mut tx = begin(pool).await?;
    record_event_on(&mut tx, task_id, event, who, note, now).await?;
    tx.commit().await
}

/// True only when an `override_land` event exists for the CURRENT claim of the task: the task is
/// `claimed` and the event's `at` is at or after the row's `claimed_at`. `at >= claimed_at`, not `>`:
/// both are `wave::stamp` values (fixed-width UTC, microseconds), so text order is time order, and an
/// override recorded in the very microsecond of the claim must still count. An override left by a
/// previous owner is older than the new `claimed_at` and does not count. A non-empty `note` must also
/// equal the event's note; an empty one matches any.
pub async fn override_recorded(pool: &SqlitePool, task_id: i64, note: &str) -> sqlx::Result<bool> {
    let found: i64 = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM task_claim_events e JOIN task_claims t ON t.id = e.task_id
             WHERE e.task_id = ? AND e.event = 'override_land'
               AND t.status = 'claimed' AND t.claimed_at IS NOT NULL AND e.at >= t.claimed_at
               AND (? = '' OR e.note = ?))",
    )
    .bind(task_id)
    .bind(note)
    .bind(note)
    .fetch_one(pool)
    .await?;
    Ok(found != 0)
}

async fn insert_open(
    conn: &mut SqliteConnection,
    project_id: &str,
    new: &NewTask,
    now: DateTime<Utc>,
) -> sqlx::Result<i64> {
    let id = sqlx::query(
        "INSERT INTO task_claims (project_id, task_key, kind, title, detail, guard_paths, hint_paths, status, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'open', ?)",
    )
    .bind(project_id)
    .bind(new.key.as_str())
    .bind(new.kind.as_str())
    .bind(&new.title)
    .bind(new.detail.to_string())
    .bind(serde_json::to_string(&new.guard_paths).unwrap_or_else(|_| "[]".to_owned()))
    .bind(serde_json::to_string(&new.hint_paths).unwrap_or_else(|_| "[]".to_owned()))
    .bind(stamp(now))
    .execute(&mut *conn)
    .await?
    .last_insert_rowid();
    record_event_on(conn, id, "created", None, "", now).await?;
    Ok(id)
}

/// Deletes a task nobody saw resolve (a discarded probe) together with its events.
async fn delete_task(conn: &mut SqliteConnection, id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM task_claim_events WHERE task_id = ?").bind(id).execute(&mut *conn).await?;
    sqlx::query("DELETE FROM task_claims WHERE id = ?").bind(id).execute(conn).await?;
    Ok(())
}

/// Live unresolved tasks of the project that look like this one: the same file under the same key kind
/// (`test:core/a.rs::x` vs `test:core/a.rs::y`), or whose guard/hint paths overlap.
async fn similar_to(
    conn: &mut SqliteConnection,
    project_id: &str,
    key: &TaskKey,
    paths: &[String],
    exclude_id: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<Vec<TaskRow>> {
    let mut similar = Vec::new();
    for row in live_rows_on(conn, project_id, now).await? {
        if row.id == exclude_id || row.status == Status::Probing {
            continue;
        }
        let same_file =
            key.path().is_some() && key.prefix() == row.key.prefix() && key.path() == row.key.path();
        let touching = paths.iter().any(|mine| {
            row.guard_paths
                .iter()
                .chain(&row.hint_paths)
                .any(|theirs| paths_overlap(mine, theirs) || paths_overlap(theirs, mine))
        });
        if same_file || touching {
            similar.push(row);
        }
    }
    Ok(similar)
}

/// Create-or-claim in one `BEGIN IMMEDIATE` transaction. An unresolved row with the same key is adopted
/// and falls into the compare-and-swap; a live `probing` row is somebody's proof in flight (`Lost`,
/// holder `verify`), a dead one is replaced. Idempotent for the same holder.
pub async fn claim(
    pool: &SqlitePool,
    project_id: &str,
    new: NewTask,
    holder: &Holder,
    now: DateTime<Utc>,
) -> sqlx::Result<ClaimOutcome> {
    let mut tx = begin(pool).await?;
    let task_id = match unresolved_by_key(&mut tx, project_id, &new.key).await? {
        None => insert_open(&mut tx, project_id, &new, now).await?,
        Some(row) => {
            if row.status == Status::Probing && !is_live_on(&mut tx, &row, now).await? {
                delete_task(&mut tx, row.id).await?;
                insert_open(&mut tx, project_id, &new, now).await?
            } else {
                row.id
            }
        }
    };
    let outcome = cas(&mut tx, task_id, holder, now).await?;
    tx.commit().await?;
    Ok(outcome)
}

/// The compare-and-swap on an existing task (the postgate claiming the incident it created, an agent
/// claiming a task it saw in `task_list`). A missing or closed task is `sqlx::Error::RowNotFound`.
pub async fn claim_by_id(
    pool: &SqlitePool,
    task_id: i64,
    holder: &Holder,
    now: DateTime<Utc>,
) -> sqlx::Result<ClaimOutcome> {
    let mut tx = begin(pool).await?;
    let outcome = cas(&mut tx, task_id, holder, now).await?;
    tx.commit().await?;
    Ok(outcome)
}

async fn cas(
    conn: &mut SqliteConnection,
    task_id: i64,
    holder: &Holder,
    now: DateTime<Utc>,
) -> sqlx::Result<ClaimOutcome> {
    let row = row_by_id(conn, task_id).await?.ok_or(sqlx::Error::RowNotFound)?;
    let paths: Vec<String> = row.guard_paths.iter().chain(&row.hint_paths).cloned().collect();
    let similar = similar_to(conn, &row.project_id, &row.key, &paths, row.id, now).await?;
    match (row.status, row.holder()) {
        (Status::Open, _) => take(conn, row, holder, None, now, similar).await,
        (Status::Claimed, Some(current)) if current.same_owner(holder) => {
            renew(conn, row, holder, now, similar).await
        }
        (Status::Claimed, Some(current)) => {
            if is_live_on(conn, &row, now).await? {
                Ok(ClaimOutcome::Lost { task: row, holder: current, similar })
            } else {
                take(conn, row, holder, Some(current), now, similar).await
            }
        }
        (Status::Probing, Some(current)) => Ok(ClaimOutcome::Lost { task: row, holder: current, similar }),
        _ => Err(sqlx::Error::RowNotFound),
    }
}

/// `UPDATE ... WHERE <what was read>`: one row changed means this caller won.
async fn take(
    conn: &mut SqliteConnection,
    row: TaskRow,
    holder: &Holder,
    previous: Option<Holder>,
    now: DateTime<Utc>,
    similar: Vec<TaskRow>,
) -> sqlx::Result<ClaimOutcome> {
    let moved = sqlx::query(
        "UPDATE task_claims
         SET status = 'claimed', holder_kind = ?, holder_id = ?, holder_worktree = ?, lease_until = ?, claimed_at = ?
         WHERE id = ? AND status = ? AND holder_kind IS ? AND holder_id IS ?",
    )
    .bind(holder.kind.as_str())
    .bind(&holder.id)
    .bind(&holder.worktree)
    .bind(lease_for(holder, now))
    .bind(stamp(now))
    .bind(row.id)
    .bind(row.status.as_str())
    .bind(row.holder_kind.map(HolderKind::as_str))
    .bind(row.holder_id.as_deref())
    .execute(&mut *conn)
    .await?;
    if moved.rows_affected() == 0 {
        // Somebody else changed the row between the read and the write. Inside BEGIN IMMEDIATE this
        // cannot happen; if it ever does, report whoever holds it now rather than pretend a win.
        let task = row_by_id(conn, row.id).await?.ok_or(sqlx::Error::RowNotFound)?;
        let holder = task.holder().ok_or(sqlx::Error::RowNotFound)?;
        return Ok(ClaimOutcome::Lost { task, holder, similar });
    }
    match &previous {
        None => record_event_on(conn, row.id, "claimed", Some(holder), "", now).await?,
        Some(prev) => {
            let event = match prev.kind {
                HolderKind::Session | HolderKind::Verify => "expired_reclaimed",
                _ => "taken_over",
            };
            let note = format!("now held by {}:{}", holder.kind.as_str(), holder.id);
            record_event_on(conn, row.id, event, Some(prev), &note, now).await?;
        }
    }
    let task = row_by_id(conn, row.id).await?.ok_or(sqlx::Error::RowNotFound)?;
    Ok(ClaimOutcome::Won { task, similar })
}

/// The same owner claiming again: a win that only moves the lease (and the worktree it works from).
async fn renew(
    conn: &mut SqliteConnection,
    row: TaskRow,
    holder: &Holder,
    now: DateTime<Utc>,
    similar: Vec<TaskRow>,
) -> sqlx::Result<ClaimOutcome> {
    sqlx::query("UPDATE task_claims SET holder_worktree = ?, lease_until = ? WHERE id = ?")
        .bind(&holder.worktree)
        .bind(lease_for(holder, now))
        .bind(row.id)
        .execute(&mut *conn)
        .await?;
    let task = row_by_id(conn, row.id).await?.ok_or(sqlx::Error::RowNotFound)?;
    Ok(ClaimOutcome::Won { task, similar })
}

// ---- writes: incidents without a holder -------------------------------------------------------------

/// Shallow object merge: keys of `new` win, a `null` never erases what is there ("add what you know
/// on top of the live incident"). Anything that is not an object replaces, unless it is `null`.
fn merge_detail(old: &Value, new: &Value) -> Value {
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => {
            let mut merged = old.clone();
            for (key, value) in new {
                if !value.is_null() {
                    merged.insert(key.clone(), value.clone());
                }
            }
            Value::Object(merged)
        }
        (old, Value::Null) => old.clone(),
        (_, new) => new.clone(),
    }
}

fn union(old: &[String], new: &[String]) -> Vec<String> {
    let mut merged = old.to_vec();
    for path in new {
        if !merged.contains(path) {
            merged.push(path.clone());
        }
    }
    merged
}

fn encode_list(list: &[String]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".to_owned())
}

/// An incident the daemon found, with no holder. If an unresolved row with the same key exists it is
/// adopted: `detail` is merged and the paths unioned, the owner (if any) is left alone, and a `probing`
/// row becomes a plain `open` incident. Returns the resulting row.
pub async fn create_or_adopt_open(
    pool: &SqlitePool,
    project_id: &str,
    new: NewTask,
    now: DateTime<Utc>,
) -> sqlx::Result<TaskRow> {
    let mut tx = begin(pool).await?;
    let id = match unresolved_by_key(&mut tx, project_id, &new.key).await? {
        None => insert_open(&mut tx, project_id, &new, now).await?,
        Some(row) => {
            let detail = merge_detail(&row.detail, &new.detail).to_string();
            let guard = encode_list(&union(&row.guard_paths, &new.guard_paths));
            let hint = encode_list(&union(&row.hint_paths, &new.hint_paths));
            if row.status == Status::Probing {
                sqlx::query(
                    "UPDATE task_claims
                     SET status = 'open', holder_kind = NULL, holder_id = NULL, holder_worktree = NULL,
                         lease_until = NULL, kind = ?, title = ?, detail = ?, guard_paths = ?, hint_paths = ?
                     WHERE id = ? AND status = 'probing'",
                )
                .bind(new.kind.as_str())
                .bind(&new.title)
                .bind(detail)
                .bind(guard)
                .bind(hint)
                .bind(row.id)
                .execute(&mut *tx)
                .await?;
                record_event_on(&mut tx, row.id, "created", None, "adopted from a probe", now).await?;
            } else {
                sqlx::query("UPDATE task_claims SET detail = ?, guard_paths = ?, hint_paths = ? WHERE id = ?")
                    .bind(detail)
                    .bind(guard)
                    .bind(hint)
                    .bind(row.id)
                    .execute(&mut *tx)
                    .await?;
            }
            row.id
        }
    };
    let row = row_by_id(&mut tx, id).await?.ok_or(sqlx::Error::RowNotFound)?;
    tx.commit().await?;
    Ok(row)
}

/// Merges `detail` into the task's (`merge_detail`) and, when given, REPLACES the guard / hint paths
/// (the bisect finishing replaces the hint paths with the culprit commit's files). `now` is unused by
/// the columns today and kept so every store function has the same shape.
pub async fn update_detail(
    pool: &SqlitePool,
    task_id: i64,
    detail: Value,
    guard: Option<Vec<String>>,
    hint: Option<Vec<String>>,
    _now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let mut tx = begin(pool).await?;
    let row = row_by_id(&mut tx, task_id).await?.ok_or(sqlx::Error::RowNotFound)?;
    let merged = merge_detail(&row.detail, &detail).to_string();
    let guard = encode_list(guard.as_deref().unwrap_or(&row.guard_paths));
    let hint = encode_list(hint.as_deref().unwrap_or(&row.hint_paths));
    sqlx::query("UPDATE task_claims SET detail = ?, guard_paths = ?, hint_paths = ? WHERE id = ?")
        .bind(merged)
        .bind(guard)
        .bind(hint)
        .bind(task_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

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

    #[tokio::test]
    async fn claiming_a_new_key_creates_it_and_wins() {
        let pool = test_pool().await;
        let outcome = claim(&pool, "p", task("test:core/a.rs::it_works"), &session("s1"), t0()).await.unwrap();
        let ClaimOutcome::Won { task, similar } = outcome else { panic!("expected a win") };
        assert_eq!(task.status, Status::Claimed);
        assert_eq!(task.holder().unwrap().id, "s1");
        assert_eq!(task.lease_until, Some(stamp(t0() + secs(SESSION_LEASE_SECONDS))));
        assert_eq!(task.claimed_at, Some(stamp(t0())));
        assert!(similar.is_empty());
        let log: Vec<String> = events(&pool, task.id).await.into_iter().map(|e| e.0).collect();
        assert_eq!(log, vec!["created", "claimed"]);
    }

    #[tokio::test]
    async fn a_second_holder_loses_and_is_told_who_has_it() {
        let pool = test_pool().await;
        claim(&pool, "p", task("file:core/a.rs"), &session("s1"), t0()).await.unwrap();
        let outcome = claim(&pool, "p", task("file:core/a.rs"), &session("s2"), t0() + secs(5)).await.unwrap();
        let ClaimOutcome::Lost { task, holder, .. } = outcome else { panic!("expected a loss") };
        assert_eq!(holder.id, "s1");
        assert_eq!(task.holder().unwrap().id, "s1", "the row did not move");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_claims").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn the_same_holder_reclaiming_wins_again_and_only_renews() {
        let pool = test_pool().await;
        let first = claim(&pool, "p", task("master-red"), &session("s1"), t0()).await.unwrap();
        let id = match first { ClaimOutcome::Won { task, .. } => task.id, _ => panic!() };
        let again = claim(&pool, "p", task("master-red"), &session("s1"), t0() + secs(600)).await.unwrap();
        let ClaimOutcome::Won { task, .. } = again else { panic!("the same holder must not lose to itself") };
        assert_eq!(task.id, id);
        assert_eq!(task.lease_until, Some(stamp(t0() + secs(600 + SESSION_LEASE_SECONDS))));
        assert_eq!(task.claimed_at, Some(stamp(t0())), "claimed_at keeps the first claim");
        assert_eq!(events(&pool, id).await.len(), 2, "a renewal writes no event");
    }

    #[tokio::test]
    async fn an_expired_session_claim_is_reclaimed_and_the_event_keeps_the_previous_holder() {
        let pool = test_pool().await;
        let ClaimOutcome::Won { task: first, .. } =
            claim(&pool, "p", task("file:core/a.rs"), &session("s1"), t0()).await.unwrap() else { panic!() };
        let late = t0() + secs(SESSION_LEASE_SECONDS + 1);
        let outcome = claim(&pool, "p", task("file:core/a.rs"), &session("s2"), late).await.unwrap();
        let ClaimOutcome::Won { task, .. } = outcome else { panic!("a lapsed claim is free") };
        assert_eq!(task.holder().unwrap().id, "s2");
        let log = events(&pool, first.id).await;
        let last = log.last().unwrap();
        assert_eq!(last.0, "expired_reclaimed");
        assert_eq!(last.1.as_deref(), Some("s1"), "holder_* are the PREVIOUS owner");
        assert!(last.2.contains("session:s2"), "the note names the new one: {}", last.2);
    }

    #[tokio::test]
    async fn a_dead_run_claim_is_taken_over() {
        let pool = test_pool().await;
        let run_id = sqlx::query("INSERT INTO runs (prompt, status, created_at) VALUES ('x', 'completed', 't')")
            .execute(&pool).await.unwrap().last_insert_rowid();
        let run = Holder { kind: HolderKind::Run, id: run_id.to_string(), worktree: "/wt/run".into() };
        // `claim` does not ask whether the claimer itself is alive, so a run already finished can still
        // be the fixture's holder; the second claim then finds it dead.
        let ClaimOutcome::Won { task: first, .. } =
            claim(&pool, "p", task("file:core/a.rs"), &run, t0()).await.unwrap() else { panic!() };
        let outcome = claim(&pool, "p", task("file:core/a.rs"), &session("s2"), t0() + secs(1)).await.unwrap();
        assert!(outcome.is_won());
        let log = events(&pool, first.id).await;
        assert_eq!(log.last().unwrap().0, "taken_over");
        assert_eq!(log.last().unwrap().1.as_deref(), Some(run_id.to_string().as_str()));
    }

    #[tokio::test]
    async fn claim_by_id_takes_an_open_task_and_refuses_a_missing_or_closed_one() {
        let pool = test_pool().await;
        // A postgate claim is live only while the project is red.
        sqlx::query("INSERT INTO postgate_state (project_id, target, red_groups) VALUES ('p', 'main', '[\"core\"]')")
            .execute(&pool).await.unwrap();
        let id = seed(&pool, Seed { key: "master-red", status: "open", ..Seed::default() }).await;
        let postgate = Holder { kind: HolderKind::Postgate, id: "p".into(), worktree: "/wt/postgate".into() };
        assert!(claim_by_id(&pool, id, &postgate, t0()).await.unwrap().is_won());
        assert!(claim_by_id(&pool, id, &postgate, t0()).await.unwrap().is_won(), "the postgate repeats its claim");
        let ClaimOutcome::Lost { holder, .. } = claim_by_id(&pool, id, &session("s1"), t0()).await.unwrap()
            else { panic!("a session must lose to a live claim") };
        assert_eq!(holder.kind, HolderKind::Postgate);
        assert!(matches!(claim_by_id(&pool, 9999, &session("s1"), t0()).await, Err(sqlx::Error::RowNotFound)));
        sqlx::query("UPDATE task_claims SET status = 'done' WHERE id = ?").bind(id).execute(&pool).await.unwrap();
        assert!(matches!(claim_by_id(&pool, id, &session("s1"), t0()).await, Err(sqlx::Error::RowNotFound)));
    }

    #[tokio::test]
    async fn a_live_probe_blocks_a_claim_and_a_dead_one_is_replaced() {
        let pool = test_pool().await;
        seed(&pool, Seed {
            key: "master-red", status: "probing", holder: Some(("verify", "ticket-1")),
            lease: Some(t0() + secs(60)), ..Seed::default()
        }).await;
        let ClaimOutcome::Lost { holder, .. } =
            claim(&pool, "p", task("master-red"), &session("s1"), t0()).await.unwrap() else { panic!() };
        assert_eq!(holder.kind, HolderKind::Verify);
        let outcome = claim(&pool, "p", task("master-red"), &session("s1"), t0() + secs(61)).await.unwrap();
        assert!(outcome.is_won(), "a probe whose lease lapsed is a dead proof, not an incident");
        let probing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_claims WHERE status = 'probing'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(probing, 0);
    }

    #[tokio::test]
    async fn the_answer_lists_similar_live_tasks_by_file_and_by_overlapping_paths() {
        let pool = test_pool().await;
        claim(&pool, "p", task("test:core/a.rs::one"), &session("s1"), t0()).await.unwrap();
        let mut guarded = task("topic:flaky-ci");
        guarded.guard_paths = vec!["core/src".into()];
        claim(&pool, "p", guarded, &session("s1"), t0()).await.unwrap();

        let ClaimOutcome::Won { similar, .. } =
            claim(&pool, "p", task("test:core/a.rs::two"), &session("s2"), t0()).await.unwrap() else { panic!() };
        assert_eq!(similar.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), vec!["test:core/a.rs::one"]);

        let mut overlapping = task("file:core/src/b.rs");
        overlapping.hint_paths = vec!["core/src/b.rs".into()];
        let ClaimOutcome::Won { similar, .. } =
            claim(&pool, "p", overlapping, &session("s3"), t0()).await.unwrap() else { panic!() };
        assert_eq!(similar.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), vec!["topic:flaky-ci"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_connections_claiming_one_key_yield_exactly_one_winner() {
        let pool = crate::testdb::fresh_shared_pool().await;
        let mut handles = Vec::new();
        for who in ["s1", "s2", "s3", "s4"] {
            let pool = pool.clone();
            handles.push(tokio::spawn(async move {
                claim(&pool, "p", task("master-red"), &session(who), t0()).await.unwrap()
            }));
        }
        let mut winners = 0;
        for handle in handles {
            if handle.await.unwrap().is_won() {
                winners += 1;
            }
        }
        assert_eq!(winners, 1, "the compare-and-swap lets only one holder through");
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_claims").fetch_one(&pool).await.unwrap();
        assert_eq!(rows, 1);
    }

    #[tokio::test]
    async fn record_event_writes_one_row_in_its_own_transaction() {
        let pool = test_pool().await;
        let ClaimOutcome::Won { task, .. } =
            claim(&pool, "p", task("file:core/a.rs"), &session("s1"), t0()).await.unwrap() else { panic!() };
        record_event(&pool, task.id, "override_land", Some(&session("s1")), "landing over a red", t0() + secs(1))
            .await.unwrap();
        record_event(&pool, task.id, "note", None, "", t0() + secs(2)).await.unwrap();
        let log = events(&pool, task.id).await;
        assert_eq!(log.len(), 4, "created, claimed and the two new ones");
        assert_eq!(log[2].0, "override_land");
        assert_eq!(log[2].1.as_deref(), Some("s1"));
        assert_eq!(log[2].2, "landing over a red");
        assert_eq!(log[3].0, "note");
        assert_eq!(log[3].1, None, "an event about nobody keeps the holder columns empty");
    }

    #[tokio::test]
    async fn override_recorded_is_true_only_for_the_current_claim() {
        let pool = test_pool().await;
        let ClaimOutcome::Won { task: first, .. } =
            claim(&pool, "p", task("file:core/a.rs"), &session("s1"), t0()).await.unwrap() else { panic!() };
        assert!(!override_recorded(&pool, first.id, "").await.unwrap(), "no override yet");

        // Same microsecond as `claimed_at`: `at >= claimed_at` must still count it.
        record_event(&pool, first.id, "override_land", Some(&session("s1")), "why", t0()).await.unwrap();
        assert!(override_recorded(&pool, first.id, "").await.unwrap(), "same tick counts");
        assert!(override_recorded(&pool, first.id, "why").await.unwrap(), "the note matches");
        assert!(!override_recorded(&pool, first.id, "another reason").await.unwrap(), "a different note does not");

        // Another owner takes the task over: the old override belongs to the previous claim.
        let late = t0() + secs(SESSION_LEASE_SECONDS + 1);
        assert!(claim(&pool, "p", task("file:core/a.rs"), &session("s2"), late).await.unwrap().is_won());
        assert!(!override_recorded(&pool, first.id, "").await.unwrap(), "the old owner's override is stale");
        record_event(&pool, first.id, "override_land", Some(&session("s2")), "mine", late).await.unwrap();
        assert!(override_recorded(&pool, first.id, "").await.unwrap());

        // A released task has no current claim.
        sqlx::query("UPDATE task_claims SET status = 'open' WHERE id = ?").bind(first.id).execute(&pool).await.unwrap();
        assert!(!override_recorded(&pool, first.id, "").await.unwrap());
    }

    fn incident(detail: serde_json::Value, guard: &[&str], hint: &[&str]) -> NewTask {
        NewTask {
            key: TaskKey::master_red(),
            kind: TaskKind::Incident,
            title: "master red".into(),
            detail,
            guard_paths: guard.iter().map(|p| (*p).to_owned()).collect(),
            hint_paths: hint.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    #[test]
    fn detail_merges_shallowly_new_wins_and_nulls_never_erase() {
        let old = serde_json::json!({"red_since_sha": "a", "red_tests": ["t1"]});
        let new = serde_json::json!({"culprit_sha": "c", "red_tests": ["t1", "t2"], "red_since_sha": null});
        assert_eq!(
            merge_detail(&old, &new),
            serde_json::json!({"red_since_sha": "a", "culprit_sha": "c", "red_tests": ["t1", "t2"]})
        );
        assert_eq!(merge_detail(&old, &Value::Null), old);
    }

    #[test]
    fn path_lists_union_without_duplicates_keeping_order() {
        let a = vec!["x".to_owned(), "y".to_owned()];
        let b = vec!["y".to_owned(), "z".to_owned()];
        assert_eq!(union(&a, &b), vec!["x", "y", "z"]);
    }

    #[tokio::test]
    async fn an_incident_without_a_holder_is_created_open() {
        let pool = test_pool().await;
        let row = create_or_adopt_open(
            &pool, "p", incident(serde_json::json!({"red_since_sha": "a"}), &["core/a.rs"], &["core/b.rs"]), t0(),
        ).await.unwrap();
        assert_eq!((row.status, row.kind), (Status::Open, TaskKind::Incident));
        assert_eq!(row.holder(), None);
        assert_eq!(row.guard_paths, vec!["core/a.rs"]);
        assert_eq!(events(&pool, row.id).await.len(), 1);
    }

    #[tokio::test]
    async fn a_second_creator_adopts_the_live_row_and_adds_what_it_knows() {
        let pool = test_pool().await;
        let first = create_or_adopt_open(&pool, "p", incident(serde_json::json!({"red_since_sha": "a"}), &["core/a.rs"], &[]), t0())
            .await.unwrap();
        claim_by_id(&pool, first.id, &session("s1"), t0()).await.unwrap();
        let adopted = create_or_adopt_open(
            &pool, "p", incident(serde_json::json!({"culprit_sha": "c"}), &["core/b.rs"], &["core/c.rs"]), t0() + secs(5),
        ).await.unwrap();
        assert_eq!(adopted.id, first.id);
        assert_eq!(adopted.status, Status::Claimed, "adopting does not disturb the owner");
        assert_eq!(adopted.detail, serde_json::json!({"red_since_sha": "a", "culprit_sha": "c"}));
        assert_eq!(adopted.guard_paths, vec!["core/a.rs", "core/b.rs"]);
        assert_eq!(adopted.hint_paths, vec!["core/c.rs"]);
    }

    #[tokio::test]
    async fn adopting_a_probe_turns_it_into_a_plain_open_incident() {
        let pool = test_pool().await;
        let id = seed(&pool, Seed {
            key: "master-red", status: "probing", holder: Some(("verify", "t1")),
            lease: Some(t0() + secs(60)), ..Seed::default()
        }).await;
        let adopted = create_or_adopt_open(&pool, "p", incident(serde_json::json!({"red_since_sha": "a"}), &[], &[]), t0())
            .await.unwrap();
        assert_eq!((adopted.id, adopted.status), (id, Status::Open));
        assert_eq!((adopted.holder_kind, adopted.lease_until), (None, None));
    }

    #[tokio::test]
    async fn update_detail_merges_and_replaces_paths_only_when_given() {
        let pool = test_pool().await;
        let created = create_or_adopt_open(&pool, "p", incident(serde_json::json!({"red_since_sha": "a"}), &["g"], &["h"]), t0())
            .await.unwrap();
        update_detail(&pool, created.id, serde_json::json!({"culprit_sha": "c"}), None, Some(vec!["culprit.rs".into()]), t0())
            .await.unwrap();
        let after = row(&pool, created.id).await;
        assert_eq!(after.detail, serde_json::json!({"red_since_sha": "a", "culprit_sha": "c"}));
        assert_eq!(after.guard_paths, vec!["g"], "untouched when None");
        assert_eq!(after.hint_paths, vec!["culprit.rs"], "replaced when Some");
    }
}
