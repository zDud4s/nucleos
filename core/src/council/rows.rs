//! Council database rows and persistence helpers.

use super::*;

/// One seat as it is stored and read back.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct SeatRow {
    pub seat_idx: i64,
    pub kind: String,
    pub model_ref: String,
    /// The catalogue agent that took the seat, or NULL for one declared as a bare model. The NAME
    /// is deliberately not stored beside it: a name is editable and this column is a reference, so
    /// the two would drift and the row would be the one that looked authoritative.
    pub agent_id: Option<String>,
    // What each phase of the seat did is not here: it is one row per step in `council_rounds`
    // (`store::steps_of`), which replaced the column triple per phase this row used to carry.
    /// The role the seat was asked to play (`formats::Role`), or NULL for a plain seat. Text as
    /// stored, so a role a later version adds still reads rather than failing the whole row.
    pub role: Option<String>,
}

/// One council as it is stored and read back, without its seats.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct CouncilRow {
    pub id: String,
    pub created_at: String,
    pub question: String,
    pub status: String,
    /// Legacy and no longer advanced: written as 1 when a council convenes and never moved, because
    /// `current_round`/`current_phase` replaced it. Kept because `job.rs` and `hooks.rs` fixtures
    /// write it (see `0155_council_rounds.sql`).
    pub stage: i64,
    /// Critique rounds asked for, 1 to `config::MAX_COUNCIL_ROUNDS`, fixed when the council
    /// convened. `rounds_run` is how many actually ran.
    pub rounds: i64,
    pub anon_seed: String,
    pub anon_map: Option<String>,
    pub chairman_kind: String,
    pub chairman_ref: String,
    pub chairman_agent_id: Option<String>,
    pub chairman_run_id: Option<i64>,
    pub error: Option<String>,
    /// Critique rounds that ran, and whether the council stopped before `rounds` because nothing
    /// was left to change.
    pub rounds_run: i64,
    pub stopped_early: bool,
    /// Where the council is: a round and a phase (`answer`, `critique`, `revise`, `chairman`,
    /// `done`). Replaces `stage`, which could not count past one revision round.
    pub current_round: i64,
    pub current_phase: String,
    /// The chairman's structured synthesis as stored, and how producing it ended. NULL on every
    /// council recorded before it existed — its synthesis is the chairman run's transcript.
    pub synthesis_json: Option<String>,
    pub synthesis_status: Option<String>,
}

/// Writes a council and its seats in one transaction.
///
/// One transaction and not two statements, because a council row with no seats is a deliberation
/// nothing will ever drive: `run_council` reads its roster back from `council_seats`, so a crash
/// between the two writes would leave a row stuck at `running` for the reconciliation to find and
/// nothing else.
///
/// Test-only since roles arrived: `start_with` writes through [`insert_council_with_roles`], and
/// this role-less shape is what the fixtures convene councils with.
#[cfg(any(test, feature = "testkit"))]
pub async fn insert_council(
    pool: &sqlx::SqlitePool,
    id: &str,
    question: &str,
    chairman: &CouncilSeat,
    members: &[CouncilSeat],
    rounds: i64,
) -> sqlx::Result<()> {
    insert_council_with_roles(pool, id, question, chairman, members, rounds, &[]).await
}

/// [`insert_council`], with the role each seat plays: `roles[seat_idx]`, `None` (or past the end of
/// the slice) for a plain seat. A sibling rather than a seventh argument on `insert_council`, so
/// every caller that has no roles — the fixtures above all — keeps writing exactly what it wrote;
/// and the same transaction, so a council never exists with its roles still to come.
pub(super) async fn insert_council_with_roles(
    pool: &sqlx::SqlitePool,
    id: &str,
    question: &str,
    chairman: &CouncilSeat,
    members: &[CouncilSeat],
    rounds: i64,
    roles: &[Option<formats::Role>],
) -> sqlx::Result<()> {
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "INSERT INTO council_runs
           (id, created_at, question, status, stage, rounds, anon_seed, chairman_kind,
            chairman_ref, chairman_agent_id, current_round, current_phase)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?)",
    )
    .bind(id)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(question)
    .bind(STATUS_RUNNING)
    // The legacy `stage`, written as the 1 it always started at and never advanced: the position
    // is `current_round`/`current_phase` now. See `CouncilRow::stage`.
    .bind(1_i64)
    // Written rather than left to the column's DEFAULT: the number the driver runs and the number
    // the database holds must not be able to drift apart.
    .bind(rounds)
    // The council's own id. Stored again under its own name so the shuffle stays recomputable even
    // if what the seed is derived from ever changes.
    .bind(id)
    .bind(chairman.kind.as_db_str())
    // The MODEL, beside the agent and not instead of it. An agent is editable and deletable; what
    // answered in March has to keep reading as what answered in March.
    .bind(&chairman.model_ref)
    .bind(chairman.agent_id())
    .bind(store::PHASE_ANSWER)
    .execute(&mut *transaction)
    .await?;

    for (seat_idx, seat) in members.iter().enumerate() {
        let role = roles.get(seat_idx).copied().flatten().map(|r| r.as_str());
        sqlx::query(
            "INSERT INTO council_seats (council_id, seat_idx, kind, model_ref, agent_id, role)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(seat_idx as i64)
        .bind(seat.kind.as_db_str())
        .bind(&seat.model_ref)
        .bind(seat.agent_id())
        .bind(role)
        .execute(&mut *transaction)
        .await?;
    }

    transaction.commit().await
}

pub async fn get_council_row(
    pool: &sqlx::SqlitePool,
    id: &str,
) -> sqlx::Result<Option<CouncilRow>> {
    sqlx::query_as::<_, CouncilRow>("SELECT * FROM council_runs WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

impl CouncilRow {
    /// Whether this council is over, however it ended.
    ///
    /// The positive list, for the reason [`TERMINAL_COUNCIL_STATUSES`] gives: spelled as
    /// `!= running`, a status added later and forgotten would read as SETTLED, and an internal
    /// consumer waiting on one would walk on with no synthesis while the council was still
    /// deliberating. This way the same oversight only makes it wait.
    pub fn is_settled(&self) -> bool {
        TERMINAL_COUNCIL_STATUSES.contains(&self.status.as_str())
    }
}

/// The chairman's synthesis, or `None` when there is not one to read.
///
/// The same read `get_council` does to fill `CouncilView::synthesis`, lifted out because it is now
/// asked for off the HTTP path too — a job's review node and a proposal's note both want the text
/// and neither is a client. **The synthesis is not a column**: it is the transcript of the run in
/// `chairman_run_id`, so a council that ended `error` before phase 3, or one whose transcript has
/// since been pruned by `runs::prune_transcripts`, answers `None` rather than an empty string. A
/// caller must treat that as "no advice", never as "the council advised nothing".
pub async fn synthesis_of(pool: &sqlx::SqlitePool, row: &CouncilRow) -> Option<String> {
    synthesis_text(pool, row).await.0
}

/// The synthesis as text and, when the chairman produced one, as the structure it was written in.
///
/// One reader for both, so the detail view and the internal consumers (`synthesis_of`) cannot
/// disagree about what the council concluded. A `synthesis_json` that parses is composed into
/// markdown with each seat's NAME — the agent's when an agent took the seat, the model's otherwise
/// — because the struct refers to seats by index and an index means nothing to a reader. One that
/// does not parse, or is absent (every council recorded before it existed), falls back to the
/// chairman run's transcript, which is what the synthesis always was before; and with neither, the
/// answer is `None`, never an empty string.
pub async fn synthesis_text(
    pool: &sqlx::SqlitePool,
    row: &CouncilRow,
) -> (Option<String>, Option<formats::Synthesis>) {
    let structured = row
        .synthesis_json
        .as_deref()
        .and_then(|text| serde_json::from_str::<formats::Synthesis>(text).ok());
    if let Some(synthesis) = structured {
        let seats = get_seat_rows(pool, &row.id).await.unwrap_or_default();
        let names = agent_names(pool).await;
        let seat_names: BTreeMap<usize, String> = seats
            .into_iter()
            .map(|seat| {
                let name = seat
                    .agent_id
                    .as_deref()
                    .and_then(|id| names.get(id).cloned())
                    .unwrap_or(seat.model_ref);
                (seat.seat_idx as usize, name)
            })
            .collect();
        // A seat index the roster does not have is printed as one rather than dropped: the chairman
        // named it, and hiding that would make its position look unanimous.
        let name_of = |seat: usize| {
            seat_names
                .get(&seat)
                .cloned()
                .unwrap_or_else(|| format!("seat {seat}"))
        };
        let text = formats::compose_markdown(&synthesis, &name_of);
        return (Some(text), Some(synthesis));
    }
    let text = match row.chairman_run_id {
        Some(run_id) => transcript_of(pool, run_id).await,
        None => None,
    };
    (text, None)
}

/// Agent id to name, from the catalogue. One read of a table that holds a handful of rows, rather
/// than one lookup per seat. An agent the roster named and somebody has since deleted is simply
/// absent, and its seat shows the id it pointed at with no name beside it — which is the honest
/// rendering of what the record actually says.
pub(super) async fn agent_names(pool: &sqlx::SqlitePool) -> BTreeMap<String, String> {
    crate::agent::list(pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|agent| (agent.id, agent.name))
        .collect()
}

pub async fn get_seat_rows(pool: &sqlx::SqlitePool, id: &str) -> sqlx::Result<Vec<SeatRow>> {
    sqlx::query_as::<_, SeatRow>(
        "SELECT seat_idx, kind, model_ref, agent_id, role
         FROM council_seats WHERE council_id = ? ORDER BY seat_idx",
    )
    .bind(id)
    .fetch_all(pool)
    .await
}

/// Newest first, with the paging the sibling list routes use.
pub async fn list_council_rows(
    pool: &sqlx::SqlitePool,
    limit: i64,
    offset: i64,
) -> sqlx::Result<Vec<CouncilRow>> {
    sqlx::query_as::<_, CouncilRow>(
        "SELECT * FROM council_runs ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// The position of a council whose seats are done and whose chairman is synthesising. Not a step
/// phase — the chairman has no row in `council_rounds` — only a value of `current_phase`.
pub(super) const PHASE_CHAIRMAN: &str = "chairman";

/// The position of a council the chairman has finished with.
pub(super) const PHASE_DONE: &str = "done";

/// What `council_runs.synthesis_status` records: a synthesis that parsed and validated, or the
/// chairman's raw text kept with the reason it could not be structured (`formats::degraded`).
pub(super) const SYNTHESIS_OK: &str = "ok";
pub(super) const SYNTHESIS_DEGRADED: &str = "degraded";

/// Records the phase-1 shuffle.
pub async fn set_anon_map(
    pool: &sqlx::SqlitePool,
    id: &str,
    anon_map: &BTreeMap<String, usize>,
) -> sqlx::Result<()> {
    let encoded = serde_json::to_string(anon_map).unwrap_or_else(|_| "{}".to_string());
    sqlx::query("UPDATE council_runs SET anon_map = ? WHERE id = ?")
        .bind(encoded)
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Attaches the chairman's run before it starts producing anything.
pub async fn set_chairman_run(pool: &sqlx::SqlitePool, id: &str, run_id: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE council_runs SET chairman_run_id = ? WHERE id = ?")
        .bind(run_id)
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Settles a council.
///
/// Guarded on `running` so the first terminal verdict wins. A cancel and a phase-3 failure can race
/// — the cancel aborts the task that is about to write the failure — and the owner is entitled to
/// see the reason they caused rather than the symptom it produced.
pub async fn finish(
    pool: &sqlx::SqlitePool,
    id: &str,
    status: &str,
    error: Option<&str>,
) -> sqlx::Result<bool> {
    let result =
        sqlx::query("UPDATE council_runs SET status = ?, error = ? WHERE id = ? AND status = ?")
            .bind(status)
            .bind(error)
            .bind(id)
            .bind(STATUS_RUNNING)
            .execute(pool)
            .await?;
    Ok(result.rows_affected() > 0)
}

/// A completed run's text, or `None` for a run that produced none.
///
/// An empty transcript reads as `None` rather than as `Some("")`, because to a client those mean
/// "nothing yet" and "the seat answered with nothing", and only one of them is true here.
pub(super) async fn transcript_of(pool: &sqlx::SqlitePool, run_id: i64) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT stdout FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .flatten()
        .filter(|text| !text.trim().is_empty())
}
