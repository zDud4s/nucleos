//! The correction loop for a reverted postgate culprit.

use super::*;

/// The name of the correction branch: `fix/<source>-<sha7>`, `source` being the branch the culprit
/// was merged from (`merge` when the queue has no record of it) with its `/` turned into `-`, so
/// that no ref ever has to live under another (`fix/feat` against `fix/feat/x`).
pub(super) fn fix_branch_name(source: Option<&str>, merge_sha: &str) -> String {
    let source = source.unwrap_or("merge").replace('/', "-");
    format!("fix/{source}-{}", merge_sha.get(..7).unwrap_or(merge_sha))
}

/// The revert is published: build `fix/...` on top of it and tell the owner, or free the state and
/// say that the revert stands without a branch.
pub(super) async fn fix_branch_step(
    executor: &Arc<Executor>,
    project_id: &str,
    project_root: &Path,
    state: &State,
    revert_sha: &str,
) -> Step {
    let pool = &executor.pool;
    let Some(merge_sha) = state.revert_merge_sha.as_deref() else {
        return Step::Failed("a revert is pending without its culprit".to_owned());
    };
    let deadline = Instant::now() + git_exec::OPERATION_TIMEOUT;
    let source = sqlx::query_scalar::<_, Option<String>>(
        "SELECT json_extract(args, '$.source') FROM vcs_requests \
         WHERE project_id = ? AND op = 'merge' AND status = 'succeeded' AND result_sha = ? \
         ORDER BY id DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(merge_sha)
    .fetch_optional(pool)
    .await
    .unwrap_or_default()
    .flatten();
    let branch = fix_branch_name(source.as_deref(), merge_sha);
    let target = &state.target;
    match git_exec::prepare_fix_branch(project_root, revert_sha, &branch, deadline).await {
        Ok(_) => {
            let ticket = ticket_note(state);
            let summary = format!(
                "post-merge gate on {target}: reverted culprit {} as {}{ticket}; correction \
                 branch {branch} is ready and a correction run will open on it.",
                short(merge_sha),
                short(revert_sha)
            );
            match verify_postgate::set_fix_branch(pool, project_id, merge_sha, &branch, &summary)
                .await
            {
                Ok(true) => Step::FixBranch { branch },
                Ok(false) => Step::Waiting {
                    sha: merge_sha.to_owned(),
                },
                Err(error) => Step::Failed(format!("cannot record the fix branch: {error}")),
            }
        }
        Err(reason) => {
            let summary = format!(
                "post-merge gate on {target}: the revert of {}{} is on {target} as {}, but the \
                 correction branch {branch} was not created ({reason}).",
                short(merge_sha),
                ticket_note(state),
                short(revert_sha)
            );
            settle_failed_revert(
                pool,
                project_id,
                merge_sha,
                &summary,
                reason,
                incident::RevertEnd::Failed,
            )
            .await
        }
    }
}

/// A prepared `fix/` branch still waiting for its correction run.
#[derive(sqlx::FromRow)]
pub(super) struct PendingCorrection {
    pub(super) project_id: String,
    pub(super) project_root: String,
    pub(super) target: String,
    pub(super) revert_merge_sha: Option<String>,
    pub(super) revert_sha: Option<String>,
    pub(super) fix_branch: String,
    pub(super) red_groups: String,
}

/// The task of a correction run: what was reverted, where the work stands, and how it ends.
pub(super) fn correction_prompt(row: &PendingCorrection) -> String {
    let groups: Vec<String> = serde_json::from_str(&row.red_groups).unwrap_or_default();
    let red = if groups.is_empty() {
        "unknown".to_owned()
    } else {
        groups.join(", ")
    };
    format!(
        "The post-merge gate on `{target}` went red and bisection pointed at merge {culprit}. The \
         daemon reverted it on `{target}` as {revert}, and your worktree starts on branch `{fix}`, \
         whose first commit reverts that revert, so the culprit's change is back in front of \
         you.\n\n\
         Failing gate groups: {red}.\n\n\
         Fix the change so the gate passes, then run `verify scope` and land it normally.",
        target = row.target,
        culprit = row.revert_merge_sha.as_deref().unwrap_or("(unknown)"),
        revert = row.revert_sha.as_deref().unwrap_or("(unknown)"),
        fix = row.fix_branch,
    )
}

/// Sets the correction run of `row`'s branch: its id once opened, `None` to give the claim back.
pub(super) async fn set_fix_run(
    pool: &sqlx::SqlitePool,
    row: &PendingCorrection,
    value: Option<i64>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE postgate_state SET fix_run_id = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND fix_branch = ?",
    )
    .bind(value)
    .bind(&row.project_id)
    .bind(&row.fix_branch)
    .execute(pool)
    .await
    .map(|_| ())
}

/// How many times a `fix/` branch's correction run is asked for after definitive failures.
pub(super) const CORRECTION_ATTEMPTS: u32 = 3;

/// The wait between two attempts on the same branch.
pub(super) const CORRECTION_BACKOFF: Duration = Duration::from_secs(10 * 60);

/// What the process remembers of one branch's failed attempts.
pub(super) struct Attempts {
    pub(super) count: u32,
    pub(super) last: Instant,
}

/// Failed attempts per `(project, fix branch)`. In memory on purpose: no migration. A restart forgets
/// the count, which only matters to a branch whose claim was given back: one that gave up keeps
/// `fix_run_id = 0` in its row and stays given up.
pub(super) static CORRECTION_TRIES: LazyLock<Mutex<HashMap<(String, String), Attempts>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn correction_tries() -> MutexGuard<'static, HashMap<(String, String), Attempts>> {
    CORRECTION_TRIES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn correction_key(row: &PendingCorrection) -> (String, String) {
    (row.project_id.clone(), row.fix_branch.clone())
}

/// Whether a branch may be claimed now: it never failed, or its backoff is over.
pub(super) fn may_try(entry: Option<&Attempts>, now: Instant) -> bool {
    entry.is_none_or(|attempts| now.duration_since(attempts.last) >= CORRECTION_BACKOFF)
}

/// Whether the claim is given back after the `count`-th definitive failure (false: give up).
pub(super) fn after_failure(count: u32) -> bool {
    count < CORRECTION_ATTEMPTS
}

/// Notes one more definitive failure of `row`'s branch and returns how many there have been.
pub(super) fn record_failure(row: &PendingCorrection) -> u32 {
    let mut tries = correction_tries();
    let attempts = tries.entry(correction_key(row)).or_insert(Attempts {
        count: 0,
        last: Instant::now(),
    });
    attempts.count += 1;
    attempts.last = Instant::now();
    attempts.count
}

/// `d` as the feed says it: whole minutes when it is a whole number of them, seconds otherwise.
pub(super) fn backoff_text(d: Duration) -> String {
    match d.as_secs() {
        60 => "1 minute".to_owned(),
        s if s > 60 && s.is_multiple_of(60) => format!("{} minutes", s / 60),
        1 => "1 second".to_owned(),
        s => format!("{s} seconds"),
    }
}

/// What the feed says comes next after a definitive failure. `retry`: attempts remain;
/// `released`: the claim went back to `None`. A claim that could not be released stays at 0, and
/// `open_corrections` only picks `fix_run_id IS NULL`, so nothing will try that branch again.
pub(super) fn correction_outcome(retry: bool, released: bool) -> String {
    match (retry, released) {
        (true, true) => format!("trying again in {}", backoff_text(CORRECTION_BACKOFF)),
        (true, false) => "the claim could not be released, so nothing will retry it".to_owned(),
        (false, _) => "gave up".to_owned(),
    }
}

/// Opens one correction run for every prepared `fix/` branch that has none, and returns their ids.
///
/// A branch is claimed (`fix_run_id = 0`) before its run is asked for, so a run is never attempted
/// twice at once. A full project or disk gives the claim back and the next pass asks again; any
/// other failure gives it back too, up to `CORRECTION_ATTEMPTS` times and `CORRECTION_BACKOFF`
/// apart, and the last one leaves the claim at 0 and says in the feed that it gave up. The count is
/// kept in this process, so a restart resets it only for a branch whose claim was given back; a
/// branch that gave up, or whose claim could not be released, keeps `fix_run_id = 0` across
/// restarts and is not picked again. The kill switch and the budget stop
/// it like any other run the daemon starts on its own; a switch that cannot be read stops it too.
pub async fn open_corrections(state: &crate::state::AppState) -> Vec<i64> {
    let pool = &state.pool;
    if crate::governance::autopilot::kill_switch_engaged(pool)
        .await
        .unwrap_or(true)
    {
        return Vec::new();
    }
    let rows = match sqlx::query_as::<_, PendingCorrection>(
        "SELECT p.project_id, a.project_root, p.target, p.revert_merge_sha, p.revert_sha, \
                p.fix_branch, p.red_groups \
         FROM postgate_state p JOIN autopilot_state a ON a.project_id = p.project_id \
         WHERE p.fix_branch IS NOT NULL AND p.fix_run_id IS NULL \
         ORDER BY p.project_id",
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "postgate: cannot look for correction branches");
            return Vec::new();
        }
    };
    // After the query, so an exhausted budget is only logged when a branch is actually waiting.
    if rows.is_empty() {
        return Vec::new();
    }
    if let crate::governance::budget::BudgetDecision::Pause { reason, .. } =
        crate::quota::permits_new_run(state, chrono::Utc::now()).await
    {
        tracing::info!(%reason, "budget exhausted; no correction run opened this tick");
        return Vec::new();
    }
    let mut opened = Vec::new();
    for row in rows {
        // A branch that failed definitively waits out its backoff before it is claimed again.
        if !may_try(
            correction_tries().get(&correction_key(&row)),
            Instant::now(),
        ) {
            continue;
        }
        let claimed = sqlx::query(
            "UPDATE postgate_state SET fix_run_id = 0, updated_at = CURRENT_TIMESTAMP \
             WHERE project_id = ? AND fix_branch = ? AND fix_run_id IS NULL",
        )
        .bind(&row.project_id)
        .bind(&row.fix_branch)
        .execute(pool)
        .await;
        match claimed {
            Ok(done) if done.rows_affected() == 1 => {}
            Ok(_) => continue,
            Err(error) => {
                tracing::warn!(
                    project = %row.project_id, %error,
                    "postgate: cannot claim a fix branch"
                );
                continue;
            }
        }
        let started = crate::execution::runs::create_correction_run(
            state,
            correction_prompt(&row),
            row.project_id.clone(),
            row.project_root.clone(),
            crate::execution::runs::Correction {
                base: row.fix_branch.clone(),
            },
        )
        .await;
        match started {
            Ok(run_id) => {
                correction_tries().remove(&correction_key(&row));
                if let Err(error) = set_fix_run(pool, &row, Some(run_id)).await {
                    tracing::warn!(
                        project = %row.project_id, run_id, %error,
                        "postgate: cannot record the correction run"
                    );
                }
                let _ = crate::feed::append(
                    pool,
                    Some(&row.project_id),
                    crate::verify::postgate::POSTGATE_RED_KIND,
                    &format!("correction run {run_id} opened on {}", row.fix_branch),
                    Some(run_id),
                    None,
                )
                .await;
                opened.push(run_id);
            }
            // Nothing was spent: give the claim back and ask again on a later pass.
            Err(
                crate::execution::runs::CreateRunError::Busy
                | crate::execution::runs::CreateRunError::NoRoomOnDisk(_),
            ) => {
                if let Err(error) = set_fix_run(pool, &row, None).await {
                    tracing::warn!(
                        project = %row.project_id, %error,
                        "postgate: cannot release a correction claim"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(
                    project = %row.project_id, ?error,
                    "postgate: the correction run could not be opened"
                );
                let count = record_failure(&row);
                let retry = after_failure(count);
                let released = if retry {
                    match set_fix_run(pool, &row, None).await {
                        Ok(()) => true,
                        Err(error) => {
                            tracing::warn!(
                                project = %row.project_id, %error,
                                "postgate: cannot release a correction claim"
                            );
                            false
                        }
                    }
                } else {
                    false
                };
                let outcome = correction_outcome(retry, released);
                let _ = crate::feed::append(
                    pool,
                    Some(&row.project_id),
                    crate::verify::postgate::POSTGATE_RED_KIND,
                    &format!(
                        "the correction run could not be opened: {error:?}; attempt {count} of \
                         {CORRECTION_ATTEMPTS}, {outcome}; the work is on {}",
                        row.fix_branch
                    ),
                    None,
                    None,
                )
                .await;
            }
        }
    }
    opened
}

/// The loop `main` spawns beside the gate worker, because opening a run needs an `AppState` and the
/// worker only holds an executor. With no `fix/` branch waiting, which is every project that has
/// not turned `revert_on_red` on, a pass reads one empty query and stops.
pub async fn run_correction_loop(state: crate::state::AppState) {
    let mut interval = tokio::time::interval(POSTGATE_POLL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        for run_id in open_corrections(&state).await {
            tracing::info!(run_id, "postgate: correction run opened");
        }
    }
}
