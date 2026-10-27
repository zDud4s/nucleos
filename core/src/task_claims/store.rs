//! SQL, the compare-and-swap and `is_live` (spec task-claims section 1).
//!
//! Every function that decides who owns a task runs in `BEGIN IMMEDIATE`: the write lock is taken up
//! front, so two claimers cannot both read "free" and both write. Inside a transaction only `&mut tx`
//! is used -- never the pool. Every time goes through `execution::wave::stamp` and is compared as TEXT.
//!
//! **Expiry is lazy.** Nothing is rewritten when a lease lapses; `is_live` is the one helper every
//! consumer (the claim, `task_list`, the verify, the land gate, the postgate, the hooks) asks, so an
//! expired claim counts as free everywhere.

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
    refuse_declared_master_red(new.kind, &new.key)?;
    let mut tx = begin(pool).await?;
    let task_id = match unresolved_by_key(&mut tx, project_id, &new.key).await? {
        None => insert_open(&mut tx, project_id, &new, now).await?,
        Some(row) => {
            if row.status == Status::Probing && !is_live_on(&mut tx, &row, now).await? {
                delete_task(&mut tx, row.id).await?;
                insert_open(&mut tx, project_id, &new, now).await?
            } else {
                if row.status != Status::Probing {
                    // Adopting the row: what the claimer knows goes on top of it; the title stays.
                    let detail = merge_detail(&row.detail, &new.detail).to_string();
                    let guard = encode_list(&union(&row.guard_paths, &new.guard_paths));
                    let hint = encode_list(&union(&row.hint_paths, &new.hint_paths));
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
    let row = row_by_id(&mut tx, task_id).await?.ok_or(sqlx::Error::RowNotFound)?;
    refuse_declared_master_red(row.kind, &row.key)?;
    let outcome = cas(&mut tx, task_id, holder, now).await?;
    tx.commit().await?;
    Ok(outcome)
}

/// Only the daemon creates `master-red` (through `create_or_adopt_open` / `begin_probe`); an agent's
/// `declared` claim on it is refused. The store has no refusal shape, so this is a `Protocol` error.
fn refuse_declared_master_red(kind: TaskKind, key: &TaskKey) -> sqlx::Result<()> {
    if kind == TaskKind::Declared && key.as_str() == super::MASTER_RED_KEY {
        return Err(sqlx::Error::Protocol(
            "the master-red task is created by the daemon only; it cannot be declared".to_owned(),
        ));
    }
    Ok(())
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

// ---- writes: leaving a task -------------------------------------------------------------------------

/// The note of a closure by green: `green <sha>` or `green <sha>; <note>`. `take_notices` reads the sha
/// back, which is what tells a session its incident was closed from outside from its own `task_done`.
pub fn green_note(sha: &str, note: &str) -> String {
    if note.is_empty() {
        format!("green {sha}")
    } else {
        format!("green {sha}; {note}")
    }
}

pub fn green_sha(note: &str) -> Option<String> {
    let rest = note.strip_prefix("green ")?;
    Some(rest.split(';').next()?.trim().to_owned())
}

/// `claimed` back to `open`: the holder columns go, and the event keeps who held it.
async fn reopen(
    conn: &mut SqliteConnection,
    row: &TaskRow,
    event: &str,
    note: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE task_claims
         SET status = 'open', holder_kind = NULL, holder_id = NULL, holder_worktree = NULL, lease_until = NULL
         WHERE id = ? AND status = 'claimed'",
    )
    .bind(row.id)
    .execute(&mut *conn)
    .await?;
    record_event_on(conn, row.id, event, row.holder().as_ref(), note, now).await
}

/// The owner gives the task back. `false` when `holder` does not hold it (a lapsed owner nobody
/// displaced still does).
pub async fn release(
    pool: &SqlitePool,
    task_id: i64,
    holder: &Holder,
    note: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let mut tx = begin(pool).await?;
    let Some(row) = row_by_id(&mut tx, task_id).await? else { return Ok(false) };
    let owns = row.status == Status::Claimed && row.holder().is_some_and(|current| current.same_owner(holder));
    if !owns {
        return Ok(false);
    }
    reopen(&mut tx, &row, "released", note, now).await?;
    tx.commit().await?;
    Ok(true)
}

/// Closes an `open` or `claimed` task. `Closer::Holder` needs the caller to be the owner; `Green` and
/// `Reconcile` close whatever the state. The event stores the PREVIOUS owner in `holder_*`.
pub async fn done(
    pool: &SqlitePool,
    task_id: i64,
    closer: Closer<'_>,
    note: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let mut tx = begin(pool).await?;
    let closed = close_in_tx(&mut tx, task_id, &closer, note, now).await?;
    if closed {
        tx.commit().await?;
    }
    Ok(closed)
}

async fn close_in_tx(
    conn: &mut SqliteConnection,
    task_id: i64,
    closer: &Closer<'_>,
    note: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let Some(row) = row_by_id(conn, task_id).await? else { return Ok(false) };
    if !matches!(row.status, Status::Open | Status::Claimed) {
        return Ok(false);
    }
    let previous = row.holder();
    let event_note = match closer {
        Closer::Holder(owner) => {
            let owns = row.status == Status::Claimed
                && previous.as_ref().is_some_and(|current| current.same_owner(owner));
            if !owns {
                return Ok(false);
            }
            note.to_owned()
        }
        Closer::Green { sha } => green_note(sha, note),
        Closer::Reconcile => format!("reconciled: {note}"),
    };
    // A closure by green records the sha it was proved on: `finish_reprobe(false)` closes through here,
    // and the row must say which master sha showed the failure gone. Other closers keep `checked_sha`.
    let closing_sha = match closer {
        Closer::Green { sha } => Some(sha.as_str()),
        _ => None,
    };
    sqlx::query(
        "UPDATE task_claims
         SET status = 'done', lease_until = NULL, probe_lease_until = NULL, closed_at = ?, close_note = ?,
             checked_sha = COALESCE(?, checked_sha)
         WHERE id = ? AND status IN ('open', 'claimed')",
    )
    .bind(stamp(now))
    .bind(&event_note)
    .bind(closing_sha)
    .bind(task_id)
    .execute(&mut *conn)
    .await?;
    record_event_on(conn, task_id, "done", previous.as_ref(), &event_note, now).await?;
    Ok(true)
}

/// A run or job ended: every claim it held goes back to `open` (event `released`). Returns how many.
pub async fn release_for_owner(
    pool: &SqlitePool,
    kind: HolderKind,
    id: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<u64> {
    let mut tx = begin(pool).await?;
    let raws = sqlx::query_as::<_, RawRow>(
        "SELECT * FROM task_claims WHERE status = 'claimed' AND holder_kind = ? AND holder_id = ?",
    )
    .bind(kind.as_str())
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let mut released = 0;
    for raw in raws {
        let row = decode(raw)?;
        reopen(&mut tx, &row, "released", "owner finished without task_done", now).await?;
        released += 1;
    }
    tx.commit().await?;
    Ok(released)
}

// ---- writes: master-red probes ----------------------------------------------------------------------

/// A verify found a suspect group and asks to prove it on the master's HEAD. At most one `probing`
/// row per project (the unique index); a lapsed one is taken over and the proof repeats.
///
/// The row is born `probing`, held by the verify ticket on a 10-minute lease, and records no event: it
/// is not a task until the proof fails on master (`finish_probe`).
pub async fn begin_probe(
    pool: &SqlitePool,
    project_id: &str,
    ticket_id: &str,
    worktree: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<ProbeStart> {
    let mut tx = begin(pool).await?;
    let lease = stamp(now + Duration::seconds(PROBE_LEASE_SECONDS));
    let key = TaskKey::master_red();
    let result = match unresolved_by_key(&mut tx, project_id, &key).await? {
        None => {
            let id = sqlx::query(
                "INSERT INTO task_claims (project_id, task_key, kind, title, status, holder_kind, holder_id,
                                          holder_worktree, lease_until, created_at)
                 VALUES (?, ?, 'incident', 'master red (probing)', 'probing', 'verify', ?, ?, ?, ?)",
            )
            .bind(project_id)
            .bind(key.as_str())
            .bind(ticket_id)
            .bind(worktree)
            .bind(&lease)
            .bind(stamp(now))
            .execute(&mut *tx)
            .await?
            .last_insert_rowid();
            let row = row_by_id(&mut tx, id).await?.ok_or(sqlx::Error::RowNotFound)?;
            ProbeStart::Started(row)
        }
        Some(row) if row.status == Status::Probing => {
            // Only a LAPSED probe is retaken, whoever held it (the same ticket included). A live one is
            // in flight even for its own ticket: a retry must not start a second proof beside the first.
            if !is_live_on(&mut tx, &row, now).await? {
                sqlx::query(
                    "UPDATE task_claims SET holder_id = ?, holder_worktree = ?, lease_until = ? WHERE id = ?",
                )
                .bind(ticket_id)
                .bind(worktree)
                .bind(&lease)
                .bind(row.id)
                .execute(&mut *tx)
                .await?;
                let row = row_by_id(&mut tx, row.id).await?.ok_or(sqlx::Error::RowNotFound)?;
                ProbeStart::Started(row)
            } else {
                ProbeStart::InFlight(row)
            }
        }
        Some(row) => ProbeStart::Exists(row),
    };
    tx.commit().await?;
    Ok(result)
}

/// The proof ended. `Some(failed)`: the groups fail on master too, so the row becomes an `open`
/// incident (`red_groups`, `red_tests`, `red_sha` and `red_since_sha` merged into the detail, the last
/// two both the master sha the proof ran on, i.e. the postgate's own keys; `checked_sha` set, the guard
/// paths added). `None`: they pass there, so the row is deleted and the failure is the task's.
///
/// `ticket_id` is the ticket that holds the probe. Returns the row after the call:
/// - `Some(open | claimed)`: an incident (made now, or adopted by the postgate meanwhile, in which case
///   it is left alone -- a "passes on master" must not delete the postgate's incident).
/// - `None`: the probe row is gone (deleted by this call, or already gone before it).
/// - `Some(probing)`: the row is another ticket's proof (this ticket's lease lapsed and the proof was
///   retaken). It is returned untouched and nothing is written; the caller must conclude NOTHING from
///   its own result (neither "the failure is the task's" nor "master is red").
pub async fn finish_probe(
    pool: &SqlitePool,
    task_id: i64,
    ticket_id: &str,
    failed_on_master: Option<FailedProbe>,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<TaskRow>> {
    let mut tx = begin(pool).await?;
    let Some(row) = row_by_id(&mut tx, task_id).await? else { return Ok(None) };
    if row.status == Status::Probing && row.holder_id.as_deref() != Some(ticket_id) {
        tx.commit().await?;
        return Ok(Some(row));
    }
    if row.status == Status::Probing {
        match failed_on_master {
            Some(failed) => {
                let proved = serde_json::json!({
                    "red_groups": failed.red_groups,
                    "red_tests": failed.red_tests,
                    "red_sha": failed.master_sha,
                    "red_since_sha": failed.master_sha,
                });
                sqlx::query(
                    "UPDATE task_claims
                     SET status = 'open', holder_kind = NULL, holder_id = NULL, holder_worktree = NULL,
                         lease_until = NULL, title = 'master red', detail = ?, guard_paths = ?, checked_sha = ?
                     WHERE id = ? AND status = 'probing'",
                )
                .bind(merge_detail(&row.detail, &proved).to_string())
                .bind(encode_list(&union(&row.guard_paths, &failed.guard_paths)))
                .bind(&failed.master_sha)
                .bind(task_id)
                .execute(&mut *tx)
                .await?;
                record_event_on(&mut tx, task_id, "created", None, "proved failing on master", now).await?;
            }
            None => delete_task(&mut tx, task_id).await?,
        }
    }
    let after = row_by_id(&mut tx, task_id)
        .await?
        .filter(|row| matches!(row.status, Status::Open | Status::Claimed));
    tx.commit().await?;
    Ok(after)
}

/// Takes the single flight of the master-red re-proof: `true` when the caller may run it. The flight
/// lapses after `PROBE_LEASE_SECONDS`, so a crashed re-proof does not wedge the incident.
pub async fn begin_reprobe(pool: &SqlitePool, task_id: i64, now: DateTime<Utc>) -> sqlx::Result<bool> {
    let taken = sqlx::query(
        "UPDATE task_claims SET probe_lease_until = ?
         WHERE id = ? AND status IN ('open', 'claimed')
           AND (probe_lease_until IS NULL OR probe_lease_until < ?)",
    )
    .bind(stamp(now + Duration::seconds(PROBE_LEASE_SECONDS)))
    .bind(task_id)
    .bind(stamp(now))
    .execute(pool)
    .await?;
    Ok(taken.rows_affected() == 1)
}

/// The re-proof ran the `red_tests` on `master_sha`. Still failing: `checked_sha` moves to it and the
/// flight is released. Passing: the incident closes by green at that sha (previous owner kept in the
/// event, exactly as the postgate's green does).
pub async fn finish_reprobe(
    pool: &SqlitePool,
    task_id: i64,
    master_sha: &str,
    still_red: bool,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let mut tx = begin(pool).await?;
    if still_red {
        sqlx::query("UPDATE task_claims SET checked_sha = ?, probe_lease_until = NULL WHERE id = ?")
            .bind(master_sha)
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
    } else {
        let sha = master_sha.to_owned();
        close_in_tx(&mut tx, task_id, &Closer::Green { sha }, "re-proof passed", now).await?;
    }
    tx.commit().await?;
    Ok(())
}

// ---- startup ----------------------------------------------------------------------------------------

/// The daemon restarted. Must run AFTER the run and job reconciliations: a claim held by a run or job
/// is judged by whether that run or job is live, and before them every dead one still reads live.
///
/// - `probing` rows are deleted: the proof died with the daemon.
/// - `claimed` by a dead run or job: back to `open` (event `released`).
/// - `claimed` by the postgate while the project is no longer red: closed `done`.
/// - Session leases are not touched: they lapse lazily, like everywhere else.
pub async fn reconcile_at_startup(pool: &SqlitePool, now: DateTime<Utc>) -> sqlx::Result<()> {
    let mut tx = begin(pool).await?;
    let probes: Vec<i64> = sqlx::query_scalar("SELECT id FROM task_claims WHERE status = 'probing'")
        .fetch_all(&mut *tx)
        .await?;
    for id in probes {
        delete_task(&mut tx, id).await?;
    }
    let raws = sqlx::query_as::<_, RawRow>(
        "SELECT * FROM task_claims WHERE status = 'claimed' AND holder_kind IN ('run', 'job', 'postgate') ORDER BY id",
    )
    .fetch_all(&mut *tx)
    .await?;
    for raw in raws {
        let row = decode(raw)?;
        if is_live_on(&mut tx, &row, now).await? {
            continue;
        }
        if row.holder_kind == Some(HolderKind::Postgate) {
            close_in_tx(&mut tx, row.id, &Closer::Reconcile, "the master is no longer red", now).await?;
        } else {
            reopen(&mut tx, &row, "released", "owner finished before the daemon restarted", now).await?;
        }
    }
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
        // `master-red` is the daemon's alone: a declared claim on it is refused, so its fixtures are incidents.
        let kind = if key == super::super::MASTER_RED_KEY { TaskKind::Incident } else { TaskKind::Declared };
        NewTask {
            key: super::super::keys::normalize(key, std::path::Path::new("/repo")).unwrap(),
            kind,
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

    #[tokio::test]
    async fn startup_frees_dead_runs_closes_stale_postgate_claims_and_drops_probes() {
        let pool = test_pool().await;
        let dead_run = sqlx::query("INSERT INTO runs (prompt, status, created_at) VALUES ('x', 'interrupted', 't')")
            .execute(&pool).await.unwrap().last_insert_rowid();
        let live_run = sqlx::query("INSERT INTO runs (prompt, status, created_at) VALUES ('x', 'running', 't')")
            .execute(&pool).await.unwrap().last_insert_rowid();
        sqlx::query("INSERT INTO postgate_state (project_id, target, red_groups) VALUES ('p', 'main', '[]')")
            .execute(&pool).await.unwrap();

        let probe = seed(&pool, Seed { key: "master-red", status: "probing", holder: Some(("verify", "t")), ..Seed::default() }).await;
        let dead = seed(&pool, Seed { key: "file:dead.rs", status: "claimed", holder: Some(("run", &dead_run.to_string())), ..Seed::default() }).await;
        let alive = seed(&pool, Seed { key: "file:alive.rs", status: "claimed", holder: Some(("run", &live_run.to_string())), ..Seed::default() }).await;
        let stale_postgate = seed(&pool, Seed { key: "file:pg.rs", status: "claimed", holder: Some(("postgate", "p")), ..Seed::default() }).await;
        let session_claim = seed(&pool, Seed {
            key: "file:s.rs", status: "claimed", holder: Some(("session", "s1")), lease: Some(t0() - secs(10_000)), ..Seed::default()
        }).await;

        reconcile_at_startup(&pool, t0()).await.unwrap();

        let gone: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_claims WHERE id = ?")
            .bind(probe).fetch_one(&pool).await.unwrap();
        assert_eq!(gone, 0, "the proof died with the daemon");
        assert_eq!(row(&pool, dead).await.status, Status::Open);
        assert_eq!(events(&pool, dead).await.last().unwrap().0, "released");
        assert_eq!(row(&pool, alive).await.status, Status::Claimed);
        let closed = row(&pool, stale_postgate).await;
        assert_eq!(closed.status, Status::Done, "the master is green again");
        assert_eq!(events(&pool, stale_postgate).await.last().unwrap().1.as_deref(), Some("p"));
        assert_eq!(row(&pool, session_claim).await.status, Status::Claimed, "leases lapse lazily; the sweep does not touch them");
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
    async fn claiming_an_open_incident_adds_the_claimers_guard_paths_and_keeps_the_title() {
        let pool = test_pool().await;
        let mut incident = task("topic:flaky-ci");
        incident.kind = TaskKind::Incident;
        incident.title = "original".into();
        create_or_adopt_open(&pool, "p", incident, t0()).await.unwrap();

        let mut mine = task("topic:flaky-ci");
        mine.title = "mine".into();
        mine.guard_paths = vec!["core/src".into()];
        mine.hint_paths = vec!["core/src/a.rs".into()];
        mine.detail = serde_json::json!({"note": "seen"});
        let ClaimOutcome::Won { task: won, .. } = claim(&pool, "p", mine, &session("s1"), t0()).await.unwrap()
            else { panic!("expected a win") };
        assert_eq!(won.title, "original");
        assert_eq!(won.guard_paths, vec!["core/src".to_owned()]);
        assert_eq!(won.hint_paths, vec!["core/src/a.rs".to_owned()]);
        assert_eq!(won.detail["note"], "seen");

        let changed = vec!["core/src/x.rs".to_owned()];
        let guarded = guarding(&pool, "p", &changed, t0()).await.unwrap();
        assert_eq!(guarded.len(), 1, "the claimed incident now guards the claimer's paths");
        assert_eq!(guarded[0].id, won.id);
    }

    #[tokio::test]
    async fn a_declared_claim_on_master_red_is_refused_and_leaves_no_row() {
        let pool = test_pool().await;
        let mut declared = task("master-red");
        declared.kind = TaskKind::Declared;
        let refused = claim(&pool, "p", declared, &session("s1"), t0()).await;
        assert!(matches!(refused, Err(sqlx::Error::Protocol(_))), "a declared master-red claim is refused");
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_claims").fetch_one(&pool).await.unwrap();
        assert_eq!(rows, 0);

        let id = seed(&pool, Seed { key: "master-red", status: "open", ..Seed::default() }).await;
        sqlx::query("UPDATE task_claims SET kind = 'declared' WHERE id = ?").bind(id).execute(&pool).await.unwrap();
        let by_id = claim_by_id(&pool, id, &session("s1"), t0()).await;
        assert!(matches!(by_id, Err(sqlx::Error::Protocol(_))), "claim_by_id refuses it too");
        assert_eq!(row(&pool, id).await.status, Status::Open, "the refused claim changed nothing");
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

    #[test]
    fn a_green_note_round_trips_its_sha() {
        assert_eq!(green_note("abc", ""), "green abc");
        assert_eq!(green_sha(&green_note("abc", "reverted")), Some("abc".to_owned()));
        assert_eq!(green_sha("released by s1"), None);
    }

    async fn claimed_by(pool: &SqlitePool, key: &str, who: &Holder) -> i64 {
        match claim(pool, "p", task(key), who, t0()).await.unwrap() {
            ClaimOutcome::Won { task, .. } => task.id,
            ClaimOutcome::Lost { .. } => panic!("the fixture claim must win"),
        }
    }

    #[tokio::test]
    async fn the_owner_releases_and_the_task_goes_back_to_open_with_the_owner_in_the_event() {
        let pool = test_pool().await;
        let owner = session("s1");
        let id = claimed_by(&pool, "file:core/a.rs", &owner).await;
        assert!(!release(&pool, id, &session("s2"), "not mine", t0()).await.unwrap(), "only the owner releases");
        assert!(release(&pool, id, &owner, "gave up", t0() + secs(1)).await.unwrap());
        let after = row(&pool, id).await;
        assert_eq!((after.status, after.holder_kind, after.lease_until), (Status::Open, None, None));
        let log = events(&pool, id).await;
        assert_eq!(log.last().unwrap(), &("released".to_owned(), Some("s1".to_owned()), "gave up".to_owned()));
        assert!(!release(&pool, id, &owner, "again", t0()).await.unwrap(), "an open task has no owner to release");
    }

    #[tokio::test]
    async fn done_by_the_owner_closes_and_by_anyone_else_does_nothing() {
        let pool = test_pool().await;
        let owner = session("s1");
        let id = claimed_by(&pool, "file:core/a.rs", &owner).await;
        let other = session("s2");
        assert!(!done(&pool, id, Closer::Holder(&other), "x", t0()).await.unwrap());
        assert!(done(&pool, id, Closer::Holder(&owner), "fixed", t0() + secs(5)).await.unwrap());
        let after = row(&pool, id).await;
        assert_eq!(after.status, Status::Done);
        assert_eq!((after.closed_at, after.close_note), (Some(stamp(t0() + secs(5))), Some("fixed".into())));
        assert_eq!(after.lease_until, None);
        assert!(!done(&pool, id, Closer::Holder(&owner), "twice", t0()).await.unwrap(), "closing is not repeatable");
    }

    #[tokio::test]
    async fn a_lapsed_owner_can_still_report_done_if_nobody_took_over() {
        let pool = test_pool().await;
        let owner = session("s1");
        let id = claimed_by(&pool, "file:core/a.rs", &owner).await;
        let much_later = t0() + secs(SESSION_LEASE_SECONDS * 4);
        assert!(done(&pool, id, Closer::Holder(&owner), "finished after a long pause", much_later).await.unwrap());
    }

    #[tokio::test]
    async fn closing_by_green_keeps_the_previous_owner_and_says_which_sha() {
        let pool = test_pool().await;
        let owner = session("s1");
        let id = claimed_by(&pool, "master-red", &owner).await;
        assert!(done(&pool, id, Closer::Green { sha: "abc123".into() }, "revert landed", t0() + secs(9)).await.unwrap());
        let log = events(&pool, id).await;
        let last = log.last().unwrap();
        assert_eq!((last.0.as_str(), last.1.as_deref()), ("done", Some("s1")));
        assert_eq!(green_sha(&last.2), Some("abc123".to_owned()));
        let after = row(&pool, id).await;
        assert_eq!(after.status, Status::Done);
        assert_eq!(after.checked_sha.as_deref(), Some("abc123"), "a green closure records the sha it was proved on");
    }

    #[tokio::test]
    async fn only_a_green_closure_touches_checked_sha() {
        let pool = test_pool().await;
        let owner = session("s1");
        let id = claimed_by(&pool, "master-red", &owner).await;
        sqlx::query("UPDATE task_claims SET checked_sha = 'm1' WHERE id = ?").bind(id).execute(&pool).await.unwrap();
        assert!(done(&pool, id, Closer::Holder(&owner), "fixed by hand", t0()).await.unwrap());
        assert_eq!(row(&pool, id).await.checked_sha.as_deref(), Some("m1"), "the owner's closure leaves it alone");
    }

    #[tokio::test]
    async fn green_also_closes_a_task_nobody_held() {
        let pool = test_pool().await;
        let id = seed(&pool, Seed { key: "master-red", status: "open", ..Seed::default() }).await;
        assert!(done(&pool, id, Closer::Green { sha: "abc".into() }, "", t0()).await.unwrap());
        let log = events(&pool, id).await;
        assert_eq!(log.last().unwrap().1, None, "no previous owner to record");
        let probing = seed(&pool, Seed { key: "file:p.rs", status: "probing", holder: Some(("verify", "t")), ..Seed::default() }).await;
        assert!(!done(&pool, probing, Closer::Green { sha: "abc".into() }, "", t0()).await.unwrap(), "a probe is not a task yet");
    }

    #[tokio::test]
    async fn release_for_owner_reopens_only_that_owners_claims() {
        let pool = test_pool().await;
        let run = Holder { kind: HolderKind::Run, id: "7".into(), worktree: "/wt/run".into() };
        let mine = claimed_by(&pool, "file:a.rs", &run).await;
        let also_mine = claimed_by(&pool, "file:b.rs", &run).await;
        let other_run = Holder { id: "8".into(), ..run.clone() };
        let theirs = claimed_by(&pool, "file:c.rs", &other_run).await;
        let session_claim = claimed_by(&pool, "file:d.rs", &session("7")).await;

        assert_eq!(release_for_owner(&pool, HolderKind::Run, "7", t0()).await.unwrap(), 2);
        for id in [mine, also_mine] {
            assert_eq!(row(&pool, id).await.status, Status::Open);
            assert_eq!(events(&pool, id).await.last().unwrap().0, "released");
        }
        assert_eq!(row(&pool, theirs).await.status, Status::Claimed);
        assert_eq!(row(&pool, session_claim).await.status, Status::Claimed, "a session '7' is not run '7'");
        assert_eq!(release_for_owner(&pool, HolderKind::Run, "7", t0()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn the_first_verify_starts_the_probe_and_a_second_finds_it_in_flight() {
        let pool = test_pool().await;
        let ProbeStart::Started(row) = begin_probe(&pool, "p", "ticket-1", "/probe", t0()).await.unwrap()
            else { panic!("the first verify owns the proof") };
        assert_eq!(row.status, Status::Probing);
        assert_eq!(row.key, TaskKey::master_red());
        assert_eq!(row.holder().unwrap().kind, HolderKind::Verify);
        assert_eq!(row.lease_until, Some(stamp(t0() + secs(PROBE_LEASE_SECONDS))));

        assert!(matches!(
            begin_probe(&pool, "p", "ticket-2", "/probe", t0() + secs(5)).await.unwrap(),
            ProbeStart::InFlight(_)
        ));
        assert!(
            matches!(begin_probe(&pool, "p", "ticket-1", "/probe", t0() + secs(5)).await.unwrap(), ProbeStart::InFlight(_)),
            "the same ticket on its own LIVE probe is in flight too: it must not start a second proof"
        );
        assert!(matches!(
            begin_probe(&pool, "other", "ticket-3", "/probe", t0()).await.unwrap(),
            ProbeStart::Started(_)
        ), "another project proves on its own");
    }

    #[tokio::test]
    async fn a_lapsed_probe_is_taken_over_by_the_next_verify() {
        let pool = test_pool().await;
        begin_probe(&pool, "p", "ticket-1", "/probe", t0()).await.unwrap();
        let late = t0() + secs(PROBE_LEASE_SECONDS + 1);
        let ProbeStart::Started(row) = begin_probe(&pool, "p", "ticket-2", "/probe", late).await.unwrap()
            else { panic!("a crashed proof is retaken") };
        assert_eq!(row.holder_id.as_deref(), Some("ticket-2"));
        assert_eq!(row.lease_until, Some(stamp(late + secs(PROBE_LEASE_SECONDS))));
    }

    #[tokio::test]
    async fn the_same_ticket_restarts_its_own_probe_once_the_lease_lapsed() {
        let pool = test_pool().await;
        begin_probe(&pool, "p", "ticket-1", "/probe", t0()).await.unwrap();
        let late = t0() + secs(PROBE_LEASE_SECONDS + 1);
        assert!(matches!(begin_probe(&pool, "p", "ticket-1", "/probe", late).await.unwrap(), ProbeStart::Started(_)));
    }

    #[tokio::test]
    async fn an_incident_already_open_or_claimed_needs_no_probe() {
        let pool = test_pool().await;
        seed(&pool, Seed { key: "master-red", status: "open", ..Seed::default() }).await;
        assert!(matches!(begin_probe(&pool, "p", "t", "/probe", t0()).await.unwrap(), ProbeStart::Exists(_)));
    }

    fn failed(sha: &str) -> FailedProbe {
        FailedProbe {
            master_sha: sha.into(),
            red_groups: vec!["core".into()],
            red_tests: vec!["core/a.rs::it".into()],
            guard_paths: vec!["core/a.rs".into()],
        }
    }

    #[tokio::test]
    async fn a_failed_probe_opens_the_incident_with_what_it_proved() {
        let pool = test_pool().await;
        let ProbeStart::Started(probing) = begin_probe(&pool, "p", "t1", "/probe", t0()).await.unwrap() else { panic!() };
        let opened = finish_probe(&pool, probing.id, "t1", Some(failed("m1")), t0() + secs(30)).await.unwrap().unwrap();
        assert_eq!((opened.id, opened.status), (probing.id, Status::Open));
        assert_eq!((opened.holder_kind, opened.lease_until), (None, None));
        assert_eq!(opened.checked_sha.as_deref(), Some("m1"));
        assert_eq!(opened.guard_paths, vec!["core/a.rs"]);
        assert_eq!(opened.detail["red_tests"], serde_json::json!(["core/a.rs::it"]));
        // The same keys as the postgate's incident, so verify case 1 can test ancestry against
        // `red_since_sha`; both are the master sha the proof ran on.
        assert_eq!(opened.detail["red_sha"], "m1");
        assert_eq!(opened.detail["red_since_sha"], "m1");
        assert_eq!(opened.detail["red_groups"], serde_json::json!(["core"]));
        assert!(opened.detail.get("proved_sha").is_none(), "no key of its own: it is red_sha / red_since_sha");
        assert_eq!(events(&pool, probing.id).await.len(), 1, "created, when it became an incident");
    }

    #[tokio::test]
    async fn a_probe_that_passes_on_master_deletes_the_row() {
        let pool = test_pool().await;
        let ProbeStart::Started(probing) = begin_probe(&pool, "p", "t1", "/probe", t0()).await.unwrap() else { panic!() };
        assert!(finish_probe(&pool, probing.id, "t1", None, t0()).await.unwrap().is_none());
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_claims").fetch_one(&pool).await.unwrap();
        assert_eq!(left, 0);
        // And the slot is free: the next verify can start a new proof.
        assert!(matches!(begin_probe(&pool, "p", "t2", "/probe", t0()).await.unwrap(), ProbeStart::Started(_)));
    }

    #[tokio::test]
    async fn finishing_a_probe_the_postgate_already_adopted_leaves_the_incident_alone() {
        let pool = test_pool().await;
        let ProbeStart::Started(probing) = begin_probe(&pool, "p", "t1", "/probe", t0()).await.unwrap() else { panic!() };
        create_or_adopt_open(&pool, "p", incident(serde_json::json!({"red_since_sha": "a"}), &[], &[]), t0() + secs(1))
            .await.unwrap();
        let kept = finish_probe(&pool, probing.id, "t1", None, t0() + secs(2)).await.unwrap();
        assert_eq!(kept.map(|r| r.status), Some(Status::Open), "a 'passes on master' must not delete the postgate's incident");
    }

    #[tokio::test]
    async fn a_stale_ticket_cannot_finish_or_delete_another_tickets_live_probe() {
        let pool = test_pool().await;
        let ProbeStart::Started(first) = begin_probe(&pool, "p", "t1", "/probe", t0()).await.unwrap() else { panic!() };
        let late = t0() + secs(PROBE_LEASE_SECONDS + 1);
        let ProbeStart::Started(second) = begin_probe(&pool, "p", "t2", "/probe", late).await.unwrap() else { panic!() };
        assert_eq!(first.id, second.id, "the lapsed row is retaken, not duplicated");

        // t1 wakes up late and reports "passes on master": t2's proof must survive.
        let theirs = finish_probe(&pool, first.id, "t1", None, late).await.unwrap().unwrap();
        assert_eq!((theirs.status, theirs.holder_id.as_deref()), (Status::Probing, Some("t2")), "returned untouched, still probing");
        // Nor can t1 turn it into an incident with its own result.
        let theirs = finish_probe(&pool, first.id, "t1", Some(failed("m1")), late).await.unwrap().unwrap();
        assert_eq!(theirs.status, Status::Probing);
        let left = row(&pool, first.id).await;
        assert_eq!((left.status, left.checked_sha, left.detail), (Status::Probing, None, serde_json::json!({})));
        assert!(events(&pool, first.id).await.is_empty(), "no event for a probe that is not a task yet");

        // The owner of the row finishes normally.
        assert!(finish_probe(&pool, first.id, "t2", None, late).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_reprobe_is_single_flight() {
        let pool = test_pool().await;
        let id = seed(&pool, Seed { key: "master-red", status: "open", ..Seed::default() }).await;
        assert!(begin_reprobe(&pool, id, t0()).await.unwrap());
        assert!(!begin_reprobe(&pool, id, t0() + secs(5)).await.unwrap(), "one re-proof at a time");
        assert!(begin_reprobe(&pool, id, t0() + secs(PROBE_LEASE_SECONDS + 1)).await.unwrap(), "a crashed one lapses");
        let closed = seed(&pool, Seed { key: "file:x.rs", status: "done", ..Seed::default() }).await;
        assert!(!begin_reprobe(&pool, closed, t0()).await.unwrap(), "only unresolved incidents are re-proved");
    }

    #[tokio::test]
    async fn a_reprobe_still_red_moves_checked_sha_and_releases_the_flight() {
        let pool = test_pool().await;
        let id = seed(&pool, Seed { key: "master-red", status: "open", ..Seed::default() }).await;
        begin_reprobe(&pool, id, t0()).await.unwrap();
        finish_reprobe(&pool, id, "m2", true, t0() + secs(30)).await.unwrap();
        let after = row(&pool, id).await;
        assert_eq!((after.status, after.checked_sha.as_deref(), after.probe_lease_until), (Status::Open, Some("m2"), None));
        assert!(begin_reprobe(&pool, id, t0() + secs(31)).await.unwrap(), "the flight is free again");
    }

    #[tokio::test]
    async fn a_reprobe_that_passes_closes_the_incident_by_green() {
        let pool = test_pool().await;
        let owner = session("s1");
        let id = claimed_by(&pool, "master-red", &owner).await;
        begin_reprobe(&pool, id, t0()).await.unwrap();
        finish_reprobe(&pool, id, "m3", false, t0() + secs(30)).await.unwrap();
        let after = row(&pool, id).await;
        assert_eq!(after.status, Status::Done);
        let last = events(&pool, id).await.pop().unwrap();
        assert_eq!((last.0.as_str(), last.1.as_deref()), ("done", Some("s1")));
        assert_eq!(green_sha(&last.2), Some("m3".to_owned()));
    }
}
