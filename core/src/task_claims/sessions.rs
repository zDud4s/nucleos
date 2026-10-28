//! Interactive sessions and the notices they are owed (spec task-claims section 2).
//!
//! The hooks send `(session_id, cwd)` on every PreToolUse and PostToolUse. `touch_session` registers it
//! and renews the lease of every claim the session still holds; `take_notices` answers "what happened
//! to my claims since I last asked" from the event log, advancing a per-session cursor so each fact is
//! delivered once.

use super::store::{green_sha, row_by_id};
use super::{HolderKind, Notice, SESSION_LEASE_SECONDS, Status};
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

/// What happened to the session's claims since its cursor, and the cursor moves past it.
///
/// - `ClaimLost`: an `expired_reclaimed` / `taken_over` event names this session as the PREVIOUS owner,
///   and the task is claimed now by somebody else.
/// - `IncidentClosed`: a `done` event closed by green (`green <sha>` note) names this session as the
///   owner. The session's own `task_done` has no such note and says nothing.
///
/// - `InheritedClosed`: a `done` event for an incident that this session's worktree inherited in a
///   verify ticket (`verify_requests.inherited_task_ids`); see `inherited_notices`.
pub async fn take_notices(
    pool: &SqlitePool,
    session_id: &str,
    _now: DateTime<Utc>,
) -> sqlx::Result<Vec<Notice>> {
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
         WHERE id > ? AND id <= ? AND holder_kind = 'session' AND holder_id = ?
           AND event IN ('taken_over', 'expired_reclaimed', 'done')
         ORDER BY id",
    )
    .bind(cursor)
    .bind(newest)
    .bind(session_id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("UPDATE task_sessions SET notice_cursor = ? WHERE session_id = ?")
        .bind(newest)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;

    let mut notices = Vec::new();
    for (task_id, event, note) in events {
        let Some(task) = row_by_id(&mut tx, task_id).await? else {
            continue;
        };
        if event == "done" {
            if let Some(sha) = green_sha(&note) {
                notices.push(Notice::IncidentClosed {
                    task,
                    sha: Some(sha),
                });
            }
        } else if task.status == Status::Claimed
            && let Some(new_holder) = task.holder()
            && !(new_holder.kind == HolderKind::Session && new_holder.id == session_id)
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
    inherited_notices(&mut tx, session_id, cursor, newest, &mut notices).await?;
    tx.commit().await?;
    Ok(notices)
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
    for (task_id, holder_kind, holder_id) in closed {
        let held_it =
            holder_kind.as_deref() == Some("session") && holder_id.as_deref() == Some(session_id);
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

#[cfg(test)]
mod tests {
    use super::*;
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

        let notices = take_notices(&pool, "s1", late).await.unwrap();
        let [Notice::ClaimLost { task, new_holder }] = notices.as_slice() else {
            panic!("expected one ClaimLost, got {notices:?}")
        };
        assert_eq!(task.key.as_str(), "file:core/a.rs");
        assert_eq!(new_holder.id, "s2");
        assert!(
            take_notices(&pool, "s1", late).await.unwrap().is_empty(),
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

        let notices = take_notices(&pool, "s1", t3).await.unwrap();
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
            take_notices(&pool, "s1", late).await.unwrap().is_empty(),
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

        let notices = take_notices(&pool, "s1", t0() + secs(7)).await.unwrap();
        let [Notice::IncidentClosed { task, sha }] = notices.as_slice() else {
            panic!("expected one IncidentClosed, got {notices:?}")
        };
        assert_eq!(
            (task.id, task.status, sha.as_deref()),
            (red, Status::Done, Some("abc123"))
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
        assert!(take_notices(&pool, "s1", t0()).await.unwrap().is_empty());
        assert!(
            take_notices(&pool, "never-seen", t0())
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

        let s1 = take_notices(&pool, "s1", t0() + secs(6)).await.unwrap();
        let [Notice::InheritedClosed { task, sha }] = s1.as_slice() else {
            panic!("expected one InheritedClosed, got {s1:?}")
        };
        assert_eq!((task.id, sha.as_deref()), (red, Some("abc123")));
        assert!(
            take_notices(&pool, "s1", t0() + secs(7))
                .await
                .unwrap()
                .is_empty(),
            "the cursor advanced"
        );
        let s2 = take_notices(&pool, "s2", t0() + secs(6)).await.unwrap();
        assert!(
            matches!(s2.as_slice(), [Notice::InheritedClosed { .. }]),
            "{s2:?}"
        );
        assert!(
            take_notices(&pool, "s3", t0() + secs(6))
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

        let notices = take_notices(&pool, "s1", t0() + secs(6)).await.unwrap();
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

        let live_notices = take_notices(&pool, "s-live", t0() + secs(6)).await.unwrap();
        assert!(
            matches!(live_notices.as_slice(), [Notice::InheritedClosed { .. }]),
            "{live_notices:?}"
        );
        assert!(
            take_notices(&pool, "s-gone", t0() + secs(6))
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
}
