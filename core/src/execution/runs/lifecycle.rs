//! The end of a run: stop decisions, termination, cancel, orphan reconciliation at startup and transcript retention.

use super::*;

/// `?leading=` on `GET /runs/{id}/stop`: how many decisions accompany the last one. Absent is
/// [`STOP_LEADING_DEFAULT`]; anything above [`STOP_LEADING_MAX`] is clamped rather than refused
/// (spec §4).
#[derive(serde::Deserialize)]
pub struct StopQuery {
    pub leading: Option<i64>,
}

pub(super) const STOP_LEADING_DEFAULT: i64 = 10;
pub(super) const STOP_LEADING_MAX: i64 = 100;

/// The `runs` columns `GET /runs/{id}/stop` needs, read in one query the way `get_run` reads its
/// own wider set. Not `RunStatusResponse`: that struct answers a different route's shape, and
/// borrowing it here would make an unrelated route's column list load-bearing for this one's.
#[derive(sqlx::FromRow)]
pub(super) struct RunStopRow {
    pub(super) status: String,
    pub(super) mode: String,
    pub(super) exit_code: Option<i64>,
    pub(super) stderr: Option<String>,
    pub(super) successor_run_id: Option<i64>,
    pub(super) created_at: String,
    pub(super) completed_at: Option<String>,
}

/// One `shadow_decisions` row, read for `GET /runs/{id}/stop` only — the same seven columns spec
/// §5.2 lists, in the shape `sqlx` fills directly rather than a positional tuple.
#[derive(sqlx::FromRow)]
pub(super) struct StopDecisionRow {
    pub(super) tool_name: String,
    pub(super) action_class: String,
    pub(super) decision: String,
    pub(super) reason: Option<String>,
    pub(super) classifier_version: i64,
    pub(super) policy_digest: Option<String>,
    pub(super) tool_input: Option<String>,
    pub(super) created_at: String,
}

impl StopDecisionRow {
    pub(super) fn into_view(self) -> crate::execution::run_stop::GateDecisionView {
        crate::execution::run_stop::gate_decision_view(
            self.tool_name,
            self.action_class,
            self.decision,
            self.reason,
            self.classifier_version,
            self.policy_digest,
            self.tool_input,
            self.created_at,
        )
    }
}

/// Seconds between `created_at` and `completed_at` — or, for a run still running, between
/// `created_at` and now. Always a majorant of how long the run actually ran (spec §6): the
/// interval includes any time the run spent queued before it started, which `runs` does not
/// record separately.
///
/// `None` only if `created_at` itself fails to parse, which every writer of that column in this
/// codebase writes as `chrono::Utc::now().to_rfc3339()` — so in practice this is infallible.
pub(super) fn elapsed_seconds_since_created(
    created_at: &str,
    completed_at: Option<&str>,
) -> Option<i64> {
    let parse = |value: &str| {
        chrono::DateTime::parse_from_rfc3339(value).map(|when| when.with_timezone(&chrono::Utc))
    };
    let created = parse(created_at).ok()?;
    let end = match completed_at {
        Some(value) => parse(value).ok()?,
        None => chrono::Utc::now(),
    };
    Some((end - created).num_seconds())
}

/// `GET /runs/{id}/stop` (spec `.ai/specs/2026-08-29-porque-parou-design.md` §4): why a run
/// stopped, in one call. `404` if the run does not exist; `200` — never `204` — for every run that
/// does, including one still `running` (spec §4, §11 item 5).
///
/// Thin by design (spec §7): this reads the run row and, only for `gate` and `timeout` kinds, the
/// `shadow_decisions` leading up to it, and hands both to `run_stop::build_response` to become the
/// answer. Deriving `kind` here first — rather than inside `build_response` — is what lets this
/// skip the `shadow_decisions` query entirely for the other six kinds.
pub async fn get_run_stop(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(query): Query<StopQuery>,
) -> Result<Json<crate::execution::run_stop::RunStopResponse>, StatusCode> {
    let row = sqlx::query_as::<_, RunStopRow>(
        "SELECT status, mode, exit_code, stderr, successor_run_id, created_at, completed_at
         FROM runs WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let kind = crate::execution::run_stop::derive_kind(&row.status)
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let decisions = if crate::execution::run_stop::kind_shows_leading_up(kind) {
        let leading = query
            .leading
            .unwrap_or(STOP_LEADING_DEFAULT)
            .clamp(0, STOP_LEADING_MAX);
        // `kind: gate` fetches one extra row: the newest fills `gate`, and the `leading` after it
        // fill `leading_up`. `kind: timeout` never fills `gate` (spec §5.1), so it fetches exactly
        // `leading` rows, all of which become `leading_up` — otherwise `leading_up` would end up one
        // row longer than `?leading=` promised.
        let limit = if crate::execution::run_stop::kind_shows_gate(kind) {
            leading + 1
        } else {
            leading
        };
        sqlx::query_as::<_, StopDecisionRow>(
            "SELECT tool_name, action_class, decision, reason, classifier_version, policy_digest,
                    tool_input, created_at
             FROM shadow_decisions WHERE run_id = ?
             ORDER BY id DESC LIMIT ?",
        )
        .bind(id)
        .bind(limit)
        .fetch_all(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .map(StopDecisionRow::into_view)
        .collect()
    } else {
        Vec::new()
    };

    let elapsed_seconds =
        elapsed_seconds_since_created(&row.created_at, row.completed_at.as_deref())
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(crate::execution::run_stop::build_response(
        id,
        row.status,
        kind,
        &row.mode,
        elapsed_seconds,
        row.exit_code,
        row.stderr,
        row.successor_run_id,
        decisions,
    )))
}

/// PURE: whether a run status means the run is over for good.
///
/// `finalize_termination` is called with three distinct statuses by five callers, and one of them —
/// `hooks.rs`'s `pause_for_approval` — passes `awaiting_approval` for a run that is *pausing so a
/// human can approve something*. That run resumes. Treating it as ended would cancel the merge it
/// asked for, and the human would then approve a request that no longer exists.
///
/// `interrupted` and `timed_out` never arrive through `finalize_termination` — both are written by
/// direct UPDATEs — so listing them here is about the predicate being true rather than about that
/// call site: `timed_out` has its own call in `spawn_run`'s wall-clock arm, and `interrupted` is
/// inert today and would be obviously right if startup recovery ever routed through here.
pub(super) fn ends_the_run(status: &str) -> bool {
    ENDED_RUN_STATUSES.contains(&status)
}

/// The run statuses that mean the run is over for good — it will not resume, and anything it asked
/// for and never started should go with it.
///
/// A constant rather than a literal inside `ends_the_run`, because `vcs.rs` builds a SQL `IN` clause
/// from this same list and a second copy would drift silently: the failure would be a queue that
/// stops reaping, or one that reaps a run that was only paused, and neither announces itself.
///
/// `awaiting_approval` is deliberately absent, and it is the whole reason this is a list rather than
/// `status != 'running'`: a run pausing for a human resumes, and its merge must survive the pause.
pub const ENDED_RUN_STATUSES: &[&str] = &["cancelled", "failed", "interrupted", "timed_out"];

/// Every status a run can come to rest in — which is a wider question than the one above.
///
/// [`ENDED_RUN_STATUSES`] asks "did this run stop without finishing, so should its queued work be
/// thrown away?", and deliberately excludes both `completed` (whose queued merge is the point) and
/// `superseded` (whose work continues in the successor). This one asks "is this run still using its
/// worktree?", and the answer for all six is no.
///
/// It exists because something else has to agree with it: `worktree::gc_candidates` collects a
/// run's worktree only for a status it lists, so an ending missing from there leaks a directory
/// forever — invisibly, because as far as the system is concerned that run is over and its tree is
/// nobody's. `every_ending_a_run_can_have_is_an_ending_the_gc_collects` holds the two lists
/// together. The job side has had that guard since `stopped` opened exactly this leak
/// (`job::TERMINAL_STATUSES`); the run side had the same shape and no guard, and `superseded` —
/// added later, by the approval resume — was already missing from the GC when this was written.
pub const TERMINAL_RUN_STATUSES: &[&str] = &[
    "completed",
    "failed",
    "cancelled",
    "interrupted",
    "timed_out",
    "superseded",
];

/// Writes what a run's stream showed before a terminator cut it off: the transcript itself, and the
/// turns and prompt tokens `runner::usage_without_a_result` rebuilds from it.
///
/// The record the wall-clock branch of `spawn_run` already keeps for a timed-out run, for the same
/// reason — there is no `RunOutcome` to read — and it was missing for exactly the run that reaches
/// here most: one paused for approval, which left `stdout`, `num_turns` and every token count NULL
/// and no `run_events` at all.
///
/// `stdout IS NULL` keeps it from replacing anything a body already recorded, and the events follow
/// only a write that landed, so a second call cannot duplicate them. Best-effort, like the rest of
/// termination: the run is terminated either way.
pub(super) async fn record_the_cut_stream(pool: &sqlx::SqlitePool, id: i64, seen: &str) {
    if seen.is_empty() {
        return;
    }
    let usage = crate::execution::runner::usage_without_a_result(seen);
    let written = sqlx::query(
        "UPDATE runs SET stdout = ?, num_turns = ?, input_tokens = ?, cache_read_tokens = ?, \
         cache_creation_tokens = ?, context_peak = ?, tools_used = ? WHERE id = ? AND stdout IS NULL",
    )
    .bind(seen)
    .bind(usage.num_turns)
    .bind(usage.input_tokens)
    .bind(usage.cache_read_tokens)
    .bind(usage.cache_creation_tokens)
    .bind(peak_of(seen))
    .bind(tools_of(seen))
    .bind(id)
    .execute(pool)
    .await;
    match written {
        Ok(done) if done.rows_affected() == 1 => append_run_events(pool, id, seen).await,
        Ok(_) => {}
        Err(error) => tracing::warn!(
            run_id = id,
            %error,
            "could not record the stream of a run cut off before its result"
        ),
    }
}

/// Terminates an in-flight run: aborts its task (which, via `kill_on_drop`, kills the CLI process)
/// and records `status`. Removing the entry from the handle map is the atomic arbiter when several
/// termination reasons race (user cancel, timeout, or Chunk 3's §8.4 approval-pause): whoever removes
/// it first sets the final status; a later reason finds it gone and no-ops. Returns true iff this call
/// terminated the run. Factored out so Chunk 3's `pending_approval` path reuses the same arbiter.
///
/// The handle arbitrates between terminators, but it does not arbitrate against the run's own body:
/// `abort()` takes effect where the future is next dropped, so a body already past its last `.await`
/// keeps running, and a body that has just written its own terminal status is still registered until
/// its `Registration` drops. The status write below is therefore compare-and-set, and `true` keeps
/// meaning "this call terminated the task" — not "the run ended up in `status`". A caller that needs
/// the latter reads the row back; today none does.
pub async fn finalize_termination(state: &AppState, id: i64, status: &str) -> bool {
    let handle = state.run_handles.lock().unwrap().remove(&id);
    match handle {
        Some(h) => {
            // Taken BEFORE the abort. The task's `Registration` removes this entry when it drops, and
            // dropping it is exactly what the abort does — read afterwards, the stream the run wrote
            // is already gone, which is how every paused run came to leave no transcript.
            let stream = state
                .run_tails
                .lock()
                .ok()
                .and_then(|tails| tails.get(&id).cloned());
            h.abort();
            let now = chrono::Utc::now().to_rfc3339();
            // First writer wins: no rows means the run finalised itself while this call was on its
            // way, which is an outcome, not a failure — nothing to retry and nothing to report.
            let result = sqlx::query(
                "UPDATE runs SET status = ?, completed_at = ? WHERE id = ? AND status = 'running'",
            )
            .bind(status)
            .bind(&now)
            .bind(id)
            .execute(&state.pool)
            .await;
            warn_on_terminal_write_err(&result, id, status);
            // Only for the terminator that won. No rows means the body finished on its own and wrote
            // its own stdout and usage, read off a real `result` — those are the ones to keep.
            let won = matches!(&result, Ok(done) if done.rows_affected() == 1);
            if won && let Some(stream) = stream {
                let seen = stream
                    .lock()
                    .map(|shared| shared.clone())
                    .unwrap_or_default();
                record_the_cut_stream(&state.pool, id, &seen).await;
            }
            // A routed run something stopped — the user's cancel, or the daemon's hook ending a run
            // that kept probing denied actions (`failed`) — tells the router `error`
            // (`route_advice::report_stopped` argues why not `fail`). Only after this write won: the
            // aborted body's own run-end report needs the same CAS, so exactly one of the two
            // speaks. Only an ending — a pause resumes, and the run's later end reports for it.
            if won
                && ends_the_run(status)
                && let Some(router) = state.runner.router()
            {
                crate::models::route_advice::report_stopped(&state.pool, router, id).await;
            }
            // The run is over; anything it queued and never started goes with it (spec §7). After
            // the status write, because this is a consequence of the run ending — and best-effort,
            // because the run IS terminated either way and a queue row that outlives its run is a
            // stale request a human can cancel, not a broken run.
            //
            // Only when this call won the status write. A lost CAS means the body already wrote its
            // own ending — usually `completed` — and a completed run's queued merge is the work it
            // was asked to deliver, not a leftover: a late cancel must not take it away.
            if won
                && ends_the_run(status)
                && let Err(error) = crate::execution::vcs::cancel_for_run(&state.pool, id).await
            {
                tracing::warn!(run_id = id, %error, "could not cancel the run's queued vcs requests");
            }
            // Every terminator here stops the run before its `result`, the only line that carries a
            // cost, so every one of them leaves a run that spent money and reported none — the pause
            // included. It was left out once, on the belief that the resume "carries the real cost
            // for the whole session". It does not: a resume reports its own invocation and nothing
            // before it. Run 900475 resumed a session of 31 turns and reported 5 turns and $0.12, and
            // every total that reads `runs` — the budget, the ablation — lost the paused half.
            // `ends_the_run` still decides the queue above, where the pause must stay out; it does
            // not decide this.
            if won || ends_the_run(status) {
                record_time_approx_cost(&state.pool, id).await;
            }
            true
        }
        None => false,
    }
}

pub async fn cancel_run(State(state): State<AppState>, Path(id): Path<i64>) -> StatusCode {
    // Uncancellable: `finalize_termination` removes the handle and kills the process before it
    // writes the status, so a request dropped on that write leaves a `running` row nothing can
    // reach — the handle is gone, so a second `/cancel` answers 404 and the GC never collects it.
    match crate::door::uncancellable(
        async move { finalize_termination(&state, id, "cancelled").await },
    )
    .await
    {
        Ok(true) => StatusCode::OK,
        Ok(false) => StatusCode::NOT_FOUND,
        Err(status) => status,
    }
}

/// A run recovered from orphaned state: (id, project_id, chat_id, mode)
pub(super) type ReconcileRow = (i64, Option<String>, Option<String>, Option<String>);

/// Marks every run still `"running"` as `"interrupted"` — called once at startup to recover from a
/// daemon crash that left in-flight runs' rows stuck (spec §3.2). Returns how many rows it changed.
pub async fn reconcile_orphaned_runs(pool: &sqlx::SqlitePool) -> Result<u64, sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    let reconciled: Vec<ReconcileRow> = sqlx::query_as(
        "UPDATE runs SET status = 'interrupted', completed_at = ? WHERE status = 'running'
         RETURNING id, project_id, chat_id, mode",
    )
    .bind(&now)
    .fetch_all(pool)
    .await?;
    for (id, project_id, chat_id, mode) in &reconciled {
        let _ = crate::feed::append(
            pool,
            project_id.as_deref(),
            "run_interrupted",
            "run interrupted during startup recovery",
            Some(*id),
            Some(&crate::feed::run_subject(pool, *id).await),
        )
        .await;
        // A chat turn the restart cut says so in its conversation. A failed post never fails the
        // reconcile: the run is already `interrupted` and the notice is only a courtesy.
        if let (Some(chat_id), Some("assistant")) = (chat_id.as_deref(), mode.as_deref())
            && let Err(error) = crate::chats::notices::post_system(
                pool,
                chat_id,
                crate::chats::notices::KIND_RESTART,
                *id,
                crate::chats::notices::RESTART_NOTICE,
            )
            .await
        {
            tracing::warn!(
                run_id = *id,
                %error,
                "could not tell a conversation its turn was cut by the restart"
            );
        }
    }
    Ok(reconciled.len() as u64)
}

/// Marks every `"awaiting_approval"` run whose action-approval proposal is gone or already decided
/// as `"interrupted"` — the pause's counterpart to `reconcile_orphaned_runs`, run once at startup.
/// Returns how many rows it changed.
///
/// Such a run is unreachable, not merely idle: both `/approve` and `/reject` start from the pending
/// proposal row, so with none there is no input left that can move it. It is also load-bearing:
/// the concurrency sweep spares `awaiting_approval`, so one strand holds a slot for good, and the
/// worktree GC only collects terminal runs.
/// A run holding a *pending* proposal is resumable by design — the NOT EXISTS leaves it alone.
///
/// The doors that created these strands are closed, so this only heals rows predating that fix; the
/// pass stays because a strand is permanent and invisible otherwise. Worktrees are left to the GC,
/// which collects `interrupted` on its own — no removal here.
pub async fn reconcile_stranded_approvals(pool: &sqlx::SqlitePool) -> Result<u64, sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    // Single statement: the status guard and the proposal check must observe one snapshot, or an
    // approval landing mid-pass would be overwritten by a decision taken before it existed.
    let reconciled: Vec<(i64, Option<String>)> = sqlx::query_as(
        "UPDATE runs SET status = 'interrupted', completed_at = ?
         WHERE status = 'awaiting_approval'
           AND NOT EXISTS (
               SELECT 1 FROM proposals
               WHERE proposals.run_id = runs.id
                 AND proposals.kind = 'action-approval'
                 AND proposals.status = 'pending')
         RETURNING id, project_id",
    )
    .bind(&now)
    .fetch_all(pool)
    .await?;
    for (id, project_id) in &reconciled {
        let _ = crate::feed::append(
            pool,
            project_id.as_deref(),
            "run_interrupted",
            "run interrupted during startup recovery: its approval request no longer exists",
            Some(*id),
            Some(&crate::feed::run_subject(pool, *id).await),
        )
        .await;
    }
    Ok(reconciled.len() as u64)
}

/// How long a finished run keeps the bulk it produced.
///
/// A run's transcript is stored twice — the whole stream as `runs.stdout`, and again line by line
/// in `run_events`, which migration 0035 then indexes for search. Nothing removed any of it, so the
/// three copies grew for as long as the daemon was ever used. Measured on one lightly-used install:
/// 4.9 MB of `stdout` and 4.1 MB of `run_events` from 87 runs, one transcript alone a megabyte.
///
/// The row itself is NOT deleted, and that is the whole design. What costs is the transcript; what
/// people look at months later is the metadata — what ran, when, whether it passed, what it cost —
/// and that is a couple of hundred bytes a run. So a finished run keeps its history forever and
/// loses its transcript on a window, rather than the row disappearing out from under a feed entry,
/// a job item or a proposal that still points at it.
pub const DEFAULT_TRANSCRIPT_RETENTION_DAYS: i64 = 30;

/// The window, overridable for an operator who wants a different one.
///
/// An environment variable rather than a config file, matching `NUCLEOS_WORKTREE_RETENTION_HOURS`
/// in `worktree.rs`: runs are not a pillar, and a knob nobody has yet asked to turn does not earn
/// a settings file of its own.
pub(super) fn transcript_retention_days() -> i64 {
    std::env::var("NUCLEOS_TRANSCRIPT_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_TRANSCRIPT_RETENTION_DAYS)
}

/// What one retention pass removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PrunedTranscripts {
    /// Runs whose transcript columns were emptied.
    pub runs: u64,
    /// Rows removed from `run_events` — and, through migration 0035's delete trigger, the terms
    /// they had put in the search index.
    pub events: u64,
}

/// Empties the transcript of every finished run past the window, leaving the row and its metadata.
///
/// Only terminal runs, read from [`TERMINAL_RUN_STATUSES`] rather than spelled here: a run still
/// `running` or paused at `awaiting_approval` is going to write more, and stripping it mid-flight
/// would delete a transcript while its author still holds the file. The `IN` clause's placeholders
/// are generated from the constant's length and every status bound, for the reason
/// `vcs::reap_requests_of_ended_runs` gives at length: a status is data.
pub async fn prune_transcripts(
    pool: &sqlx::SqlitePool,
    retain_days: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<PrunedTranscripts> {
    if retain_days <= 0 {
        // Zero would empty every transcript on the machine at the next sweep, which is not a
        // retention policy but a typo with a plausible-looking value. Refusing to act is the safe
        // reading — the same call `web::prune` makes.
        return Ok(PrunedTranscripts::default());
    }

    // Computed here rather than with SQLite's `datetime('now', '-N days')`, and the difference is
    // not stylistic — see `web::prune`, which pays for this lesson in full. `completed_at` is
    // RFC 3339 (`2026-08-01T12:00:00+00:00`), `datetime()` returns `2026-08-01 12:00:00`, and they
    // are compared as TEXT: within the cutoff's own day `T` (0x54) sorts after the space (0x20), so
    // a row hours too old compares as newer and survives every sweep for ever.
    // `transcript_retention_is_exact_at_the_boundary` is what fails if this goes back to
    // `datetime()`; the coarse test beside it does not.
    let cutoff = (now - chrono::Duration::days(retain_days)).to_rfc3339();
    let placeholders = vec!["?"; TERMINAL_RUN_STATUSES.len()].join(", ");
    // `COALESCE(completed_at, created_at)` because a terminal row with no completion stamp is a row
    // some older path left half-written; ageing it from when it was created is what keeps it from
    // being immortal.
    let past_the_window =
        format!("status IN ({placeholders}) AND COALESCE(completed_at, created_at) < ?");

    // `AssertSqlSafe` because sqlx otherwise takes only `&'static str`. The sole interpolated thing
    // is a row of `?` generated from a constant's length — every status and the cutoff are bound —
    // so nothing caller-supplied reaches the SQL text (same justification as
    // `vcs::reap_requests_of_ended_runs`).
    //
    // Events first. Between the two statements the row still says it holds a transcript, so a crash
    // in the gap leaves work to redo rather than a run that claims to have events it no longer has.
    let mut delete = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM run_events
          WHERE run_id IN (SELECT id FROM runs WHERE {past_the_window})"
    )));
    for status in TERMINAL_RUN_STATUSES {
        delete = delete.bind(status);
    }
    let events = delete.bind(&cutoff).execute(pool).await?.rows_affected();

    // The `IS NOT NULL` guard is what makes the count mean something: without it every sweep
    // rewrites every old row for ever and reports them all as freshly pruned.
    // Assistant turns are exempt, and only from THIS half. A turn is a run whose `stdout` holds the
    // reply rather than a stream, and the shell rebuilds a conversation out of those replies
    // (`GET /assistant/{chat}` selects `stdout AS answer`) — so emptying the column would give the
    // app a chat history that erases itself a month at a time. Their events are pruned above with
    // everyone else's, which is where their bulk actually is.
    let mut update = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE runs SET stdout = NULL, stderr = NULL, gate_output = NULL
          WHERE {past_the_window}
            AND (mode IS NULL OR mode <> 'assistant')
            AND (stdout IS NOT NULL OR stderr IS NOT NULL OR gate_output IS NOT NULL)"
    )));
    for status in TERMINAL_RUN_STATUSES {
        update = update.bind(status);
    }
    let runs = update.bind(&cutoff).execute(pool).await?.rows_affected();

    Ok(PrunedTranscripts { runs, events })
}

/// How often retention runs while the daemon is up.
///
/// Hourly rather than tied to a read, for the reason `main.rs` gives the web cache: a transcript
/// nobody opens again must still expire, or "30 days" means "30 days after the last time anyone
/// looked". Once at startup too, because a daemon that only ever runs for an hour at a time would
/// otherwise never reach a sweep at all — the lesson `triage::run_triage_loop` already learned.
pub(super) const RETENTION_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

/// Retention for everything finished work leaves behind: a run's transcript and events, its
/// entries in the activity feed, councils that are over, queued VCS output tails, and briefing
/// traces. The sixth sweep consolidates durable gate and refused-action histories on its own
/// six-hour interval.
///
/// One loop rather than six, because they are the same sweep at different windows and splitting
/// them would mean six tasks waking on the same hour to take the same write lock. Every failure
/// is best-effort and logged: a sweep that could not run is a fuller disk later, not a reason to
/// take a daemon down now.
pub async fn run_retention_loop(state: AppState) {
    let mut ticker = tokio::time::interval(RETENTION_INTERVAL);
    let mut last_consolidated: Option<std::time::Instant> = None;
    loop {
        ticker.tick().await;
        let now = chrono::Utc::now();
        match prune_transcripts(&state.pool, transcript_retention_days(), now).await {
            Ok(pruned) if pruned == PrunedTranscripts::default() => {}
            Ok(pruned) => tracing::info!(
                runs = pruned.runs,
                events = pruned.events,
                "runs: transcripts past the retention window"
            ),
            Err(error) => tracing::warn!(%error, "runs: transcript retention sweep failed"),
        }
        match crate::feed::prune(&state.pool, crate::feed::retention_days(), now).await {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(pruned, "feed: entries past the retention window"),
            Err(error) => tracing::warn!(%error, "feed: retention sweep failed"),
        }
        // Unconditional, unlike the pillar's other work: a council is deleted whether or not
        // `~/.nucleos/council.yaml` still names a roster. Gating the sweep on the pillar being
        // configured would make a roster somebody removed the way their history stops being
        // collected.
        match crate::council::prune(&state.pool, crate::council::retention_days(), now).await {
            Ok(0) => {}
            Ok(pruned) => {
                tracing::info!(pruned, "council: deliberations past the retention window")
            }
            Err(error) => tracing::warn!(%error, "council: retention sweep failed"),
        }
        // The fourth window, and the one that empties rather than deletes: a queued operation's row
        // is what `action_grants.queued_request_id` points at, so only what git printed goes.
        match crate::execution::vcs::prune_output_tails(
            &state.pool,
            crate::execution::vcs::output_retention_days(),
            now,
        )
        .await
        {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(pruned, "vcs: outputs past the retention window"),
            Err(error) => tracing::warn!(%error, "vcs: retention sweep failed"),
        }
        match crate::brief::prune(&state.pool, crate::brief::retention_days(), now).await {
            Ok(0) => {}
            Ok(pruned) => {
                tracing::info!(
                    pruned,
                    "knowledge: briefing traces past the retention window"
                )
            }
            Err(error) => tracing::warn!(%error, "knowledge: trace retention sweep failed"),
        }
        match crate::knowledge::close_orphaned_working(&state.pool).await {
            Ok(0) => {}
            Ok(closed) => tracing::info!(closed, "knowledge: orphaned working findings closed"),
            Err(error) => tracing::warn!(%error, "knowledge: orphaned finding sweep failed"),
        }
        if crate::consolidate::due(last_consolidated, std::time::Instant::now()) {
            last_consolidated = Some(std::time::Instant::now());
            match crate::consolidate::run_pass(&state.pool, now).await {
                Ok(report) if report.is_quiet() => {}
                Ok(report) => tracing::info!(
                    created = report.created,
                    remeasured = report.remeasured,
                    pending = report.pending,
                    refused = report.refused,
                    successors = report.successors,
                    skipped_busy = report.skipped_busy,
                    "consolidate: pass finished"
                ),
                Err(error) => tracing::warn!(%error, "consolidate: pass failed"),
            }
        }
    }
}

#[cfg(test)]
mod terminal_write_ratchet {
    use std::path::{Path, PathBuf};

    /// Statements that take a run, a team item, a job item or a distill row to a state something is
    /// waiting on. Matched against the statement with its whitespace collapsed.
    const TERMINAL: [&str; 5] = [
        "UPDATE runs SET status",
        "UPDATE job_items",
        "UPDATE team_items",
        "UPDATE team_runs",
        "UPDATE distill_queue",
    ];

    /// `file:line` of terminal writes that may stay discarded, each with the reason. Empty on
    /// purpose: a dropped terminal write leaves a run `running` and its waiter stuck.
    const ALLOWED: [&str; 0] = [];

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "tests") {
                    continue;
                }
                rust_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name().is_some_and(|name| name != "tests.rs")
            {
                out.push(path);
            }
        }
    }

    /// Line numbers of every `let _ = sqlx::query(` in `source` whose statement
    /// is a terminal write, ignoring everything from a `#[cfg(test)] mod` on.
    fn discarded_terminal_writes(source: &str) -> Vec<usize> {
        let lines: Vec<&str> = source.lines().collect();
        let mut found = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if line.trim() == "#[cfg(test)]"
                && lines
                    .get(index + 1)
                    .is_some_and(|next| next.trim_start().starts_with("mod "))
            {
                break;
            }
            if !line.contains("let _ = sqlx::query(") {
                continue;
            }
            let statement = lines[index..lines.len().min(index + 4)]
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if TERMINAL.iter().any(|sql| statement.contains(sql)) {
                found.push(index + 1);
            }
        }
        found
    }

    #[test]
    fn the_detector_sees_a_discarded_terminal_write() {
        let bad = "fn f() {\n    let _ = sqlx::query(\n        \"UPDATE runs SET status = 'failed'\",\n    )\n    .execute(&pool)\n    .await;\n}\n";
        assert_eq!(discarded_terminal_writes(bad), [2]);
        let decorative = "let _ = sqlx::query(\"UPDATE runs SET session_id = ?\")";
        assert!(discarded_terminal_writes(decorative).is_empty());
        let in_tests = "fn f() {}\n#[cfg(test)]\nmod tests {\n let _ = sqlx::query(\"UPDATE runs SET status = 'x'\")\n}\n";
        assert!(discarded_terminal_writes(in_tests).is_empty());
    }

    /// A terminal write that fails must say so, and retry while the database is busy: use
    /// `storage::settle_write`. `let _ =` here once left runs `running` until the next start, team
    /// items and council seats waiting on a run that had ended, and `cost_usd` NULL.
    #[test]
    fn no_terminal_write_is_discarded() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        let mut offenders = Vec::new();
        for file in files {
            let source = std::fs::read_to_string(&file).unwrap();
            let relative = file
                .strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            for line in discarded_terminal_writes(&source) {
                let at = format!("{relative}:{line}");
                if !ALLOWED.contains(&at.as_str()) {
                    offenders.push(at);
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "terminal writes discarded with `let _ =`; use storage::settle_write: {offenders:#?}"
        );
    }
}
