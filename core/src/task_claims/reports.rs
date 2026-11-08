//! Reports on a ticket (spec 2026-11-06-tickets-globais): a problem is reported first and the same key
//! adds one more report to the same ticket. `store` re-exports [`report`] so callers keep one path.

use super::store::{
    begin, encode_list, insert_open, is_declared_machine, machine_refusal, merge_detail,
    record_event_on, row_by_id, similar_to, union, unresolved_by_key,
};
use super::{Holder, HolderKind, NewTask, ReportOutcome, Reporter, Status};
use crate::execution::wave::stamp;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};

/// Reports kept per ticket; the oldest beyond this are deleted and counted in `detail.reports_dropped`.
const REPORTS_KEPT: i64 = 50;

/// The `reporter_kind` stored for a holder: the daemon-side holders (`postgate`, `verify`) are `daemon`.
fn reporter_kind_of(holder: &Holder) -> &'static str {
    match holder.kind {
        HolderKind::Session => "session",
        HolderKind::Run => "run",
        HolderKind::Job => "job",
        HolderKind::Postgate | HolderKind::Verify => "daemon",
    }
}

/// Reports a problem WITHOUT claiming it, in one `BEGIN IMMEDIATE` transaction. The same key adds one
/// more report to the live ticket (idempotent: never a second ticket), whoever owns it. The report's
/// paths go to `hint_paths` only: a report never guards, so `new.guard_paths` is ignored. A `declared`
/// report that would CREATE `master-red` is refused, as for a claim.
#[allow(clippy::too_many_arguments)]
pub async fn report(
    pool: &SqlitePool,
    project_id: &str,
    new: NewTask,
    reporter: &Reporter<'_>,
    detail: &str,
    tests: &[String],
    blocking: bool,
    linked_to: Option<i64>,
    now: DateTime<Utc>,
) -> sqlx::Result<ReportOutcome> {
    let mut tx = begin(pool).await?;
    if let Some(target) = linked_to {
        let same_project = row_by_id(&mut tx, target)
            .await?
            .is_some_and(|row| row.project_id == project_id);
        if !same_project {
            return Err(sqlx::Error::Protocol(
                "linked_to must name a ticket of this project".to_owned(),
            ));
        }
    }
    let existing = unresolved_by_key(&mut tx, project_id, &new.key).await?;
    if is_declared_machine(new.kind, &new.key)
        && !existing
            .as_ref()
            .is_some_and(|row| matches!(row.status, Status::Open | Status::Claimed))
    {
        return Err(machine_refusal());
    }
    let (task_id, created) = match existing {
        None => {
            let reported = NewTask {
                guard_paths: Vec::new(),
                ..new.clone()
            };
            (
                insert_open(&mut tx, project_id, &reported, now).await?,
                true,
            )
        }
        Some(row) if row.status == Status::Probing => {
            return Err(sqlx::Error::Protocol(
                "a proof for this key is in flight; report again when it ends".to_owned(),
            ));
        }
        Some(row) => {
            let merged = merge_detail(&row.detail, &new.detail).to_string();
            let hint = encode_list(&union(&row.hint_paths, &new.hint_paths));
            sqlx::query("UPDATE task_claims SET detail = ?, hint_paths = ? WHERE id = ?")
                .bind(merged)
                .bind(hint)
                .bind(row.id)
                .execute(&mut *tx)
                .await?;
            (row.id, false)
        }
    };
    let (kind, id, worktree, who) = match reporter {
        Reporter::Holder(holder) => (
            reporter_kind_of(holder),
            holder.id.as_str(),
            Some(holder.worktree.as_str()),
            Some(*holder),
        ),
        Reporter::Daemon(label) => ("daemon", *label, None, None),
    };
    sqlx::query(
        "INSERT INTO task_claim_reports
             (task_id, reporter_kind, reporter_id, reporter_worktree, at, detail, paths, tests, blocking)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(task_id)
    .bind(kind)
    .bind(id)
    .bind(worktree)
    .bind(stamp(now))
    .bind(detail)
    .bind(encode_list(&new.hint_paths))
    .bind(encode_list(tests))
    .bind(i64::from(blocking))
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE task_claims SET report_count = report_count + 1, linked_to = COALESCE(?, linked_to) WHERE id = ?",
    )
    .bind(linked_to)
    .bind(task_id)
    .execute(&mut *tx)
    .await?;
    let note: String = detail.chars().take(80).collect();
    record_event_on(&mut tx, task_id, "reported", who, &note, now).await?;
    prune_reports(&mut tx, task_id).await?;
    let task = row_by_id(&mut tx, task_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    let paths: Vec<String> = task
        .guard_paths
        .iter()
        .chain(&task.hint_paths)
        .cloned()
        .collect();
    let similar = similar_to(&mut tx, project_id, &task.key, &paths, task.id, now).await?;
    tx.commit().await?;
    Ok(ReportOutcome {
        reports: task.report_count,
        task,
        created,
        similar,
    })
}

/// Deletes the oldest reports beyond [`REPORTS_KEPT`] and counts them in `detail.reports_dropped`, so a
/// flood of reports keeps the ticket readable.
async fn prune_reports(conn: &mut SqliteConnection, task_id: i64) -> sqlx::Result<()> {
    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM task_claim_reports WHERE task_id = ?")
            .bind(task_id)
            .fetch_one(&mut *conn)
            .await?;
    let excess = stored - REPORTS_KEPT;
    if excess <= 0 {
        return Ok(());
    }
    sqlx::query(
        "DELETE FROM task_claim_reports WHERE id IN
             (SELECT id FROM task_claim_reports WHERE task_id = ? ORDER BY id LIMIT ?)",
    )
    .bind(task_id)
    .bind(excess)
    .execute(&mut *conn)
    .await?;
    let row = row_by_id(conn, task_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    let dropped = row
        .detail
        .get("reports_dropped")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        + excess;
    let merged = merge_detail(
        &row.detail,
        &serde_json::json!({ "reports_dropped": dropped }),
    );
    sqlx::query("UPDATE task_claims SET detail = ? WHERE id = ?")
        .bind(merged.to_string())
        .bind(task_id)
        .execute(conn)
        .await?;
    Ok(())
}
