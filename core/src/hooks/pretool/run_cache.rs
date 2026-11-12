//! A short-lived, per-run cache of what PreToolUse reads about the run it decides for (spec
//! `2026-11-09-arvores-autonomia-design` F2: "~10 SELECTs per tool call").
//!
//! One joined read of the run row, its tree node and the job's `requested` flag replaces what the
//! hook used to ask four times per call (the mode lookup, the node verdict twice, the project of the
//! output filter, the job of the unrecognized-tool policy). A warm `Read` then costs the hook two
//! statements: the ticket-notice look (`take_run_notices`) and the shadow-decision INSERT; the
//! heartbeat is throttled on its own.
//!
//! **What is cached, and for how long.** Only what a run's own row fixes: `cwd`, `mode`, `project_id`,
//! the judge snapshots, `job_id`, the node binding and whether the job was asked for. Two columns do
//! move during a run and are covered by the TTL instead of being fresh: `permission_mode` (a CLI
//! turn rewrites it) and `context_fill` (the runner writes it as usage arrives). The TTL is
//! [`TTL`], 3 s: a wrap limit is enforced at most 3 s late, and a selector moved by the owner
//! reaches the next call after that.
//!
//! **What is NOT cached, on purpose.** Anything the owner or another process can change and expect to
//! bite at once: the project's shell and GitHub rules, declared git operations, grants, declined
//! actions, queued requests, the role of the caller, the loadout and the ticket notices. Those are
//! read fresh on the paths that need them, and a `Read` reaches none of them.
//!
//! **Only an in-flight run is cached.** The key also carries the pool, so two databases in one
//! process (the tests) never share a run id; the entry holds a clone of that pool, so its address
//! cannot be reused while the entry exists. A call for a run that is no longer in flight evicts its
//! entry and reads fresh, and every insert sweeps the entries that are expired or whose run has left
//! `run_handles`, so a finished run's entry never outlives the next insert by more than [`TTL`].

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use sqlx::{Row, SqlitePool};

use crate::state::AppState;

/// How long an entry answers without a read. Tests run with their own (zero unless a test asks).
#[cfg_attr(test, allow(dead_code))]
pub(super) const TTL: Duration = Duration::from_secs(3);
/// A hard bound; the sweep on insert keeps the map far below it in practice.
const MAX_ENTRIES: usize = 256;

/// The node a run works for.
pub(super) struct NodeFacts {
    pub id: String,
    pub root_id: String,
    pub kind: String,
    pub project_id: Option<String>,
}

/// What the hook reads about a run, in one piece.
pub(super) struct RunFacts {
    pub cwd: Option<String>,
    pub mode: String,
    pub project_id: Option<String>,
    pub permission_mode: Option<String>,
    pub judge: String,
    pub lineage_root: i64,
    pub job_id: Option<i64>,
    pub judge_resolve: String,
    pub context_fill: Option<i64>,
    pub node: Option<NodeFacts>,
    /// The run's job was asked for by the owner (`jobs.rule_name IS NULL`).
    pub job_requested: bool,
}

struct Entry {
    at: Instant,
    facts: Arc<RunFacts>,
    /// Keeps the pool alive, so the address its key is made of cannot be handed to another pool.
    _pool: SqlitePool,
}

type Key = (usize, i64);

static CACHE: LazyLock<Mutex<HashMap<Key, Entry>>> = LazyLock::new(Mutex::default);

#[cfg(test)]
thread_local! {
    /// Zero outside the cache's own tests, so every other test reads fresh exactly as before.
    static TEST_TTL: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
    /// How many joined reads this thread's calls have made.
    static LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn ttl() -> Duration {
    #[cfg(test)]
    {
        TEST_TTL.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        TTL
    }
}

fn pool_id(pool: &SqlitePool) -> usize {
    // `options()` lives inside the pool's shared allocation, so every clone of a pool names it alike.
    std::ptr::from_ref(pool.options()) as usize
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<Key, Entry>> {
    CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether the daemon still holds a task for `run_id`.
pub(super) fn in_flight(state: &AppState, run_id: i64) -> bool {
    state
        .run_handles
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains_key(&run_id)
}

/// The run's facts: from the cache while it is warm and the run is in flight, otherwise one joined
/// read. `Ok(None)` is a run with no row; it is never cached.
pub(super) async fn run_facts(
    state: &AppState,
    run_id: i64,
    in_flight: bool,
) -> Result<Option<Arc<RunFacts>>, sqlx::Error> {
    let key = (pool_id(&state.pool), run_id);
    let window = ttl();
    if !in_flight {
        lock().remove(&key);
    } else if !window.is_zero() {
        let mut map = lock();
        match map.get(&key) {
            Some(entry) if entry.at.elapsed() < window => return Ok(Some(entry.facts.clone())),
            Some(_) => {
                map.remove(&key);
            }
            None => {}
        }
    }
    let Some(facts) = load(&state.pool, run_id).await?.map(Arc::new) else {
        return Ok(None);
    };
    if in_flight && !window.is_zero() {
        store(state, key, window, facts.clone());
    }
    Ok(Some(facts))
}

fn store(state: &AppState, key: Key, window: Duration, facts: Arc<RunFacts>) {
    let live: Vec<i64> = state
        .run_handles
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .keys()
        .copied()
        .collect();
    let mut map = lock();
    map.retain(|(pool, run), entry| {
        entry.at.elapsed() < window && (*pool != key.0 || live.contains(run))
    });
    if map.len() >= MAX_ENTRIES {
        map.clear();
    }
    map.insert(
        key,
        Entry {
            at: Instant::now(),
            facts,
            _pool: state.pool.clone(),
        },
    );
}

async fn load(pool: &SqlitePool, run_id: i64) -> Result<Option<RunFacts>, sqlx::Error> {
    #[cfg(test)]
    LOADS.with(|loads| loads.set(loads.get() + 1));
    // Several nodes bound to one run are not expected; `fetch_optional` keeps the first, as the
    // node verdict's own join did.
    let Some(row) = sqlx::query(
        "SELECT r.cwd, r.mode, r.project_id, r.permission_mode, r.judge,
                COALESCE(r.lineage_root_id, r.id) AS lineage, r.job_id, r.judge_resolve,
                r.context_fill,
                n.id AS node_id, n.root_id AS node_root, n.kind AS node_kind,
                n.project_id AS node_project,
                (SELECT 1 FROM jobs j WHERE j.id = r.job_id AND j.rule_name IS NULL) AS requested
         FROM runs r LEFT JOIN work_nodes n ON n.run_id = CAST(r.id AS TEXT)
         WHERE r.id = ?",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let node = row
        .try_get::<Option<String>, _>("node_id")?
        .map(|id| -> Result<NodeFacts, sqlx::Error> {
            Ok(NodeFacts {
                id,
                root_id: row.try_get("node_root")?,
                kind: row.try_get("node_kind")?,
                project_id: row.try_get("node_project")?,
            })
        })
        .transpose()?;
    Ok(Some(RunFacts {
        cwd: row.try_get("cwd")?,
        mode: row.try_get("mode")?,
        project_id: row.try_get("project_id")?,
        permission_mode: row.try_get("permission_mode")?,
        judge: row.try_get("judge")?,
        lineage_root: row.try_get("lineage")?,
        job_id: row.try_get("job_id")?,
        judge_resolve: row.try_get("judge_resolve")?,
        context_fill: row.try_get("context_fill")?,
        node,
        job_requested: row.try_get::<Option<i64>, _>("requested")?.is_some(),
    }))
}

/// `posttool::unrecognized_policy_for` over facts already read: the same two conditions, an
/// unattended mode and a job the owner asked for, and `AsksAPerson` for everything else.
pub(super) fn unrecognized(
    facts: Option<&RunFacts>,
    mode: &str,
) -> crate::governance::classifier::Unrecognized {
    use crate::governance::classifier::Unrecognized;
    if crate::execution::runs::runs_unattended(mode) && facts.is_some_and(|f| f.job_requested) {
        Unrecognized::MayBeConfined
    } else {
        Unrecognized::AsksAPerson
    }
}

#[cfg(test)]
pub(super) fn set_test_ttl(window: Duration) {
    TEST_TTL.with(|ttl| ttl.set(window));
}

#[cfg(test)]
pub(super) fn loads() -> usize {
    LOADS.with(std::cell::Cell::get)
}

/// How many entries the cache holds for `pool`.
#[cfg(test)]
pub(super) fn entries_for(pool: &SqlitePool) -> usize {
    let id = pool_id(pool);
    lock().keys().filter(|(pool, _)| *pool == id).count()
}
