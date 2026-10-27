//! Background collection of finished and orphaned worktrees.

use super::*;

// Two arms rather than one join, because a worktree's owner decides which table says whether it is
// finished. `owner_kind` is load-bearing in both: the joins match on the bare id, and run ids and
// job ids come from different sequences, so without the filters a job whose id happened to equal a
// terminal run's would have its worktree collected mid-use. There is a test on exactly that
// (`a_job_worktree_is_not_collected_by_a_run_of_the_same_id`).
//
// Both arms list terminal statuses explicitly rather than excluding live ones. A status added later
// then defaults to *not collected* — a stale directory — instead of to deleting the worktree of
// something still using it. That is the safe direction and still a leak, so each arm is held
// against the module that writes those statuses:
// `job::every_ending_a_job_can_have_is_an_ending_the_gc_collects` for the job arm, and
// `runs::every_ending_a_run_can_have_is_an_ending_the_gc_collects` for the run arm. The run arm had
// no such guard until `superseded` was found missing from it.
pub const GC_CANDIDATES_SQL: &str =
    "SELECT w.owner_kind, w.owner_id, w.project_id, w.project_root, w.path, w.branch
         FROM worktrees w
         JOIN runs r ON r.id = w.owner_id
         WHERE w.owner_kind = 'run'
           AND w.removed_at IS NULL
           AND r.status IN ('completed','failed','cancelled','timed_out','interrupted','superseded')
           AND COALESCE(r.completed_at, w.created_at) <= ?
           AND NOT EXISTS (SELECT 1 FROM verify_wakes v
                           WHERE v.run_id = w.owner_id AND v.state = 'pending')
         UNION ALL
         SELECT w.owner_kind, w.owner_id, w.project_id, w.project_root, w.path, w.branch
         FROM worktrees w
         JOIN jobs j ON j.id = w.owner_id
         WHERE w.owner_kind = 'job'
           AND w.removed_at IS NULL
           AND j.status IN ('completed','failed','gate_failed','gate_errored','expired','stopped',
                            'cancelled','interrupted')
           AND COALESCE(j.completed_at, w.created_at) <= ?
         UNION ALL
         SELECT w.owner_kind, w.owner_id, w.project_id, w.project_root, w.path, w.branch
         FROM worktrees w
         JOIN job_nodes i ON i.id = w.owner_id
         JOIN jobs j ON j.id = i.job_id
         WHERE w.owner_kind = 'item'
           AND w.removed_at IS NULL
           AND j.status IN ('completed','failed','gate_failed','gate_errored','expired','stopped',
                            'cancelled','interrupted')
           AND COALESCE(j.completed_at, w.created_at) <= ?
         ORDER BY 1, 2";

pub async fn gc_candidates(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    retention: chrono::Duration,
) -> sqlx::Result<Vec<WorktreeRow>> {
    let cutoff = (now - retention).to_rfc3339();
    sqlx::query_as(GC_CANDIDATES_SQL)
        .bind(&cutoff)
        .bind(&cutoff)
        .bind(&cutoff)
        .fetch_all(pool)
        .await
}

fn retention() -> chrono::Duration {
    std::env::var("NUCLEOS_WORKTREE_RETENTION_HOURS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .map(chrono::Duration::hours)
        .unwrap_or_else(|| chrono::Duration::hours(72))
}

pub async fn gc_pass(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    retention: chrono::Duration,
    backoff: &[Duration],
) {
    let candidates = gc_candidates(pool, now, retention)
        .await
        .unwrap_or_default();

    let project_roots = candidates
        .iter()
        .map(|worktree| worktree.project_root.clone())
        .collect::<std::collections::HashSet<_>>();
    for project_root in project_roots {
        let _ = git()
            .arg("-C")
            .arg(project_root)
            .arg("worktree")
            .arg("prune")
            .output()
            .await;
    }

    for worktree in candidates {
        // A row whose kind this build does not recognise is not ours to delete. `gc_candidates`
        // filters to `owner_kind = 'run'`, so this cannot fire today; it exists so that a future
        // kind added to the CHECK constraint but not to `Owner` skips collection rather than
        // falling through to a branch that removes directories.
        let Some(owner) = worktree.owner() else {
            tracing::warn!(
                owner_kind = %worktree.owner_kind,
                owner_id = worktree.owner_id,
                "skipping a worktree row whose owner kind is unrecognised"
            );
            continue;
        };
        let feed_run_id = owner.feed_run_id();
        let subject = feed_subject(pool, owner).await;

        match remove(
            Path::new(&worktree.project_root),
            Path::new(&worktree.path),
            backoff,
        )
        .await
        {
            Ok(()) => {
                if let Err(error) = mark_removed(pool, owner).await {
                    tracing::warn!(
                        owner_id = owner.id(),
                        %error,
                        "failed to mark collected worktree as removed"
                    );
                }

                let branch_deleted =
                    delete_branch_if_merged(&worktree.project_root, &worktree.branch).await;
                let summary = if branch_deleted {
                    format!("removed worktree + merged branch {}", worktree.branch)
                } else {
                    format!("removed worktree {}", worktree.path)
                };
                let _ = feed::append(
                    pool,
                    Some(&worktree.project_id),
                    "worktree_removed",
                    &summary,
                    feed_run_id,
                    subject.as_ref(),
                )
                .await;
                feed_branch_outcome(pool, &worktree, branch_deleted).await;
            }
            // A directory that is no longer there is the outcome this pass wanted, however it got
            // that way — a hand-deleted stale worktree, a `git worktree remove` that ran elsewhere,
            // a volume that came back empty. `git worktree remove` still fails on a path git has
            // forgotten, so without this the row stayed a candidate: same backoff, same failure,
            // another feed row, every half hour forever.
            Err(_) if !Path::new(&worktree.path).exists() => {
                if let Err(error) = mark_removed(pool, owner).await {
                    tracing::warn!(
                        owner_id = owner.id(),
                        %error,
                        "failed to retire a worktree row whose directory had already gone"
                    );
                }
                tracing::info!(
                    owner_id = owner.id(),
                    path = %worktree.path,
                    "worktree directory was already gone; retiring its row"
                );
            }
            // A feed row is a Telegram message, and this arm runs every half hour for as long as
            // the failure lasts. The failure is news once; the same failure again is not, so it
            // goes to the log instead. The comparison is against the last thing this worktree's
            // row said — a *different* failure (or the same one after a success in between, which
            // retires the row) is reported again.
            Err(error) => {
                let prefix = format!("failed to remove worktree {}:", worktree.path);
                let summary = format!("{prefix} {error}");
                let already_reported =
                    feed::latest_summary(pool, "worktree_gc_failed", feed_run_id, &prefix)
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|last| last == summary);
                if already_reported {
                    tracing::info!(
                        owner_id = owner.id(),
                        path = %worktree.path,
                        %error,
                        "worktree cleanup failed the same way as last pass; not repeating the feed row"
                    );
                    continue;
                }
                let _ = feed::append(
                    pool,
                    Some(&worktree.project_id),
                    "worktree_gc_failed",
                    &summary,
                    feed_run_id,
                    subject.as_ref(),
                )
                .await;
            }
        }
    }
}

/// How old an unaccounted-for directory must be before startup will collect it. Generous on purpose:
/// the cost of waiting is a stale directory for an hour, the cost of being wrong is deleting a
/// worktree a live run is mid-way through creating.
pub const ORPHAN_MIN_AGE: Duration = Duration::from_secs(3600);

/// Worktree directories on disk that no LIVE `worktrees` row accounts for.
///
/// Two ways one appears, and the normal GC pass can reach neither, because it only ever walks rows
/// that exist: a crash between `git worktree add` and the row INSERT leaves a directory nothing in
/// the database knows about, and a removal that set `removed_at` but whose files never actually went
/// away leaves a directory the GC now believes is gone. Both leak forever without this.
///
/// Fail-safe: a directory whose age cannot be determined is left alone, and anything not named
/// `run-<id>` or `job-<id>` is not ours to touch.
/// The owner a worktree directory name denotes, or `None` when the name is not ours to touch.
///
/// Fail-safe by construction: an unparseable name yields `None` and the sweeper leaves the directory
/// alone. Nothing here may split on `.` — the operations downstream delete directories, and a looser
/// parse would let a sibling like `job-5.artifacts` be read as job 5 and taken with it.
pub(super) fn owner_from_dir_name(name: &str) -> Option<Owner> {
    if let Some(rest) = name.strip_prefix("run-") {
        return rest.parse::<i64>().ok().map(Owner::Run);
    }
    if let Some(rest) = name.strip_prefix("job-") {
        return rest.parse::<i64>().ok().map(Owner::Job);
    }
    // Without this arm an item's checkout — the biggest directory this daemon creates, since it
    // carries a `target/` of its own — is invisible to the orphan sweep for ever.
    if let Some(rest) = name.strip_prefix("item-") {
        return rest.parse::<i64>().ok().map(Owner::Item);
    }
    None
}

/// The orphans of one candidate root, appended onto `orphans`.
///
/// Factored out of `orphaned_worktrees` so the current default and the legacy sibling location are
/// swept by exactly one body and cannot drift apart. A root that does not exist — the ordinary case
/// for the legacy location on a daemon that never had one, or for either root before its first
/// worktree — answers with no entries rather than an error: `orphaned_worktrees` only ever reports
/// what a directory actually holds.
async fn orphans_under(
    root: &Path,
    live: &std::collections::HashSet<(String, i64)>,
    min_age: Duration,
    orphans: &mut Vec<PathBuf>,
) {
    let Ok(mut entries) = tokio::fs::read_dir(root).await else {
        return;
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let file_name = entry.file_name();
        let Some(owner) = owner_from_dir_name(&file_name.to_string_lossy()) else {
            continue;
        };
        if live.contains(&(owner.kind().to_owned(), owner.id())) {
            continue;
        }

        let old_enough = entry
            .metadata()
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= min_age);
        if !old_enough {
            continue;
        }

        orphans.push(entry.path());
    }
}

pub async fn orphaned_worktrees(
    pool: &SqlitePool,
    project_root: &Path,
    min_age: Duration,
) -> sqlx::Result<Vec<PathBuf>> {
    // Keyed by the full owner pair, not by id alone: run ids and job ids come from different
    // sequences, so a live `run-7` row would otherwise account for a `job-7` directory and leave it
    // uncollectable forever.
    let live: std::collections::HashSet<(String, i64)> =
        sqlx::query_as("SELECT owner_kind, owner_id FROM worktrees WHERE removed_at IS NULL")
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();

    let mut orphans = Vec::new();
    let root = worktree_root(project_root);
    orphans_under(&root, &live, min_age, &mut orphans).await;

    // The legacy sibling root predates this task's move of the default inside the project. Swept
    // unconditionally — not only when `root` is the legacy default — because `NUCLEOS_WORKTREE_ROOT`
    // says where NEW worktrees are created, not where a tree left by a daemon that predates the
    // override might still be sitting; a worktree already there before the override was ever set
    // must stay collectable, override or not. Guarded against `root` only to avoid walking the same
    // directory twice on the rare host where the two happen to coincide.
    let legacy = legacy_worktree_root(project_root);
    if !paths_equal(&legacy, &root) {
        orphans_under(&legacy, &live, min_age, &mut orphans).await;
    }

    // `NUCLEOS_WORKTREE_ROOT` is one directory for every project, so a `run-5` under it may be
    // another repository's tree. That repository's git does not list it here, which is exactly the
    // `Registration::Absent` that lets `collect_by_hand` delete it — so a tree is only this
    // project's to sweep when this repository's own bookkeeping claims it.
    let mut ours = Vec::with_capacity(orphans.len());
    for orphan in orphans {
        if belongs_to_project(project_root, &orphan).await {
            ours.push(orphan);
        } else {
            tracing::debug!(
                path = %orphan.display(),
                project = %project_root.display(),
                "leaving a worktree directory another repository owns to that repository's sweep"
            );
        }
    }
    Ok(ours)
}

/// Whether a directory under a worktree root is `project_root`'s to sweep.
///
/// Yes when this repository's `git worktree list` registers it, or when its `.git` file points into
/// this repository's git directory (a registration gone stale). A directory with no `.git` at all is
/// one `git worktree add` never finished, and holds no history anybody could lose. Anything else —
/// a `.git` naming another repository, a `.git` directory, an answer git could not give — is not
/// ours, and the sweep leaves it.
async fn belongs_to_project(project_root: &Path, dir: &Path) -> bool {
    let dot_git = dir.join(".git");
    if tokio::fs::symlink_metadata(&dot_git).await.is_err() {
        return true;
    }
    if matches!(
        registration(project_root, dir).await,
        Ok(Registration::Present)
    ) {
        return true;
    }

    let Ok(contents) = tokio::fs::read_to_string(&dot_git).await else {
        return false;
    };
    let Some(gitdir) = contents.trim().strip_prefix("gitdir:") else {
        return false;
    };
    let gitdir = PathBuf::from(gitdir.trim());

    let Ok(output) = git()
        .arg("-C")
        .arg(project_root)
        .arg("rev-parse")
        .arg("--path-format=absolute")
        .arg("--git-common-dir")
        .output()
        .await
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let common = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    if path_contains(&common, &gitdir) {
        return true;
    }
    // Spelled differently (a short 8.3 name, a symlinked temp dir): compare where they resolve.
    match (
        std::fs::canonicalize(&common),
        std::fs::canonicalize(&gitdir),
    ) {
        (Ok(common), Ok(gitdir)) => path_contains(&common, &gitdir),
        _ => false,
    }
}

/// The last resort for an orphan `git worktree remove` would not take: delete the directory here.
///
/// Reached only when git no longer holds a row for the path, and that single condition is what
/// makes the delete defensible. Git refuses removal for reasons that must be respected — a
/// worktree someone locked needs `--force` twice, which `try_remove_once` deliberately does not
/// pass — and every refusal leaves the row in place. So the row is the signal: finishing a removal
/// git already began is repair, while deleting past one it declined is data loss. The failure's
/// own message is not consulted, because it is localised and version-dependent.
pub(super) async fn collect_by_hand(project_root: &Path, orphan: &Path, cause: &io::Error) -> bool {
    if is_dangerous_removal_path(orphan, project_root) {
        tracing::warn!(
            path = %orphan.display(),
            %cause,
            "refusing to recursively delete an orphan at a dangerous path"
        );
        return false;
    }

    match registration(project_root, orphan).await {
        Ok(Registration::Absent) => {}
        Ok(Registration::Present) => {
            tracing::warn!(
                path = %orphan.display(),
                %cause,
                "git still registers this worktree, so its removal was refused rather than \
                 interrupted; leaving it for the next startup"
            );
            return false;
        }
        Err(error) => {
            tracing::warn!(
                path = %orphan.display(),
                %cause,
                %error,
                "could not ask git whether this worktree is still registered; leaving it alone"
            );
            return false;
        }
    }

    if let Err(error) = tokio::fs::remove_dir_all(orphan).await {
        tracing::warn!(
            path = %orphan.display(),
            %cause,
            %error,
            "failed to remove an orphaned worktree directory"
        );
        return false;
    }

    // The directory is gone, so the orphan is collected however the bookkeeping went. Reporting
    // otherwise would promise a retry that cannot happen — the sweep only ever finds directories,
    // and this one no longer exists. A surviving row is still worth saying out loud: it is exactly
    // the leak this step exists to close.
    if let Err(error) = prune_and_verify_registration_gone(project_root, orphan).await {
        tracing::warn!(
            path = %orphan.display(),
            %error,
            "removed the orphan directory but git's registration did not clear; \
             run `git worktree prune` in the project to drop it"
        );
    }

    true
}

/// Sweeps orphaned worktree directories for every project the daemon knows a root for, returning how
/// many it collected. Runs once at startup, where `reconcile_orphaned_runs` has already established
/// that nothing from a previous life is still running.
pub async fn reconcile_orphaned_worktrees(
    pool: &SqlitePool,
    min_age: Duration,
    backoff: &[Duration],
) -> sqlx::Result<usize> {
    // Both sources matter: a project switched off still owns whatever its runs left behind.
    let roots: Vec<String> = sqlx::query_scalar(
        "SELECT project_root FROM autopilot_state WHERE project_root IS NOT NULL
         UNION
         SELECT project_root FROM worktrees",
    )
    .fetch_all(pool)
    .await?;

    let mut collected = 0;
    for root in roots {
        let project_root = Path::new(&root);
        for orphan in orphaned_worktrees(pool, project_root, min_age).await? {
            // `git worktree remove` first, so git's own bookkeeping is updated when it still knows
            // about the worktree; a bare directory it never registered falls through to the fs.
            let removed = match remove(project_root, &orphan, backoff).await {
                Ok(()) => true,
                Err(error) if is_preservation_failure(&error) => false,
                Err(error) => collect_by_hand(project_root, &orphan, &error).await,
            };
            if removed {
                collected += 1;
                tracing::warn!(
                    path = %orphan.display(),
                    "collected an orphaned worktree directory left by a previous daemon"
                );
            } else {
                tracing::warn!(
                    path = %orphan.display(),
                    "could not collect an orphaned worktree directory; will retry next startup"
                );
            }
        }
    }
    Ok(collected)
}

pub async fn run_gc(pool: SqlitePool) {
    let mut interval = tokio::time::interval(Duration::from_secs(1800));
    loop {
        interval.tick().await;
        gc_pass(&pool, Utc::now(), retention(), GC_BACKOFF).await;
    }
}
