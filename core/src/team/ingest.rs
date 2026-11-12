//! Team result ingestion and run finalization.

use super::*;

// ---------------------------------------------------------------------------------------------
// Ingesting what landed
// ---------------------------------------------------------------------------------------------

/// Specialist runs that have finished since the last pass: file the answer, mark the item.
///
/// A failed item leaves no file and does not stop the run — there is no shared worktree here
/// accumulating damage, so what a failure costs is one absence the director sees in the next
/// round's index.
pub(super) async fn ingest_landed_items(
    state: &AppState,
    run: &TeamRun,
) -> Result<(), sqlx::Error> {
    let landed: Vec<(i64, String, i64, String)> = sqlx::query_as(
        "SELECT i.ordinal, i.agent_id, r.id, r.status
         FROM team_items i JOIN runs r ON r.id = i.run_id
         WHERE i.team_run_id = ? AND i.state = 'running' AND r.status != 'running'",
    )
    .bind(&run.id)
    .fetch_all(&state.pool)
    .await?;

    for (ordinal, agent_id, run_id, status) in landed {
        if status != "completed" {
            sqlx::query(
                "UPDATE team_items SET state = 'failed' WHERE team_run_id = ? AND ordinal = ?",
            )
            .bind(&run.id)
            .bind(ordinal)
            .execute(&state.pool)
            .await?;
            crate::models::seat_advice::report_team_item(
                &state.pool,
                state.runner.router(),
                run_id,
            )
            .await;
            continue;
        }

        let answer: Option<String> = sqlx::query_scalar("SELECT stdout FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(&state.pool)
            .await?
            .flatten();
        let answer = crate::execution::runner::extract_reply(answer.as_deref().unwrap_or_default())
            .unwrap_or_else(|| answer.unwrap_or_default());

        // The core names the file, which is what removes collisions by construction: the ordinal is
        // unique across the whole run, so two specialists — in one round or across rounds — can
        // never choose the same name, because neither of them chooses.
        let name = format!("{ordinal}-{agent_id}.md");
        match write_into_workspace(state, run, &name, &answer) {
            Ok(()) => {
                sqlx::query(
                    "UPDATE team_items SET state = 'done', output_path = ?
                     WHERE team_run_id = ? AND ordinal = ?",
                )
                .bind(&name)
                .bind(&run.id)
                .bind(ordinal)
                .execute(&state.pool)
                .await?;
            }
            Err(error) => {
                tracing::warn!(team_run = %run.id, ordinal, %error, "could not file a specialist's answer");
                sqlx::query(
                    "UPDATE team_items SET state = 'failed' WHERE team_run_id = ? AND ordinal = ?",
                )
                .bind(&run.id)
                .bind(ordinal)
                .execute(&state.pool)
                .await?;
            }
        }
        // The seat is reported on how its RUN ended, after the item's own state is written — so a
        // pass that could not write it retries next tick and reports once, not twice. A completed
        // run whose answer could not be filed is still `pass`: the model did the work, the daemon
        // is what failed to keep it.
        crate::models::seat_advice::report_team_item(&state.pool, state.runner.router(), run_id)
            .await;
    }
    Ok(())
}

fn write_into_workspace(
    state: &AppState,
    run: &TeamRun,
    name: &str,
    body: &str,
) -> std::io::Result<()> {
    let root = files_root(state).map_err(std::io::Error::other)?;
    let folder = crate::files::resolve_within(&root, &run.workspace)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    std::fs::create_dir_all(&folder)?;
    let target = crate::files::resolve_within(&folder, name)
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    crate::storage::write_atomic(&target, body.as_bytes())
}

/// Ingests the director node in flight, if it has landed.
///
/// The marker is claimed by a compare-and-swap before anything is written, so ingesting the same
/// `director_run_id` twice cannot advance the round twice — and neither call site cares which of
/// the two happened, which is why this reports nothing.
pub(super) async fn ingest_director(state: &AppState, run: TeamRun) -> Result<(), sqlx::Error> {
    let Some(director_run_id) = run.director_run_id else {
        // A marker with no run behind it: nothing can land, so clear it and let the next pass
        // relaunch rather than leave the run wedged forever.
        return clear_director_node(state, &run).await;
    };

    let landed: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT status, stdout FROM runs WHERE id = ?")
            .bind(director_run_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some((status, stdout)) = landed else {
        return clear_director_node(state, &run).await;
    };
    if status == "running" {
        return Ok(());
    }

    let node = run.director_node.clone();
    if status != "completed" {
        // Without a director there is nobody to replan, so the run ends with whatever is in the
        // folder. `delivering` is the one node whose failure still leaves the specialists' files.
        return finish(
            state,
            &run,
            "failed",
            &format!("the director's {node} node {status}"),
        )
        .await;
    }

    let answer = stdout.unwrap_or_default();
    let answer = crate::execution::runner::extract_reply(&answer).unwrap_or(answer);

    if node == "delivering" {
        // A delivery that cannot be written ends the run: returning the error instead would repeat
        // it on every tick, with the run counting as live (and un-deletable) forever.
        if let Err(error) = write_into_workspace(state, &run, DELIVERY_FILE, &answer) {
            return finish(
                state,
                &run,
                "failed",
                &format!("the delivery could not be written: {error}"),
            )
            .await;
        }
        return finish(state, &run, "done", "the department delivered").await;
    }

    let team_roster = roster(&state.pool, &run.team_id).await.unwrap_or_default();
    let plan = parse_team_plan(&answer, &team_roster, MAX_ITEMS_PER_ROUND);
    for dropped in plan.iter().flat_map(|plan| plan.dropped.iter()) {
        let _ = crate::feed::append(
            &state.pool,
            None,
            "team_item_dropped",
            dropped,
            None,
            Some(&crate::feed::Subject::TeamRun(run.id.clone())),
        )
        .await;
    }

    let max_rounds: i64 = sqlx::query_scalar("SELECT max_rounds FROM teams WHERE id = ?")
        .bind(&run.team_id)
        .fetch_one(&state.pool)
        .await?;
    let progress = next_after_director(
        &run.state,
        plan.as_ref(),
        run.round,
        run.dry_rounds,
        run.plan_retries,
        max_rounds,
    );

    if progress.state == "failed" {
        return finish(
            state,
            &run,
            "failed",
            progress.why.as_deref().unwrap_or("the director failed"),
        )
        .await;
    }

    // **One transaction, and the CLAIM comes first.** Guarding only the final `UPDATE` was not
    // enough and failed loudly the first time it was tested: the items are written before it, so a
    // second ingestion of the same node inserted the same ordinals again and died on the primary
    // key — with the first copy already committed. The compare-and-swap below is `job::spawn_node`'s
    // shape: zero rows affected means another caller has already taken this node, and there is
    // nothing left to do. Both halves inside one transaction, so a crash between them cannot leave
    // a claimed node whose items were never queued.
    let mut transaction = state.pool.begin().await?;

    let claimed = sqlx::query(
        "UPDATE team_runs SET director_node = 'none', director_run_id = NULL, updated_at = ?
         WHERE id = ? AND director_node = ? AND director_run_id = ?",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(&run.id)
    .bind(&node)
    .bind(director_run_id)
    .execute(&mut *transaction)
    .await?;
    if claimed.rows_affected() == 0 {
        transaction.rollback().await?;
        return Ok(());
    }

    let mut next_ordinal = run.next_ordinal;
    if progress.queue_items
        && let Some(plan) = plan.as_ref()
    {
        for item in &plan.items {
            sqlx::query(
                "INSERT INTO team_items (team_run_id, ordinal, round, agent_id, description, state)
                 VALUES (?, ?, ?, ?, ?, 'pending')",
            )
            .bind(&run.id)
            .bind(next_ordinal)
            .bind(progress.round)
            .bind(&item.agent_id)
            .bind(&item.description)
            .execute(&mut *transaction)
            .await?;
            next_ordinal += 1;
        }
    }

    sqlx::query(
        "UPDATE team_runs
         SET state = ?, round = ?, next_ordinal = ?, dry_rounds = ?, plan_retries = ?,
             replanned = ?, why = ?, updated_at = ?
         WHERE id = ?",
    )
    .bind(progress.state)
    .bind(progress.round)
    .bind(next_ordinal)
    .bind(progress.dry_rounds)
    .bind(progress.plan_retries)
    .bind(if node == "replanning" {
        "done"
    } else {
        &run.replanned
    })
    .bind(&progress.why)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(&run.id)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;
    Ok(())
}

async fn clear_director_node(state: &AppState, run: &TeamRun) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE team_runs SET director_node = 'none', director_run_id = NULL, updated_at = ?
         WHERE id = ?",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(&run.id)
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// Ends a run, once. The `state IN (live)` guard is what makes a second call a no-op rather than a
/// way to overwrite how a run ended.
pub(super) async fn finish(
    state: &AppState,
    run: &TeamRun,
    ending: &str,
    why: &str,
) -> Result<(), sqlx::Error> {
    // The one place a run's ending is written, so the one place worth holding to the vocabulary.
    // `ending` is a `&str`, and a typo would store a state `is_live` reads as not-live, the GC
    // reads as collectable and nothing at all recognises — a run that has quietly stopped existing
    // to every reader while still sitting in the table.
    debug_assert!(
        TERMINAL_STATES.contains(&ending),
        "{ending} is not one of the five ways a team run ends"
    );
    // Kill what is in flight BEFORE the row reads terminal (see `cancel`): a run that ends with
    // specialists still running would keep spending on a department nobody is waiting on.
    super::lifecycle::terminate_in_flight(state, &run.id).await?;
    let now = chrono::Utc::now().to_rfc3339();
    let ended = sqlx::query(
        "UPDATE team_runs
         SET state = ?, outcome = ?, why = ?, director_node = 'none', updated_at = ?,
             finished_at = ?
         WHERE id = ? AND state IN ('planning', 'working', 'delivering')",
    )
    .bind(ending)
    .bind(ending)
    .bind(why)
    .bind(&now)
    .bind(&now)
    .bind(&run.id)
    .execute(&state.pool)
    .await?;
    if ended.rows_affected() > 0 {
        sqlx::query(
            "UPDATE team_items SET state = 'failed'
             WHERE team_run_id = ? AND state IN ('pending', 'running')",
        )
        .bind(&run.id)
        .execute(&state.pool)
        .await?;
    }

    let _ = crate::feed::append(
        &state.pool,
        None,
        "team_run_finished",
        &format!("a team run {ending}: {why}"),
        None,
        Some(&crate::feed::Subject::TeamRun(run.id.clone())),
    )
    .await;
    Ok(())
}
