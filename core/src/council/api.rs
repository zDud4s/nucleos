//! Council HTTP request, response, and view types and handlers.

use super::*;

// ── HTTP ─────────────────────────────────────────────────────────────────────────────────────
//
// The handlers, and nothing about transport beyond them: `http.rs` owns the router, as it owns
// every other pillar's.

#[derive(Deserialize)]
pub struct CreateCouncilRequest {
    pub question: String,
    /// Replaces the configured roster FOR THIS QUESTION. Writes no configuration — a council asked
    /// of three particular models is a thing somebody wants once, and persisting it would make
    /// every later council inherit an answer nobody gave.
    #[serde(default)]
    pub roster: Option<RosterOverride>,
    /// Critique rounds for this question; `None` is the file's. Validated by [`start_with`], so a
    /// count outside the bounds is a 400 rather than a quiet clamp to something nobody asked for.
    #[serde(default)]
    pub rounds: Option<u32>,
    /// A role per seat, keyed by the seat index as a string (`{"1": "skeptic"}`). `None` and an
    /// empty map mean the same thing: every seat plays itself.
    #[serde(default)]
    pub roles: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Serialize)]
pub struct CreateCouncilResponse {
    pub id: String,
}

/// One step of one seat as a client sees it: the record, plus what the step said.
#[derive(Debug, Serialize)]
pub struct StepView {
    /// 0 for the answer, 1 and up for the critique rounds. Unsigned, as `tally` counts rounds: the
    /// column is an INTEGER only because SQLite has no other kind.
    pub round: u32,
    /// `answer`, `critique` or `revise`.
    pub phase: String,
    pub run_id: Option<i64>,
    pub status: String,
    pub error: Option<String>,
    /// The text an answer or a revision wrote: the payload's `answer` when the step carries one,
    /// else the transcript of its run (every council recorded before payloads existed). `None`
    /// on a critique, and `None` rather than `""` when there is nothing — "nothing yet" and "the
    /// seat answered with nothing" are different things to a client, and only one is true here.
    pub answer: Option<String>,
    /// A critique's reviews and ballot. `None` on any other phase, and on a critique that left no
    /// readable payload — which is also a critique that cast no ballot.
    pub critique: Option<formats::Critique>,
    /// A revision's own account of itself: whether it changed its answer, and why.
    pub changed: Option<bool>,
    pub why: Option<String>,
}

/// One seat as a client sees it: the record, plus every step it took, in the order they happen.
#[derive(Serialize)]
pub struct SeatView {
    pub seat_idx: i64,
    pub kind: String,
    #[serde(rename = "ref")]
    pub model_ref: String,
    pub agent_id: Option<String>,
    /// Read from the catalogue when the view is built, and `None` when the agent has since been
    /// deleted. Not stored on the row: a name is editable, and a copy of one is a second version of
    /// the truth that looks authoritative because it is older.
    pub agent_name: Option<String>,
    /// The role the seat was asked to play, or `None` for a plain seat.
    pub role: Option<String>,
    /// Answer, then each round's critique and revise. The client is never told a `runs` table
    /// exists: the text is read out of it here.
    pub steps: Vec<StepView>,
}

#[derive(Serialize)]
pub struct CouncilView {
    pub id: String,
    pub created_at: String,
    pub question: String,
    pub status: String,
    /// Critique rounds asked for, rounds that ran, and whether it stopped before the asked number
    /// because nothing was left to change.
    pub rounds: i64,
    pub rounds_run: i64,
    pub stopped_early: bool,
    pub current_round: i64,
    pub current_phase: String,
    pub error: Option<String>,
    pub chairman_kind: String,
    #[serde(rename = "chairman_ref")]
    pub chairman_ref: String,
    pub chairman_agent_id: Option<String>,
    pub chairman_agent_name: Option<String>,
    /// How far the LAST critique round's ballots agree. `None` when no critique round exists. The
    /// last entry of `agreement_by_round`.
    pub agreement: Option<tally::Agreement>,
    /// The last critique round's Borda leaderboard — THE leaderboard. Computed from the stored
    /// ballots on every read, never read off the legacy `leaderboard` column, so it cannot drift
    /// from the votes it claims to summarise.
    pub leaderboard: Vec<tally::BordaRow>,
    /// Every critique round's leaderboard, in round order, so a client can show how standings moved.
    pub leaderboard_by_round: Vec<Vec<tally::BordaRow>>,
    /// How far each critique round's ballots agree, in the same order as `leaderboard_by_round`
    /// and only for the rounds that have a readable ballot. The last entry is `agreement`.
    pub agreement_by_round: Vec<tally::Agreement>,
    /// The round each `*_by_round` index stands for. Dense like them: a round with no readable
    /// ballot has no entry, so index i is round `ballot_rounds[i]`, not round i + 1.
    pub ballot_rounds: Vec<u32>,
    /// The run of the chairman's synthesis step, set as soon as that step opens (before it
    /// finishes), so a client can tail the verdict while it is being written. `None` until the
    /// council reaches phase 3, and for a council whose chairman never got a run.
    pub synthesis_run_id: Option<i64>,
    /// The synthesis as markdown, once the chairman has produced one. See [`synthesis_text`].
    pub synthesis: Option<String>,
    /// The same synthesis as its structure, when the chairman wrote a structured one.
    pub synthesis_structured: Option<formats::Synthesis>,
    pub synthesis_status: Option<String>,
    pub anon_map: BTreeMap<String, usize>,
    pub seats: Vec<SeatView>,
}

/// One council row without its seats, for the list.
#[derive(Debug, Serialize)]
pub struct CouncilSummary {
    pub id: String,
    pub created_at: String,
    pub question: String,
    pub status: String,
    /// Beside the position here as well as on the detail, because the LIST is the other place a
    /// council's progress is drawn and a total is what makes a round number legible.
    pub rounds: i64,
    pub rounds_run: i64,
    pub current_round: i64,
    pub current_phase: String,
}

/// What the shell's council form needs before anyone types a question: the bounds it may offer,
/// the closed set of roles, and the roster it would be overriding.
///
/// Built by hand rather than derived, because the config types are deserialize-only on purpose —
/// they describe a file — and a `Serialize` on them would invite echoing the whole file, consumers
/// and timeout included, to a client that has no business with either.
#[derive(Debug, Serialize)]
pub struct CouncilConfigView {
    /// `false` when there is no roster: the form says so instead of rendering an empty one.
    pub configured: bool,
    /// The file's rounds when configured, else [`crate::config::DEFAULT_COUNCIL_ROUNDS`], so the
    /// form's control has a value either way.
    pub default_rounds: u32,
    pub max_rounds: u32,
    /// `formats::Role::ALL`, in its declared order, so a new role reaches the form without a
    /// second list to keep in step.
    pub roles: Vec<&'static str>,
    pub default_roster: Option<RosterView>,
}

#[derive(Debug, Serialize)]
pub struct RosterView {
    pub chairman: serde_json::Value,
    pub members: Vec<serde_json::Value>,
}

/// PURE: one declared seat in the form the file wrote it — `{ "agent" }` or `{ "kind", "ref" }`,
/// never both. A loaded roster never holds both (the file is refused at load), so the agent form
/// wins only as a tie-break that cannot occur.
fn seat_spec_view(seat: &SeatSpec) -> serde_json::Value {
    match &seat.agent {
        Some(agent) => serde_json::json!({ "agent": agent }),
        None => serde_json::json!({
            "kind": seat.kind.map(SeatKind::as_db_str),
            "ref": seat.model_ref,
        }),
    }
}

pub async fn get_council_config(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
) -> axum::Json<CouncilConfigView> {
    let configured = state.council.config();
    axum::Json(CouncilConfigView {
        configured: configured.is_some(),
        default_rounds: configured.map_or(crate::config::DEFAULT_COUNCIL_ROUNDS, |c| c.rounds),
        max_rounds: crate::config::MAX_COUNCIL_ROUNDS,
        roles: formats::Role::ALL
            .iter()
            .map(|role| role.as_str())
            .collect(),
        default_roster: configured.map(|config| RosterView {
            chairman: seat_spec_view(&config.chairman),
            members: config.members.iter().map(seat_spec_view).collect(),
        }),
    })
}

pub async fn post_council(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::Json(request): axum::Json<CreateCouncilRequest>,
) -> Result<
    (axum::http::StatusCode, axum::Json<CreateCouncilResponse>),
    (axum::http::StatusCode, String),
> {
    match start_with(
        &state,
        &request.question,
        request.roster,
        request.rounds,
        request.roles.unwrap_or_default(),
    )
    .await
    {
        Ok(id) => Ok((
            // 202: the record exists and the deliberation has not happened yet. A 201 would claim a
            // finished resource, and a caller reading the body would find a council with no answers
            // in it and conclude something failed.
            axum::http::StatusCode::ACCEPTED,
            axum::Json(CreateCouncilResponse { id }),
        )),
        // Said as its own status rather than folded into 400, because "you asked wrongly" and
        // "this daemon has no council" are two different things for a client to do about.
        Err(error @ (StartError::NotConfigured | StartError::NoLocalModel)) => Err((
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            error.to_string(),
        )),
        Err(error @ StartError::Invalid(_)) => {
            Err((axum::http::StatusCode::BAD_REQUEST, error.to_string()))
        }
        // 429, not 402: the ceiling is a window that reopens, and the caller should come back.
        Err(error @ (StartError::BudgetExhausted(_) | StartError::QuotaExhausted(_))) => {
            Err((axum::http::StatusCode::TOO_MANY_REQUESTS, error.to_string()))
        }
        Err(error @ StartError::Unavailable(_)) => Err((
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            error.to_string(),
        )),
    }
}

pub async fn get_council(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::Json<CouncilView>, (axum::http::StatusCode, String)> {
    let pool = &state.pool;
    let row = get_council_row(pool, &id).await.map_err(internal)?.ok_or((
        axum::http::StatusCode::NOT_FOUND,
        "no such council".to_string(),
    ))?;
    let seats = get_seat_rows(pool, &id).await.map_err(internal)?;
    let steps = store::steps_of(pool, &id).await.map_err(internal)?;
    let names = agent_names(pool).await;

    // An `anon_map` that will not parse becomes an empty one rather than a 500. The record is worth
    // reading even when one of its JSON columns is not.
    let anon_map: BTreeMap<String, usize> = row
        .anon_map
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_default();

    // Each critique round's ballots, `voter seat -> ranking labels`, gathered while the steps are
    // turned into views. Only a critique whose payload reads is a ballot: one that failed, or wrote
    // something unreadable, abstains rather than voting for nothing.
    let mut ballots_by_round: BTreeMap<i64, BTreeMap<usize, Vec<String>>> = BTreeMap::new();
    let mut steps_by_seat: BTreeMap<i64, Vec<StepView>> = BTreeMap::new();
    for step in steps {
        let seat_idx = step.seat_idx;
        let view = step_view(pool, step, &mut ballots_by_round).await;
        steps_by_seat.entry(seat_idx).or_default().push(view);
    }

    let tallies = tally::by_round(&ballots_by_round, &anon_map);

    let (synthesis, synthesis_structured) = synthesis_text(pool, &row).await;

    Ok(axum::Json(CouncilView {
        seats: seats
            .into_iter()
            .map(|seat| SeatView {
                steps: steps_by_seat.remove(&seat.seat_idx).unwrap_or_default(),
                seat_idx: seat.seat_idx,
                kind: seat.kind,
                model_ref: seat.model_ref,
                agent_name: seat
                    .agent_id
                    .as_deref()
                    .and_then(|id| names.get(id).cloned()),
                agent_id: seat.agent_id,
                role: seat.role,
            })
            .collect(),
        anon_map,
        agreement: tallies.agreement,
        leaderboard: tallies.leaderboard,
        leaderboard_by_round: tallies.leaderboard_by_round,
        agreement_by_round: tallies.agreement_by_round,
        ballot_rounds: tallies.ballot_rounds,
        synthesis_run_id: row.chairman_run_id,
        synthesis,
        synthesis_structured,
        synthesis_status: row.synthesis_status,
        id: row.id,
        created_at: row.created_at,
        question: row.question,
        status: row.status,
        rounds: row.rounds,
        rounds_run: row.rounds_run,
        stopped_early: row.stopped_early,
        current_round: row.current_round,
        current_phase: row.current_phase,
        error: row.error,
        chairman_kind: row.chairman_kind,
        chairman_ref: row.chairman_ref,
        chairman_agent_name: row
            .chairman_agent_id
            .as_deref()
            .and_then(|id| names.get(id).cloned()),
        chairman_agent_id: row.chairman_agent_id,
    }))
}

/// One stored step as the view serves it; a readable critique also lands in
/// `ballots_by_round` as that seat's ballot for its round.
///
/// The payload is read loosely, field by field, rather than into the strict `formats` structs:
/// an answer or a revision written by a later version with a field this one does not know, or
/// missing one this one would require, still serves its text.
async fn step_view(
    pool: &sqlx::SqlitePool,
    step: store::StepRow,
    ballots_by_round: &mut BTreeMap<i64, BTreeMap<usize, Vec<String>>>,
) -> StepView {
    let seat_idx = step.seat_idx;
    let payload: Option<serde_json::Value> = step
        .payload
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok());
    let mut view = StepView {
        // A negative round is not a value this daemon writes; read as 0 rather than wrapped round.
        round: u32::try_from(step.round).unwrap_or_default(),
        phase: step.phase,
        run_id: step.run_id,
        status: step.status,
        error: step.error,
        answer: None,
        critique: None,
        changed: None,
        why: None,
    };
    match view.phase.as_str() {
        store::PHASE_CRITIQUE => {
            view.critique =
                payload.and_then(|value| serde_json::from_value::<formats::Critique>(value).ok());
            if let Some(critique) = &view.critique {
                ballots_by_round
                    .entry(i64::from(view.round))
                    .or_default()
                    .insert(seat_idx as usize, critique.ranking.clone());
            }
        }
        store::PHASE_ANSWER | store::PHASE_REVISE => {
            let from_payload = payload
                .as_ref()
                .and_then(|value| value.get("answer"))
                .and_then(|answer| answer.as_str())
                .filter(|text| !text.trim().is_empty())
                .map(str::to_string);
            view.answer = match (from_payload, view.run_id) {
                (Some(text), _) => Some(text),
                (None, Some(run_id)) => transcript_of(pool, run_id).await,
                (None, None) => None,
            };
            if view.phase == store::PHASE_REVISE {
                view.changed = payload
                    .as_ref()
                    .and_then(|value| value.get("changed"))
                    .and_then(|changed| changed.as_bool());
                view.why = payload
                    .as_ref()
                    .and_then(|value| value.get("why"))
                    .and_then(|why| why.as_str())
                    .map(str::to_string);
            }
        }
        _ => {}
    }
    view
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

pub async fn list_councils(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::extract::Query(query): axum::extract::Query<ListQuery>,
) -> Result<axum::Json<Vec<CouncilSummary>>, (axum::http::StatusCode, String)> {
    // Clamped rather than trusted, like every other paged list here: the number arrives in a query
    // string, and `LIMIT -1` in SQLite means no limit at all.
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = query.offset.unwrap_or(0).max(0);
    let rows = list_council_rows(&state.pool, limit, offset)
        .await
        .map_err(internal)?;
    Ok(axum::Json(
        rows.into_iter()
            .map(|row| CouncilSummary {
                id: row.id,
                created_at: row.created_at,
                question: row.question,
                status: row.status,
                rounds: row.rounds,
                rounds_run: row.rounds_run,
                current_round: row.current_round,
                current_phase: row.current_phase,
            })
            .collect(),
    ))
}

#[derive(Debug, Serialize)]
pub struct CancelResponse {
    /// Whether THIS call is what settled it. `false` means it had already ended — which is not an
    /// error, and the caller is entitled to know which of the two happened.
    pub cancelled: bool,
}

pub async fn post_council_cancel(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::Json<CancelResponse>, (axum::http::StatusCode, String)> {
    if get_council_row(&state.pool, &id)
        .await
        .map_err(internal)?
        .is_none()
    {
        return Err((
            axum::http::StatusCode::NOT_FOUND,
            "no such council".to_string(),
        ));
    }
    let cancelled = cancel(&state, &id).await.map_err(internal)?;
    Ok(axum::Json(CancelResponse { cancelled }))
}

fn internal(error: sqlx::Error) -> (axum::http::StatusCode, String) {
    tracing::warn!(%error, "council: a database read failed");
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "could not read the council".to_string(),
    )
}
