//! Continuing a finished run with the verdict of a verification it asked for.

use super::*;

/// Why a verify wake did not continue a run. All but `AlreadyHandled` go to the feed (`reason`).
#[derive(Debug)]
pub enum WakeRefusal {
    /// Another delivery already settled this ticket's wake: nothing to say, nothing to undo.
    AlreadyHandled,
    NotCompleted,
    KillSwitch,
    Brake(String),
    LineageCap,
    DailyCeiling,
    HandedOff,
    NoSession,
    NoWorktree,
    NoSlot(String),
    OriginGone,
    NoTask(String),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for WakeRefusal {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl WakeRefusal {
    /// The line the owner reads in the `verify_wake_refused` feed row.
    pub fn reason(&self) -> String {
        match self {
            Self::AlreadyHandled => "the verdict was already delivered".to_owned(),
            Self::NotCompleted => "the run did not finish normally".to_owned(),
            Self::KillSwitch => "the kill switch is engaged".to_owned(),
            Self::Brake(reason) => format!("the brake on new runs is on: {reason}"),
            Self::LineageCap => format!(
                "this task was already woken {} times for a verdict",
                super::super::verify_wake::WAKES_PER_LINEAGE
            ),
            Self::DailyCeiling => format!(
                "the project reached {} verdict wakes in the last day",
                super::super::verify_wake::WAKES_PER_PROJECT_PER_DAY
            ),
            Self::HandedOff => {
                "the run handed off to a successor, which is working in its tree".to_owned()
            }
            Self::NoSession => "the run recorded no session to continue".to_owned(),
            Self::NoWorktree => {
                "the run's tree is gone or already belongs to another run".to_owned()
            }
            Self::NoSlot(why) => format!("no slot to run it in: {why}"),
            Self::OriginGone => "the run's row is gone".to_owned(),
            Self::NoTask(why) => {
                format!("the task this lineage started with could not be read: {why}")
            }
            Self::Db(error) => format!("the wake could not be written: {error}"),
        }
    }
}

/// Continues a finished worktree run with the verdict of a verification it asked for, as a lineage
/// child in the same session and the same tree. Modelled on `resume_for_correction`, whose
/// transaction shape it keeps (`write_verify_wake`), with the correction's own safety rules replaced
/// by the ones a launch has: the global and the project's kill switch, and the brake on new runs
/// (budget, then quota), all read before anything is written and an unreadable switch counts as
/// engaged. The origin stays `completed`; its judge-side work was deferred when the wake opened.
pub async fn resume_for_verify(
    state: &AppState,
    origin: i64,
    ticket_id: i64,
    ticket: &serde_json::Value,
) -> Result<i64, WakeRefusal> {
    // project_id, session_id, lineage root, steerable, status.
    type OriginRow = (Option<String>, Option<String>, i64, i64, String);
    let row: Option<OriginRow> = sqlx::query_as(
        "SELECT project_id, session_id, COALESCE(lineage_root_id, id), steerable, status
         FROM runs WHERE id = ?",
    )
    .bind(origin)
    .fetch_optional(&state.pool)
    .await?;
    let Some((project_id, session_id, root, steerable, status)) = row else {
        return Err(WakeRefusal::OriginGone);
    };
    if status != "completed" {
        return Err(WakeRefusal::NotCompleted);
    }
    let project_id = project_id.ok_or(WakeRefusal::OriginGone)?;
    let session_id = session_id.ok_or(WakeRefusal::NoSession)?;

    // A launch, so the brakes of a launch; an unreadable switch counts as engaged, as the scheduler
    // reads them.
    if crate::governance::autopilot::kill_switch_engaged(&state.pool)
        .await
        .unwrap_or(true)
        || crate::governance::autopilot::scoped_kill_engaged(&state.pool, "project", &project_id)
            .await
            .unwrap_or(true)
    {
        return Err(WakeRefusal::KillSwitch);
    }
    if let crate::governance::budget::BudgetDecision::Pause { reason, .. } =
        crate::quota::permits_new_run(state, chrono::Utc::now()).await
    {
        return Err(WakeRefusal::Brake(reason));
    }

    let (project_root, wt_path, branch): (String, String, String) = sqlx::query_as(
        "SELECT project_root, path, branch FROM worktrees
         WHERE owner_kind = 'run' AND owner_id = ? AND removed_at IS NULL",
    )
    .bind(origin)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(WakeRefusal::NoWorktree)?;
    // Outside the transaction, for `resume_for_correction`'s reason: `original_task` walks the runs
    // table with its own connection. Unreadable or empty is a refusal, never an empty brief.
    let task = original_task(&state.pool, origin)
        .await
        .map_err(|error| WakeRefusal::NoTask(error.to_string()))?;
    if task.trim().is_empty() {
        return Err(WakeRefusal::NoTask("it is empty".to_owned()));
    }
    let prompt = super::super::verify_wake::verdict_prompt(ticket_id, ticket, &task);
    let prompt_for_route = prompt.clone();
    let draft = VerifyWakeDraft {
        origin,
        root,
        ticket_id,
        project_id: &project_id,
        wt_path: &wt_path,
        prompt: &prompt,
        now: chrono::Utc::now(),
    };
    let wake = write_verify_wake(&state.pool, &draft).await?;

    let daemon_token = mint_run_token(&state.pool, wake).await;
    let rules =
        crate::config::load_schedule_rules(state.machine_config_root.as_deref(), &project_id);
    let max_turns = rules.as_ref().ok().and_then(|rules| rules.max_turns);
    let gate_config = match rules {
        Ok(rules) => {
            let scoped_final = rules.scoped_final();
            rules
                .gate_command
                .map_or(GateConfig::NotConfigured, |command| GateConfig::Command {
                    command,
                    project_root: project_root.clone(),
                    scoped_final,
                })
        }
        Err(error) => GateConfig::Unreadable(format!("gate configuration is unreadable: {error}")),
    };
    let governed_by_classifier = classifier_governs_tools(
        "worktree",
        crate::execution::runner::ToolPolicy::Unrestricted,
        Some(std::path::Path::new(&wt_path)),
    );
    spawn_run(
        state,
        state.runner.clone(),
        wake,
        prompt,
        Some(project_id),
        Some(std::path::PathBuf::from(&wt_path)),
        if governed_by_classifier {
            crate::execution::runner::Permission::Bypass
        } else {
            crate::execution::runner::Permission::Default
        },
        Some(session_id.clone()),
        session_id,
        false,
        Some((
            "worktree_run_completed".to_owned(),
            format!("verify wake run completed on {branch}"),
        )),
        gate_config,
        1,
        crate::execution::runner::ToolPolicy::Unrestricted,
        run_env(&daemon_token, wake, None, crate::speed::Capacity::solo()),
        steerable != 0,
        run_timeout_for_mode(state.run_timeout, "worktree"),
        progress_timeout_for_mode(state.progress_timeout, "worktree"),
        governed_by_classifier,
        state.runner.model_for_stage(None),
        None,
        // The same session continuing, so it is routed as a resume (`codex exec resume` for a Codex
        // origin): only the runner that holds the session is eligible and the advice is recorded.
        Some(crate::models::route_advice::RouteQuery {
            task: prompt_for_route,
            stage: None,
            item: None,
            resume: true,
            restrict_runners: None,
            pin: None,
            judged_later: false,
            effort: None,
            codex_profile: None,
        }),
        None,
        max_turns,
    );
    Ok(wake)
}

/// Re-arms the watcher of every verify wake a previous daemon left `pending`. Public because the
/// binary's startup is a different crate from the `verify_wake` module it belongs to.
pub async fn rearm_verify_wakes(state: &AppState) {
    super::super::verify_wake::rearm(state).await;
}

/// What `write_verify_wake` writes, read and built before its transaction opens.
pub(super) struct VerifyWakeDraft<'a> {
    pub(super) origin: i64,
    pub(super) root: i64,
    pub(super) ticket_id: i64,
    pub(super) project_id: &'a str,
    pub(super) wt_path: &'a str,
    pub(super) prompt: &'a str,
    pub(super) now: chrono::DateTime<chrono::Utc>,
}

/// The one transaction of `resume_for_verify`, in the order of `write_correction`:
/// 1. the wake row is the FIRST write, so from here the write lock is this delivery's and a second
///    delivery of the same ticket waits, then reads `delivered` and stands down;
/// 2. the lineage's and the project's ceilings, counted inside the lock;
/// 3. the child by `INSERT ... SELECT` over the origin, its slot handed over (or claimed anew when
///    the sweep already freed it), the tree's owner moved;
/// 4. the wake marked `delivered`.
///
/// Any refusal drops the transaction, so the row of step 1 is rolled back with the rest.
pub(super) async fn write_verify_wake(
    pool: &sqlx::SqlitePool,
    draft: &VerifyWakeDraft<'_>,
) -> Result<i64, WakeRefusal> {
    let (origin, root, now_text) = (draft.origin, draft.root, draft.now.to_rfc3339());
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO verify_wakes (ticket_id, run_id, root_run_id, state, created_at)
         VALUES (?, ?, ?, 'pending', ?)
         ON CONFLICT(ticket_id) DO NOTHING",
    )
    .bind(draft.ticket_id)
    .bind(origin)
    .bind(root)
    .bind(&now_text)
    .execute(&mut *tx)
    .await?;
    let wake_state: String =
        sqlx::query_scalar("SELECT state FROM verify_wakes WHERE ticket_id = ?")
            .bind(draft.ticket_id)
            .fetch_one(&mut *tx)
            .await?;
    if wake_state != "pending" {
        return Err(WakeRefusal::AlreadyHandled);
    }
    let successor: Option<Option<i64>> =
        sqlx::query_scalar("SELECT successor_run_id FROM runs WHERE id = ?")
            .bind(origin)
            .fetch_optional(&mut *tx)
            .await?;
    match successor {
        None => return Err(WakeRefusal::OriginGone),
        Some(Some(_)) => return Err(WakeRefusal::HandedOff),
        Some(None) => {}
    }
    let in_lineage: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM verify_wakes WHERE root_run_id = ? AND state = 'delivered'",
    )
    .bind(root)
    .fetch_one(&mut *tx)
    .await?;
    if in_lineage >= super::super::verify_wake::WAKES_PER_LINEAGE {
        return Err(WakeRefusal::LineageCap);
    }
    let since = (draft.now - chrono::Duration::days(1)).to_rfc3339();
    let in_project: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM verify_wakes w JOIN runs r ON r.id = w.run_id
         WHERE r.project_id = ? AND w.state = 'delivered' AND w.created_at >= ?",
    )
    .bind(draft.project_id)
    .bind(&since)
    .fetch_one(&mut *tx)
    .await?;
    if in_project >= super::super::verify_wake::WAKES_PER_PROJECT_PER_DAY {
        return Err(WakeRefusal::DailyCeiling);
    }
    let inserted = sqlx::query(CORRECTION_INSERT.as_str())
        .bind(draft.wt_path)
        .bind(draft.prompt)
        .bind(&now_text)
        .bind(origin)
        .execute(&mut *tx)
        .await?;
    if inserted.rows_affected() != 1 {
        return Err(WakeRefusal::OriginGone);
    }
    let child = inserted.last_insert_rowid();
    let handed = sqlx::query(
        "UPDATE project_slots SET owner_id = ? WHERE owner_kind = 'run' AND owner_id = ?",
    )
    .bind(child)
    .bind(origin)
    .execute(&mut *tx)
    .await?;
    if handed.rows_affected() == 0 {
        match crate::execution::concurrency::claim_on(
            &mut tx,
            draft.project_id,
            crate::execution::worktree::Owner::Run(child),
        )
        .await?
        {
            crate::execution::concurrency::ClaimOutcome::Claimed(_) => {}
            crate::execution::concurrency::ClaimOutcome::Full(no_room) => {
                return Err(WakeRefusal::NoSlot(no_room.reason()));
            }
        }
    }
    let moved = sqlx::query(
        "UPDATE worktrees SET owner_id = ?
         WHERE owner_kind = 'run' AND owner_id = ? AND removed_at IS NULL",
    )
    .bind(child)
    .bind(origin)
    .execute(&mut *tx)
    .await?;
    if moved.rows_affected() != 1 {
        return Err(WakeRefusal::NoWorktree);
    }
    sqlx::query("UPDATE verify_wakes SET state = 'delivered' WHERE ticket_id = ?")
        .bind(draft.ticket_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(child)
}
