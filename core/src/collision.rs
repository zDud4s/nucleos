//! Two live worktrees of one project touching the same files.
//!
//! It lives apart from `concurrency.rs` because it answers a different question. That one says *how
//! much work fits*, and the answer is an invariant held by a primary key; this one says *what that
//! work is touching*, and the answer is a best-effort warning with three states. Putting them
//! together would give a reader the impression that collision is as hard as a slot, and it is not.
//!
//! **Two sources, never merged.** The declared one (`job_items.files`) is intent and arrives in
//! time for you to stop; the observed one is fact and arrives after execution has started. A
//! warning that only comes once both worktrees have written to the same file comes late — hence
//! both — and an intention presented as fact would be a lie — hence separate.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The pair `worktrees` and `project_slots` use to name an owner.
///
/// `kind` is a `String` and not an `Owner`: this leaves over JSON to an interface that only wants
/// to know which tab to link to, and a serialised `enum` would force the other side to know the
/// internal shape.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct OwnerRef {
    pub kind: String,
    pub id: i64,
}

/// A coincidence between two trees, and the paths where it happens.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Overlap {
    pub a: OwnerRef,
    pub b: OwnerRef,
    pub paths: Vec<String>,
}

/// The three states, and the third exists so the second is never said in vain.
///
/// It follows `gate.rs`, which distinguishes *failed* from *never measured*. Saying `clean` without
/// having measured is the one way this screen can do active damage: somebody lets two jobs run,
/// trusting a warning nobody ever computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Collide,
    Clean,
    NotMeasured,
}

/// One source, with a state of its own. The two answer independently and are **not** collapsed into
/// one: a project whose slots are all runs has the declared source in `not_measured` and the
/// observed one with a real answer.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Source {
    pub state: State,
    pub overlaps: Vec<Overlap>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Collisions {
    pub declared: Source,
    pub observed: Source,
}

impl Collisions {
    /// What is said when the question could not be asked. Never `Clean`.
    pub fn unmeasured() -> Self {
        let unmeasured = || Source {
            state: State::NotMeasured,
            overlaps: Vec::new(),
        };
        Self {
            declared: unmeasured(),
            observed: unmeasured(),
        }
    }
}

/// PURE: every pair of trees sharing at least one path.
///
/// Stable order — the sets arrive ordered by owner and the paths by name — because this feeds a
/// screen that repaints every 3 seconds, and an order that dances makes warnings jump around with
/// nothing having changed.
///
/// Quadratic in the number of trees, and that is acceptable because the number has a ceiling: the
/// per-project one is 2 by default. Measure before raising it much.
pub fn overlaps(sets: &[(OwnerRef, BTreeSet<String>)]) -> Vec<Overlap> {
    let mut found = Vec::new();
    for (index, (a, left)) in sets.iter().enumerate() {
        for (b, right) in sets.iter().skip(index + 1) {
            let paths: Vec<String> = left.intersection(right).cloned().collect();
            if !paths.is_empty() {
                found.push(Overlap {
                    a: a.clone(),
                    b: b.clone(),
                    paths,
                });
            }
        }
    }
    found
}

/// PURE: the predicted warning, minus what the observed one already says.
///
/// When both sources name the same path it is **one** event, not two: a `Running` item declares
/// what it is going to write and has already written part of it, and the stronger source wins. What
/// is left to the predicted one are the paths not yet touched, which is the only thing it knows how
/// to say better.
///
/// The subtraction is **per pair**. A path shared between 1 and 2 says nothing about what 1 and 3
/// are going to do, and subtracting per path would erase warnings nobody confirmed.
pub fn only_predicted(declared: Vec<Overlap>, observed: &[Overlap]) -> Vec<Overlap> {
    declared
        .into_iter()
        .filter_map(|mut overlap| {
            if let Some(confirmed) = observed
                .iter()
                .find(|other| other.a == overlap.a && other.b == overlap.b)
            {
                overlap.paths.retain(|path| !confirmed.paths.contains(path));
            }
            (!overlap.paths.is_empty()).then_some(overlap)
        })
        .collect()
}

/// How many worktrees one tick measures.
///
/// The house ceiling bounds SLOTS, not `worktrees` rows — a tree pinned by an approval can outlive
/// its slot — so this number is not redundant. It says so when it truncates: a silent ceiling reads
/// as "I measured everything", which is precisely what this module cannot say without it being
/// true.
///
/// **It truncates by starvation, not by deferral.** The order is stable, so the tree that does not
/// fit is not measured on the next tick — it is not measured at all, and its project stays
/// `not_measured` for as long as that holds. It is the safe side, and it is not what a reader
/// assumes on seeing a per-pass ceiling.
///
/// Which is why the budget is **shared between projects** rather than spent in order. It used to be
/// spent in `ORDER BY owner_kind`, which sorts `'item'` before `'job'` before `'run'` — so once one
/// team's job filled the pass with item trees, a project running nothing but standalone runs was
/// not measured on this tick, nor on the next, nor ever while that held, and read `not_measured`
/// for reasons that had nothing to do with it. `fair_share` takes one tree from each project in
/// turn instead: a project goes short only when there are more projects than the cap, never because
/// of the shape of a neighbour's work.
const MEASURE_CAP: usize = 16;

/// How long a tick gives the whole pass.
///
/// The tick is 30 seconds and drives jobs after this. A third is enough slack for half a dozen
/// `git status` calls, and the deadline protects the tick from a hung git — which
/// `inspect::run_git` cannot kill while it writes nothing (a limitation inherited from
/// `inspect::diff`).
const MEASURE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(sqlx::FromRow)]
struct LiveTree {
    owner_kind: String,
    owner_id: i64,
    project_id: String,
    path: String,
    base_sha: Option<String>,
}

/// PURE: the trees one pass takes, at most `cap`, dealt one per project in turn.
///
/// The input arrives grouped by project (`ORDER BY project_id, …`) and each project's queue keeps
/// that order, so what a project loses to the cap is its tail and not an arbitrary subset — the
/// same stability the ordering was there to give.
///
/// A single project with more trees than the cap still loses the tail, and there is nothing to
/// share it with. What this removes is the case where the loser is decided by somebody else's
/// worktrees.
fn fair_share(trees: Vec<LiveTree>, cap: usize) -> Vec<LiveTree> {
    let mut by_project: BTreeMap<String, VecDeque<LiveTree>> = BTreeMap::new();
    for tree in trees {
        by_project
            .entry(tree.project_id.clone())
            .or_default()
            .push_back(tree);
    }

    let mut taken = Vec::new();
    // One round per lap: every project hands over its next tree, until the cap is full or nobody
    // has one left. `dealt` is what ends the loop when the cap is larger than the fleet.
    loop {
        let mut dealt = false;
        for queue in by_project.values_mut() {
            if taken.len() == cap {
                return taken;
            }
            if let Some(tree) = queue.pop_front() {
                taken.push(tree);
                dealt = true;
            }
        }
        if !dealt {
            return taken;
        }
    }
}

/// Measures every live worktree and stores the result. Called by the job tick.
///
/// Nothing here propagates an error: a measurement is a warning, and a warning that could not be
/// computed must not stop the tick that drives the work. What was not measured is left without a
/// row, and the read turns that into `not_measured`.
pub async fn measure(pool: &sqlx::SqlitePool) {
    let trees: Vec<LiveTree> = match sqlx::query_as(
        "SELECT owner_kind, owner_id, project_id, path, base_sha
         FROM worktrees WHERE removed_at IS NULL
         ORDER BY project_id, owner_kind, owner_id",
    )
    .fetch_all(pool)
    .await
    {
        Ok(trees) => trees,
        Err(error) => {
            tracing::warn!(%error, "could not list live worktrees to measure");
            return;
        }
    };

    // Counted AFTER the filter, so the warning does not fire about trees that were never
    // candidates: one without a `base_sha` goes unmeasured for want of a base, not for truncation.
    let candidates: Vec<LiveTree> = trees
        .into_iter()
        .filter(|tree| tree.base_sha.is_some())
        .collect();
    let candidate_count = candidates.len();
    let measurable = fair_share(candidates, MEASURE_CAP);
    if candidate_count > measurable.len() {
        tracing::warn!(
            candidates = candidate_count,
            measuring = measurable.len(),
            "more measurable worktrees than one pass takes; the rest stay not measured"
        );
    }

    // All spawned first, drained after: `spawn_blocking` starts at the moment of the call, so this
    // overlaps the `git` invocations without needing `futures` — which is not a dependency of this
    // crate.
    let mut pending = Vec::with_capacity(measurable.len());
    for tree in measurable {
        let path = std::path::PathBuf::from(&tree.path);
        let base = tree.base_sha.clone().expect("filtered to Some above");
        let handle =
            tokio::task::spawn_blocking(move || crate::inspect::changed_paths(&path, &base));
        pending.push((tree, handle));
    }

    let drain = async {
        for (tree, handle) in pending {
            let owner = (tree.owner_kind.clone(), tree.owner_id);
            match handle.await {
                Ok(Ok(paths)) => {
                    if let Err(error) = store(pool, &tree, &paths).await {
                        tracing::warn!(?owner, %error, "could not store a collision measurement");
                    }
                }
                // A git that refused, or a thread that panicked. In both cases the previous answer
                // stops being worth anything: keeping it would let a measurement no longer being
                // taken carry on saying `clean`.
                _ => {
                    if let Err(error) = forget(pool, &tree.owner_kind, tree.owner_id).await {
                        tracing::warn!(?owner, %error, "could not clear a stale measurement");
                    }
                }
            }
        }
    };

    if tokio::time::timeout(MEASURE_BUDGET, drain).await.is_err() {
        // The ones never drained keep their previous answer, which is the last true thing observed
        // about those trees and stands until the next pass replaces it. The tick waits no longer
        // than this.
        //
        // Dropping the future releases the remaining `JoinHandle`s, which DETACHES them — it does
        // not cancel them. A `git` hung without writing anything stays stuck inside
        // `inspect::run_git`'s `read_to_end` (the deadline there only applies after EOF), and the
        // stable order above relaunches it every 30 seconds. `MEASURE_CAP` bounds a pass, not the
        // passage of time, and the threads pile up in the blocking pool shared with `http.rs` and
        // `health.rs`. If this ever hurts, this comment is what points at the cause; the fix is to
        // give `run_git` a deadline that covers the read too.
        tracing::warn!("the collision measuring pass ran out of time");
    }
}

async fn store(pool: &sqlx::SqlitePool, tree: &LiveTree, paths: &[String]) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO worktree_touched_paths
             (owner_kind, owner_id, project_id, paths, measured_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (owner_kind, owner_id)
         DO UPDATE SET project_id = excluded.project_id,
                       paths = excluded.paths,
                       measured_at = excluded.measured_at",
    )
    .bind(&tree.owner_kind)
    .bind(tree.owner_id)
    .bind(&tree.project_id)
    .bind(serde_json::to_string(paths).unwrap_or_else(|_| "[]".to_string()))
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// Erases an owner's measurement. Public because `worktree::mark_removed` calls it: a tree that has
/// left the disk contributes nothing, and its row would describe something that no longer exists.
pub async fn forget(pool: &sqlx::SqlitePool, owner_kind: &str, owner_id: i64) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM worktree_touched_paths WHERE owner_kind = ? AND owner_id = ?")
        .bind(owner_kind)
        .bind(owner_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Both sources, for one project.
///
/// The order of work is: gather the observed sets, cross them, drop the pairs that are one item
/// beside its own job, gather the predicted ones, cross those, and **subtract the observed from the
/// predicted** — because a path both name is one event, not two.
///
/// The subtraction only lands because the two sources name owners the same way: an item of a job a
/// team directs is `item:<id>` on both sides. While the predicted source collapsed a job's items
/// into `job:<id>` and the observed one measured their checkouts separately, no pair ever matched
/// and every shared path was reported twice, once per source.
///
/// **The declared source's `Clean` is about JOB worktrees, not about the whole project.** A project
/// with one job (which declared files) and one live run reads `declared: clean`, even though the
/// run was never consulted — because a run has no items and so declares nothing, leaving no
/// declaration of its to collide with. §3.3 licenses this; it is worth saying, because the word
/// `clean` in that table speaks of "every live worktree" and here it is only the ones that can
/// declare.
pub async fn for_project(pool: &sqlx::SqlitePool, project_id: &str) -> sqlx::Result<Collisions> {
    let (measured, observed_complete) = observed_sets(pool, project_id).await?;
    let observed = between_strangers(overlaps(&measured), &item_families(pool, project_id).await?);

    let (predicted, anybody_declared) = declared_sets(pool, project_id).await?;
    let declared = only_predicted(overlaps(&predicted), &observed);

    Ok(Collisions {
        declared: Source {
            // `not_measured` on the declared source means one thing only: no live worktree of this
            // project declared any files. That includes every project whose slots are runs, which
            // have no items. It does not contaminate the observed source.
            //
            // `Clean` here subsumes a case the word does not say well: when ALL the predicted paths
            // have been confirmed by the observed source, `only_predicted` empties the list and
            // this source reads `clean` beside an observed one reading `collide`. The intersection
            // was not empty — it changed source. That is the design (the stronger one wins) and not
            // a mistake; a fourth state just for it would be one more word on the card.
            state: source_state(&declared, anybody_declared),
            overlaps: declared,
        },
        observed: Source {
            state: source_state(&observed, observed_complete),
            overlaps: observed,
        },
    })
}

/// PURE: drops the pairs that are one piece of work seen twice rather than two racing.
///
/// An item's tree and its job's tree are not two units of concurrent work; they are one unit at two
/// moments. `changed_paths` measures `base..HEAD`, so the moment an item's branch merges, the paths
/// it wrote appear on the job's tree as well as on its own, and the pair collides — every merged
/// item, always, for as long as both trees live. Left in, a team's job would paint the screen red
/// with its own progress and the warnings worth reading would be lost among them.
///
/// **What it costs, said out loud.** A sibling still writing that file is a real warning, and this
/// removes one of the two ways to see it: `(item:2, job:5)`, where item 1's work reached the job's
/// branch. The other way is `(item:1, item:2)` — the two checkouts, compared directly — which says
/// the same thing more precisely and is what this makes visible in the first place. The pair is
/// only lost when item 1's tree is already gone, and the GC keeps it for 72 hours.
///
/// Observed only. The predicted source names an item OR its job and never both, so it has no pair
/// of this shape to lose.
fn between_strangers(found: Vec<Overlap>, family: &BTreeMap<i64, i64>) -> Vec<Overlap> {
    let same_family = |item: &OwnerRef, job: &OwnerRef| {
        item.kind == "item" && job.kind == "job" && family.get(&item.id) == Some(&job.id)
    };
    found
        .into_iter()
        .filter(|overlap| {
            !same_family(&overlap.a, &overlap.b) && !same_family(&overlap.b, &overlap.a)
        })
        .collect()
}

/// Which job each live item tree belongs to.
///
/// Read through `worktrees` and not through `jobs`, so it is bounded by the trees that can appear
/// in an overlap rather than by every item the project has ever had.
async fn item_families(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> sqlx::Result<BTreeMap<i64, i64>> {
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT job_items.id, job_items.job_id
         FROM job_items
         JOIN worktrees ON worktrees.owner_kind = 'item' AND worktrees.owner_id = job_items.id
         WHERE worktrees.project_id = ? AND worktrees.removed_at IS NULL",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// A collision that was found is true even when the measurement is incomplete — what incompleteness
/// forbids is saying `clean`, not saying `collide`.
fn source_state(found: &[Overlap], complete: bool) -> State {
    if !found.is_empty() {
        State::Collide
    } else if complete {
        State::Clean
    } else {
        State::NotMeasured
    }
}

/// The observed sets, and whether they are complete.
///
/// Complete means: **every** live worktree of the project has a row, that row is later than the
/// birth of the tree it claims to describe, and its JSON parses. Each of those failures is a way of
/// not having measured, and none of them is `clean`. Age is not one of them: see
/// `a_measurement_taken_long_ago_still_counts`.
///
/// **One worktree poisons the whole project, and old ones have no cure.** A tree created before
/// migration `0062` has `base_sha = NULL`, is never measured, and therefore never has a row — which
/// puts its project into `not_measured` for as long as it lives. That is the correct behaviour
/// (where it branched from is unknown and unrecoverable after the fact), and this is where the
/// consequence shows up.
async fn observed_sets(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> sqlx::Result<(Vec<(OwnerRef, BTreeSet<String>)>, bool)> {
    let live: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT owner_kind, owner_id, created_at
         FROM worktrees WHERE project_id = ? AND removed_at IS NULL
         ORDER BY owner_kind, owner_id",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let measured: Vec<(String, i64, String, String)> = sqlx::query_as(
        "SELECT owner_kind, owner_id, paths, measured_at
         FROM worktree_touched_paths WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    let by_owner: BTreeMap<(String, i64), (String, String)> = measured
        .into_iter()
        .map(|(kind, id, paths, at)| ((kind, id), (paths, at)))
        .collect();

    let mut sets = Vec::new();
    let mut complete = true;
    for (kind, id, born) in live {
        let usable = by_owner
            .get(&(kind.clone(), id))
            .and_then(|(paths, at)| {
                // Identity, not freshness: a measurement predating the tree describes a different
                // tree that happened to reuse the id. Age on its own disqualifies nothing.
                let measured_at = chrono::DateTime::parse_from_rfc3339(at).ok()?;
                let born_at = chrono::DateTime::parse_from_rfc3339(&born).ok()?;
                (measured_at >= born_at).then_some(paths)
            })
            // Malformed reads as absent, never as an empty set.
            .and_then(|paths| serde_json::from_str::<Vec<String>>(paths).ok());

        match usable {
            Some(paths) => sets.push((
                OwnerRef { kind, id },
                paths.into_iter().collect::<BTreeSet<String>>(),
            )),
            None => complete = false,
        }
    }
    Ok((sets, complete))
}

/// The predicted sets, and whether anybody predicted anything.
///
/// Joined at read time and copied nowhere: `job_items.files` is already in SQLite, and holding it
/// here would give it a staleness window it does not have.
///
/// Found through `'job'` worktrees: a run has no items, so it declares nothing. `files` is nullable
/// and NULL is the ordinary case — a planner that names no files did not name an empty set of them.
///
/// **The job's tree is how the items are found; it is not who declares.** In a job a team directs,
/// each item is its own unit of work in a checkout of its own, and collapsing the siblings into one
/// owner — which is what this did, and all it could do while there was one checkout — makes the one
/// collision that matters most invisible: two items of the same job about to write the same file.
/// So the owner is the ITEM there, and stays the JOB where there is no team and the work really
/// does all land in one tree.
///
/// **It is found through the job's tree and not through the item's, and that is the point.** An
/// item that has not started has no checkout, and that is exactly when its declaration is worth
/// something — *"intent … arrives in time for you to stop"*. Requiring a tree would leave the
/// source with nothing to say until it had nothing left to warn about.
///
/// **A negative list, and not `IN ('pending','running')`.** `item_state_from` (`job.rs:438`) has no
/// arm for `"pending"` — `Pending` is the *fallback*, with the reason written down: *"An
/// unrecognised item status is treated as still to do rather than as done… erring the other way
/// silently skips work the job was created to perform"*. A positive list would invert that default:
/// a new status the core reads as `Pending` would fall outside the predicted set, shrinking the
/// intersection and turning a real `Collide` into a `Clean` — the outcome this module exists never
/// to produce. Written as the negation of the explicit arms, an unknown status counts, exactly as
/// it does there.
async fn declared_sets(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> sqlx::Result<(Vec<(OwnerRef, BTreeSet<String>)>, bool)> {
    let rows: Vec<(i64, i64, Option<String>, Option<String>)> = sqlx::query_as(DECLARED_SETS_SQL)
        .bind(project_id)
        .fetch_all(pool)
        .await?;

    let mut by_owner: BTreeMap<OwnerRef, BTreeSet<String>> = BTreeMap::new();
    let mut anybody = false;
    for (job_id, item_id, team_id, files) in rows {
        // A team is what makes the siblings separable, because it is what gives each of them a
        // checkout. Without one they share the job's, and two entries would claim two units of
        // work where the disk holds one.
        let owner = match team_id {
            Some(_) => OwnerRef {
                kind: "item".to_string(),
                id: item_id,
            },
            None => OwnerRef {
                kind: "job".to_string(),
                id: job_id,
            },
        };
        let entry = by_owner.entry(owner).or_default();
        let Some(files) = files else { continue };
        let Ok(paths) = serde_json::from_str::<Vec<String>>(&files) else {
            continue;
        };
        if !paths.is_empty() {
            anybody = true;
        }
        entry.extend(paths);
    }

    Ok((by_owner.into_iter().collect(), anybody))
}

/// The states that **leave** the predicted set; whatever is left goes in.
///
/// Mirrors the explicit arms of `item_state_from` minus `"running"`. Kept apart from the function
/// because it is what the drift guard compares, and written by hand because sqlx refuses SQL built
/// at runtime — the same trade `LIVE_JOBS_SQL` and `ORPHANED_SLOTS_SQL` make, with the same guard.
///
/// **The four states of a parallel item all leave, for two different reasons.** `orphaned` is
/// terminal and would otherwise declare for ever, on work nobody will ever do. `merging`,
/// `conflicted` and `reverted` leave for the reason `implemented` does, which is not that the item
/// will never write again — `conflicted` and `reverted` both go round again — but that it already
/// HAS: each of them owns a live checkout with its work in it, so the observed source is measuring
/// that work directly and is the better of the two answers. Leaving them in would shrink to nothing
/// under `only_predicted` anyway, after saying the same thing twice on the way.
///
/// Removing a state from the predicted set is the unsafe direction — a path that leaves the
/// intersection turns a `Collide` into a `Clean` — and what makes it safe here is precisely that
/// these three have trees. `orphaned` never gets one and never writes, so there is nothing to lose.
const DECLARED_SETS_SQL: &str = "SELECT worktrees.owner_id, job_items.id, jobs.team_id,
            job_items.files
     FROM worktrees
     JOIN job_items ON job_items.job_id = worktrees.owner_id
     JOIN jobs ON jobs.id = job_items.job_id
     WHERE worktrees.project_id = ?
       AND worktrees.removed_at IS NULL
       AND worktrees.owner_kind = 'job'
       AND job_items.status NOT IN ('implemented','passed','failed','cancelled',
                                    'gate_failed','gate_errored','skipped',
                                    'merging','conflicted','reverted','orphaned')
     ORDER BY worktrees.owner_id, job_items.id";

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(kind: &str, id: i64) -> OwnerRef {
        OwnerRef {
            kind: kind.to_string(),
            id,
        }
    }

    fn set(paths: &[&str]) -> std::collections::BTreeSet<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    async fn seed_worktree_row(
        pool: &sqlx::SqlitePool,
        kind: &str,
        id: i64,
        project_id: &str,
        path: &std::path::Path,
        base_sha: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO worktrees
             (owner_kind, owner_id, project_id, project_root, path, branch, base_sha, created_at)
             VALUES (?, ?, ?, ?, ?, 'nucleos/x', ?, '2026-01-01T00:00:00Z')",
        )
        .bind(kind)
        .bind(id)
        .bind(project_id)
        .bind(path.to_string_lossy().into_owned())
        .bind(path.to_string_lossy().into_owned())
        .bind(base_sha)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_measurement(
        pool: &sqlx::SqlitePool,
        kind: &str,
        id: i64,
        project_id: &str,
        paths: &[&str],
    ) {
        seed_measurement_at(
            pool,
            kind,
            id,
            project_id,
            paths,
            &chrono::Utc::now().to_rfc3339(),
        )
        .await;
    }

    /// The same, with the time of one's choosing — which is what the staleness tests need.
    async fn seed_measurement_at(
        pool: &sqlx::SqlitePool,
        kind: &str,
        id: i64,
        project_id: &str,
        paths: &[&str],
        measured_at: &str,
    ) {
        sqlx::query(
            "INSERT INTO worktree_touched_paths (owner_kind, owner_id, project_id, paths, measured_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(kind)
        .bind(id)
        .bind(project_id)
        .bind(serde_json::to_string(paths).unwrap())
        .bind(measured_at)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A job with an **explicit** id, unlike `concurrency.rs`'s `seed_job` which returns one: the
    /// tests here pin ids 1 and 2 so they line up with the `worktrees` rows.
    async fn seed_job_row(pool: &sqlx::SqlitePool, id: i64, project_id: &str) {
        seed_directed_job_row(pool, id, project_id, None).await;
    }

    /// The same, with a team directing it — which is what decides whether its items declare
    /// separately or as one.
    async fn seed_directed_job_row(
        pool: &sqlx::SqlitePool,
        id: i64,
        project_id: &str,
        team_id: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO jobs (id, project_id, project_root, status, max_items, created_at, team_id)
             VALUES (?, ?, 'C:/somewhere', 'implementing', 5, '2026-08-08T00:00:00Z', ?)",
        )
        .bind(id)
        .bind(project_id)
        .bind(team_id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A team a job can name. Not ceremony: `storage.rs` runs with `foreign_keys` on, so a job
    /// cannot name a team out of thin air, nor a team a director.
    async fn seed_team(pool: &sqlx::SqlitePool, id: &str) {
        let now = "2026-08-08T00:00:00Z";
        let director = format!("{id}-director");
        sqlx::query(
            "INSERT INTO agents (id, name, speciality, prompt, engine, tool_policy,
                                 created_at, updated_at)
             VALUES (?, 'Dir', 'directing', 'lead', 'claude', 'inherit', ?, ?)",
        )
        .bind(&director)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO teams (id, name, mission, director_agent_id, max_rounds, max_parallel,
                                created_at, updated_at)
             VALUES (?, 'Crew', 'ship it', ?, 3, 2, ?, ?)",
        )
        .bind(id)
        .bind(&director)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    }

    /// `files` is a JSON array, or `NULL` — which is the ordinary case, not the exception. Returns
    /// the item's id, because that is what an item of a team's job is named by from here on.
    async fn seed_item(
        pool: &sqlx::SqlitePool,
        job_id: i64,
        ordinal: i64,
        status: &str,
        files: Option<&[&str]>,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, files)
             VALUES (?, ?, 'an item', ?, ?)",
        )
        .bind(job_id)
        .bind(ordinal)
        .bind(status)
        .bind(files.map(|paths| serde_json::to_string(paths).unwrap()))
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// A worktree marked `removed_at` stops contributing — and the row goes with it, rather than
    /// staying behind to describe a tree that is no longer on disk.
    #[tokio::test]
    async fn marking_a_worktree_removed_takes_its_measurement_with_it() {
        let db = crate::storage::TempDb::new().await;
        seed_measurement(&db.pool, "job", 1, "project-a", &["a.rs"]).await;

        crate::worktree::mark_removed(&db.pool, crate::worktree::Owner::Job(1))
            .await
            .unwrap();

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worktree_touched_paths")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
        db.close().await;
    }

    fn at(path: &str) -> &std::path::Path {
        std::path::Path::new(path)
    }

    /// Two measured trees sharing one file: it collides, and it names the path.
    #[tokio::test]
    async fn two_measured_trees_on_one_file_collide() {
        let db = crate::storage::TempDb::new().await;
        seed_worktree_row(
            &db.pool,
            "job",
            1,
            "project-a",
            at("C:/x"),
            Some(&"a".repeat(40)),
        )
        .await;
        seed_worktree_row(
            &db.pool,
            "run",
            2,
            "project-a",
            at("C:/y"),
            Some(&"b".repeat(40)),
        )
        .await;
        seed_measurement(
            &db.pool,
            "job",
            1,
            "project-a",
            &["core/src/runs.rs", "core/src/job.rs"],
        )
        .await;
        seed_measurement(&db.pool, "run", 2, "project-a", &["core/src/runs.rs"]).await;

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.observed.state, State::Collide);
        assert_eq!(
            collisions.observed.overlaps[0].paths,
            vec!["core/src/runs.rs".to_string()]
        );
        db.close().await;
    }

    /// A live tree with no row: `not_measured`, and **never** `clean`. It is the outcome this
    /// module exists not to produce by accident.
    #[tokio::test]
    async fn a_live_tree_with_no_measurement_makes_the_project_not_measured() {
        let db = crate::storage::TempDb::new().await;
        seed_worktree_row(
            &db.pool,
            "job",
            1,
            "project-a",
            at("C:/x"),
            Some(&"a".repeat(40)),
        )
        .await;
        seed_worktree_row(
            &db.pool,
            "run",
            2,
            "project-a",
            at("C:/y"),
            Some(&"b".repeat(40)),
        )
        .await;
        seed_measurement(&db.pool, "job", 1, "project-a", &["a.rs"]).await;

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.observed.state, State::NotMeasured);
        db.close().await;
    }

    /// A measurement older than the birth of the tree it claims to describe does not describe it.
    #[tokio::test]
    async fn a_measurement_older_than_the_tree_it_describes_does_not_count() {
        let db = crate::storage::TempDb::new().await;
        seed_worktree_row(
            &db.pool,
            "job",
            1,
            "project-a",
            at("C:/x"),
            Some(&"a".repeat(40)),
        )
        .await;
        sqlx::query("UPDATE worktrees SET created_at = '2026-06-01T00:00:00Z'")
            .execute(&db.pool)
            .await
            .unwrap();
        seed_measurement_at(
            &db.pool,
            "job",
            1,
            "project-a",
            &["a.rs"],
            "2026-05-01T00:00:00Z",
        )
        .await;

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.observed.state, State::NotMeasured);
        db.close().await;
    }

    /// An old measurement still counts: age alone does not disqualify one.
    ///
    /// There used to be a five-minute shelf life here, and it was this plan's addition rather than
    /// anything the spec asked for. It was vetoed, because what it actually bought was worse than
    /// what it cost. A measurement is only ever taken of a LIVE worktree, and a live worktree that
    /// has stopped being measured is one the daemon stopped measuring — the daemon being down, or
    /// the pass overrunning its budget. In both of those the last measurement is still the last
    /// true thing anybody observed about that tree, and discarding it replaces a true answer with
    /// no answer. Meanwhile the cost was concrete: stop the daemon, and five minutes later the
    /// whole fleet reads `not measured` even though nothing is running and nothing can collide.
    ///
    /// What still disqualifies a measurement is the check below it: one taken BEFORE the tree was
    /// born describes a different tree that happened to reuse the id. That rule is about identity,
    /// not freshness, and it stays.
    #[tokio::test]
    async fn a_measurement_taken_long_ago_still_counts() {
        let db = crate::storage::TempDb::new().await;
        seed_worktree_row(
            &db.pool,
            "job",
            1,
            "project-a",
            at("C:/x"),
            Some(&"a".repeat(40)),
        )
        .await;
        let long_ago = (chrono::Utc::now() - chrono::Duration::hours(6)).to_rfc3339();
        seed_measurement_at(&db.pool, "job", 1, "project-a", &["a.rs"], &long_ago).await;

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.observed.state, State::Clean);
        db.close().await;
    }

    /// A malformed `paths` reads as ABSENT, never as an empty set. Empty would claim "I measured
    /// and it touched nothing", which is something a read error did not say.
    #[tokio::test]
    async fn a_malformed_path_list_reads_as_absent_and_not_as_empty() {
        let db = crate::storage::TempDb::new().await;
        seed_worktree_row(
            &db.pool,
            "job",
            1,
            "project-a",
            at("C:/x"),
            Some(&"a".repeat(40)),
        )
        .await;
        sqlx::query(
            "INSERT INTO worktree_touched_paths (owner_kind, owner_id, project_id, paths, measured_at)
             VALUES ('job', 1, 'project-a', 'not json at all', ?)",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&db.pool)
        .await
        .unwrap();

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.observed.state, State::NotMeasured);
        db.close().await;
    }

    /// A tree with `removed_at` contributes nothing on the READ side — the twin of the Task 8 test,
    /// which only proves the write side. Without this half, a row that survived `forget` would let
    /// a tree no longer on disk carry on colliding with one that is.
    #[tokio::test]
    async fn a_removed_worktree_contributes_nothing_even_if_its_measurement_survives() {
        let db = crate::storage::TempDb::new().await;
        let base = "a".repeat(40);
        seed_worktree_row(&db.pool, "job", 1, "project-a", at("C:/x"), Some(&base)).await;
        seed_worktree_row(&db.pool, "job", 2, "project-a", at("C:/y"), Some(&base)).await;
        seed_measurement(&db.pool, "job", 1, "project-a", &["shared.rs"]).await;
        seed_measurement(&db.pool, "job", 2, "project-a", &["shared.rs"]).await;
        // The row is left behind on purpose: `forget` is best-effort and can fail.
        sqlx::query("UPDATE worktrees SET removed_at = '2026-08-09T00:00:00Z' WHERE owner_id = 2")
            .execute(&db.pool)
            .await
            .unwrap();

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.observed.state, State::Clean);
        assert!(collisions.observed.overlaps.is_empty());
        db.close().await;
    }

    /// The two sources do NOT collapse: a project of runs alone has the declared source in
    /// `not_measured` — runs have no items — and the observed one with a real answer.
    #[tokio::test]
    async fn a_project_of_runs_has_no_declared_source_and_a_real_observed_one() {
        let db = crate::storage::TempDb::new().await;
        seed_worktree_row(
            &db.pool,
            "run",
            1,
            "project-a",
            at("C:/x"),
            Some(&"a".repeat(40)),
        )
        .await;
        seed_measurement(&db.pool, "run", 1, "project-a", &["a.rs"]).await;

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.declared.state, State::NotMeasured);
        assert_eq!(collisions.observed.state, State::Clean);
        db.close().await;
    }

    /// The declared set is the union of the items that have not yet written anything that survives:
    /// `Pending` and `Running`. `Implemented` is OUT — its writes are already on disk, so they are
    /// already the observed source's business. `Skipped` and `GateFailed` are out on their own
    /// merit: in both the tree is reverted to where the item started, so nothing they were going to
    /// write survived.
    #[tokio::test]
    async fn the_predicted_set_is_the_items_that_have_not_written_anything_that_survives() {
        let db = crate::storage::TempDb::new().await;
        seed_worktree_row(
            &db.pool,
            "job",
            1,
            "project-a",
            at("C:/x"),
            Some(&"a".repeat(40)),
        )
        .await;
        seed_worktree_row(
            &db.pool,
            "job",
            2,
            "project-a",
            at("C:/y"),
            Some(&"b".repeat(40)),
        )
        .await;
        seed_job_row(&db.pool, 1, "project-a").await;
        seed_job_row(&db.pool, 2, "project-a").await;
        seed_item(&db.pool, 1, 0, "pending", Some(&["shared.rs"])).await;
        seed_item(&db.pool, 1, 1, "implemented", Some(&["already_written.rs"])).await;
        seed_item(&db.pool, 1, 2, "skipped", Some(&["never_written.rs"])).await;
        seed_item(
            &db.pool,
            2,
            0,
            "running",
            Some(&["shared.rs", "already_written.rs", "never_written.rs"]),
        )
        .await;

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.declared.state, State::Collide);
        assert_eq!(
            collisions.declared.overlaps[0].paths,
            vec!["shared.rs".to_string()],
            "only what neither side has already written"
        );
        db.close().await;
    }

    /// A path predicted by a `Running` item and already written yields ONE warning, marked observed.
    #[tokio::test]
    async fn a_path_predicted_and_already_written_is_one_warning_on_the_observed_source() {
        let db = crate::storage::TempDb::new().await;
        for (id, path) in [(1, "C:/x"), (2, "C:/y")] {
            seed_worktree_row(
                &db.pool,
                "job",
                id,
                "project-a",
                at(path),
                Some(&"a".repeat(40)),
            )
            .await;
            seed_job_row(&db.pool, id, "project-a").await;
            seed_item(&db.pool, id, 0, "running", Some(&["shared.rs"])).await;
            seed_measurement(&db.pool, "job", id, "project-a", &["shared.rs"]).await;
        }

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.observed.state, State::Collide);
        assert!(
            collisions.declared.overlaps.is_empty(),
            "the stronger source wins; the warning does not appear twice"
        );
        db.close().await;
    }

    /// Two items of one team's job, both about to write the same file.
    ///
    /// **The collision this screen exists for, and the one it could not see.** While a job's items
    /// were collapsed into `job:<id>`, their declarations were unioned before anything was crossed,
    /// and `overlaps` only compares distinct owners — so the sibling pair had nowhere to appear. It
    /// is also the pair with the shortest fuse: unlike two jobs, these two are going to merge into
    /// the same branch within minutes of each other.
    #[tokio::test]
    async fn two_items_of_one_team_declaring_one_file_collide_as_siblings() {
        let db = crate::storage::TempDb::new().await;
        seed_worktree_row(
            &db.pool,
            "job",
            1,
            "project-a",
            at("C:/x"),
            Some(&"a".repeat(40)),
        )
        .await;
        seed_team(&db.pool, "crew").await;
        seed_directed_job_row(&db.pool, 1, "project-a", Some("crew")).await;
        let first = seed_item(&db.pool, 1, 0, "pending", Some(&["shared.rs", "only-a.rs"])).await;
        let second = seed_item(&db.pool, 1, 1, "pending", Some(&["shared.rs"])).await;

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.declared.state, State::Collide);
        assert_eq!(collisions.declared.overlaps.len(), 1);
        assert_eq!(collisions.declared.overlaps[0].a, owner("item", first));
        assert_eq!(collisions.declared.overlaps[0].b, owner("item", second));
        assert_eq!(
            collisions.declared.overlaps[0].paths,
            vec!["shared.rs".to_string()],
            "the intersection, not the union of what the two declared"
        );
        db.close().await;
    }

    /// Neither item has a checkout, and that is the whole value of the declared source: a `pending`
    /// item is the one moment a warning still arrives in time to stop something.
    #[tokio::test]
    async fn siblings_declare_before_either_of_them_has_a_tree() {
        let db = crate::storage::TempDb::new().await;
        seed_worktree_row(
            &db.pool,
            "job",
            1,
            "project-a",
            at("C:/x"),
            Some(&"a".repeat(40)),
        )
        .await;
        seed_team(&db.pool, "crew").await;
        seed_directed_job_row(&db.pool, 1, "project-a", Some("crew")).await;
        seed_item(&db.pool, 1, 0, "pending", Some(&["shared.rs"])).await;
        seed_item(&db.pool, 1, 1, "pending", Some(&["shared.rs"])).await;

        let item_trees: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM worktrees WHERE owner_kind = 'item'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(
            item_trees, 0,
            "the point of the test is that there are none"
        );
        assert_eq!(
            for_project(&db.pool, "project-a")
                .await
                .unwrap()
                .declared
                .state,
            State::Collide
        );
        db.close().await;
    }

    /// A job with no team declares as ONE owner, and this is the regression that matters most: two
    /// of its items naming the same file is a queue writing it twice in a row, in one checkout, and
    /// calling that a collision would put a permanent warning on every ordinary job.
    #[tokio::test]
    async fn two_items_of_a_job_with_no_team_do_not_collide_with_each_other() {
        let db = crate::storage::TempDb::new().await;
        seed_worktree_row(
            &db.pool,
            "job",
            1,
            "project-a",
            at("C:/x"),
            Some(&"a".repeat(40)),
        )
        .await;
        seed_job_row(&db.pool, 1, "project-a").await;
        seed_item(&db.pool, 1, 0, "pending", Some(&["shared.rs"])).await;
        seed_item(&db.pool, 1, 1, "pending", Some(&["shared.rs"])).await;

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.declared.state, State::Clean);
        assert!(collisions.declared.overlaps.is_empty());
        db.close().await;
    }

    /// A path two siblings declared and both already wrote is ONE warning, on the observed source —
    /// and the one they have not written yet is still the predicted source's to say.
    ///
    /// **It is the naming that makes the subtraction land**, and the two declared paths are how
    /// this test can tell. While the predicted source said `job:1` and the observed one said
    /// `item:…`, `only_predicted` had no pair to match — it subtracts per pair, deliberately —
    /// so `written.rs` came out on both sources, for the same two checkouts, as two warnings about
    /// one event. Asserting only that the predicted source is empty would not catch that: a
    /// predicted source with nothing in it at all passes that assertion for the opposite reason.
    #[tokio::test]
    async fn one_path_two_siblings_wrote_is_subtracted_and_the_other_is_not() {
        let db = crate::storage::TempDb::new().await;
        let base = "a".repeat(40);
        seed_worktree_row(&db.pool, "job", 1, "project-a", at("C:/x"), Some(&base)).await;
        seed_team(&db.pool, "crew").await;
        seed_directed_job_row(&db.pool, 1, "project-a", Some("crew")).await;
        let declared = Some(&["written.rs", "still_to_write.rs"][..]);
        let first = seed_item(&db.pool, 1, 0, "running", declared).await;
        let second = seed_item(&db.pool, 1, 1, "running", declared).await;
        for id in [first, second] {
            seed_worktree_row(&db.pool, "item", id, "project-a", at("C:/i"), Some(&base)).await;
            seed_measurement(&db.pool, "item", id, "project-a", &["written.rs"]).await;
        }
        // The job's own tree has had nothing merged into it yet.
        seed_measurement(&db.pool, "job", 1, "project-a", &[]).await;

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.observed.state, State::Collide);
        assert_eq!(
            collisions.observed.overlaps[0].paths,
            vec!["written.rs".to_string()]
        );
        assert_eq!(
            collisions.declared.overlaps[0].paths,
            vec!["still_to_write.rs".to_string()],
            "the written path is the observed source's; the unwritten one is not"
        );
        db.close().await;
    }

    /// An item's tree beside its own job's tree is one piece of work at two moments, not two
    /// pieces racing.
    ///
    /// `changed_paths` measures `base..HEAD`, so the instant an item's branch merges, every path it
    /// wrote is on the job's tree as well as on its own. Left alone, a team's job would report a
    /// collision against itself for every item it successfully landed — a screen painted red by its
    /// own progress, which is the one way a warning stops being read.
    #[tokio::test]
    async fn an_item_beside_its_own_job_is_one_piece_of_work_and_not_a_collision() {
        let db = crate::storage::TempDb::new().await;
        let base = "a".repeat(40);
        seed_worktree_row(&db.pool, "job", 1, "project-a", at("C:/x"), Some(&base)).await;
        seed_team(&db.pool, "crew").await;
        seed_directed_job_row(&db.pool, 1, "project-a", Some("crew")).await;
        let item = seed_item(&db.pool, 1, 0, "merging", None).await;
        seed_worktree_row(&db.pool, "item", item, "project-a", at("C:/i"), Some(&base)).await;
        seed_measurement(&db.pool, "item", item, "project-a", &["merged.rs"]).await;
        seed_measurement(&db.pool, "job", 1, "project-a", &["merged.rs"]).await;

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.observed.state, State::Clean);
        assert!(collisions.observed.overlaps.is_empty());
        db.close().await;
    }

    /// And the negation, so the rule above is about ONE job and not about the word `item`: another
    /// job's tree is concurrent work, and an item colliding with it is exactly what this reports.
    #[tokio::test]
    async fn an_item_still_collides_with_the_tree_of_a_job_it_does_not_belong_to() {
        let db = crate::storage::TempDb::new().await;
        let base = "a".repeat(40);
        seed_worktree_row(&db.pool, "job", 1, "project-a", at("C:/x"), Some(&base)).await;
        seed_worktree_row(&db.pool, "job", 2, "project-a", at("C:/y"), Some(&base)).await;
        seed_team(&db.pool, "crew").await;
        seed_directed_job_row(&db.pool, 1, "project-a", Some("crew")).await;
        seed_job_row(&db.pool, 2, "project-a").await;
        let item = seed_item(&db.pool, 1, 0, "merging", None).await;
        seed_worktree_row(&db.pool, "item", item, "project-a", at("C:/i"), Some(&base)).await;
        seed_measurement(&db.pool, "item", item, "project-a", &["shared.rs"]).await;
        seed_measurement(&db.pool, "job", 1, "project-a", &["shared.rs"]).await;
        seed_measurement(&db.pool, "job", 2, "project-a", &["shared.rs"]).await;

        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(collisions.observed.state, State::Collide);
        let pairs: Vec<(OwnerRef, OwnerRef)> = collisions
            .observed
            .overlaps
            .iter()
            .map(|overlap| (overlap.a.clone(), overlap.b.clone()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (owner("item", item), owner("job", 2)),
                (owner("job", 1), owner("job", 2)),
            ],
            "the item's own job is dropped and nothing else is"
        );
        db.close().await;
    }

    fn live_tree(project_id: &str, owner_id: i64) -> LiveTree {
        LiveTree {
            owner_kind: "run".to_string(),
            owner_id,
            project_id: project_id.to_string(),
            path: "C:/x".to_string(),
            base_sha: Some("a".repeat(40)),
        }
    }

    fn dealt(taken: Vec<LiveTree>) -> Vec<(String, i64)> {
        taken
            .into_iter()
            .map(|tree| (tree.project_id, tree.owner_id))
            .collect()
    }

    /// The budget is dealt one tree per project in turn, and a project is never starved by the
    /// shape of a neighbour's work.
    ///
    /// The order used to be `owner_kind, owner_id` across the whole fleet, and `'item'` sorts before
    /// `'job'` before `'run'`. One team's job with enough item trees took the entire pass, and a
    /// project running nothing but standalone runs read `not_measured` — not this tick, but every
    /// tick, for as long as that held. The truncation is by starvation and not by deferral, which
    /// is what makes the difference permanent rather than a delay.
    #[test]
    fn the_measuring_budget_is_dealt_one_tree_per_project_in_turn() {
        let trees = vec![
            live_tree("busy", 1),
            live_tree("busy", 2),
            live_tree("busy", 3),
            live_tree("busy", 4),
            live_tree("quiet", 9),
        ];

        assert_eq!(
            dealt(fair_share(trees, 3)),
            vec![
                ("busy".to_string(), 1),
                ("quiet".to_string(), 9),
                ("busy".to_string(), 2),
            ],
            "the quiet project must be measured before the busy one's second lap"
        );
    }

    /// A fleet smaller than the budget is measured whole — and the dealing stops rather than
    /// looping over empty queues for ever.
    #[test]
    fn a_fleet_smaller_than_the_budget_is_measured_whole() {
        let trees = vec![
            live_tree("a", 1),
            live_tree("a", 2),
            live_tree("b", 3),
            live_tree("c", 4),
        ];

        let taken = dealt(fair_share(trees, MEASURE_CAP));

        assert_eq!(taken.len(), 4);
        assert!(taken.contains(&("a".to_string(), 2)));
    }

    /// A single project with more trees than the budget still loses its tail, because there is
    /// nobody to share with. What went away is the loser being chosen by somebody else's work.
    #[test]
    fn one_project_over_the_budget_loses_its_own_tail_in_order() {
        let trees = vec![live_tree("a", 1), live_tree("a", 2), live_tree("a", 3)];

        assert_eq!(
            dealt(fair_share(trees, 2)),
            vec![("a".to_string(), 1), ("a".to_string(), 2)]
        );
    }

    /// A state an item can take that this SQL does not know is a predicted path lost in silence —
    /// and a lost path reads as `clean`. The guard walks the explicit arms of `item_state_from` and
    /// demands each be named, minus `running`, the one that stays in.
    ///
    /// Written as a literal list and not by reflection because Rust has no reflection over `match`.
    /// What it catches is the asymmetric edit: somebody adds a state to `job.rs` and not here.
    #[test]
    fn the_predicted_set_names_every_state_that_has_finished_writing() {
        // The explicit arms of `item_state_from` (`job.rs:890`), minus `running`, which stays in
        // the predicted set because it is still writing.
        let finished = [
            "implemented",
            "passed",
            "failed",
            crate::job::STATUS_CANCELLED,
            "gate_failed",
            "gate_errored",
            crate::job::STATUS_SKIPPED,
            // The four of a parallel item. Three of them own a checkout the observed source is
            // measuring, and the fourth will never write anything at all.
            "merging",
            "conflicted",
            "reverted",
            "orphaned",
        ];
        for status in finished {
            assert!(
                DECLARED_SETS_SQL.contains(&format!("'{status}'")),
                "the predicted set does not exclude `{status}`"
            );
        }
        let named = DECLARED_SETS_SQL
            .split('\'')
            .skip(1)
            .step_by(2)
            .filter(|token| !token.is_empty())
            .count();
        assert_eq!(
            named,
            finished.len() + 1,
            "the SQL names something extra (the +1 is the join's 'job'), or has lost a state"
        );
    }

    /// The measurement writes down what `changed_paths` returned, and this is where the tick and
    /// the table meet.
    #[tokio::test]
    async fn a_measuring_pass_records_what_each_live_worktree_touched() {
        let db = crate::storage::TempDb::new().await;
        let (repo, base) = crate::inspect::tests::seeded_repo();
        std::fs::write(repo.path().join("touched.rs"), "x\n").unwrap();
        seed_worktree_row(&db.pool, "job", 1, "project-a", repo.path(), Some(&base)).await;

        measure(&db.pool).await;

        let stored: String = sqlx::query_scalar(
            "SELECT paths FROM worktree_touched_paths WHERE owner_kind = 'job' AND owner_id = 1",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(stored.contains("touched.rs"), "{stored}");
        db.close().await;
    }

    /// The whole loop, on real repositories: two live trees, each genuinely edited, measured by the
    /// real pass, and read back as a collision that names the one file they share.
    ///
    /// The two tests around it each cover half and neither covers the join. `measure` is exercised
    /// against one repository, so it never sees two; the arithmetic is exercised against SEEDED
    /// measurements, so it never sees `changed_paths`. Everything between them — two trees measured
    /// in one pass, their path sets crossing, the intersection surviving into the readout — was
    /// only ever going to be checked by hand, and checking it by hand costs two autonomous agents
    /// and a person watching a screen for thirty seconds.
    ///
    /// `only-a.rs` is what makes this an intersection rather than a union: it is touched, it is
    /// measured, and it must NOT appear. Without it the assertion would pass on a bug that reported
    /// every path either tree touched.
    #[tokio::test]
    async fn two_real_trees_editing_one_file_collide_over_exactly_that_file() {
        let db = crate::storage::TempDb::new().await;
        let (tree_a, base_a) = crate::inspect::tests::seeded_repo();
        let (tree_b, base_b) = crate::inspect::tests::seeded_repo();
        std::fs::write(tree_a.path().join("shared.rs"), "written by a\n").unwrap();
        std::fs::write(tree_a.path().join("only-a.rs"), "a alone\n").unwrap();
        std::fs::write(tree_b.path().join("shared.rs"), "written by b\n").unwrap();
        seed_worktree_row(
            &db.pool,
            "job",
            1,
            "project-a",
            tree_a.path(),
            Some(&base_a),
        )
        .await;
        seed_worktree_row(
            &db.pool,
            "run",
            2,
            "project-a",
            tree_b.path(),
            Some(&base_b),
        )
        .await;

        measure(&db.pool).await;
        let collisions = for_project(&db.pool, "project-a").await.unwrap();

        assert_eq!(
            collisions.observed.state,
            State::Collide,
            "two real trees on one file did not read as a collision"
        );
        assert_eq!(
            collisions.observed.overlaps.len(),
            1,
            "one pair of trees should produce one overlap"
        );
        assert_eq!(
            collisions.observed.overlaps[0].paths,
            vec!["shared.rs".to_string()],
            "the overlap is not the intersection of what the two trees touched"
        );
        db.close().await;
    }

    /// A worktree with no base is left unmeasured, and the missing row is what the read turns into
    /// `not_measured`. Writing an empty set would say "I measured, and it touched nothing".
    #[tokio::test]
    async fn a_worktree_with_no_base_is_left_unmeasured_rather_than_measured_empty() {
        let db = crate::storage::TempDb::new().await;
        let (repo, _) = crate::inspect::tests::seeded_repo();
        seed_worktree_row(&db.pool, "job", 1, "project-a", repo.path(), None).await;

        measure(&db.pool).await;

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worktree_touched_paths")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
        db.close().await;
    }

    /// A git that refuses erases the previous row. Keeping it would let a measurement that has
    /// stopped being taken carry on answering `clean`.
    #[tokio::test]
    async fn a_measurement_that_fails_removes_the_previous_answer() {
        let db = crate::storage::TempDb::new().await;
        let absent = std::path::Path::new("C:/nucleos-does-not-exist");
        seed_worktree_row(
            &db.pool,
            "job",
            1,
            "project-a",
            absent,
            Some(&"a".repeat(40)),
        )
        .await;
        seed_measurement(&db.pool, "job", 1, "project-a", &["stale.rs"]).await;

        measure(&db.pool).await;

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worktree_touched_paths")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
        db.close().await;
    }

    /// The ordinary case: two trees in one file.
    #[test]
    fn two_trees_touching_one_file_are_one_overlap_naming_both() {
        let sets = vec![
            (
                owner("job", 1),
                set(&["core/src/runs.rs", "core/src/job.rs"]),
            ),
            (
                owner("run", 7),
                set(&["core/src/runs.rs", "shell/src/App.tsx"]),
            ),
        ];

        let found = overlaps(&sets);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].a, owner("job", 1));
        assert_eq!(found[0].b, owner("run", 7));
        assert_eq!(found[0].paths, vec!["core/src/runs.rs".to_string()]);
    }

    /// A lone tree collides with nobody, and two disjoint trees do not either.
    #[test]
    fn disjoint_sets_and_a_lone_set_produce_nothing() {
        assert!(overlaps(&[(owner("job", 1), set(&["a.rs"]))]).is_empty());
        assert!(
            overlaps(&[
                (owner("job", 1), set(&["a.rs"])),
                (owner("job", 2), set(&["b.rs"])),
            ])
            .is_empty()
        );
    }

    /// Three trees give three pairs, and each pair is named exactly once.
    #[test]
    fn three_trees_on_one_file_give_three_pairs_each_named_once() {
        let sets = vec![
            (owner("job", 1), set(&["a.rs"])),
            (owner("job", 2), set(&["a.rs"])),
            (owner("run", 3), set(&["a.rs"])),
        ];

        let found = overlaps(&sets);

        assert_eq!(found.len(), 3);
        let pairs: Vec<(i64, i64)> = found
            .iter()
            .map(|overlap| (overlap.a.id, overlap.b.id))
            .collect();
        assert_eq!(pairs, vec![(1, 2), (1, 3), (2, 3)]);
    }

    /// When both sources name the same path it is one event, not two: the stronger one wins. What
    /// is left to the predicted source is what has not been touched yet, which is the only thing it
    /// knows how to say better.
    #[test]
    fn a_path_both_sources_name_is_left_to_the_observed_one() {
        let declared = vec![Overlap {
            a: owner("job", 1),
            b: owner("job", 2),
            paths: vec!["written.rs".into(), "still_to_write.rs".into()],
        }];
        let observed = vec![Overlap {
            a: owner("job", 1),
            b: owner("job", 2),
            paths: vec!["written.rs".into()],
        }];

        let predicted = only_predicted(declared, &observed);

        assert_eq!(predicted.len(), 1);
        assert_eq!(predicted[0].paths, vec!["still_to_write.rs".to_string()]);
    }

    /// A pair whose prediction was entirely confirmed leaves the predicted source — otherwise the
    /// card would show two warnings for a single event.
    #[test]
    fn a_pair_the_observed_source_fully_covers_leaves_the_predicted_one() {
        let declared = vec![Overlap {
            a: owner("job", 1),
            b: owner("job", 2),
            paths: vec!["written.rs".into()],
        }];
        let observed = declared.clone();

        assert!(only_predicted(declared, &observed).is_empty());
    }

    /// Different pairs do not subtract from each other. A path shared between 1 and 2 says nothing
    /// about what 1 and 3 are going to do.
    #[test]
    fn the_subtraction_is_per_pair_and_not_per_path() {
        let declared = vec![Overlap {
            a: owner("job", 1),
            b: owner("job", 3),
            paths: vec!["shared.rs".into()],
        }];
        let observed = vec![Overlap {
            a: owner("job", 1),
            b: owner("job", 2),
            paths: vec!["shared.rs".into()],
        }];

        assert_eq!(only_predicted(declared.clone(), &observed), declared);
    }
}
