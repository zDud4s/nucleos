//! How a project leaves the núcleo, and what it leaves behind.
//!
//! **One project could be added and none could leave.** Every other pillar of this app has both
//! directions — a command can be deleted, a workflow ejected, a proposal answered — and the roster
//! had only the one. So it accumulated: a folder somebody moved, a repository they finished with, a
//! project added to try something once. They stay on the roster, they are polled every few seconds,
//! and on a page ordered by trouble they sit near the top for ever.
//!
//! **Removing a project and deleting its folder are two acts and this module only does the first.**
//! Taking a project off the roster touches nothing on the disk and is undone by adding the folder
//! again; deleting the folder is the one thing this app could ever do that nothing undoes. Giving
//! them one route with a flag would be giving them one weight, and the flag would eventually be
//! passed by something that meant the other one.
//!
//! What the record is for: the owner's standing decision is that history is kept unless somebody
//! says otherwise, at the moment they say it. A checkbox reading "forget the history too" over a
//! number nobody can see is not a decision — so `record` counts what is actually there first, and
//! the shell shows it beside the checkbox.

use serde::Serialize;
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};

/// Every table in this database that carries a `project_id`, minus `autopilot_state` itself.
///
/// **Written out, and held honest by a test rather than by a loop.** Deriving this from
/// `sqlite_master` at runtime would delete out of a table nobody had ever considered — a schema
/// change would silently widen what "forget the history" means, which is the one thing about a
/// destructive operation that must never move without somebody agreeing to it. So the list is
/// explicit, and `every_project_scoped_table_is_listed` reads the live schema and fails the day a
/// migration adds the twenty-second. The fix then is a person deciding whether the new table is
/// part of a project's history, which is exactly the decision that should not be automatic.
///
/// `autopilot_state` is absent because it is not history: it is the roster row, and it goes whether
/// or not the history does.
const PROJECT_SCOPED: &[&str] = &[
    "attention_heartbeats",
    "browser_profiles",
    "browser_sessions",
    "browser_sites",
    "browser_writes",
    // Dev time is project history, and it goes with the project. Its file offsets in
    // `devtime_files` are deliberately KEPT (owner's decision, 2026-10-05): re-adding the folder
    // then counts time from that moment on, instead of resurrecting the old time from transcripts
    // that still exist on disk.
    "devtime_cwd_map",
    // The parent of five session-keyed tables; those are written out in `remove`, ahead of the loop.
    "devtime_sessions",
    // Pending distillations are the project's history; a forgotten project must not be distilled later.
    "distill_queue",
    "feed",
    "fleet_exclusions",
    "jobs",
    // One row per lineage that got its correction (spec B D6). It is the guard that makes a lineage's
    // correction happen once, so it is part of the project's run history and goes with it: a project
    // forgotten and added back starts with no lineage, and a stale guard would only point at runs
    // that no longer exist.
    "judge_corrections",
    "map_decisions",
    "project_commands",
    // The same species as its neighbours, and the resurrection argument below bites hardest here:
    // a row in this table is a standing grant to perform a git operation — a push, a merge — for an
    // autonomous run without asking anybody. Forgetting a project and adding the folder back must
    // not hand its runs that authority again on the strength of a decision nobody remembers making.
    "project_git_ops",
    "project_github_ops",
    // Per-project configuration, the same species as its three neighbours here: which brain judges
    // what the rules did not recognise, on this project. It is history in the sense this list means
    // — forgetting a project and then adding the folder back must not silently resurrect a judge
    // somebody configured and has since forgotten choosing.
    "project_judge",
    "project_land_targets",
    "project_shell_rules",
    "project_slots",
    "proposals",
    "repo_trigger_state",
    "run_presets",
    "runs",
    "scheduler_state",
    "vcs_requests",
    "verify_runs",
    "webhook_deliveries",
    "worktree_touched_paths",
    "worktrees",
];

/// The tables that belong to a project through a parent rather than by carrying its name.
///
/// `(child, its column, the project-scoped parent)`, joined on the parent's `id`.
///
/// **This list exists because the schema-honesty test above found it missing.** `map_stamps` and
/// `map_triage` were written into `PROJECT_SCOPED` from a scan of the migrations, and both of them
/// say, in a comment on the column: *this table has no `project_id`, and that is on purpose — a
/// decision already belongs to exactly one project and a second copy of that fact is a second place
/// for it to be wrong*. The consequence they name is exactly this one: every read, and every
/// delete, has to go through `map_decisions` to know whose it is.
///
/// Had `foreign_keys` been off, forgetting a project's history would have deleted the decisions and
/// left every stamp and every triage verdict behind, pointing at rows that no longer exist. It is
/// on — `storage.rs` sets it — so the real outcome was a refused DELETE rather than a silent
/// orphan, which is the better failure and still a failure.
const VIA_PARENT: &[(&str, &str, &str)] = &[
    ("job_items", "job_id", "jobs"),
    ("job_notes", "job_id", "jobs"),
    ("map_anchors", "decision_id", "map_decisions"),
    ("map_stamps", "decision_id", "map_decisions"),
    ("map_triage", "decision_id", "map_decisions"),
    // A run's briefing trail. It belongs to the project through its run and through nothing else,
    // which is enough: a trace row only exists if there is a run, and every run of this project is
    // being deleted in this same transaction.
    //
    // Its sibling `knowledge_events` is NOT here and could not be: this list joins on the `id` of a
    // parent that is itself project-scoped, and `knowledge` is not — the store keeps its scope in
    // two columns. See the two statements written out in `remove`.
    ("run_knowledge", "run_id", "runs"),
];

/// What a project has on record, in the nouns somebody would recognise.
///
/// Not every one of the forty-two tables the forget clears: `scheduler_state` and
/// `repo_trigger_state` are bookkeeping nobody has ever seen a screen for, and a count of them
/// would be a number that makes the decision harder rather than easier. These seven are the ones
/// this app has surfaces for, so each one is a thing the reader can picture losing.
#[derive(Debug, Default, Serialize, sqlx::FromRow)]
pub struct Forgets {
    pub runs: i64,
    pub jobs: i64,
    pub proposals: i64,
    pub decisions: i64,
    pub stamps: i64,
    pub commands: i64,
    pub feed: i64,
}

/// What is still going on in this project right now.
///
/// **A different question from `Forgets`, and kept in a different struct for that reason.** One is
/// what a removal would erase and the other is what stops it happening at all; flattening them into
/// one bag of numbers would make a screen read "312 runs, 1 slot" as if those were the same kind of
/// fact, and only one of them is a reason the button will refuse.
#[derive(Debug, Default, Serialize, sqlx::FromRow)]
pub struct Holds {
    /// Slots taken right now — a run, a job or one of a job's items, working here.
    pub slots: i64,
    /// Worktrees still checked out on the disk: a row in `worktrees` with no `removed_at`.
    ///
    /// Counted separately from the slots because the two come apart in both directions. A run can
    /// hold a slot before it has checked anything out, and a worktree can outlive the work that made
    /// it — and a removal that forgot the second kind would strand a directory that nothing in this
    /// app can find again, which is a worse outcome than the removal being refused.
    pub worktrees: i64,
}

/// What a project would lose, and what is holding it.
#[derive(Debug, Default, Serialize)]
pub struct ProjectRecord {
    pub forgets: Forgets,
    pub holds: Holds,
}

/// What happened when a removal was asked for.
#[derive(Debug)]
pub enum Removed {
    /// The roster row is gone. `forgotten` is how many history rows went with it — zero when the
    /// history was kept, which is the default.
    Done { forgotten: u64 },
    /// Nothing on the roster answers to that name.
    NotRegistered,
    /// Work is still in flight here. Nothing was touched.
    Held(Holds),
    /// Something outside this project's history still points into it, so forgetting it would leave
    /// a dangling reference. Nothing was touched, and removing without forgetting still works.
    Referenced,
}

/// One read of everything a removal would touch, or `None` for a project nobody registered.
///
/// **The roster row is asked for first, and not inferred from the counts.** A project with no
/// history and a project that does not exist both count zero of everything, and only one of them
/// has a remove control that could ever work — a screen that could not tell them apart would offer
/// to remove something that was never there and report success.
///
/// Scalar subqueries in a single statement rather than seven round trips, for the reason
/// `autopilot::shadow_readiness` gives about its own: a per-table query would turn one panel into
/// seven.
pub async fn record(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Option<ProjectRecord>> {
    let registered: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM autopilot_state WHERE project_id = ?)")
            .bind(project_id)
            .fetch_one(pool)
            .await?;
    if !registered {
        return Ok(None);
    }

    let forgets: Forgets = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM runs             WHERE project_id = ?1) AS runs,
                (SELECT COUNT(*) FROM jobs             WHERE project_id = ?1) AS jobs,
                (SELECT COUNT(*) FROM proposals        WHERE project_id = ?1) AS proposals,
                (SELECT COUNT(*) FROM map_decisions    WHERE project_id = ?1) AS decisions,
                -- Through the decision, because `map_stamps` deliberately carries no `project_id`
                -- of its own — 0124's column comment argues for exactly that, and names this join
                -- as the price. A count that skipped it would be a count of every owner's stamps.
                (SELECT COUNT(*) FROM map_stamps
                   JOIN map_decisions ON map_decisions.id = map_stamps.decision_id
                  WHERE map_decisions.project_id = ?1)                        AS stamps,
                (SELECT COUNT(*) FROM project_commands WHERE project_id = ?1) AS commands,
                (SELECT COUNT(*) FROM feed             WHERE project_id = ?1) AS feed",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;

    Ok(Some(ProjectRecord {
        forgets,
        holds: holds(pool, project_id).await?,
    }))
}

/// The two counts that decide whether a removal is allowed at all.
///
/// Its own function because `remove` runs it a second time inside the write transaction, and a
/// check written twice is a check that eventually disagrees with itself.
async fn holds<'e, E>(executor: E, project_id: &str) -> sqlx::Result<Holds>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM project_slots WHERE project_id = ?1) AS slots,
                (SELECT COUNT(*) FROM worktrees
                  WHERE project_id = ?1 AND removed_at IS NULL)            AS worktrees",
    )
    .bind(project_id)
    .fetch_one(executor)
    .await
}

/// Take a project off the roster, optionally forgetting everything it did.
///
/// **The delete happens before the check, and that ordering is the whole guard.** SQLite hands out
/// the write lock on the first statement that writes, so deleting the roster row first is what stops
/// a run claiming a slot in the window between "nothing is in flight" and "the project is gone".
/// Checked first and deleted after, this would have a race that shows up as a slot held by a project
/// that no longer exists — rare, unreproducible, and permanent. Held, the transaction rolls back and
/// the row is still there.
///
/// **`defer_foreign_keys` is what makes the forget expressible at all.** `storage.rs` runs with
/// `foreign_keys` on, and a project's own history references itself in both directions — a run
/// naming its successor, a job naming the runs that are its items — so there is no order in which
/// forty-two immediate deletes all succeed. Deferred, the checks all happen at `COMMIT`, by which
/// point everything that had to go has gone. It also means the one failure left is the honest one:
/// something OUTSIDE this project's history still points into it, and the commit refuses rather than
/// leaving a dangling reference.
pub async fn remove(
    pool: &SqlitePool,
    project_id: &str,
    forget_history: bool,
) -> sqlx::Result<Removed> {
    let mut tx = pool.begin().await?;
    sqlx::query("PRAGMA defer_foreign_keys = ON")
        .execute(&mut *tx)
        .await?;

    let gone = sqlx::query("DELETE FROM autopilot_state WHERE project_id = ?")
        .bind(project_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if gone == 0 {
        tx.rollback().await?;
        return Ok(Removed::NotRegistered);
    }

    let holding = holds(&mut *tx, project_id).await?;
    if holding.slots > 0 || holding.worktrees > 0 {
        tx.rollback().await?;
        return Ok(Removed::Held(holding));
    }

    let mut forgotten = 0;
    if forget_history {
        // Every table name below is interpolated and every value is bound, which is the only shape
        // available: a table name cannot be a parameter in SQLite. `AssertSqlSafe` because sqlx
        // otherwise takes only `&'static str`, and the audit it asks for is short — every name comes
        // from the two consts at the top of this file, no caller reaches either with a string of its
        // own, and the one value that does come from a caller is the bound `?`.
        //
        // Two shapes neither list can express, and both are this store's. The scope is two columns,
        // so `PROJECT_SCOPED`'s `WHERE project_id = ?` does not reach it; and a job-scoped row names
        // its job in a polymorphic TEXT column with no foreign key, so `VIA_PARENT`'s join on a
        // parent's `id` does not either. Written out here rather than bent into a list, because a
        // list entry that means something different from its neighbours is a list nobody can read.
        //
        // **BEFORE the two loops, and that position is load-bearing.** `defer_foreign_keys` makes
        // the order indifferent to the foreign-key CHECKS, and it does nothing at all for a
        // statement that READS a table another statement deletes: `jobs` is in `PROJECT_SCOPED`, so
        // run after that loop these two find no jobs, match no rows, and leave every job-scoped row
        // behind — silently, and for ever. Measured, not reasoned: written after the loop, both
        // `forgetting_a_project_also_takes_the_knowledge_of_its_jobs` and
        // `forgetting_a_project_leaves_no_history_of_the_knowledge_it_took` failed with alpha's job
        // row still on record.
        //
        // `knowledge_events` before `knowledge` for the same reason one step down: it reads the
        // rows the next statement deletes.
        //
        // CAST, and it is not decoration. `jobs.id` is INTEGER (`0042_jobs.sql:5`) and `scope_id` is
        // TEXT, so comparing them without it matches NOTHING — the same silent nothing as the wrong
        // position above, arriving by a different route.
        //
        // `machine` appears in neither statement, and that is correct: machine rows are not a
        // project's.
        forgotten += sqlx::query(
            "DELETE FROM knowledge_events
              WHERE knowledge_id IN (
                    SELECT id FROM knowledge
                     WHERE (scope_kind = 'project' AND scope_id = ?1)
                        OR (scope_kind = 'job'
                            AND scope_id IN (SELECT CAST(id AS TEXT) FROM jobs WHERE project_id = ?1)))",
        )
        .bind(project_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        forgotten += sqlx::query(
            "DELETE FROM knowledge
              WHERE (scope_kind = 'project' AND scope_id = ?1)
                 OR (scope_kind = 'job'
                     AND scope_id IN (SELECT CAST(id AS TEXT) FROM jobs WHERE project_id = ?1))",
        )
        .bind(project_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        // The devtime children hang off `devtime_sessions` by `session_id` (TEXT, no foreign key, and
        // the parent's key is `session_id`, not `id`), which `VIA_PARENT`'s join cannot express.
        // BEFORE the `PROJECT_SCOPED` loop for the reason given above: the subquery reads
        // `devtime_sessions`, which that loop deletes. `devtime_files` and `devtime_ingest_status`
        // are deliberately not touched: the offsets are what stop a re-added folder from being
        // re-ingested from the start.
        for child in [
            "devtime_turns",
            "devtime_messages",
            "devtime_attempts",
            "devtime_spans",
            "devtime_markers",
        ] {
            forgotten += sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {child}
                  WHERE session_id IN (SELECT session_id FROM devtime_sessions WHERE project_id = ?)"
            )))
            .bind(project_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        }

        // Children first. The deferral above means it would work in any order, and doing it in the
        // order the rows actually depend on keeps the statement log readable when one of them fails.
        for (child, key, parent) in VIA_PARENT {
            forgotten += sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {child}
                  WHERE {key} IN (SELECT id FROM {parent} WHERE project_id = ?)"
            )))
            .bind(project_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        }
        for table in PROJECT_SCOPED {
            forgotten += sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE project_id = ?"
            )))
            .bind(project_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        }
    }

    // Where every deferred check lands. A failure here is one specific thing — something outside
    // this project's history still points into it, a team that directed one of its runs being the
    // case that exists — and it is worth its own answer rather than a 500: the escape is to remove
    // the project and keep the history, which is a checkbox away and is the default besides.
    match tx.commit().await {
        Ok(()) => Ok(Removed::Done { forgotten }),
        Err(error) if is_foreign_key_violation(&error) => Ok(Removed::Referenced),
        Err(error) => Err(error),
    }
}

/// SQLite's `SQLITE_CONSTRAINT_FOREIGNKEY`, which is `787` and reaches us as a string.
///
/// Matched on the extended code rather than on the message, because the message is prose that has
/// been reworded between SQLite releases and a `contains("FOREIGN KEY")` would go quiet the first
/// time it was.
fn is_foreign_key_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db.code().as_deref() == Some("787"))
}

/* ------------------------------------------------------------------ the folder -- */

/// Why a folder may not be deleted, in the words the screen will use.
///
/// **Every one of these is checked before anything is touched**, and every one names what to do
/// instead. A refusal somebody cannot act on is a refusal they work around.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsafe {
    /// The project has no folder recorded, so there is nothing on a disk to delete.
    NoRoot,
    /// A drive root, a home directory, or a path one level below a root.
    TooBig(String),
    /// Another registered project lives inside this folder, and would go with it.
    HoldsAnother(String),
    /// Forgetting the history is what got refused, not the folder. Its own variant because the
    /// escape is the opposite of giving up: untick the box and the same button works.
    HistoryReferenced,
}

impl Unsafe {
    /// The sentence, built where the reason is known rather than in the shell.
    pub fn detail(&self) -> String {
        match self {
            Self::NoRoot => {
                "this project has no folder recorded, so there is nothing to delete".into()
            }
            Self::TooBig(path) => format!(
                "{path} is too near the top of a disk for this app to delete — \
                 delete it yourself if that is really what you want"
            ),
            Self::HoldsAnother(other) => format!(
                "{other} is registered inside this folder and would go with it — \
                 remove that project first, or delete the folder yourself"
            ),
            Self::HistoryReferenced => "something outside this project still points at its \
                 history — untick the box and the folder still goes"
                .into(),
        }
    }

    /// The stable name the shell switches on.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoRoot => "no_root",
            Self::TooBig(_) => "root_too_big",
            Self::HoldsAnother(_) => "holds_another_project",
            Self::HistoryReferenced => "history_referenced",
        }
    }
}

/// A path this app will not delete, whatever it is asked.
///
/// **Two normal components, minimum.** A drive root, `/`, and `C:\repos` are all refused;
/// `C:\repos\alpha` is not. The rule is deliberately blunt and deliberately strict: the cost of
/// refusing a legitimate `D:\work` is one sentence telling somebody to delete it themselves, and
/// the cost of the other mistake is a disk. A relative path is refused outright — there is nothing
/// to resolve it against here, and removing a relative path removes it relative to whatever the
/// daemon's working directory happens to be.
fn too_big_to_delete(root: &Path) -> bool {
    // `dirs` is not a dependency here and does not need to be: these two are what every shell on
    // both platforms sets, and a machine that sets neither falls through to the component rule
    // rather than to a delete.
    let homes = ["USERPROFILE", "HOME"]
        .into_iter()
        .filter_map(|variable| std::env::var(variable).ok());
    too_big_beside(root, homes)
}

/// The rule itself, with the home directories handed in.
///
/// Split from the reader above so a test can state which homes it is reasoning about. The
/// alternative is mutating the process environment inside a test, which is unsound with a
/// multi-threaded runner and is exactly the shape `nothing_sets_the_worktree_root_without_restoring
/// _it` exists elsewhere in this crate to catch.
fn too_big_beside(root: &Path, homes: impl IntoIterator<Item = String>) -> bool {
    use std::path::Component;

    if !root.is_absolute() {
        return true;
    }
    let normal = root
        .components()
        .filter(|part| matches!(part, Component::Normal(_)))
        .count();
    if normal < 2 {
        return true;
    }
    // The home directory, and everything it is inside. Both directions matter and only one is
    // obvious: `~` itself is the obvious one, and `C:/Users` is the one that gets missed — and that
    // one takes every account on the machine with it.
    for home in homes {
        let home = PathBuf::from(home);
        if home.as_os_str().is_empty() {
            continue;
        }
        if root == home || home.starts_with(root) {
            return true;
        }
    }
    false
}

/// Whether this project's folder may be deleted, and why not when it may not.
///
/// Its own function because the screen asks it before drawing the control and the route asks it
/// again before acting: a guard the caller is trusted to have run is a guard the second caller
/// forgets.
pub async fn folder_check(
    pool: &SqlitePool,
    project_id: &str,
    root: Option<&str>,
) -> sqlx::Result<Result<PathBuf, Unsafe>> {
    let Some(root) = root.filter(|path| !path.trim().is_empty()) else {
        return Ok(Err(Unsafe::NoRoot));
    };
    let root = PathBuf::from(root);
    if too_big_to_delete(&root) {
        return Ok(Err(Unsafe::TooBig(root.to_string_lossy().into_owned())));
    }

    // A project registered inside this one. Cheap — the roster is tens of rows — and it is the
    // mistake nothing else would catch: a monorepo registered alongside one of its own packages,
    // where deleting the outer one takes the inner one's folder with it and leaves its roster row
    // pointing at nothing.
    let others: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT project_id, project_root FROM autopilot_state WHERE project_id != ?",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    for (other_id, other_root) in others {
        let Some(other_root) = other_root else {
            continue;
        };
        if Path::new(&other_root).starts_with(&root) {
            return Ok(Err(Unsafe::HoldsAnother(other_id)));
        }
    }

    Ok(Ok(root))
}

/// What happened when a folder deletion was asked for.
#[derive(Debug)]
pub enum Deleted {
    /// The folder is gone and the project is off the roster.
    Done { forgotten: u64 },
    /// Nothing on the roster answers to that name.
    NotRegistered,
    /// Work is still in flight here. Nothing was touched.
    Held(Holds),
    /// The folder is one this app will not delete.
    Refused(Unsafe),
    /// The roster row went and the folder did not, wholly or in part. The one outcome that leaves
    /// the machine in a state somebody has to finish by hand, so it carries the path and the
    /// operating system's own words.
    Partial { root: String, error: String },
}

/// Delete a project's folder, and take it off the roster.
///
/// **The roster row goes first and the folder second, and the order is chosen rather than
/// incidental.** Off the roster nothing schedules work into this folder — `autopilot_projects`
/// selects on `mode IN ('shadow','active')`, and there is no row left to select — so the directory
/// is not being written to while it is being removed. The other order has the failure that matters:
/// a removal that stops halfway leaves a half-deleted repository that a live project is still
/// pointed at, and the next tick runs a gate inside it.
///
/// The price is `Partial`, which is a real outcome and is reported as one. A row removed and a
/// folder that would not go is recoverable by hand and is visible; the reverse is neither.
pub async fn delete_folder(
    pool: &SqlitePool,
    project_id: &str,
    forget_history: bool,
) -> sqlx::Result<Deleted> {
    let recorded: Option<Option<String>> =
        sqlx::query_scalar("SELECT project_root FROM autopilot_state WHERE project_id = ?")
            .bind(project_id)
            .fetch_optional(pool)
            .await?;
    let Some(recorded) = recorded else {
        return Ok(Deleted::NotRegistered);
    };

    // The recorded root, and never a path from the request. There is no argument to this function
    // that could name a directory, which is the property that makes the guards worth having.
    let root = match folder_check(pool, project_id, recorded.as_deref()).await? {
        Ok(root) => root,
        Err(refusal) => return Ok(Deleted::Refused(refusal)),
    };

    match remove(pool, project_id, forget_history).await? {
        Removed::Done { forgotten } => {
            let path = root.clone();
            let outcome = tokio::task::spawn_blocking(move || remove_tree(&path))
                .await
                .map_err(|error| {
                    sqlx::Error::Protocol(format!("the delete task failed: {error}"))
                })?;
            match outcome {
                Ok(()) => Ok(Deleted::Done { forgotten }),
                Err(error) => Ok(Deleted::Partial {
                    root: root.to_string_lossy().into_owned(),
                    error: error.to_string(),
                }),
            }
        }
        Removed::NotRegistered => Ok(Deleted::NotRegistered),
        Removed::Held(holds) => Ok(Deleted::Held(holds)),
        Removed::Referenced => Ok(Deleted::Refused(Unsafe::HistoryReferenced)),
    }
}

/// A recursive removal, with the two things the standard one does not do on this machine.
///
/// **Permissions in the way.** Which permission blocks a removal is a platform question, and the
/// answer on one platform is wrong on the other. On Windows the obstacle is the read-only ATTRIBUTE
/// of the file: git marks everything under `.git/objects` read-only, and a plain removal refuses
/// such a file with `Access is denied`, so the attribute is cleared on the way down. On Unix a
/// file's own write bit is never consulted by `unlink` — the write bit of the DIRECTORY holding the
/// entry is — so clearing the file's would buy no deletion at all. What has to be widened there is
/// a directory missing its owner-write bit, `0o555` being how Go's module cache ships its
/// directories. Both halves live in `prepare_for_removal`, which is called on the way down. A
/// repository is the overwhelmingly common case here, which makes this the ordinary path rather
/// than an edge one.
///
/// **Junctions and symlinks.** `symlink_metadata` rather than `metadata`, and a link is unlinked
/// rather than descended into. Not hypothetical in this repository: `nucleos/target` is a junction
/// to the shared `C:/Projects/.cargo-target` that every crate on this machine builds into.
/// Following it would delete twenty other projects' build output — and would do it while reporting
/// that it had deleted one project's folder.
fn remove_tree(path: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;

    if meta.is_symlink() {
        // A directory symlink or junction is removed as a directory and a file symlink as a file.
        // Neither touches what it points at.
        return if meta.is_dir() {
            std::fs::remove_dir(path)
        } else {
            std::fs::remove_file(path)
        };
    }

    prepare_for_removal(path, &meta);

    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            remove_tree(&entry?.path())?;
        }
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Widens exactly what the removal underneath needs, which is not the same permission on the two
/// platforms.
///
/// On Windows the read-only attribute belongs to the entry itself and a removal refuses while it is
/// set, so it is cleared — the arm below is that rule and nothing else.
///
/// On Unix the same gesture would be a leak in exchange for nothing. `set_readonly(false)` grants
/// write to the owner, the group AND everybody else — `0o444` becomes `0o666` — which is precisely
/// why clippy ships `permissions_set_readonly_false` as a lint. And it buys no deletion: `unlink`
/// never consults the FILE's write bit, only the write bit of the DIRECTORY the entry lives in. So
/// files are left exactly as they are. A directory is the one thing that can genuinely stand in the
/// way: `0o555` grants read and execute, which is enough to walk it and not enough to unlink
/// anything inside it, and that is the mode Go's module cache writes for every directory it
/// creates. The grant is the owner's three bits and stops there; the group and the world need
/// nothing, because the process doing the deleting is the owner.
fn prepare_for_removal(path: &Path, meta: &std::fs::Metadata) {
    #[cfg(windows)]
    {
        let mut permissions = meta.permissions();
        if permissions.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            // Best effort: a permission that will not clear is reported by the removal below, in words
            // about the file that actually refused rather than about this attempt.
            let _ = std::fs::set_permissions(path, permissions);
        }
    }

    #[cfg(unix)]
    if meta.is_dir() {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        if mode & 0o700 != 0o700 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o700));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **`foreign_keys` on, because `storage.rs` runs with it on.**
    ///
    /// SQLite's default is off, and a pool built without this would be testing a different database
    /// from the one the daemon opens — one where deleting a decision quietly orphans its stamps
    /// instead of refusing. Every claim this module makes about what a forget can and cannot do
    /// depends on the setting being the real one.
    async fn pool() -> SqlitePool {
        // The URL form and then `.foreign_keys(true)` — both halves copied from `email::test_pool`,
        // which already paid for each of them. Built from a filename instead, every connection gets
        // a private in-memory database and the migrations land on one the queries never see; and
        // without the second, an FK bug lives for a week because no test has the constraint on.
        let options: sqlx::sqlite::SqliteConnectOptions = "sqlite::memory:".parse().unwrap();
        let pool = SqlitePool::connect_with(options.foreign_keys(true))
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn register(pool: &SqlitePool, project_id: &str) {
        sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES (?, 'shadow')")
            .bind(project_id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn on_roster(pool: &SqlitePool, project_id: &str) -> bool {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM autopilot_state WHERE project_id = ?")
            .bind(project_id)
            .fetch_one(pool)
            .await
            .unwrap()
            > 0
    }

    /// **The list is the destructive part, so the schema is what holds it honest.**
    ///
    /// A migration that adds a table keyed on `project_id` and does not touch this file leaves a
    /// "forget the history" that quietly keeps some of it — and nobody would find out, because the
    /// only symptom is a row surviving in a table nobody looked at. This fails on the day the
    /// migration lands, while somebody is still holding the reason for it.
    #[tokio::test]
    async fn every_project_scoped_table_is_listed() {
        let pool = pool().await;
        let live: Vec<String> = sqlx::query_scalar(
            "SELECT m.name
             FROM sqlite_master AS m
             JOIN pragma_table_info(m.name) AS c
             WHERE m.type = 'table' AND c.name = 'project_id' AND m.name != 'autopilot_state'
             ORDER BY m.name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        let listed: Vec<String> = PROJECT_SCOPED
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        assert_eq!(
            live, listed,
            "a table carrying project_id is missing from PROJECT_SCOPED (or listed and gone). \
             Decide whether it is part of a project's history, then add or remove it."
        );
    }

    /// **And the tables that belong to a project through a parent, which is the harder half.**
    ///
    /// `map_stamps` and `map_triage` carry no `project_id` at all, on purpose — 0124 spends a
    /// paragraph on why, and names this join as the price. A list built by scanning for the column
    /// misses them, and the forget then deletes their parent decisions and leaves them behind. This
    /// asks the schema which tables point at a project-scoped parent and requires every one of them
    /// to have been considered.
    ///
    /// Self-references are excluded — a run naming its successor, one thing known naming what it
    /// supersedes — because the parent is already going and the child is the same table.
    #[tokio::test]
    async fn every_child_of_a_project_scoped_table_is_listed() {
        let pool = pool().await;
        let owned: Vec<String> = PROJECT_SCOPED
            .iter()
            .map(|name| (*name).to_owned())
            .collect();

        let mut found: Vec<String> = Vec::new();
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        for table in tables {
            if owned.contains(&table) {
                continue;
            }
            let parents: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                r#"SELECT "table" FROM pragma_foreign_key_list('{table}')"#
            )))
            .fetch_all(&pool)
            .await
            .unwrap();
            if parents.iter().any(|parent| owned.contains(parent)) {
                found.push(table);
            }
        }

        // What the list holds, plus what it deliberately does not. `team_runs.director_run_id` and
        // `team_items.run_id` point INTO this history from outside it: a team is not a project's,
        // and a run it once directed is. They are the case `Removed::Referenced` exists for, and
        // they are named here rather than quietly passing a subset check — a new table appearing on
        // this side is a new way for a forget to be refused, and somebody should have to say so.
        let listed: Vec<&str> = VIA_PARENT
            .iter()
            .map(|(child, _, _)| *child)
            .chain(["team_items", "team_runs"])
            .collect();
        let mut expected: Vec<String> = listed.iter().map(|name| (*name).to_owned()).collect();
        expected.sort();
        found.sort();

        assert_eq!(
            found, expected,
            "a table hangs off a project-scoped one and is in neither VIA_PARENT nor the excused \
             list. Decide whether the project owns it — if it does, forgetting must delete it; if \
             it does not, it will block the commit and `Removed::Referenced` is the answer."
        );
    }

    /// The default, and the one the owner asked for: the row goes, the record stays.
    #[tokio::test]
    async fn removing_keeps_the_history_unless_it_is_asked_to_forget() {
        let pool = pool().await;
        register(&pool, "alpha").await;
        sqlx::query("INSERT INTO runs (project_id, prompt, status, created_at) VALUES ('alpha', 'go', 'done', '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();

        let removed = remove(&pool, "alpha", false).await.unwrap();
        assert!(matches!(removed, Removed::Done { forgotten: 0 }));
        assert!(!on_roster(&pool, "alpha").await);

        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE project_id = 'alpha'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            runs, 1,
            "the history is kept unless somebody says otherwise"
        );
    }

    /// A project, a job of that project, a run of that job, and one thing known at each of the two
    /// scopes the forget has to reach — plus the trail that says the run was briefed.
    ///
    /// One helper and not four seeded blocks, because the four tests below differ in what they
    /// assert and not in what they are about: forgetting a project that used the store.
    async fn briefed_project(pool: &SqlitePool, project_id: &str) -> (i64, i64) {
        register(pool, project_id).await;
        let job: i64 = sqlx::query_scalar(
            "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
             VALUES (?, 'C:/tmp', 'done', 1, '2026-01-01T00:00:00Z') RETURNING id",
        )
        .bind(project_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let run: i64 = sqlx::query_scalar(
            "INSERT INTO runs (project_id, job_id, prompt, status, created_at)
             VALUES (?, ?, 'go', 'done', '2026-01-01T00:00:00Z') RETURNING id",
        )
        .bind(project_id)
        .bind(job)
        .fetch_one(pool)
        .await
        .unwrap();

        for (scope_kind, scope_id, layer) in [
            ("project", project_id.to_owned(), "semantic"),
            ("job", job.to_string(), "working"),
        ] {
            let known: i64 = sqlx::query_scalar(
                "INSERT INTO knowledge
                   (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
                 VALUES (?, ?, ?, 'run', 'memory', 'a lesson', 'body', 'active',
                         '2026-01-01T00:00:00Z') RETURNING id",
            )
            .bind(layer)
            .bind(scope_kind)
            .bind(&scope_id)
            .fetch_one(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
                 VALUES (?, 'proposed', 'active', 'approved by the owner', '2026-01-01T00:00:00Z')",
            )
            .bind(known)
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO run_knowledge
                   (run_id, knowledge_id, shown, s_fts, s_scope, s_structure, s_recency, s_use, at)
                 VALUES (?, ?, 1, 0.0, 1.0, 0.0, 0.0, 0.0, '2026-01-01T00:00:00Z')",
            )
            .bind(run)
            .bind(known)
            .execute(pool)
            .await
            .unwrap();
        }
        (job, run)
    }

    async fn count(pool: &SqlitePool, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
    }

    /// Forgetting a project whose runs received a briefing must not fail at COMMIT. Before
    /// `run_knowledge` joined `VIA_PARENT` this presented as `Removed::Referenced` — "something
    /// outside this project's history still points into it" — the right message for the wrong
    /// mechanism. And `every_project_scoped_table_is_listed` structurally cannot catch it: its query
    /// filters on `c.name = 'project_id'`, and this table has no such column.
    #[tokio::test]
    async fn forgetting_a_project_whose_runs_were_briefed_does_not_fail_at_commit() {
        let pool = pool().await;
        briefed_project(&pool, "alpha").await;

        let removed = remove(&pool, "alpha", true).await.unwrap();
        assert!(
            matches!(removed, Removed::Done { .. }),
            "the forget was refused rather than performed: {removed:?}"
        );
        assert_eq!(
            count(&pool, "SELECT COUNT(*) FROM run_knowledge").await,
            0,
            "the briefing trail of a forgotten project is still on record"
        );
    }

    /// The scope lives in two columns now, so `DELETE ... WHERE project_id = ?` no longer reaches
    /// it. And machine-scoped rows are not a project's to forget.
    #[tokio::test]
    async fn forgetting_a_project_takes_its_project_scope_and_leaves_the_machine_alone() {
        let pool = pool().await;
        briefed_project(&pool, "alpha").await;
        briefed_project(&pool, "bravo").await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', 'machine', NULL, 'owner', 'prompt', 'about the house', 'body',
                     'active', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        remove(&pool, "alpha", true).await.unwrap();

        assert_eq!(
            count(
                &pool,
                "SELECT COUNT(*) FROM knowledge WHERE scope_kind = 'project' AND scope_id = 'alpha'"
            )
            .await,
            0,
            "the project's own knowledge outlived the project"
        );
        assert_eq!(
            count(
                &pool,
                "SELECT COUNT(*) FROM knowledge WHERE scope_kind = 'machine'"
            )
            .await,
            1,
            "forgetting one project took what is known about the house"
        );
        assert_eq!(
            count(
                &pool,
                "SELECT COUNT(*) FROM knowledge WHERE scope_kind = 'project' AND scope_id = 'bravo'"
            )
            .await,
            1,
            "forgetting one project took another project's knowledge"
        );
    }

    /// The leak the spec found on its fourth pass: every `working` row is `scope_kind='job'`,
    /// `scope_id` is polymorphic and therefore has no FK, and neither list can say "delete where the
    /// job belongs to this project". Without a third form of DELETE, forgetting a project leaves the
    /// knowledge of its jobs behind FOR EVER — and the test first prescribed passed while the leak
    /// existed, because it only compared `project` against `machine`.
    #[tokio::test]
    async fn forgetting_a_project_also_takes_the_knowledge_of_its_jobs() {
        let pool = pool().await;
        let (alpha_job, _) = briefed_project(&pool, "alpha").await;
        let (bravo_job, _) = briefed_project(&pool, "bravo").await;

        remove(&pool, "alpha", true).await.unwrap();

        let left: Vec<String> = sqlx::query_scalar(
            "SELECT scope_id FROM knowledge WHERE scope_kind = 'job' ORDER BY scope_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            left,
            vec![bravo_job.to_string()],
            "what a forgotten project's jobs knew is still on record, or another project's went \
             with it (the forgotten job was {alpha_job})"
        );
    }

    /// And the history of those rows goes with them. Asserted rather than left to surface as a
    /// dangling-FK commit failure: the deferred check would catch it, but it would present as
    /// `Removed::Referenced` — the one message this whole task exists to stop meaning the wrong
    /// thing.
    #[tokio::test]
    async fn forgetting_a_project_leaves_no_history_of_the_knowledge_it_took() {
        let pool = pool().await;
        briefed_project(&pool, "alpha").await;
        briefed_project(&pool, "bravo").await;

        remove(&pool, "alpha", true).await.unwrap();

        assert_eq!(
            count(&pool, "SELECT COUNT(*) FROM knowledge_events").await,
            2,
            "the events of a forgotten project's knowledge outlived it, or bravo's went too"
        );
        assert_eq!(
            count(
                &pool,
                "SELECT COUNT(*) FROM knowledge_events AS e
                  WHERE NOT EXISTS (SELECT 1 FROM knowledge AS k WHERE k.id = e.knowledge_id)"
            )
            .await,
            0,
            "an event row points at knowledge that is gone"
        );
    }

    /// Dev time is project history and goes with the project; the file offsets are kept on purpose,
    /// so re-adding the folder does not re-ingest the old time from transcripts that still exist.
    #[tokio::test]
    async fn forgetting_a_project_takes_its_devtime_and_keeps_the_offsets() {
        let pool = pool().await;
        register(&pool, "alpha").await;
        register(&pool, "bravo").await;
        let stamp = "2026-01-01T00:00:00.000Z";
        for id in ["alpha", "bravo"] {
            let session = format!("sess-{id}");
            let statements = [
                "INSERT INTO devtime_sessions (session_id, project_id, parser_version, updated_at)
                 VALUES (?1, ?2, 1, ?3)",
                "INSERT INTO devtime_turns (session_id, seq, started_at, ended_at, parser_version)
                 VALUES (?1, 0, ?3, ?3, 1)",
                "INSERT INTO devtime_messages (session_id, lane, message_id, first_at, last_at, parser_version)
                 VALUES (?1, 'main', 'm1', ?3, ?3, 1)",
                "INSERT INTO devtime_attempts (attempt_id, session_id, lane, tool_use_id, kind, tool_name, started_at, outcome, parser_version)
                 VALUES ('att-' || ?1, ?1, 'main', 'tu1', 'tool', 'Bash', ?3, 'ok', 1)",
                "INSERT INTO devtime_spans (session_id, lane, kind, started_at, ended_at, confidence, parser_version)
                 VALUES (?1, 'main', 'work', ?3, ?3, 'high', 1)",
                "INSERT INTO devtime_markers (session_id, lane, ts, kind, parser_version)
                 VALUES (?1, 'main', ?3, 'note', 1)",
                "INSERT INTO devtime_cwd_map (cwd, project_id, kind, resolved_at)
                 VALUES ('/work/' || ?2, ?2, 'project', ?3)",
                "INSERT INTO devtime_files (path, session_id, offset, parser_version, updated_at)
                 VALUES ('/t/' || ?1 || '.jsonl', ?1, 4096, 1, ?3)",
            ];
            for sql in statements {
                sqlx::query(sql)
                    .bind(&session)
                    .bind(id)
                    .bind(stamp)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }

        let removed = remove(&pool, "alpha", true).await.unwrap();
        assert!(matches!(removed, Removed::Done { .. }));

        for (table, key, alpha_left, bravo_left) in [
            ("devtime_sessions", "session_id", 0, 1),
            ("devtime_turns", "session_id", 0, 1),
            ("devtime_messages", "session_id", 0, 1),
            ("devtime_attempts", "session_id", 0, 1),
            ("devtime_spans", "session_id", 0, 1),
            ("devtime_markers", "session_id", 0, 1),
            ("devtime_cwd_map", "cwd", 0, 1),
            // Kept on purpose: the offsets are what stop a re-added folder being re-ingested.
            ("devtime_files", "session_id", 1, 1),
        ] {
            for (id, expected) in [("alpha", alpha_left), ("bravo", bravo_left)] {
                let wanted = if key == "cwd" {
                    format!("/work/{id}")
                } else {
                    format!("sess-{id}")
                };
                let left: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                    "SELECT COUNT(*) FROM {table} WHERE {key} = ?"
                )))
                .bind(wanted)
                .fetch_one(&pool)
                .await
                .unwrap();
                assert_eq!(left, expected, "{table} rows left for {id}");
            }
        }

        let offset: i64 = sqlx::query_scalar(
            "SELECT offset FROM devtime_files WHERE path = '/t/sess-alpha.jsonl'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(offset, 4096, "the kept offset must be unchanged");
    }

    /// And when it IS asked, it takes the lot — including the tables no screen counts.
    #[tokio::test]
    async fn forgetting_clears_every_table_the_project_touched() {
        let pool = pool().await;
        register(&pool, "alpha").await;
        register(&pool, "bravo").await;
        for id in ["alpha", "bravo"] {
            sqlx::query("INSERT INTO runs (project_id, prompt, status, created_at) VALUES (?, 'go', 'done', '2026-01-01T00:00:00Z')")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO scheduler_state (project_id, rule_name, last_fired_at) VALUES (?, 'tick', '2026-01-01T00:00:00Z')")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            // The three tables §alcada-por-projecto added. At the SQL level on purpose — this test
            // is about the shared `DELETE FROM {table} WHERE project_id = ?` loop, not about
            // `project_policy`'s functions, which have no reason to exist yet by the time this runs.
            sqlx::query("INSERT INTO project_shell_rules (project_id, prefix, verdict, created_at) VALUES (?, 'bash scripts/gates.sh', 'allow', '2026-01-01T00:00:00Z')")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO project_github_ops (project_id, op_kind, created_at) VALUES (?, 'run_list', '2026-01-01T00:00:00Z')")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO project_land_targets (project_id, branch, created_at) VALUES (?, 'master', '2026-01-01T00:00:00Z')")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }

        let removed = remove(&pool, "alpha", true).await.unwrap();
        assert!(matches!(removed, Removed::Done { forgotten } if forgotten == 5));

        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE project_id = 'alpha'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(runs, 0);

        let shell_rules: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_shell_rules WHERE project_id = 'alpha'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(shell_rules, 0);

        let github_ops: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_github_ops WHERE project_id = 'alpha'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(github_ops, 0);

        let land_targets: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_land_targets WHERE project_id = 'alpha'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(land_targets, 0);

        // The other project is untouched, which is the property a `WHERE project_id = ?` written
        // thirty times has to get right thirty times.
        let theirs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE project_id = 'bravo'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(theirs, 1);

        let their_shell_rules: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_shell_rules WHERE project_id = 'bravo'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(their_shell_rules, 1);

        let their_github_ops: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_github_ops WHERE project_id = 'bravo'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(their_github_ops, 1);

        let their_land_targets: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM project_land_targets WHERE project_id = 'bravo'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(their_land_targets, 1);

        assert!(on_roster(&pool, "bravo").await);
    }

    /// A pending distillation is the project's history: forgetting the project must not leave a
    /// row that would later be distilled, and must not touch another project's queue.
    #[tokio::test]
    async fn forgetting_a_project_takes_its_distill_queue() {
        let pool = pool().await;
        register(&pool, "alpha").await;
        register(&pool, "bravo").await;
        // Job ids are global, so each project gets its own (the unique index has no project_id).
        for (id, job_id) in [("alpha", 1_i64), ("bravo", 2_i64)] {
            sqlx::query(
                "INSERT INTO distill_queue (cause, project_id, job_id, status, attempts, created_at)
                 VALUES ('job_failed', ?, ?, 'pending', 0, '2026-01-01T00:00:00Z')",
            )
            .bind(id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        }

        remove(&pool, "alpha", true).await.unwrap();

        let ours: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM distill_queue WHERE project_id = 'alpha'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(ours, 0, "a forgotten project's distill queue outlived it");
        let theirs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM distill_queue WHERE project_id = 'bravo'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(theirs, 1, "forgetting alpha took bravo's distill queue");
    }

    /// A slot taken here is work in flight, and nothing is removed under it.
    #[tokio::test]
    async fn a_project_with_work_in_flight_is_not_removed() {
        let pool = pool().await;
        register(&pool, "alpha").await;
        sqlx::query(
            "INSERT INTO project_slots (project_id, slot, owner_kind, owner_id, claimed_at)
             VALUES ('alpha', 0, 'run', 7, '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let removed = remove(&pool, "alpha", true).await.unwrap();
        match removed {
            Removed::Held(holding) => assert_eq!(holding.slots, 1),
            other => panic!("expected the removal to be refused, got {other:?}"),
        }
        // Refused means nothing happened, not "happened and then complained".
        assert!(on_roster(&pool, "alpha").await);
    }

    /// A checkout still on the disk is the one this cannot afford to forget.
    ///
    /// Deleting the `worktrees` row would leave a directory that nothing in this app can name any
    /// more — no run owns it, no project lists it, and the janitor reads that table to find it.
    #[tokio::test]
    async fn a_project_with_a_live_worktree_is_not_removed() {
        let pool = pool().await;
        register(&pool, "alpha").await;
        sqlx::query(
            "INSERT INTO worktrees
                 (owner_kind, owner_id, project_id, project_root, path, branch, created_at)
             VALUES ('run', 7, 'alpha', 'C:/repos/alpha', 'C:/wt/alpha-7', 'wt/7', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        match remove(&pool, "alpha", false).await.unwrap() {
            Removed::Held(holding) => assert_eq!((holding.slots, holding.worktrees), (0, 1)),
            other => panic!("expected the removal to be refused, got {other:?}"),
        }

        // A worktree that has been cleaned up is history and not a hold, so the same project comes
        // off the roster once its checkout is gone.
        sqlx::query("UPDATE worktrees SET removed_at = '2026-01-02T00:00:00Z' WHERE owner_id = 7")
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            remove(&pool, "alpha", false).await.unwrap(),
            Removed::Done { .. }
        ));
    }

    /// **A stamp goes with the decision it was put on, and neither is left behind.**
    ///
    /// This is the case the schema-honesty test above found: `map_stamps` has no `project_id`, so a
    /// forget built from a column scan deleted the decision and left the stamp pointing at nothing.
    /// With `foreign_keys` on the real symptom was a refused DELETE rather than an orphan, which is
    /// the better failure and still a failure — and either way the owner's verdict on a decision
    /// they asked to forget would have outlived it.
    #[tokio::test]
    async fn forgetting_takes_the_stamps_that_hang_off_a_decision() {
        let pool = pool().await;
        register(&pool, "alpha").await;
        sqlx::query(
            "INSERT INTO map_decisions
                 (id, project_id, spec_slug, section, ordinal, text, kind, brain, extracted_at)
             VALUES (1, 'alpha', 'spec', 'A heading', 1, 'a decision', 'b', 'x',
                     '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO map_stamps (decision_id, verdict, stamped_at, code_digest)
             VALUES (1, 'settled', '2026-01-01T00:00:00Z', '')",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert!(matches!(
            remove(&pool, "alpha", true).await.unwrap(),
            Removed::Done { .. }
        ));
        let stamps: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM map_stamps")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stamps, 0);
    }

    /// A name nobody registered is a 404 and not a silent success.
    #[tokio::test]
    async fn removing_something_that_was_never_on_the_roster_says_so() {
        let pool = pool().await;
        assert!(matches!(
            remove(&pool, "ghost", true).await.unwrap(),
            Removed::NotRegistered
        ));
    }

    /// The record counts what the person is being asked to give up, in both of its two questions.
    #[tokio::test]
    async fn the_record_counts_what_is_there_and_what_is_holding_it() {
        let pool = pool().await;
        register(&pool, "alpha").await;
        for _ in 0..3 {
            sqlx::query("INSERT INTO runs (project_id, prompt, status, created_at) VALUES ('alpha', 'go', 'done', '2026-01-01T00:00:00Z')")
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query(
            "INSERT INTO project_slots (project_id, slot, owner_kind, owner_id, claimed_at)
             VALUES ('alpha', 0, 'run', 7, '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let record = record(&pool, "alpha")
            .await
            .unwrap()
            .expect("alpha is registered");
        assert_eq!(record.forgets.runs, 3);
        assert_eq!(record.forgets.stamps, 0);
        assert_eq!(record.holds.slots, 1);
        assert_eq!(record.holds.worktrees, 0);
    }

    /* --------------------------------------------------------------- the folder -- */

    /// **The blunt rule, and it is meant to be blunt.**
    ///
    /// The cost of refusing a legitimate `D:\work` is one sentence telling somebody to delete it
    /// themselves. The cost of the other mistake is a disk. A relative path is in here because
    /// removing one removes it relative to whatever the daemon's working directory happens to be,
    /// which on this app is wherever the tray icon was launched from.
    #[test]
    fn a_path_near_the_top_of_a_disk_is_never_deleted() {
        let nowhere = || Vec::<String>::new();
        assert!(too_big_beside(Path::new("C:/"), nowhere()));
        assert!(too_big_beside(Path::new("/"), nowhere()));
        assert!(too_big_beside(Path::new("C:/repos"), nowhere()));
        assert!(too_big_beside(Path::new("repos/alpha"), nowhere()));
        #[cfg(windows)]
        assert!(!too_big_beside(Path::new("C:/repos/alpha"), nowhere()));

        // **A POSIX path is not absolute on Windows**, and that is the platform's answer rather
        // than this rule's: `Path::is_absolute` wants a drive or UNC prefix here, so `/home/me/x`
        // falls out at the first check as if it were relative. Which is the safe direction, and is
        // why this assertion is written per platform instead of being deleted — a root spelt that
        // way is a root on Linux and nowhere to delete from on Windows.
        assert_eq!(
            too_big_beside(Path::new("/home/me/alpha"), nowhere()),
            cfg!(windows)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_posix_path_near_the_top_of_a_disk_is_never_deleted() {
        let nowhere = || Vec::<String>::new();
        assert!(too_big_beside(Path::new("/repos"), nowhere()));
        assert!(!too_big_beside(Path::new("/repos/alpha"), nowhere()));
    }

    /// The home directory, and everything it is inside.
    ///
    /// Both directions matter and only one is obvious. `~` itself is the obvious one; `C:/Users` is
    /// the one that gets missed, and it takes every account on the machine with it.
    ///
    /// The home is handed in rather than read: the machine running this has one somewhere nobody
    /// here can predict, and asserting about the real one would pass vacuously wherever the
    /// variable is unset. Setting it for the test is worse still — mutating the environment under a
    /// multi-threaded runner is unsound.
    #[cfg(windows)]
    #[test]
    fn the_home_directory_and_its_parents_are_never_deleted() {
        let home = || vec!["C:/Users/someone".to_owned()];
        assert!(too_big_beside(Path::new("C:/Users/someone"), home()));
        assert!(too_big_beside(Path::new("C:/Users"), home()));
        assert!(!too_big_beside(Path::new("C:/Users/someone/repos"), home()));
    }

    #[cfg(unix)]
    #[test]
    fn the_posix_home_directory_and_its_parents_are_never_deleted() {
        let home = || vec!["/home/someone".to_owned()];
        assert!(too_big_beside(Path::new("/home/someone"), home()));
        assert!(too_big_beside(Path::new("/home"), home()));
        assert!(!too_big_beside(Path::new("/home/someone/repos"), home()));
    }

    /// **A project registered inside another one, which nothing else would catch.**
    ///
    /// A monorepo and one of its own packages, both on the roster. Deleting the outer folder takes
    /// the inner project's code with it and leaves that project's roster row pointing at nothing —
    /// a row the app would go on polling, and a folder nobody asked to delete.
    async fn assert_a_folder_holding_another_project_is_refused(outer: &str, inner: &str) {
        let pool = pool().await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root)
             VALUES ('outer', 'shadow', ?)",
        )
        .bind(outer)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root)
             VALUES ('inner', 'shadow', ?)",
        )
        .bind(inner)
        .execute(&pool)
        .await
        .unwrap();

        let refused = folder_check(&pool, "outer", Some(outer)).await.unwrap();
        assert_eq!(refused, Err(Unsafe::HoldsAnother("inner".into())));

        // And the inner one is deletable on its own, which is the half that must not be broken by
        // the check above: the outer project is not inside it.
        assert!(
            folder_check(&pool, "inner", Some(inner))
                .await
                .unwrap()
                .is_ok()
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_folder_holding_another_registered_project_is_refused() {
        assert_a_folder_holding_another_project_is_refused(
            "C:/repos/mono",
            "C:/repos/mono/packages/inner",
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_posix_folder_holding_another_registered_project_is_refused() {
        assert_a_folder_holding_another_project_is_refused(
            "/repos/mono",
            "/repos/mono/packages/inner",
        )
        .await;
    }

    /// The real thing, on a real directory, including the file kind that defeats the standard call.
    ///
    /// Git marks everything under `.git/objects` read-only, so a repository is the ordinary case
    /// here rather than an edge one — and `remove_dir_all` refuses a read-only file on Windows with
    /// `Access is denied`. Without the attribute being cleared on the way down, deleting a project
    /// folder would fail on nearly every project this app has.
    #[tokio::test]
    async fn deleting_a_folder_takes_the_read_only_files_with_it() {
        let pool = pool().await;
        let holder = tempfile::tempdir().unwrap();
        let root = holder.path().join("alpha");
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        let object = root.join(".git/objects/ab12");
        std::fs::write(&object, b"an object").unwrap();
        let mut readonly = std::fs::metadata(&object).unwrap().permissions();
        readonly.set_readonly(true);
        std::fs::set_permissions(&object, readonly).unwrap();

        sqlx::query("INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('alpha', 'shadow', ?)")
            .bind(root.to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();

        let done = delete_folder(&pool, "alpha", false).await.unwrap();
        assert!(matches!(done, Deleted::Done { .. }), "got {done:?}");
        assert!(!root.exists());
        assert!(!on_roster(&pool, "alpha").await);
    }

    /// Held means nothing happened — including to the folder, which is the half worth asserting.
    ///
    /// Deleting a directory under a running agent is how a machine ends up with half a repository
    /// and a process still writing into it.
    #[tokio::test]
    async fn a_folder_is_not_deleted_under_work_that_is_still_running() {
        let pool = pool().await;
        let holder = tempfile::tempdir().unwrap();
        let root = holder.path().join("alpha");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("src.rs"), b"fn main() {}").unwrap();

        sqlx::query("INSERT INTO autopilot_state (project_id, mode, project_root) VALUES ('alpha', 'shadow', ?)")
            .bind(root.to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO project_slots (project_id, slot, owner_kind, owner_id, claimed_at)
             VALUES ('alpha', 0, 'run', 7, '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        match delete_folder(&pool, "alpha", false).await.unwrap() {
            Deleted::Held(holds) => assert_eq!(holds.slots, 1),
            other => panic!("expected the delete to be refused, got {other:?}"),
        }
        assert!(
            root.join("src.rs").exists(),
            "nothing on the disk may be touched"
        );
        assert!(on_roster(&pool, "alpha").await);
    }

    /// A project with no folder named has nothing to delete, and says so rather than succeeding.
    #[tokio::test]
    async fn a_project_with_no_folder_has_nothing_to_delete() {
        let pool = pool().await;
        register(&pool, "alpha").await;
        assert!(matches!(
            delete_folder(&pool, "alpha", false).await.unwrap(),
            Deleted::Refused(Unsafe::NoRoot)
        ));
        assert!(on_roster(&pool, "alpha").await, "a refusal removes nothing");
    }

    /// A project with nothing on record and a project that does not exist are different answers.
    ///
    /// Both count zero of everything, so nothing in the numbers separates them — which is why the
    /// roster row is asked for rather than inferred. A screen that read the first as the second
    /// would offer to remove something that had never been added, and report that it worked.
    #[tokio::test]
    async fn a_project_with_no_history_is_not_a_project_that_does_not_exist() {
        let pool = pool().await;
        register(&pool, "fresh").await;

        let fresh = record(&pool, "fresh").await.unwrap();
        assert!(fresh.is_some());
        assert_eq!(fresh.unwrap().forgets.runs, 0);

        assert!(record(&pool, "ghost").await.unwrap().is_none());
    }

    /// **Unlinking needs the parent directory's write bit, never the file's.**
    ///
    /// Clearing the read-only attribute is a Windows necessity that Unix answers far too
    /// generously: `set_readonly(false)` on a `0o444` file writes `0o666`, handing write to the
    /// group and to everybody else in exchange for nothing at all. The removal underneath it would
    /// have succeeded either way, because what `unlink` consults is the mode of the DIRECTORY the
    /// entry lives in and never the mode of the file itself. So the widening buys no deletion and
    /// leaks a permission on every file of every project folder this app has ever deleted — on
    /// paths whose names are already known, for as long as the walk takes to reach them.
    #[cfg(unix)]
    #[test]
    fn preparing_a_read_only_file_never_widens_it() {
        use std::os::unix::fs::PermissionsExt;

        let holder = tempfile::tempdir().unwrap();
        let object = holder.path().join("ab12");
        std::fs::write(&object, b"an object").unwrap();
        std::fs::set_permissions(&object, std::fs::Permissions::from_mode(0o444)).unwrap();

        let meta = std::fs::symlink_metadata(&object).unwrap();
        prepare_for_removal(&object, &meta);

        let mode = std::fs::metadata(&object).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o444, "got {mode:o}");
    }

    /// **A locked directory is the one thing that cannot be emptied without its owner bits.**
    ///
    /// Go's module cache ships its directories `0o555`, and a project folder that has ever built Go
    /// code holds thousands of them. Read and execute are enough to walk such a directory and not
    /// enough to unlink anything inside it, so this is the single case on Unix where the walk must
    /// widen something before it can descend. It widens it for the owner and stops there: `0o755`,
    /// not the `0o777` that `set_readonly(false)` would write, because the group and the world need
    /// nothing here and the process doing the deleting is the owner.
    #[cfg(unix)]
    #[test]
    fn preparing_a_locked_directory_grants_only_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let holder = tempfile::tempdir().unwrap();
        let locked = holder.path().join("pkg");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

        let meta = std::fs::symlink_metadata(&locked).unwrap();
        prepare_for_removal(&locked, &meta);

        let mode = std::fs::metadata(&locked).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "got {mode:o}");
    }

    /// Both halves at once, on a real tree: the narrow grant still deletes.
    ///
    /// A `0o444` file inside a `0o555` directory is what a Go module cache looks like on disk, and
    /// it is the shape that would appear to justify widening the file — it is read-only, and the
    /// tree must go. It does not justify it: the directory's new owner-write bit is the whole of
    /// what lets the entry be unlinked, and the file's own bits are never consulted. Asserting that
    /// the tree is gone is what separates a permission left alone from a deletion left undone.
    #[cfg(unix)]
    #[test]
    fn a_locked_tree_of_read_only_files_is_still_deleted() {
        use std::os::unix::fs::PermissionsExt;

        let holder = tempfile::tempdir().unwrap();
        let d = holder.path().join("d");
        std::fs::create_dir(&d).unwrap();
        let f = d.join("f");
        std::fs::write(&f, b"a module file").unwrap();
        // The file first and the directory second: writing into `d` is refused once `d` is locked.
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).unwrap();

        remove_tree(&d).unwrap();
        assert!(!d.exists(), "the locked tree is still there");
    }
}
