//! Ticket summaries, settlement, dismissal, ownership decisions, and waits.

use super::*;

/// What `wait_for` hands back: either the row's outcome, if the wait caught it before the deadline,
/// or its current in-flight status if not.
///
/// Serializable because it crosses the boundary Task 8 adds: an agent's blocking merge request gets
/// exactly this back as its HTTP response body, whether the queue answered inside the deadline or
/// not.
///
/// `status` is the same string the `status` column holds rather than an enum: `rejected` is in that
/// column's CHECK constraint even though nothing in this module writes it yet — `cancelled` was too
/// until `cancel_for_run` arrived — and a `Ticket` round-trips whichever one a row holds without this
/// module needing to know what it means.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: i64,
    pub status: String,
    pub result_sha: Option<String>,
    pub failure_reason: Option<String>,
}
/// One row as a queue listing shows it: what was asked, for which repository, by whom, and where it
/// got to.
///
/// A separate type from `Ticket` rather than a reuse of it, because the two answer different
/// questions. A ticket answers "how did MY request end" and needs the result; a listing answers
/// "what is this queue doing" and needs the operation and the project, which a ticket does not
/// carry — reusing it would produce a column of statuses attached to nothing.
///
/// `op` is the `op` column verbatim, not a parsed `Op`. Parsing can fail on a row written by an
/// older version or edited by hand, and one such row must not be able to fail the whole listing —
/// the listing is exactly where somebody would go to find out that a row is wrong.
/// `FromRow` rather than a positional tuple, for the reason `runs.rs` states: a tuple makes the
/// column-order-to-field-order correspondence load-bearing and invisible, and six of these seven
/// fields are `String`, so a swap would compile and pass.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RequestSummary {
    pub id: i64,
    pub op: String,
    pub project_id: String,
    /// What the queue locked on for this row, and therefore what the listing is grouped by.
    ///
    /// Carried ALONGSIDE `project_id` rather than instead of it. The project is the label a reader
    /// recognises; the key is the only thing that says whether two differently-labelled rows were
    /// competing for the same refs. Without it, a listing that contains another project's work
    /// reads as a bug in the listing rather than as the fact it is reporting.
    pub repo_key: String,
    pub origin: String,
    pub status: String,
    pub created_at: String,
    /// What a person needs to decide on a row, filled only by `waiting_on_a_person`.
    ///
    /// The history listing (`list`) leaves these at their defaults: it can return 200 rows and
    /// nobody decides anything from it. A Waiting card that showed only the fields above told the
    /// owner "merge #573 escalated" and nothing about which branches, which files, or whether an
    /// agent was already on it — the card asked for a decision it gave no grounds for.
    ///
    /// `source`/`target` are the merge's branches (absent for other operations).
    #[sqlx(default)]
    pub source: Option<String>,
    #[sqlx(default)]
    pub target: Option<String>,
    /// Git's own output, where a conflict names its files.
    #[sqlx(default)]
    pub output_tail: Option<String>,
    #[sqlx(default)]
    pub failure_reason: Option<String>,
    /// The run minted to resolve this conflict, and where that run got to.
    #[sqlx(default)]
    pub resolution_run_id: Option<i64>,
    #[sqlx(default)]
    pub resolution_status: Option<String>,
    /// This merge was itself a resolution's landing, so no agent will be minted for it.
    #[sqlx(default)]
    #[serde(default)]
    pub from_resolution: bool,
}
/// How many rows a listing returns at most.
///
/// Note what this table is: nothing prunes `vcs_requests`, so it is the permanent history of every
/// git operation this daemon has ever queued, not a snapshot of what is pending. A listing that hits
/// this cap therefore means "the daemon has been running a while" — it is not a finding, and this
/// cap is not a diagnostic. It exists only so one HTTP call cannot return an unbounded response.
///
/// When someone adds retention, or paging, this is where they start.
pub(super) const LIST_LIMIT: i64 = 200;
/// Newest first, optionally narrowed to one repository.
///
/// Newest first because the question a listing answers is almost always "what just happened", and a
/// caller reading a truncated oldest-first list would be reading history while missing the present.
///
/// **Narrowed by REPOSITORY, though the caller names a project.** The lock this queue takes is on
/// the repository, so two projects pointing at one checkout share a queue and compete for the same
/// refs. A listing that filtered on `project_id` would hand each of them a view with the other's
/// operations missing — which is the single fact about this queue a listing must not hide, and it
/// was the abbreviation this function shipped with while `project_id` and the key were still the
/// same thing.
///
/// The key is read from the TABLE, not from `resolve_repo`, and that is deliberate twice over.
/// A listing must not need a git subprocess: it is where somebody goes when something is already
/// wrong, and a project whose directory has moved or gone would then have no listing at all rather
/// than a listing of what it did. And the row's own `repo_key` is the better authority anyway — it
/// is what the queue actually locked on at the time, which is not necessarily what the disk would
/// say now.
///
/// A project with nothing queued yet makes the subquery NULL, so the comparison is NULL and the
/// listing is empty. That is the right answer and not an accident of SQL: nothing has been queued
/// for it, so there is no repository to widen to.
pub async fn list(
    pool: &sqlx::SqlitePool,
    project_id: Option<&str>,
) -> sqlx::Result<Vec<RequestSummary>> {
    sqlx::query_as(
        "SELECT id, op, project_id, repo_key, origin, status, created_at
           FROM vcs_requests
          WHERE ?1 IS NULL
             OR repo_key = (
                  SELECT repo_key FROM vcs_requests
                   WHERE project_id = ?1
                   ORDER BY id DESC LIMIT 1
                )
          ORDER BY id DESC
          LIMIT ?2",
    )
    .bind(project_id)
    .bind(LIST_LIMIT)
    .fetch_all(pool)
    .await
}
/// The one predicate for "this request still wants a person": a terminal `escalated` or `blocked`
/// row nobody has settled. The count on the badge and the list on the Waiting page both read it, so
/// they cannot disagree about what is open.
pub const WANTS_A_PERSON_SQL: &str = "status IN ('escalated','blocked') AND settled_at IS NULL";
/// A merge paused because it changes the test map, until the owner approves or refuses it.
/// Kept apart from `WANTS_A_PERSON_SQL` on purpose: that one also drives `settle_moot` and
/// `dismiss`, which settle terminal rows, and this row is not terminal and is not dismissed but
/// decided.
pub const OWNER_DECISION_SQL: &str = "status = 'awaiting_owner'";
/// Every request that still wants a person, newest first, with no limit: the rows
/// `WANTS_A_PERSON_SQL` names plus the merges waiting for the owner's decision on the test map
/// (`OWNER_DECISION_SQL`).
///
/// `list` is capped at `LIST_LIMIT`, so a count taken from it silently stopped at the cap. This is
/// the uncapped read, and it carries the same columns so the shell draws both the same way, plus
/// the ones a decision needs. The resolution's status is a subquery, not a join: `runs` has a
/// `status` column too, and the shared predicates name theirs unqualified.
pub async fn waiting_on_a_person(pool: &sqlx::SqlitePool) -> sqlx::Result<Vec<RequestSummary>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT id, op, project_id, repo_key, origin, status, created_at,
                json_extract(args, '$.source') AS source,
                json_extract(args, '$.target') AS target,
                output_tail, failure_reason, resolution_run_id, from_resolution,
                (SELECT r.status FROM runs AS r WHERE r.id = vcs_requests.resolution_run_id)
                    AS resolution_status
           FROM vcs_requests
          WHERE ({WANTS_A_PERSON_SQL}) OR {OWNER_DECISION_SQL}
          ORDER BY id DESC"
    )))
    .fetch_all(pool)
    .await
}
/// How many requests `waiting_on_a_person` lists — the badge's number, read from the same two
/// predicates and without the payload columns (`output_tail` can be large).
pub async fn count_waiting_on_a_person(pool: &sqlx::SqlitePool) -> sqlx::Result<usize> {
    let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM vcs_requests WHERE ({WANTS_A_PERSON_SQL}) OR {OWNER_DECISION_SQL}"
    )))
    .fetch_one(pool)
    .await?;
    Ok(count as usize)
}
/// Marks an open `escalated`/`blocked` request as no longer wanting a person, and says why.
///
/// The status is left alone: it is the history. Only the first settlement counts, so a row keeps
/// its first reason. Returns whether this call settled it.
pub async fn settle(pool: &sqlx::SqlitePool, id: i64, reason: &str) -> sqlx::Result<bool> {
    let settled_at = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE vcs_requests SET settled_at = ?, settled_reason = ?
          WHERE id = ? AND status IN ('escalated','blocked') AND settled_at IS NULL",
    )
    .bind(settled_at)
    .bind(reason)
    .bind(id)
    .execute(pool)
    .await?;
    let settled = result.rows_affected() == 1;
    if settled {
        say_settled(pool, id, reason).await;
    }
    Ok(settled)
}
/// The feed line that closes a request's sequence once it stops wanting a person.
///
/// **Without it the Feed never learns the request is over.** The last line written under
/// `vcs:<id>` was `vcs_resolution_started`, which the trace reads as still going — and settling
/// wrote nothing, so every escalated merge that was later merged by hand, superseded, dismissed or
/// left without its branch drew as a resolution "still open" for days. Best-effort, like every
/// other feed write: the settlement is the record, and this only tells the reader about it.
pub(super) async fn say_settled(pool: &sqlx::SqlitePool, id: i64, reason: &str) {
    let project: Option<(String,)> =
        sqlx::query_as("SELECT project_id FROM vcs_requests WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    let _ = crate::feed::append(
        pool,
        project.as_ref().map(|(project,)| project.as_str()),
        "vcs_request_settled",
        &format!(
            "vcs request {id} no longer needs a person: {}",
            settled_words(reason)
        ),
        None,
        Some(&crate::feed::Subject::Vcs(id)),
    )
    .await;
}
/// PURE: a settlement reason, in the words the feed line says it with.
pub(super) fn settled_words(reason: &str) -> &str {
    match reason {
        "superseded" => "a later request on the same merge succeeded",
        "resolved" => "a conflict resolution landed it under another request",
        "landed" => "its target has since been merged into its source",
        "merged" => "its source is already in its target",
        "source-gone" => "its source branch is gone",
        "dismissed" => "dismissed",
        other => other,
    }
}
/// Settles what the table itself already proves moot, with no git subprocess.
///
/// `superseded`: a later request with the same project and arguments succeeded (the wording of
/// `resolver::next_conflict`'s own subquery). `resolved`: the row was handed to a resolution that
/// lands under another request — linked by `resolved_by`, or, when somebody merged the resolution's
/// branch into the same target by hand (a plain merge, so `land` never wrote the link), by a later
/// success of exactly that merge. `landed`: a later merge of this row's target INTO its source
/// succeeded, so whatever this merge would have brought over is already where it was going.
///
/// The two by-hand cases were measured, not imagined: requests 568 and 573 (master into
/// `feat/agent-loadout-preview`) each got a resolution, each resolution was merged in from the
/// shell (572, 576) and the branch then landed on master (577) — and both escalations stayed on the
/// Waiting page a day later, asking the owner to decide something already done.
/// Returns how many rows were settled.
pub async fn settle_by_record(pool: &sqlx::SqlitePool) -> sqlx::Result<u64> {
    let settled_at = chrono::Utc::now().to_rfc3339();
    let superseded: Vec<(i64,)> = sqlx::query_as(
        "UPDATE vcs_requests AS open SET settled_at = ?, settled_reason = 'superseded'
          WHERE open.status IN ('escalated','blocked') AND open.settled_at IS NULL
            AND EXISTS (
                SELECT 1 FROM vcs_requests AS settled
                 WHERE settled.project_id = open.project_id
                   AND settled.args = open.args
                   AND settled.status = 'succeeded'
                   AND settled.id > open.id
            )
          RETURNING id",
    )
    .bind(&settled_at)
    .fetch_all(pool)
    .await?;
    let resolved: Vec<(i64,)> = sqlx::query_as(
        "UPDATE vcs_requests AS open SET settled_at = ?, settled_reason = 'resolved'
          WHERE open.status IN ('escalated','blocked') AND open.settled_at IS NULL
            AND (
                open.resolved_by IS NOT NULL
                OR EXISTS (
                    SELECT 1 FROM vcs_requests AS landed
                      JOIN worktrees AS w
                        ON w.owner_kind = 'run' AND w.owner_id = open.resolution_run_id
                     WHERE landed.project_id = open.project_id
                       AND landed.op = 'merge'
                       AND landed.status = 'succeeded'
                       AND landed.id > open.id
                       AND json_extract(landed.args, '$.source') = w.branch
                       AND json_extract(landed.args, '$.target')
                           = json_extract(open.args, '$.target')
                )
            )
          RETURNING id",
    )
    .bind(&settled_at)
    .fetch_all(pool)
    .await?;
    let landed: Vec<(i64,)> = sqlx::query_as(
        "UPDATE vcs_requests AS open SET settled_at = ?, settled_reason = 'landed'
          WHERE open.status IN ('escalated','blocked') AND open.settled_at IS NULL
            AND open.op = 'merge'
            AND EXISTS (
                SELECT 1 FROM vcs_requests AS back
                 WHERE back.project_id = open.project_id
                   AND back.op = 'merge'
                   AND back.status = 'succeeded'
                   AND back.id > open.id
                   AND json_extract(back.args, '$.source') = json_extract(open.args, '$.target')
                   AND json_extract(back.args, '$.target') = json_extract(open.args, '$.source')
            )
          RETURNING id",
    )
    .bind(&settled_at)
    .fetch_all(pool)
    .await?;
    for (id,) in &superseded {
        say_settled(pool, *id, "superseded").await;
    }
    for (id,) in &resolved {
        say_settled(pool, *id, "resolved").await;
    }
    for (id,) in &landed {
        say_settled(pool, *id, "landed").await;
    }
    Ok((superseded.len() + resolved.len() + landed.len()) as u64)
}
/// Why `dismiss` did not dismiss.
#[derive(Debug)]
pub enum DismissError {
    /// No request has this id.
    NotFound,
    /// It exists, but is not open for this decision: succeeded, still running, already settled,
    /// or (for an owner decision) not paused for the owner.
    NotOpen,
    Db(sqlx::Error),
}
impl From<sqlx::Error> for DismissError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}
/// A person putting an open request away: settles it as `dismissed`.
pub async fn dismiss(pool: &sqlx::SqlitePool, id: i64) -> Result<(), DismissError> {
    if settle(pool, id, "dismissed").await? {
        return Ok(());
    }
    let exists: Option<(i64,)> = sqlx::query_as("SELECT id FROM vcs_requests WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Err(match exists {
        None => DismissError::NotFound,
        Some(_) => DismissError::NotOpen,
    })
}
/// The owner approves the map change a paused merge carries: back to `queued`, with the blob it
/// paused on recorded as the one approved. 404/409 as for `dismiss`.
pub async fn approve_for_owner(pool: &sqlx::SqlitePool, id: i64) -> Result<(), DismissError> {
    let approved = sqlx::query(
        "UPDATE vcs_requests
            SET status = 'queued', approved_map_blob = pending_map_blob, started_at = NULL
          WHERE id = ? AND status = 'awaiting_owner'",
    )
    .bind(id)
    .execute(pool)
    .await?;
    wake_worker();
    owner_decided(pool, id, approved.rows_affected()).await
}
/// The `failure_reason` `refuse_for_owner` records; the session hook matches on it to tell the
/// session not to retry.
pub const OWNER_REFUSED_REASON: &str = "the owner refused the change to the test map";
/// The owner refuses it: `rejected`, terminal, and the target never sees the change.
pub async fn refuse_for_owner(pool: &sqlx::SqlitePool, id: i64) -> Result<(), DismissError> {
    let finished_at = chrono::Utc::now().to_rfc3339();
    let refused = sqlx::query(
        "UPDATE vcs_requests
            SET status = 'rejected', finished_at = ?,
                failure_reason = ?
          WHERE id = ? AND status = 'awaiting_owner'",
    )
    .bind(finished_at)
    .bind(OWNER_REFUSED_REASON)
    .bind(id)
    .execute(pool)
    .await?;
    owner_decided(pool, id, refused.rows_affected()).await
}
pub(super) async fn owner_decided(
    pool: &sqlx::SqlitePool,
    id: i64,
    changed: u64,
) -> Result<(), DismissError> {
    if changed == 1 {
        return Ok(());
    }
    let exists: Option<(i64,)> = sqlx::query_as("SELECT id FROM vcs_requests WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Err(match exists {
        None => DismissError::NotFound,
        Some(_) => DismissError::NotOpen,
    })
}
/// The longest a caller that asked to wait is held before it gets a ticket instead.
///
/// Spec decision 3. The common case — an empty queue — answers from the first read and never
/// approaches this. The bad case is two merges queued behind a slow one, and the number exists so
/// that case stops killing the caller's run by timeout: the agent gets a ticket back and decides for
/// itself whether to keep waiting.
///
/// **The constraint to check this against is `state.rs`'s `DEFAULT_PROGRESS_TIMEOUT` (300s), not the
/// 600s wall clock.** A CLI blocked on a call for this long streams no events, and the progress
/// timeout is what kills a run that has gone quiet — so it binds first, and the real margin is
/// roughly 6.7x rather than the 13x the wall clock would suggest. Anyone tempted to lengthen this
/// has to answer to 300s. A wait that outlived the run waiting on it would be worse than no wait.
pub const DEFAULT_WAIT: std::time::Duration = std::time::Duration::from_secs(45);
/// How often `wait_for` re-checks a row that has not reached a terminal status yet.
///
/// 25ms, chosen from two directions that happen to agree.
///
/// In production it bounds how often one waiting caller queries: against the ~45s deadline this
/// pillar is designed around, 10ms would be roughly 4500 reads per waiting agent and 25ms roughly
/// 1800, while the extra latency it can cost — one interval, for a request that finishes just after
/// a poll — is nothing beside a merge measured in seconds.
///
/// In the tests it is the discrimination margin, and that is the reason it is not smaller.
/// `a_finished_request_returns_its_outcome_without_waiting` proves the answer came from the read
/// *before* the first sleep, and elapsed time is the only evidence of that — a loop that slept first
/// would return the same status, just one interval later. At 10ms the assertion sat exactly on the
/// boundary with no headroom, so ordinary scheduler jitter on a loaded laptop could fail a correct
/// implementation. The interval IS the margin; this file's other timing tests are documented as
/// leaving 10x.
///
/// A `Notify` would remove the wait entirely, but nothing here has a real operation duration yet to
/// make that worth the added machinery — the same tradeoff `drain_once`'s doc comment argues for
/// `VcsExecutor` staying a plain trait rather than a channel.
pub(super) const WAIT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
/// The four columns every read of `vcs_requests` in [`wait_for`] selects, named once for the
/// reason the other `*Row` aliases in this crate give: three of the four are `Option` and two of
/// those are `Option<String>`, so a `SELECT` that swapped the resulting sha for the failure
/// reason would still typecheck, and would surface as a ticket reporting a hash where its reason
/// belongs.
pub(super) type TicketRow = (String, Option<String>, Option<String>, Option<i64>);
/// Blocks the caller until request `id` reaches a terminal status or `deadline` passes — whichever
/// comes first — and returns a `Ticket` either way.
///
/// Never an error for "still going": a caller told the wait failed would reasonably retry or give
/// up, and both are wrong when the request is simply still queued behind another. The common case —
/// an empty queue — answers from the very first read, before any sleep, so it behaves like an
/// ordinary blocking call. The bad case — two merges queued behind a slow one — stops costing the
/// caller a hard timeout: it gets a ticket back instead and decides for itself whether to keep
/// waiting.
///
/// Terminal means `succeeded`, `failed`, `blocked`, `interrupted`, or `cancelled` — the five statuses
/// `finish`, `reconcile_interrupted`, `cancel_for_run` and `reap_requests_of_ended_runs` actually
/// write today, matching the vocabulary those four already use (see `finish`'s own doc comment, and
/// the interrupted-is-terminal test above). `cancelled` is the one this list gained last, and it has
/// two writers rather than one: `cancel_for_run` retires a request the moment its run is cancelled,
/// and the reaper retires one whose run had already ended by some other door when the queue reached
/// it. Either way the row can no longer change, so a caller waiting on it must be told now rather
/// than at the deadline. `blocked` is as
/// terminal as the other four: the queue never retries it, so a caller held to the deadline would
/// be waiting on a row that can no longer change — and it is the outcome that most needs a human to
/// see it promptly. `queued`, `running` and `awaiting_approval` are treated identically: all three
/// can still change, so none of them ends the wait early, and if the deadline passes while a row is
/// in any of them the ticket just reports whichever one it is. `awaiting_approval` is deliberately
/// not special-cased to end the wait sooner — a human approving mid-wait is exactly the change this
/// loop is built to catch on its next poll, and treating "needs a human" as if it were "done" would
/// tell an agent to stop watching a request that is very much still alive.
///
/// An `id` with no matching row is answered `Err(RowNotFound)` on the very first read, without
/// spending any of the deadline: every id in circulation came from `submit`, which hands one back
/// only after its INSERT has committed, and nothing in this module deletes a row *today*. So a
/// missing row is not "hasn't arrived yet" — it cannot ever arrive — and polling it out to the
/// deadline would just be quietly burning the caller's wait on a request that does not exist.
///
/// The hedge was deliberate: that is a claim about the whole module, not about this function, and
/// the first retention or cleanup pass added anywhere in `vcs.rs` would invalidate it silently — the
/// failure being a caller told "no such request" about one that merely aged out. **Pruning has since
/// arrived and the claim still holds**, because `prune_output_tails` empties `output_tail` and keeps
/// the row rather than deleting it — a shape it was pushed into by `action_grants.queued_request_id`
/// having no foreign key, and which happens to discharge this obligation for free. The hedge stays
/// written down because the next cleanup pass inherits it.
///
/// Reads before it ever sleeps, and every subsequent iteration does the same: the terminal check
/// runs on freshly read data, not on whatever the previous iteration saw, so a row that finishes
/// between two polls is reported the moment the next read sees it rather than after the deadline.
///
/// **A terminal `escalated` row carrying `resolved_by` is not the end of the wait — it is
/// followed.** Design decision #5: the row a conflict resolution's landing wrote is a NEW request,
/// admitted after this one already went terminal, and a caller polling only the id it was handed
/// would read `escalated` and stop — told its merge failed while the daemon is, in fact, still
/// carrying it. So a terminal row that names a successor is not returned; the successor is read
/// instead, on the same loop and against the same deadline. The chain can be more than one link
/// long — a resolution that itself conflicts escalates as its own row, which a later resolution
/// then answers — and this follows it however far it goes, because each link is written by the
/// same admission this loop already trusts. The `Ticket` this returns keeps the id the caller
/// asked about, whichever row's status, sha and reason it took to answer it.
pub async fn wait_for(
    pool: &sqlx::SqlitePool,
    id: i64,
    deadline: std::time::Duration,
) -> sqlx::Result<Ticket> {
    let started = std::time::Instant::now();
    let mut current = id;
    loop {
        let row: Option<TicketRow> = sqlx::query_as(
            "SELECT status, result_sha, failure_reason, resolved_by FROM vcs_requests WHERE id = ?",
        )
        .bind(current)
        .fetch_optional(pool)
        .await?;

        let Some((status, result_sha, failure_reason, resolved_by)) = row else {
            return Err(sqlx::Error::RowNotFound);
        };

        if TERMINAL_STATUSES.contains(&status.as_str()) {
            if let Some(next) = resolved_by {
                current = next;
                continue;
            }
            return Ok(Ticket {
                id,
                status,
                result_sha,
                failure_reason,
            });
        }
        if started.elapsed() >= deadline {
            return Ok(Ticket {
                id,
                status,
                result_sha,
                failure_reason,
            });
        }

        tokio::time::sleep(WAIT_POLL_INTERVAL).await;
    }
}
