//! Interactive sessions and the notices they are owed (spec task-claims section 2).
//!
//! The hooks send `(session_id, cwd)` on every PreToolUse and PostToolUse. `touch_session` registers it
//! and renews the lease of every claim the session still holds; `take_notices` answers "what happened
//! to my claims since I last asked" from the event log, advancing a per-session cursor so each fact is
//! delivered once.

use super::store::{green_sha, row_by_id};
use super::{HolderKind, Notice, Status, SESSION_LEASE_SECONDS};
use crate::execution::wave::stamp;
use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;

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
pub async fn session_seen_in(pool: &SqlitePool, session_id: &str, worktree: &str) -> sqlx::Result<bool> {
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
/// The "an incident I inherited in a verify ticket was closed" notice depends on verify tickets and is
/// added by the verify phase; this function covers what the event log alone can say.
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
        let Some(task) = row_by_id(&mut tx, task_id).await? else { continue };
        if event == "done" {
            if let Some(sha) = green_sha(&note) {
                notices.push(Notice::IncidentClosed { task, sha: Some(sha) });
            }
        } else if task.status == Status::Claimed
            && let Some(new_holder) = task.holder()
            && !(new_holder.kind == HolderKind::Session && new_holder.id == session_id)
        {
            notices.push(Notice::ClaimLost { task, new_holder });
        }
    }
    tx.commit().await?;
    Ok(notices)
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
        Holder { kind: HolderKind::Session, id: id.into(), worktree: format!("/wt/{id}") }
    }

    fn task(key: &str) -> NewTask {
        NewTask {
            key: crate::task_claims::keys::normalize(key, std::path::Path::new("/repo")).unwrap(),
            kind: TaskKind::Declared,
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
        let expired = claimed(&pool, "file:c.rs", &me, t0() - secs(SESSION_LEASE_SECONDS + 5)).await;

        touch_session(&pool, "s1", "/wt/s1", "p", t0() + secs(600)).await.unwrap();

        let lease = |id: i64| {
            let pool = pool.clone();
            async move {
                let mut conn = pool.acquire().await.unwrap();
                store::row_by_id(&mut conn, id).await.unwrap().unwrap().lease_until
            }
        };
        assert_eq!(lease(live).await, Some(stamp(t0() + secs(600 + SESSION_LEASE_SECONDS))));
        assert_eq!(lease(other).await, Some(stamp(t0() + secs(SESSION_LEASE_SECONDS))), "another session's claim is untouched");
        assert_eq!(lease(expired).await, Some(stamp(t0() - secs(5))), "a lapsed lease is not revived");
        assert!(session_seen_in(&pool, "s1", "/wt/s1").await.unwrap());
        assert!(!session_seen_in(&pool, "s1", "/wt/elsewhere").await.unwrap(), "only the worktree it was seen in");
        assert!(!session_seen_in(&pool, "ghost", "/wt/s1").await.unwrap());
    }

    #[tokio::test]
    async fn a_session_is_told_once_that_its_claim_was_taken() {
        let pool = pool().await;
        let me = holder("s1");
        touch_session(&pool, "s1", "/wt/s1", "p", t0()).await.unwrap();
        claimed(&pool, "file:core/a.rs", &me, t0()).await;
        let late = t0() + secs(SESSION_LEASE_SECONDS + 1);
        claimed(&pool, "file:core/a.rs", &holder("s2"), late).await;

        let notices = take_notices(&pool, "s1", late).await.unwrap();
        let [Notice::ClaimLost { task, new_holder }] = notices.as_slice() else {
            panic!("expected one ClaimLost, got {notices:?}")
        };
        assert_eq!(task.key.as_str(), "file:core/a.rs");
        assert_eq!(new_holder.id, "s2");
        assert!(take_notices(&pool, "s1", late).await.unwrap().is_empty(), "the cursor advanced");
    }

    #[tokio::test]
    async fn a_session_that_took_the_claim_back_hears_nothing() {
        let pool = pool().await;
        let me = holder("s1");
        touch_session(&pool, "s1", "/wt/s1", "p", t0()).await.unwrap();
        claimed(&pool, "file:core/a.rs", &me, t0()).await;
        let late = t0() + secs(SESSION_LEASE_SECONDS + 1);
        claimed(&pool, "file:core/a.rs", &holder("s2"), late).await;
        sqlx::query("UPDATE task_claims SET lease_until = ?").bind(stamp(late - secs(1))).execute(&pool).await.unwrap();
        claimed(&pool, "file:core/a.rs", &me, late + secs(1)).await;
        assert!(take_notices(&pool, "s1", late).await.unwrap().is_empty(), "it holds the task again");
    }

    #[tokio::test]
    async fn the_owner_is_told_when_the_incident_closes_by_green_but_not_for_its_own_done() {
        let pool = pool().await;
        let me = holder("s1");
        touch_session(&pool, "s1", "/wt/s1", "p", t0()).await.unwrap();
        let red = claimed(&pool, "master-red", &me, t0()).await;
        done(&pool, red, Closer::Green { sha: "abc123".into() }, "revert", t0() + secs(5)).await.unwrap();
        let own = claimed(&pool, "file:own.rs", &me, t0()).await;
        done(&pool, own, Closer::Holder(&me), "fixed", t0() + secs(6)).await.unwrap();

        let notices = take_notices(&pool, "s1", t0() + secs(7)).await.unwrap();
        let [Notice::IncidentClosed { task, sha }] = notices.as_slice() else {
            panic!("expected one IncidentClosed, got {notices:?}")
        };
        assert_eq!((task.id, task.status, sha.as_deref()), (red, Status::Done, Some("abc123")));
    }

    #[tokio::test]
    async fn a_new_session_does_not_replay_history_and_an_unknown_one_gets_nothing() {
        let pool = pool().await;
        claimed(&pool, "file:old.rs", &holder("s1"), t0()).await;
        claimed(&pool, "file:old.rs", &holder("s2"), t0() + secs(SESSION_LEASE_SECONDS + 1)).await;
        // s1 shows up only now: the events above happened before it was registered.
        touch_session(&pool, "s1", "/wt/s1", "p", t0() + secs(SESSION_LEASE_SECONDS + 2)).await.unwrap();
        assert!(take_notices(&pool, "s1", t0()).await.unwrap().is_empty());
        assert!(take_notices(&pool, "never-seen", t0()).await.unwrap().is_empty());
        let _ = TaskKey::master_red(); // keeps the import honest if the fixtures above change
    }
}
