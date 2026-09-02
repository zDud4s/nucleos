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
    "feed",
    "fleet_exclusions",
    "jobs",
    "map_decisions",
    "project_commands",
    "project_slots",
    "proposals",
    "refinements",
    "repo_trigger_state",
    "run_presets",
    "runs",
    "scheduler_state",
    "vcs_requests",
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
    ("refinement_events", "refinement_id", "refinements"),
];

/// What a project has on record, in the nouns somebody would recognise.
///
/// Not every one of the twenty-seven tables the forget clears: `scheduler_state` and
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
/// twenty-seven immediate deletes all succeed. Deferred, the checks all happen at `COMMIT`, by which
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
    /// Self-references are excluded — a run naming its successor, a refinement naming what it
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
        }

        let removed = remove(&pool, "alpha", true).await.unwrap();
        assert!(matches!(removed, Removed::Done { forgotten } if forgotten == 2));

        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE project_id = 'alpha'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(runs, 0);

        // The other project is untouched, which is the property a `WHERE project_id = ?` written
        // twenty-seven times has to get right twenty-seven times.
        let theirs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE project_id = 'bravo'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(theirs, 1);
        assert!(on_roster(&pool, "bravo").await);
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
}
