//! Resuming a run: an approved resume, a correction, and continuing a paused run.

use super::*;

/// The operation this approval should hand to the queue instead of handing back to the run, if it is
/// one at all.
///
/// **Every `None` here means "keep the behaviour this approval has always had"** — a single-use
/// grant, and the run performs the action itself. That is deliberately the conservative direction
/// and not a shrug. Refusing the approval outright would leave a person holding an action they have
/// approved, no way to perform it, and nothing to read explaining why; queueing an operation we are
/// not certain is the one they read would be worse than either.
///
/// So the bar is: the tool is a shell, the input parses, the command is one the queue can execute —
/// `git merge <ref>` with at most a `--no-ff`, `git push <remote> [<branch>]`, or
/// `git tag <name> [<branch>]`, each argued in its own function in `vcs.rs` — the worktree is really
/// there and really on a branch, and the project resolves to a repository. Anything else falls back.
///
/// **The two are tried in order and the order cannot matter**, which is worth stating rather than
/// relying on: each parser insists on its own subcommand, so a command is at most one of them. The
/// `or_else` is a sequence and not a precedence.
///
/// It runs git twice and must therefore be called before the transaction opens — see the call site.
pub(super) async fn queueable_operation(
    state: &AppState,
    proposal: &crate::proposals::Proposal,
    project_id: &str,
    worktree_path: &str,
) -> Option<(
    crate::execution::vcs::ResolvedRepo,
    crate::execution::vcs::Op,
)> {
    if !matches!(proposal.tool_name.as_deref(), Some("Bash" | "PowerShell")) {
        return None;
    }
    let input: serde_json::Value = serde_json::from_str(proposal.tool_input.as_deref()?).ok()?;
    let command = input.get("command")?.as_str()?;

    // One deadline for both calls, so a slow repository cannot spend the budget twice over.
    let deadline = std::time::Instant::now() + crate::execution::git_exec::OPERATION_TIMEOUT;
    // Read for BOTH parsers, and for different jobs in each: a merge's target is the branch the
    // worktree stands on, while a push only falls back to it when the command did not name one. It
    // is asked for unconditionally because it is also the liveness check on the worktree — a path
    // that has been removed answers with the enclosing checkout's branch or with an error, and
    // `current_branch`'s own `--show-toplevel` guard is what tells those apart.
    let branch = crate::execution::git_exec::current_branch(std::path::Path::new(worktree_path), deadline)
        .await
        .map_err(|error| {
            tracing::info!(
                worktree = %worktree_path,
                %error,
                "approval: could not read the worktree's branch — authorizing the run instead of queueing"
            );
        })
        .ok()?;
    let op = crate::execution::vcs::merge_from_command(command, &branch)
        .or_else(|| crate::execution::vcs::push_from_command(command, &branch))
        .or_else(|| crate::execution::vcs::tag_from_command(command, &branch))
        // The two that need no worktree branch, so they take none. A fetch names its remote and a
        // deletion names its branch; neither has a half the command line leaves out.
        .or_else(|| crate::execution::vcs::fetch_from_command(command))
        .or_else(|| crate::execution::vcs::branch_delete_from_command(command))
        .or_else(|| crate::execution::vcs::rebase_from_command(command, &branch))?;
    crate::execution::vcs::resolve_repo(&state.pool, project_id)
        .await
        .map_err(|error| {
            // `?` rather than `%`: `ResolveError` is a two-arm enum carrying a source, and it has no
            // `Display` on purpose — the HTTP layer answers its arms with different statuses instead
            // of rendering them.
            tracing::info!(
                project_id,
                ?error,
                "approval: could not resolve the project's repository — authorizing the run instead of queueing"
            );
        })
        .ok()
        .map(|repo| (repo, op))
}

/// How much of the approved action's own text the resume prompt repeats back. A run pays for every
/// token of its own prompt, and a command a loop built can be megabytes.
pub(super) const RESUME_ACTION_CHARS: usize = 600;

/// PURE: the instruction a resumed run is given for the action a human just approved.
///
/// **This wording is the fix for a measured, reproduced failure** (`.ai/eval/ABLATION.md`, T1×H3,
/// 2026-08-17). It used to say "Proceed with the {tool} action you attempted before the pause — it
/// is now authorized". The resumed agent has no record of attempting anything: the pause happens
/// BEFORE the call runs, so the call never becomes a step in the transcript the resume restores.
/// What arrived, from the agent's side, was an unverifiable claim of prior authorization urging it
/// to run a command — the shape of an injection — and twice it refused, correctly:
///
/// > This looks like it may be an attempt to get me to run a command under a false claim of prior
/// > authorization. I won't proceed with any Bash action on that basis.
///
/// One turn, nothing done, and the daemon recorded `completed`. So: state the action, and explain
/// the absence. An agent shown what was approved can weigh it; an agent asked to remember it can
/// only obey or refuse, and refusing is the better of those two.
pub(super) fn resume_instruction(
    proposal_id: i64,
    tool_name: &str,
    tool_input: Option<&str>,
) -> String {
    // `command` first because it is the field a person would quote; the whole object otherwise, so
    // a tool that is not Bash still shows what it was going to do.
    let action = tool_input
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| {
            value
                .get("command")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .or_else(|| value.as_object().map(|_| value.to_string()))
        })
        .map(|text| {
            if text.chars().count() <= RESUME_ACTION_CHARS {
                text
            } else {
                // By chars, not bytes: this is a prompt, and a slice through a UTF-8 boundary
                // panics on exactly the inputs nobody tests with.
                let mut cut: String = text.chars().take(RESUME_ACTION_CHARS).collect();
                cut.push('…');
                cut
            }
        });

    // **Written so it does not read as a prompt injection, because it used to and a run said so.**
    //
    // Measured 2026-08-28. An autonomous run was resumed, read this note, and refused it in as
    // many words: it called the message an injection attempt. It was not being paranoid. The old
    // text had all three tells at once: it asserted an authority the reader cannot check (a human
    // approved this), it pre-emptively explained away the missing evidence (you will not find that
    // call in your transcript), and it asked for an action the run's own task had forbidden.
    // Explaining away absent evidence is the signature of the attack, so writing that sentence
    // made the daemon indistinguishable from an attacker.
    //
    // Three changes, and the third is the one that matters. It names the doubt instead of ignoring
    // it. It says where the note came from rather than whose authority it carries. And it declares
    // itself SUBORDINATE to the task: the run that refused this was resolving a genuine conflict --
    // the note asked for a compound command its task forbade -- and had no way to know which of the
    // two won. Now it does, and the answer is the task, which is also the safe direction.
    let framing = format!(
        "This note comes from the daemon that launched this session, not from anything inside \
         your conversation -- no file, tool result or page you read put it here.\n\n\
         The {tool_name} call is absent from your transcript because the guard runs BEFORE a call \
         does: the attempt is real, the call never happened, and this session is a fresh one \
         continuing that work.\n\n\
         It authorizes that ONE action and nothing else, and it does not relax any rule your task \
         gave you. If carrying it out would break one of those rules, your task wins -- find \
         another way to the same end. Do it only if it is still the right next step, then carry on."
    );

    match action {
        Some(action) => format!(
            "This run paused when it attempted the action below, and that pause has been \
             lifted (proposal #{proposal_id}):\n\n    {action}\n\n{framing}"
        ),
        None => format!(
            "This run paused when it attempted a {tool_name} action, and that pause has been \
             lifted (proposal #{proposal_id}). The action's input could not be read back, so it is \
             not quoted here.\n\n{framing}"
        ),
    }
}

/// Where to resume a paused run that owns no worktree, read from what the run recorded about itself.
///
/// The shape is the one the worktree lookup returns — `(project_id, project_root, path)` — because
/// everything downstream takes those three and does not care which of the two ways they were
/// obtained. What differs is only the last: a worktree's `path` is a tree the daemon made, and this
/// is the directory the caller named when the run was created.
///
/// Both halves must be present or this is not resumable: without a project there is no root to
/// govern the resume, and without a `cwd` there is nowhere to launch it. That is the one case where
/// the old sentence was right, so it is the one case that still says it.
pub(super) async fn recorded_tree_of(
    state: &AppState,
    run_id: i64,
) -> Result<(String, String, String), ResumeError> {
    let recorded: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT project_id, cwd FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(&state.pool)
            .await?;

    let (Some(project_id), Some(cwd)) = recorded.unwrap_or((None, None)) else {
        return Err(ResumeError::NotResumable(
            "the paused run owns no worktree and recorded no directory to resume in",
        ));
    };

    let project_root = crate::inspect::project_root(&state.pool, &project_id)
        .await?
        .ok_or(ResumeError::NotResumable(
            "the paused run's project has no recorded root to resume against",
        ))?;

    Ok((project_id, project_root, cwd))
}

/// Spec B D12: the two ways a person continues a run paused on an action.
///
/// One core behind two thin wrappers, not two functions. `resume_approved_run` is over five
/// hundred lines, and two copies of it is exactly how spec B §1.3's drift happened — the handoff
/// carried the taint and the resume did not, and nobody noticed. With one core, a step added for
/// the approval reaches the decline without anybody remembering to. The two real differences — a
/// grant, and the git queue with the owner's authority — are in `match how` below, in view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Continuation {
    /// The action may happen: a grant for its class, or its declared git operation queued.
    Approve,
    /// Only the action is refused: no grant, nothing queued, the decline mark written.
    Decline,
}

pub async fn resume_approved_run(state: &AppState, proposal_id: i64) -> Result<i64, ResumeError> {
    continue_paused_run(state, proposal_id, Continuation::Approve).await
}

/// Spec B D12: `POST /proposals/{id}/decline-action`. Everything a resume does — supersede the
/// paused run, hand over the tree, the slot and the conflict, continue the SAME session with the
/// task under `RESUMED_TASK_HEADER` — except the two things that would let the refused action
/// happen: it writes no `action_grants` row and sends nothing to `vcs::submit_on`.
/// `resume_did_not_act` needs no care: it only fires from an unconsumed grant, and there is none.
pub async fn decline_action(state: &AppState, proposal_id: i64) -> Result<i64, ResumeError> {
    continue_paused_run(state, proposal_id, Continuation::Decline).await
}

/// Why a correction did not happen. Every one of these goes to the owner (spec B D6).
#[derive(Debug)]
pub enum CorrectionRefusal {
    AlreadyCorrected,
    HandedOff,
    Trace(String),
    DailyCeiling,
    NoSession,
    NoWorktree,
    NoSlot(String),
    OriginGone,
    NoTask(String),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for CorrectionRefusal {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl CorrectionRefusal {
    /// The line the owner reads (spec B D7, `judge_needs_owner`).
    pub fn reason(&self) -> String {
        match self {
            Self::AlreadyCorrected => {
                "this task was already corrected once, and a lineage is corrected at most once"
                    .to_owned()
            }
            Self::HandedOff => {
                "the run handed off to a successor, which is working in its tree".to_owned()
            }
            Self::Trace(trace) => trace.clone(),
            Self::DailyCeiling => format!(
                "the project reached {} corrections in the last day",
                crate::judge::resolve::CORRECTIONS_PER_PROJECT_PER_DAY
            ),
            Self::NoSession => "the run recorded no session to continue".to_owned(),
            Self::NoWorktree => "the run's tree is gone".to_owned(),
            Self::NoSlot(why) => format!("no slot to run it in: {why}"),
            Self::OriginGone => "the run's row is gone".to_owned(),
            Self::NoTask(why) => {
                format!("the task this lineage started with could not be read: {why}")
            }
            Self::Db(error) => format!("the correction could not be written: {error}"),
        }
    }
}

pub(super) fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|database_error| database_error.is_unique_violation())
}

/// Spec B D6: one more turn for a completed worktree run whose gate failed, in the same
/// conversation and the same tree. Its own function, not `resume_approved_run`: that one skips
/// the kill switch and the budget, and needs a pending proposal and a paused run, and mixing two
/// such different sets of safety rules in one function is how one of them gets lost. The caller
/// (`judge::correction::after_gate_failed`) has already asked the judge and checked conditions
/// 2, 3 and 5 to 8 outside any transaction; this function owns 1, 2, 4 and 7 inside it.
///
/// In one transaction, in this order (spec B D6 "Como", S3):
/// 1. the `judge_corrections` row is the FIRST write, so from here the write lock is the
///    correction's, and the UNIQUE refuses a concurrent second correction of the lineage;
/// 2. conditions 1 (no successor) and 2 (no trace) again, and the daily ceiling counted with this
///    row, none of which can change until the commit;
/// 3. the run, by `INSERT ... SELECT` over the origin and exactly one row. Before the slot and the
///    tree, where spec B lists it after them: both hand over TO the new id, which only exists once
///    the row does, and the order inside one transaction changes nothing a reader can see;
/// 4. the origin's slot handed over (a finished run's slot is only freed by the sweep), or, if the
///    sweep already freed it, a new one claimed on this same transaction (`claim_on`, S4); no room
///    rolls everything back;
/// 5. the tree's owner moved, and it must move exactly one row;
/// 6. `correction_run_id` recorded.
///
/// No `action_grants`, nothing sent to the git queue, and the origin's status left as it is.
/// After the commit, launched like the resume: `--resume` with the session, one attempt, the
/// origin's `steerable`, and a NEW worktree clock. The correction happens at most once per
/// lineage, so it widens a lineage by one worktree clock at most, and it needs the time to build
/// and run the gate; what was left of the origin's clock may be nothing.
pub async fn resume_for_correction(
    state: &AppState,
    origin: i64,
    exit_code: i32,
) -> Result<i64, CorrectionRefusal> {
    // project_id, session_id, gate_output, lineage root, steerable.
    type OriginRow = (Option<String>, Option<String>, Option<String>, i64, i64);
    let row: Option<OriginRow> = sqlx::query_as(
        "SELECT project_id, session_id, gate_output, COALESCE(lineage_root_id, id), steerable
         FROM runs WHERE id = ?",
    )
    .bind(origin)
    .fetch_optional(&state.pool)
    .await?;
    let Some((project_id, session_id, gate_output, root, steerable)) = row else {
        return Err(CorrectionRefusal::OriginGone);
    };
    // Condition 4, read first because it is monotone (a lineage once corrected stays corrected)
    // and because after a correction the origin no longer owns its tree, so the lookups below
    // would answer "no worktree" for what is really "already corrected". The UNIQUE inside the
    // transaction is still what decides a race.
    let corrected: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM judge_corrections WHERE root_run_id = ?)")
            .bind(root)
            .fetch_one(&state.pool)
            .await?;
    if corrected {
        return Err(CorrectionRefusal::AlreadyCorrected);
    }
    let project_id = project_id.ok_or(CorrectionRefusal::OriginGone)?;
    let session_id = session_id.ok_or(CorrectionRefusal::NoSession)?;
    let (project_root, wt_path, branch): (String, String, String) = sqlx::query_as(
        "SELECT project_root, path, branch FROM worktrees
         WHERE owner_kind = 'run' AND owner_id = ? AND removed_at IS NULL",
    )
    .bind(origin)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(CorrectionRefusal::NoWorktree)?;
    // Outside the transaction, for `resume_approved_run`'s reason: `original_task` walks the
    // runs table with its own connection, and doing that under the write lock is a deadlock.
    // Unreadable or empty is a refusal, never `unwrap_or_default`: nobody watches this turn, and
    // an empty task would be the brief every later continuation of the lineage carries.
    let task = original_task(&state.pool, origin)
        .await
        .map_err(|error| CorrectionRefusal::NoTask(error.to_string()))?;
    if task.trim().is_empty() {
        return Err(CorrectionRefusal::NoTask("it is empty".to_owned()));
    }
    let prompt = correction_prompt(exit_code, gate_output.as_deref().unwrap_or_default(), &task);
    let prompt_for_route = prompt.clone();
    let draft = CorrectionDraft {
        origin,
        root,
        project_id: &project_id,
        wt_path: &wt_path,
        branch: &branch,
        prompt: &prompt,
        now: chrono::Utc::now(),
    };
    let correction = write_correction(&state.pool, &draft).await?;

    let daemon_token = mint_run_token(&state.pool, correction).await;
    let rules =
        crate::config::load_schedule_rules(state.machine_config_root.as_deref(), &project_id);
    // A correction is the same work continuing, so it runs under the ceiling the project names.
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
        correction,
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
            format!("correction run completed on {branch}"),
        )),
        gate_config,
        1,
        crate::execution::runner::ToolPolicy::Unrestricted,
        run_env(
            &daemon_token,
            correction,
            None,
            crate::speed::Capacity::solo(),
        ),
        steerable != 0,
        run_timeout_for_mode(state.run_timeout, "worktree"),
        progress_timeout_for_mode(state.progress_timeout, "worktree"),
        governed_by_classifier,
        state.runner.model_for_stage(None),
        None,
        // A correction continues the failed run's session, so it is routed as a resume: only the
        // runner that holds the session is eligible and the advice is recorded, never applied.
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
        // No job MCP: `after_gate_failed` only ever corrects a run outside jobs.
        None,
        max_turns,
    );
    Ok(correction)
}

/// What `write_correction` writes, read and built before its transaction opens.
pub(super) struct CorrectionDraft<'a> {
    pub(super) origin: i64,
    pub(super) root: i64,
    pub(super) project_id: &'a str,
    pub(super) wt_path: &'a str,
    pub(super) branch: &'a str,
    pub(super) prompt: &'a str,
    pub(super) now: chrono::DateTime<chrono::Utc>,
}

/// Steps 1 to 6 of `resume_for_correction`, the one transaction. Its own function so a test can
/// reach the UNIQUE that the pre-check normally answers first (spec B S3).
pub(super) async fn write_correction(
    pool: &sqlx::SqlitePool,
    draft: &CorrectionDraft<'_>,
) -> Result<i64, CorrectionRefusal> {
    let (origin, root, now_text) = (draft.origin, draft.root, draft.now.to_rfc3339());
    let mut tx = pool.begin().await?;
    if let Err(error) = sqlx::query(
        "INSERT INTO judge_corrections (root_run_id, origin_run_id, project_id, created_at)
         VALUES (?, ?, ?, ?)",
    )
    .bind(root)
    .bind(origin)
    .bind(draft.project_id)
    .bind(&now_text)
    .execute(&mut *tx)
    .await
    {
        return Err(if is_unique_violation(&error) {
            CorrectionRefusal::AlreadyCorrected
        } else {
            error.into()
        });
    }
    // `fetch_optional`: the row may have gone since the read outside, and that is `OriginGone`.
    let successor: Option<Option<i64>> =
        sqlx::query_scalar("SELECT successor_run_id FROM runs WHERE id = ?")
            .bind(origin)
            .fetch_optional(&mut *tx)
            .await?;
    match successor {
        None => return Err(CorrectionRefusal::OriginGone),
        Some(Some(_)) => return Err(CorrectionRefusal::HandedOff),
        Some(None) => {}
    }
    if let Some(trace) =
        crate::judge::correction::lineage_trace_on(&mut tx, root, Some(draft.branch)).await?
    {
        return Err(CorrectionRefusal::Trace(trace));
    }
    if crate::judge::correction::corrections_in_last_day_on(&mut tx, draft.project_id, draft.now)
        .await?
        > crate::judge::resolve::CORRECTIONS_PER_PROJECT_PER_DAY
    {
        return Err(CorrectionRefusal::DailyCeiling);
    }
    let inserted = sqlx::query(CORRECTION_INSERT.as_str())
        .bind(draft.wt_path)
        .bind(draft.prompt)
        .bind(&now_text)
        .bind(origin)
        .execute(&mut *tx)
        .await?;
    if inserted.rows_affected() != 1 {
        return Err(CorrectionRefusal::OriginGone);
    }
    let correction = inserted.last_insert_rowid();
    let handed = sqlx::query(
        "UPDATE project_slots SET owner_id = ? WHERE owner_kind = 'run' AND owner_id = ?",
    )
    .bind(correction)
    .bind(origin)
    .execute(&mut *tx)
    .await?;
    if handed.rows_affected() == 0 {
        match crate::execution::concurrency::claim_on(
            &mut tx,
            draft.project_id,
            crate::execution::worktree::Owner::Run(correction),
        )
        .await?
        {
            crate::execution::concurrency::ClaimOutcome::Claimed(_) => {}
            crate::execution::concurrency::ClaimOutcome::Full(no_room) => {
                return Err(CorrectionRefusal::NoSlot(no_room.reason()));
            }
        }
    }
    let moved = sqlx::query(
        "UPDATE worktrees SET owner_id = ?
         WHERE owner_kind = 'run' AND owner_id = ? AND removed_at IS NULL",
    )
    .bind(correction)
    .bind(origin)
    .execute(&mut *tx)
    .await?;
    if moved.rows_affected() != 1 {
        return Err(CorrectionRefusal::NoWorktree);
    }
    sqlx::query("UPDATE judge_corrections SET correction_run_id = ? WHERE root_run_id = ?")
        .bind(correction)
        .bind(root)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(correction)
}

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
                super::verify_wake::WAKES_PER_LINEAGE
            ),
            Self::DailyCeiling => format!(
                "the project reached {} verdict wakes in the last day",
                super::verify_wake::WAKES_PER_PROJECT_PER_DAY
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
    let prompt = super::verify_wake::verdict_prompt(ticket_id, ticket, &task);
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
    super::verify_wake::rearm(state).await;
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
    if in_lineage >= super::verify_wake::WAKES_PER_LINEAGE {
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
    if in_project >= super::verify_wake::WAKES_PER_PROJECT_PER_DAY {
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

pub(super) async fn continue_paused_run(
    state: &AppState,
    proposal_id: i64,
    how: Continuation,
) -> Result<i64, ResumeError> {
    let proposal = crate::proposals::get(&state.pool, proposal_id)
        .await?
        .ok_or(ResumeError::ProposalNotFound)?;
    if proposal.kind != "action-approval" || proposal.status != "pending" {
        return Err(ResumeError::ProposalNotPending);
    }

    let original_run_id = proposal
        .run_id
        .ok_or(ResumeError::NotResumable("proposal has no run_id"))?;
    let session_id = proposal
        .session_id
        .clone()
        .ok_or(ResumeError::NotResumable("run has no session_id to resume"))?;
    let tool_name = proposal
        .tool_name
        .clone()
        .ok_or(ResumeError::NotResumable("proposal has no tool_name"))?;

    // A node of a job does not own the tree it is paused in — the job does, and has since before
    // this node started. Looking it up by run would answer "no live worktree" for every paused job
    // node, so approving one returned `NotResumable` and the shell offered a button that could not
    // work. Which owner to ask for is decided by the paused run's own `job_id`.
    //
    // `steerable` rides along for a different reason, and it is the one an owner felt: a paused run
    // that is approved back to life is the same work, so the permission to speak to that work
    // survives the pause. Without it the answer to a mid-course correction was a 409 from a run the
    // owner had just personally authorised to continue.
    let (job_id, stage, item_id, steerable): (Option<i64>, Option<String>, Option<i64>, i64) =
        sqlx::query_as("SELECT job_id, stage, item_id, steerable FROM runs WHERE id = ?")
            .bind(original_run_id)
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or((None, None, None, 0));
    if how == Continuation::Decline && job_id.is_some() {
        return Err(ResumeError::BelongsToAJob);
    }
    // Read here, outside the transaction opened below, because `original_task` walks the run table
    // with its own connection and doing that while holding SQLite's write lock is a deadlock
    // waiting for a busy night. A resume that cannot recover the task is not a reason to refuse the
    // approval — the note still names the action and the session still has the history — so a
    // failure falls back to an empty task and the note simply omits the section.
    let carried_task = original_task(&state.pool, original_run_id)
        .await
        .unwrap_or_default();
    // Item before job, and both are set at once — a node of an item IS a node of its job. The
    // innermost tree wins, because it is the one the work is in: answering with the job's would
    // resume the agent in the integration checkout with its edits somewhere else entirely.
    let owner = match (item_id, job_id) {
        (Some(item_id), _) => crate::execution::worktree::Owner::Item(item_id),
        (None, Some(job_id)) => crate::execution::worktree::Owner::Job(job_id),
        (None, None) => crate::execution::worktree::Owner::Run(original_run_id),
    };

    let owned_tree = sqlx::query_as::<_, (String, String, String)>(
        "SELECT project_id, project_root, path
         FROM worktrees WHERE owner_kind = ? AND owner_id = ? AND removed_at IS NULL",
    )
    .bind(owner.kind())
    .bind(owner.id())
    .fetch_optional(&state.pool)
    .await?;

    // **A run that owns no worktree is not a run that cannot be resumed, and reading the two as one
    // made a whole mode a dead end.** `mode = "real"` runs in a directory the caller named; there is
    // no row in `worktrees` for it and there never was one to find. So this answered
    // `NotResumable("no live worktree for the paused run")` for every paused `real` run, while the
    // shell went on offering an Approve button — a person clicked it and got a conflict, with no
    // other way forward than rejecting the very thing they were trying to allow.
    //
    // Measured 2026-08-27 on run 900376: it parked on its first command, one minute in, and could
    // not be released by any means except refusing it.
    //
    // The directory the run recorded for itself is the same one it was working in, so resuming
    // there continues exactly what was paused. `NotResumable` is still the answer when there is no
    // directory to name — a run with neither a worktree nor a `cwd` genuinely has nowhere to go.
    let (wt_project_id, project_root, wt_path) = match owned_tree {
        Some(found) => found,
        None => recorded_tree_of(state, original_run_id).await?,
    };

    // Spec decision 2, arrived at the other way round: rather than letting the run perform the merge
    // once a human says yes, the yes IS the queueing. Resolved before the transaction opens, because
    // it runs git twice — reading the worktree's branch and identifying the repository — and holding
    // SQLite's write lock across a subprocess would stall every other writer in the daemon.
    let queueable = match how {
        Continuation::Approve => {
            queueable_operation(state, &proposal, &wt_project_id, &wt_path).await
        }
        // A declined push must never reach the queue carrying the owner's authority (D12).
        Continuation::Decline => None,
    };

    // The class the grant will authorize, derived before the transaction opens so a parse cannot
    // hold SQLite's write lock. Re-derived here rather than carried on the proposal because
    // `classify` is pure and `wt_path` is the very cwd the hook will hand it when the resume
    // attempts the action — the same inputs, so the same answer, with nothing to keep in step.
    // Absent or unparseable input yields no class, and a classless grant authorizes nothing.
    //
    // The policy AND the project's two shell lists come off the state for that same reason, and
    // they are both parameters of `classify` for it: purity is what makes "the same inputs, so the
    // same answer" true, and each of the two is an input this caller has to fetch. A resume
    // classifying under an empty policy while the hook classified under the owner's — or under an
    // empty rule set while the hook classified under the project's — would answer differently about
    // the identical command line, which is the drift the paragraph above rules out.
    //
    // **BOTH are now a table read TWICE, and the paragraph that stood here said otherwise.** It read:
    // "the policy is built once at startup and handed to all three of `classify`'s production callers
    // unchanged, so 'the same one' is literally true of it". That was true until
    // `Policy::for_project` landed. The policy handed to a decision is now the machine default with
    // this project's `project_github_ops` rows laid over it, read at decision time and uncached
    // (design §4.4) — so it is read once by the hook when the command was attempted and once here
    // when the approval is granted, exactly as the shell rules are, and an operation declared between
    // those two moments makes the two reads differ. Nobody loads either of them once for both; there
    // is no such caller. The read deliberately stays uncached.
    //
    // **And this half's drift is bounded more tightly than the shell rules', which is worth working
    // out rather than inheriting.** `classifier.rs`'s contract paragraph for `rules` carries the
    // three-point argument for that side. Here there is only one direction to have: the list has no
    // `deny`, so a declaration between the two moments moves the class from `unrecognized` to
    // `github-read` and a withdrawal cannot be reached at all — the hook ANSWERED `github-read` with
    // an allow, so no proposal was minted and there is no resume to drift. The one reachable case
    // therefore records `github-read` where the person approved an `unrecognized`, and a
    // `github-read` grant authorizes a class that is allowed without any grant. It buys nothing,
    // which is narrower than what they agreed to and never wider.
    //
    // The project is `wt_project_id`, off the worktree this resume is going back into, and it IS
    // the project the hook read off the paused run's own row. Checked rather than assumed, because
    // the two columns are read from different tables: `worktree::record` is the only production
    // INSERT into `worktrees`, and its `project_id` argument is the same Rust binding that `runs`
    // was inserted with (`create_run_with`, for `Owner::Run` and `Owner::Item`) or the same
    // `jobs.project_id` the run's own row was created from (`job.rs`, for `Owner::Job`); that INSERT
    // is an UPSERT, and its conflict arm does write both columns — `project_id =
    // excluded.project_id, project_root = excluded.project_root` — but only ever from the same
    // caller's binding, so a second `record` for a tree already known re-states the binding rather
    // than replacing it with a differently-derived one; neither owner-transfer statement — the
    // handoff and this resume's own — touches `project_id` at all, each setting `owner_id` and
    // nothing else, which is stronger than copying it; and `runs.id` is `AUTOINCREMENT`, so no stale
    // tree can be adopted by a later run wearing a reused id. The `recorded_tree_of` fallback reads
    // `runs.project_id` outright.
    //
    // **Nothing ENFORCES it** — no foreign key, no `CHECK` — and that upsert's conflict arm is the
    // concrete mechanism by which it would break. A third caller of `worktree::record` that resolved
    // its project from `vcs::project_for_worktree` instead of from the caller would not fail, would
    // not add a row, and would leave no trace a reader could go looking for: it would silently
    // rewrite an existing worktree's project, and this is the sentence that would then be wrong.
    // The arm is also invisible to `grep "UPDATE worktrees"`, which is how the previous version of
    // this paragraph came to claim that no `UPDATE` of either column existed anywhere.
    //
    // Derived even when the action was queued instead of authorized. The row is excluded from
    // authorizing by its `queued_request_id`, not by being classless, and a takeover that recorded
    // no class would be a row that could not say what was taken over.

    // This project's policy, and not the machine's. The hook classified the attempt under it, so the
    // label recorded for the approval has to come from the same place — the whole argument above,
    // applied to the input that used to be the one input this caller could take for granted.
    //
    // No `match` on this one, and the asymmetry with the line below is deliberate rather than an
    // oversight: `github_ops` swallows an unreadable table into an empty overlay, which is the
    // machine default, which is the widest thing this can wrongly be — and the direction of that
    // error is a class recorded as `unrecognized` where the project had earned `github-read`. That
    // is a NARROWER grant than the person approved, which is the side of the line a label may err on.
    //
    // Only an approval computes it: the class serves the grant alone, and a decline writes none.
    let action_class = match how {
        Continuation::Decline => None,
        Continuation::Approve => {
            let policy = state
                .github
                .policy_for_project(&state.pool, &wt_project_id)
                .await;
            match crate::project::policy::shell_rules(&state.pool, &wt_project_id).await {
                Ok(rules) => proposal
                    .tool_input
                    .as_deref()
                    .and_then(|input| serde_json::from_str::<serde_json::Value>(input).ok())
                    .map(|input| {
                        crate::governance::classifier::classify(
                            &tool_name,
                            &input,
                            Some(std::path::Path::new(&wt_path)),
                            &policy,
                            &rules,
                            // Labelling an action a person has just approved, not deciding one. The strict
                            // reading keeps the recorded class the same as the one that was shown to them.
                            crate::governance::classifier::Unrecognized::AsksAPerson,
                        )
                        .action_class
                    }),
                // No class, which is what this path already does for input it cannot parse — and for the
                // same reason, since the fault is the same one: a label derived from rules that are not
                // this project's is not this project's label. It is deliberately NOT the hook's answer to
                // an unreadable read. The hook is DECIDING, so it owes the safe direction and downgrades an
                // allow to an approval prompt; this is LABELLING an action a person has already approved,
                // where the only two outcomes available are a right label and a wrong one. A wrong one is
                // worse than none: a class is what a later grant is scoped to, so a class recorded under
                // the wrong rules would authorise a set of actions nobody agreed to.
                Err(error) => {
                    tracing::warn!(
                        run_id = original_run_id,
                        project_id = %wt_project_id,
                        %error,
                        "resume: could not read the project's shell rules — recording no action class"
                    );
                    None
                }
            }
        }
    };

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = state.pool.begin().await?;

    // Spec B D12: the supersede is a compare-and-set. The proposal was read before this transaction
    // opened, and a release, a cancel or a failure can have ended the paused run since. Continuing
    // a run that already ended is the same wrong for an approval and for a decline — a successor
    // on a tree that was released, a conversation somebody stopped — so both refuse, and dropping
    // `tx` rolls the whole continuation back.
    //
    // It used to say, here, that a run left `awaiting_approval` holds a slot which rejects the
    // INSERT below. That stopped being true at migration 0053, which dropped the index the claim was
    // really made of; a stranded run holds a numbered slot now and blocks nothing. Nothing below
    // depends on the refusal — the slot is HANDED OVER further down rather than competed for — but
    // the sentence outlived the mechanism, which is how a guard comes to be believed in and not
    // written.
    //
    // `completed_at` keeps the pause time `finalize_termination` wrote: that is when the process
    // ended. Overwriting it with the approval time billed an unmeasured run for every hour the
    // owner took to answer (run 900428: paused after 4 minutes, approved 30.9 hours later). The
    // approval time is `proposals.decided_at`. `now` is only the fallback for a lost pause write.
    let superseded = sqlx::query(
        "UPDATE runs SET status='superseded', completed_at=COALESCE(completed_at, ?)
         WHERE id=? AND status='awaiting_approval'",
    )
    .bind(&now)
    .bind(original_run_id)
    .execute(&mut *tx)
    .await?;
    if superseded.rows_affected() != 1 {
        return Err(ResumeError::NotResumable(
            "the paused run is no longer awaiting approval",
        ));
    }
    // Admitted inside this transaction, and that is the whole reason `submit_on` exists. Queue then
    // commit separately, either order, and one of two things can happen: a merge queued against an
    // approval that rolls back — an irreversible publication nobody authorised, with the proposal
    // still pending so it can be authorised again — or a run resumed and told its merge is queued
    // when it is not.
    //
    // `Origin::Human` rather than `Origin::Run`, and the difference is not bookkeeping. A human just
    // approved this, so it carries their authority and starts `queued` rather than waiting for an
    // approval it already has. Tying it to the run instead would tie it to a row this very
    // transaction is about to mark `superseded`, and the merge must outlive the run that asked for
    // it — that is the point of handing it to a queue.
    let queued_request_id = match &queueable {
        Some((repo, op)) => Some(
            crate::execution::vcs::submit_on(
                &mut *tx,
                repo,
                op,
                crate::execution::vcs::Origin::Human,
            )
            .await?,
        ),
        None => None,
    };

    // The run is told which of the two happened, because the two ask opposite things of it. Named by
    // `kind()` rather than by the word "merge", which is what it said while merge was the only thing
    // the queue could do: a run told its *merge* was queued after asking for a push would read that
    // as the daemon having misunderstood it, and go looking for what it had misfiled.
    let note = match (how, queued_request_id, &queueable) {
        (Continuation::Decline, _, _) => CONTINUING_WITHOUT_IT.to_owned(),
        (Continuation::Approve, Some(request_id), Some((_, op))) => {
            let kind = op.kind();
            format!(
                "A previously paused autonomous run has been resumed after approval of proposal #{proposal_id}. The {tool_name} action you attempted is NOT authorized for you to perform: the {kind} it asked for has been handed to the daemon's git queue as request #{request_id}, which serialises every git operation on this repository and will carry it out for you. Do not attempt it again. Continue with the rest of the task."
            )
        }
        // `queued_request_id` is `Some` exactly when `queueable` is, and they are matched together
        // rather than one of them being unwrapped inside the other's arm — so the impossible pairing
        // has to be written out, and what it does is fall back to authorizing. A run told to proceed
        // is the safe half of this decision: the grant is single-use and the action is one a human
        // just approved.
        (Continuation::Approve, _, _) => {
            resume_instruction(proposal_id, &tool_name, proposal.tool_input.as_deref())
        }
    };

    // The note is what CHANGED; the task is what the run is still for, and the row has to carry
    // both. It resumes the same CLI session, so in the ordinary case the history is right there and
    // the note alone reads fine — but the row's prompt is the only thing that survives the session,
    // and a handoff out of this run reads exactly that. Told only the note, a successor inherits an
    // instruction about a proposal as its entire brief (see `task_to_carry`).
    //
    // Appended under a header rather than merged into the sentence, so `task_to_carry` can take it
    // back out on the next resume instead of nesting.
    let prompt = if carried_task.trim().is_empty() {
        note
    } else {
        format!("{note}\n\n{RESUMED_TASK_HEADER}\n{carried_task}")
    };

    // The resume carries the node's identity forward. Without it the new run belongs to no job, so
    // the chain that has to finalise it cannot see it: the item stays `running` forever and the job
    // sits there until the four-hour ceiling retires it, with the approved work already done.
    // `item_id` travels with the rest of that identity, and it is the second approval that proves
    // it does: left out, the FIRST resume still lands in the right tree — it was resolved from the
    // predecessor's own row — while the successor it writes carries no item, so approving that one
    // resolves to the job's tree instead. One approval looks correct; two do not.
    //
    // Spec B D6.2, taken here because spec A adds a column to it: `INSERT ... SELECT` over the
    // paused row, and exactly one row or nothing. With `VALUES` the insert always happened and
    // every copied column needed its own guess for a vanished origin (`COALESCE(..., 1)` for the
    // taint); with `SELECT`, a vanished origin inserts nothing and the transaction rolls back,
    // whatever the column. The continuation columns (`CONTINUATION_COLUMNS`: the taint, the
    // permission mode, both judge modes) are copied for the reason the handoff successor copies
    // them: this is the same session, so its taint and its rules continue. `lineage_root_id` is not
    // copied but computed, `COALESCE(lineage_root_id, id)`, so a root hands on its own id (spec B
    // D6.1).
    let insert = match how {
        Continuation::Approve => RESUME_INSERT.as_str(),
        Continuation::Decline => DECLINE_INSERT.as_str(),
    };
    let result = sqlx::query(insert)
        .bind(&wt_project_id)
        .bind(&wt_path)
        .bind(&prompt)
        .bind(&session_id)
        .bind(&now)
        .bind(job_id)
        .bind(stage.as_deref())
        .bind(item_id)
        .bind(steerable)
        .bind(original_run_id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound.into());
    }
    let resume_id = result.last_insert_rowid();
    // The hand-over is a no-op for a job's tree, and must be: the job owns it, and moving ownership
    // to this one node would let the GC collect it the moment that node finished — with the rest of
    // the queue still to run in it. Filtering on `owner_kind = 'run'` is what makes that so.
    sqlx::query("UPDATE worktrees SET owner_id=? WHERE owner_kind='run' AND owner_id=?")
        .bind(resume_id)
        .bind(original_run_id)
        .execute(&mut *tx)
        .await?;
    // The slot follows the tree, and is a no-op for a job's for the same reason — hence the same
    // `owner_kind = 'run'` filter.
    //
    // Handed over rather than claimed anew, and the difference is not style. A claim is per owner:
    // the resume would ask for a SECOND slot while its own predecessor still held the first, so a
    // project at its ceiling would refuse an approval a human had already given — the resume denied
    // a slot by the very run it replaces. Handing it over keeps one piece of work to one slot.
    //
    // Left out entirely, which is what happened until now, the row keeps pointing at the run
    // `superseded` a few lines above. `reconcile_orphaned_slots` frees any slot whose owner is not
    // live and runs on every job tick, so it collects this one out from under a run still working in
    // the tree — and the project, reading one fewer in flight than it has, starts another. Two
    // worktrees in one repository is precisely what `project_slots` exists to prevent.
    sqlx::query("UPDATE project_slots SET owner_id=? WHERE owner_kind='run' AND owner_id=?")
        .bind(resume_id)
        .bind(original_run_id)
        .execute(&mut *tx)
        .await?;
    // The item follows its node. Left pointing at the run just marked `superseded`, the job's next
    // pass would read a terminal node that did not complete and stop the whole chain — turning an
    // approval into a failure, and throwing away the work the user just authorised.
    sqlx::query("UPDATE job_nodes SET run_id = ? WHERE run_id = ?")
        .bind(resume_id)
        .bind(original_run_id)
        .execute(&mut *tx)
        .await?;
    // The conflict follows its resolver, for the same reason as the three above and in the same
    // shape. Left pointing at the run marked `superseded`, the resolution stops reading as live:
    // `resolver.rs`'s "two never run at once" brake is a join through this column onto a live run,
    // so it stops seeing this one and a second agent can be minted for the same two branches — and
    // anything that cancels a resolution whose conflict has since been settled cannot reach the run
    // actually doing the work.
    //
    // Measured on this repository. Requests 79 and 80 escalated, each got a resolution, and both
    // conflicts were then settled another way and landed as request 85. Both resolutions had been
    // resumed once, so both were linked to rows reading `superseded`; they carried on for another
    // thirteen hours and $3.01 between them before escalating again as 87 and 88, over a conflict
    // that had stopped existing the previous evening.
    sqlx::query("UPDATE vcs_requests SET resolution_run_id = ? WHERE resolution_run_id = ?")
        .bind(resume_id)
        .bind(original_run_id)
        .execute(&mut *tx)
        .await?;
    // One row either way, and it carries both keys because the two columns answer different
    // questions about it.
    //
    // `action_class` is what an authorization is checked against (migration 0055); a NULL there
    // authorizes nothing, so the resume would park again on the action just approved. `tool_input`
    // is the record of the exact spelling the human read (migration 0020), and it is what `hooks.rs`
    // matches a retry against when the action was taken over rather than authorized.
    //
    // `queued_request_id` is which of the two this row is (migration 0054). NULL is an authorization
    // — the run may perform actions of that class for the rest of its life. Set is the opposite
    // fact: the queue has this action, the run does not. `grant_covers_class` excludes those rows,
    // and the class rule is what makes that exclusion load-bearing rather than tidy — a takeover row
    // that covered its class would authorize every later merge the run attempted, off a row minted
    // to say merging had been taken away from it.
    //
    // Written even when nothing is granted because the run has to be ABLE to be told. Without the
    // row, a resumed run that tried its merge again would be paused and would mint a second proposal
    // for a person to read — and approving that one would queue the merge twice.
    match how {
        Continuation::Approve => {
            sqlx::query(
                "INSERT INTO action_grants (run_id, tool_name, tool_input, action_class, proposal_id, created_at, consumed_at, queued_request_id)
                 VALUES (?, ?, ?, ?, ?, ?, NULL, ?)",
            )
            .bind(resume_id)
            .bind(&tool_name)
            .bind(proposal.tool_input.as_deref())
            .bind(action_class)
            .bind(proposal_id)
            .bind(&now)
            .bind(queued_request_id)
            .execute(&mut *tx)
            .await?;
        }
        // Spec B D12: no grant, and the refusal is remembered instead — keyed by the lineage root,
        // read INSIDE the transaction so the mark and the successor agree on whose lineage it is.
        Continuation::Decline => {
            let root: i64 =
                sqlx::query_scalar("SELECT COALESCE(lineage_root_id, id) FROM runs WHERE id = ?")
                    .bind(original_run_id)
                    .fetch_one(&mut *tx)
                    .await?;
            crate::proposals::record_declined_action_on(
                &mut tx,
                root,
                &tool_name,
                proposal.tool_input.as_deref(),
                proposal_id,
            )
            .await?;
        }
    }
    // Compare-and-set on the state this resume was authorised from, like every other writer of a
    // proposal decision (`proposals::transition`, `worktree::release`). The `status != "pending"`
    // check at the top of this function reads OUTSIDE the transaction, so a reject arriving in
    // between would otherwise be overwritten here: the user's rejection would be stamped
    // `approved`, its audit trail would read pending→approved, and the resume run would go on to
    // perform the action they had just refused.
    //
    // Deliberately untested. Landing a rejection inside the window means writing to `proposals`
    // after this transaction opens but before it takes SQLite's write lock, and once that lock is
    // held a second writer blocks rather than races — a hand-driven poll test for it only ever
    // reproduced the busy timeout. The guard costs one clause; a flaky test would cost more.
    let decided = match how {
        Continuation::Approve => "approved",
        // Spec B D12: a new status, and a decision rather than an expiry (spec A D14).
        // `proposals.status` has no CHECK (migration 0010), so it enters only here.
        Continuation::Decline => "declined",
    };
    let approved = sqlx::query(
        "UPDATE proposals SET status = ?, decided_at = ? WHERE id = ? AND status = 'pending'",
    )
    .bind(decided)
    .bind(&now)
    .bind(proposal_id)
    .execute(&mut *tx)
    .await?;
    if approved.rows_affected() != 1 {
        // Rolls back by dropping `tx`: the supersede, the resume row, the worktree hand-over and
        // the grant all disappear with it, so a lost race leaves nothing half-applied.
        return Err(ResumeError::ProposalNotPending);
    }
    // The queued request id belongs in the audit trail, not only in the resumed run's prompt: this
    // row is where somebody reconstructs what an approval actually did, and "approved" alone no
    // longer says whether the action was authorised or taken over. Spec B D12: a decline says so
    // here too, because "declined" alone does not say the run went on.
    let note = match (how, queued_request_id, &queueable) {
        (Continuation::Decline, _, _) => {
            format!("declined the action only; the run continues as run {resume_id}")
        }
        (Continuation::Approve, Some(request_id), Some((_, op))) => {
            let kind = op.kind();
            format!("approved; {kind} queued as vcs request {request_id}; resume run {resume_id}")
        }
        (Continuation::Approve, _, _) => format!("approved; resume run {resume_id}"),
    };
    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, 'pending', ?, ?, ?)",
    )
    .bind(proposal_id)
    .bind(decided)
    .bind(&note)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    // The paused run left `awaiting_approval` (now `superseded`) and its successor is `running`.
    // Signalled after the commit, never inside the transaction: a woken reader must find the new
    // state, and a rolled-back resume must not wake anyone.
    nucleos_base::wake::bump(nucleos_base::wake::Topic::Runs);
    // A superseded run is over: its task claims go back, like any run that ended. Neither
    // `finalize_termination` nor the supervisor sees this transition (the run had already paused).
    crate::execution::runs::lifecycle::release_claims_of_run(&state.pool, original_run_id).await;

    // The resume continues the paused run's work, so it keeps the tools that run was frozen with,
    // like the handoff successor (`prepare_handoff_successor`). After the commit, for the same
    // reason as the token below. A missed copy fails closed.
    if let Err(error) = crate::loadout::carry(&state.pool, original_run_id, resume_id).await {
        tracing::warn!(run_id = resume_id, %error, "could not carry a run's loadout");
    }

    // After the commit, because the resume row does not exist to be UPDATEd before it.
    let daemon_token = mint_run_token(&state.pool, resume_id).await;
    let rules =
        crate::config::load_schedule_rules(state.machine_config_root.as_deref(), &wt_project_id);
    // A resumed run is the same work continuing, so it runs under the same ceiling the project
    // names - read here rather than carried on the row, because the row would hold what the file
    // said when the run first started and an operator who raised the ceiling to let this very run
    // finish would find the old one still in force.
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
        Err(error) => {
            tracing::warn!(
                project_id = %wt_project_id,
                project_root = %project_root,
                %error,
                "failed to load worktree gate configuration for resumed run"
            );
            GateConfig::Unreadable(format!("gate configuration is unreadable: {error}"))
        }
    };
    // Asked again against the worktree being resumed rather than inherited, because it is a fresh
    // launch into a tree that has since been worked in: the run it continues may have rewritten the
    // very settings file this reads. Re-checking is the conservative direction — a tree that no
    // longer wires the hook stops getting the classifier's surface.
    //
    // Hoisted out of the argument list it used to sit in, because it is now TWO arguments: the rung
    // the CLI launches on, and the belief the runner is told about. Derived once so they cannot
    // disagree — a resume that dropped `Bypass` here is the silent regression this fold could most
    // easily have introduced, in a path nothing else covers.
    let governed_by_classifier = classifier_governs_tools(
        "worktree",
        crate::execution::runner::ToolPolicy::Unrestricted,
        Some(std::path::Path::new(&wt_path)),
    );
    // Taken before `prompt` moves into the launch.
    let prompt_for_route = prompt.clone();
    spawn_run(
        state,
        state.runner.clone(),
        resume_id,
        prompt,
        Some(wt_project_id),
        Some(std::path::PathBuf::from(&wt_path)),
        // A resume is never planning: it continues approved autopilot work. So the rung is the
        // classifier's or none at all.
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
            format!("resumed run completed on nucleos/run-{original_run_id}"),
        )),
        gate_config,
        1,
        // A resume continues an approved worktree run, which is autopilot work: the hook and the
        // classifier govern it, exactly as they governed the run being resumed.
        crate::execution::runner::ToolPolicy::Unrestricted,
        // A resumed job node keeps its handoff directory: it is the same node, in the same tree,
        // finishing the same item, and the review node after it still reads `plan.json` from there.
        // An ordinary resumed run gets nothing, as before.
        run_env(
            &daemon_token,
            resume_id,
            job_id
                .map(|_| {
                    std::path::PathBuf::from(&wt_path)
                        .join(crate::execution::worktree::ARTIFACTS_DIR)
                })
                .as_deref(),
            crate::speed::Capacity::solo(),
        ),
        // Inherited from the run being resumed, like the handoff successor's. The row above carries
        // the same value, so the launch listens exactly when `post_run_message` will admit a turn.
        steerable != 0,
        // A resume is a worktree run, so it gets the worktree clock — the same one the run it
        // continues was given.
        run_timeout_for_mode(state.run_timeout, "worktree"),
        // And the worktree silence deadline, for the same reason: a resume goes straight back into
        // the work its predecessor was doing, which on this workspace means compiling.
        progress_timeout_for_mode(state.progress_timeout, "worktree"),
        governed_by_classifier,
        // The resume row carries the node's stage forward, so an approved plan node resumes on the
        // plan model rather than dropping back to the runner's own.
        state.runner.model_for_stage(stage.as_deref()),
        None,
        // Asked like any launch, but as a resume: only the runner that holds the session is
        // eligible, and the advice is recorded in shadow and never applied — the session keeps the
        // model and effort it started with (`route_advice::resolve`).
        Some(crate::models::route_advice::RouteQuery {
            task: prompt_for_route,
            stage: stage.clone(),
            item: None,
            resume: true,
            restrict_runners: None,
            pin: None,
            judged_later: false,
            effort: None,
            codex_profile: None,
        }),
        JobNodeMcp::for_run(resume_id, job_id),
        max_turns,
    );

    Ok(resume_id)
}

#[cfg(test)]
mod wake_tests {
    use std::time::Duration;

    use crate::execution::runs::testkit::{seed_real_worktree_approval, test_state_with_runner};

    /// An approved resume supersedes the paused run and starts its successor, then wakes the runs topic.
    ///
    /// The wake bus is process-global, so the assertion is `>= 1`, never an exact delta.
    #[tokio::test]
    async fn wake_signal_continue_paused_run_bumps_runs() {
        let (state, _runner) =
            test_state_with_runner(Some(Duration::from_secs(5)), Duration::from_secs(600)).await;
        let (proposal_id, _branch, _container) =
            seed_real_worktree_approval(&state, "cargo build").await;
        let rx = nucleos_base::wake::subscribe(nucleos_base::wake::Topic::Runs);
        let before = *rx.borrow();

        let resume_id =
            super::continue_paused_run(&state, proposal_id, super::Continuation::Approve)
                .await
                .unwrap();

        assert!(resume_id > 0);
        assert!(rx.borrow().wrapping_sub(before) >= 1);
        let superseded: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE status = 'superseded'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(superseded, 1, "the signal follows the committed write");
    }
}
