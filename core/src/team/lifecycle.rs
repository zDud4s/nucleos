//! Team run startup, cancellation, reconciliation, and daemon loops.

use super::*;

// ---------------------------------------------------------------------------------------------
// Starting a run
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
pub enum StartError {
    NotFound,
    Invalid(String),
    BudgetExhausted(String),
    QuotaExhausted(String),
    Unavailable(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("team not found"),
            Self::Invalid(message)
            | Self::BudgetExhausted(message)
            | Self::QuotaExhausted(message)
            | Self::Unavailable(message) => formatter.write_str(message),
        }
    }
}

/// `start_with` at `normal`, with no lineage: the shape most tests start a run in. Test-only since
/// `post_team_run` began carrying a speed, which left no production caller.
#[cfg(any(test, feature = "testkit"))]
pub async fn start(
    state: &AppState,
    team_id: &str,
    request: &str,
    report_to_chat_id: Option<&str>,
) -> Result<String, StartError> {
    start_with(
        state,
        team_id,
        request,
        Lineage::default(),
        crate::speed::Speed::Normal,
        report_to_chat_id,
    )
    .await
}

/// Starts a department on one request: validates, mints the key, makes the folder, writes the row.
///
/// Returns as soon as the row exists — everything after is the tick's. The four refusals here are
/// the four things that cannot be discovered later without wasting money: a team with nobody in it,
/// a director that was deleted, a local member on a machine with no local model, and a house budget
/// that is already spent.
///
/// The caller says where the run came from and where it should speak. One function and not two
/// paths: a triggered run is an ordinary run with four columns filled in, and every refusal
/// applies to it unchanged. What a trigger adds — the depth, the tree
/// ceiling, the live-run counts — is checked by `team_trigger::fire` BEFORE it gets here, because
/// those are questions about whether to start at all rather than about whether this team can.
/// `speed` is this run's, chosen by whoever started it (speed spec, section 3.4).
pub async fn start_with(
    state: &AppState,
    team_id: &str,
    request: &str,
    lineage: Lineage,
    speed: crate::speed::Speed,
    report_to_chat_id: Option<&str>,
) -> Result<String, StartError> {
    let request = request.trim();
    if request.is_empty() {
        return Err(StartError::Invalid("the request is empty".to_owned()));
    }

    // Refused HERE and not when the department first tries to speak. A destination that does not
    // exist is a mistake by whoever started the run, and they are present, at a keyboard, right now;
    // discovering it two hours later means the refusal reaches a director mid-turn instead — which
    // can do nothing about it except write the words into a delivery nobody asked for.
    //
    // `chats::brain_of` rather than a second `archived_at IS NULL` clause, for `relay::admit`'s
    // reason: "still there" has exactly one definition in this codebase and this is not a second.
    if let Some(chat_id) = report_to_chat_id {
        let lives = crate::chats::brain_of(&state.pool, chat_id)
            .await
            .map_err(|error| StartError::Unavailable(error.to_string()))?
            .is_some();
        if !lives {
            return Err(StartError::Invalid(format!(
                "there is no conversation `{chat_id}` for this department to report in"
            )));
        }
    }

    let team = get(&state.pool, team_id)
        .await
        .map_err(|error| StartError::Unavailable(error.to_string()))?
        .ok_or(StartError::NotFound)?;

    if team.members.is_empty() {
        return Err(StartError::Invalid(
            "this team has no members yet; add at least one specialist".to_owned(),
        ));
    }

    // The director is asked for by id rather than assumed present: `agent::delete` refuses to
    // delete one, but a database restored from a backup taken before the team existed would not
    // have that history.
    let director = crate::agent::get(&state.pool, &team.team.director_agent_id)
        .await
        .map_err(|error| StartError::Unavailable(error.to_string()))?
        .ok_or_else(|| {
            StartError::Invalid(format!(
                "this team's director ({}) is no longer in the catalogue",
                team.team.director_agent_id
            ))
        })?;

    // A local member with no local assistant on this machine is refused rather than silently run in
    // the cloud, which would swap the model the owner chose for one that spends.
    let mut members = Vec::with_capacity(team.members.len());
    for id in &team.members {
        let agent = crate::agent::get(&state.pool, id)
            .await
            .map_err(|error| StartError::Unavailable(error.to_string()))?
            .ok_or_else(|| {
                StartError::Invalid(format!("{id} is on this team and not in the catalogue"))
            })?;
        members.push(agent);
    }
    // `state.local_assistant.is_none()` before the migration to the assistant factory: the field
    // this read no longer exists, so this call site is the one line that migration is allowed to
    // touch beyond the `AppState` literal.
    if state.assistants.serves(crate::chats::Brain::Local).is_err()
        && let Some(local) = members
            .iter()
            .chain(std::iter::once(&director))
            .find(|agent| agent.engine == "local")
    {
        return Err(StartError::Invalid(format!(
            "{} runs on a local model and this machine has none available",
            local.id
        )));
    }

    match crate::quota::permits_new_run(state, chrono::Utc::now()).await {
        crate::governance::budget::BudgetDecision::Allow => {}
        crate::governance::budget::BudgetDecision::Pause { reason, source, .. } => {
            return Err(if source == crate::quota::PAUSE_SOURCE {
                StartError::QuotaExhausted(reason)
            } else {
                StartError::BudgetExhausted(reason)
            });
        }
    }

    let id = crate::auth::generate_uuid_v4();
    let workspace = workspace_for(&team.team.id, &id);

    // The folder before the row: a run whose row exists and whose folder does not is a run whose
    // every specialist fails on its first write. The other order leaves an empty directory the GC
    // collects, which costs nothing.
    let root = files_root(state).map_err(StartError::Unavailable)?;
    let folder = crate::files::resolve_within(&root, &workspace)
        .map_err(|error| StartError::Unavailable(format!("{error:?}")))?;
    std::fs::create_dir_all(&folder).map_err(|error| StartError::Unavailable(error.to_string()))?;

    let (_, secret) = crate::auth::mint_team_token(&id);
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO team_runs (id, team_id, request, workspace, token, state, created_at,
                                updated_at, trigger_id, parent_id, root_id, depth,
                                report_to_chat_id, speed)
         VALUES (?, ?, ?, ?, ?, 'planning', ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&team.team.id)
    .bind(request)
    .bind(&workspace)
    .bind(&secret)
    .bind(&now)
    .bind(&now)
    .bind(lineage.trigger_id)
    .bind(&lineage.parent_id)
    // Its own id when nobody named a root. Written rather than left NULL for the reason the column
    // documents: a sentinel meaning "me" is a `COALESCE` at every reader, and one missing `COALESCE`
    // is a tree ceiling read off the wrong run.
    .bind(lineage.root_id.as_deref().unwrap_or(&id))
    .bind(lineage.depth)
    // NULL for nearly every run, which is what makes a department that reports nowhere behave
    // exactly as it did before this column existed.
    .bind(report_to_chat_id)
    .bind(speed.as_str())
    .execute(&state.pool)
    .await
    .map_err(|error| StartError::Unavailable(error.to_string()))?;

    let _ = crate::feed::append(
        &state.pool,
        None,
        "team_run_started",
        &format!("{} was asked to {request}", team.team.name),
        None,
        Some(&crate::feed::Subject::TeamRun(id.clone())),
    )
    .await;

    Ok(id)
}

// ---------------------------------------------------------------------------------------------
// Cancellation, reconciliation, retention
// ---------------------------------------------------------------------------------------------

/// Terminates every subprocess a team run still has running, and sweeps the rows that no abort
/// handle covers. Shared by `cancel` and by `ingest::finish`, so no ending leaves a run spending.
pub(super) async fn terminate_in_flight(state: &AppState, id: &str) -> Result<(), sqlx::Error> {
    let in_flight: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM runs WHERE team_run_id = ? AND status = 'running'")
            .bind(id)
            .fetch_all(&state.pool)
            .await?;
    for run_id in in_flight {
        // `finalize_termination` rather than the `/runs/{id}/cancel` handler beside it: that one is
        // an axum extractor signature, and reaching it would mean a loopback HTTP call to a
        // function already in scope. This is the same body that handler runs.
        crate::execution::runs::finalize_termination(state, run_id, "cancelled").await;
    }

    // And then the sweep, which is not belt-and-braces but a second case: `finalize_termination`
    // writes a status only for a run it holds an ABORT HANDLE for, and there is a window between
    // `open_run` writing the row and `spawn_registered` registering one. A row left `running` there
    // would be a run this department is recorded as still spending on, forever — nothing else
    // collects it until the next daemon restart. First writer still wins, so a row that finalised
    // itself a moment ago is untouched.
    sqlx::query(
        "UPDATE runs SET status = 'cancelled', completed_at = ?
         WHERE team_run_id = ? AND status = 'running'",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// Cancels a run and everything it has in flight.
///
/// The runs are terminated BEFORE the row is marked, so there is no window in which the run reads
/// terminal while its subprocesses are still spending. A cancelled run that left an orphan is the
/// one failure `core/AGENTS.md` §*Cancellation safety* names.
pub async fn cancel(state: &AppState, id: &str) -> Result<(), TeamError> {
    terminate_in_flight(state, id).await?;

    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE team_runs
         SET state = 'cancelled', outcome = 'cancelled', why = 'the owner cancelled it',
             director_node = 'none', updated_at = ?, finished_at = ?
         WHERE id = ? AND state IN ('planning', 'working', 'delivering')",
    )
    .bind(&now)
    .bind(&now)
    .bind(id)
    .execute(&state.pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(TeamError::NotFound);
    }
    sqlx::query("UPDATE team_items SET state = 'failed' WHERE team_run_id = ? AND state IN ('pending', 'running')")
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(())
}

/// Unwedges the runs a dead daemon left behind, at startup.
///
/// Runs AFTER `runs::reconcile_orphaned_runs`, which is what marks the abandoned subprocesses
/// `interrupted` — the same ordering, and for the same reason, that `job::reconcile_orphaned_jobs`
/// respects.
///
/// **A director node that COMPLETED is ingested rather than reset**, and that is the case worth
/// getting right: the daemon died after the CLI wrote its answer, so resetting `director_node` to
/// `none` would throw away a node that has already been paid for and run it again. The reasoning is
/// `job::reconcile_nodes`': "it records work that already happened."
pub async fn reconcile_orphaned_team_runs(state: &AppState) -> Result<(), sqlx::Error> {
    let orphaned: Vec<(String, i64)> = sqlx::query_as(
        "SELECT i.team_run_id, i.ordinal
         FROM team_items i JOIN runs r ON r.id = i.run_id
         JOIN team_runs t ON t.id = i.team_run_id
         WHERE i.state = 'running' AND r.status != 'running' AND r.status != 'completed'
           AND t.state IN ('planning', 'working', 'delivering')",
    )
    .fetch_all(&state.pool)
    .await?;
    for (team_run_id, ordinal) in orphaned {
        sqlx::query("UPDATE team_items SET state = 'failed' WHERE team_run_id = ? AND ordinal = ?")
            .bind(&team_run_id)
            .bind(ordinal)
            .execute(&state.pool)
            .await?;
    }

    // The completed-but-uningested nodes go through the very same function the tick uses. One
    // ingestion path and not two: the assertion `job.rs` bought with migration 0051 is that
    // ingesting the same node twice must not advance the round, and it only holds if there is one
    // place that can advance it.
    let live: Vec<TeamRun> = sqlx::query_as(
        "SELECT id, team_id, request, workspace, state, director_node, director_run_id, round,
                next_ordinal, dry_rounds, plan_retries, replanned, outcome, why, created_at,
                updated_at, finished_at, trigger_id, parent_id, root_id, depth
         FROM team_runs
         WHERE state IN ('planning', 'working', 'delivering') AND director_node != 'none'",
    )
    .fetch_all(&state.pool)
    .await?;
    for run in live {
        let id = run.id.clone();
        if let Err(error) = ingest_director(state, run).await {
            tracing::warn!(team_run = %id, %error, "could not reconcile a team run's director node");
        }
    }
    Ok(())
}

/// Deletes the folders of runs that ended long enough ago, by AGE and never by state.
///
/// A worktree is disposable and nobody reads it; a department's folder IS the delivery, so
/// collecting it when the run ends would delete the thing the owner was about to open.
pub async fn workspace_gc(state: &AppState, now: chrono::DateTime<chrono::Utc>) {
    let cutoff = (now - chrono::Duration::days(TEAM_WORKSPACE_RETENTION_DAYS)).to_rfc3339();
    let stale: Vec<(String, String)> = match sqlx::query_as(
        "SELECT id, workspace FROM team_runs
         WHERE finished_at IS NOT NULL AND finished_at < ?
           AND state NOT IN ('planning', 'working', 'delivering')",
    )
    .bind(&cutoff)
    .fetch_all(&state.pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "could not list the team folders due for collection");
            return;
        }
    };

    let Ok(root) = files_root(state) else {
        return;
    };
    for (id, workspace) in stale {
        let Ok(folder) = crate::files::resolve_within(&root, &workspace) else {
            continue;
        };
        if folder.exists()
            && let Err(error) = std::fs::remove_dir_all(&folder)
        {
            tracing::warn!(team_run = %id, %error, "could not collect a finished team run's folder");
        }
    }
}

pub async fn run_team_loop(state: AppState) {
    let mut interval = tokio::time::interval(TEAM_TICK);
    loop {
        interval.tick().await;
        team_tick(&state, chrono::Utc::now()).await;
    }
}

pub async fn run_workspace_gc_loop(state: AppState) {
    let mut interval = tokio::time::interval(GC_INTERVAL);
    loop {
        interval.tick().await;
        workspace_gc(&state, chrono::Utc::now()).await;
    }
}
