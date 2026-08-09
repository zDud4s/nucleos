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

// TEMPORARY, and it goes away with the read path: until `http.rs` calls `for_project`, nothing
// reachable from `main` names anything in this module, and `cargo clippy --all-targets -- -D
// warnings` fails the gate on seven `dead_code` findings. At module level rather than seven
// attributes because it is meant to be deleted in one edit, not maintained.
#![allow(dead_code)]

use std::collections::BTreeSet;

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
/// **It truncates by starvation, not by deferral.** The order is stable (`ORDER BY owner_kind,
/// owner_id`), so the seventeenth tree is not measured on the next tick — it is not measured at
/// all, and its project stays `not_measured` for as long as that holds. It is the safe side, and it
/// is not what a reader assumes on seeing a per-pass ceiling.
const MEASURE_CAP: usize = 16;

/// How long a tick gives the whole pass.
///
/// The tick is 30 seconds and drives jobs after this. A third is enough slack for half a dozen
/// `git status` calls, and the deadline protects the tick from a hung git — which
/// `inspect::run_git` cannot kill while it writes nothing (a limitation inherited from
/// `inspect::diff`).
const MEASURE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// When a measurement stops counting.
///
/// Ten ticks. Not a condition the spec enumerates — it is this plan's addition, because without it
/// a git that stopped answering freezes the last good value and the screen carries on saying
/// `clean` about a measurement no longer being taken. Age reads as `not_measured`, never as
/// `clean`.
const MEASUREMENT_TTL: chrono::Duration = chrono::Duration::minutes(5);

#[derive(sqlx::FromRow)]
struct LiveTree {
    owner_kind: String,
    owner_id: i64,
    project_id: String,
    path: String,
    base_sha: Option<String>,
}

/// Measures every live worktree and stores the result. Called by the job tick.
///
/// Nothing here propagates an error: a measurement is a warning, and a warning that could not be
/// computed must not stop the tick that drives the work. What was not measured is left without a
/// row, and the read turns that into `not_measured`.
pub async fn measure(pool: &sqlx::SqlitePool) {
    let trees: Vec<LiveTree> = match sqlx::query_as(
        "SELECT owner_kind, owner_id, project_id, path, base_sha
         FROM worktrees WHERE removed_at IS NULL ORDER BY owner_kind, owner_id",
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
    let measurable: Vec<LiveTree> = candidates.into_iter().take(MEASURE_CAP).collect();
    if candidate_count > MEASURE_CAP {
        tracing::warn!(
            candidates = candidate_count,
            measuring = MEASURE_CAP,
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
        // The ones never drained keep their previous answer, which `MEASUREMENT_TTL` eventually
        // invalidates. The tick waits no longer than this.
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
        sqlx::query(
            "INSERT INTO jobs (id, project_id, project_root, status, max_items, created_at)
             VALUES (?, ?, 'C:/somewhere', 'implementing', 5, '2026-08-08T00:00:00Z')",
        )
        .bind(id)
        .bind(project_id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// `files` is a JSON array, or `NULL` — which is the ordinary case, not the exception.
    async fn seed_item(
        pool: &sqlx::SqlitePool,
        job_id: i64,
        ordinal: i64,
        status: &str,
        files: Option<&[&str]>,
    ) {
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
        .unwrap();
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
