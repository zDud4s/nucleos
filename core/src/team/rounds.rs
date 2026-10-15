//! Team tick and round-advancement mechanics.

use super::*;

// ---------------------------------------------------------------------------------------------
// The tick
// ---------------------------------------------------------------------------------------------

/// Advances every live run as far as it will go, once.
///
/// Runs are advanced independently and a failure in one is logged rather than propagated: a
/// department that cannot be moved must not stop the sweep that moves the others.
pub async fn team_tick(state: &AppState, now: chrono::DateTime<chrono::Utc>) {
    // Before the runs, and independently of them. An action outlives the run that asked for it —
    // people decide overnight, and by morning that department has usually finished — so this loop
    // reads `team_actions` rather than walking the live runs. It is also why a pending action never
    // holds a run open: nothing here is waiting for anything there.
    execute_due_actions(state).await;

    let live: Vec<TeamRun> = match sqlx::query_as(
        "SELECT id, team_id, request, workspace, state, director_node, director_run_id, round,
                next_ordinal, dry_rounds, plan_retries, replanned, outcome, why, created_at,
                updated_at, finished_at, trigger_id, parent_id, root_id, depth
         FROM team_runs
         WHERE state IN ('planning', 'working', 'delivering')
         ORDER BY created_at",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "could not read the live team runs");
            return;
        }
    };

    for run in live {
        let id = run.id.clone();
        if let Err(error) = advance(state, run, now).await {
            tracing::warn!(team_run = %id, %error, "a team run could not be advanced this pass");
        }
    }
}

/// One run, one pass.
pub(super) async fn advance(
    state: &AppState,
    run: TeamRun,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    // Whatever is in flight is ingested first, so a ceiling reached while a node was running does
    // not throw away a node that has already been paid for. It is the reasoning `job::reconcile_nodes`
    // states: refusing to write down a finished node because the budget ran out loses the node and
    // repeats it.
    if run.director_node != "none" {
        // One step per pass, whichever way that went: a node still in flight means nothing else can
        // happen, and a node just ingested means the row this pass is holding is already stale.
        // What comes next is decided from the row the next tick reads, ten seconds later, which is
        // the cost of never acting on a snapshot that has moved underneath.
        ingest_director(state, run).await?;
        return Ok(());
    }
    ingest_landed_items(state, &run).await?;

    // Ceilings, at the two points the design names: before a round and before delivery. Both are
    // reached here, because `director_node == none` is exactly "between nodes".
    if let Some((ending, why)) = ceiling_reached(state, &run, now).await {
        return finish(state, &run, ending, &why).await;
    }

    match run.state.as_str() {
        "planning" => launch_director(state, &run, DirectorNode::Planning).await,
        "delivering" => launch_director(state, &run, DirectorNode::Delivering).await,
        "working" => advance_round(state, &run).await,
        _ => Ok(()),
    }
}

/// Which ceiling, if any, this run has hit.
///
/// Time before money, because an expired run tells the owner something a stopped one does not: the
/// department is not slow, it has been going for four hours.
async fn ceiling_reached(
    state: &AppState,
    run: &TeamRun,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(&'static str, String)> {
    if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&run.created_at)
        && now.signed_duration_since(created.with_timezone(&chrono::Utc)) > MAX_TEAM_RUN_LIFETIME
    {
        return Some((
            "expired",
            format!(
                "the run passed its ceiling of {} hours",
                MAX_TEAM_RUN_LIFETIME.num_hours()
            ),
        ));
    }

    if let crate::governance::budget::BudgetDecision::Pause { reason, .. } =
        crate::governance::budget::budget_permits_new_run(&state.pool, now).await
    {
        return Some(("stopped", reason));
    }

    // `Option<f64>` spelled out, and the two `flatten`s are not noise: the outer one is "is there a
    // team row", the inner is "did that row name a ceiling". Decoding straight into `f64` read a
    // NULL as 0.00, so a team with NO ceiling was stopped before its first round with the message
    // "the team's ceiling of $0.00 is spent" — the whole pillar dead on arrival for every team the
    // owner did not put a number on, which is the default.
    let ceiling: Option<f64> =
        sqlx::query_scalar::<_, Option<f64>>("SELECT budget_usd FROM teams WHERE id = ?")
            .bind(&run.team_id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
            .flatten();
    if let Some(ceiling) = ceiling {
        let spent = spend_of(&state.pool, &run.id).await;
        if spent >= ceiling {
            return Some((
                "stopped",
                format!("the team's ceiling of ${ceiling:.2} is spent (${spent:.2})"),
            ));
        }
    }

    // And the TREE's, which is a different question once a run can start another. Two teams with a
    // $5 ceiling that start each other spend $5 every time, for ever, and every run is inside its
    // own ceiling the whole time — so the check above never fires and nothing is locally wrong.
    //
    // Both checks stand, and neither weakens the other: a $1 department does not get to spend $4
    // because the root allowed it, and a chain does not get to spend without end because each link
    // is cheap. For a run a person asked for the two are the same question, since it is its own
    // root and its own tree.
    if run.root_id != run.id {
        let root_ceiling: Option<f64> = sqlx::query_scalar::<_, Option<f64>>(
            "SELECT teams.budget_usd FROM team_runs
               JOIN teams ON teams.id = team_runs.team_id
              WHERE team_runs.id = ?",
        )
        .bind(&run.root_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .flatten();
        if let Some(root_ceiling) = root_ceiling {
            let spent = spend_of_tree(&state.pool, &run.root_id).await;
            if spent >= root_ceiling {
                return Some((
                    "stopped",
                    format!(
                        "this chain has spent ${spent:.2} of the ${root_ceiling:.2} its first run \
                         was given"
                    ),
                ));
            }
        }
    }
    None
}

/// What a whole chain has cost, every run in it and every director node of each.
///
/// Keyed on `root_id` with an index, not a recursive walk of `parent_id`: this is asked at every
/// pass of every run in the tree, one indexed read beats a CTE that arrives at the same answer by a
/// route that can be got wrong. It rests on `runs.team_run_id` (migration 0084) — without that
/// column a run's cost counts the specialists alone, and a tree ceiling built on a sum that
/// undercounts errs towards spending, multiplied by the depth.
pub async fn spend_of_tree(pool: &sqlx::SqlitePool, root_id: &str) -> f64 {
    sqlx::query_scalar::<_, Option<f64>>(
        "SELECT SUM(cost_usd) FROM runs
          WHERE team_run_id IN (SELECT id FROM team_runs WHERE root_id = ?)",
    )
    .bind(root_id)
    .fetch_one(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(0.0)
}

/// What one run has cost, director nodes included.
///
/// Reads `runs.team_run_id` and not `team_items.run_id`, and that is why the column exists: the
/// director's plan, replans and delivery are runs of this department that no item points at.
pub async fn spend_of(pool: &sqlx::SqlitePool, team_run_id: &str) -> f64 {
    sqlx::query_scalar::<_, Option<f64>>("SELECT SUM(cost_usd) FROM runs WHERE team_run_id = ?")
        .bind(team_run_id)
        .fetch_one(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(0.0)
}

/// How many specialists a round may have running at once: the team's value, widened by this
/// run's speed, never past `MAX_PARALLEL_CEILING` (speed spec, section 5.1). The column is
/// validated on the way in, but a value written around that validation is exactly what a
/// ceiling exists for, so it is applied here, at the one place that opens the fan.
pub(super) async fn round_width(state: &AppState, run: &TeamRun) -> Result<i64, sqlx::Error> {
    Ok(run_capacity(state, run).await?.parallel_ceiling)
}

/// This run's speed and the width it opens, as one value: `round_width` enforces the width, and
/// `spawn_agent` hands both to the session it launches. One reader, so the number a session is
/// told and the number the fan is held to cannot come from two different queries.
pub(super) async fn run_capacity(
    state: &AppState,
    run: &TeamRun,
) -> Result<crate::speed::Capacity, sqlx::Error> {
    let (stored, speed): (i64, Option<String>) = sqlx::query_as(
        "SELECT t.max_parallel, r.speed FROM teams t JOIN team_runs r ON r.team_id = t.id
         WHERE r.id = ?",
    )
    .bind(&run.id)
    .fetch_one(&state.pool)
    .await?;
    let speed = crate::speed::Speed::from_column(speed.as_deref());
    let (_, degraded) = crate::speed::team_width(speed, stored);
    if let Some(reason) = degraded {
        tracing::info!(team_run = %run.id, reason, "the speed asked for more width than this team allows");
    }
    Ok(crate::speed::Capacity::team(speed, stored))
}

/// A round: start what is pending, up to the team's parallelism, and replan when it is empty.
async fn advance_round(state: &AppState, run: &TeamRun) -> Result<(), sqlx::Error> {
    let items: Vec<TeamItem> = sqlx::query_as(
        "SELECT ordinal, round, agent_id, description, state, run_id, output_path
         FROM team_items WHERE team_run_id = ? AND round = ? ORDER BY ordinal",
    )
    .bind(&run.id)
    .bind(run.round)
    .fetch_all(&state.pool)
    .await?;

    let running = items.iter().filter(|item| item.state == "running").count() as i64;
    let pending: Vec<&TeamItem> = items
        .iter()
        .filter(|item| item.state == "pending")
        .collect();

    if pending.is_empty() && running == 0 {
        // The round is over. Replanning is a director node like any other, and `director_node`
        // is what stops the next tick launching a second one.
        return launch_director(state, run, DirectorNode::Replanning).await;
    }

    let parallel = round_width(state, run).await?;

    for item in pending
        .into_iter()
        .take((parallel - running).max(0) as usize)
    {
        if let Err(error) = launch_specialist(state, run, item).await {
            tracing::warn!(
                team_run = %run.id,
                ordinal = item.ordinal,
                %error,
                "a specialist could not be launched; the item is marked failed and the run goes on"
            );
            sqlx::query(
                "UPDATE team_items SET state = 'failed' WHERE team_run_id = ? AND ordinal = ?",
            )
            .bind(&run.id)
            .bind(item.ordinal)
            .execute(&state.pool)
            .await?;
        }
    }
    Ok(())
}
