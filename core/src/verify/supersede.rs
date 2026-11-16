//! Retiring queued units that a newer request of the same caller has made pointless.
//!
//! A job or a run that pushes a new HEAD asks again for the same groups. The units of its earlier
//! request that are still waiting in the queue would measure a tree nobody reads any more, and with
//! a saturated queue each of them delays everyone else. They are ended as `superseded` before they
//! ever take a slot. Running units are left alone: they finish and nobody is hurt.

use sqlx::SqlitePool;

use crate::verify::runs::{ORIGIN_VERIFY, STATUS_QUEUED, STATUS_SUPERSEDED};
use crate::verify::store::PlannedUnit;

/// Whether `caller` (a `verify_requests.caller` label) names one job or one run, the only
/// identities that own their older tickets. The owner label is shared by every session, a merge
/// gate belongs to the queue, and the post-merge family and probes exist to measure again.
fn supersedes_own_tickets(caller: &str) -> bool {
    caller.starts_with("job:") || caller.starts_with("run:")
}

/// Ends the still-`queued` units of `caller`'s older requests on `(project, worktree, kind)` whose
/// group `request_id` now asks for at a different fingerprint. Returns how many it ended.
///
/// A unit is never touched when
/// - it is not `queued` (the claim is an `UPDATE` on the same condition, so a unit is either
///   claimed or superseded, never both);
/// - another request's plan also points at it (it was joined: that caller still needs it);
/// - its fingerprint equals the new unit's (the same content, nothing is stale);
/// - it was asked for by the post-merge family or a probe.
///
/// Call it after the new request's plan is stored, so a unit the new request itself joined is seen
/// as shared.
pub async fn supersede_older(
    pool: &SqlitePool,
    request_id: i64,
    project_id: &str,
    worktree: &str,
    kind: &str,
    caller: &str,
    units: &[PlannedUnit],
) -> sqlx::Result<u64> {
    if !supersedes_own_tickets(caller) {
        return Ok(0);
    }
    let reason = format!("superseded by request #{request_id}");
    let mut ended = 0;
    for unit in units.iter().filter(|unit| unit.skipped.is_none()) {
        let (Some(group), Some(fingerprint)) = (&unit.group, &unit.fingerprint) else {
            continue;
        };
        ended += sqlx::query(
            "UPDATE verify_runs SET status = ?, finished_at = ?, output_tail = ? \
             WHERE status = ? AND origin = ? AND project_id = ? AND worktree = ? AND kind = ? \
             AND group_name = ? AND fingerprint IS NOT NULL AND fingerprint <> ? \
             AND requested_by NOT IN ('postgate', 'flake-check', 'bisect', 'probe') \
             AND origin_id IN (SELECT q.id FROM verify_requests q WHERE q.id < ? AND q.caller = ?) \
             AND NOT EXISTS (SELECT 1 FROM verify_requests o, json_each(o.plan) j \
                             WHERE o.worktree = ? AND o.id > verify_runs.origin_id \
                             AND json_extract(j.value, '$.run_id') = verify_runs.id)",
        )
        .bind(STATUS_SUPERSEDED)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&reason)
        .bind(STATUS_QUEUED)
        .bind(ORIGIN_VERIFY)
        .bind(project_id)
        .bind(worktree)
        .bind(kind)
        .bind(group)
        .bind(fingerprint)
        .bind(request_id)
        .bind(caller)
        .bind(worktree)
        .execute(pool)
        .await?
        .rows_affected();
    }
    Ok(ended)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::runs::{self as runs, Request, STATUS_PASSED, STATUS_RUNNING};
    use crate::verify::store;

    fn planned(group: &str, fingerprint: &str, run_id: Option<i64>) -> PlannedUnit {
        PlannedUnit {
            group: Some(group.to_owned()),
            argv: vec!["run".to_owned(), group.to_owned()],
            why: "paths: a".to_owned(),
            fingerprint: Some(fingerprint.to_owned()),
            cacheable: true,
            run_id,
            cached_from: None,
            skipped: None,
        }
    }

    async fn newer(pool: &SqlitePool, caller: &str) -> i64 {
        store::insert_request(
            pool,
            &store::NewRequest {
                project_id: "p",
                worktree: "/wt",
                kind: "test",
                scope: "scope",
                base: None,
                priority: 2,
                caller,
                note: None,
                unclaimed: &[],
            },
        )
        .await
        .unwrap()
    }

    /// A request row plus one queued unit of `group` at `fingerprint`, tied to it as its plan says.
    async fn seed(
        pool: &SqlitePool,
        caller: &str,
        requested_by: &str,
        group: &str,
        fingerprint: &str,
    ) -> (i64, i64) {
        let request = newer(pool, caller).await;
        let unit = runs::enqueue(
            pool,
            &Request {
                project_id: Some("p".to_owned()),
                worktree: "/wt".to_owned(),
                scope: "scope".to_owned(),
                origin: ORIGIN_VERIFY.to_owned(),
                origin_id: Some(request),
                requested_by: requested_by.to_owned(),
                group_name: Some(group.to_owned()),
                kind: Some("test".to_owned()),
                argv: vec!["run".to_owned(), group.to_owned()],
                fingerprint: Some(fingerprint.to_owned()),
                priority: 2,
                weight: 1,
                timeout_ms: 1000,
            },
            100,
        )
        .await
        .unwrap()
        .id;
        store::set_plan(pool, request, &[planned(group, fingerprint, Some(unit))])
            .await
            .unwrap();
        (request, unit)
    }

    async fn status(pool: &SqlitePool, unit: i64) -> String {
        runs::get(pool, unit).await.unwrap().unwrap().status
    }

    #[tokio::test]
    async fn a_newer_head_of_the_same_job_supersedes_its_queued_unit_with_a_reason() {
        let pool = crate::testdb::fresh_pool().await;
        let (_, old) = seed(&pool, "job:1", "scope", "core", "fp-old").await;
        let request = newer(&pool, "job:1").await;
        let units = [planned("core", "fp-new", Some(999))];
        let ended = supersede_older(&pool, request, "p", "/wt", "test", "job:1", &units)
            .await
            .unwrap();
        assert_eq!(ended, 1);
        let state = runs::get(&pool, old).await.unwrap().unwrap();
        assert_eq!(state.status, STATUS_SUPERSEDED);
        assert_eq!(
            state.output_tail.as_deref(),
            Some(format!("superseded by request #{request}").as_str())
        );
    }

    #[tokio::test]
    async fn a_running_unit_is_left_to_finish() {
        let pool = crate::testdb::fresh_pool().await;
        let (_, old) = seed(&pool, "run:4", "scope", "core", "fp-old").await;
        runs::claim(&pool, old).await.unwrap().unwrap();
        let request = newer(&pool, "run:4").await;
        let units = [planned("core", "fp-new", Some(999))];
        let ended = supersede_older(&pool, request, "p", "/wt", "test", "run:4", &units)
            .await
            .unwrap();
        assert_eq!(ended, 0);
        assert_eq!(status(&pool, old).await, STATUS_RUNNING);
        runs::finish(&pool, old, STATUS_PASSED, Some(0), 1, None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn another_callers_ticket_the_owner_and_the_post_merge_family_are_never_cancelled() {
        let pool = crate::testdb::fresh_pool().await;
        let (_, other_job) = seed(&pool, "job:2", "scope", "core", "fp-old").await;
        let (_, owner) = seed(&pool, "owner", "scope", "hooks", "fp-old").await;
        let (_, postgate) = seed(&pool, "postgate", "postgate", "shell", "fp-old").await;
        let (_, probe) = seed(&pool, "probe", "probe", "tauri", "fp-old").await;
        let units = ["core", "hooks", "shell", "tauri"].map(|g| planned(g, "fp-new", Some(999)));
        let request = newer(&pool, "job:1").await;
        let ended = supersede_older(&pool, request, "p", "/wt", "test", "job:1", &units)
            .await
            .unwrap();
        assert_eq!(ended, 0);
        // The owner label is shared by every session: even an owner asking again keeps them.
        let owner_request = newer(&pool, "owner").await;
        let ended = supersede_older(&pool, owner_request, "p", "/wt", "test", "owner", &units)
            .await
            .unwrap();
        assert_eq!(ended, 0);
        for unit in [other_job, owner, postgate, probe] {
            assert_eq!(status(&pool, unit).await, STATUS_QUEUED);
        }
    }

    #[tokio::test]
    async fn the_same_fingerprint_and_a_unit_another_request_joined_are_kept() {
        let pool = crate::testdb::fresh_pool().await;
        let (_, same) = seed(&pool, "job:1", "scope", "core", "fp-same").await;
        let (_, joined) = seed(&pool, "job:1", "scope", "hooks", "fp-old").await;
        // Another request's plan points at the second unit: it joined it, and still needs it.
        let sharer = newer(&pool, "job:9").await;
        store::set_plan(&pool, sharer, &[planned("hooks", "fp-old", Some(joined))])
            .await
            .unwrap();
        let request = newer(&pool, "job:1").await;
        let units = [
            planned("core", "fp-same", Some(same)),
            planned("hooks", "fp-new", Some(999)),
        ];
        let ended = supersede_older(&pool, request, "p", "/wt", "test", "job:1", &units)
            .await
            .unwrap();
        assert_eq!(ended, 0);
        assert_eq!(status(&pool, same).await, STATUS_QUEUED);
        assert_eq!(status(&pool, joined).await, STATUS_QUEUED);
    }

    #[tokio::test]
    async fn the_ticket_of_a_superseded_unit_is_done_and_neither_passed_nor_failed() {
        let pool = crate::testdb::fresh_pool().await;
        let (old_request, _) = seed(&pool, "job:1", "scope", "core", "fp-old").await;
        let request = newer(&pool, "job:1").await;
        let units = [planned("core", "fp-new", Some(999))];
        supersede_older(&pool, request, "p", "/wt", "test", "job:1", &units)
            .await
            .unwrap();
        // A waiter on the old ticket gets an answer at once instead of waiting for a unit that
        // will never run.
        let (ticket, _) = crate::verify::wait_ticket_with(
            &pool,
            None,
            old_request,
            std::time::Duration::from_secs(5),
            false,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(ticket.done);
        assert_eq!(ticket.verdict.as_deref(), Some("superseded"));
        assert_eq!(ticket.units[0].status, STATUS_SUPERSEDED);
        assert_eq!(
            ticket.units[0].skipped_reason.as_deref(),
            Some(format!("superseded by request #{request}").as_str())
        );
    }
}
