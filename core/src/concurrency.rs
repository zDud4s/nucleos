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

/// Why there was no slot.
///
/// The two are kept apart because they have different remedies: one waits for this project's own
/// work to finish, the other for anybody's. Collapsing them into a single "full" would send someone
/// to look at a project that has room.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoRoom {
    /// Every slot this PROJECT may hold is taken.
    Project { limit: i64 },
    /// The house is at its ceiling across all projects, even though this project has room of its own.
    House { limit: i64 },
}

impl NoRoom {
    /// What to tell whoever asked. One sentence, and it names which wall was hit.
    pub fn reason(self) -> String {
        match self {
            Self::Project { limit } => {
                format!("this project already has {limit} piece(s) of work in flight")
            }
            Self::House { limit } => {
                format!("the machine already has {limit} piece(s) of work in flight")
            }
        }
    }
}

/// How a claim ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed(i64),
    Full(NoRoom),
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

    if let Some(full) = room_for(pool, project_id).await? {
        return Ok(ClaimOutcome::Full(full));
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
    Ok(ClaimOutcome::Full(NoRoom::Project { limit }))
}

/// Whether a claim would find room, without taking anything.
///
/// **Advisory. `claim` is the only authoritative answer**, and a caller that treats this as the
/// decision has reintroduced count-then-insert — two askers can both read room and both proceed.
///
/// It exists because the authoritative answer costs a row. A job has to be inserted before it can
/// claim (a slot is keyed on its owner's id), so a refused job leaves a retired row behind; a
/// scheduler firing every half hour against a full project would mint one of those every time, and
/// `/jobs` would fill with jobs that never began. Asking first turns that into the rare case where
/// two starts actually crossed.
pub async fn room_for(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Option<NoRoom>> {
    let house = house_limit(pool).await?;
    if slots_in_flight(pool).await? >= house {
        return Ok(Some(NoRoom::House { limit: house }));
    }
    let limit = slots_limit(pool, project_id).await?;
    let held: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_slots WHERE project_id = ?")
        .bind(project_id)
        .fetch_one(pool)
        .await?;
    if held >= limit {
        return Ok(Some(NoRoom::Project { limit }));
    }
    Ok(None)
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
/// It was `#[cfg(test)]` until the live listing existed, and the comment of the time said that
/// making it public would *"claim a guarantee it does not give"* — because no production path read
/// it, every site spelling the pair into its own SQL. `runs::search` reads it now, and can do so
/// because it builds its query with `QueryBuilder`, which is runtime assembly and therefore does not
/// hit the sqlx refusal that forces the sweep below to be written out by hand. The guarantee is
/// real: an edit to this list moves the filter and the sweep at once.
pub const LIVE_RUN_STATUSES: [&str; 2] = ["running", "awaiting_approval"];

/// The ceiling of a listing filtered to live work, shared by both of them.
///
/// It is not "bounded by construction", and that is why it carries a number at all: the house check
/// above is declaredly advisory and non-atomic — *"the worst case of two claims crossing is one
/// extra slot for one tick"* — so the count of live things is small in practice without being a hard
/// invariant. 200 leaves two orders of magnitude of slack over the house ceiling and still bounds
/// the response.
pub const LIVE_LIST_LIMIT: i64 = 200;

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

/// One taken slot, with its owner.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct HeldSlot {
    pub project_id: String,
    pub slot: i64,
    /// `'run'` or `'job'`. A `String` and not `Owner`, because this leaves over JSON and the
    /// consumer is an interface that only wants to know which tab to link to.
    pub owner_kind: String,
    pub owner_id: i64,
    pub claimed_at: String,
}

/// Every slot taken right now, in project and number order.
pub async fn held_slots(pool: &SqlitePool) -> sqlx::Result<Vec<HeldSlot>> {
    sqlx::query_as(
        "SELECT project_id, slot, owner_kind, owner_id, claimed_at
         FROM project_slots ORDER BY project_id, slot",
    )
    .fetch_all(pool)
    .await
}

#[derive(Debug, serde::Serialize)]
pub struct HouseReadout {
    pub limit: i64,
    pub held: i64,
}

#[derive(Debug, serde::Serialize)]
pub struct ProjectReadout {
    pub project_id: String,
    pub limit: i64,
    pub slots: Vec<HeldSlot>,
    pub collision: crate::collision::Collisions,
}

#[derive(Debug, serde::Serialize)]
pub struct Readout {
    pub house: HouseReadout,
    pub projects: Vec<ProjectReadout>,
}

/// How much fits, and what is inside it.
///
/// The project list is the **union** of the roster with the projects that hold slots. The roster
/// alone would leave a slot invisible if its project left `autopilot_state`, and capacity vanishing
/// in silence is the one outcome this reading cannot have — it is the screen's authority, and the
/// other calls only lay description over it.
pub async fn readout(pool: &SqlitePool) -> sqlx::Result<Readout> {
    let held = held_slots(pool).await?;
    let roster: Vec<String> =
        sqlx::query_scalar("SELECT project_id FROM autopilot_state ORDER BY project_id")
            .fetch_all(pool)
            .await?;

    let mut ids: Vec<String> = roster;
    for slot in &held {
        if !ids.iter().any(|id| id == &slot.project_id) {
            ids.push(slot.project_id.clone());
        }
    }
    ids.sort();

    let mut projects = Vec::with_capacity(ids.len());
    for project_id in ids {
        let limit = slots_limit(pool, &project_id).await?;
        let slots = held
            .iter()
            .filter(|slot| slot.project_id == project_id)
            .cloned()
            .collect();
        // No `?`. Collision is a best-effort warning and this route is the fleet's authority: a
        // malformed row or a locked table must not take the capacity, the cards and the start-a-job
        // action down with it. A failure degrades to `not measured`, which the screen already knows
        // how to draw — the same posture `measure` takes on the write side.
        let collision = match crate::collision::for_project(pool, &project_id).await {
            Ok(collision) => collision,
            Err(error) => {
                tracing::warn!(%project_id, %error, "could not read the collision state");
                crate::collision::Collisions::unmeasured()
            }
        };
        projects.push(ProjectReadout {
            project_id,
            limit,
            slots,
            collision,
        });
    }

    Ok(Readout {
        house: HouseReadout {
            limit: house_limit(pool).await?,
            // `held.len()` and not `slots_in_flight(pool)`: counting the reading just made is what
            // stops the total and the sum of the columns from disagreeing because of a write
            // landing between two queries.
            held: held.len() as i64,
        },
        projects,
    })
}

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
            ClaimOutcome::Full(NoRoom::Project { limit: 2 })
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
            ClaimOutcome::Full(NoRoom::House { limit: 2 })
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
        // And the other direction, which is the one that leaks: a status the sweep spares but no
        // pass drives is a slot held forever by a job nothing will ever move.
        let spared = ORPHANED_SLOTS_SQL
            .split('\'')
            .skip(1)
            .step_by(2)
            .filter(|token| !token.is_empty())
            .count();
        assert_eq!(
            spared,
            LIVE_RUN_STATUSES.len() + crate::job::LIVE_STATUSES.len() + 2,
            "the sweep spares a status nothing drives, or names an owner kind it cannot judge"
        );
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

    async fn seed_project(pool: &SqlitePool, project_id: &str) {
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES (?, 'active')")
            .bind(project_id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// The roster, not the slots table. A quiet project has to keep its column: `project_slots`
    /// only has rows for slots that were taken, and a column that comes and goes would make the
    /// layout jump every night that ends.
    #[tokio::test]
    async fn the_readout_enumerates_the_roster_including_a_project_holding_nothing() {
        let pool = test_pool().await;
        set_limits(&pool, 2, 9).await;
        seed_project(&pool, "project-a").await;
        seed_project(&pool, "project-quiet").await;
        let job = seed_job(&pool, "project-a", "implementing").await;
        claim(&pool, "project-a", Owner::Job(job)).await.unwrap();

        let readout = readout(&pool).await.unwrap();

        let ids: Vec<&str> = readout
            .projects
            .iter()
            .map(|project| project.project_id.as_str())
            .collect();
        assert_eq!(ids, ["project-a", "project-quiet"]);
        let quiet = &readout.projects[1];
        assert_eq!(quiet.limit, 2);
        assert!(quiet.slots.is_empty());
    }

    /// An autonomous worktree run holds a slot too. A reading that counted only jobs would say
    /// `0/2` about a project that is going to refuse the next job — and the screen would offer
    /// *New job* against a guaranteed 409.
    #[tokio::test]
    async fn a_project_with_a_worktree_run_reports_honest_capacity() {
        let pool = test_pool().await;
        set_limits(&pool, 2, 9).await;
        seed_project(&pool, "project-a").await;
        let run = seed_run(&pool, "project-a", "running").await;
        claim(&pool, "project-a", Owner::Run(run)).await.unwrap();

        let readout = readout(&pool).await.unwrap();

        let project = &readout.projects[0];
        // `1/2` is two numbers, and the denominator is half the honesty: without this line the
        // test passes with `slots_limit` wired up wrongly in `readout`.
        assert_eq!(project.slots.len(), 1);
        assert_eq!(project.limit, 2);
        assert_eq!(project.slots[0].owner_kind, "run");
        assert_eq!(project.slots[0].owner_id, run);
        assert_eq!(readout.house.held, 1);
    }

    /// A slot whose project is not on the roster still counts. Capacity never lies, even when the
    /// description fails — and an invisible slot would be capacity vanishing in silence.
    #[tokio::test]
    async fn a_slot_for_a_project_outside_the_roster_still_gets_a_column() {
        let pool = test_pool().await;
        set_limits(&pool, 2, 9).await;
        let run = seed_run(&pool, "project-ghost", "running").await;
        claim(&pool, "project-ghost", Owner::Run(run))
            .await
            .unwrap();

        let readout = readout(&pool).await.unwrap();

        assert_eq!(readout.projects.len(), 1);
        assert_eq!(readout.projects[0].project_id, "project-ghost");
        assert_eq!(readout.projects[0].slots.len(), 1);
    }

    /// The unreadable ceiling is one, and it is tested here rather than over HTTP:
    /// `UNREADABLE_CEILING` is private, and the `1` that comes out is indistinguishable from a
    /// ceiling configured to 1 when seen from outside.
    #[tokio::test]
    async fn a_ceiling_that_cannot_be_read_reaches_the_readout_as_one() {
        let pool = test_pool().await;
        seed_project(&pool, "project-a").await;
        sqlx::query(
            "UPDATE autopilot_global SET max_concurrent_slots = NULL, max_concurrent_total = NULL",
        )
        .execute(&pool)
        .await
        .unwrap();

        let readout = readout(&pool).await.unwrap();

        assert_eq!(readout.house.limit, UNREADABLE_CEILING);
        assert_eq!(readout.projects[0].limit, UNREADABLE_CEILING);
    }
}
