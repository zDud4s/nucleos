//! Numbered concurrency slots (night-jobs spec §7.1).
//!
//! What bounds how much autonomous work one project — and one machine — may have in flight at once.
//! It replaces two partial unique indexes, `one_open_worktree_run_per_project` (migration 0009) and
//! `one_live_job_per_project` (0042), and the thing it deliberately does NOT replace is where the
//! property lives.
//!
//! Those indexes never gave "one at a time" as such. What they gave is that **the `INSERT` is the
//! lock**: with the constraint in the storage layer, a scheduler tick and a manual `POST` racing for
//! the same project cannot both pass a check and then both proceed. The primary key
//! `(project_id, slot)` does the same work with a number in place of a boolean — claiming is an
//! `INSERT` at slot 0, then 1, up to the ceiling, and whoever loses the race loses it on the key
//! rather than on a read.
//!
//! Count-then-insert in a transaction would also work and is rejected in the spec: it would need a
//! disciplined `BEGIN IMMEDIATE` at every caller, moving the property out of the database and into
//! the correctness of whoever writes the next caller.
//!
//! Judges whether there is room to start; never what to start, or when. The brakes it sits beside
//! answer different questions — `budget.rs` what autonomy costs, `wip.rs` what it costs *you*,
//! `attention.rs` whether you are at the keyboard — and this one only how many at once.

use crate::worktree::Owner;
use sqlx::SqlitePool;

/// How a claim ended.
///
/// The two full states are kept apart because they have different remedies: one waits for this
/// project's own work to finish, the other for anybody's. Collapsing them into a single `Full` would
/// send a user to look at a project that has room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed(i64),
    /// Every slot this PROJECT may hold is taken.
    ProjectFull {
        limit: i64,
    },
    /// The house is at its ceiling across all projects, even though this project has room of its own.
    HouseFull {
        limit: i64,
    },
}

/// The ceiling a project falls back to when its configured one cannot be read.
///
/// One, and not the configured default of two. A concurrency ceiling that cannot be read is a
/// ceiling nobody chose, and failing closed here means fewer in flight rather than more — which
/// lands exactly on the behaviour this module replaces, one at a time. The alternative, restating
/// the migration's `DEFAULT 2` in Rust, would put the real number in two places that can drift.
const UNREADABLE_CEILING: i64 = 1;

/// The effective per-project ceiling: the project's own override when set, otherwise the house
/// default.
///
/// Unlike `wip::wip_limit` this does not return `Option`, because there is no "off". A WIP brake
/// switched off is a defensible configuration; a concurrency ceiling switched off is an unbounded
/// number of cold Rust builds, and it is not a state worth being able to express.
pub async fn slots_limit(pool: &SqlitePool, project_id: &str) -> sqlx::Result<i64> {
    let project_override: Option<Option<i64>> =
        sqlx::query_scalar("SELECT max_concurrent_slots FROM autopilot_state WHERE project_id = ?")
            .bind(project_id)
            .fetch_optional(pool)
            .await?;
    if let Some(Some(limit)) = project_override {
        return Ok(limit.max(1));
    }
    let global: Option<Option<i64>> =
        sqlx::query_scalar("SELECT max_concurrent_slots FROM autopilot_global LIMIT 1")
            .fetch_optional(pool)
            .await?;
    Ok(global.flatten().unwrap_or(UNREADABLE_CEILING).max(1))
}

/// The ceiling across every project at once.
///
/// Both numbers exist because they bound different resources: the per-project one stops a single
/// project from monopolising the machine, this one stops N projects from saturating it together.
pub async fn house_limit(pool: &SqlitePool) -> sqlx::Result<i64> {
    let global: Option<Option<i64>> =
        sqlx::query_scalar("SELECT max_concurrent_total FROM autopilot_global LIMIT 1")
            .fetch_optional(pool)
            .await?;
    Ok(global.flatten().unwrap_or(UNREADABLE_CEILING).max(1))
}

/// How many slots are held right now, everywhere.
pub async fn slots_in_flight(pool: &SqlitePool) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM project_slots")
        .fetch_one(pool)
        .await
}

/// Takes a slot for `owner` in `project_id`, or says why it could not.
///
/// **Idempotent per owner.** An owner that already holds a slot gets the same number back rather
/// than a second one. This is not defensive tidying: the callers are on paths that retry — a job
/// start that failed after claiming and before inserting, a run resumed after a restart — and
/// without this each retry would leak a slot until the next reconciliation.
///
/// **The house check is not atomic with the claim, and does not need to be.** The per-project number
/// is an invariant and is held by the primary key; the house number bounds a machine, and the worst
/// case of two claims crossing is one extra slot for one tick. Do not "fix" this into a transaction:
/// that is the count-then-insert design the spec rejected, and it would put the per-project property
/// back into the caller's discipline.
pub async fn claim(
    pool: &SqlitePool,
    project_id: &str,
    owner: Owner,
) -> sqlx::Result<ClaimOutcome> {
    if let Some(slot) = slot_of(pool, owner).await? {
        return Ok(ClaimOutcome::Claimed(slot));
    }

    let house = house_limit(pool).await?;
    if slots_in_flight(pool).await? >= house {
        return Ok(ClaimOutcome::HouseFull { limit: house });
    }

    let limit = slots_limit(pool, project_id).await?;
    let claimed_at = chrono::Utc::now().to_rfc3339();
    for slot in 0..limit {
        let attempt = sqlx::query(
            "INSERT INTO project_slots (project_id, slot, owner_kind, owner_id, claimed_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(project_id)
        .bind(slot)
        .bind(owner.kind())
        .bind(owner.id())
        .bind(&claimed_at)
        .execute(pool)
        .await;

        match attempt {
            Ok(_) => return Ok(ClaimOutcome::Claimed(slot)),
            // Not an error: this is the race, lost, and the answer is to try the next number. Any
            // other database error is a real one and propagates.
            Err(error) if is_unique_violation(&error) => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(ClaimOutcome::ProjectFull { limit })
}

/// Gives the slot back. Idempotent — releasing what was never claimed is not an error, because the
/// endings that call this are many and some of them can run twice.
pub async fn release(pool: &SqlitePool, owner: Owner) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM project_slots WHERE owner_kind = ? AND owner_id = ?")
        .bind(owner.kind())
        .bind(owner.id())
        .execute(pool)
        .await?;
    Ok(())
}

/// Which slot an owner holds, if any.
pub async fn slot_of(pool: &SqlitePool, owner: Owner) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar("SELECT slot FROM project_slots WHERE owner_kind = ? AND owner_id = ?")
        .bind(owner.kind())
        .bind(owner.id())
        .fetch_optional(pool)
        .await
}

/// The statuses that mean a run still holds its slot. Mirrors what
/// `one_open_worktree_run_per_project` covered, which is what this replaces.
///
/// Test-only, and the asymmetry with `job::LIVE_STATUSES` is worth stating rather than hiding: that
/// one is production truth, read by `live_jobs` and by `cancel`, so a guard against it catches drift
/// anywhere. There is no such constant for runs — every site spells the pair into its own SQL — so
/// this catches an edit to the sweep below and nothing wider. Making it `pub` to look symmetric
/// would claim a guarantee it does not give.
#[cfg(test)]
const LIVE_RUN_STATUSES: [&str; 2] = ["running", "awaiting_approval"];

/// Frees slots whose owner is no longer live, and reports how many.
///
/// This is the compensation for the trade §7.4 of the spec names. Under the old index a stranded run
/// blocked its whole project, which is catastrophic and impossible to miss. Under slots it merely
/// consumes one — less catastrophic, more silent, and therefore easier to live with for weeks
/// without noticing that a project's effective ceiling has quietly become one.
///
/// Runs at startup AFTER `runs::reconcile_orphaned_runs` and `job::reconcile_orphaned_jobs`, for the
/// same reason the worktree sweep does: before those, a dead owner's row still reads live, and this
/// pass would leave its slot held forever.
pub async fn reconcile_orphaned_slots(pool: &SqlitePool) -> sqlx::Result<u64> {
    let swept = sqlx::query(ORPHANED_SLOTS_SQL).execute(pool).await?;
    Ok(swept.rows_affected())
}

/// Spelled out rather than assembled from `LIVE_RUN_STATUSES` and `job::LIVE_STATUSES`, because sqlx
/// refuses SQL built at runtime — the same trade `LIVE_JOBS_SQL` makes, and with the same guard:
/// `every_live_status_is_a_status_the_sweep_spares` compares this text against both constants, so
/// they cannot drift apart in silence.
///
/// An owner kind this does not know is left alone on purpose. Deleting it would free a slot that
/// something may still be working in, and over-concurrency is the one failure this table exists to
/// prevent; a leaked slot is visible in `project_slots` and costs a lower ceiling. Adding a third
/// owner kind means teaching this pass, and `Owner` being an enum is what makes that a compile-time
/// conversation rather than a silent one.
const ORPHANED_SLOTS_SQL: &str = "DELETE FROM project_slots
     WHERE (owner_kind = 'run'
            AND NOT EXISTS (SELECT 1 FROM runs
                            WHERE runs.id = project_slots.owner_id
                              AND runs.status IN ('running','awaiting_approval')))
        OR (owner_kind = 'job'
            AND NOT EXISTS (SELECT 1 FROM jobs
                            WHERE jobs.id = project_slots.owner_id
                              AND jobs.status IN ('planning','implementing','gating','reviewing',
                                                  'awaiting_approval','waiting')))";

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|database_error| database_error.is_unique_violation())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn set_limits(pool: &SqlitePool, per_project: i64, house: i64) {
        sqlx::query(
            "UPDATE autopilot_global SET max_concurrent_slots = ?, max_concurrent_total = ?",
        )
        .bind(per_project)
        .bind(house)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_run(pool: &SqlitePool, project_id: &str, status: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES (?, 'a prompt', ?, 'worktree', '2026-08-08T00:00:00Z')",
        )
        .bind(project_id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn seed_job(pool: &SqlitePool, project_id: &str, status: &str) -> i64 {
        sqlx::query(
            "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
             VALUES (?, 'C:/somewhere', ?, 5, '2026-08-08T00:00:00Z')",
        )
        .bind(project_id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// The numbers start at zero and go up, which is the whole mechanism: the second claimant does
    /// not fail, it takes the next number.
    #[tokio::test]
    async fn slots_are_handed_out_in_order_from_zero() {
        let pool = test_pool().await;
        set_limits(&pool, 2, 9).await;

        assert_eq!(
            claim(&pool, "project-a", Owner::Run(1)).await.unwrap(),
            ClaimOutcome::Claimed(0)
        );
        assert_eq!(
            claim(&pool, "project-a", Owner::Run(2)).await.unwrap(),
            ClaimOutcome::Claimed(1)
        );
    }

    /// A full project is a refusal, not an error. The caller turns this into the same 409 the old
    /// unique index produced, and an `Err` here would become a 500 instead.
    #[tokio::test]
    async fn a_project_at_its_ceiling_is_refused_rather_than_failing() {
        let pool = test_pool().await;
        set_limits(&pool, 2, 9).await;
        claim(&pool, "project-a", Owner::Run(1)).await.unwrap();
        claim(&pool, "project-a", Owner::Run(2)).await.unwrap();

        assert_eq!(
            claim(&pool, "project-a", Owner::Run(3)).await.unwrap(),
            ClaimOutcome::ProjectFull { limit: 2 }
        );
    }

    /// Numbers are reused. Without this a long-lived project reaches its ceiling having never had
    /// two things in flight at once — the count would be of everything it had ever run.
    #[tokio::test]
    async fn a_released_number_is_handed_out_again() {
        let pool = test_pool().await;
        set_limits(&pool, 2, 9).await;
        claim(&pool, "project-a", Owner::Run(1)).await.unwrap();
        claim(&pool, "project-a", Owner::Run(2)).await.unwrap();

        release(&pool, Owner::Run(1)).await.unwrap();

        assert_eq!(
            claim(&pool, "project-a", Owner::Run(3)).await.unwrap(),
            ClaimOutcome::Claimed(0)
        );
    }

    /// The two ceilings answer different questions, so a project with room of its own can still be
    /// refused — and is told which wall it hit.
    #[tokio::test]
    async fn the_house_ceiling_refuses_a_project_that_still_has_room() {
        let pool = test_pool().await;
        set_limits(&pool, 5, 2).await;
        claim(&pool, "project-a", Owner::Run(1)).await.unwrap();
        claim(&pool, "project-b", Owner::Run(2)).await.unwrap();

        assert_eq!(
            claim(&pool, "project-a", Owner::Run(3)).await.unwrap(),
            ClaimOutcome::HouseFull { limit: 2 }
        );
    }

    /// Runs and jobs share the table on purpose. They consume the same machine and the same quota,
    /// and a ceiling that counted only one of the two kinds is not a ceiling.
    #[tokio::test]
    async fn a_job_and_a_run_of_one_project_take_different_slots() {
        let pool = test_pool().await;
        set_limits(&pool, 2, 9).await;

        assert_eq!(
            claim(&pool, "project-a", Owner::Job(1)).await.unwrap(),
            ClaimOutcome::Claimed(0)
        );
        assert_eq!(
            claim(&pool, "project-a", Owner::Run(1)).await.unwrap(),
            ClaimOutcome::Claimed(1)
        );
    }

    /// A job id and a run id come from different sequences and collide constantly. Keying releases
    /// on the id alone would have one kind free the other's slot.
    #[tokio::test]
    async fn releasing_a_run_leaves_the_job_of_the_same_number_alone() {
        let pool = test_pool().await;
        set_limits(&pool, 2, 9).await;
        claim(&pool, "project-a", Owner::Job(7)).await.unwrap();
        claim(&pool, "project-a", Owner::Run(7)).await.unwrap();

        release(&pool, Owner::Run(7)).await.unwrap();

        assert_eq!(slot_of(&pool, Owner::Job(7)).await.unwrap(), Some(0));
        assert_eq!(slot_of(&pool, Owner::Run(7)).await.unwrap(), None);
    }

    /// Claiming twice for the same owner returns the slot it already has. The callers retry, and
    /// without this each retry would leak a number until the next restart.
    #[tokio::test]
    async fn claiming_twice_for_one_owner_gives_back_the_same_slot() {
        let pool = test_pool().await;
        set_limits(&pool, 3, 9).await;
        claim(&pool, "project-a", Owner::Job(1)).await.unwrap();

        assert_eq!(
            claim(&pool, "project-a", Owner::Job(1)).await.unwrap(),
            ClaimOutcome::Claimed(0)
        );
        assert_eq!(slots_in_flight(&pool).await.unwrap(), 1);
    }

    /// Releasing something that holds nothing is not an error: the endings that call it are many,
    /// and some of them can run twice.
    #[tokio::test]
    async fn releasing_a_slot_nobody_holds_is_not_an_error() {
        let pool = test_pool().await;

        release(&pool, Owner::Run(404)).await.unwrap();

        assert_eq!(slots_in_flight(&pool).await.unwrap(), 0);
    }

    /// The sweep frees what died and spares what did not — the whole of it, because a sweep that
    /// took a live owner's slot would put two workers in one number.
    #[tokio::test]
    async fn the_sweep_frees_dead_owners_and_spares_live_ones() {
        let pool = test_pool().await;
        set_limits(&pool, 5, 9).await;
        let live_run = seed_run(&pool, "project-a", "running").await;
        let dead_run = seed_run(&pool, "project-a", "interrupted").await;
        let live_job = seed_job(&pool, "project-a", "implementing").await;
        let dead_job = seed_job(&pool, "project-b", "completed").await;
        for owner in [
            Owner::Run(live_run),
            Owner::Run(dead_run),
            Owner::Job(live_job),
            Owner::Job(dead_job),
        ] {
            let project = if owner == Owner::Job(dead_job) {
                "project-b"
            } else {
                "project-a"
            };
            claim(&pool, project, owner).await.unwrap();
        }

        assert_eq!(reconcile_orphaned_slots(&pool).await.unwrap(), 2);

        assert!(
            slot_of(&pool, Owner::Run(live_run))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            slot_of(&pool, Owner::Job(live_job))
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(slot_of(&pool, Owner::Run(dead_run)).await.unwrap(), None);
        assert_eq!(slot_of(&pool, Owner::Job(dead_job)).await.unwrap(), None);
    }

    /// A slot whose owner row is gone entirely is swept too. A deleted run cannot be checked for
    /// liveness, and leaving it would hold a number nothing will ever release.
    #[tokio::test]
    async fn a_slot_whose_owner_no_longer_exists_is_swept() {
        let pool = test_pool().await;
        set_limits(&pool, 5, 9).await;
        claim(&pool, "project-a", Owner::Run(9_999)).await.unwrap();

        assert_eq!(reconcile_orphaned_slots(&pool).await.unwrap(), 1);
    }

    /// The guard on the hand-written SQL: every status either constant calls live must appear in the
    /// sweep's spare-list, or a restart would free a slot out from under something still working.
    #[test]
    fn every_live_status_is_a_status_the_sweep_spares() {
        for status in LIVE_RUN_STATUSES {
            assert!(
                ORPHANED_SLOTS_SQL.contains(&format!("'{status}'")),
                "the sweep does not spare live run status `{status}`"
            );
        }
        for status in crate::job::LIVE_STATUSES {
            assert!(
                ORPHANED_SLOTS_SQL.contains(&format!("'{status}'")),
                "the sweep does not spare live job status `{status}`"
            );
        }
    }

    /// An unreadable ceiling is one, not two. Failing closed for a concurrency limit means fewer in
    /// flight, and one is exactly the behaviour this module replaces.
    #[tokio::test]
    async fn a_ceiling_that_cannot_be_read_is_one() {
        let pool = test_pool().await;
        sqlx::query("UPDATE autopilot_global SET max_concurrent_slots = NULL")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(slots_limit(&pool, "project-a").await.unwrap(), 1);
    }

    /// A project override beats the house default, which is the only reason it exists.
    #[tokio::test]
    async fn a_projects_own_ceiling_beats_the_default() {
        let pool = test_pool().await;
        set_limits(&pool, 2, 9).await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, max_concurrent_slots)
             VALUES ('project-a', 'off', 4)",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(slots_limit(&pool, "project-a").await.unwrap(), 4);
        assert_eq!(slots_limit(&pool, "project-b").await.unwrap(), 2);
    }

    /// The primary key is the lock. Stated as a test because everything else in this module rests on
    /// it: if `(project_id, slot)` stopped being unique, `claim` would hand the same number to two
    /// owners and every ceiling above would be decorative.
    #[tokio::test]
    async fn the_table_is_keyed_on_the_project_and_the_number() {
        let pool = test_pool().await;
        let sql: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = 'project_slots'")
                .fetch_one(&pool)
                .await
                .unwrap();

        assert!(
            sql.contains("PRIMARY KEY (project_id, slot)"),
            "project_slots is not keyed on (project_id, slot): {sql}"
        );
    }
}
