//! Daemon startup reconciliation and migration helpers.

use super::*;

/// The workflow library, with the built-in autopilot in it.
///
/// **Seeding failing does not stop the daemon**, and that is the whole of the error handling here.
/// A library that could not be written is a canvas with one fewer workflow on it; refusing to start
/// over it would take away email, chats, runs and the autopilot itself because a picture could not
/// be drawn. It is said out loud and then let go — the same shape [`crate::backup`] uses for a
/// restore that could not be applied.
pub(super) fn seeded_library() -> Option<std::path::PathBuf> {
    let root = workflows::library_root()?;
    match seed::seed(&root) {
        // The ordinary case, on every start after the first, and it says nothing.
        Ok(seed::Seeded::Unchanged) => {}
        Ok(seed::Seeded::Written) => {
            tracing::info!(
                name = seed::NAME,
                version = seed::VERSION,
                "seeded the built-in workflow"
            )
        }
        // The one case a person needs told: their disk changed under a name that means one thing.
        Ok(seed::Seeded::Restored) => tracing::warn!(
            name = seed::NAME,
            version = seed::VERSION,
            "the built-in workflow had been changed on disk and was restored; publish a new version \
             to keep your own"
        ),
        Err(error) => {
            tracing::warn!(%error, "could not seed the built-in workflow; the library is short one")
        }
    }
    Some(root)
}

/// Copies this machine's settings from the old `.ai/` beside the working directory into `root`,
/// and puts a line in the feed for each file it copied.
///
/// The copying is [`machine_config::migrate_legacy`]'s, and so is everything about when it does and
/// does not happen; this only supplies the two directories and tells somebody. The feed line is
/// said because a person looking for why their settings moved will look there, and a log line is
/// read by nobody on a desktop. A lost feed line is logged and let go: the copy already happened.
pub(super) async fn migrate_machine_settings(pool: &sqlx::SqlitePool, root: &std::path::Path) {
    let Ok(started_in) = std::env::current_dir() else {
        // No working directory means no `.ai/` beside it either, so there is nothing to copy.
        return;
    };
    for file in machine_config::migrate_legacy(root, &started_in) {
        let summary = format!(
            "{} copied from .ai/{file} in the directory the daemon was started from; the old file \
             was left where it was and is no longer read",
            machine_config::display_path(file)
        );
        if let Err(error) = feed::append(pool, None, "config_migrated", &summary, None, None).await
        {
            tracing::warn!(%error, file, "a settings file was copied but the feed line was lost");
        }
    }
}

/// Copies each rostered project's state files from its old `<project root>/.ai/` into
/// `~/.nucleos/projects/<project_id>/`, and puts a line in that project's feed for each one.
///
/// The copying is [`project::state::migrate_legacy`]'s, and so is every rule about when it does and
/// does not happen; this only reads the roster and tells somebody, for the reason
/// [`migrate_machine_settings`] gives. The roster is every project with a root on record — a
/// project in `off` has none, and its old files are copied the next time it is given one and the
/// daemon starts.
///
/// An unreadable roster copies nothing and says so: those projects behave as projects with no rules
/// file until the next start, which is the state an absent file has always meant.
pub(super) async fn migrate_project_state(pool: &sqlx::SqlitePool, root: &std::path::Path) {
    let roster: Vec<(String, String)> = match sqlx::query_as(
        "SELECT project_id, project_root FROM autopilot_state WHERE project_root IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    {
        Ok(roster) => roster,
        Err(error) => {
            tracing::warn!(%error, "could not read the project roster, so no project's .ai/ state was copied");
            return;
        }
    };
    let roster: Vec<(String, std::path::PathBuf)> = roster
        .into_iter()
        .map(|(id, project_root)| (id, std::path::PathBuf::from(project_root)))
        .collect();
    let root = root.to_path_buf();
    let copied =
        match tokio::task::spawn_blocking(move || project::state::migrate_legacy(&root, &roster))
            .await
        {
            Ok(copied) => copied,
            Err(error) => {
                tracing::warn!(%error, "copying project state from .ai/ did not finish");
                return;
            }
        };
    for (project_id, file) in copied {
        let summary = format!(
            "{} copied from .ai/{file} in this project's folder; the old file was left where it \
             was and is no longer read",
            project::state::display_path(&project_id, file)
        );
        if let Err(error) = feed::append(
            pool,
            Some(&project_id),
            "config_migrated",
            &summary,
            None,
            None,
        )
        .await
        {
            tracing::warn!(%error, %project_id, file, "a project state file was copied but the feed line was lost");
        }
    }
}

/// Marks every rostered project that passed the old activation check as onboarded, once, and puts
/// a line in that project's feed for each one.
///
/// So that nobody loses autopilot on update: activation used to require `.ai/workflow/workflow.md`
/// and now requires the onboarding marker, so a project that had the first and not the second is
/// given a `migrated: true` marker. The rules are [`onboarding::migrate_legacy`]'s; this reads the
/// same roster [`migrate_project_state`] reads — every project with a root on record — and tells
/// somebody, for the reason [`migrate_machine_settings`] gives.
pub(super) async fn migrate_onboarding(pool: &sqlx::SqlitePool, root: &std::path::Path) {
    let roster: Vec<(String, String)> = match sqlx::query_as(
        "SELECT project_id, project_root FROM autopilot_state WHERE project_root IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    {
        Ok(roster) => roster,
        Err(error) => {
            tracing::warn!(%error, "could not read the project roster, so no project was marked onboarded");
            return;
        }
    };
    let roster: Vec<(String, std::path::PathBuf)> = roster
        .into_iter()
        .map(|(id, project_root)| (id, std::path::PathBuf::from(project_root)))
        .collect();
    let root = root.to_path_buf();
    let now = chrono::Utc::now().to_rfc3339();
    let marked =
        match tokio::task::spawn_blocking(move || onboarding::migrate_legacy(&root, &roster, &now))
            .await
        {
            Ok(marked) => marked,
            Err(error) => {
                tracing::warn!(%error, "marking projects onboarded did not finish");
                return;
            }
        };
    for project_id in marked {
        let summary = format!(
            "marked onboarded in {}, because this project had .ai/workflow/workflow.md, which is \
             what onboarded meant before; that file is no longer read",
            project::state::display_path(&project_id, onboarding::MARKER_FILE)
        );
        if let Err(error) = feed::append(
            pool,
            Some(&project_id),
            onboarding::FEED_KIND,
            &summary,
            None,
            None,
        )
        .await
        {
            tracing::warn!(%error, %project_id, "a project was marked onboarded but the feed line was lost");
        }
    }
}

pub(super) async fn reconcile_after_restart(pool: &sqlx::SqlitePool) {
    // Beside the run sweep and for the same reason: a row saying `running` against a process that
    // has not existed since the last restart would refuse every future click with "already
    // running". Best-effort — a project command left unsettled is a button that will not press, not
    // a daemon that must not start.
    match project::commands::reconcile_orphaned_commands(pool).await {
        Ok(0) => {}
        Ok(settled) => tracing::info!(
            settled,
            "settled project commands left running by a restart"
        ),
        Err(error) => tracing::warn!(%error, "could not settle project commands left running"),
    }

    let interrupted = execution::runs::reconcile_orphaned_runs(pool)
        .await
        .expect("failed to reconcile orphaned runs on startup");
    if interrupted > 0 {
        tracing::warn!(
            "reconciled {interrupted} run(s) left 'running' by a previous crash -> 'interrupted'"
        );
    }

    // Spec 2026-10-04 §4.2 item 5: a task caught mid-flight by a restart is `orphaned`, beside the
    // turns marked `interrupted` above. Best effort: a stale task row must not stop the daemon.
    match chats::tasks::orphan_running(pool).await {
        Ok(0) => {}
        Ok(orphaned) => tracing::warn!(
            orphaned,
            "chat tasks left running by a restart -> 'orphaned'"
        ),
        Err(error) => tracing::warn!(%error, "could not mark chat tasks left running"),
    }

    let stranded = execution::runs::reconcile_stranded_approvals(pool)
        .await
        .expect("failed to reconcile stranded approval pauses on startup");
    if stranded > 0 {
        tracing::warn!(
            "reconciled {stranded} run(s) left 'awaiting_approval' with no pending proposal -> 'interrupted'"
        );
    }

    // Spec A D14: heals rows a release or a job cancel left before `expire_for_run` existed, and
    // does not `expect` — unlike the two reconciliations above, a pending approval holds no slot,
    // so the daemon must not refuse to start over one.
    match proposals::expire_orphaned_approvals(pool).await {
        Ok(0) => {}
        Ok(expired) => tracing::warn!(
            "expired {expired} approval(s) whose run had already stopped (spec A D14)"
        ),
        Err(error) => tracing::warn!(%error, "could not expire orphaned approvals at startup"),
    }

    // After the run reconciliations, and for a reason worth stating: they mark every run left
    // `running` as `interrupted`, so by now no job has a live node under it — which means "has no
    // live run" is true of every job, including the ones that died in the gap between two nodes.
    // Those are exactly the recoverable ones, so the pass below discriminates on HEAD instead.
    match execution::job::reconcile_orphaned_jobs(pool).await {
        Ok(retired) if retired > 0 => {
            tracing::warn!(
                "retired {retired} job(s) whose repository moved while the daemon was down -> 'interrupted'"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "orphaned-job reconciliation failed"),
    }

    // After both owner reconciliations above, and that order is the whole correctness of this pass:
    // it frees a slot by asking whether its owner is still live, and before those two every dead
    // owner still reads live. Run earlier it would free nothing at all.
    match execution::concurrency::reconcile_orphaned_slots(pool).await {
        Ok(freed) if freed > 0 => {
            tracing::warn!("freed {freed} concurrency slot(s) left held by a previous crash");
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "orphaned concurrency slot sweep failed"),
    }

    // Neither fatal like the run reconciliations above nor mere hygiene like the worktree sweep
    // below: louder than the sweep, quieter than the panics.
    //
    // A verification unit left `running` was cut off by the restart, and the agent holding its ticket
    // is still waiting: put it back in the queue. One that keeps taking the daemon down gives up
    // after `verify::runs::MAX_INTERRUPTIONS` instead of looping.
    match verify::runs::requeue_interrupted(pool).await {
        Ok((requeued, given_up)) if requeued + given_up > 0 => tracing::warn!(
            "requeued {requeued} verification unit(s) interrupted by the restart; gave up on {given_up}"
        ),
        Ok(_) => {}
        Err(error) => tracing::error!(%error, "verification queue reconciliation failed"),
    }

    // A `vcs_requests` row left `running` holds its repository's only slot — the partial unique
    // index sees to that — so failing to clear it means no git operation for that project until
    // somebody notices. That is a jam, not untidiness, hence `error!`. But it is one pillar's queue:
    // refusing to boot mail, voice, calendar and runs over it would trade a stuck repository for a
    // stuck machine.
    match execution::vcs::reconcile_interrupted(pool).await {
        Ok(released) if released > 0 => {
            tracing::warn!(
                "reconciled {released} vcs request(s) left 'running' by a previous crash -> 'interrupted'"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::error!(
            %error,
            "vcs request reconciliation failed — a repository may stay queue-locked, and nothing retries before the next startup"
        ),
    }

    // After the run reconciliations, and that order is the whole of this pass's correctness: they
    // mark every run left `running` as `interrupted`, so by now no council has a live seat and
    // "still running" needs no further test to mean "abandoned". A council left that way is stuck
    // at a phase that will never advance, with nothing to advance it.
    match council::reconcile(pool).await {
        Ok(reconciled) if reconciled > 0 => {
            tracing::warn!(
                "reconciled {reconciled} council(s) left deliberating by a previous crash -> 'error'"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "council reconciliation failed"),
    }

    // After the run reconciliations above, so nothing from a previous life still counts as live.
    match execution::worktree::reconcile_orphaned_worktrees(
        pool,
        execution::worktree::ORPHAN_MIN_AGE,
        execution::worktree::GC_BACKOFF,
    )
    .await
    {
        Ok(collected) if collected > 0 => {
            tracing::warn!("collected {collected} orphaned worktree director(ies) on startup");
        }
        Ok(_) => {}
        // Hygiene, not a prerequisite: a daemon that cannot tidy up must still start.
        Err(error) => tracing::warn!(%error, "orphaned-worktree reconciliation failed"),
    }
}
