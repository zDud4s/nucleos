//! §spec pilar-de-browser

use serde::Serialize;
use sqlx::{FromRow, Sqlite, SqlitePool, Transaction};

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Proposal {
    pub id: i64,
    pub kind: String,
    pub status: String,
    pub run_id: Option<i64>,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
    /// The errand this came from, when it came from one — which is almost never.
    ///
    /// Not derivable from `project_id`: an errand HAS no project, so an errand's proposal and a
    /// machine-wide one both carry `project_id IS NULL` and nothing else in the row tells them
    /// apart.
    pub errand_id: Option<i64>,
    /// The errand's name, joined in by the queries whose readers need it and `NULL` in the rest.
    ///
    /// Carried on the same struct rather than in a second type, because the alternative was a
    /// near-copy of eleven fields that would drift the first time one of them changed. The `NULL AS
    /// errand_name` in the other queries is what keeps that honest: a reader that gets `None` is
    /// being told this query did not ask, and the id is still there to ask with.
    pub errand_name: Option<String>,
    pub tool_name: Option<String>,
    pub reasoning: String,
    pub tool_input: Option<String>,
    /// What the turn had read when it reached for this, as JSON, copied off `run_untrusted_reads`
    /// at the moment the refusal was written.
    ///
    /// Selected by every query rather than by the ones that care, which is the opposite of what
    /// `errand_name` above does — and deliberately. `errand_name` is a join, so `NULL AS
    /// errand_name` honestly means "this query did not ask" and the id is still there to ask with.
    /// This is a column; `NULL` in it already means "nothing was recorded", and there is nothing to
    /// ask with afterwards because the run may have been pruned. A second meaning for the same
    /// `NULL` would make an unasked question indistinguishable from an answered one.
    ///
    /// `None` is a legitimate state and not a defect: `ERRAND_MAY_NOT_ACT` refuses on whose work it
    /// is rather than on what the turn read, and an errand's first message has read nothing at all.
    /// A reader must not present its absence as contamination.
    pub read_from: Option<String>,
    pub created_at: String,
    pub decided_at: Option<String>,
}

#[derive(Debug)]
pub enum RejectError {
    NotFound,
    NotPending,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for RejectError {
    fn from(error: sqlx::Error) -> Self {
        RejectError::Db(error)
    }
}

pub async fn create_action_approval(
    pool: &SqlitePool,
    run_id: i64,
    session_id: Option<&str>,
    project_id: Option<&str>,
    tool_name: &str,
    reasoning: &str,
    tool_input: Option<&str>,
) -> sqlx::Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO proposals
         (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
         VALUES ('action-approval', 'pending', ?, ?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(run_id)
    .bind(session_id)
    .bind(project_id)
    .bind(tool_name)
    .bind(reasoning)
    .bind(tool_input)
    .bind(&now)
    .execute(&mut *transaction)
    .await?;
    let proposal_id = result.last_insert_rowid();

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

/// An action the injection barrier refused, kept where a person can read it.
///
/// The fifth `kind`, and it exists because of what `approve` means. §6 closes acting tools once a
/// turn has read a stranger's words — which, for an errand, is every turn that did any research.
/// Until this row existed the refusal was the end of the line: the model was stopped and the owner
/// never learned what it had wanted to do, so an errand could spend an afternoon finding the right
/// car and have no way to say so.
///
/// **Not `action-approval`, and the reason is mechanical rather than aesthetic.** Approving one of
/// those calls `runs::resume_approved_run`, which looks up a live worktree for the paused run and
/// answers `NotResumable` without one. An errand turn has no worktree and was never paused — it was
/// denied and carried on. Filed as an action approval, this would appear under a button that cannot
/// work, which is worse than appearing under none.
///
/// So nothing resumes here either, exactly as for [`create_skipped_item`]. What the record buys is
/// that somebody finds out: they do the thing themselves, or they ask the errand again, and the new
/// turn starts clean and may act. The door is a person, not a button.
///
/// **`read_from` is what makes that door usable rather than merely open.** Deciding whether to do
/// the thing yourself means deciding whether the idea was the agent's or the page's, and the row
/// could not answer that: it said what was going to happen and never where it came from. An email
/// to accounts asking for the bank details to change reads identically either way. It is a copy and
/// not a join because `runs` rows are pruned, and `None` means nothing was recorded — which is the
/// normal state for the OTHER refusal this kind carries, where an errand was stopped for whose work
/// it is rather than for anything it read.
// Eight, and the eighth is `read_from`. Bundling them into a struct to satisfy the lint would put a
// type between the caller and a row it is spelling out field by field, which is what the sibling
// constructors above all do; the shape stays consistent with them rather than with the count.
#[allow(clippy::too_many_arguments)]
pub async fn create_refused_action(
    pool: &SqlitePool,
    run_id: i64,
    session_id: Option<&str>,
    errand_id: Option<i64>,
    tool_name: &str,
    reasoning: &str,
    tool_input: Option<&str>,
    read_from: Option<&str>,
) -> sqlx::Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO proposals
         (kind, status, run_id, session_id, errand_id, tool_name, reasoning, tool_input, read_from, created_at, decided_at)
         VALUES ('refused-action', 'pending', ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(run_id)
    .bind(session_id)
    .bind(errand_id)
    .bind(tool_name)
    .bind(reasoning)
    .bind(tool_input)
    .bind(read_from)
    .bind(&now)
    .execute(&mut *transaction)
    .await?;
    let proposal_id = result.last_insert_rowid();

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

/// What the barrier refused and nobody has read yet.
///
/// Its own door rather than a `kind` filter on `list_pending`, for the reason `list_skipped_items`
/// gives: that list feeds a screen with approve and reject buttons, and both of those answer 409
/// for anything that is not an `action-approval`.
pub async fn list_refused_actions(pool: &SqlitePool) -> sqlx::Result<Vec<Proposal>> {
    sqlx::query_as::<_, Proposal>(
        // The one query that joins. A person reading this list is deciding whether to do the thing
        // themselves, and "send_email" without the errand is not a decidable question — it is the
        // verb with the subject missing. LEFT, so a refused action with no errand (an ordinary chat
        // that read its mail and then reached for a control) still appears, unnamed.
        "SELECT p.id, p.kind, p.status, p.run_id, p.session_id, p.project_id, p.errand_id,
                e.name AS errand_name, p.tool_name, p.reasoning,
                p.tool_input, p.read_from, p.created_at, p.decided_at
         FROM proposals p
         LEFT JOIN errands e ON e.id = p.errand_id
         WHERE p.status = 'pending' AND p.kind = 'refused-action'
         ORDER BY p.id ASC",
    )
    .fetch_all(pool)
    .await
}

/// A job put an item down because it asked for a decision, and this is the record of it.
///
/// The fourth `kind` this table carries, and the one that means the OPPOSITE of `action-approval`
/// despite arriving through the same door in `hooks.rs`. An action approval is work stopped
/// mid-stride, waiting to be let through; this is work that was never started, in a job that has
/// already moved on. Nothing resumes when it is approved, which is why it is not `action-approval`
/// with a flag: `approve` would have to mean two different things.
///
/// `tool_input` carries what the item was about to do when it asked, so a person reading this in the
/// morning can tell an item worth picking up from one worth dropping. **Resuming from it is
/// deliberately out of scope for v1** — the tree has moved under it by then, which is the same class
/// of risk as a catch-up run and deserves the same deliberate decision, taken with a real case in
/// hand rather than now.
pub async fn create_skipped_item(
    pool: &SqlitePool,
    run_id: i64,
    session_id: Option<&str>,
    project_id: Option<&str>,
    tool_name: &str,
    reasoning: &str,
    tool_input: Option<&str>,
) -> sqlx::Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO proposals
         (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
         VALUES ('skipped-item', 'pending', ?, ?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(run_id)
    .bind(session_id)
    .bind(project_id)
    .bind(tool_name)
    .bind(reasoning)
    .bind(tool_input)
    .bind(&now)
    .execute(&mut *transaction)
    .await?;
    let proposal_id = result.last_insert_rowid();

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

pub async fn create_contact_merge(
    pool: &SqlitePool,
    keep_id: i64,
    absorb_id: i64,
    reasoning: &str,
) -> sqlx::Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let tool_input = serde_json::json!({
        "keep_id": keep_id,
        "absorb_id": absorb_id,
    })
    .to_string();
    let mut transaction = pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO proposals
         (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
         VALUES ('contact-merge', 'pending', NULL, NULL, NULL, NULL, ?, ?, ?, NULL)",
    )
    .bind(reasoning)
    .bind(tool_input)
    .bind(&now)
    .execute(&mut *transaction)
    .await?;
    let proposal_id = result.last_insert_rowid();

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

/// A proposal to set aside time for a message that needs work.
///
/// The third kind this table carries, and the second that touches no run at all. It exists because
/// the `action` triage class — "needs something, but not today" — had nowhere to go: the agent can
/// see that a message needs an hour and could not previously say so anywhere durable.
///
/// The agent never writes the event itself. This row is the whole mechanism: a human approves, and
/// only then does `calendar_events` gain a row. That keeps the roadmap's "draft yes, send never"
/// rule intact with the calendar as the destination.
pub async fn create_calendar_event(
    pool: &SqlitePool,
    email_id: i64,
    title: &str,
    starts_at_local: &str,
    duration_minutes: i64,
    tz: &str,
    reasoning: &str,
) -> sqlx::Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let tool_input = serde_json::json!({
        "email_id": email_id,
        "title": title,
        "starts_at_local": starts_at_local,
        "duration_minutes": duration_minutes,
        "tz": tz,
    })
    .to_string();
    let mut transaction = pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO proposals
         (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
         VALUES ('calendar-event', 'pending', NULL, NULL, NULL, NULL, ?, ?, ?, NULL)",
    )
    .bind(reasoning)
    .bind(tool_input)
    .bind(&now)
    .execute(&mut *transaction)
    .await?;
    let proposal_id = result.last_insert_rowid();

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

/// A run asked GitHub for something the owner's list does not run on its own.
///
/// The seventh `kind` this table carries, and the third that starts no run. The shape is
/// `create_calendar_event`'s and the column convention is `create_team_action_in_transaction`'s:
/// `tool_name` carries the OPERATION's kind -- `pr_comment`, `workflow_run` -- because that is the
/// column the approvals list renders, and a queue saying only "github-action" would make a person
/// open every row to find out what they are agreeing to.
///
/// **Unlike `team-action`, approving this one ACTS.** There is no later tick that picks it up:
/// `github::approve_proposed_operation` runs `gh` on the approval path itself, because a pillar
/// answering synchronously has no pass to be picked up on. Without that, the button would approve
/// nothing.
///
/// `project_id` is NULL like its three siblings, so `wip::open_review_items` does not count
/// these against a project's review ceiling. That is deliberate and it is a real gap: the ceiling
/// that would govern them is the autonomy list itself, and an agent that files a hundred refused
/// operations is an agent filling somebody's approvals queue. Nothing here throttles that yet, and
/// saying so is better than leaving it to be discovered.
pub async fn create_github_action(
    pool: &SqlitePool,
    kind: &str,
    why: &str,
    payload: &str,
) -> sqlx::Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO proposals
         (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
         VALUES ('github-action', 'pending', NULL, NULL, NULL, ?, ?, ?, ?, NULL)",
    )
    .bind(kind)
    .bind(why)
    .bind(payload)
    .bind(&now)
    .execute(&mut *transaction)
    .await?;
    let proposal_id = result.last_insert_rowid();

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

/// A department asked to do something, and this is the question a human answers.
///
/// The fifth `kind`, the third that touches no run, and the first that is filed by an agent about
/// an action the AGENT will not perform. Approving it does not resume anything and does not act:
/// it marks the proposal, and `team_tick` picks the action up on its next pass. That separation is
/// the point — see `team::execute_due_actions` and the design's #7.
///
/// **Written inside the caller's transaction, on purpose.** `team.rs` writes the `team_actions` row
/// and this proposal together or writes neither: an action with no proposal is an action nobody
/// will ever decide, and a proposal with no action is a button that approves nothing. That is why
/// this takes a transaction where its four siblings take a pool.
///
/// `project_id` is NULL, like `calendar-event` and `contact-merge` before it, and the consequence is
/// deliberate: `wip::open_review_items` filters by project, so these never reach the per-project
/// ceiling. The ceiling that governs them is `teams.max_open_actions`, which is per team, because a
/// department has no project to be counted against.
pub(crate) async fn create_team_action_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    kind: &str,
    why: &str,
    payload: &str,
    now: &str,
) -> sqlx::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO proposals
         (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
         VALUES ('team-action', 'pending', NULL, NULL, NULL, ?, ?, ?, ?, NULL)",
    )
    // `tool_name` carries the ACTION's kind — `send_email`, `file_document`. It is the one column
    // the approvals list already renders, and a queue that says only "team-action" would make the
    // person open every row to find out what they are agreeing to.
    .bind(kind)
    .bind(why)
    .bind(payload)
    .bind(now)
    .execute(&mut **transaction)
    .await?;
    let proposal_id = result.last_insert_rowid();

    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'pending', 'created', ?)",
    )
    .bind(proposal_id)
    .bind(now)
    .execute(&mut **transaction)
    .await?;

    Ok(proposal_id)
}

/// A director found a gap in its roster and is asking for somebody to fill it.
///
/// The sixth `kind`, and the third application of the shape `create_calendar_event` established:
/// the agent never writes the `agents` row. A catalogue that a director could write into directly
/// would be permanent house staff created from a sentence a model wrote mid-round, and in a month
/// nobody knows who is who — which is the problem the catalogue was built to solve.
///
/// **Not a `GRANTABLE_ACTIONS` entry**, deliberately, for the reason `list_skipped_items` gives
/// about sharing a door: approving an action means *do that*, approving a recruitment means *keep
/// this person*. One executes and is finished; the other executes nothing and lasts forever. And
/// only one of them is editable at the moment of approval, which no shared button could express.
///
/// `team_run_id` is kept in the payload so that months later somebody can read WHAT WORK made this
/// person be hired. `project_id` is NULL, like every other proposal a department files.
pub async fn create_agent_recruit(
    pool: &SqlitePool,
    team_id: &str,
    team_run_id: &str,
    slug: &str,
    request: &serde_json::Value,
    why: &str,
) -> sqlx::Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut payload = request.clone();
    if let Some(object) = payload.as_object_mut() {
        object.insert("team_id".into(), team_id.into());
        object.insert("team_run_id".into(), team_run_id.into());
        // The id the name will earn, stored beside it so `recruit_pending_for` can ask about it
        // without re-deriving a slug from a name somebody may have edited since.
        object.insert("slug".into(), slug.into());
    }
    let mut transaction = pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO proposals
         (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
         VALUES ('agent-recruit', 'pending', NULL, NULL, NULL, ?, ?, ?, ?, NULL)",
    )
    .bind(slug)
    .bind(why)
    .bind(payload.to_string())
    .bind(&now)
    .execute(&mut *transaction)
    .await?;
    let proposal_id = result.last_insert_rowid();

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

/// Everything a wheel request has to say, as one value.
///
/// A struct rather than seven arguments, and the grouping is not only length: three of these are
/// `&str` and one of them is the thing the person will read and decide on. Two same-typed arguments
/// swapped in a call would compile, and the swap that mattered would put the wrong host in the
/// dialogue — which is exactly the failure spec §5.2's measures exist to prevent.
#[derive(Debug, Clone, Copy)]
pub struct WheelAsk<'a> {
    pub run_id: Option<i64>,
    pub project_id: &'a str,
    /// The row in `browser_sessions` this is about.
    pub session_id: i64,
    pub requested_url: &'a str,
    pub final_url: &'a str,
    /// The complete, literal origin — punycode as stored, never prettified.
    pub origin: &'a str,
    pub reasoning: &'a str,
}

/// An agent hit a wall and is asking for the wheel (spec §4.4 rule 3).
///
/// It lands here and not in `attention.rs` for a reason the spec calls out by name: that module is
/// the owner-presence brake, and what it holds expires in two minutes — the exact opposite of what
/// this needs. A wheel request has to survive the shell being closed, the run ending, and the person
/// going away for a day. **It never expires**, because §4.4 rule 2 says the wheel does not come back
/// by time; only a person decides.
///
/// # What goes into `tool_input`, and why each field is there
///
/// The three measures of spec §5.2 against the confused deputy are all dialogue content, and this is
/// where the dialogue gets its facts:
///
/// - `origin` is the **complete and literal** origin, in the punycode form the policy stored, never
///   abbreviated. `xn--exemp1o-...` is the information; prettifying it is the attack.
/// - `requested_url` and `final_url` are how the agent got there. A permission asked for from a page
///   the agent followed a link to is not the same request as one from a url a person typed, and the
///   only way for a person to tell is to be shown both.
/// - `session_id` is the row in `browser_sessions`, so accepting can find the session without the
///   caller naming it — and so a proposal cannot be pointed at a different session after the fact.
pub async fn create_wheel_request(pool: &SqlitePool, ask: WheelAsk<'_>) -> sqlx::Result<i64> {
    let WheelAsk {
        run_id,
        project_id,
        session_id,
        requested_url,
        final_url,
        origin,
        reasoning,
    } = ask;
    let now = chrono::Utc::now().to_rfc3339();
    let tool_input = serde_json::json!({
        "session_id": session_id,
        "requested_url": requested_url,
        "final_url": final_url,
        "origin": origin,
    })
    .to_string();
    let mut transaction = pool.begin().await?;
    let result = sqlx::query(
        "INSERT INTO proposals
         (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
         VALUES ('browser-wheel', 'pending', ?, NULL, ?, 'browser_handoff', ?, ?, ?, NULL)",
    )
    .bind(run_id)
    .bind(project_id)
    .bind(reasoning)
    .bind(tool_input)
    .bind(&now)
    .execute(&mut *transaction)
    .await?;
    let proposal_id = result.last_insert_rowid();

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

/// Whether somebody has already been asked for under this id and nobody has answered.
///
/// `calendar_proposal_pending_for` exists for the identical reason and says it: *"Without this,
/// every triage pass over the same message would file another one."* A director replans every round
/// with the same prompt and the same gap in front of it, so without this it asks for three lawyers
/// in three rounds.
///
/// Keyed on `tool_name`, which carries the slug, rather than on a `LIKE` over the payload: the id is
/// what would collide in the catalogue, and it is the only field here that is not free text.
pub async fn recruit_pending_for(pool: &SqlitePool, slug: &str) -> sqlx::Result<bool> {
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM proposals
          WHERE kind = 'agent-recruit' AND status = 'pending' AND tool_name = ?
          LIMIT 1",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await?;
    Ok(found.is_some())
}

/// Every recruitment still waiting, and — for a director's prompt — the ones from one run.
pub async fn list_pending_recruits(
    pool: &SqlitePool,
    team_run_id: Option<&str>,
) -> sqlx::Result<Vec<Proposal>> {
    let rows = sqlx::query_as::<_, Proposal>(
        // `errand_id` and `NULL AS errand_name` in master's own shape. A department has no errand
        // and never will, so the id is always NULL here -- but the column has to be SELECTED all
        // the same, because `Proposal` grew both fields and `query_as` hydrates by name. Missing
        // one is not a compile error; it is a row that fails to decode at runtime.
        "SELECT id, kind, status, run_id, session_id, project_id, errand_id,
                NULL AS errand_name, tool_name, reasoning,
                tool_input, read_from, created_at, decided_at
         FROM proposals
         WHERE status = 'pending' AND kind = 'agent-recruit'
         ORDER BY id ASC",
    )
    .fetch_all(pool)
    .await?;
    let Some(team_run_id) = team_run_id else {
        return Ok(rows);
    };
    // Filtered here rather than in SQL, because the run id lives inside the JSON payload and a
    // `LIKE` over it would match a name that happened to contain the id. Read back, compared, done —
    // the list is a handful of rows.
    Ok(rows
        .into_iter()
        .filter(|proposal| {
            proposal
                .tool_input
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .and_then(|payload| {
                    payload
                        .get("team_run_id")
                        .and_then(|value| value.as_str())
                        .map(str::to_owned)
                })
                .is_some_and(|named| named == team_run_id)
        })
        .collect())
}

/// The queue a person works through for departments.
///
/// A door of its own rather than a `kind` argument on `list_pending`, for the reason
/// `list_skipped_items` gives: those two lists are answered by different actions. `list_pending` is
/// work stopped mid-stride that `approve` lets through; this is work that will START when approved,
/// and the run that asked has usually finished by then.
pub async fn list_pending_team_actions(pool: &SqlitePool) -> sqlx::Result<Vec<Proposal>> {
    sqlx::query_as::<_, Proposal>(
        // `errand_id` and `NULL AS errand_name` in master's own shape. A department has no errand
        // and never will, so the id is always NULL here -- but the column has to be SELECTED all
        // the same, because `Proposal` grew both fields and `query_as` hydrates by name. Missing
        // one is not a compile error; it is a row that fails to decode at runtime.
        "SELECT id, kind, status, run_id, session_id, project_id, errand_id,
                NULL AS errand_name, tool_name, reasoning,
                tool_input, read_from, created_at, decided_at
         FROM proposals
         WHERE status = 'pending' AND kind = 'team-action'
         ORDER BY id ASC",
    )
    .fetch_all(pool)
    .await
}

/// Whether a message already has a calendar proposal waiting on a decision.
///
/// Without this, every triage pass over the same `action` message would file another one, and the
/// per-project WIP brake would trip on a queue this feature generated by itself.
pub async fn calendar_proposal_pending_for(pool: &SqlitePool, email_id: i64) -> sqlx::Result<bool> {
    let needle = format!("\"email_id\":{email_id},");
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM proposals
          WHERE kind = 'calendar-event' AND status = 'pending' AND tool_input LIKE ?
          LIMIT 1",
    )
    .bind(format!("%{needle}%"))
    .fetch_optional(pool)
    .await?;
    Ok(found.is_some())
}

pub async fn get(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<Proposal>> {
    sqlx::query_as::<_, Proposal>(
        "SELECT id, kind, status, run_id, session_id, project_id, errand_id,
                NULL AS errand_name, tool_name, reasoning,
                tool_input, read_from, created_at, decided_at
         FROM proposals WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn list_pending(pool: &SqlitePool) -> sqlx::Result<Vec<Proposal>> {
    sqlx::query_as::<_, Proposal>(
        "SELECT id, kind, status, run_id, session_id, project_id, errand_id,
                NULL AS errand_name, tool_name, reasoning,
                tool_input, read_from, created_at, decided_at
         FROM proposals
         WHERE status = 'pending' AND kind = 'action-approval'
         ORDER BY id ASC",
    )
    .fetch_all(pool)
    .await
}

/// The items a job put down overnight, still unread.
///
/// A separate door from `list_pending` rather than a `kind` parameter on it, because the two lists
/// are answered by different actions and mixing them would put a decision that resumes a run next
/// to one that resumes nothing. `list_pending` is a *queue*: everything in it is work stopped
/// mid-stride, and `approve` lets it through. This is a *record*: the work was never started, the
/// job moved on hours ago, and the only thing left to do with it is read it and put it away. Offered
/// through the same endpoint they would share an approve button, and `reject_proposal` guards on
/// `kind = 'action-approval'`, so half of it would answer 409 to a click that looked identical.
///
/// Newest first, unlike `list_pending`'s ascending order, and for the opposite reason: an approval
/// queue is worked front to back, and this is read the morning after.
pub async fn list_skipped_items(pool: &SqlitePool) -> sqlx::Result<Vec<Proposal>> {
    sqlx::query_as::<_, Proposal>(
        "SELECT id, kind, status, run_id, session_id, project_id, errand_id,
                NULL AS errand_name, tool_name, reasoning,
                tool_input, read_from, created_at, decided_at
         FROM proposals
         WHERE status = 'pending' AND kind = 'skipped-item'
         ORDER BY id DESC",
    )
    .fetch_all(pool)
    .await
}

/// The kinds that are read and put away rather than decided.
///
/// Both name work that never happened and cannot be made to happen from here: a job item skipped
/// hours ago in a tree that has moved on, and an action the barrier refused in a turn that has
/// ended. Neither has anything to resume, which is what separates them from `action-approval`.
///
/// An allow-list and not "anything that is not an action-approval", so a sixth kind arriving later
/// has to say out loud that dismissing it is the right verb.
const DISMISSABLE_KINDS: [&str; 2] = ["skipped-item", "refused-action"];

/// Puts a skipped item away once it has been read.
///
/// Without it the listing above only ever grows: nothing else moves a `skipped-item` off `pending`,
/// so a week of night jobs would bury the one item that mattered, and a list that cannot be cleared
/// stops being read — which is the same outcome as having no route at all, arrived at more slowly.
///
/// `dismissed`, not `rejected` or `approved`: neither of those is true. Nothing was proposed, so
/// there is nothing to refuse, and nothing runs on the way out — where `reject_proposal` also
/// releases the paused run's worktree, this touches no run at all. The job's own machinery released
/// everything when it skipped the item and carried on.
///
/// Reuses `RejectError` rather than growing a near-identical twin: the three cases a caller has to
/// tell apart — gone, already decided, and the database said no — are the same three.
pub async fn dismiss_skipped_item(pool: &SqlitePool, id: i64) -> Result<(), RejectError> {
    let proposal = get(pool, id).await?.ok_or(RejectError::NotFound)?;
    if !DISMISSABLE_KINDS.contains(&proposal.kind.as_str()) || proposal.status != "pending" {
        return Err(RejectError::NotPending);
    }
    // Compare-and-set, so a second dismissal racing this one is reported rather than answered 204.
    if !transition(pool, id, "dismissed", "dismissed by user").await? {
        return Err(RejectError::NotPending);
    }
    Ok(())
}

/// Add a line to a proposal's history without changing its status.
///
/// For the case spec §4.4a describes: a wheel request that was accepted and whose window then failed
/// to open. The proposal is no longer pending, so `transition` cannot carry the news, and the person
/// needs to be told why the window they asked for is not there. The event is written with the same
/// status on both sides, which is what says "nothing was decided here, something happened".
pub async fn note(pool: &SqlitePool, id: i64, note: &str) -> sqlx::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let Some(proposal) = get(pool, id).await? else {
        return Ok(());
    };
    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(&proposal.status)
    .bind(&proposal.status)
    .bind(note)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn transition(
    pool: &SqlitePool,
    id: i64,
    to_status: &str,
    note: &str,
) -> sqlx::Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;
    let transitioned =
        transition_in_transaction(&mut transaction, id, to_status, note, &now).await?;

    if !transitioned {
        return Ok(false);
    }

    transaction.commit().await?;
    Ok(true)
}

pub(crate) async fn transition_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    id: i64,
    to_status: &str,
    note: &str,
    at: &str,
) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE proposals SET status = ?, decided_at = ? WHERE id = ? AND status = 'pending'",
    )
    .bind(to_status)
    .bind(at)
    .bind(id)
    .execute(&mut **transaction)
    .await?;

    if result.rows_affected() != 1 {
        return Ok(false);
    }

    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, 'pending', ?, ?, ?)",
    )
    .bind(id)
    .bind(to_status)
    .bind(note)
    .bind(at)
    .execute(&mut **transaction)
    .await?;

    Ok(true)
}

// Reject = discard: reject the pending proposal and discard its paused run.
pub async fn reject_proposal(pool: &SqlitePool, id: i64) -> Result<(), RejectError> {
    let proposal = get(pool, id).await?.ok_or(RejectError::NotFound)?;
    if proposal.kind != "action-approval" || proposal.status != "pending" {
        return Err(RejectError::NotPending);
    }
    // `transition` is a compare-and-set and returns false when the proposal is no longer pending.
    // Discarding that answered 204 No Content to a rejection that had lost the race to a concurrent
    // approve: the user was told their refusal landed while the resume run was already executing
    // the action they refused. Report the loss instead of hiding it.
    if !transition(pool, id, "rejected", "rejected by user").await? {
        return Err(RejectError::NotPending);
    }
    if let Some(run_id) = proposal.run_id {
        crate::worktree::release(pool, run_id).await?;
    }
    Ok(())
}

/// Records a class-scoped authorization for a resume run — test fixture only.
///
/// Production does not call this and must not start: `resume_approved_run` inlines the same INSERT
/// inside its transaction, because the grant has to land atomically with the supersede, the resume
/// row, and the proposal's approval. A standalone helper is a second, non-atomic way to do the same
/// thing, so `#[cfg(test)]` keeps it available to the tests that need to mint a grant while making
/// it unavailable to anything else.
#[cfg(test)]
pub async fn grant_action(
    pool: &SqlitePool,
    resume_run_id: i64,
    tool_name: &str,
    action_class: Option<&str>,
    proposal_id: i64,
) -> sqlx::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO action_grants (run_id, tool_name, action_class, proposal_id, created_at, consumed_at)
         VALUES (?, ?, ?, ?, ?, NULL)",
    )
    .bind(resume_run_id)
    .bind(tool_name)
    .bind(action_class)
    .bind(proposal_id)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Whether this run's grant covers `action_class`. `Ok(true)` iff it does.
///
/// The grant is scoped to a class and lasts the run, not a single call. What the human agreed to is
/// a kind of action: keyed on the exact input they read, an approval of `git push origin main`
/// parked the resume again on `git push origin other` — the same decision asked twice. Keyed on the
/// tool alone it would be worse, since `tool_name` is the constant `"Bash"` for every shell action.
/// The class is the only key that is neither.
///
/// `consumed_at` records the FIRST use and is an audit stamp, not a fuse: `COALESCE` keeps the
/// original timestamp, so the row says when the authorization began rather than when it was last
/// exercised. A class the grant does not cover matches no row, so an unauthorized attempt leaves
/// even that stamp alone.
///
/// `=` never matches NULL, so a pre-0055 grant — minted under the tool_input rule, unable to say
/// which class it stood for — authorizes nothing at all.
///
/// The §8.4 invariant is unchanged: this never lifts a `deny`, because the caller only reaches it
/// for a `pending_approval`.
///
/// `queued_request_id IS NULL` excludes the rows that record a TAKEOVER (migration 0054) rather than
/// an authorization, and the class rule is what makes that exclusion matter more than it did. A
/// takeover row was already forbidden to authorize the one action it names; covering a class for the
/// rest of the run, it would authorize an open-ended number of them — every merge the run cared to
/// attempt, off the back of a row minted to say the queue had taken merging away from it.
/// The authorization this run was resumed with and never used, if there is one.
///
/// A resume exists to carry out one action a human agreed to. `grant_covers_class` stamps
/// `consumed_at` the first time that action is attempted, so a grant still NULL when the run reaches
/// its end says the run finished without ever doing the thing it was resumed for.
///
/// **Measured 2026-08-17** (`.ai/eval/ABLATION.md`, T1×H3): two resumed runs refused the instruction
/// they were given, answered with a question nobody was there to read, and were recorded
/// `completed`, `exit_code: 0`, `gate_status: passed` — the gate green precisely because the tree was
/// untouched. The run that did continue (900270) consumed its grant; the two that did not never
/// touched it. The distinction the record was missing is already in this table.
///
/// A takeover row (`queued_request_id IS NOT NULL`) authorizes nothing and is never consumed by
/// design, so it is excluded — otherwise every queued merge would report itself as work not done.
pub async fn unconsumed_grant(
    pool: &SqlitePool,
    run_id: i64,
) -> sqlx::Result<Option<(String, i64)>> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT tool_name, proposal_id FROM action_grants
         WHERE run_id = ? AND consumed_at IS NULL AND queued_request_id IS NULL",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
}

pub async fn grant_covers_class(
    pool: &SqlitePool,
    run_id: i64,
    action_class: &str,
) -> sqlx::Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    // SQLite counts a row the UPDATE matched as affected even when `COALESCE` wrote back the value
    // already there, which is what lets the second and later actions of a covered class read as
    // authorized rather than as a missing grant.
    let result = sqlx::query(
        "UPDATE action_grants SET consumed_at = COALESCE(consumed_at, ?)
         WHERE run_id = ? AND action_class = ? AND queued_request_id IS NULL",
    )
    .bind(&now)
    .bind(run_id)
    .bind(action_class)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// The queued request that already has this action, if the approval handed it to the queue instead
/// of back to the run (migration 0054).
///
/// It consumes NOTHING: this is a standing fact about where the work went, and it has to answer
/// identically however many times a run asks. A run that gets a different answer on its second
/// attempt would be one that could wait out the refusal.
///
/// Keyed on the exact `tool_input`, and deliberately NOT moved to the class key that `action_grants`
/// otherwise took (migration 0055). The two keys answer different questions. A class is the right
/// scope for "what did the human agree to", because they agreed to a kind of action. It is the wrong
/// scope for "where did this action go": one merge was queued as one request, and answering for the
/// whole class would tell a run attempting a SECOND merge that it is already queued as #77 when
/// nothing of the sort happened — sending it on believing work is in hand that nobody has.
///
/// **The two must never both match the same row**, which is what the `IS NULL` / `IS NOT NULL` pair
/// buys: the row minted to record the takeover would otherwise be a grant authorising the very
/// action it records having taken away.
pub async fn matching_queued_request(
    pool: &SqlitePool,
    run_id: i64,
    tool_name: &str,
    tool_input: &str,
) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT queued_request_id FROM action_grants
          WHERE run_id = ? AND tool_name = ? AND tool_input IS ? AND queued_request_id IS NOT NULL",
    )
    .bind(run_id)
    .bind(tool_name)
    .bind(tool_input)
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    /// The distinction the run record was missing: resumed and acted, against resumed and did not.
    ///
    /// Both runs end `completed` with exit 0, and no field on `runs` separates them. This one does,
    /// and it was already being written — `grant_covers_class` stamps `consumed_at` on the first
    /// attempt, so a grant still NULL at the end is a resume that never carried out its errand.
    #[tokio::test]
    async fn a_grant_the_resumed_run_never_used_is_reported_and_one_it_used_is_not() {
        let pool = test_pool().await;
        grant_action(&pool, 900, "Bash", Some("write-local"), 61)
            .await
            .unwrap();
        grant_action(&pool, 901, "Bash", Some("write-local"), 62)
            .await
            .unwrap();

        // 901 attempts the action it was resumed for; 900 finishes without ever trying.
        assert!(grant_covers_class(&pool, 901, "write-local").await.unwrap());

        assert_eq!(
            unconsumed_grant(&pool, 900).await.unwrap(),
            Some(("Bash".to_string(), 61)),
            "a resume that never attempted its action must be reportable"
        );
        assert_eq!(
            unconsumed_grant(&pool, 901).await.unwrap(),
            None,
            "a resume that did the work must not be reported as if it had not"
        );
        assert_eq!(
            unconsumed_grant(&pool, 902).await.unwrap(),
            None,
            "a run that was never resumed has no errand to have skipped"
        );
    }

    #[tokio::test]
    async fn create_pending_proposal_records_row_and_initial_event() {
        let pool = test_pool().await;

        let id = create_action_approval(
            &pool,
            1,
            Some("s1"),
            Some("p"),
            "Bash",
            "push needs approval",
            Some(r#"{"command":"git push"}"#),
        )
        .await
        .unwrap();

        let proposal = get(&pool, id).await.unwrap().unwrap();
        assert_eq!(proposal.kind, "action-approval");
        assert_eq!(proposal.status, "pending");
        assert_eq!(proposal.run_id, Some(1));
        assert_eq!(proposal.session_id.as_deref(), Some("s1"));
        assert_eq!(proposal.tool_name.as_deref(), Some("Bash"));
        assert!(!proposal.reasoning.is_empty());
        assert_eq!(proposal.decided_at, None);

        let events = sqlx::query_scalar::<_, String>(
            "SELECT to_status FROM proposal_events WHERE proposal_id = ? ORDER BY id ASC",
        )
        .bind(id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(events, vec!["pending".to_string()]);
    }

    #[tokio::test]
    async fn dedup_rejects_a_second_open_proposal_for_the_same_run() {
        let pool = test_pool().await;

        create_action_approval(
            &pool,
            7,
            Some("s7"),
            Some("p"),
            "Bash",
            "first approval",
            None,
        )
        .await
        .unwrap();

        let duplicate = create_action_approval(
            &pool,
            7,
            Some("s7"),
            Some("p"),
            "Bash",
            "second approval",
            None,
        )
        .await;

        assert!(duplicate.is_err());
    }

    #[tokio::test]
    async fn transition_pending_to_approved_appends_event_and_sets_decided_at() {
        let pool = test_pool().await;
        let id = create_action_approval(
            &pool,
            20,
            Some("s20"),
            Some("p"),
            "Bash",
            "approval required",
            None,
        )
        .await
        .unwrap();

        assert!(
            transition(&pool, id, "approved", "user approved")
                .await
                .unwrap()
        );

        let proposal = get(&pool, id).await.unwrap().unwrap();
        assert_eq!(proposal.status, "approved");
        assert!(proposal.decided_at.is_some());

        let events = sqlx::query_as::<_, (Option<String>, String, String)>(
            "SELECT from_status, to_status, note FROM proposal_events WHERE proposal_id = ? ORDER BY id ASC",
        )
        .bind(id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, None);
        assert_eq!(events[0].1, "pending");
        assert_eq!(events[1].0.as_deref(), Some("pending"));
        assert_eq!(events[1].1, "approved");
        assert_eq!(events[1].2, "user approved");
    }

    #[tokio::test]
    async fn transition_of_a_missing_or_non_pending_proposal_is_false() {
        let pool = test_pool().await;

        assert!(
            !transition(&pool, 999_999, "approved", "missing")
                .await
                .unwrap()
        );

        let id = create_action_approval(
            &pool,
            30,
            Some("s30"),
            Some("p"),
            "Bash",
            "approval required",
            None,
        )
        .await
        .unwrap();
        assert!(
            transition(&pool, id, "approved", "user approved")
                .await
                .unwrap()
        );
        assert!(!transition(&pool, id, "rejected", "too late").await.unwrap());
    }

    #[tokio::test]
    async fn list_pending_returns_only_pending_action_approvals_ordered() {
        let pool = test_pool().await;

        let first = create_action_approval(
            &pool,
            10,
            Some("s10"),
            Some("p"),
            "Bash",
            "first pending",
            None,
        )
        .await
        .unwrap();
        let second = create_action_approval(
            &pool,
            11,
            Some("s11"),
            Some("p"),
            "Bash",
            "second pending",
            None,
        )
        .await
        .unwrap();
        let rejected = create_action_approval(
            &pool,
            12,
            Some("s12"),
            Some("p"),
            "Bash",
            "will be rejected",
            None,
        )
        .await
        .unwrap();
        assert!(
            transition(&pool, rejected, "rejected", "user rejected")
                .await
                .unwrap()
        );

        let pending = list_pending(&pool).await.unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|proposal| proposal.id)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        assert!(
            pending
                .iter()
                .all(|proposal| proposal.kind == "action-approval" && proposal.status == "pending")
        );
    }

    /// The two listings must not leak into each other. `list_pending` is a queue somebody works
    /// through; this is a record somebody reads. A `skipped-item` appearing in the queue would be
    /// offered an approve button that resumes nothing, and an action approval appearing here would
    /// be offered a dismiss that abandons a run still holding a worktree.
    #[tokio::test]
    async fn the_skipped_items_listing_and_the_approval_queue_stay_apart() {
        let pool = test_pool().await;

        let approval =
            create_action_approval(&pool, 10, Some("s10"), Some("p"), "Bash", "asked", None)
                .await
                .unwrap();
        let older =
            create_skipped_item(&pool, 11, Some("s11"), Some("p"), "Bash", "set aside", None)
                .await
                .unwrap();
        let newer = create_skipped_item(
            &pool,
            12,
            Some("s12"),
            Some("p"),
            "Bash",
            "set aside later",
            None,
        )
        .await
        .unwrap();

        let skipped = list_skipped_items(&pool).await.unwrap();
        // Newest first: this is read the morning after, not worked front to back.
        assert_eq!(
            skipped.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![newer, older]
        );

        let queue = list_pending(&pool).await.unwrap();
        assert_eq!(
            queue.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![approval]
        );
    }

    #[tokio::test]
    async fn dismissing_a_skipped_item_takes_it_off_the_listing_once() {
        let pool = test_pool().await;
        let id = create_skipped_item(&pool, 11, Some("s11"), Some("p"), "Bash", "set aside", None)
            .await
            .unwrap();

        dismiss_skipped_item(&pool, id).await.unwrap();

        assert!(list_skipped_items(&pool).await.unwrap().is_empty());
        // A second dismissal lost the race and is told so, rather than being answered 204 for a
        // change it did not make.
        assert!(matches!(
            dismiss_skipped_item(&pool, id).await,
            Err(RejectError::NotPending)
        ));
    }

    /// Dismiss is not a third spelling of reject. An action approval holds a paused run and a
    /// worktree, and putting it away without releasing either is how a project loses its
    /// exclusivity slot until somebody restarts the daemon.
    #[tokio::test]
    async fn dismiss_refuses_anything_that_is_not_a_skipped_item() {
        let pool = test_pool().await;
        let approval =
            create_action_approval(&pool, 10, Some("s10"), Some("p"), "Bash", "asked", None)
                .await
                .unwrap();

        assert!(matches!(
            dismiss_skipped_item(&pool, approval).await,
            Err(RejectError::NotPending)
        ));
        assert!(matches!(
            dismiss_skipped_item(&pool, 999_999).await,
            Err(RejectError::NotFound)
        ));
        // Still in the queue, still pending, still holding whatever it was holding.
        assert_eq!(list_pending(&pool).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn reject_marks_proposal_rejected_and_cancels_the_run() {
        let pool = test_pool().await;
        let result = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('reject this run', 'awaiting_approval', 'worktree', '2026-07-20T12:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let run_id = result.last_insert_rowid();
        let proposal_id = create_action_approval(
            &pool,
            run_id,
            Some("s"),
            Some("p"),
            "Bash",
            "push needs approval",
            None,
        )
        .await
        .unwrap();

        reject_proposal(&pool, proposal_id).await.unwrap();

        let proposal = get(&pool, proposal_id).await.unwrap().unwrap();
        assert_eq!(proposal.status, "rejected");
        let run_status = sqlx::query_scalar::<_, String>("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(run_status, "cancelled");
    }

    #[tokio::test]
    async fn reject_non_pending_proposal_is_conflict() {
        let pool = test_pool().await;
        let proposal_id = create_action_approval(
            &pool,
            40,
            Some("s"),
            Some("p"),
            "Bash",
            "push needs approval",
            None,
        )
        .await
        .unwrap();
        assert!(
            transition(&pool, proposal_id, "approved", "x")
                .await
                .unwrap()
        );

        let result = reject_proposal(&pool, proposal_id).await;

        assert!(matches!(result, Err(RejectError::NotPending)));
    }

    #[tokio::test]
    async fn reject_unknown_proposal_is_not_found() {
        let pool = test_pool().await;

        let result = reject_proposal(&pool, 999_999).await;

        assert!(matches!(result, Err(RejectError::NotFound)));
    }

    /// The row that RECORDS a takeover must never authorize the thing it records having taken away.
    ///
    /// The class key raised the stakes on this rather than settling them. Under the tool_input rule
    /// the row could at worst have authorized the one action it names; covering a class for the rest
    /// of the run, it would authorize every merge the run went on to attempt — off a row minted to
    /// say that merging had been taken out of its hands.
    #[tokio::test]
    async fn a_row_recording_a_takeover_is_not_a_grant() {
        let pool = test_pool().await;
        let input = r#"{"command":"git merge feature/x"}"#;
        // Written the long way because no helper mints this shape: `grant_action` writes an
        // authorization, and the point of this row is that it is the other thing. It carries BOTH
        // keys — the class the classifier assigned and the exact input the human read — so the test
        // cannot pass merely by failing to match.
        sqlx::query(
            "INSERT INTO action_grants
             (run_id, tool_name, tool_input, action_class, proposal_id, created_at, consumed_at, queued_request_id)
             VALUES (100, 'Bash', ?, 'push-merge-deploy', 5, '2026-01-01T00:00:00Z', NULL, 77)",
        )
        .bind(input)
        .execute(&pool)
        .await
        .unwrap();

        assert!(
            !grant_covers_class(&pool, 100, "push-merge-deploy")
                .await
                .unwrap(),
            "a takeover must not cover the class it records having lost"
        );
        assert_eq!(
            matching_queued_request(&pool, 100, "Bash", input)
                .await
                .unwrap(),
            Some(77)
        );
        // Asked twice, it answers the same: this is a standing fact, not a single-use pass. A run
        // that got a different answer on its second attempt could wait the refusal out.
        assert_eq!(
            matching_queued_request(&pool, 100, "Bash", input)
                .await
                .unwrap(),
            Some(77)
        );
    }

    /// And the other direction: an ordinary grant is invisible to the takeover lookup, so a run that
    /// legitimately holds one is never told its action was queued when it was not.
    #[tokio::test]
    async fn an_ordinary_grant_is_not_mistaken_for_a_takeover() {
        let pool = test_pool().await;
        let input = r#"{"command":"git push origin main"}"#;
        grant_action(&pool, 101, "Bash", Some("push-merge-deploy"), 5)
            .await
            .unwrap();

        assert_eq!(
            matching_queued_request(&pool, 101, "Bash", input)
                .await
                .unwrap(),
            None
        );
        assert!(
            grant_covers_class(&pool, 101, "push-merge-deploy")
                .await
                .unwrap()
        );
    }

    /// The grant covers its class for the REST of the run, and `consumed_at` is an audit stamp of
    /// when it was first used — not a fuse. A second action of the same class is still covered, and
    /// still reads back the FIRST timestamp, so the record says when the authorization began rather
    /// than when it was last touched.
    #[tokio::test]
    async fn a_grant_covers_its_class_for_every_later_action_of_that_class() {
        let pool = test_pool().await;

        grant_action(&pool, 100, "Bash", Some("push-merge-deploy"), 5)
            .await
            .unwrap();

        assert!(
            grant_covers_class(&pool, 100, "push-merge-deploy")
                .await
                .unwrap()
        );

        let first_use = sqlx::query_scalar::<_, Option<String>>(
            "SELECT consumed_at FROM action_grants WHERE run_id = ?",
        )
        .bind(100)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(first_use.is_some());

        assert!(
            grant_covers_class(&pool, 100, "push-merge-deploy")
                .await
                .unwrap(),
            "the grant is not spent by its first use"
        );

        let still_first_use = sqlx::query_scalar::<_, Option<String>>(
            "SELECT consumed_at FROM action_grants WHERE run_id = ?",
        )
        .bind(100)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            still_first_use, first_use,
            "consumed_at records first use, so a later use must not overwrite it"
        );
    }

    /// Covering a class for the rest of a run is only safe because the class is the boundary. A
    /// push approval reaches pushes and nothing else, and the attempt leaves the grant untouched —
    /// including its first-use stamp, which an unauthorized action must not create.
    #[tokio::test]
    async fn a_grant_does_not_cover_a_different_class() {
        let pool = test_pool().await;

        grant_action(&pool, 101, "Bash", Some("push-merge-deploy"), 6)
            .await
            .unwrap();

        assert!(
            !grant_covers_class(&pool, 101, "self-governing-file")
                .await
                .unwrap(),
            "a grant for one class must not authorize another"
        );

        let consumed_at = sqlx::query_scalar::<_, Option<String>>(
            "SELECT consumed_at FROM action_grants WHERE run_id = ?",
        )
        .bind(101)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(consumed_at.is_none());

        assert!(
            grant_covers_class(&pool, 101, "push-merge-deploy")
                .await
                .unwrap(),
            "the approved class itself must still be authorized"
        );
    }

    #[tokio::test]
    async fn a_grant_with_no_class_authorizes_nothing() {
        // Rows predating the action_class column have a NULL class. They cannot prove what kind of
        // action was approved, so they authorize nothing rather than everything — and `=` never
        // matches NULL, which is what makes that true for EVERY class rather than for the ones
        // someone remembered to enumerate.
        let pool = test_pool().await;

        grant_action(&pool, 103, "Bash", None, 8).await.unwrap();

        for class in ["push-merge-deploy", "unrecognized", "and-chain"] {
            assert!(
                !grant_covers_class(&pool, 103, class).await.unwrap(),
                "a classless grant must not cover {class}"
            );
        }
    }

    #[tokio::test]
    async fn a_class_check_for_an_unknown_run_is_false() {
        let pool = test_pool().await;

        assert!(
            !grant_covers_class(&pool, 999_999, "push-merge-deploy")
                .await
                .unwrap()
        );
    }
}
