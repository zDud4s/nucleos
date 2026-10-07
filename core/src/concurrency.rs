//! §spec trabalho-noturno-e-jobs-paralelos
//!
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
use sqlx::{SqliteConnection, SqlitePool};

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
    slots_limit_on(&mut *pool.acquire().await?, project_id).await
}

async fn slots_limit_on(conn: &mut SqliteConnection, project_id: &str) -> sqlx::Result<i64> {
    let project_override: Option<Option<i64>> =
        sqlx::query_scalar("SELECT max_concurrent_slots FROM autopilot_state WHERE project_id = ?")
            .bind(project_id)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some(Some(limit)) = project_override {
        return Ok(limit.max(1));
    }
    let global: Option<Option<i64>> =
        sqlx::query_scalar("SELECT max_concurrent_slots FROM autopilot_global LIMIT 1")
            .fetch_optional(&mut *conn)
            .await?;
    Ok(global.flatten().unwrap_or(UNREADABLE_CEILING).max(1))
}

/// The ceiling across every project at once.
///
/// Both numbers exist because they bound different resources: the per-project one stops a single
/// project from monopolising the machine, this one stops N projects from saturating it together.
pub async fn house_limit(pool: &SqlitePool) -> sqlx::Result<i64> {
    house_limit_on(&mut *pool.acquire().await?).await
}

async fn house_limit_on(conn: &mut SqliteConnection) -> sqlx::Result<i64> {
    let global: Option<Option<i64>> =
        sqlx::query_scalar("SELECT max_concurrent_total FROM autopilot_global LIMIT 1")
            .fetch_optional(&mut *conn)
            .await?;
    Ok(global.flatten().unwrap_or(UNREADABLE_CEILING).max(1))
}

/// How many slots are held right now, everywhere.
#[cfg(any(test, feature = "testkit"))]
pub async fn slots_in_flight(pool: &SqlitePool) -> sqlx::Result<i64> {
    slots_in_flight_on(&mut *pool.acquire().await?).await
}

async fn slots_in_flight_on(conn: &mut SqliteConnection) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT COUNT(*) FROM project_slots")
        .fetch_one(&mut *conn)
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
    claim_on(&mut *pool.acquire().await?, project_id, owner).await
}

/// `claim` on a connection the caller holds, in practice an open write transaction.
///
/// Spec autopilot-juiz-resolve-bloqueios D6 (S4): the correction's transaction asks for a slot only
/// when the sweep already freed the one its origin held, and the whole creation (the correction
/// row, the run, the slot, the tree) must be one atomic write. `claim` on the pool would ask for a
/// second connection that waits on the first one's write lock until the busy timeout; claiming
/// after the commit would need a compensation path that is easy to get wrong.
///
/// Everything `claim`'s own comment says still holds: idempotent per owner, the per-project number
/// held by the primary key, the house number advisory. A unique violation inside a SQLite
/// transaction aborts only the statement, so the next number is tried as before.
pub async fn claim_on(
    conn: &mut SqliteConnection,
    project_id: &str,
    owner: Owner,
) -> sqlx::Result<ClaimOutcome> {
    if let Some(slot) = slot_of_on(conn, owner).await? {
        return Ok(ClaimOutcome::Claimed(slot));
    }

    if let Some(full) = room_for_on(conn, project_id).await? {
        return Ok(ClaimOutcome::Full(full));
    }

    let limit = slots_limit_on(conn, project_id).await?;
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
        .execute(&mut *conn)
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
    room_for_on(&mut *pool.acquire().await?, project_id).await
}

async fn room_for_on(
    conn: &mut SqliteConnection,
    project_id: &str,
) -> sqlx::Result<Option<NoRoom>> {
    let house = house_limit_on(conn).await?;
    if slots_in_flight_on(conn).await? >= house {
        return Ok(Some(NoRoom::House { limit: house }));
    }
    let limit = slots_limit_on(conn, project_id).await?;
    let held: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_slots WHERE project_id = ?")
        .bind(project_id)
        .fetch_one(&mut *conn)
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
#[cfg(any(test, feature = "testkit"))]
pub async fn slot_of(pool: &SqlitePool, owner: Owner) -> sqlx::Result<Option<i64>> {
    slot_of_on(&mut *pool.acquire().await?, owner).await
}

async fn slot_of_on(conn: &mut SqliteConnection, owner: Owner) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar("SELECT slot FROM project_slots WHERE owner_kind = ? AND owner_id = ?")
        .bind(owner.kind())
        .bind(owner.id())
        .fetch_optional(&mut *conn)
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
    reconcile_orphaned_slots_at(pool, chrono::Utc::now()).await
}

/// [`reconcile_orphaned_slots`] at a given instant — the one clock the wave arm reads, taken as an
/// argument so a lease can be tested at its edge rather than by sleeping past it.
pub async fn reconcile_orphaned_slots_at(
    pool: &SqlitePool,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<u64> {
    let swept = sqlx::query(ORPHANED_SLOTS_SQL)
        .bind(crate::wave::cutoff(now))
        .execute(pool)
        .await?;
    Ok(swept.rows_affected())
}

/// Spelled out rather than assembled from `LIVE_RUN_STATUSES` and `job::LIVE_STATUSES`, because sqlx
/// refuses SQL built at runtime — the same trade `LIVE_JOBS_SQL` makes, and with the same guard:
/// `every_live_status_is_a_status_the_sweep_spares` compares this text against both constants, so
/// they cannot drift apart in silence.
///
/// An owner kind this does not know is left alone on purpose. Deleting it would free a slot that
/// something may still be working in, and over-concurrency is the one failure this table exists to
/// prevent; a leaked slot is visible in `project_slots` and costs a lower ceiling. Adding an owner
/// kind means teaching this pass, and `Owner` being an enum is what makes that a compile-time
/// conversation rather than a silent one.
///
/// **The item arm takes TWO conditions, and the pair is the whole of it.** An item's own status is
/// what frees its slot mid-job — otherwise a five-item job would hold five slots to the end and
/// parallelism would be worth nothing — and its job's status is what frees it in the end. Neither
/// alone is enough: item liveness alone would hold a slot forever for an item that ran out of gate
/// retries, and job liveness alone would hold every item's slot until the last one landed.
///
/// **The wave arm judges a lease, not a status.** A wave's controller is a session the daemon has
/// no row of liveness for, so a worker's slot lives while its wave is unreleased and renewed within
/// `wave::LEASE_SECONDS` — the one `?` in this text, bound by `reconcile_orphaned_slots_at` from a
/// cutoff computed in Rust, because SQLite's own `datetime('now')` does not sort against RFC 3339.
///
/// `gate_failed` is spared deliberately, and it is the one status here that may be either thing. A
/// red gate with a retry left is an item that WILL run again, and telling that from an item that is
/// over needs `gate_attempts` weighed against `jobs.gate_retries` — a rule `job::item_state_from`
/// owns, and one this repository has three times been bitten by writing out a second time. Sparing
/// it costs a slot held until the job ends; getting the arithmetic wrong here costs a tree deleted
/// out from under work that was going to continue in it.
const ORPHANED_SLOTS_SQL: &str = "DELETE FROM project_slots
     WHERE (owner_kind = 'run'
            AND NOT EXISTS (SELECT 1 FROM runs
                            WHERE runs.id = project_slots.owner_id
                              AND runs.status IN ('running','awaiting_approval')))
        OR (owner_kind = 'job'
            AND NOT EXISTS (SELECT 1 FROM jobs
                            WHERE jobs.id = project_slots.owner_id
                              AND jobs.status IN ('planning','implementing','gating','reviewing',
                                                  'awaiting_approval','waiting')))
        OR (owner_kind = 'item'
            AND NOT EXISTS (SELECT 1 FROM job_items
                            JOIN jobs ON jobs.id = job_items.job_id
                            WHERE job_items.id = project_slots.owner_id
                              AND job_items.status IN ('pending','running','implemented','merging',
                                                       'conflicted','reverted','gate_failed')
                              AND jobs.status IN ('planning','implementing','gating','reviewing',
                                                  'awaiting_approval','waiting')))
        OR (owner_kind = 'wave'
            AND NOT EXISTS (SELECT 1 FROM wave_workers
                            JOIN waves ON waves.id = wave_workers.wave_id
                            WHERE wave_workers.id = project_slots.owner_id
                              AND waves.released_at IS NULL
                              AND waves.renewed_at >= ?))";

/// One taken slot, with its owner.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct HeldSlot {
    pub project_id: String,
    pub slot: i64,
    /// `'run'`, `'job'`, `'item'` or `'wave'`. A `String` and not `Owner`, because this leaves over JSON and
    /// the consumer is an interface that only wants to know which tab to link to.
    pub owner_kind: String,
    pub owner_id: i64,
    pub claimed_at: String,
    /// The job an ITEM belongs to, and `None` for every other kind of owner.
    ///
    /// **This and the two below are what let a screen describe an item at all.** `owner_id` is
    /// `job_items.id` — a number from a sequence nobody reads, which the fleet card could only
    /// print raw. There is no listing of items by id anywhere in the API to look it up in, and
    /// there should not be: an item is a step of a job, and the way to reach one is through the job
    /// that owns it. So the join happens here, once, beside the row that needs it.
    ///
    /// Three fields because they answer three questions a card has to answer at once: which job to
    /// point at, which of its items this is, and whether the slot it holds is busy or stuck.
    pub job_id: Option<i64>,
    /// Which item of that job this is, counting from zero. `None` for every other kind of owner.
    pub ordinal: Option<i64>,
    /// What that item is doing. `None` for every other kind of owner.
    ///
    /// Not redundant with "it holds a slot". A slot is held from the claim until the item goes
    /// terminal, and that window covers `running`, `merging`, `conflicted` and `reverted` — one of
    /// which is work in progress and one of which is work waiting on a person. A capacity screen
    /// that cannot tell those apart cannot answer the question it exists for: is this slot busy or
    /// is it stuck.
    pub item_status: Option<String>,
    /// The wave a WAVE's worker belongs to, and `None` for every other kind of owner.
    ///
    /// `owner_id` for a wave is `wave_workers.id`, one row per worker: a screen that named the
    /// slot by it would show as many waves as there are workers.
    pub wave_id: Option<i64>,
    /// When that wave last renewed its lease (`waves.renewed_at`). `None` for every other kind.
    ///
    /// The slot lives until `wave::LEASE_SECONDS` after this, so it is what tells a live wave from
    /// a controller that died and whose slots the next sweep takes.
    pub lease_renewed_at: Option<String>,
}

/// Every slot taken right now, in project and number order.
///
/// The owner-kind predicate is in the `ON` clause and not in a `WHERE`, for the reason
/// `job::SUMMARY_SQL` gives about both of its own: in a `WHERE` it would turn the left join into an
/// inner one and drop every slot held by a run or a job — which is nearly all of them.
pub async fn held_slots(pool: &SqlitePool) -> sqlx::Result<Vec<HeldSlot>> {
    sqlx::query_as(
        "SELECT project_slots.project_id, project_slots.slot, project_slots.owner_kind,
                project_slots.owner_id, project_slots.claimed_at,
                job_items.job_id AS job_id, job_items.ordinal AS ordinal,
                job_items.status AS item_status,
                wave_workers.wave_id AS wave_id, waves.renewed_at AS lease_renewed_at
         FROM project_slots
         LEFT JOIN job_items
           ON project_slots.owner_kind = 'item' AND job_items.id = project_slots.owner_id
         LEFT JOIN wave_workers
           ON project_slots.owner_kind = 'wave' AND wave_workers.id = project_slots.owner_id
         LEFT JOIN waves ON waves.id = wave_workers.wave_id
         ORDER BY project_slots.project_id, project_slots.slot",
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
        crate::storage::MIGRATOR.run(&pool).await.unwrap();
        pool
    }

    /// A fresh install has room for a job and two of its items, and this is where the feature is
    /// switched on or off.
    ///
    /// **One tree, one slot.** A job holds a slot for the branch it integrates into and each item
    /// worked in parallel holds its own, so running `k` items at once needs `k + 1`. At the ceiling
    /// of two that `0052` set, `k` is one: every plan executed one item at a time, every measurement
    /// showing the sequential timings, and no symptom at all beyond parallelism never seeming to
    /// help. That is how an earlier draft of this design convinced itself it worked.
    ///
    /// Asserted here rather than left to the migration, because a migration that is reverted or
    /// whose `WHERE` stops matching says nothing at all, and this says it.
    #[tokio::test]
    async fn a_fresh_install_has_room_for_a_job_and_two_of_its_items() {
        let pool = test_pool().await;

        assert_eq!(
            slots_limit(&pool, "project-a").await.unwrap(),
            3,
            "one integration tree plus two items"
        );
        assert_eq!(
            house_limit(&pool).await.unwrap(),
            4,
            "one project running a team, and a second getting on with something"
        );
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

    /// Spec autopilot-juiz-resolve-bloqueios D6 (S4): a claim made inside the caller's transaction
    /// is part of it. Rolled back, nothing is held; inside, it is the same claim (idempotent per
    /// owner, refusing a full project) and never asks the pool for a second connection while the
    /// first holds the lock. The test pool has one connection, so a second request would hang.
    #[tokio::test]
    async fn a_claim_on_a_transaction_is_undone_with_it() {
        let pool = test_pool().await;
        set_limits(&pool, 1, 4).await;
        let run = seed_run(&pool, "project-a", "running").await;
        {
            let mut tx = pool.begin().await.unwrap();
            assert_eq!(
                claim_on(&mut tx, "project-a", Owner::Run(run))
                    .await
                    .unwrap(),
                ClaimOutcome::Claimed(0)
            );
            assert_eq!(
                claim_on(&mut tx, "project-a", Owner::Run(run))
                    .await
                    .unwrap(),
                ClaimOutcome::Claimed(0),
                "idempotent per owner"
            );
            assert_eq!(
                claim_on(&mut tx, "project-a", Owner::Run(run + 1))
                    .await
                    .unwrap(),
                ClaimOutcome::Full(NoRoom::Project { limit: 1 })
            );
        }
        assert_eq!(
            slots_in_flight(&pool).await.unwrap(),
            0,
            "dropped, so rolled back"
        );
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

    /// A slot held by an item carries the job it is a step of, and the ones held by anything else
    /// carry nothing.
    ///
    /// **Both halves, because the join is the kind that quietly loses rows.** The owner-kind
    /// predicate is in the `ON` clause; moved to a `WHERE` it turns the left join into an inner one
    /// and every slot held by a run or a job — nearly all of them — vanishes from the readout. That
    /// failure looks like an empty fleet screen, not like a missing field.
    ///
    /// The pair is what makes an item describable at all. `owner_id` for an item is
    /// `job_items.id`, a number no route lists and no reader recognises, and the screen could only
    /// ever print it raw.
    #[tokio::test]
    async fn a_slot_held_by_an_item_says_which_job_and_which_item_it_is() {
        let pool = test_pool().await;
        let job = seed_job(&pool, "project-a", "implementing").await;
        let item: i64 = sqlx::query_scalar(
            "INSERT INTO job_items (job_id, ordinal, description, status)
             VALUES (?, 2, 'an item', 'running') RETURNING id",
        )
        .bind(job)
        .fetch_one(&pool)
        .await
        .unwrap();
        claim(&pool, "project-a", Owner::Job(job)).await.unwrap();
        claim(&pool, "project-a", Owner::Item(item)).await.unwrap();

        let held = held_slots(&pool).await.unwrap();

        let of_item = held
            .iter()
            .find(|slot| slot.owner_kind == "item")
            .expect("the item's slot is in the readout");
        assert_eq!(of_item.job_id, Some(job));
        assert_eq!(
            of_item.ordinal,
            Some(2),
            "counting from zero, as the row does"
        );
        assert_eq!(
            of_item.item_status.as_deref(),
            Some("running"),
            "a slot is held from the claim until the item is terminal, so what it is DOING is the \
             difference between a slot that is busy and one that is stuck"
        );

        let of_job = held
            .iter()
            .find(|slot| slot.owner_kind == "job")
            .expect("the job's slot did not survive the join");
        assert_eq!(of_job.job_id, None, "a job is not a step of anything");
        assert_eq!(of_job.ordinal, None);
        assert_eq!(of_job.item_status, None);
    }

    /// A wave's slot says which wave it is and when that wave last renewed its lease.
    ///
    /// `owner_id` for a wave is `wave_workers.id`, one per worker, so a screen that named the slot
    /// by it would show three waves where there is one; and `renewed_at` is what tells a live wave
    /// from a controller that died and whose slots the next sweep takes.
    #[tokio::test]
    async fn a_slot_held_by_a_wave_says_which_wave_and_when_its_lease_was_renewed() {
        let pool = test_pool().await;
        set_limits(&pool, 5, 9).await;
        let renewed = chrono::DateTime::parse_from_rfc3339("2026-09-29T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        // Wave 1 gets two workers first, so the worker we claim for (wave 2) has a different
        // number from its wave: a join that read `owner_id` for the wave could not pass.
        let first = seed_wave_worker(&pool, renewed, false).await;
        sqlx::query(
            "INSERT INTO wave_workers (wave_id) SELECT wave_id FROM wave_workers WHERE id = ?",
        )
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();
        let worker = seed_wave_worker(&pool, renewed, false).await;
        let wave: i64 = sqlx::query_scalar("SELECT wave_id FROM wave_workers WHERE id = ?")
            .bind(worker)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(wave, worker, "the fixture must tell a wave from its worker");
        let job = seed_job(&pool, "project-a", "implementing").await;
        claim(&pool, "project-a", Owner::Wave(worker))
            .await
            .unwrap();
        claim(&pool, "project-a", Owner::Job(job)).await.unwrap();

        let held = held_slots(&pool).await.unwrap();

        let of_wave = held
            .iter()
            .find(|slot| slot.owner_kind == "wave")
            .expect("the wave's slot is in the readout");
        assert_eq!(of_wave.wave_id, Some(wave));
        assert_eq!(
            of_wave.lease_renewed_at.as_deref(),
            Some(crate::wave::stamp(renewed).as_str())
        );
        let of_job = held
            .iter()
            .find(|slot| slot.owner_kind == "job")
            .expect("the job's slot did not survive the joins");
        assert_eq!(of_job.wave_id, None, "a job is not a worker of anything");
        assert_eq!(of_job.lease_renewed_at, None);
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

    /// An item's slot is freed on TWO conditions, and the test walks both because either alone is a
    /// defect with a different shape.
    ///
    /// A running item inside a live job keeps its slot — the ordinary case, and the one a sweep that
    /// judged by the job alone would still get right. A finished item inside a LIVE job gives its
    /// slot back mid-flight, which is the whole of what parallelism buys: without it a five-item job
    /// would hold five slots until its last item landed. And an item still reading `gate_failed`,
    /// which may or may not be over, gives its slot back when the job around it ends — the arm that
    /// keeps the deliberate generosity about that status from leaking a slot for ever.
    #[tokio::test]
    async fn an_items_slot_is_freed_by_its_own_ending_or_by_its_jobs() {
        async fn seed(pool: &SqlitePool, job_status: &str, item_status: &str) -> i64 {
            let job_id = sqlx::query(
                "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
                 VALUES ('project-a', '/repo', ?, 5, '2026-01-01T00:00:00Z')",
            )
            .bind(job_status)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid();
            sqlx::query(
                "INSERT INTO job_items (job_id, ordinal, description, status)
                 VALUES (?, 0, 'an item', ?)",
            )
            .bind(job_id)
            .bind(item_status)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid()
        }

        for (job_status, item_status, survives) in [
            ("implementing", "running", true),
            ("implementing", "gate_failed", true),
            ("implementing", "passed", false),
            ("implementing", "orphaned", false),
            ("completed", "running", false),
            ("completed", "gate_failed", false),
        ] {
            let pool = test_pool().await;
            set_limits(&pool, 5, 9).await;
            let item_id = seed(&pool, job_status, item_status).await;
            claim(&pool, "project-a", Owner::Item(item_id))
                .await
                .unwrap();

            reconcile_orphaned_slots(&pool).await.unwrap();

            assert_eq!(
                slot_of(&pool, Owner::Item(item_id))
                    .await
                    .unwrap()
                    .is_some(),
                survives,
                "job `{job_status}` with item `{item_status}`"
            );
        }
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
        for status in crate::job::LIVE_ITEM_STATUSES {
            assert!(
                ORPHANED_SLOTS_SQL.contains(&format!("'{status}'")),
                "the sweep does not spare live item status `{status}`"
            );
        }
        // And the other direction, which is the one that leaks: a status the sweep spares but no
        // pass drives is a slot held forever by a job nothing will ever move.
        //
        // The job statuses are counted TWICE: the item arm asks the same question of the item's job
        // that the job arm asks of the job itself, because an item's slot has to be freed both when
        // the item is over and when the job around it is.
        let spared = ORPHANED_SLOTS_SQL
            .split('\'')
            .skip(1)
            .step_by(2)
            .filter(|token| !token.is_empty())
            .count();
        assert_eq!(
            spared,
            LIVE_RUN_STATUSES.len()
                + crate::job::LIVE_STATUSES.len() * 2
                + crate::job::LIVE_ITEM_STATUSES.len()
                // The four owner kinds: 'run', 'job', 'item' and 'wave'.
                + 4,
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

    /// A wave owns slots and never a daemon worktree: its units are checked out by the controller,
    /// by git directly (perfil-de-velocidade spec §4.2), and the `worktrees` CHECK refusing `'wave'`
    /// is that rule held by the schema rather than by every caller's memory.
    ///
    /// The same row goes in first as an item, so a refusal for some other reason — a column this
    /// test forgot — cannot pass for the one being asserted.
    #[tokio::test]
    async fn a_wave_owns_slots_and_never_a_daemon_worktree() {
        let pool = test_pool().await;
        assert_eq!(Owner::Wave(7).kind(), "wave");

        let insert = |kind: &'static str| {
            sqlx::query(
                "INSERT INTO worktrees
                     (owner_kind, owner_id, project_id, project_root, path, branch, created_at)
                 VALUES (?, 7, 'project-a', 'C:/somewhere', 'C:/somewhere/x-7', 'nucleos/x-7',
                         '2026-09-24T00:00:00Z')",
            )
            .bind(kind)
        };
        insert("item")
            .execute(&pool)
            .await
            .expect("the row is well formed");
        assert!(
            insert("wave").execute(&pool).await.is_err(),
            "the worktrees CHECK must refuse a wave"
        );
    }

    /// A wave's worker, written by hand: the lease row, then the worker that holds the slot.
    async fn seed_wave_worker(
        pool: &SqlitePool,
        renewed_at: chrono::DateTime<chrono::Utc>,
        released: bool,
    ) -> i64 {
        let at = crate::wave::stamp(renewed_at);
        let wave_id =
            sqlx::query("INSERT INTO waves (created_at, renewed_at, released_at) VALUES (?, ?, ?)")
                .bind(&at)
                .bind(&at)
                .bind(released.then(|| at.clone()))
                .execute(pool)
                .await
                .unwrap()
                .last_insert_rowid();
        sqlx::query("INSERT INTO wave_workers (wave_id) VALUES (?)")
            .bind(wave_id)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid()
    }

    /// A wave's slot lives exactly as long as its lease, and not a sweep longer.
    ///
    /// This is the orphan collection of spec §4.6. A controller is a session the daemon cannot sweep
    /// by id, so without this arm a controller that died mid-wave would hold its slots forever —
    /// "worse than not having asked for a slot at all", in the spec's words — and a slot held that
    /// way is silent: it lowers a project's ceiling with no error anywhere.
    #[tokio::test]
    async fn a_waves_slot_lives_as_long_as_its_lease() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-24T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let seconds = chrono::Duration::seconds;
        let lease = crate::wave::LEASE_SECONDS;

        // (renewed this long ago, released, survives the sweep)
        let cases = [
            (seconds(0), false, true),
            (seconds(lease), false, true),
            (seconds(lease + 1), false, false),
            (seconds(0), true, false),
        ];
        for (ago, released, survives) in cases {
            let pool = test_pool().await;
            set_limits(&pool, 5, 9).await;
            let worker = seed_wave_worker(&pool, now - ago, released).await;
            claim(&pool, "project-a", Owner::Wave(worker))
                .await
                .unwrap();

            reconcile_orphaned_slots_at(&pool, now).await.unwrap();

            assert_eq!(
                slot_of(&pool, Owner::Wave(worker)).await.unwrap().is_some(),
                survives,
                "renewed {ago} ago, released: {released}"
            );
        }
    }

    /// A worker the daemon has no row for is swept, like a run that no longer exists: nothing can
    /// renew a lease that is not there.
    #[tokio::test]
    async fn a_wave_slot_with_no_worker_row_is_swept() {
        let pool = test_pool().await;
        claim(&pool, "project-a", Owner::Wave(9_999)).await.unwrap();

        assert_eq!(reconcile_orphaned_slots(&pool).await.unwrap(), 1);
    }
}
