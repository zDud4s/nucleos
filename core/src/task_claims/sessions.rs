//! Interactive sessions and the notices they are owed (spec task-claims section 2).
//!
//! The hooks send `(session_id, cwd)` on every PreToolUse and PostToolUse. `touch_session` registers it
//! and renews the lease of every claim the session still holds; `take_notices` answers "what happened
//! to my claims since I last asked" from the event log, advancing a per-session cursor so each fact is
//! delivered once. A run has no hook of its own session id: `register_pseudo` gives it a cursor row
//! (`run-<id>`) when a ticket is offered to it, and `take_notices` is asked with that cursor and the
//! run's holder.

use super::store::{green_sha, row_by_id};
use super::{Holder, HolderKind, Notice, SESSION_LEASE_SECONDS, Status, TaskRow, competence};
use crate::execution::wave::stamp;
use chrono::{DateTime, Duration, Utc};
use sqlx::{SqliteConnection, SqlitePool};
use std::path::Path;

/// Registers (or refreshes) a session and renews every claim it holds that has not already lapsed.
///
/// A lapsed lease is NOT revived: the spec renews "live" claims only, and a session that went silent
/// past the lease learns of it through `take_notices` if somebody took the task. A session registered
/// for the first time starts its cursor at the current end of the event log, so it is not told about
/// history that predates it.
pub async fn touch_session(
    pool: &SqlitePool,
    session_id: &str,
    worktree: &str,
    project_id: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let at = stamp(now);
    sqlx::query(
        "INSERT INTO task_sessions (session_id, project_id, worktree, last_seen, notice_cursor)
         VALUES (?, ?, ?, ?, (SELECT COALESCE(MAX(id), 0) FROM task_claim_events))
         ON CONFLICT(session_id) DO UPDATE SET
             project_id = excluded.project_id, worktree = excluded.worktree, last_seen = excluded.last_seen",
    )
    .bind(session_id)
    .bind(project_id)
    .bind(worktree)
    .bind(&at)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE task_claims SET lease_until = ?
         WHERE status = 'claimed' AND holder_kind = 'session' AND holder_id = ? AND lease_until >= ?",
    )
    .bind(stamp(now + Duration::seconds(SESSION_LEASE_SECONDS)))
    .bind(session_id)
    .bind(&at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Registers a cursor row for a holder that is not an interactive session (a run offered a ticket is
/// `run-<id>`), so `take_notices` can deliver to it. The cursor starts at `cursor_before_event` -- the
/// caller passes the id of the event it just wrote minus one -- and NOT at the end of the log as
/// `touch_session` does, which would skip that very event. A row that already exists keeps its own,
/// older cursor and everything else (`ON CONFLICT DO NOTHING`).
pub async fn register_pseudo(
    pool: &SqlitePool,
    session_id: &str,
    project_id: &str,
    worktree: &str,
    cursor_before_event: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO task_sessions (session_id, project_id, worktree, last_seen, notice_cursor)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(session_id) DO NOTHING",
    )
    .bind(session_id)
    .bind(project_id)
    .bind(worktree)
    .bind(stamp(now))
    .bind(cursor_before_event.max(0))
    .execute(pool)
    .await?;
    Ok(())
}

/// Was `session_id` registered from this worktree? Plan B validates a session id passed by an agent
/// against this: the id must have been seen coming from the very worktree the MCP box serves.
pub async fn session_seen_in(
    pool: &SqlitePool,
    session_id: &str,
    worktree: &str,
) -> sqlx::Result<bool> {
    let found: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM task_sessions WHERE session_id = ? AND worktree = ?)",
    )
    .bind(session_id)
    .bind(worktree)
    .fetch_one(pool)
    .await?;
    Ok(found != 0)
}

/// What happened to a holder's claims since its cursor, and the cursor moves past it. The cursor is the
/// `task_sessions` row `cursor_session_id`; the events are read for the holder `(holder_kind,
/// holder_id)`. An interactive session is its own cursor: `(id, HolderKind::Session, id)`; a run is
/// `("run-<id>", HolderKind::Run, "<id>")` (see `register_pseudo`).
///
/// - `ClaimLost`: an `expired_reclaimed` / `taken_over` event names this session as the PREVIOUS owner,
///   and the task is claimed now by somebody else.
/// - `IncidentClosed`: a `done` event closed by green (`green <sha>` note) names this session as the
///   owner. The session's own `task_done` has no such note and says nothing.
///
/// - `InheritedClosed`: a `done` event for an incident that this session's worktree inherited in a
///   verify ticket (`verify_requests.inherited_task_ids`); see `inherited_notices`.
/// - `Offered`: an `offered` event names this holder as the offeree, and the ticket is still `open`
///   and still offered to it (a claim by anyone, or a re-route, drops the notice). It carries
///   `competence::context` as of `now`.
pub async fn take_notices(
    pool: &SqlitePool,
    cursor_session_id: &str,
    holder_kind: HolderKind,
    holder_id: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Vec<Notice>> {
    let session_id = cursor_session_id;
    // This runs on every tool call of every session, almost always with nothing new to say: look first
    // with plain reads and only take the write lock (`BEGIN IMMEDIATE`) when there is something to
    // deliver. The unlocked look is only a filter; the real read of the cursor happens again inside.
    let Some(seen): Option<i64> =
        sqlx::query_scalar("SELECT notice_cursor FROM task_sessions WHERE session_id = ?")
            .bind(session_id)
            .fetch_optional(pool)
            .await?
    else {
        return Ok(Vec::new());
    };
    let latest: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM task_claim_events")
        .fetch_one(pool)
        .await?;
    if latest <= seen {
        return Ok(Vec::new());
    }

    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    // Re-read under the lock: another call for the same session may have delivered meanwhile.
    let Some(cursor): Option<i64> =
        sqlx::query_scalar("SELECT notice_cursor FROM task_sessions WHERE session_id = ?")
            .bind(session_id)
            .fetch_optional(&mut *tx)
            .await?
    else {
        return Ok(Vec::new());
    };
    let newest: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM task_claim_events")
        .fetch_one(&mut *tx)
        .await?;
    if newest <= cursor {
        return Ok(Vec::new());
    }
    let events: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT task_id, event, note FROM task_claim_events
         WHERE id > ? AND id <= ? AND holder_kind = ? AND holder_id = ?
           AND event IN ('taken_over', 'expired_reclaimed', 'done', 'offered')
         ORDER BY id",
    )
    .bind(cursor)
    .bind(newest)
    .bind(holder_kind.as_str())
    .bind(holder_id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("UPDATE task_sessions SET notice_cursor = ? WHERE session_id = ?")
        .bind(newest)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;

    let mut notices = Vec::new();
    let mut offered: Vec<(TaskRow, String)> = Vec::new();
    for (task_id, event, note) in events {
        let Some(task) = row_by_id(&mut tx, task_id).await? else {
            continue;
        };
        if event == "offered" {
            let still_mine = task.status == Status::Open
                && task
                    .offered_to
                    .as_ref()
                    .is_some_and(|to| to.kind == holder_kind && to.id == holder_id);
            if still_mine {
                // A later offer of the same ticket supersedes the earlier one.
                offered.retain(|(seen, _)| seen.id != task.id);
                offered.push((task, note));
            }
        } else if event == "done" {
            if let Some(sha) = green_sha(&note) {
                notices.push(Notice::IncidentClosed {
                    task,
                    sha: Some(sha),
                });
            } else if let Some(resolved) = super::resolved_note(&note) {
                notices.push(Notice::TicketClosed {
                    task,
                    note: resolved.to_owned(),
                });
            }
        } else if task.status == Status::Claimed
            && let Some(new_holder) = task.holder()
            && !(new_holder.kind == holder_kind && new_holder.id == holder_id)
        {
            // One notice per task per call: a later event for the same task supersedes the earlier one.
            notices.retain(
                |n| !matches!(n, Notice::ClaimLost { task: seen, .. } if seen.id == task.id),
            );
            notices.push(Notice::ClaimLost { task, new_holder });
        }
    }
    // Inside the same transaction: the cursor has advanced, so an error here rolls the advance back and
    // the notice is not lost.
    inherited_notices(
        &mut tx,
        session_id,
        holder_kind,
        holder_id,
        cursor,
        newest,
        &mut notices,
    )
    .await?;
    reporter_notices(
        &mut tx,
        holder_kind,
        holder_id,
        cursor,
        newest,
        &mut notices,
    )
    .await?;
    tx.commit().await?;
    // The context reads through the pool (git included, bounded), so it is built after the commit:
    // the cursor has moved, and an offer is delivered once. A failed context read must not swallow the
    // other notices, so the offer then goes out with a null context.
    for (task, note) in offered {
        let context = competence::context(pool, &task, now)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, task = task.id, "task claims: offer delivered without its context");
                serde_json::Value::Null
            });
        notices.push(Notice::Offered {
            task,
            note,
            context,
        });
    }
    Ok(notices)
}

/// A session that reported a ticket as blocking hears when it closes, whoever closed it. A ticket this
/// call already notified about (the old owner's `IncidentClosed` / `TicketClosed`) is not repeated.
async fn reporter_notices(
    conn: &mut SqliteConnection,
    holder_kind: HolderKind,
    holder_id: &str,
    cursor: i64,
    newest: i64,
    notices: &mut Vec<Notice>,
) -> sqlx::Result<()> {
    let closed: Vec<(i64, String)> = sqlx::query_as(
        "SELECT e.task_id, e.note FROM task_claim_events e
         WHERE e.id > ? AND e.id <= ? AND e.event = 'done'
           AND EXISTS (SELECT 1 FROM task_claim_reports r
                       WHERE r.task_id = e.task_id AND r.reporter_kind = ?
                         AND r.reporter_id = ? AND r.blocking = 1)
         ORDER BY e.id",
    )
    .bind(cursor)
    .bind(newest)
    .bind(holder_kind.as_str())
    .bind(holder_id)
    .fetch_all(&mut *conn)
    .await?;
    for (task_id, note) in closed {
        let already = notices.iter().any(|notice| match notice {
            Notice::IncidentClosed { task, .. }
            | Notice::InheritedClosed { task, .. }
            | Notice::TicketClosed { task, .. } => task.id == task_id,
            Notice::ClaimLost { .. } | Notice::Offered { .. } => false,
        });
        if already {
            continue;
        }
        let Some(task) = row_by_id(conn, task_id).await? else {
            continue;
        };
        let note = super::resolved_note(&note).unwrap_or(&note).to_owned();
        notices.push(Notice::TicketClosed { task, note });
    }
    Ok(())
}

/// The comparison key of a worktree path. `canonicalize` adds the `\\?\` verbatim prefix on Windows,
/// git prints forward slashes, a hook may send a trailing one, and Windows paths ignore case: none of
/// these make two paths different.
pub(super) fn worktree_key(canonical: &str) -> String {
    let path = match canonical.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!("//{rest}"),
        None => canonical
            .strip_prefix(r"\\?\")
            .unwrap_or(canonical)
            .to_owned(),
    };
    let path = path.replace('\\', "/");
    let path = path.trim_end_matches('/');
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_owned()
    }
}

/// `None` when the path cannot be canonicalised (a deleted worktree): the caller skips it.
async fn worktree_key_of(path: &str) -> Option<String> {
    match crate::execution::git_exec::canonical(Path::new(path)).await {
        Ok(canonical) => Some(worktree_key(&canonical)),
        Err(reason) => {
            tracing::debug!(%reason, path, "task claims: skipping a worktree that cannot be canonicalised");
            None
        }
    }
}

/// The sessions whose verify ticket inherited an incident that just closed are owed "#N resolved at
/// <sha>, rebase and re-verify" (spec section 3, "Aviso de fecho"). A `done` event is read for ANY
/// holder; the session that held the incident gets `IncidentClosed` instead and is skipped here.
/// The `verify_requests.worktree` is git's toplevel and `task_sessions.worktree` is what the hook sent:
/// both are canonicalised and compared by `worktree_key`; a ticket whose worktree is gone is skipped.
async fn inherited_notices(
    conn: &mut SqliteConnection,
    session_id: &str,
    holder_kind: HolderKind,
    holder_id: &str,
    cursor: i64,
    newest: i64,
    notices: &mut Vec<Notice>,
) -> sqlx::Result<()> {
    let closed: Vec<(i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT task_id, holder_kind, holder_id FROM task_claim_events
         WHERE id > ? AND id <= ? AND event = 'done' ORDER BY id",
    )
    .bind(cursor)
    .bind(newest)
    .fetch_all(&mut *conn)
    .await?;
    if closed.is_empty() {
        return Ok(());
    }
    let session: Option<(String, String)> =
        sqlx::query_as("SELECT worktree, project_id FROM task_sessions WHERE session_id = ?")
            .bind(session_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((worktree, project_id)) = session else {
        return Ok(());
    };
    let Some(mine) = worktree_key_of(&worktree).await else {
        return Ok(());
    };
    for (task_id, event_kind, event_id) in closed {
        let held_it = event_kind.as_deref() == Some(holder_kind.as_str())
            && event_id.as_deref() == Some(holder_id);
        if held_it {
            continue;
        }
        let Some(task) = row_by_id(&mut *conn, task_id).await? else {
            continue;
        };
        if task.project_id != project_id {
            continue;
        }
        let worktrees: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT v.worktree
               FROM verify_requests v, json_each(v.inherited_task_ids) j
              WHERE v.inherited_task_ids != '[]' AND v.project_id = ? AND j.value = ?",
        )
        .bind(&project_id)
        .bind(task_id)
        .fetch_all(&mut *conn)
        .await?;
        let mut waited = false;
        for candidate in worktrees {
            if worktree_key_of(&candidate)
                .await
                .is_some_and(|key| key == mine)
            {
                waited = true;
                break;
            }
        }
        if waited {
            let sha = task.close_note.as_deref().and_then(green_sha);
            notices.push(Notice::InheritedClosed { task, sha });
        }
    }
    Ok(())
}

/// What a run is owed, worded for its next allowed PreToolUse: `take_notices` on the run's
/// pseudo-session `run-<id>` (registered by `offers` when a ticket is offered to it), each notice
/// rendered by [`render_notice`]. Empty for a run nothing was ever offered to: it has no cursor row.
pub async fn take_run_notices(
    pool: &SqlitePool,
    run_id: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<Vec<String>> {
    let id = run_id.to_string();
    let notices = take_notices(pool, &format!("run-{id}"), HolderKind::Run, &id, now).await?;
    Ok(notices.iter().map(render_notice).collect())
}

/// A notice as the agent reads it: what happened and what to do about it. The one renderer:
/// `http::tasks` re-exports it and the PreToolUse hook reaches it through [`take_run_notices`].
pub fn render_notice(notice: &Notice) -> String {
    match notice {
        Notice::ClaimLost { task, new_holder } => format!(
            "Your claim on #{} `{}` is gone: it is now held by {}. Stop working on it; take another task or ask.",
            task.id,
            task.key,
            holder_label(new_holder),
        ),
        Notice::IncidentClosed { task, sha } => format!(
            "Incident #{} `{}` was closed{}: master is green again. If you were fixing it, stop; \
             if your verify inherited it, rebase on master and verify again.",
            task.id,
            task.key,
            at_sha(sha.as_deref()),
        ),
        Notice::InheritedClosed { task, sha } => format!(
            "Incident #{} `{}` was closed{}: master is green again. Your verify inherited it: \
             rebase on master and verify again.",
            task.id,
            task.key,
            at_sha(sha.as_deref()),
        ),
        Notice::TicketClosed { task, note } => format!(
            "Ticket #{} `{}` was closed: {note}. If you were fixing it, stop. If you were waiting for it, \
             rebase on the target and continue.",
            task.id, task.key,
        ),
        Notice::Offered {
            task,
            note,
            context,
        } => format!(
            "Ticket #{} `{}` is yours: {}. {note}. Competent: {}. Claim it with task_claim, or \
             decline with task_decline (message, optional suggest_session_id). Independent of your \
             own work? Launch a leaf for it and carry on.",
            task.id,
            task.key,
            task.title,
            render_competent(context),
        ),
    }
}

/// How an agent can name a holder when it reads who has a task.
fn holder_label(holder: &Holder) -> String {
    match holder.kind {
        HolderKind::Run => format!("run #{}", holder.id),
        HolderKind::Job => format!("job #{}", holder.id),
        HolderKind::Postgate => "the daemon (post-merge gate)".to_owned(),
        HolderKind::Verify => "the daemon (verify)".to_owned(),
        HolderKind::Session => format!("another session in {}", holder.worktree),
    }
}

/// ` at <first seven characters of the sha>`, cut on a character boundary, or nothing.
fn at_sha(sha: Option<&str>) -> String {
    sha.map(|sha| {
        let short = sha
            .char_indices()
            .nth(7)
            .map_or(sha, |(end, _)| &sha[..end]);
        format!(" at {short}")
    })
    .unwrap_or_default()
}

/// The `competent` list of a `competence::context` as one line: `session:abc (owned: doing), ...`,
/// or `none` when the context is null, unreadable or lists nobody.
fn render_competent(context: &serde_json::Value) -> String {
    use serde_json::Value;
    let listed: Vec<String> = context
        .get("competent")
        .and_then(Value::as_array)
        .map(|competent| {
            competent
                .iter()
                .filter_map(|entry| {
                    let holder = entry.get("holder")?;
                    let kind = holder.get("kind").and_then(Value::as_str)?;
                    let id = holder.get("id").and_then(Value::as_str)?;
                    let source = entry.get("source").and_then(Value::as_str).unwrap_or("?");
                    let doing = entry.get("doing").and_then(Value::as_str).unwrap_or("");
                    Some(if doing.is_empty() {
                        format!("{kind}:{id} ({source})")
                    } else {
                        format!("{kind}:{id} ({source}: {doing})")
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if listed.is_empty() {
        "none".to_owned()
    } else {
        listed.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_claims::offers;
    use crate::task_claims::store::{self, claim, done};
    use crate::task_claims::{ClaimOutcome, Closer, Holder, NewTask, TaskKey, TaskKind};
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 22, 10, 0, 0).unwrap()
    }

    fn secs(n: i64) -> chrono::Duration {
        chrono::Duration::seconds(n)
    }

    fn holder(id: &str) -> Holder {
        Holder {
            kind: HolderKind::Session,
            id: id.into(),
            worktree: format!("/wt/{id}"),
        }
    }

    fn task(key: &str) -> NewTask {
        // `master-red` is the daemon's alone: a declared claim on it is refused, so its fixtures are incidents.
        let kind = if key == crate::task_claims::MASTER_RED_KEY {
            TaskKind::Incident
        } else {
            TaskKind::Declared
        };
        NewTask {
            key: crate::task_claims::keys::normalize(key, std::path::Path::new("/repo")).unwrap(),
            kind,
            title: "t".into(),
            detail: serde_json::json!({}),
            guard_paths: vec![],
            hint_paths: vec![],
        }
    }

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn claimed(pool: &SqlitePool, key: &str, who: &Holder, at: DateTime<Utc>) -> i64 {
        match claim(pool, "p", task(key), who, at).await.unwrap() {
            ClaimOutcome::Won { task, .. } => task.id,
            ClaimOutcome::Lost { .. } => panic!("the fixture claim must win"),
        }
    }

    #[tokio::test]
    async fn touching_a_session_registers_it_and_renews_only_its_live_claims() {
        let pool = pool().await;
        let me = holder("s1");
        let live = claimed(&pool, "file:a.rs", &me, t0()).await;
        let other = claimed(&pool, "file:b.rs", &holder("s2"), t0()).await;
        let expired = claimed(
            &pool,
            "file:c.rs",
            &me,
            t0() - secs(SESSION_LEASE_SECONDS + 5),
        )
        .await;

        touch_session(&pool, "s1", "/wt/s1", "p", t0() + secs(600))
            .await
            .unwrap();

        let lease = |id: i64| {
            let pool = pool.clone();
            async move {
                let mut conn = pool.acquire().await.unwrap();
                store::row_by_id(&mut conn, id)
                    .await
                    .unwrap()
                    .unwrap()
                    .lease_until
            }
        };
        assert_eq!(
            lease(live).await,
            Some(stamp(t0() + secs(600 + SESSION_LEASE_SECONDS)))
        );
        assert_eq!(
            lease(other).await,
            Some(stamp(t0() + secs(SESSION_LEASE_SECONDS))),
            "another session's claim is untouched"
        );
        assert_eq!(
            lease(expired).await,
            Some(stamp(t0() - secs(5))),
            "a lapsed lease is not revived"
        );
        assert!(session_seen_in(&pool, "s1", "/wt/s1").await.unwrap());
        assert!(
            !session_seen_in(&pool, "s1", "/wt/elsewhere").await.unwrap(),
            "only the worktree it was seen in"
        );
        assert!(!session_seen_in(&pool, "ghost", "/wt/s1").await.unwrap());
    }

    #[tokio::test]
    async fn a_session_is_told_once_that_its_claim_was_taken() {
        let pool = pool().await;
        let me = holder("s1");
        touch_session(&pool, "s1", "/wt/s1", "p", t0())
            .await
            .unwrap();
        claimed(&pool, "file:core/a.rs", &me, t0()).await;
        let late = t0() + secs(SESSION_LEASE_SECONDS + 1);
        claimed(&pool, "file:core/a.rs", &holder("s2"), late).await;

        let notices = take_notices(&pool, "s1", HolderKind::Session, "s1", late)
            .await
            .unwrap();
        let [Notice::ClaimLost { task, new_holder }] = notices.as_slice() else {
            panic!("expected one ClaimLost, got {notices:?}")
        };
        assert_eq!(task.key.as_str(), "file:core/a.rs");
        assert_eq!(new_holder.id, "s2");
        assert!(
            take_notices(&pool, "s1", HolderKind::Session, "s1", late)
                .await
                .unwrap()
                .is_empty(),
            "the cursor advanced"
        );
    }

    #[tokio::test]
    async fn two_losses_of_one_task_in_one_window_give_one_notice() {
        let pool = pool().await;
        touch_session(&pool, "s1", "/wt/s1", "p", t0())
            .await
            .unwrap();
        claimed(&pool, "file:core/a.rs", &holder("s1"), t0()).await;
        let expire = |at: DateTime<Utc>| {
            let pool = pool.clone();
            async move {
                sqlx::query("UPDATE task_claims SET lease_until = ?")
                    .bind(stamp(at - secs(1)))
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        };
        let t1 = t0() + secs(SESSION_LEASE_SECONDS + 1);
        claimed(&pool, "file:core/a.rs", &holder("s2"), t1).await;
        expire(t1).await;
        let t2 = t1 + secs(10);
        claimed(&pool, "file:core/a.rs", &holder("s1"), t2).await;
        expire(t2).await;
        let t3 = t2 + secs(10);
        claimed(&pool, "file:core/a.rs", &holder("s3"), t3).await;

        let notices = take_notices(&pool, "s1", HolderKind::Session, "s1", t3)
            .await
            .unwrap();
        let [Notice::ClaimLost { new_holder, .. }] = notices.as_slice() else {
            panic!("expected one ClaimLost, got {notices:?}")
        };
        assert_eq!(new_holder.id, "s3");
    }

    #[tokio::test]
    async fn a_session_that_took_the_claim_back_hears_nothing() {
        let pool = pool().await;
        let me = holder("s1");
        touch_session(&pool, "s1", "/wt/s1", "p", t0())
            .await
            .unwrap();
        claimed(&pool, "file:core/a.rs", &me, t0()).await;
        let late = t0() + secs(SESSION_LEASE_SECONDS + 1);
        claimed(&pool, "file:core/a.rs", &holder("s2"), late).await;
        sqlx::query("UPDATE task_claims SET lease_until = ?")
            .bind(stamp(late - secs(1)))
            .execute(&pool)
            .await
            .unwrap();
        claimed(&pool, "file:core/a.rs", &me, late + secs(1)).await;
        assert!(
            take_notices(&pool, "s1", HolderKind::Session, "s1", late)
                .await
                .unwrap()
                .is_empty(),
            "it holds the task again"
        );
    }

    #[tokio::test]
    async fn the_owner_is_told_when_the_incident_closes_by_green_but_not_for_its_own_done() {
        let pool = pool().await;
        let me = holder("s1");
        touch_session(&pool, "s1", "/wt/s1", "p", t0())
            .await
            .unwrap();
        let red = claimed(&pool, "master-red", &me, t0()).await;
        done(
            &pool,
            red,
            Closer::Green {
                sha: "abc123".into(),
            },
            "revert",
            t0() + secs(5),
        )
        .await
        .unwrap();
        let own = claimed(&pool, "file:own.rs", &me, t0()).await;
        done(&pool, own, Closer::Holder(&me), "fixed", t0() + secs(6))
            .await
            .unwrap();

        let notices = take_notices(&pool, "s1", HolderKind::Session, "s1", t0() + secs(7))
            .await
            .unwrap();
        let [Notice::IncidentClosed { task, sha }] = notices.as_slice() else {
            panic!("expected one IncidentClosed, got {notices:?}")
        };
        assert_eq!(
            (task.id, task.status, sha.as_deref()),
            (red, Status::Done, Some("abc123"))
        );
    }

    async fn reported_by(pool: &SqlitePool, key: &str, who: &Holder, blocking: bool) -> i64 {
        let outcome = store::report(
            pool,
            "p",
            task(key),
            &crate::task_claims::Reporter::Holder(who),
            "it blocks me",
            &[],
            blocking,
            None,
            t0(),
        )
        .await
        .unwrap();
        outcome.task.id
    }

    #[tokio::test]
    async fn a_blocking_reporter_hears_when_the_ticket_closes_whoever_closed_it() {
        let pool = pool().await;
        let (reporter, fixer) = (holder("s1"), holder("s2"));
        touch_session(&pool, "s1", "/wt/s1", "p", t0())
            .await
            .unwrap();
        let id = claimed(&pool, "file:core/a.rs", &fixer, t0()).await;
        reported_by(&pool, "file:core/a.rs", &reporter, true).await;
        done(&pool, id, Closer::Holder(&fixer), "fixed", t0() + secs(5))
            .await
            .unwrap();

        let notices = take_notices(&pool, "s1", HolderKind::Session, "s1", t0() + secs(6))
            .await
            .unwrap();
        let [Notice::TicketClosed { task, note }] = notices.as_slice() else {
            panic!("expected one TicketClosed, got {notices:?}")
        };
        assert_eq!((task.id, note.as_str()), (id, "fixed"));
    }

    #[tokio::test]
    async fn a_non_blocking_reporter_hears_nothing() {
        let pool = pool().await;
        let (reporter, fixer) = (holder("s1"), holder("s2"));
        touch_session(&pool, "s1", "/wt/s1", "p", t0())
            .await
            .unwrap();
        let id = claimed(&pool, "file:core/a.rs", &fixer, t0()).await;
        reported_by(&pool, "file:core/a.rs", &reporter, false).await;
        done(&pool, id, Closer::Holder(&fixer), "fixed", t0() + secs(5))
            .await
            .unwrap();
        assert!(
            take_notices(&pool, "s1", HolderKind::Session, "s1", t0() + secs(6))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_resolved_closure_tells_the_old_owner() {
        let pool = pool().await;
        let me = holder("s1");
        touch_session(&pool, "s1", "/wt/s1", "p", t0())
            .await
            .unwrap();
        let id = claimed(&pool, "file:core/a.rs", &me, t0()).await;
        done(
            &pool,
            id,
            Closer::Resolved,
            "branch no longer collides",
            t0() + secs(5),
        )
        .await
        .unwrap();

        let notices = take_notices(&pool, "s1", HolderKind::Session, "s1", t0() + secs(6))
            .await
            .unwrap();
        let [Notice::TicketClosed { task, note }] = notices.as_slice() else {
            panic!("expected one TicketClosed, got {notices:?}")
        };
        assert_eq!((task.id, note.as_str()), (id, "branch no longer collides"));
    }

    #[tokio::test]
    async fn master_red_green_closure_still_gives_incident_closed() {
        let pool = pool().await;
        let me = holder("s1");
        touch_session(&pool, "s1", "/wt/s1", "p", t0())
            .await
            .unwrap();
        let red = claimed(&pool, "master-red", &me, t0()).await;
        // The owner also reported it as blocking: the green closure must still be one notice.
        reported_by(&pool, "master-red", &me, true).await;
        done(
            &pool,
            red,
            Closer::Green {
                sha: "abc123".into(),
            },
            "revert",
            t0() + secs(5),
        )
        .await
        .unwrap();
        let notices = take_notices(&pool, "s1", HolderKind::Session, "s1", t0() + secs(6))
            .await
            .unwrap();
        assert!(
            matches!(notices.as_slice(), [Notice::IncidentClosed { .. }]),
            "{notices:?}"
        );
    }

    #[tokio::test]
    async fn a_new_session_does_not_replay_history_and_an_unknown_one_gets_nothing() {
        let pool = pool().await;
        claimed(&pool, "file:old.rs", &holder("s1"), t0()).await;
        claimed(
            &pool,
            "file:old.rs",
            &holder("s2"),
            t0() + secs(SESSION_LEASE_SECONDS + 1),
        )
        .await;
        // s1 shows up only now: the events above happened before it was registered.
        touch_session(
            &pool,
            "s1",
            "/wt/s1",
            "p",
            t0() + secs(SESSION_LEASE_SECONDS + 2),
        )
        .await
        .unwrap();
        assert!(
            take_notices(&pool, "s1", HolderKind::Session, "s1", t0())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            take_notices(&pool, "never-seen", HolderKind::Session, "never-seen", t0())
                .await
                .unwrap()
                .is_empty()
        );
        let _ = TaskKey::master_red(); // keeps the import honest if the fixtures above change
    }

    use crate::verify::store::{NewRequest, insert_request, set_classification};

    /// A verify ticket of project `p` over `worktree`, classified as having inherited `incidents`.
    async fn ticket_at(pool: &SqlitePool, worktree: &str, incidents: &[i64]) -> i64 {
        let id = insert_request(
            pool,
            &NewRequest {
                project_id: "p",
                worktree,
                kind: "test",
                scope: "scope",
                base: None,
                priority: 0,
                caller: "owner",
                note: None,
                unclaimed: &[],
            },
        )
        .await
        .unwrap();
        set_classification(pool, id, "{}", incidents).await.unwrap();
        id
    }

    fn native(dir: &tempfile::TempDir) -> String {
        dir.path().to_string_lossy().into_owned()
    }

    /// How git prints a toplevel: forward slashes, whatever the platform.
    fn git_form(dir: &tempfile::TempDir) -> String {
        native(dir).replace('\\', "/")
    }

    #[tokio::test]
    async fn a_session_whose_ticket_inherited_the_incident_is_told_when_it_closes() {
        let pool = pool().await;
        let (a, b, c) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        for (session, dir) in [("s1", &a), ("s2", &b), ("s3", &c)] {
            touch_session(&pool, session, &native(dir), "p", t0())
                .await
                .unwrap();
        }
        let red = claimed(&pool, "master-red", &holder("owner"), t0()).await;
        let unrelated = claimed(&pool, "file:x.rs", &holder("owner"), t0()).await;
        ticket_at(&pool, &git_form(&a), &[red]).await; // s1's worktree, twice: one notice, not two
        ticket_at(&pool, &git_form(&a), &[red]).await;
        ticket_at(&pool, &git_form(&b), &[red]).await; // s2's
        ticket_at(&pool, &git_form(&c), &[unrelated]).await; // s3 inherited something else

        done(
            &pool,
            red,
            Closer::Green {
                sha: "abc123".into(),
            },
            "revert",
            t0() + secs(5),
        )
        .await
        .unwrap();

        let s1 = take_notices(&pool, "s1", HolderKind::Session, "s1", t0() + secs(6))
            .await
            .unwrap();
        let [Notice::InheritedClosed { task, sha }] = s1.as_slice() else {
            panic!("expected one InheritedClosed, got {s1:?}")
        };
        assert_eq!((task.id, sha.as_deref()), (red, Some("abc123")));
        assert!(
            take_notices(&pool, "s1", HolderKind::Session, "s1", t0() + secs(7))
                .await
                .unwrap()
                .is_empty(),
            "the cursor advanced"
        );
        let s2 = take_notices(&pool, "s2", HolderKind::Session, "s2", t0() + secs(6))
            .await
            .unwrap();
        assert!(
            matches!(s2.as_slice(), [Notice::InheritedClosed { .. }]),
            "{s2:?}"
        );
        assert!(
            take_notices(&pool, "s3", HolderKind::Session, "s3", t0() + secs(6))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn the_holder_keeps_its_own_notice_and_does_not_get_the_inherited_one_twice() {
        let pool = pool().await;
        let a = tempfile::tempdir().unwrap();
        touch_session(&pool, "s1", &native(&a), "p", t0())
            .await
            .unwrap();
        let red = claimed(&pool, "master-red", &holder("s1"), t0()).await;
        ticket_at(&pool, &git_form(&a), &[red]).await;

        done(
            &pool,
            red,
            Closer::Green {
                sha: "abc123".into(),
            },
            "revert",
            t0() + secs(5),
        )
        .await
        .unwrap();

        let notices = take_notices(&pool, "s1", HolderKind::Session, "s1", t0() + secs(6))
            .await
            .unwrap();
        assert!(
            matches!(notices.as_slice(), [Notice::IncidentClosed { .. }]),
            "{notices:?}"
        );
    }

    #[tokio::test]
    async fn a_ticket_over_a_deleted_worktree_is_skipped_and_so_is_a_session_in_one() {
        let pool = pool().await;
        let (live, gone) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let gone_path = git_form(&gone);
        drop(gone); // the worktree was removed: `canonicalize` fails for it
        touch_session(&pool, "s-live", &native(&live), "p", t0())
            .await
            .unwrap();
        touch_session(
            &pool,
            "s-gone",
            &gone_path.replace('/', std::path::MAIN_SEPARATOR_STR),
            "p",
            t0(),
        )
        .await
        .unwrap();
        let red = claimed(&pool, "master-red", &holder("owner"), t0()).await;
        ticket_at(&pool, &gone_path, &[red]).await; // skipped: cannot be canonicalised
        ticket_at(&pool, &git_form(&live), &[red]).await;

        done(
            &pool,
            red,
            Closer::Green {
                sha: "abc123".into(),
            },
            "revert",
            t0() + secs(5),
        )
        .await
        .unwrap();

        let live_notices = take_notices(
            &pool,
            "s-live",
            HolderKind::Session,
            "s-live",
            t0() + secs(6),
        )
        .await
        .unwrap();
        assert!(
            matches!(live_notices.as_slice(), [Notice::InheritedClosed { .. }]),
            "{live_notices:?}"
        );
        assert!(
            take_notices(
                &pool,
                "s-gone",
                HolderKind::Session,
                "s-gone",
                t0() + secs(6)
            )
            .await
            .unwrap()
            .is_empty(),
            "a session whose own worktree is gone hears nothing, and nothing panics"
        );
    }

    #[test]
    fn worktree_keys_ignore_the_verbatim_prefix_the_slash_direction_and_a_trailing_slash() {
        assert_eq!(worktree_key(r"\\?\C:\Repo\wt"), worktree_key("C:/Repo/wt/"));
        assert_eq!(
            worktree_key(r"\\?\UNC\srv\share\wt"),
            worktree_key("//srv/share/wt")
        );
        assert_ne!(worktree_key("/r/wt-a"), worktree_key("/r/wt-b"));
    }

    #[test]
    #[cfg(windows)]
    fn on_windows_worktree_keys_ignore_case() {
        assert_eq!(worktree_key(r"\\?\C:\Repo\WT"), worktree_key("c:/repo/wt"));
    }

    #[test]
    #[cfg(not(windows))]
    fn elsewhere_worktree_keys_keep_case() {
        assert_ne!(worktree_key("/r/WT"), worktree_key("/r/wt"));
    }

    /// An `open` ticket of project `p` guarding `core/a.rs`, and the session `owner` registered and
    /// owning that path through a running work node (so `offers::route` offers it to `owner`).
    async fn offerable_ticket(pool: &SqlitePool, owner: &str) -> i64 {
        touch_session(pool, owner, &format!("/wt/{owner}"), "p", t0())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO work_nodes (id, project_id, root_id, depth, kind, title, goal, status,
                                     owned_paths, session_id, created_at, updated_at)
             VALUES ('n1', 'p', 'n1', 0, 'leaf', 'edit a', 'goal', 'running', '[\"core/a.rs\"]', ?, 't', 't')",
        )
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
        let mut new = task("file:core/a.rs");
        new.guard_paths = vec!["core/a.rs".to_owned()];
        store::create_or_adopt_open(pool, "p", new, t0())
            .await
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn the_offered_session_is_told_once_with_the_context() {
        let pool = pool().await;
        let id = offerable_ticket(&pool, "s1").await;
        let routed = offers::route(&pool, id, t0()).await.unwrap();
        assert_eq!(routed, offers::Routed::Offered(holder("s1")));

        let notices = take_notices(&pool, "s1", HolderKind::Session, "s1", t0() + secs(1))
            .await
            .unwrap();
        let [
            Notice::Offered {
                task,
                note,
                context,
            },
        ] = notices.as_slice()
        else {
            panic!("expected one Offered, got {notices:?}")
        };
        assert_eq!(task.id, id);
        assert!(note.starts_with("routed: owned"), "{note}");
        assert_eq!(context["competent"][0]["holder"]["id"], "s1", "{context}");
        assert!(
            take_notices(&pool, "s1", HolderKind::Session, "s1", t0() + secs(2))
                .await
                .unwrap()
                .is_empty(),
            "delivered once"
        );
    }

    #[tokio::test]
    async fn an_offer_already_claimed_by_someone_else_is_not_delivered() {
        let pool = pool().await;
        let id = offerable_ticket(&pool, "s1").await;
        offers::route(&pool, id, t0()).await.unwrap();
        store::claim_by_id(&pool, id, &holder("s2"), t0() + secs(1))
            .await
            .unwrap();

        let notices = take_notices(&pool, "s1", HolderKind::Session, "s1", t0() + secs(2))
            .await
            .unwrap();
        assert!(
            !notices.iter().any(|n| matches!(n, Notice::Offered { .. })),
            "{notices:?}"
        );
    }

    #[tokio::test]
    async fn a_decline_notifies_the_suggested_session_with_the_message() {
        let pool = pool().await;
        let id = offerable_ticket(&pool, "s1").await;
        touch_session(&pool, "s2", "/wt/s2", "p", t0())
            .await
            .unwrap();
        offers::route(&pool, id, t0()).await.unwrap();

        let routed = offers::decline(
            &pool,
            id,
            &holder("s1"),
            "you wrote this module",
            Some(holder("s2")),
            t0() + secs(1),
        )
        .await
        .unwrap();
        assert_eq!(routed, offers::Routed::Offered(holder("s2")));

        let notices = take_notices(&pool, "s2", HolderKind::Session, "s2", t0() + secs(2))
            .await
            .unwrap();
        let [Notice::Offered { task, note, .. }] = notices.as_slice() else {
            panic!("expected one Offered, got {notices:?}")
        };
        assert_eq!(task.id, id);
        assert!(note.contains("session:s1"), "{note}");
        assert!(note.contains("you wrote this module"), "{note}");
        assert!(
            !take_notices(&pool, "s1", HolderKind::Session, "s1", t0() + secs(2))
                .await
                .unwrap()
                .iter()
                .any(|n| matches!(n, Notice::Offered { .. })),
            "the offer moved off s1"
        );
    }
}
