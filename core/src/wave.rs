//! §spec perfil-de-velocidade §4.6
//!
//! Slot leases for the controller's execution waves.
//!
//! A wave's workers are local processes — they compile, write and load the machine — so they are
//! bounded by the same slots as the daemon's own work (spec §2.1, invariant II). The controller that
//! runs them is a Claude Code or codex session, which the daemon cannot sweep by a run or a job id.
//! So a wave holds its slots on a LEASE, renewed by the controller and lapsed by its silence: a
//! controller that dies mid-wave gives its slots back within `LEASE_SECONDS` and one sweep, never
//! "when somebody notices".

use crate::concurrency::ClaimOutcome;
use crate::worktree::Owner;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

/// How long a wave's slots outlive its controller's last word.
///
/// Two minutes — the window `attention::HEARTBEAT_WINDOW_SECONDS` gives a person at the keyboard. A
/// controller renewing every thirty seconds survives three missed beats; a dead one holds a machine's
/// capacity for two minutes and one sweep at most.
pub const LEASE_SECONDS: i64 = 120;

/// One spelling for every lease column and for every cutoff compared against one.
///
/// Fixed width, in UTC with `Z`, because the sweep compares these as TEXT: with every value the same
/// length and the same zone, text order is time order without having to argue it.
pub fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// The oldest renewal still live at `now`.
pub fn cutoff(now: DateTime<Utc>) -> String {
    stamp(now - chrono::Duration::seconds(LEASE_SECONDS))
}

/// The most workers one wave may ask for.
///
/// Four, because the planner emits at most four units (`.claude/skills/planner/SKILL.md:136`) and a
/// wave never needs more workers than it has units. Above it is a caller's bug, refused at the
/// route, rather than trimmed into a grant nobody asked for.
pub const MAX_WORKERS: i64 = 4;

/// What a grant came to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Grant {
    /// `None` when nothing was granted: there is no lease to renew or release, and the controller
    /// runs one worker, as it would with no wave at all.
    pub wave_id: Option<i64>,
    pub requested: i64,
    pub granted: i64,
    /// How long the lease lives without a renewal, so the controller never restates the number.
    pub lease_seconds: i64,
    /// Which wall stopped the grant short, in `NoRoom::reason`'s words; `None` when granted in full.
    pub reason: Option<String>,
}

/// Seats up to `workers` workers for a wave in `project_id`, one slot each.
///
/// Sweeps first. A lapsed wave's slots are the likeliest thing between this request and room, and
/// the job tick that would otherwise free them runs every thirty seconds at best — and not at all
/// under the kill switch, or while a gate holds it.
///
/// Worker by worker through `concurrency::claim`, so each seat is taken on the primary key like any
/// other slot, and the first refusal ends the grant: a wave of three that fits two runs as two.
/// A database error halfway leaves the seats already taken under a fresh lease, which nobody will
/// renew; they come back within `LEASE_SECONDS` and one sweep, like any silent controller's.
pub async fn grant(
    pool: &SqlitePool,
    project_id: &str,
    workers: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<Grant> {
    crate::concurrency::reconcile_orphaned_slots_at(pool, now).await?;

    let at = stamp(now);
    let wave_id = sqlx::query("INSERT INTO waves (created_at, renewed_at) VALUES (?, ?)")
        .bind(&at)
        .bind(&at)
        .execute(pool)
        .await?
        .last_insert_rowid();

    let mut granted = 0;
    let mut reason = None;
    for _ in 0..workers {
        let worker_id = sqlx::query("INSERT INTO wave_workers (wave_id) VALUES (?)")
            .bind(wave_id)
            .execute(pool)
            .await?
            .last_insert_rowid();
        match crate::concurrency::claim(pool, project_id, Owner::Wave(worker_id)).await? {
            ClaimOutcome::Claimed(_) => granted += 1,
            ClaimOutcome::Full(no_room) => {
                sqlx::query("DELETE FROM wave_workers WHERE id = ?")
                    .bind(worker_id)
                    .execute(pool)
                    .await?;
                reason = Some(no_room.reason());
                break;
            }
        }
    }

    if granted == 0 {
        sqlx::query("DELETE FROM waves WHERE id = ?")
            .bind(wave_id)
            .execute(pool)
            .await?;
    }

    Ok(Grant {
        wave_id: (granted > 0).then_some(wave_id),
        requested: workers,
        granted,
        lease_seconds: LEASE_SECONDS,
        reason,
    })
}

/// How a renewal ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renewal {
    Renewed,
    /// Released, or silent past `LEASE_SECONDS`. Its slots are gone, or given back now; the
    /// controller must stop opening units on capacity it no longer holds.
    Lapsed,
    Unknown,
}

/// Extends a wave's lease by another `LEASE_SECONDS` from `now`.
///
/// A lease past its time is NOT renewed back to life, even when no sweep has taken its slots yet:
/// whether one has is a race the controller cannot see, and a slot the sweep freed may already be
/// somebody else's. So it is released here, and the controller is told.
pub async fn renew(pool: &SqlitePool, wave_id: i64, now: DateTime<Utc>) -> sqlx::Result<Renewal> {
    let renewed = sqlx::query(
        "UPDATE waves SET renewed_at = ?
         WHERE id = ? AND released_at IS NULL AND renewed_at >= ?",
    )
    .bind(stamp(now))
    .bind(wave_id)
    .bind(cutoff(now))
    .execute(pool)
    .await?;
    if renewed.rows_affected() == 1 {
        return Ok(Renewal::Renewed);
    }
    if release(pool, wave_id, now).await? {
        Ok(Renewal::Lapsed)
    } else {
        Ok(Renewal::Unknown)
    }
}

/// Gives every slot a wave holds back, and marks it released. `false` when there is no such wave.
///
/// Idempotent — a controller's cleanup runs on every ending, and some endings run it twice. The
/// first release's instant is kept.
pub async fn release(pool: &SqlitePool, wave_id: i64, now: DateTime<Utc>) -> sqlx::Result<bool> {
    let known = sqlx::query("UPDATE waves SET released_at = COALESCE(released_at, ?) WHERE id = ?")
        .bind(stamp(now))
        .bind(wave_id)
        .execute(pool)
        .await?;
    if known.rows_affected() == 0 {
        return Ok(false);
    }
    let workers: Vec<i64> = sqlx::query_scalar("SELECT id FROM wave_workers WHERE wave_id = ?")
        .bind(wave_id)
        .fetch_all(pool)
        .await?;
    for worker in workers {
        crate::concurrency::release(pool, Owner::Wave(worker)).await?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concurrency::{claim, slot_of, slots_in_flight};
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

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-24T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn after(seconds: i64) -> DateTime<Utc> {
        t0() + chrono::Duration::seconds(seconds)
    }

    /// A run that is really running, so its slot survives the sweep every grant does first. A
    /// slot claimed for an owner with no row — `Owner::Run(1)` on an empty `runs` — would be swept
    /// by that very sweep, and the test would be measuring a ceiling that is not there.
    async fn live_run(pool: &SqlitePool) -> Owner {
        let id = sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES ('project-a', 'a prompt', 'running', 'worktree', '2026-09-24T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        Owner::Run(id)
    }

    async fn count(pool: &SqlitePool, table: &str) -> i64 {
        // `AssertSqlSafe`: sqlx 0.9 takes only `&'static str` as SQL, and `table` is a test's literal.
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// A wave gets what fits and is told which wall stopped it, never more than the project's
    /// ceiling. That is invariant (II) for a wave: `fast` buys concurrency the machine has, and
    /// never creates it.
    #[tokio::test]
    async fn a_wave_is_granted_what_fits_and_told_which_wall_stopped_it() {
        let pool = test_pool().await;
        set_limits(&pool, 3, 9).await;
        let run = live_run(&pool).await;
        claim(&pool, "project-a", run).await.unwrap();

        let grant = grant(&pool, "project-a", 4, t0()).await.unwrap();

        assert_eq!(grant.requested, 4);
        assert_eq!(grant.granted, 2);
        assert!(grant.wave_id.is_some());
        assert_eq!(grant.lease_seconds, LEASE_SECONDS);
        assert!(
            grant.reason.as_deref().unwrap().contains("this project"),
            "{:?}",
            grant.reason
        );
        assert_eq!(
            count(&pool, "wave_workers").await,
            2,
            "no seat for a worker refused"
        );
        assert_eq!(slots_in_flight(&pool).await.unwrap(), 3);
    }

    /// Granted nothing, a wave leaves nothing behind: no lease to renew, no worker rows to read as
    /// capacity. The controller runs one worker, as it would with no wave at all.
    #[tokio::test]
    async fn a_wave_granted_nothing_leaves_no_lease() {
        let pool = test_pool().await;
        set_limits(&pool, 1, 1).await;
        let run = live_run(&pool).await;
        claim(&pool, "project-a", run).await.unwrap();

        let grant = grant(&pool, "project-a", 2, t0()).await.unwrap();

        assert_eq!((grant.wave_id, grant.granted), (None, 0));
        assert!(grant.reason.is_some());
        assert_eq!(count(&pool, "waves").await, 0);
        assert_eq!(count(&pool, "wave_workers").await, 0);
    }

    /// Renewing keeps the lease alive past its first two minutes, and silence lets it lapse; a
    /// lapsed lease is not renewed back to life, because its slots may already be somebody else's.
    #[tokio::test]
    async fn a_renewal_keeps_the_lease_and_silence_lapses_it() {
        let pool = test_pool().await;
        set_limits(&pool, 5, 9).await;
        let wave = grant(&pool, "project-a", 2, t0())
            .await
            .unwrap()
            .wave_id
            .unwrap();

        assert_eq!(
            renew(&pool, wave, after(100)).await.unwrap(),
            Renewal::Renewed
        );
        crate::concurrency::reconcile_orphaned_slots_at(&pool, after(200))
            .await
            .unwrap();
        assert_eq!(
            slots_in_flight(&pool).await.unwrap(),
            2,
            "renewed at 100, live at 200"
        );

        let silent = after(100 + LEASE_SECONDS + 1);
        crate::concurrency::reconcile_orphaned_slots_at(&pool, silent)
            .await
            .unwrap();
        assert_eq!(slots_in_flight(&pool).await.unwrap(), 0);
        assert_eq!(renew(&pool, wave, silent).await.unwrap(), Renewal::Lapsed);
        assert_eq!(renew(&pool, 9_999, silent).await.unwrap(), Renewal::Unknown);
    }

    /// A lease past its time is not renewed even before any sweep has run, and renewing it gives its
    /// slots back there and then — a controller told `Lapsed` must not find its slots still held.
    #[tokio::test]
    async fn renewing_a_lapsed_lease_gives_its_slots_back() {
        let pool = test_pool().await;
        set_limits(&pool, 5, 9).await;
        let wave = grant(&pool, "project-a", 2, t0())
            .await
            .unwrap()
            .wave_id
            .unwrap();

        assert_eq!(
            renew(&pool, wave, after(LEASE_SECONDS + 1)).await.unwrap(),
            Renewal::Lapsed
        );
        assert_eq!(slots_in_flight(&pool).await.unwrap(), 0);
    }

    /// Releasing gives every slot back at once, twice is harmless, and an unknown wave says so.
    #[tokio::test]
    async fn a_released_wave_gives_its_slots_back_and_twice_is_harmless() {
        let pool = test_pool().await;
        set_limits(&pool, 5, 9).await;
        let wave = grant(&pool, "project-a", 3, t0())
            .await
            .unwrap()
            .wave_id
            .unwrap();
        let first_worker: i64 =
            sqlx::query_scalar("SELECT MIN(id) FROM wave_workers WHERE wave_id = ?")
                .bind(wave)
                .fetch_one(&pool)
                .await
                .unwrap();

        assert!(release(&pool, wave, after(10)).await.unwrap());
        assert_eq!(slots_in_flight(&pool).await.unwrap(), 0);
        assert_eq!(
            slot_of(&pool, Owner::Wave(first_worker)).await.unwrap(),
            None
        );
        assert!(release(&pool, wave, after(20)).await.unwrap());
        assert!(!release(&pool, 9_999, after(20)).await.unwrap());
        assert_eq!(
            renew(&pool, wave, after(30)).await.unwrap(),
            Renewal::Lapsed
        );
    }

    /// A grant sweeps before it claims, so a dead controller's slots are free to the next wave at
    /// once rather than a job tick later.
    #[tokio::test]
    async fn a_lapsed_wave_is_swept_before_the_next_is_granted() {
        let pool = test_pool().await;
        set_limits(&pool, 2, 2).await;
        grant(&pool, "project-a", 2, t0()).await.unwrap();

        let next = grant(&pool, "project-a", 2, after(LEASE_SECONDS + 1))
            .await
            .unwrap();

        assert_eq!(next.granted, 2);
    }
}
