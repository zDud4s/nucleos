//! §spec autopilot-juiz-resolve-bloqueios
//!
//! Spec E4: a completed worktree run whose gate failed, and the correction, which is one more
//! turn in the same conversation and tree, with a clock of its own (D6). This file owns the
//! questions about a whole LINEAGE that decide whether a correction may happen.

use sqlx::{SqliteConnection, SqlitePool};

use crate::judge::JudgeMode;
use crate::judge::resolve::{self, Asked, Event, Outcome, Subject};
use crate::state::AppState;

/// Spec D6, condition 2 (S2): any trace in the WHOLE lineage of an approved risky action or of
/// git, as the reason a correction goes to the owner, or `None`.
///
/// - ANY `action_grants` row, whatever its class or state: grants exist only for risky actions
///   the owner approved, and telling a "harmless" one apart is a judgement that cannot be made
///   safely. The owner knows what they approved and why.
/// - ANY `vcs_requests` row not `rejected` or `cancelled`: only those two say nothing happened.
///   A list of "bad" states would leave out `running`, `escalated`, `blocked` and whatever state
///   is added next. "Of the lineage" is three ways: by a lineage run's `run_id`; queued as
///   `Origin::Human` by an approval in the lineage (the grant's `queued_request_id`, already
///   refused by the first rule and kept so the two stay independent); and naming the tree's
///   branch anywhere in its arguments, which catches a human request with no run at all.
///
/// On a connection, because it is asked twice: before the transaction (fast refusal) and inside
/// it, after the correction row took the write lock, where it can no longer change until commit.
pub(crate) async fn lineage_trace_on(
    conn: &mut SqliteConnection,
    root: i64,
    branch: Option<&str>,
) -> sqlx::Result<Option<String>> {
    let granted: Option<i64> = sqlx::query_scalar(
        "SELECT g.proposal_id FROM action_grants g JOIN runs r ON r.id = g.run_id
         WHERE r.id = ?1 OR r.lineage_root_id = ?1
         LIMIT 1",
    )
    .bind(root)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(proposal_id) = granted {
        return Ok(Some(format!(
            "an action was approved in this lineage (proposal #{proposal_id})"
        )));
    }
    let request: Option<(i64, String)> = sqlx::query_as(
        "SELECT v.id, v.status FROM vcs_requests v
         WHERE v.status NOT IN ('rejected', 'cancelled')
           AND (v.run_id IN (SELECT id FROM runs WHERE id = ?1 OR lineage_root_id = ?1)
                OR v.id IN (SELECT g.queued_request_id FROM action_grants g
                            JOIN runs r ON r.id = g.run_id
                            WHERE (r.id = ?1 OR r.lineage_root_id = ?1)
                              AND g.queued_request_id IS NOT NULL)
                OR (?2 IS NOT NULL
                    AND EXISTS (SELECT 1 FROM json_each(v.args)
                                WHERE instr(json_each.value, ?2) > 0)))
         ORDER BY v.id
         LIMIT 1",
    )
    .bind(root)
    .bind(branch)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(request.map(|(id, status)| {
        format!("git request {id} of this lineage is {status}, so its work may already have left")
    }))
}

/// Spec D6, condition 8 (S8): whether any run of the lineage read a stranger's words. No
/// worktree run consults the mark today (spec §1.2), so the judge consults it itself: an
/// automatic continuation, with nobody watching, of a session that may have read a stranger is
/// exactly the case to hand to the owner.
pub(crate) async fn lineage_read_untrusted(pool: &SqlitePool, root: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM runs WHERE (id = ?1 OR lineage_root_id = ?1) AND read_untrusted = 1)",
    )
    .bind(root)
    .fetch_one(pool)
    .await
}

/// Spec D6, condition 7: the project's corrections in the last 24 hours. A rolling day rather
/// than a calendar one: no timezone to choose, and no midnight at which six can happen in an
/// hour.
pub(crate) async fn corrections_in_last_day_on(
    conn: &mut SqliteConnection,
    project_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM judge_corrections WHERE project_id = ? AND created_at >= ?",
    )
    .bind(project_id)
    .bind((now - chrono::Duration::hours(24)).to_rfc3339())
    .fetch_one(&mut *conn)
    .await
}

/// The E4's `judge_resolutions` row, written INLINE and not by `record_later`: the E4 is already
/// detached, there is no hook response to keep fast, and a reader right after it finds the row.
async fn settle(pool: &SqlitePool, row: resolve::ResolutionRow) {
    if let Err(error) = resolve::record(pool, &row).await {
        tracing::warn!(run_id = row.run_id, %error, "judge: could not record a resolution");
    }
}

/// Spec D7: the resolver's own line when a failed gate goes to the owner. Never an edit of
/// `worktree_gate_failed`, which went out before the E4 existed and which Telegram may already
/// have forwarded.
async fn needs_owner(pool: &SqlitePool, project_id: &str, run_id: i64, reason: &str) {
    let _ = crate::feed::append(
        pool,
        Some(project_id),
        "judge_needs_owner",
        &format!("run {run_id}'s gate failed and it comes back to you: {reason}"),
        Some(run_id),
        Some(&crate::feed::run_subject(pool, run_id).await),
    )
    .await;
}

/// Why `refusal_before_the_transaction` stopped a correction.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// A condition failed: the owner is told, with the reason.
    Owner(String),
    /// The resolver is no longer in enforce: nothing is corrected and no owner line is written
    /// (spec D11), as in observe.
    Silent,
}

/// Spec D6, conditions 2 and 5 to 8, read AFTER the judge answered (S3): they read state that
/// changes (the queue, the brakes, the switches, the ceiling), and reading them after a call that
/// may take 10 s shortens the window between reading and acting. `None` when all hold.
pub(crate) async fn refusal_before_the_transaction(
    state: &AppState,
    project_id: &str,
    root: i64,
    branch: Option<&str>,
) -> Option<Refusal> {
    let pool = &state.pool;
    // The resolver's setting once more: it was read before the judge, which may take 10 s, and the
    // owner may have moved it to observe or off meanwhile. Anything but enforce corrects nothing
    // and writes no owner line, as the observe path does (spec D11).
    if !matches!(
        crate::autopilot::autopilot_judge_resolve_mode(pool, project_id).await,
        Ok(JudgeMode::Enforce)
    ) {
        return Some(Refusal::Silent);
    }
    // 5: Active NOW. The E4 may fire hours after the launch, and launching work on a project the
    // owner took out of Active would be less cautious than today.
    if !matches!(
        crate::autopilot::project_mode(pool, project_id).await,
        Ok(crate::autopilot::Mode::Active)
    ) {
        return Some(Refusal::Owner("the project is not in Active".to_owned()));
    }
    // 5: the scheduler's own door, budget then quota. A correction is a launch.
    if let crate::budget::BudgetDecision::Pause { reason, .. } =
        crate::quota::permits_new_run(state, chrono::Utc::now()).await
    {
        return Some(Refusal::Owner(format!(
            "the brake on new runs is on: {reason}"
        )));
    }
    // 6 (B1): both switches, and an unreadable one counts as engaged, as the scheduler reads them.
    if crate::autopilot::kill_switch_engaged(pool)
        .await
        .unwrap_or(true)
    {
        return Some(Refusal::Owner("the kill switch is engaged".to_owned()));
    }
    if crate::autopilot::scoped_kill_engaged(pool, "project", project_id)
        .await
        .unwrap_or(true)
    {
        return Some(Refusal::Owner(
            "the project's kill switch is engaged".to_owned(),
        ));
    }
    let Ok(mut conn) = pool.acquire().await else {
        return Some(Refusal::Owner("the lineage could not be read".to_owned()));
    };
    // 2
    match lineage_trace_on(&mut conn, root, branch).await {
        Ok(Some(trace)) => return Some(Refusal::Owner(trace)),
        Ok(None) => {}
        Err(_) => return Some(Refusal::Owner("the lineage could not be read".to_owned())),
    }
    // 7: this correction would be one more.
    match corrections_in_last_day_on(&mut conn, project_id, chrono::Utc::now()).await {
        Ok(count) if count >= resolve::CORRECTIONS_PER_PROJECT_PER_DAY => {
            return Some(Refusal::Owner(format!(
                "the project reached {} corrections in the last day",
                resolve::CORRECTIONS_PER_PROJECT_PER_DAY
            )));
        }
        Ok(_) => {}
        Err(_) => {
            return Some(Refusal::Owner(
                "the day's corrections could not be counted".to_owned(),
            ));
        }
    }
    drop(conn);
    // 8
    match lineage_read_untrusted(pool, root).await {
        Ok(false) => None,
        _ => Some(Refusal::Owner(
            "a run of this task read text from outside the project".to_owned(),
        )),
    }
}

/// mode, job_id, project_id, lineage root, gate_output, successor_run_id.
type GateFailedRow = (
    String,
    Option<i64>,
    Option<String>,
    i64,
    Option<String>,
    Option<i64>,
);

/// Spec E4 (D6). Spawned by `spawn_run` after `observe_run`, only on the arm where the terminal
/// write won with `completed` and the gate `failed` (not `errored`: that is the gate that did not
/// run, not the code). Every way out is today's behaviour (the `worktree_gate_failed` line already
/// out) plus, in enforce, one line of the resolver's own. Any failure falls back to the owner.
pub(crate) async fn after_gate_failed(state: AppState, run_id: i64, exit_code: i32) {
    let pool = &state.pool;
    let row: Option<GateFailedRow> = sqlx::query_as(
        "SELECT mode, job_id, project_id, COALESCE(lineage_root_id, id), gate_output, successor_run_id
         FROM runs WHERE id = ?",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    // 1. D2's static checks: a worktree run, outside jobs and outside the resolver's lineages
    // (which is also condition 3: a finished run cannot join a resolution's lineage later).
    let Some((mode, None, Some(project_id), root, gate_output, successor)) = row else {
        return;
    };
    if mode != "worktree" || resolve::is_resolution_lineage(pool, root).await {
        return;
    }
    // The resolver's setting NOW, not the launch snapshot (S7): the snapshot exists so a run's
    // rules do not change halfway through; here the run is over, and a correction is a new launch
    // that may come hours later. An unreadable setting is `off`.
    let now = crate::autopilot::autopilot_judge_resolve_mode(pool, &project_id)
        .await
        .unwrap_or(JudgeMode::Off);
    if now == JudgeMode::Off {
        return;
    }
    // Conditions 1 and 4 before the judge, because they are MONOTONE (a successor is never unset,
    // a correction is never undone), so asking the judge first could only spend. Both are checked
    // again inside the transaction (`resume_for_correction`).
    let already_corrected: sqlx::Result<bool> =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM judge_corrections WHERE root_run_id = ?)")
            .bind(root)
            .fetch_one(pool)
            .await;
    // Fail closed on a failed read (no correction), but say so: "already corrected" would be a
    // claim about the lineage that nobody checked.
    let held_back = match (successor.is_some(), already_corrected) {
        (true, _) => Some("the run handed off to a successor, which is working in its tree"),
        (false, Ok(true)) => {
            Some("this task was already corrected once, and a lineage is corrected at most once")
        }
        (false, Err(_)) => Some(
            "whether this task was already corrected could not be read, so it is not corrected",
        ),
        (false, Ok(false)) => None,
    };
    if let Some(reason) = held_back {
        if now == JudgeMode::Enforce {
            needs_owner(pool, &project_id, run_id, reason).await;
        }
        return;
    }
    // 2. The judge, first (S3).
    let asked = Asked {
        run_id,
        lineage_root_id: root,
        event: Event::GateFailed,
        event_ref: None,
        project_id: Some(project_id.clone()),
        machine_root: state.machine_config_root.clone(),
        subject: Subject::Gate {
            exit_code,
            output: gate_output.unwrap_or_default(),
        },
    };
    let row = resolve::ask(pool, &state.judge, &asked).await;
    let opinion = row.judge_outcome;
    if now == JudgeMode::Observe {
        settle(pool, row.settled(Outcome::Owner, false)).await;
        return;
    }
    match opinion {
        Some(Outcome::Correction) => {}
        // D1: no answer is today's outcome, and today says nothing more than the gate's line.
        None => {
            settle(pool, row.settled(Outcome::Owner, false)).await;
            return;
        }
        Some(_) => {
            needs_owner(
                pool,
                &project_id,
                run_id,
                &resolve::phrase(Outcome::Owner, row.p),
            )
            .await;
            settle(pool, row.settled(Outcome::Owner, false)).await;
            return;
        }
    }
    // 3. Conditions 2 and 5 to 8, outside any transaction; failing one opens none.
    let branch: Option<String> = sqlx::query_scalar(
        "SELECT branch FROM worktrees WHERE owner_kind = 'run' AND owner_id = ? AND removed_at IS NULL",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    match refusal_before_the_transaction(&state, &project_id, root, branch.as_deref()).await {
        None => {}
        Some(Refusal::Owner(reason)) => {
            needs_owner(pool, &project_id, run_id, &reason).await;
            settle(pool, row.settled(Outcome::Owner, false)).await;
            return;
        }
        Some(Refusal::Silent) => {
            settle(pool, row.settled(Outcome::Owner, false)).await;
            return;
        }
    }
    // 4. The transaction, whose first write is the correction row.
    match crate::runs::resume_for_correction(&state, run_id, exit_code).await {
        Ok(correction) => {
            let phrase = resolve::phrase(Outcome::Correction, row.p);
            let mut settled = row.settled(Outcome::Correction, true);
            settled.correction_run_id = Some(correction);
            settle(pool, settled).await;
            let _ = crate::feed::append(
                pool,
                Some(&project_id),
                "judge_correction_started",
                &format!("correcting run {run_id}'s failed gate as run {correction} ({phrase})"),
                Some(correction),
                Some(&crate::feed::run_subject(pool, correction).await),
            )
            .await;
        }
        Err(refusal) => {
            needs_owner(pool, &project_id, run_id, &refusal.reason()).await;
            settle(pool, row.settled(Outcome::Owner, false)).await;
        }
    }
}

/// How long a `completed` correction run may still be handing off to a successor, before the sweep
/// takes it as the end of the chain (spec D6/D7). Every other terminal status is final at once.
const COMPLETED_GRACE: chrono::Duration = chrono::Duration::seconds(60);

/// Whether a run completed so recently that its handoff may not have written the successor yet.
/// A missing or unreadable `completed_at` is not recent: the sweep cannot wait on it forever.
fn completed_within_grace(completed_at: Option<&str>) -> bool {
    completed_at
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .is_some_and(|at| chrono::Utc::now() - at.with_timezone(&chrono::Utc) < COMPLETED_GRACE)
}

/// Spec D6/D7: says, once, every correction whose chain ended any way but `completed`. The owner
/// has to know the automatic turn did not finish, whatever the reason. The chain's LATEST run
/// decides: a parked correction goes on in its resume, a handed-off one in its successor, and both
/// carry the lineage. A chain that completed with its gate failing already has its
/// `worktree_gate_failed` line and is never corrected again (condition 4); it is marked without a
/// line, so the sweep never reads it again.
pub(crate) async fn report_ended_corrections(pool: &SqlitePool) {
    let open: Vec<(i64, i64, String, i64, String, Option<String>)> = match sqlx::query_as(
        "SELECT c.id, c.origin_run_id, c.project_id, r.id, r.status, r.completed_at
         FROM judge_corrections c
         JOIN runs r ON r.id = (SELECT MAX(id) FROM runs
                                WHERE (id = c.root_run_id OR lineage_root_id = c.root_run_id)
                                  AND id >= c.correction_run_id)
         WHERE c.end_reported_at IS NULL AND c.correction_run_id IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    {
        Ok(open) => open,
        Err(error) => {
            tracing::warn!(%error, "judge: could not read the open corrections");
            return;
        }
    };
    for (id, origin, project_id, latest, status, completed_at) in open {
        let failed = match status.as_str() {
            // `completed` is only final once `spawn_handoff_if_needed` has had time to set the
            // successor: read at the instant of completion, the chain's latest run is still this
            // one, and a successor that fails afterwards would never be reported (spec D6/D7).
            "completed" if completed_within_grace(completed_at.as_deref()) => continue,
            "completed" => false,
            "failed" | "timed_out" | "cancelled" | "interrupted" => true,
            // Still going: running, parked, or superseded with its resume not yet visible.
            _ => continue,
        };
        if let Err(error) = report_one(pool, id, origin, &project_id, latest, &status, failed).await
        {
            tracing::warn!(%error, correction = id, "judge: could not report an ended correction; the next tick retries");
        }
    }
}

/// One correction, one transaction: the mark and the line commit together or not at all. The
/// guard `end_reported_at IS NULL` makes a second sweep (or a concurrent one) write nothing.
async fn report_one(
    pool: &SqlitePool,
    id: i64,
    origin: i64,
    project_id: &str,
    latest: i64,
    status: &str,
    failed: bool,
) -> sqlx::Result<()> {
    // Before `begin`: `run_subject` reads on its own connection, and the test pool has one.
    let subject = crate::feed::run_subject(pool, latest).await;
    let mut tx = pool.begin().await?;
    let marked = sqlx::query(
        "UPDATE judge_corrections SET end_reported_at = ? WHERE id = ? AND end_reported_at IS NULL",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if marked.rows_affected() == 1 && failed {
        crate::feed::append_on(
            &mut tx,
            Some(project_id),
            "judge_correction_failed",
            &format!("the correction of run {origin} did not finish: run {latest} ended {status}"),
            Some(latest),
            Some(&subject),
        )
        .await?;
    }
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    /// A root, its handoff successor, and a stranger; returns (root, successor).
    async fn lineage(pool: &SqlitePool) -> (i64, i64) {
        let insert = |root: Option<i64>| {
            let pool = pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO runs (project_id, prompt, status, mode, lineage_root_id, created_at)
                     VALUES ('p', 'x', 'completed', 'worktree', ?, '2026-09-27T00:00:00Z')",
                )
                .bind(root)
                .execute(&pool)
                .await
                .unwrap()
                .last_insert_rowid()
            }
        };
        let root = insert(None).await;
        let successor = insert(Some(root)).await;
        insert(None).await;
        (root, successor)
    }

    async fn request(pool: &SqlitePool, run_id: Option<i64>, status: &str, args: &str) {
        sqlx::query(
            "INSERT INTO vcs_requests (op, args, project_id, project_root, origin, run_id, status, created_at)
             VALUES ('push', ?, 'p', 'C:/x', ?, ?, ?, '2026-09-27T00:00:00Z')",
        )
        .bind(args)
        .bind(if run_id.is_some() { "run" } else { "human" })
        .bind(run_id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn trace(pool: &SqlitePool, root: i64) -> Option<String> {
        lineage_trace_on(
            &mut pool.acquire().await.unwrap(),
            root,
            Some("nucleos/run-1"),
        )
        .await
        .unwrap()
    }

    /// Condition 2 (S2): ANY grant in the lineage, whatever its class or state, sends the case to
    /// the owner, including one granted to a successor and not to the run that failed.
    #[tokio::test]
    async fn any_grant_in_the_lineage_goes_to_the_owner() {
        let pool = pool().await;
        let (root, successor) = lineage(&pool).await;
        assert_eq!(trace(&pool, root).await, None);
        sqlx::query(
            "INSERT INTO action_grants (run_id, tool_name, action_class, proposal_id, created_at, consumed_at)
             VALUES (?, 'Bash', 'read-local', 42, '2026-09-27T00:00:00Z', '2026-09-27T00:00:01Z')",
        )
        .bind(successor)
        .execute(&pool)
        .await
        .unwrap();
        assert!(trace(&pool, root).await.unwrap().contains("42"));
    }

    /// Condition 2 (S2): every git request of the lineage that was not rejected or cancelled,
    /// by the lineage's runs or naming the tree's branch (a human request carries no run_id),
    /// sends the case to the owner; `rejected` and `cancelled` say nothing happened.
    #[tokio::test]
    async fn any_git_request_that_may_have_acted_goes_to_the_owner() {
        for status in [
            "awaiting_approval",
            "queued",
            "running",
            "succeeded",
            "failed",
            "blocked",
            "escalated",
            "interrupted",
        ] {
            let pool = pool().await;
            let (root, successor) = lineage(&pool).await;
            request(
                &pool,
                Some(successor),
                status,
                "{\"op\":\"push\",\"remote\":\"origin\",\"branch\":\"main\"}",
            )
            .await;
            assert!(
                trace(&pool, root).await.unwrap().contains(status),
                "{status}"
            );
        }
        for status in ["rejected", "cancelled"] {
            let pool = pool().await;
            let (root, successor) = lineage(&pool).await;
            request(
                &pool,
                Some(successor),
                status,
                "{\"op\":\"push\",\"remote\":\"origin\",\"branch\":\"main\"}",
            )
            .await;
            assert_eq!(trace(&pool, root).await, None, "{status}");
        }
        let pool = pool().await;
        let (root, _) = lineage(&pool).await;
        request(
            &pool,
            None,
            "succeeded",
            "{\"op\":\"push\",\"remote\":\"origin\",\"branch\":\"nucleos/run-1\"}",
        )
        .await;
        assert!(
            trace(&pool, root).await.is_some(),
            "a human request naming the tree's branch"
        );
        // A refspec or a remote-qualified name carries the branch without being equal to it.
        for arg in ["nucleos/run-1:main", "origin/nucleos/run-1"] {
            let pool = self::pool().await;
            let (root, _) = lineage(&pool).await;
            request(
                &pool,
                None,
                "queued",
                &format!("{{\"op\":\"push\",\"refspec\":\"{arg}\"}}"),
            )
            .await;
            assert!(trace(&pool, root).await.is_some(), "{arg}");
        }
    }

    /// Condition 8 (S8): any run of the lineage that read a stranger's words.
    #[tokio::test]
    async fn a_lineage_that_read_untrusted_text_is_seen() {
        let pool = pool().await;
        let (root, successor) = lineage(&pool).await;
        assert!(!lineage_read_untrusted(&pool, root).await.unwrap());
        sqlx::query("UPDATE runs SET read_untrusted = 1 WHERE id = ?")
            .bind(successor)
            .execute(&pool)
            .await
            .unwrap();
        assert!(lineage_read_untrusted(&pool, root).await.unwrap());
    }

    /// Condition 7: corrections of the project in the last day.
    #[tokio::test]
    async fn the_daily_ceiling_counts_one_project_one_day() {
        let pool = pool().await;
        let now: chrono::DateTime<chrono::Utc> = "2026-09-27T12:00:00Z".parse().unwrap();
        for (root, project, at) in [
            (1, "p", "2026-09-27T01:00:00+00:00"),
            (2, "p", "2026-09-26T13:00:00+00:00"),
            (3, "p", "2026-09-26T11:00:00+00:00"),
            (4, "q", "2026-09-27T01:00:00+00:00"),
        ] {
            sqlx::query(
                "INSERT INTO judge_corrections (root_run_id, origin_run_id, project_id, created_at) VALUES (?, ?, ?, ?)",
            )
            .bind(root)
            .bind(root)
            .bind(project)
            .bind(at)
            .execute(&pool)
            .await
            .unwrap();
        }
        let count = corrections_in_last_day_on(&mut pool.acquire().await.unwrap(), "p", now)
            .await
            .unwrap();
        assert_eq!(count, 2);
    }

    /// Spec D6: `judge_correction_failed` for every end of a correction that is not `completed`
    /// (failed, a launch that failed, which is also `failed`, timed out, cancelled, interrupted by
    /// a restart), exactly once; nothing for `completed`, `superseded` (a person approved its park
    /// and the work goes on) or a correction still live.
    #[tokio::test]
    async fn every_end_of_a_correction_but_completed_is_said_once() {
        let pool = pool().await;
        let statuses = [
            ("failed", true),
            ("timed_out", true),
            ("cancelled", true),
            ("interrupted", true),
            ("completed", false),
            ("superseded", false),
            ("running", false),
            ("awaiting_approval", false),
        ];
        for (root, (status, _)) in statuses.iter().enumerate() {
            let correction = sqlx::query(
                "INSERT INTO runs (project_id, prompt, status, mode, stderr, lineage_root_id, created_at)
                 VALUES ('p', 'x', ?, 'worktree', 'launch failed', ?, '2026-09-27T00:00:00Z')",
            )
            .bind(status)
            .bind(root as i64 + 1000)
            .execute(&pool)
            .await
            .unwrap()
            .last_insert_rowid();
            sqlx::query(
                "INSERT INTO judge_corrections (root_run_id, origin_run_id, correction_run_id, project_id, created_at)
                 VALUES (?, ?, ?, 'p', '2026-09-27T00:00:00Z')",
            )
            .bind(root as i64 + 1000)
            .bind(root as i64 + 1000)
            .bind(correction)
            .execute(&pool)
            .await
            .unwrap();
        }

        report_ended_corrections(&pool).await;
        report_ended_corrections(&pool).await;

        let said: Vec<String> = sqlx::query_scalar(
            "SELECT summary FROM feed WHERE kind = 'judge_correction_failed' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let expected: Vec<&str> = statuses
            .iter()
            .filter(|(_, said)| *said)
            .map(|(status, _)| *status)
            .collect();
        assert_eq!(said.len(), expected.len(), "once each: {said:?}");
        for status in expected {
            assert!(
                said.iter().any(|summary| summary.ends_with(status)),
                "{status}"
            );
        }
    }

    /// Spec D6/D7: a correction run that has only just completed may still be handing off, so the
    /// sweep leaves it; once the grace has passed with no successor, the chain is over and is
    /// marked without a line.
    #[tokio::test]
    async fn a_correction_that_just_completed_is_not_read_as_finished() {
        let pool = pool().await;
        let mut ids = Vec::new();
        for (root, completed_at) in [
            (2000_i64, chrono::Utc::now()),
            (2001_i64, chrono::Utc::now() - chrono::Duration::minutes(2)),
        ] {
            let correction = sqlx::query(
                "INSERT INTO runs (project_id, prompt, status, mode, lineage_root_id, created_at, completed_at)
                 VALUES ('p', 'x', 'completed', 'worktree', ?, '2026-09-27T00:00:00Z', ?)",
            )
            .bind(root)
            .bind(completed_at.to_rfc3339())
            .execute(&pool)
            .await
            .unwrap()
            .last_insert_rowid();
            let id = sqlx::query(
                "INSERT INTO judge_corrections (root_run_id, origin_run_id, correction_run_id, project_id, created_at)
                 VALUES (?, ?, ?, 'p', '2026-09-27T00:00:00Z')",
            )
            .bind(root)
            .bind(root)
            .bind(correction)
            .execute(&pool)
            .await
            .unwrap()
            .last_insert_rowid();
            ids.push(id);
        }

        report_ended_corrections(&pool).await;

        let mut reported = Vec::new();
        for id in &ids {
            let at: Option<String> =
                sqlx::query_scalar("SELECT end_reported_at FROM judge_corrections WHERE id = ?")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            reported.push(at);
        }
        assert!(reported[0].is_none(), "just completed");
        assert!(reported[1].is_some(), "completed 2 minutes ago");
        let lines: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'judge_correction_failed'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(lines, 0);
    }

    /// Spec D6: a correction parked and resumed goes on in a later run of its lineage; the sweep
    /// waits for THAT run, and says its failure once.
    #[tokio::test]
    async fn a_superseded_correction_whose_resume_fails_is_said_once() {
        let pool = pool().await;
        let run = |status: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO runs (project_id, prompt, status, mode, lineage_root_id, created_at)
                     VALUES ('p', 'x', ?, 'worktree', 500, '2026-09-27T00:00:00Z')",
                )
                .bind(status)
                .execute(&pool)
                .await
                .unwrap()
                .last_insert_rowid()
            }
        };
        let said = || async {
            sqlx::query_scalar::<_, String>(
                "SELECT summary FROM feed WHERE kind = 'judge_correction_failed'",
            )
            .fetch_all(&pool)
            .await
            .unwrap()
        };
        let correction = run("superseded").await;
        sqlx::query(
            "INSERT INTO judge_corrections (root_run_id, origin_run_id, correction_run_id, project_id, created_at)
             VALUES (500, 500, ?, 'p', '2026-09-27T00:00:00Z')",
        )
        .bind(correction)
        .execute(&pool)
        .await
        .unwrap();
        report_ended_corrections(&pool).await;
        let resume = run("running").await;
        report_ended_corrections(&pool).await;
        assert!(
            said().await.is_empty(),
            "superseded, then running: the correction is still going"
        );

        sqlx::query("UPDATE runs SET status = 'failed' WHERE id = ?")
            .bind(resume)
            .execute(&pool)
            .await
            .unwrap();
        report_ended_corrections(&pool).await;
        report_ended_corrections(&pool).await;
        assert_eq!(
            said().await,
            vec![format!(
                "the correction of run 500 did not finish: run {resume} ended failed"
            )]
        );
    }
}
