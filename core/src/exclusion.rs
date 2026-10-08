//! Two jobs somebody asked not to run at the same time.
//!
//! It lives apart from `concurrency.rs` for the reason `collision.rs` already gave. That one answers
//! *how much work fits*, and the answer is an invariant held by a primary key; this answers *what
//! somebody asked for*, and the answer is a rule with an author, an approval and a revocation. A
//! slot is arithmetic. This is a decision, and decisions have a history.
//!
//! **Asking writes no rule.** Drawing an exclusion mints a proposal and nothing else; the row in
//! `fleet_exclusions` appears when that proposal is approved. This is the difference between this
//! design and october.dev's canvas, where an edge grants access the moment it is drawn — an edge
//! that acts on being drawn is a way to make a governance decision without passing through the place
//! where governance decisions are made.
//!
//! The pair is ordered on the way in and never at read time. See the migration for why: the order is
//! also the tie-break, so both jobs parked on each other is a state this feature cannot reach.

use sqlx::SqlitePool;

/// The two jobs, ordered, or `None` when they are the same job.
///
/// `None` and not "the pair (7, 7)": a job excluded from itself is a job that holds its own slot and
/// therefore blocks forever, and the unique index would happily store it. The refusal belongs here,
/// at the one door every write goes through, rather than in each caller.
pub fn pair(a: i64, b: i64) -> Option<(i64, i64)> {
    if a == b {
        return None;
    }
    Some(if a < b { (a, b) } else { (b, a) })
}

/// Why an exclusion could not be asked for.
#[derive(Debug)]
pub enum ProposeError {
    /// Both ids name the same job.
    SameJob,
    /// One of the two ids names no job.
    UnknownJob(i64),
    /// The two jobs belong to different projects.
    DifferentProjects,
    /// This pair already has a request waiting for a decision.
    AlreadyAsked,
    /// This pair already has a rule in force.
    AlreadyExcluded,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ProposeError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

/// The key a request carries so a second request for the same pair can be recognised.
///
/// A field of its own rather than a match against `job_low` and `job_high` in the stored JSON,
/// because that match would depend on the order `serde_json` writes an object's keys in — a detail
/// no test in this repository pins and any future crate feature could change. `calendar.rs` searches
/// `tool_input` with a LIKE in the same way; this only gives the LIKE something stable to find.
fn pair_key(low: i64, high: i64) -> String {
    format!("{low}:{high}")
}

/// Asks that two jobs not run at the same time. Writes a proposal, and no rule.
///
/// The project is read from the jobs rather than supplied, which removes the one call that could
/// file an exclusion under a project neither job belongs to. Both jobs have to be in the same
/// project because the thing being serialised is a project's slots.
pub async fn propose(
    pool: &SqlitePool,
    job_a: i64,
    job_b: i64,
    paths: &[String],
) -> Result<i64, ProposeError> {
    let (low, high) = pair(job_a, job_b).ok_or(ProposeError::SameJob)?;

    let project_low = project_of(pool, low)
        .await?
        .ok_or(ProposeError::UnknownJob(low))?;
    let project_high = project_of(pool, high)
        .await?
        .ok_or(ProposeError::UnknownJob(high))?;
    if project_low != project_high {
        return Err(ProposeError::DifferentProjects);
    }

    if live_rule_for(pool, low, high).await?.is_some() {
        return Err(ProposeError::AlreadyExcluded);
    }
    if pending_request_for(pool, low, high).await? {
        return Err(ProposeError::AlreadyAsked);
    }

    let tool_input = serde_json::json!({
        "pair": pair_key(low, high),
        "job_low": low,
        "job_high": high,
        "paths": paths,
    })
    .to_string();
    let reasoning = match paths {
        [] => format!("jobs {low} and {high} are not to run at the same time"),
        [only] => format!("jobs {low} and {high} both touch {only}"),
        _ => format!(
            "jobs {low} and {high} both touch {} files, including {}",
            paths.len(),
            paths[0]
        ),
    };

    let now = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;
    let proposal_id: i64 = sqlx::query_scalar(
        "INSERT INTO proposals
         (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
         VALUES ('fleet-exclusion', 'pending', NULL, NULL, ?, NULL, ?, ?, ?, NULL)
         RETURNING id",
    )
    .bind(&project_low)
    .bind(reasoning)
    .bind(tool_input)
    .bind(&now)
    .fetch_one(&mut *transaction)
    .await?;

    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'pending', 'created', ?)",
    )
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;
    Ok(proposal_id)
}

async fn project_of(pool: &SqlitePool, job_id: i64) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT project_id FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_optional(pool)
        .await
}

/// The rule in force for this pair, if there is one.
pub async fn live_rule_for(pool: &SqlitePool, low: i64, high: i64) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT id FROM fleet_exclusions
          WHERE job_low = ? AND job_high = ? AND revoked_at IS NULL
          LIMIT 1",
    )
    .bind(low)
    .bind(high)
    .fetch_optional(pool)
    .await
}

/// The partner whose slot is keeping this job waiting, if there is one.
///
/// **Only the higher id ever waits, and that is the whole of the deadlock argument.** The query asks
/// about `job_high` alone: a job that is the low side of every rule it appears in can never be
/// parked by this brake, so of any two excluded jobs at least one is always free to run. A
/// tie-break decided at read time — who asked first, who has less left to do — could park both if
/// the two reads disagreed, and two jobs a person asked to SERIALISE deadlocking on each other is
/// the one outcome this feature must not be able to produce.
///
/// "Holds a slot" and not "is live": a live job that is waiting for a slot is not running anything,
/// so there is nothing for the other one to run at the same time as.
pub async fn blocking_partner(pool: &SqlitePool, job_id: i64) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT fleet_exclusions.job_low
           FROM fleet_exclusions
           JOIN project_slots
             ON project_slots.owner_kind = 'job'
            AND project_slots.owner_id = fleet_exclusions.job_low
          WHERE fleet_exclusions.job_high = ? AND fleet_exclusions.revoked_at IS NULL
          LIMIT 1",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await
}

/// Whether this pair already has a request waiting on a person.
///
/// Without it, drawing the same edge twice mints two proposals, and approving both would meet the
/// unique index as a 500 — the second approval failing for a reason the person could not act on.
async fn pending_request_for(pool: &SqlitePool, low: i64, high: i64) -> sqlx::Result<bool> {
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM proposals
          WHERE kind = 'fleet-exclusion' AND status = 'pending' AND tool_input LIKE ?
          LIMIT 1",
    )
    .bind(format!("%\"pair\":\"{}\"%", pair_key(low, high)))
    .fetch_optional(pool)
    .await?;
    Ok(found.is_some())
}

/// One rule in force, as the canvas draws it.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize)]
pub struct Exclusion {
    pub id: i64,
    pub project_id: String,
    pub job_low: i64,
    pub job_high: i64,
    /// The request that authorised it, so the screen can show who agreed and when.
    pub proposal_id: i64,
    pub paths: Option<String>,
    pub created_at: String,
}

/// The rules in force, newest first.
///
/// Two whole statements rather than one with a filter appended: sqlx 0.9 only trusts
/// `&'static str`, and the alternative is `AssertSqlSafe` over a string built at runtime — a
/// heavier tool than one WHERE clause deserves. The columns are written twice; the test below
/// reads both back, so a column added to one and not the other fails rather than drifts.
pub async fn live(pool: &SqlitePool, project_id: Option<&str>) -> sqlx::Result<Vec<Exclusion>> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, Exclusion>(
                "SELECT id, project_id, job_low, job_high, proposal_id, paths, created_at
                 FROM fleet_exclusions
                 WHERE revoked_at IS NULL AND project_id = ?
                 ORDER BY id DESC",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, Exclusion>(
                "SELECT id, project_id, job_low, job_high, proposal_id, paths, created_at
                 FROM fleet_exclusions
                 WHERE revoked_at IS NULL
                 ORDER BY id DESC",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// The requests still waiting on a person, oldest first.
///
/// Its own door and not a slice of `/proposals`, for the reason `get_contact_merges` already gives:
/// `list_pending` serves `action-approval` alone, and its comment argues at length that a queue
/// where approving resumes a paused run must not be mixed with decisions that resume nothing. This
/// is one of those. It also puts the question where the context is — whether two jobs should be
/// serialised is decided while looking at the fleet, not at a queue of stopped runs.
///
/// Oldest first, like the approval queue and unlike the skipped-item record: this is a queue, worked
/// front to back, and the request that has been waiting longest is the one holding somebody up.
pub async fn pending_requests(pool: &SqlitePool) -> sqlx::Result<Vec<crate::proposals::Proposal>> {
    sqlx::query_as::<_, crate::proposals::Proposal>(
        "SELECT id, kind, status, run_id, session_id, project_id, tool_name, reasoning,
                tool_input, read_from, created_at, decided_at,
                NULL AS job_id, NULL AS run_stage, NULL AS item_ordinal, NULL AS item_description
         FROM proposals
         WHERE status = 'pending' AND kind = 'fleet-exclusion'
         ORDER BY id ASC",
    )
    .fetch_all(pool)
    .await
}

/// Lifts a rule, keeping it readable.
///
/// Revoked and not deleted: the decision stays legible after it stops applying — somebody will ask
/// why two jobs were serialised last Tuesday — and the partial unique index means the same pair can
/// be asked about again afterwards.
pub async fn revoke(pool: &SqlitePool, id: i64) -> sqlx::Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let lifted = sqlx::query(
        "UPDATE fleet_exclusions SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL",
    )
    .bind(&now)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(lifted.rows_affected() == 1)
}

/// Why a decision on an exclusion request could not be taken.
#[derive(Debug)]
pub enum DecisionError {
    NotFound,
    NotPending,
    /// The request carries no usable pair — nothing this daemon writes looks like that.
    Malformed,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for DecisionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

/// What approving a request turned out to mean.
#[derive(Debug, PartialEq, Eq)]
pub enum Approved {
    /// The rule is in force, under this id.
    Written(i64),
    /// One of the two jobs had already ended, so the request was closed with a note instead.
    ///
    /// Not an error and not a refusal: the person answered a question that had stopped mattering
    /// while it waited, and the alternative is a rule about two jobs that will never run again
    /// sitting in the table until somebody prunes it.
    Stale,
}

/// Approving writes the rule.
///
/// The pair comes out of the proposal's own `tool_input` and never out of the request that
/// approved it, so what is written is what was shown to the person who agreed.
pub async fn approve(pool: &SqlitePool, proposal_id: i64) -> Result<Approved, DecisionError> {
    let proposal = crate::proposals::get(pool, proposal_id)
        .await?
        .ok_or(DecisionError::NotFound)?;
    if proposal.kind != "fleet-exclusion" || proposal.status != "pending" {
        return Err(DecisionError::NotPending);
    }
    let (low, high, paths) = requested_pair(&proposal).ok_or(DecisionError::Malformed)?;
    let project_id = proposal
        .project_id
        .clone()
        .ok_or(DecisionError::Malformed)?;

    let now = chrono::Utc::now().to_rfc3339();

    // A request outlives what it was about. A job that has ended never holds a slot again, so a rule
    // naming one could not park anything — and leaving the request in the queue would ask somebody
    // to keep deciding it forever.
    if !is_live(pool, low).await? || !is_live(pool, high).await? {
        if !crate::proposals::transition(
            pool,
            proposal_id,
            "dismissed",
            "the jobs it named have ended",
        )
        .await?
        {
            return Err(DecisionError::NotPending);
        }
        return Ok(Approved::Stale);
    }

    // Already in force is the answer the person wanted, not a collision to report. `propose` refuses
    // a second pending request for a pair, so reaching this means the rule arrived by some path
    // this one did not see; approving on top of it changes nothing and says so truthfully.
    if let Some(existing) = live_rule_for(pool, low, high).await? {
        if !crate::proposals::transition(
            pool,
            proposal_id,
            "approved",
            "this pair was already excluded",
        )
        .await?
        {
            return Err(DecisionError::NotPending);
        }
        return Ok(Approved::Written(existing));
    }

    let mut transaction = pool.begin().await?;
    let exclusion_id: i64 = sqlx::query_scalar(
        "INSERT INTO fleet_exclusions
             (project_id, job_low, job_high, proposal_id, paths, created_at)
         VALUES (?, ?, ?, ?, ?, ?)
         RETURNING id",
    )
    .bind(&project_id)
    .bind(low)
    .bind(high)
    .bind(proposal_id)
    .bind(paths)
    .bind(&now)
    .fetch_one(&mut *transaction)
    .await?;

    // One transaction, and it is load-bearing: the rule and the decision that authorised it have to
    // land together, or a dropped connection leaves a rule parking a job with a request still
    // reading `pending` beside it.
    if !crate::proposals::transition_in_transaction(
        &mut transaction,
        proposal_id,
        "approved",
        "approved by user",
        &now,
    )
    .await?
    {
        return Err(DecisionError::NotPending);
    }
    transaction.commit().await?;
    Ok(Approved::Written(exclusion_id))
}

/// Refusing writes nothing at all.
///
/// There is nothing to undo: the request never changed how anything is scheduled, which is the point
/// of it being a request. Unlike `reject_proposal`, no run is discarded either — an exclusion pauses
/// no run and holds no worktree.
pub async fn reject(pool: &SqlitePool, proposal_id: i64) -> Result<(), DecisionError> {
    let proposal = crate::proposals::get(pool, proposal_id)
        .await?
        .ok_or(DecisionError::NotFound)?;
    if proposal.kind != "fleet-exclusion" || proposal.status != "pending" {
        return Err(DecisionError::NotPending);
    }
    // Compare-and-set, so a refusal that lost the race to an approval is reported rather than
    // answered 204 — the same reason `reject_proposal` checks the return of its own transition.
    if !crate::proposals::transition(pool, proposal_id, "rejected", "rejected by user").await? {
        return Err(DecisionError::NotPending);
    }
    Ok(())
}

/// The pair a request names, as `(low, high, paths)`.
fn requested_pair(proposal: &crate::proposals::Proposal) -> Option<(i64, i64, Option<String>)> {
    let input: serde_json::Value = proposal
        .tool_input
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())?;
    let low = input.get("job_low")?.as_i64()?;
    let high = input.get("job_high")?.as_i64()?;
    // Normalised again on the way out, not trusted. The row's index depends on the order, and this
    // is the last place before it is written.
    let (low, high) = pair(low, high)?;
    let paths = input
        .get("paths")
        .filter(|paths| paths.as_array().is_some_and(|list| !list.is_empty()))
        .map(std::string::ToString::to_string);
    Some((low, high, paths))
}

/// Whether a job is still one that could hold a slot.
async fn is_live(pool: &SqlitePool, job_id: i64) -> sqlx::Result<bool> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_optional(pool)
        .await?;
    Ok(status.is_some_and(|status| crate::job::LIVE_STATUSES.contains(&status.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn add_job(pool: &SqlitePool, project_id: &str) -> i64 {
        sqlx::query(
            "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
             VALUES (?, ?, 'implementing', 5, '2026-08-15T00:00:00Z')",
        )
        .bind(project_id)
        .bind(format!("C:/projects/{project_id}"))
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// Drawn either way round, it is one rule.
    ///
    /// The relation is symmetric and the storage is not, so this is the only place the two are
    /// reconciled. Were it not, the same request drawn backwards would be a second row the unique
    /// index cannot see, and revoking one would leave the other holding the job.
    #[test]
    fn the_pair_is_the_same_pair_drawn_either_way() {
        assert_eq!(pair(41, 42), Some((41, 42)));
        assert_eq!(pair(42, 41), Some((41, 42)));
        assert_eq!(pair(41, 42), pair(42, 41));
    }

    /// A job does not exclude itself, and the refusal is here rather than in the callers.
    ///
    /// The row would be accepted by the table: `job_low = job_high = 7` breaks no constraint. What
    /// it would do is park job 7 for as long as job 7 holds a slot, which is a job that can never
    /// run again and no error message anywhere to say why.
    #[test]
    fn a_job_is_not_excluded_from_itself() {
        assert_eq!(pair(7, 7), None);
    }

    /// Asking writes a request and NOT a rule.
    ///
    /// This is decision 1 of the design, and it is the whole difference from october.dev's canvas,
    /// where drawing an edge grants what it describes. Here the edge is a question; the table below
    /// stays empty until somebody answers it.
    #[tokio::test]
    async fn asking_mints_a_proposal_and_writes_no_rule() {
        let pool = test_pool().await;
        let low = add_job(&pool, "alpha").await;
        let high = add_job(&pool, "alpha").await;

        let proposal_id = propose(&pool, high, low, &[]).await.unwrap();

        let (kind, status, project_id, run_id): (String, String, Option<String>, Option<i64>) =
            sqlx::query_as("SELECT kind, status, project_id, run_id FROM proposals WHERE id = ?")
                .bind(proposal_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(kind, "fleet-exclusion");
        assert_eq!(status, "pending");
        assert_eq!(project_id.as_deref(), Some("alpha"));
        assert_eq!(run_id, None, "an exclusion is about jobs, not about a run");

        let rules: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet_exclusions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rules, 0, "asking must write no rule");
    }

    /// The same edge drawn twice is one question, not two.
    ///
    /// Two pending requests for one pair would both be approvable, and the second approval would
    /// meet `one_live_exclusion_per_pair` as a database error — a refusal with nothing in it the
    /// person could act on.
    #[tokio::test]
    async fn a_pair_already_asked_about_is_refused() {
        let pool = test_pool().await;
        let low = add_job(&pool, "alpha").await;
        let high = add_job(&pool, "alpha").await;

        propose(&pool, low, high, &[]).await.unwrap();
        // Backwards, to prove the check is on the pair and not on the argument order.
        let second = propose(&pool, high, low, &[]).await;

        assert!(
            matches!(second, Err(ProposeError::AlreadyAsked)),
            "{second:?}"
        );
    }

    /// Two jobs of different projects share no slots, so there is nothing to serialise.
    #[tokio::test]
    async fn two_jobs_of_different_projects_cannot_be_excluded() {
        let pool = test_pool().await;
        let alpha = add_job(&pool, "alpha").await;
        let beta = add_job(&pool, "beta").await;

        let asked = propose(&pool, alpha, beta, &[]).await;

        assert!(
            matches!(asked, Err(ProposeError::DifferentProjects)),
            "{asked:?}"
        );
    }

    #[tokio::test]
    async fn a_job_that_does_not_exist_is_named_in_the_refusal() {
        let pool = test_pool().await;
        let job = add_job(&pool, "alpha").await;

        let asked = propose(&pool, job, 404, &[]).await;

        assert!(
            matches!(asked, Err(ProposeError::UnknownJob(404))),
            "{asked:?}"
        );
    }

    #[tokio::test]
    async fn asking_about_the_same_job_twice_is_refused() {
        let pool = test_pool().await;
        let job = add_job(&pool, "alpha").await;

        let asked = propose(&pool, job, job, &[]).await;

        assert!(matches!(asked, Err(ProposeError::SameJob)), "{asked:?}");
    }

    /// Approving is what writes the rule, and it writes the pair the PROPOSAL named.
    #[tokio::test]
    async fn approving_writes_the_rule_and_the_decision_together() {
        let pool = test_pool().await;
        let low = add_job(&pool, "alpha").await;
        let high = add_job(&pool, "alpha").await;
        let proposal_id = propose(&pool, high, low, &["src/shared.rs".to_owned()])
            .await
            .unwrap();

        let approved = approve(&pool, proposal_id).await.unwrap();

        let Approved::Written(exclusion_id) = approved else {
            panic!("two live jobs must yield a rule, got {approved:?}");
        };
        let (project_id, stored_low, stored_high, stored_proposal, paths, revoked): (
            String,
            i64,
            i64,
            i64,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT project_id, job_low, job_high, proposal_id, paths, revoked_at
             FROM fleet_exclusions WHERE id = ?",
        )
        .bind(exclusion_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(project_id, "alpha");
        assert_eq!((stored_low, stored_high), (low, high));
        assert_eq!(stored_proposal, proposal_id);
        assert!(paths.unwrap().contains("src/shared.rs"));
        assert_eq!(revoked, None);

        let status: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "approved");
    }

    /// Refusing leaves the table exactly as it found it.
    #[tokio::test]
    async fn rejecting_writes_nothing() {
        let pool = test_pool().await;
        let low = add_job(&pool, "alpha").await;
        let high = add_job(&pool, "alpha").await;
        let proposal_id = propose(&pool, low, high, &[]).await.unwrap();

        reject(&pool, proposal_id).await.unwrap();

        let rules: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet_exclusions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rules, 0);
        let status: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "rejected");

        // And a second decision on it is reported, not silently accepted.
        assert!(matches!(
            reject(&pool, proposal_id).await,
            Err(DecisionError::NotPending)
        ));
    }

    /// A request outlives the jobs it was about, and answering it then writes no rule.
    ///
    /// A finished job never holds a slot again, so the rule could park nothing — it would sit in the
    /// table naming two jobs that will never run, until somebody went looking for why it was there.
    /// The request is closed with a note instead, which is also what takes it out of the queue: left
    /// pending it would be a question nobody can usefully answer, asked forever.
    #[tokio::test]
    async fn approving_a_request_whose_job_has_ended_closes_it_with_a_note() {
        let pool = test_pool().await;
        let low = add_job(&pool, "alpha").await;
        let high = add_job(&pool, "alpha").await;
        let proposal_id = propose(&pool, low, high, &[]).await.unwrap();
        sqlx::query("UPDATE jobs SET status = 'completed' WHERE id = ?")
            .bind(high)
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(approve(&pool, proposal_id).await.unwrap(), Approved::Stale);

        let rules: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet_exclusions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rules, 0);
        let (status, note): (String, Option<String>) = sqlx::query_as(
            "SELECT proposals.status, proposal_events.note
               FROM proposals
               JOIN proposal_events ON proposal_events.proposal_id = proposals.id
              WHERE proposals.id = ? AND proposal_events.to_status = 'dismissed'",
        )
        .bind(proposal_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, "dismissed");
        assert_eq!(note.as_deref(), Some("the jobs it named have ended"));
    }

    /// The listing carries every column the canvas draws, and a revoked rule leaves it.
    ///
    /// Both spellings of the query are read back here, filtered and unfiltered, because the column
    /// list is written out twice: one of them gaining a column the other lacks is the way this
    /// drifts, and it would show up as a field that is null on some screens and not others.
    #[tokio::test]
    async fn the_listing_shows_live_rules_and_forgets_revoked_ones() {
        let pool = test_pool().await;
        let low = add_job(&pool, "alpha").await;
        let high = add_job(&pool, "alpha").await;
        let proposal_id = propose(&pool, low, high, &["src/shared.rs".to_owned()])
            .await
            .unwrap();
        let Approved::Written(exclusion_id) = approve(&pool, proposal_id).await.unwrap() else {
            panic!("two live jobs must yield a rule");
        };

        for listed in [
            live(&pool, None).await.unwrap(),
            live(&pool, Some("alpha")).await.unwrap(),
        ] {
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].id, exclusion_id);
            assert_eq!(listed[0].project_id, "alpha");
            assert_eq!((listed[0].job_low, listed[0].job_high), (low, high));
            assert_eq!(listed[0].proposal_id, proposal_id);
            assert!(listed[0].paths.as_deref().unwrap().contains("shared.rs"));
        }
        assert!(live(&pool, Some("beta")).await.unwrap().is_empty());

        assert!(revoke(&pool, exclusion_id).await.unwrap());
        assert!(live(&pool, None).await.unwrap().is_empty());
        // Lifting a rule that is already lifted is reported, not silently accepted: the screen that
        // asked would otherwise redraw the edge as gone twice and never say which click did it.
        assert!(!revoke(&pool, exclusion_id).await.unwrap());

        // And the pair can be asked about again, which is what the partial index is for.
        propose(&pool, low, high, &[]).await.unwrap();
    }

    /// The queue holds a request until it is decided, and only ever holds this kind.
    ///
    /// The kind filter is the load-bearing half. These proposals share a table with the ones a
    /// paused run is waiting on, and a screen that offered "approve" over the wrong one would be
    /// resuming a run from a canvas.
    #[tokio::test]
    async fn the_queue_holds_undecided_requests_and_nothing_else() {
        let pool = test_pool().await;
        let low = add_job(&pool, "alpha").await;
        let high = add_job(&pool, "alpha").await;
        crate::proposals::create_contact_merge(&pool, 1, 2, "not this one")
            .await
            .unwrap();
        let proposal_id = propose(&pool, low, high, &[]).await.unwrap();

        let waiting = pending_requests(&pool).await.unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].id, proposal_id);
        assert_eq!(waiting[0].kind, "fleet-exclusion");

        reject(&pool, proposal_id).await.unwrap();
        assert!(pending_requests(&pool).await.unwrap().is_empty());
    }

    /// The two decisions refuse anything that is not a pending exclusion, by kind and by status.
    #[tokio::test]
    async fn a_proposal_of_another_kind_is_not_decided_here() {
        let pool = test_pool().await;
        let proposal_id = crate::proposals::create_contact_merge(&pool, 1, 2, "test")
            .await
            .unwrap();

        assert!(matches!(
            approve(&pool, proposal_id).await,
            Err(DecisionError::NotPending)
        ));
        assert!(matches!(
            reject(&pool, proposal_id).await,
            Err(DecisionError::NotPending)
        ));
        assert!(matches!(
            approve(&pool, 404).await,
            Err(DecisionError::NotFound)
        ));
    }
}
