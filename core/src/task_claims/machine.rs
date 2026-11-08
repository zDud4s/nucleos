//! Daemon-side openers and closers for machine tickets (`migration:`, `fmt:`, `conflict:`). An agent
//! cannot declare these keys (`store::report` refuses them); the daemon opens them when a detector
//! sees the problem and closes them when the detector sees it gone.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::{Closer, NewTask, Reporter, TaskKey, TaskKind, TaskRow};
use super::{keys, offers, store};
use crate::execution::vcs::Op;

/// Opens the ticket for `key`, or adds one more report to the live one: the same problem seen twice
/// is one ticket with two reports, never two tickets. The daemon is the reporter. The paths are
/// hints only: a machine ticket never guards a file.
pub async fn open_machine_ticket(
    pool: &SqlitePool,
    project_id: &str,
    key: TaskKey,
    title: String,
    detail: Value,
    hint_paths: Vec<String>,
    now: DateTime<Utc>,
) -> sqlx::Result<TaskRow> {
    let report_text = title.clone();
    let new = NewTask {
        key,
        kind: TaskKind::Incident,
        title,
        detail,
        guard_paths: Vec::new(),
        hint_paths,
    };
    let outcome = store::report(
        pool,
        project_id,
        new,
        &Reporter::Daemon("daemon"),
        &report_text,
        &[],
        false,
        None,
        now,
    )
    .await?;
    // Only a new ticket is routed: one more report on a live ticket leaves its offer as it is.
    if outcome.created {
        offers::route_or_warn(pool, outcome.task.id, now).await;
    }
    Ok(outcome.task)
}

/// Closes the live ticket for `key` by resolution (`Closer::Resolved`). `false` when there is none.
pub async fn close_machine_ticket(
    pool: &SqlitePool,
    project_id: &str,
    key: &TaskKey,
    note: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let Some(row) = store::live_by_key(pool, project_id, key, now).await? else {
        return Ok(false);
    };
    store::done(pool, row.id, Closer::Resolved, note, now).await
}

/// The key of the conflict between `source` and `target`, through the one normaliser every key
/// goes through. A `conflict:` key has no path in it, so the project root plays no part.
fn conflict_key(source: &str, target: &str) -> Option<TaskKey> {
    keys::normalize(&format!("conflict:{source}->{target}"), Path::new("")).ok()
}

/// One escalated merge as the conflict opener reads it.
#[derive(sqlx::FromRow)]
struct EscalatedMerge {
    id: i64,
    project_id: String,
    op: String,
    args: String,
    resolution_attempts: i64,
    live: bool,
}

/// The escalated merges of one `(project, source, target)`.
#[derive(Default)]
struct ConflictGroup {
    request_ids: Vec<i64>,
    attempts: i64,
    any_live: bool,
}

/// Opens (or reports again) `conflict:<source>-><target>` for every escalated merge the resolver
/// has given up on, one ticket per `(project, source, target)` per call. A conflict qualifies when
/// its attempts are spent (`resolution_attempts >= max_attempts`) and no resolution run of it is
/// live, or when the same request was escalated twice or more, whatever the count.
///
/// The rows are the ones `next_conflict` in `resolver.rs` would consider: `escalated`, `merge`,
/// not from a resolution, unsettled, and with no later success of the same request. A run is live
/// as `next_conflict` reads it: `running` or `awaiting_approval`. Returns how many tickets were
/// opened or reported.
pub async fn open_conflict_tickets(
    pool: &SqlitePool,
    max_attempts: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<u64> {
    let rows: Vec<EscalatedMerge> = sqlx::query_as(
        "SELECT conflict.id, conflict.project_id, conflict.op, conflict.args,
                conflict.resolution_attempts,
                EXISTS (
                    SELECT 1 FROM runs
                     WHERE runs.id = conflict.resolution_run_id
                       AND runs.status IN ('running', 'awaiting_approval')
                ) AS live
           FROM vcs_requests AS conflict
          WHERE conflict.status = 'escalated'
            AND conflict.op = 'merge'
            AND conflict.from_resolution = 0
            AND conflict.settled_at IS NULL
            AND NOT EXISTS (
                SELECT 1 FROM vcs_requests AS settled
                 WHERE settled.project_id = conflict.project_id
                   AND settled.args = conflict.args
                   AND settled.status = 'succeeded'
                   AND settled.id > conflict.id
            )
          ORDER BY conflict.id",
    )
    .fetch_all(pool)
    .await?;
    let mut groups: BTreeMap<(String, String, String), ConflictGroup> = BTreeMap::new();
    for row in rows {
        let Ok(Op::Merge { source, target }) = Op::from_stored(&row.op, &row.args) else {
            continue;
        };
        let group = groups
            .entry((
                row.project_id,
                source.as_str().to_owned(),
                target.as_str().to_owned(),
            ))
            .or_default();
        group.request_ids.push(row.id);
        group.attempts = group.attempts.max(row.resolution_attempts);
        group.any_live |= row.live;
    }
    let mut reported = 0;
    for ((project_id, source, target), group) in groups {
        let exhausted = group.attempts >= max_attempts && !group.any_live;
        let repeated = group.request_ids.len() >= 2;
        if !exhausted && !repeated {
            continue;
        }
        let Some(key) = conflict_key(&source, &target) else {
            tracing::warn!(%source, %target, "tasks: a conflict whose branches make no ticket key");
            continue;
        };
        open_machine_ticket(
            pool,
            &project_id,
            key,
            format!("merge {source} into {target} conflicts and the resolver gave up"),
            json!({ "request_ids": group.request_ids, "attempts": group.attempts }),
            Vec::new(),
            now,
        )
        .await?;
        reported += 1;
    }
    Ok(reported)
}

/// Closes `conflict:<source>-><target>` after a merge of exactly those branches succeeded.
/// `false` when there was no live ticket for it.
pub async fn close_conflict(
    pool: &SqlitePool,
    project_id: &str,
    source: &str,
    target: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<bool> {
    let Some(key) = conflict_key(source, target) else {
        return Ok(false);
    };
    close_machine_ticket(pool, project_id, &key, "merge succeeded", now).await
}

/// `group` as the crate part of a `fmt:` key: lowercased, and every character outside
/// `[a-z0-9_-]` dropped, which is the alphabet `keys::normalize` accepts there.
fn fmt_slug(group: &str) -> Option<String> {
    let slug: String = group
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
        .collect();
    (!slug.is_empty()).then_some(slug)
}

/// The program name of `argv[0]`: no directory, no `.exe`.
fn program_name(program: &str) -> String {
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let lower = base.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_owned()
}

/// The crate slug of a `fmt:` ticket for a unit of `group` that ran `argv`, when that unit is a
/// format step: `cargo fmt ...` (`scripts/gates.sh` runs `cargo fmt --all -- --check`), any
/// `rustfmt ...`, or any `gofmt ...`. `None` for every other step and for an empty group.
pub fn fmt_unit_key(group: &str, argv: &[String]) -> Option<String> {
    let program = program_name(argv.first()?);
    let format_step = match program.as_str() {
        "cargo" => argv.get(1).map(String::as_str) == Some("fmt"),
        "rustfmt" | "gofmt" => true,
        _ => false,
    };
    if !format_step {
        return None;
    }
    fmt_slug(group)
}

/// The key `fmt:<target>:<group slug>`, through the one normaliser every key goes through. A
/// `fmt:` key has no path in it, so the project root plays no part.
fn fmt_key(target: &str, group: &str) -> Option<TaskKey> {
    let slug = fmt_slug(group)?;
    keys::normalize(&format!("fmt:{target}:{slug}"), Path::new("")).ok()
}

/// `sha` cut to the twelve characters a close note shows.
fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// Opens (or reports again) `fmt:<target>:<group slug>`: the format check of `group` failed on
/// `target` at `sha`. A group that makes no key (nothing of `[a-z0-9_-]` in it) is a protocol
/// error, since the caller is expected to have passed it through [`fmt_unit_key`] first.
pub async fn open_fmt_ticket(
    pool: &SqlitePool,
    project_id: &str,
    target: &str,
    group: &str,
    sha: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<TaskRow> {
    let Some(key) = fmt_key(target, group) else {
        return Err(sqlx::Error::Protocol(format!(
            "no fmt ticket key for target '{target}' and group '{group}'"
        )));
    };
    open_machine_ticket(
        pool,
        project_id,
        key,
        format!("{target} fails the format check in {group}"),
        json!({ "target": target, "group": group, "sha": sha }),
        Vec::new(),
        now,
    )
    .await
}

/// Closes every live `fmt:<target>:*` ticket whose group is not in `still_red` (the groups whose
/// format check is still failing, slugged the way the key is), as green at `sha`. Closed by
/// `Closer::Resolved`, like every machine ticket, so `take_notices` reports a ticket closure rather
/// than a master-red incident; the short sha goes into the event note. Returns how many tickets
/// were closed.
pub async fn close_fmt_tickets(
    pool: &SqlitePool,
    project_id: &str,
    target: &str,
    still_red: &[String],
    sha: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<u64> {
    // `keys::normalize` keeps the target trimmed and otherwise as given.
    let prefix = format!("fmt:{}:", target.trim());
    let red: Vec<String> = still_red
        .iter()
        .filter_map(|group| fmt_slug(group))
        .collect();
    let note = format!("format check green at {}", short_sha(sha));
    let mut closed = 0;
    for row in store::list_live(pool, project_id, now).await? {
        let Some(krate) = row.key.as_str().strip_prefix(&prefix) else {
            continue;
        };
        if red.iter().any(|group| group == krate) {
            continue;
        }
        if store::done(pool, row.id, Closer::Resolved, &note, now).await? {
            closed += 1;
        }
    }
    Ok(closed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::vcs::{Branch, Origin, ResolvedRepo};
    use crate::execution::wave::stamp;
    use crate::task_claims::{HolderKind, Status};
    use chrono::{Duration, TimeZone};

    const MAX: i64 = 3;

    /// An escalated merge of `source` into `target`, admitted through the real INSERT so the stored
    /// operation is the one the opener has to parse back, with `attempts` attempts already spent.
    async fn escalated(pool: &SqlitePool, source: &str, target: &str, attempts: i64) -> i64 {
        let repo = ResolvedRepo::synthetic("proj", "C:/repo", "proj");
        let op = Op::Merge {
            source: Branch::new(source).expect("a branch"),
            target: Branch::new(target).expect("a branch"),
        };
        let id = crate::execution::vcs::submit(pool, &repo, &op, Origin::Shell)
            .await
            .expect("admit the merge");
        sqlx::query(
            "UPDATE vcs_requests SET status = 'escalated', resolution_attempts = ? WHERE id = ?",
        )
        .bind(attempts)
        .bind(id)
        .execute(pool)
        .await
        .expect("escalate it");
        id
    }

    /// Links `request` to a resolution run in `status`.
    async fn resolved_by(pool: &SqlitePool, request: i64, run: i64, status: &str) {
        sqlx::query(
            "INSERT INTO runs (id, project_id, prompt, status, mode, created_at)
             VALUES (?, 'proj', 'resolve it', ?, 'worktree', ?)",
        )
        .bind(run)
        .bind(status)
        .bind(Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .expect("insert the resolution's run");
        sqlx::query("UPDATE vcs_requests SET resolution_run_id = ? WHERE id = ?")
            .bind(run)
            .bind(request)
            .execute(pool)
            .await
            .expect("link the conflict to its resolution");
    }

    async fn live_conflict(pool: &SqlitePool, source: &str, target: &str) -> Option<TaskRow> {
        let key = conflict_key(source, target).expect("a conflict key");
        store::live_by_key(pool, "proj", &key, Utc::now())
            .await
            .expect("read the ticket")
    }

    async fn ticket_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM task_claims")
            .fetch_one(pool)
            .await
            .expect("count the tickets")
    }

    #[tokio::test]
    async fn a_conflict_with_exhausted_attempts_and_no_live_run_opens_one_ticket() {
        let pool = crate::testdb::fresh_pool().await;
        let request = escalated(&pool, "feat/x", "master", MAX).await;
        resolved_by(&pool, request, 1, "cancelled").await;
        let opened = open_conflict_tickets(&pool, MAX, Utc::now()).await.unwrap();
        assert_eq!(opened, 1);
        let ticket = live_conflict(&pool, "feat/x", "master")
            .await
            .expect("a live conflict ticket");
        assert_eq!(ticket.key.as_str(), "conflict:feat/x->master");
        assert_eq!(ticket.detail["request_ids"], json!([request]));
        assert_eq!(ticket.detail["attempts"], json!(MAX));
        assert!(ticket.hint_paths.is_empty());
        assert_eq!(ticket_count(&pool).await, 1);
    }

    #[tokio::test]
    async fn a_conflict_still_being_attempted_opens_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        escalated(&pool, "feat/below", "master", MAX - 1).await;
        let running = escalated(&pool, "feat/running", "master", MAX).await;
        resolved_by(&pool, running, 1, "running").await;
        let opened = open_conflict_tickets(&pool, MAX, Utc::now()).await.unwrap();
        assert_eq!(opened, 0);
        assert_eq!(ticket_count(&pool).await, 0);
    }

    #[tokio::test]
    async fn a_settled_conflict_opens_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let settled = escalated(&pool, "feat/x", "master", MAX).await;
        sqlx::query("UPDATE vcs_requests SET settled_at = ? WHERE id = ?")
            .bind(Utc::now().to_rfc3339())
            .bind(settled)
            .execute(&pool)
            .await
            .unwrap();
        let opened = open_conflict_tickets(&pool, MAX, Utc::now()).await.unwrap();
        assert_eq!(opened, 0);
        assert_eq!(ticket_count(&pool).await, 0);
    }

    #[tokio::test]
    async fn the_same_args_escalated_twice_opens_the_ticket_even_below_the_cap() {
        let pool = crate::testdb::fresh_pool().await;
        let first = escalated(&pool, "feat/x", "master", 1).await;
        let again = escalated(&pool, "feat/x", "master", 1).await;
        let opened = open_conflict_tickets(&pool, MAX, Utc::now()).await.unwrap();
        assert_eq!(opened, 1);
        let ticket = live_conflict(&pool, "feat/x", "master")
            .await
            .expect("a live conflict ticket");
        assert_eq!(ticket.detail["request_ids"], json!([first, again]));
        assert_eq!(ticket.detail["attempts"], json!(1));
    }

    #[tokio::test]
    async fn two_ticks_open_one_ticket_with_two_reports() {
        let pool = crate::testdb::fresh_pool().await;
        escalated(&pool, "feat/x", "master", MAX).await;
        open_conflict_tickets(&pool, MAX, Utc::now()).await.unwrap();
        open_conflict_tickets(&pool, MAX, Utc::now()).await.unwrap();
        assert_eq!(ticket_count(&pool).await, 1);
        let ticket = live_conflict(&pool, "feat/x", "master")
            .await
            .expect("a live conflict ticket");
        assert_eq!(ticket.report_count, 2);
        let reports: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM task_claim_reports WHERE task_id = ?")
                .bind(ticket.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reports, 2);
    }

    #[tokio::test]
    async fn closing_a_conflict_ticket_needs_a_successful_merge_with_the_same_source_and_target() {
        let pool = crate::testdb::fresh_pool().await;
        escalated(&pool, "feat/x", "master", MAX).await;
        open_conflict_tickets(&pool, MAX, Utc::now()).await.unwrap();
        let other_source = close_conflict(&pool, "proj", "feat/y", "master", Utc::now()).await;
        assert!(!other_source.unwrap());
        let other_target = close_conflict(&pool, "proj", "feat/x", "develop", Utc::now()).await;
        assert!(!other_target.unwrap());
        assert!(live_conflict(&pool, "feat/x", "master").await.is_some());
        let matching = close_conflict(&pool, "proj", "feat/x", "master", Utc::now()).await;
        assert!(matching.unwrap());
        assert!(live_conflict(&pool, "feat/x", "master").await.is_none());
        let status: String = sqlx::query_scalar(
            "SELECT status FROM task_claims WHERE task_key = 'conflict:feat/x->master'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, Status::Done.as_str());
    }

    #[test]
    fn fmt_unit_key_needs_a_format_step_argv() {
        let argv = |parts: &[&str]| parts.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();
        let cargo_fmt = argv(&["cargo", "fmt", "--all", "--", "--check"]);
        assert_eq!(fmt_unit_key("core", &cargo_fmt).as_deref(), Some("core"));
        assert_eq!(
            fmt_unit_key("echo", &argv(&["gofmt", "-l", "."])).as_deref(),
            Some("echo")
        );
        assert_eq!(
            fmt_unit_key("Core", &argv(&["C:\\cargo\\bin\\cargo.exe", "fmt"])).as_deref(),
            Some("core"),
            "a full path and .exe still name cargo, and the group is slugged"
        );
        assert_eq!(
            fmt_unit_key("core", &argv(&["rustfmt", "--check", "src/lib.rs"])).as_deref(),
            Some("core")
        );
        assert_eq!(
            fmt_unit_key("core", &argv(&["cargo", "nextest", "run"])),
            None
        );
        assert_eq!(fmt_unit_key("", &cargo_fmt), None, "no group, no key");
        assert_eq!(fmt_unit_key("core", &[]), None, "no argv, no step");
    }

    #[tokio::test]
    async fn a_new_conflict_after_the_close_is_a_new_ticket() {
        let pool = crate::testdb::fresh_pool().await;
        escalated(&pool, "feat/x", "master", MAX).await;
        open_conflict_tickets(&pool, MAX, Utc::now()).await.unwrap();
        let first = live_conflict(&pool, "feat/x", "master").await.unwrap();
        let closed = close_conflict(&pool, "proj", "feat/x", "master", Utc::now()).await;
        assert!(closed.unwrap());
        // The old escalation is settled so only the fresh one below is the conflict seen again.
        sqlx::query("UPDATE vcs_requests SET settled_at = ?")
            .bind(Utc::now().to_rfc3339())
            .execute(&pool)
            .await
            .unwrap();
        escalated(&pool, "feat/x", "master", MAX).await;
        let opened = open_conflict_tickets(&pool, MAX, Utc::now()).await.unwrap();
        assert_eq!(opened, 1);
        let second = live_conflict(&pool, "feat/x", "master").await.unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(ticket_count(&pool).await, 2);
    }

    #[tokio::test]
    async fn a_newly_opened_machine_ticket_is_routed() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc.with_ymd_and_hms(2026, 11, 7, 10, 0, 0).unwrap();
        // A live session owning the ticket's path through a running work node.
        sqlx::query(
            "INSERT INTO task_sessions (session_id, project_id, worktree, last_seen)
             VALUES ('s1', 'alpha', '/wt/s1', ?)",
        )
        .bind(stamp(now - Duration::seconds(60)))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO work_nodes (id, project_id, root_id, depth, kind, title, goal, status,
                                     owned_paths, session_id, created_at, updated_at)
             VALUES ('n-s1', 'alpha', 'n-s1', 0, 'leaf', 'edit a', 'goal', 'running',
                     '[\"core/a.rs\"]', 's1', 't', ?)",
        )
        .bind(stamp(now - Duration::seconds(120)))
        .execute(&pool)
        .await
        .unwrap();

        let opened = open_machine_ticket(
            &pool,
            "alpha",
            TaskKey("fmt:core/a.rs".to_owned()),
            "core/a.rs is not formatted".to_owned(),
            serde_json::json!({}),
            vec!["core/a.rs".to_owned()],
            now,
        )
        .await
        .unwrap();

        let live = store::list_live(&pool, "alpha", now).await.unwrap();
        let ticket = live
            .iter()
            .find(|task| task.id == opened.id)
            .expect("the ticket is live");
        assert_eq!(
            ticket
                .offered_to
                .as_ref()
                .map(|to| (to.kind, to.id.as_str())),
            Some((HolderKind::Session, "s1")),
            "a new machine ticket is offered to its competent live holder"
        );
    }
}
