//! Team reports and notes exchanged with owners and colleagues.

use super::*;

/// How many times one run may speak to its owner unbidden.
///
/// `MAX_ROUNDS_CEILING` director nodes plus the delivery is seven — the most nodes that could ever
/// have something to say — and eight leaves one over. It is a belt rather than a brake: what
/// actually bounds this is that only a DIRECTOR may report and a run has a small, fixed number of
/// director nodes.
pub const MAX_REPORTS_PER_RUN: i64 = 8;

/// A department saying something to its owner.
#[derive(Debug, serde::Deserialize)]
pub struct ReportRequest {
    pub body: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ReportResponse {
    pub id: i64,
    pub outcome: String,
}

/// Why a department could not speak. Every variant is handed to the AGENT, mid-turn.
#[derive(Debug)]
pub enum ReportError {
    /// Not a `Scope::TeamRun` at all.
    NotADepartment,
    /// The run named by the key is gone.
    NoSuchRun,
    /// A specialist tried. A department speaks to its owner with one voice, and the sentence says
    /// what to do instead — the same shape `propose_teammate` uses for the same refusal.
    NotTheDirector,
    /// This run was not pointed at a conversation, which is the ordinary state of nearly every run
    /// and therefore not an error to be retried. Named on its own so the sentence can say so.
    NowhereToReport,
    /// It was pointed at one and that conversation has since been archived. Distinct from
    /// `NowhereToReport` because they lead somewhere different: one means this department was never
    /// given a voice, the other that the room it was given has been closed.
    DestinationGone(String),
    /// Nothing to say.
    Empty,
    /// This run has spoken as often as it may.
    Ceiling(i64),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ReportError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotADepartment => {
                formatter.write_str("only a department reports to the owner this way")
            }
            Self::NoSuchRun => formatter.write_str("no such team run"),
            Self::NotTheDirector => formatter.write_str(
                "only the director speaks for this department — put it in your answer and let the \
                 director pass it on",
            ),
            Self::NowhereToReport => formatter.write_str(
                "this department was not pointed at a conversation when it was started, so there is \
                 nowhere to say this; put it in your delivery instead",
            ),
            Self::DestinationGone(chat_id) => write!(
                formatter,
                "the conversation `{chat_id}` this department was told to report in has been \
                 archived; put it in your delivery instead"
            ),
            Self::Empty => formatter.write_str("say something, or say nothing at all"),
            Self::Ceiling(ceiling) => write!(
                formatter,
                "this department has already spoken {ceiling} times this run, which is as many as \
                 it may; put the rest in your delivery"
            ),
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}

/// A director says something in the conversation its run was pointed at. Nothing runs.
///
/// **The destination is read from the run and never from the request.** `team_runs.report_to_chat_id`
/// is written when the run is started, by the person starting it; there is no parameter here, no
/// tool that takes one, and no route a department could use to discover what conversations exist. A
/// department cannot choose who it talks to — it can only answer to the address it was given.
///
/// **Only the director**, narrowed here against `team_runs.director_run_id` exactly as
/// `propose_teammate` is, and for the same two reasons: a department speaks to its owner with one
/// voice, and the key names the RUN rather than the node, so this cannot be a question for
/// `auth::permits`.
pub async fn report(
    state: &AppState,
    scope: &crate::auth::Scope,
    headers: &axum::http::HeaderMap,
    request: &ReportRequest,
) -> Result<ReportResponse, ReportError> {
    let crate::auth::Scope::TeamRun(team_run_id) = scope else {
        return Err(ReportError::NotADepartment);
    };

    let body = request.body.trim();
    if body.is_empty() {
        return Err(ReportError::Empty);
    }

    let destination: Option<String> =
        sqlx::query_scalar("SELECT report_to_chat_id FROM team_runs WHERE id = ?")
            .bind(team_run_id)
            .fetch_optional(&state.pool)
            .await?
            .ok_or(ReportError::NoSuchRun)?;
    let Some(chat_id) = destination else {
        return Err(ReportError::NowhereToReport);
    };

    // The director check comes AFTER the destination one on purpose. A specialist calling this in a
    // department that reports nowhere should be told the thing it can act on — there is nowhere to
    // say this, put it in your answer — rather than a rule about who may speak that would not have
    // helped it even if it were the director.
    let Some((from_agent_id, from_run_id)) =
        agent_of_caller(&state.pool, team_run_id, headers).await?
    else {
        return Err(ReportError::NotTheDirector);
    };
    if calling_node(&state.pool, team_run_id, headers).await != Caller::Director {
        return Err(ReportError::NotTheDirector);
    }

    // Checked again here even though `start_with` refused an absent one: a run lasts up to four
    // hours and a conversation can be archived inside that. Reported as its own refusal rather than
    // written into a conversation nobody will open again.
    if crate::chats::brain_of(&state.pool, &chat_id)
        .await?
        .is_none()
    {
        return Err(ReportError::DestinationGone(chat_id));
    }

    let said: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_notices WHERE team_run_id = ?")
        .bind(team_run_id)
        .fetch_one(&state.pool)
        .await?;
    if said >= MAX_REPORTS_PER_RUN {
        return Err(ReportError::Ceiling(MAX_REPORTS_PER_RUN));
    }

    let id = crate::chats::notices::post(
        &state.pool,
        &chat_id,
        team_run_id,
        &from_agent_id,
        from_run_id,
        body,
    )
    .await?;

    Ok(ReportResponse {
        id,
        // Says the part a model would otherwise assume away: it was SHOWN, not answered. A director
        // that believes it has started a conversation waits for a reply that is never coming, and
        // spends its remaining turns waiting.
        outcome:
            "shown to the owner. Nobody will answer it — it is a message on their screen, not \
                  a question — so carry on, and put anything that matters in your delivery as well."
                .to_owned(),
    })
}

/// One member of a department leaving words for another.
#[derive(Debug, serde::Deserialize)]
pub struct TeamNoteRequest {
    /// The colleague's `agents.id`, as printed on the left of their line in `roster_lines`.
    pub to: String,
    pub body: String,
}

/// What the agent is told, and it is a sentence for `ProposeActionResponse`'s reason: the reader is
/// a model deciding what to do next, and "they may not be started again this run" leads somewhere
/// different from "filed".
#[derive(Debug, serde::Serialize)]
pub struct TeamNoteResponse {
    pub id: i64,
    pub outcome: String,
}

/// Why a note could not be left. Every variant is handed to the AGENT, mid-turn, while it can still
/// do something about it — which is why `NoSuchColleague` names the roster rather than saying "no".
#[derive(Debug)]
pub enum NoteError {
    /// Not a `Scope::TeamRun` at all.
    NotADepartment,
    /// The run named by the key is gone.
    NoSuchRun,
    /// The daemon cannot tell which node is calling: no header, a stale id, or one belonging to
    /// another run. Refused rather than attributed to the department at large — an unattributed note
    /// is a second brief with no way to weigh it, which is the one thing the receiving node cannot
    /// recover from.
    UnknownNode,
    /// `to` is not on this department's roster, which is listed so the model can fix it.
    NoSuchColleague {
        asked: String,
        roster: Vec<String>,
    },
    /// `to` is the caller. Not harmful, and refused anyway: the words would arrive in the caller's
    /// own next prompt as though a colleague had sent them, which is a turn arguing with itself.
    Yourself,
    /// Nothing to say. A note with no words is an entry in the queue that delivers nothing and can
    /// never be delivered again.
    Empty,
    /// This run has left as many notes as it may.
    Ceiling(i64),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for NoteError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for NoteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotADepartment => {
                formatter.write_str("only a member of a department leaves notes for its colleagues")
            }
            Self::NoSuchRun => formatter.write_str("no such team run"),
            Self::UnknownNode => formatter.write_str(
                "this call does not say which node of the department is making it, and a note has                  to be signed by somebody",
            ),
            Self::NoSuchColleague { asked, roster } => write!(
                formatter,
                "`{asked}` is not in this department. Its members are: {}",
                roster.join(", ")
            ),
            Self::Yourself => formatter.write_str(
                "that is you — put it in your own answer instead, which is where your own findings go",
            ),
            Self::Empty => formatter.write_str("say something, or say nothing at all"),
            Self::Ceiling(ceiling) => write!(
                formatter,
                "this department has already left {ceiling} notes this run, which is as many as it                  may; put it in your answer instead"
            ),
            // The raw error, as all four siblings in this file render theirs. A friendlier sentence
            // here would read better to a model and would be the only one of five that hides what
            // actually happened — and the reader of a 500 is a person looking at a log, not the
            // agent, which got its answer and moved on.
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}

/// Files one member's words for another, and does nothing else: nobody is woken and no round moves.
///
/// **Who is speaking is read from the call and never from the body.** `Scope::TeamRun` names the
/// run, `RUN_ID_HEADER` names the node, and `agent_of_caller` turns the two into an `agents.id`. A
/// `from` field on the request would let a specialist sign a colleague's name to its own finding —
/// and the receiving node, reading a quoted paragraph attributed by the daemon, has no way at all to
/// check it.
///
/// **The roster is the address book and the boundary at once.** A department may write to its own
/// members and to nobody else: there is no cross-team address, no run id to name, and no way to
/// reach a conversation. `admits`-style chain brakes are not needed here for the reason
/// `MAX_RELAY_DEPTH` exists at all — a chain can only grow one hop per ROUND, and rounds are already
/// bounded by `max_rounds` and by `DRY_ROUNDS_TO_STOP`. `MAX_NOTES_PER_RUN` is the belt.
pub async fn send_note(
    state: &AppState,
    scope: &crate::auth::Scope,
    headers: &axum::http::HeaderMap,
    request: &TeamNoteRequest,
) -> Result<TeamNoteResponse, NoteError> {
    let crate::auth::Scope::TeamRun(team_run_id) = scope else {
        return Err(NoteError::NotADepartment);
    };

    let body = request.body.trim();
    if body.is_empty() {
        return Err(NoteError::Empty);
    }

    let team_id: String = sqlx::query_scalar("SELECT team_id FROM team_runs WHERE id = ?")
        .bind(team_run_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(NoteError::NoSuchRun)?;

    let Some((from_agent_id, from_run_id)) =
        agent_of_caller(&state.pool, team_run_id, headers).await?
    else {
        return Err(NoteError::UnknownNode);
    };

    let asked = request.to.trim();
    if asked == from_agent_id {
        return Err(NoteError::Yourself);
    }
    // `addressable` and NOT `roster`, and the first draft of this used `roster` with a comment
    // asserting the director was on it. It is not — `team_members` holds the specialists and the
    // director lives on `teams.director_agent_id` — so that draft refused the one message this
    // whole feature exists to carry. A test caught it; the comment would not have.
    let roster = addressable(&state.pool, &team_id)
        .await
        .map_err(|error| match error {
            TeamError::Db(error) => NoteError::Db(error),
            _ => NoteError::NoSuchRun,
        })?;
    if !roster.iter().any(|member| member == asked) {
        return Err(NoteError::NoSuchColleague {
            asked: asked.to_owned(),
            roster,
        });
    }

    // Counted per RUN and not per member: what the ceiling protects against is a department talking
    // instead of working, and that is a property of the department rather than of any one of them.
    let left = crate::team_notes::count_for_run(&state.pool, team_run_id).await?;
    if left >= crate::team_notes::MAX_NOTES_PER_RUN {
        return Err(NoteError::Ceiling(crate::team_notes::MAX_NOTES_PER_RUN));
    }

    let id = crate::team_notes::leave(
        &state.pool,
        team_run_id,
        &from_agent_id,
        from_run_id,
        asked,
        body,
    )
    .await?;

    Ok(TeamNoteResponse {
        id,
        // Said plainly, including the part a model would otherwise assume away: there is no reply
        // coming, and there may be no delivery either. A specialist that believes it has handed the
        // problem over stops carrying it, and the department loses the finding twice.
        outcome: format!(
            "left for {asked}. They will read it at the top of their brief the next time this              department starts them, which may not happen — say it in your own answer too, and do              not wait for a reply, because there is none coming."
        ),
    })
}
