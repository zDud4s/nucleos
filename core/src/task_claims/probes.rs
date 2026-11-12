//! The master-red probes: the single-flight proof of a suspect group on the master's HEAD
//! (`begin_probe` / `finish_probe`) and of an open incident's red tests (`begin_reprobe` /
//! `finish_reprobe`). Split out of `store.rs`, whose helpers these use; `store` re-exports them.

use super::store::{
    begin, close_in_tx, delete_task, encode_list, is_live_on, merge_detail, project_is_red,
    record_event_on, row_by_id, union, unresolved_by_key,
};
use super::{Closer, FailedProbe, PROBE_LEASE_SECONDS, ProbeStart, Status, TaskKey, TaskRow};
use crate::execution::wave::stamp;
use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;

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
            let row = row_by_id(&mut tx, id)
                .await?
                .ok_or(sqlx::Error::RowNotFound)?;
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
                let row = row_by_id(&mut tx, row.id)
                    .await?
                    .ok_or(sqlx::Error::RowNotFound)?;
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
    let Some(row) = row_by_id(&mut tx, task_id).await? else {
        return Ok(None);
    };
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
                record_event_on(
                    &mut tx,
                    task_id,
                    "created",
                    None,
                    "proved failing on master",
                    now,
                )
                .await?;
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
pub async fn begin_reprobe(
    pool: &SqlitePool,
    task_id: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
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
/// event, exactly as the postgate's green does) -- unless the project's `postgate_state` is still
/// red: the postgate is still on this red (bisecting, reverting), and closing under it would make
/// its next `ensure_incident` open a second `master-red`. Then only the flight is released, and the
/// incident waits for the postgate's own green close (`incident::close_if_green`).
pub async fn finish_reprobe(
    pool: &SqlitePool,
    task_id: i64,
    master_sha: &str,
    still_red: bool,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let mut tx = begin(pool).await?;
    let postgate_red = match row_by_id(&mut tx, task_id).await? {
        Some(row) => project_is_red(&mut tx, &row.project_id).await?,
        None => false,
    };
    if !still_red && postgate_red {
        // `checked_sha` stays: the red tests were NOT proved failing at `master_sha`.
        sqlx::query("UPDATE task_claims SET probe_lease_until = NULL WHERE id = ?")
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
    } else if still_red {
        sqlx::query(
            "UPDATE task_claims SET checked_sha = ?, probe_lease_until = NULL WHERE id = ?",
        )
        .bind(master_sha)
        .bind(task_id)
        .execute(&mut *tx)
        .await?;
    } else {
        let sha = master_sha.to_owned();
        close_in_tx(
            &mut tx,
            task_id,
            &Closer::Green { sha },
            "re-proof passed",
            now,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
