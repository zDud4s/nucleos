//! The council: N seats answer one question, critique and rank each other blind, and a chairman
//! synthesises.
//!
//! Ported from `.ai/scripts/council_run.py`, which ran three phases over `claude` and `codex`
//! subprocesses from outside the product. What it did was worth more than where it lived: asking
//! several models the same thing and being told where they disagree is a capability the owner
//! wants, and nothing in the núcleo could offer it.
//!
//! The shape of the deliberation lives here and nothing else does. How one talks to a model is
//! `runner.rs` and `local_agent.rs`; whether there is money to spend is `budget.rs`; what a seat is
//! allowed to call is `hooks.rs`. This module decides who answers, who ranks whom, and in what
//! order — and every one of those is a decision that can be tested without a subprocess.
//!
//! **The ballots are averaged, not summed** (`tally::borda`), because a council degrades: a seat
//! that failed casts no votes, and a critique that could not be read is an abstention. Sums make an
//! absent participant look bad; means make it look absent.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::{CouncilConfig, CouncilSeat, SeatAgent, SeatKind, SeatSpec};

pub mod formats;
pub mod prompts;
pub mod store;
pub mod tally;

/// The roster on this machine, or `None` when there is no home directory to hang it off.
///
/// `~/.nucleos` and not the app's data directory, for the reason `workflows::library_root` argues
/// at length where it made the same choice: a file a person is meant to open, read and edit is the
/// opposite kind of thing to a database, and putting it where only the app can find it is putting
/// it where nobody edits it.
///
/// And not `.ai/council.yaml`, which is where this started. `.ai/` is the agent harness's own
/// directory inside one checkout, so the roster only existed for a daemon started from that
/// directory, and every worktree on this machine was a council that had to be written again. The
/// pillar is the product's, not the harness's; the file follows. Every other machine setting
/// followed it later, which is why the directory is now [`crate::machine_config::root`].
pub fn config_path() -> Option<PathBuf> {
    crate::machine_config::root().map(|root| root.join(crate::machine_config::COUNCIL_FILE))
}

/// The same file as a person is shown it, and the only spelling any refusal uses.
///
/// Deliberately not the absolute path [`config_path`] returns. The absolute one is what the daemon
/// opens; `~/.nucleos/council.yaml` is what somebody can be told to go and write, on any machine,
/// without the sentence carrying another person's username.
pub const CONFIG_DISPLAY_PATH: &str = "~/.nucleos/council.yaml";

/// A council's status. Text in the database, as `runs.status` is.
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_DONE: &str = "done";
pub const STATUS_ERROR: &str = "error";
pub const STATUS_CANCELLED: &str = "cancelled";

/// A seat's status within one phase.
pub const SEAT_PENDING: &str = "pending";
pub const SEAT_OK: &str = "ok";
pub const SEAT_ERROR: &str = "error";
pub const SEAT_TIMEOUT: &str = "timeout";
pub const SEAT_CANCELLED: &str = "cancelled";
/// Phase 2 and the revision: there was nothing to rank, so there was nothing to revise against.
pub const SEAT_SKIPPED: &str = "skipped";

/// Where a local seat's tool box reaches the daemon.
///
/// Loopback HTTP even though it runs INSIDE the daemon, for the reason `mcp_tools::LocalToolBox`
/// gives: it is the same request the MCP subprocess makes, through the same handler, with the same
/// authorisation — so a local seat and a cloud seat cannot diverge in what a tool DOES, only in
/// which tools they are offered.
/// Where this daemon is reached. A function and not a `const`, because the port is no longer a
/// compile-time fact: a second instance binds its own (`daemon_client::PORT_VAR`), and a const
/// would send its runs to whichever daemon happens to hold the default.
fn daemon_url() -> String {
    crate::daemon_client::daemon_url()
}

/// The `runs.mode` every seat invocation is recorded under.
///
/// A plain string like every other mode, and deliberately not a value `runs.mode` is constrained to
/// — that column carries no CHECK, which is how `'assistant'` arrived and how this does.
pub const COUNCIL_MODE: &str = "council";

/// What a seat may see of its peers, and which peer each label stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anonymized {
    /// Label to `seat_idx`. The record of the shuffle, written to the council row so that a reader
    /// months later can say which answer `B` was.
    pub anon_map: BTreeMap<String, usize>,
    /// Viewer `seat_idx` to the labels it is shown, ascending. Never contains the viewer's own.
    pub for_seat: BTreeMap<usize, Vec<String>>,
}

/// The label for a position in the shuffle: 0 is `A`.
///
/// Bounded by `MAX_COUNCIL_SEATS`, so the alphabet never runs out in practice; past `Z` it keeps
/// producing distinct labels rather than colliding, because two seats sharing a label would make
/// the anonymisation map ambiguous and every vote after it meaningless.
pub fn label_for(position: usize) -> String {
    let mut label = String::new();
    let mut remaining = position;
    loop {
        label.insert(0, char::from(b'A' + (remaining % 26) as u8));
        if remaining < 26 {
            return label;
        }
        remaining = remaining / 26 - 1;
    }
}

/// A 64-bit hash of the seed string, so a uuid becomes a number a generator can start from.
///
/// FNV-1a, written out rather than pulled in. The requirement is that the same seed always produces
/// the same shuffle, not that it agrees with any other implementation — the Python this was ported
/// from used Mersenne Twister, which no Rust crate reproduces bit-for-bit anyway.
fn seed_from(seed: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in seed.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// SplitMix64 — the shuffle's whole source of randomness.
///
/// Hand-rolled for the same reason `redact.rs` hand-rolls its detectors: it is a dozen lines, it
/// has no feature flags, and it cannot change under the crate when a dependency is bumped. A
/// shuffle that changed with a `cargo update` would silently break the one property this exists to
/// have.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..bound`, by rejection.
    ///
    /// `% bound` alone would bias the shuffle toward low indices — slightly, for a bound of eight,
    /// and a biased shuffle is a partly-guessable one. Guessability is precisely what the shuffle
    /// exists to remove.
    fn below(&mut self, bound: u64) -> u64 {
        debug_assert!(bound > 0, "the caller never asks for a bound of zero");
        let zone = u64::MAX - (u64::MAX % bound);
        loop {
            let value = self.next();
            if value < zone {
                return value % bound;
            }
        }
    }
}

/// PURE: which label stands for which seat, and what each seat is shown.
///
/// `seats` is the set that produced a valid answer, in any order; it is sorted here so the same set
/// always shuffles the same way whatever order it arrived in.
///
/// Two properties, and they are the reason phase 2 measures anything at all. Nobody sees their own
/// answer, so no seat can vote for itself. And nobody learns whose answer is whose, so a seat cannot
/// rank the model rather than the argument.
pub fn anonymize(seed: &str, seats: &[usize]) -> Anonymized {
    let mut ordered: Vec<usize> = seats.to_vec();
    ordered.sort_unstable();
    ordered.dedup();

    let mut shuffled = ordered.clone();
    let mut rng = SplitMix64(seed_from(seed));
    for index in (1..shuffled.len()).rev() {
        let swap_with = rng.below((index + 1) as u64) as usize;
        shuffled.swap(index, swap_with);
    }

    let anon_map: BTreeMap<String, usize> = shuffled
        .iter()
        .enumerate()
        .map(|(position, seat)| (label_for(position), *seat))
        .collect();

    let for_seat = ordered
        .iter()
        .map(|viewer| {
            let visible = anon_map
                .iter()
                .filter(|(_, seat)| *seat != viewer)
                .map(|(label, _)| label.clone())
                .collect();
            (*viewer, visible)
        })
        .collect();

    Anonymized { anon_map, for_seat }
}

/// PURE: whether a critique round is worth running.
///
/// Two valid answers is the floor, and it is a floor rather than a preference: with one there is
/// nothing to compare it against, and the single seat would be asked to judge an empty set. Below it
/// the round is skipped whole and the chairman still runs, because the answers themselves are the
/// part with value — a council that produced one good answer and no critique is worth reading, and
/// one that produced a critique of nothing is not.
fn critique_should_run(valid_answers: usize) -> bool {
    valid_answers >= 2
}

/// The council's settings, resolved once at startup.
///
/// `None` inside is the shipped state and means there is no council: [`CONFIG_DISPLAY_PATH`] is
/// absent, unreadable, or names a roster the daemon will not run. Held as one field on `AppState`
/// for the same reason `voice` and `web` are — read together, switched on together, and a
/// `Default` that means "off" so no test that ignores councils has to know this exists.
#[derive(Clone, Default)]
pub struct CouncilRuntime {
    config: Option<CouncilConfig>,
    /// The council's OWN daemon key (`auth::Service::Council`), minted at startup — never the
    /// control token.
    ///
    /// It lives here because a seat is spawned at request time and the key has to be in its
    /// environment, and the alternatives are both worse: re-minting per council would rotate the
    /// key out from under a council still running, and reaching for `AppState::token` would hand
    /// the daemon's controls to the one set of processes this arrangement exists to keep them from.
    /// `None` means minting failed, and `start` refuses rather than falling back — a seat with no
    /// key has its tool calls refused, which is a council that costs money to answer badly.
    token: Option<String>,
}

/// Describes everything except the one thing that must not be described.
///
/// The lesson `EmailRuntime` records, applied to the second struct that reached `AppState` holding a
/// credential: `#[derive(Debug)]` here is one `tracing::debug!(?state.council, …)` away from
/// putting the council's key in a rotating log file, written by somebody printing configuration.
impl std::fmt::Debug for CouncilRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CouncilRuntime")
            .field("config", &self.config)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl CouncilRuntime {
    /// `token` is a parameter rather than something this reads, because it is not configuration:
    /// only `main.rs` can mint one against the database.
    pub fn new(config: Option<CouncilConfig>, token: Option<String>) -> Self {
        Self { config, token }
    }

    /// The configured roster, or `None` when there is no council.
    pub fn config(&self) -> Option<&CouncilConfig> {
        self.config.as_ref()
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Whether a job's review node should be given a council's synthesis to read first.
    ///
    /// Asked of the runtime rather than of `config().consumers` at the call site, so "there is no
    /// council at all" and "the council was not asked to advise this" collapse into one answer. A
    /// consumer reading the flag itself would have to remember the `is_some_and`, and forgetting it
    /// is a panic on the shipped configuration — no roster is the state this daemon ships in.
    pub fn advises_job_review(&self) -> bool {
        self.config
            .as_ref()
            .is_some_and(|config| config.consumers.job_review)
    }

    /// Whether a proposal the daemon writes should carry a council's opinion as a note.
    pub fn advises_proposals(&self) -> bool {
        self.config
            .as_ref()
            .is_some_and(|config| config.consumers.proposal_advice)
    }
}

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
    /// write it (see `0154_council_rounds.sql`).
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
#[cfg(test)]
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
async fn insert_council_with_roles(
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
async fn agent_names(pool: &sqlx::SqlitePool) -> BTreeMap<String, String> {
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
const PHASE_CHAIRMAN: &str = "chairman";

/// The position of a council the chairman has finished with.
const PHASE_DONE: &str = "done";

/// What `council_runs.synthesis_status` records: a synthesis that parsed and validated, or the
/// chairman's raw text kept with the reason it could not be structured (`formats::degraded`).
const SYNTHESIS_OK: &str = "ok";
const SYNTHESIS_DEGRADED: &str = "degraded";

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

/// Why a council could not be started. Every variant is a refusal BEFORE anything is spent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// [`CONFIG_DISPLAY_PATH`] is absent, unreadable, or names a roster the daemon will not run.
    NotConfigured,
    /// The roster asks for a local seat and this daemon has no local model to answer with.
    ///
    /// Distinct from `NotConfigured` because the file may be perfectly good: a per-question roster
    /// override arrives in the request body, and the daemon has to be able to say which half is
    /// wrong.
    NoLocalModel,
    /// The question is empty, or the override roster is not one this daemon would accept.
    Invalid(String),
    /// The budget says no, and names the limit and the spend.
    BudgetExhausted(String),
    /// The active provider's measured quota says no.
    QuotaExhausted(String),
    /// Minting or storing failed and the council would run with a key that authenticates nothing.
    Unavailable(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::NotConfigured => write!(
                formatter,
                "no council is configured — write a roster to {CONFIG_DISPLAY_PATH} and restart"
            ),
            StartError::NoLocalModel => write!(
                formatter,
                "the roster asks for a local seat and no local model is configured"
            ),
            StartError::Invalid(reason) => write!(formatter, "{reason}"),
            StartError::BudgetExhausted(reason) => write!(formatter, "{reason}"),
            StartError::QuotaExhausted(reason) => write!(formatter, "{reason}"),
            StartError::Unavailable(reason) => write!(formatter, "{reason}"),
        }
    }
}

/// The roster a request may put in place of the configured one, for that question only.
///
/// Holds the DECLARED form, exactly as the file does, so an override may name catalogue agents too
/// and gets the same refusals in the same words when it names them wrongly.
#[derive(Debug, Clone, Deserialize)]
pub struct RosterOverride {
    pub chairman: SeatSpec,
    pub members: Vec<SeatSpec>,
}

/// Turns a declared roster into the seats that will run, reading the catalogue as it is NOW.
///
/// **Not at load, and this is the decision that gets made wrong.** `load_council_config` is pure and
/// runs at startup: it has no pool, and could not usefully have one, because the catalogue is
/// mutable state — the owner may delete `cetico` at three in the afternoon with a daemon that has
/// been up since nine. A roster validated at boot is a promise about a different afternoon.
///
/// The accepted cost: a roster naming a deleted agent is discovered only when somebody asks a
/// question. That refusal happens before the budget check and before any row is written, so it
/// costs the owner one clear error and nothing else. It is the same cost a project's
/// `autopilot.yaml`
/// already pays by naming a project root that has since moved.
async fn resolve_roster(
    state: &crate::state::AppState,
    chairman: &SeatSpec,
    members: &[SeatSpec],
) -> Result<(CouncilSeat, Vec<CouncilSeat>), StartError> {
    let resolved_chairman = resolve_seat(state, chairman, &crate::config::seat_name(0)).await?;
    let mut resolved_members = Vec::with_capacity(members.len());
    for (index, spec) in members.iter().enumerate() {
        resolved_members
            .push(resolve_seat(state, spec, &crate::config::seat_name(index + 1)).await?);
    }
    Ok((resolved_chairman, resolved_members))
}

/// One declared seat, resolved. `who` is what a refusal calls it — see `config::seat_name`.
async fn resolve_seat(
    state: &crate::state::AppState,
    spec: &SeatSpec,
    who: &str,
) -> Result<CouncilSeat, StartError> {
    // Checked here and not only in `CouncilConfig::faults`, because an override never passes
    // through the file's validation and this is the one fault whose consequence is silent: a seat
    // naming both would otherwise run as whichever half this function happened to read first.
    if spec.agent.is_some() && (spec.kind.is_some() || spec.model_ref.is_some()) {
        return Err(StartError::Invalid(format!(
            "{who} names both an agent and a model; a seat is filled by one or the other"
        )));
    }

    let Some(agent_id) = spec.agent.as_deref() else {
        let (Some(kind), Some(model_ref)) = (spec.kind, spec.model_ref.clone()) else {
            return Err(StartError::Invalid(format!(
                "{who} names neither a model nor an agent"
            )));
        };
        if model_ref.trim().is_empty() {
            return Err(StartError::Invalid("a seat names no model".to_string()));
        }
        // Deliberately NOT checking `local_assistant` here. A model seat's locality is legible from
        // the file, so `CouncilConfig::faults` has already refused it at load and `start` refuses it
        // for an override — asking a third time would refuse a configured roster that startup
        // accepted, which is a daemon disagreeing with itself.
        return Ok(CouncilSeat::of_model(kind, model_ref));
    };

    let agent = crate::agent::get(&state.pool, agent_id)
        .await
        .map_err(|error| StartError::Unavailable(error.to_string()))?
        .ok_or_else(|| {
            StartError::Invalid(format!(
                "{who} names the agent `{agent_id}`, which is not in the catalogue"
            ))
        })?;

    let kind = crate::config::seat_kind_for_engine(&agent.engine).ok_or_else(|| {
        StartError::Invalid(format!(
            "{who} names the agent `{agent_id}`, whose engine `{}` is not one a seat can run",
            agent.engine
        ))
    })?;
    // The check the file could not make: an agent's engine is a column, so only here is it known
    // that this seat wants this machine. Same refusal and same words as a `{ kind: local }` line —
    // where a seat came from does not change what the machine can serve.
    // `state.local_assistant.is_none()` before the migration to the assistant factory: the field
    // this read no longer exists, so this call site is one of the lines that migration is allowed
    // to touch beyond the `AppState` literal.
    if kind == SeatKind::Local && state.assistants.serves(crate::chats::Brain::Local).is_err() {
        return Err(StartError::NoLocalModel);
    }
    // A seat's row records WHICH MODEL ANSWERED, `NOT NULL`, and that is the whole point of copying
    // it instead of reading the roster back later. An agent may legitimately leave its model unset
    // and let the CLI choose — a team keeps no such record and does not care — but a seat filled by
    // one could only be written down blank. Refused before anything is spent, rather than stored as
    // an empty string somebody would later have to guess the meaning of.
    let model_ref = agent.model.clone().ok_or_else(|| {
        StartError::Invalid(format!(
            "{who} names the agent `{agent_id}`, which names no model — a seat records the model \
             that answered"
        ))
    })?;

    Ok(CouncilSeat {
        kind,
        model_ref,
        agent: Some(SeatAgent {
            id: agent.id,
            name: agent.name,
            prompt: agent.prompt,
            tool_policy: agent.tool_policy,
        }),
    })
}

/// Deletes a council's throwaway MCP config however the council ends.
///
/// Built BEFORE the driver task and captured by it, which is the first of the three cancellation
/// rules: a task aborted before its first poll drops what it captured without running a line of the
/// body, so a guard constructed inside would never exist at all. The same shape as
/// `assistant::TurnGuard`, minus the chat slot.
struct McpConfigGuard {
    path: std::path::PathBuf,
}

impl Drop for McpConfigGuard {
    fn drop(&mut self) {
        // Best-effort: the file is in the temp directory and the next council writes its own. A
        // failure here is worth nothing to report and would be reported on every shutdown.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Where one council's MCP config lives.
///
/// The council id is a uuid this daemon generated, so unlike `assistant::mcp_config_path` there is
/// no untrusted string to encode — but it is filtered anyway rather than trusted, because the cost
/// is one line and the failure it prevents (a `Path::join` that discards its base when the joined
/// component is absolute) is an arbitrary write and delete.
///
/// **The process id is in the name, and it is not decoration.** The temp directory is shared by
/// every process on the machine, so a name built only from the id is the SAME path in two of them
/// — and both write it and both delete it. On this machine that is not hypothetical: the daemon
/// runs while suites run, and several checkouts run suites at once, each with tests that use fixed
/// ids. One deleting the other's config mid-turn is a failure with no cause visible anywhere near
/// it. `transcribe.rs` already names its recordings this way, for the same reason.
fn mcp_config_path(council_id: &str) -> std::path::PathBuf {
    let safe: String = council_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .collect();
    std::env::temp_dir().join(format!(
        "nucleos-council-{}-{safe}.json",
        std::process::id()
    ))
}

/// Starts a council: validates, checks the budget once, writes the record, and spawns the driver.
///
/// Returns as soon as the record exists. Everything after that happens in the background and is
/// read back through `GET /council/{id}` — a council is minutes of work, and a caller holding an
/// HTTP connection open for it would be one dropped request away from a deliberation nobody can
/// find.
///
/// **The budget is read once, here.** Not between phases, which is the tempting place and the wrong
/// one: a council stopped after phase 1 has paid for every answer and produced no synthesis, which
/// is the worst point on the curve. In spend terms a council is atomic — it is refused whole or run
/// whole.
pub async fn start(
    state: &crate::state::AppState,
    question: &str,
    roster: Option<RosterOverride>,
) -> Result<String, StartError> {
    start_with(state, question, roster, None, BTreeMap::new()).await
}

/// [`start`], with the two things a request may ask for beyond the roster: how many critique rounds
/// to run (`None` is the file's), and a role per seat, keyed by the seat index as a string.
///
/// Both are validated BEFORE the budget is read and before anything is written, with the other
/// refusals: a round count outside `1..=config::MAX_COUNCIL_ROUNDS`, a key that is not the index of
/// a seat on this roster, and a role outside the closed set (`formats::Role::parse`, exact and
/// case-sensitive) are each `Invalid`. A typo that quietly became "no role" or "the default rounds"
/// would run — and bill — a council nobody asked for.
pub async fn start_with(
    state: &crate::state::AppState,
    question: &str,
    roster: Option<RosterOverride>,
    rounds: Option<u32>,
    roles: BTreeMap<String, String>,
) -> Result<String, StartError> {
    let configured = state.council.config().ok_or(StartError::NotConfigured)?;
    let token = state
        .council
        .token()
        .ok_or_else(|| {
            StartError::Unavailable(
                "the council has no daemon key; its seats could not call a tool".to_string(),
            )
        })?
        .to_string();

    let question = question.trim();
    if question.is_empty() {
        return Err(StartError::Invalid("the question is empty".to_string()));
    }

    let rounds = rounds.unwrap_or(configured.rounds);
    if !(1..=crate::config::MAX_COUNCIL_ROUNDS).contains(&rounds) {
        return Err(StartError::Invalid(format!(
            "rounds is {rounds}; a council runs 1 to {} rounds",
            crate::config::MAX_COUNCIL_ROUNDS
        )));
    }
    let seat_count = roster
        .as_ref()
        .map_or(configured.members.len(), |roster| roster.members.len());
    let roles = seat_roles(&roles, seat_count)?;

    let (chairman, members) = match &roster {
        Some(override_roster) => {
            let (chairman, members) = (&override_roster.chairman, &override_roster.members);
            if members.is_empty() {
                return Err(StartError::Invalid("the roster has no members".to_string()));
            }
            if members.len() > crate::config::MAX_COUNCIL_SEATS {
                return Err(StartError::Invalid(format!(
                    "a roster may hold at most {} members",
                    crate::config::MAX_COUNCIL_SEATS
                )));
            }
            // The same rule the file is validated against, applied to a roster that never touches
            // the file. An override is for ONE question and writes no configuration, so the
            // daemon's own limits have to be checked here rather than assumed from the file.
            //
            // Only the declared-model seats: an agent seat's locality is a column, and
            // `resolve_seat` refuses it there with this same variant.
            let wants_local = std::iter::once(chairman)
                .chain(members.iter())
                .any(|seat| seat.kind == Some(SeatKind::Local));
            if wants_local && state.assistants.serves(crate::chats::Brain::Local).is_err() {
                return Err(StartError::NoLocalModel);
            }
            resolve_roster(state, chairman, members).await?
        }
        None => resolve_roster(state, &configured.chairman, &configured.members).await?,
    };

    if let crate::budget::BudgetDecision::Pause { reason, source, .. } =
        crate::quota::permits_new_run(state, chrono::Utc::now()).await
    {
        return Err(if source == crate::quota::PAUSE_SOURCE {
            StartError::QuotaExhausted(reason)
        } else {
            StartError::BudgetExhausted(reason)
        });
    }

    let id = crate::auth::generate_uuid_v4();
    // The request's count when it gave one, bounded above by `MAX_COUNCIL_ROUNDS` like the file's
    // — so a caller can ask for more deliberation, never for more than the daemon will ever run.
    let rounds = i64::from(rounds);
    insert_council_with_roles(
        &state.pool,
        &id,
        question,
        &chairman,
        &members,
        rounds,
        &roles,
    )
    .await
    .map_err(|error| StartError::Unavailable(error.to_string()))?;

    let mcp_path = mcp_config_path(&id);
    let exe = std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| StartError::Unavailable(error.to_string()))?;
    crate::storage::write_atomic(
        &mcp_path,
        &serde_json::to_vec(&crate::assistant::build_mcp_config(&exe))
            .map_err(|error| StartError::Unavailable(error.to_string()))?,
    )
    .map_err(|error| StartError::Unavailable(error.to_string()))?;

    let _ = crate::feed::append(
        &state.pool,
        None,
        "council_started",
        &format!("council convened with {} seats", members.len()),
        None,
        Some(&crate::feed::Subject::Council(id.clone())),
    )
    .await;

    // Built here and captured below, so the config is removed however the driver ends — including
    // by being aborted before it ever polls.
    let guard = McpConfigGuard { path: mcp_path };
    let driver = Driver {
        state: state.clone(),
        id: id.clone(),
        question: question.to_string(),
        chairman,
        members,
        token,
        timeout: std::time::Duration::from_secs(configured.timeout_seconds),
        rounds,
        roles,
    };
    tokio::spawn(async move {
        let _guard = guard;
        driver.run().await;
    });

    Ok(id)
}

/// A request's `{seat index: role}` as one entry per seat, or the refusal that names what is wrong.
///
/// The key is parsed as an unsigned index, so `seat-0` and `-1` are refused as not being one, and
/// an index at or past `seat_count` as naming a seat this roster does not have.
fn seat_roles(
    requested: &BTreeMap<String, String>,
    seat_count: usize,
) -> Result<Vec<Option<formats::Role>>, StartError> {
    let mut roles = vec![None; seat_count];
    for (key, value) in requested {
        let seat_idx: usize = key
            .parse()
            .map_err(|_| StartError::Invalid(format!("`{key}` is not a seat index")))?;
        let Some(slot) = roles.get_mut(seat_idx) else {
            return Err(StartError::Invalid(format!(
                "seat {seat_idx} is not on a roster of {seat_count} seats"
            )));
        };
        let role = formats::Role::parse(value).ok_or_else(|| {
            let known: Vec<&str> = formats::Role::ALL.iter().map(|r| r.as_str()).collect();
            StartError::Invalid(format!(
                "`{value}` is not a role; a seat may be one of {}",
                known.join(", ")
            ))
        })?;
        *slot = Some(role);
    }
    Ok(roles)
}

/// Everything one council needs, moved into the background task in one piece.
struct Driver {
    state: crate::state::AppState,
    id: String,
    question: String,
    chairman: CouncilSeat,
    members: Vec<CouncilSeat>,
    /// The council's scoped daemon key — never the control token. See `auth::COUNCIL_ROUTES`.
    token: String,
    /// The wall clock ONE seat gets.
    timeout: std::time::Duration,
    /// Critique rounds asked for, as the row records it. Read from the driver rather than from the
    /// row at each phase boundary, because it cannot change under a running council: the file may be
    /// edited mid-flight and the deliberation that is already paid for has to finish the shape it
    /// started. One critique round runs whatever the count, for now; the revise rounds that make
    /// the count matter come with the next packet of council-deliberacao.
    rounds: i64,
    /// The role each seat plays, by `seat_idx`; `None` for a plain seat.
    roles: Vec<Option<formats::Role>>,
}

/// Where a seat's state is recorded.
///
/// A step of one seat — a row of `council_rounds` keyed by (round, seat, phase) — or the chairman,
/// which has no seat and hangs its run off the council itself.
enum Slot {
    Step {
        round: i64,
        seat_idx: usize,
        phase: &'static str,
        /// The labels a critiquing seat was shown — the filter `formats::parse_critique` applies.
        /// Empty for every other phase.
        shown: Vec<String>,
    },
    Chairman,
}

/// How one seat's invocation ended.
struct SeatOutcome {
    status: &'static str,
    answer: String,
    error: Option<String>,
}

impl Driver {
    async fn run(self) {
        let answers = self.answer_phase().await;
        if self.was_settled().await {
            return;
        }

        // The shuffle is over the seats that answered: a seat with no answer has nothing to be
        // judged on, so it is neither shown to a peer nor asked to judge one — it gets no step
        // after its failed answer.
        let participants: Vec<usize> = answers.keys().copied().collect();
        let anon = anonymize(&self.id, &participants);
        if let Err(error) = set_anon_map(&self.state.pool, &self.id, &anon.anon_map).await {
            tracing::warn!(council = %self.id, %error, "could not record the anonymisation map");
        }

        let critiques = self.critique_phase(1, &answers, &anon).await;
        let rounds_run = i64::from(critiques.is_some());
        if let Err(error) = store::set_progress(&self.state.pool, &self.id, rounds_run, false).await
        {
            tracing::warn!(council = %self.id, %error, "could not record the council's progress");
        }
        if self.was_settled().await {
            return;
        }

        self.chairman_phase(rounds_run, &answers, &anon, &critiques.unwrap_or_default())
            .await;
    }

    /// Moves the council's position, warning rather than failing: the position is what a reader is
    /// shown, never what the driver decides from.
    async fn move_to(&self, round: i64, phase: &str) {
        if let Err(error) = store::set_position(&self.state.pool, &self.id, round, phase).await {
            tracing::warn!(council = %self.id, %error, round, phase, "could not move the council");
        }
    }

    /// The name the chairman knows a seat by: its agent's, or the model's when no agent took it.
    fn seat_name(&self, seat_idx: usize) -> String {
        match self.members.get(seat_idx) {
            Some(seat) => match &seat.agent {
                Some(agent) => agent.name.clone(),
                None => seat.model_ref.clone(),
            },
            None => format!("seat {seat_idx}"),
        }
    }

    fn role_of(&self, seat_idx: usize) -> Option<formats::Role> {
        self.roles.get(seat_idx).copied().flatten()
    }

    /// Whether somebody has already closed this council — a cancel, or a write that lost a race.
    ///
    /// Checked at every phase boundary instead of the driver holding an abort handle of its own.
    /// Cancelling terminates the seat runs, which is what makes the phase in flight END; this is
    /// what stops the phase AFTER it from starting. A row that cannot be read is treated as settled,
    /// because continuing to spend against a record the daemon cannot see is the worse direction.
    async fn was_settled(&self) -> bool {
        match get_council_row(&self.state.pool, &self.id).await {
            Ok(Some(row)) => row.status != STATUS_RUNNING,
            Ok(None) => true,
            Err(error) => {
                tracing::warn!(council = %self.id, %error, "could not read the council; stopping");
                true
            }
        }
    }

    /// Round 0 — every seat answers the owner's question, all at once, behind its role.
    ///
    /// Returns the valid answers by `seat_idx`.
    async fn answer_phase(&self) -> BTreeMap<usize, String> {
        self.move_to(0, store::PHASE_ANSWER).await;
        let running = self.members.iter().enumerate().map(|(seat_idx, seat)| {
            let prompt = prompts::answer_prompt(&self.question, self.role_of(seat_idx));
            // With tools: this is the phase where a seat goes and finds what it needs. The critique
            // and the chairman get none, so nobody can go looking for ammunition after seeing a
            // peer's answer.
            async move {
                let slot = Slot::Step {
                    round: 0,
                    seat_idx,
                    phase: store::PHASE_ANSWER,
                    shown: Vec::new(),
                };
                let outcome = self.run_seat(slot, seat, prompt, true).await;
                (seat_idx, outcome)
            }
        });
        let outcomes = crate::join::all(running).await;

        let mut answers = BTreeMap::new();
        for (seat_idx, outcome) in outcomes {
            if outcome.status == SEAT_OK {
                answers.insert(seat_idx, outcome.answer);
            }
        }

        let _ = crate::feed::append(
            &self.state.pool,
            None,
            "council_stage",
            &format!(
                "council answers done: {} of {} seats answered",
                answers.len(),
                self.members.len()
            ),
            None,
            Some(&crate::feed::Subject::Council(self.id.clone())),
        )
        .await;

        answers
    }

    /// A critique round — each seat that answered reviews and ranks its peers' answers under
    /// shuffled labels, never its own.
    ///
    /// Returns the critiques that could be read, by the critic's `seat_idx`, or `None` when the round
    /// did not run. Below two valid answers it is skipped whole, and skipping is recorded rather
    /// than left blank: a `skipped` step and a `pending` one mean different things, and only one of
    /// them is a council that stopped. A critique that ran and could not be read is an ABSTENTION —
    /// one ballot fewer, recorded `invalid` with the reason — not a failed council.
    async fn critique_phase(
        &self,
        round: i64,
        answers: &BTreeMap<usize, String>,
        anon: &Anonymized,
    ) -> Option<BTreeMap<usize, formats::Critique>> {
        if !critique_should_run(answers.len()) {
            for seat_idx in answers.keys() {
                if let Err(error) = store::upsert_step(
                    &self.state.pool,
                    &self.id,
                    round,
                    *seat_idx as i64,
                    store::PHASE_CRITIQUE,
                    None,
                    SEAT_SKIPPED,
                    None,
                    None,
                )
                .await
                {
                    tracing::warn!(council = %self.id, %error, "could not record a skipped critique");
                }
            }
            return None;
        }

        self.move_to(round, store::PHASE_CRITIQUE).await;

        let critiquing = anon.for_seat.iter().filter_map(|(viewer, labels)| {
            let seat = self.members.get(*viewer)?;
            let peers: Vec<(String, String)> = labels
                .iter()
                .filter_map(|label| {
                    let seat_idx = anon.anon_map.get(label)?;
                    Some((label.clone(), answers.get(seat_idx)?.clone()))
                })
                .collect();
            let shown: Vec<String> = peers.iter().map(|(label, _)| label.clone()).collect();
            let prompt = prompts::critique_prompt(&self.question, self.role_of(*viewer), &peers);
            Some(async move {
                // No tools. A seat that could fetch targeted evidence after reading its peers'
                // answers would turn the critique into a measure of who had time left.
                let slot = Slot::Step {
                    round,
                    seat_idx: *viewer,
                    phase: store::PHASE_CRITIQUE,
                    shown: shown.clone(),
                };
                let outcome = self.run_seat(slot, seat, prompt, false).await;
                (*viewer, shown, outcome)
            })
        });
        let outcomes = crate::join::all(critiquing).await;

        // Parsed again rather than handed back by `record`: the parse is pure, so the ballot the
        // tally reads and the payload the step stores cannot differ.
        let mut critiques = BTreeMap::new();
        for (seat_idx, shown, outcome) in outcomes {
            if outcome.status == SEAT_OK
                && let Ok(critique) = formats::parse_critique(&outcome.answer, &shown)
            {
                critiques.insert(seat_idx, critique);
            }
        }

        let _ = crate::feed::append(
            &self.state.pool,
            None,
            "council_stage",
            &format!(
                "council round {round} of {}: {} of {} critiques read",
                self.rounds,
                critiques.len(),
                answers.len()
            ),
            None,
            Some(&crate::feed::Subject::Council(self.id.clone())),
        )
        .await;

        Some(critiques)
    }

    /// The last phase — the chairman writes the council's synthesis, as structured JSON.
    ///
    /// A chairman RUN that fails is the one failure that settles the council as `error`, and it is
    /// survivable in the way the others are not: the answers and critiques are still readable, and
    /// they are the part with value.
    ///
    /// A chairman that ran and wrote something unreadable is a different fact and does not fail the
    /// council. It is asked ONCE more, with the first prompt plus the reason the first reply was
    /// refused; if that also cannot be used, its raw text is kept as a `degraded` synthesis — shown
    /// as it came, with the reason — rather than thrown away.
    ///
    /// `answers` are the final answers by seat. A seat whose answer failed is absent, so the
    /// chairman is never asked to weigh a position nobody stated.
    async fn chairman_phase(
        &self,
        rounds_run: i64,
        answers: &BTreeMap<usize, String>,
        anon: &Anonymized,
        critiques: &BTreeMap<usize, formats::Critique>,
    ) {
        self.move_to(rounds_run, PHASE_CHAIRMAN).await;

        let ballots: BTreeMap<usize, Vec<String>> = critiques
            .iter()
            .map(|(critic, critique)| (*critic, critique.ranking.clone()))
            .collect();
        let agreement = tally::agreement(&ballots, &anon.anon_map);
        // A seat no ballot scored has no standing to report; listing it at 0.00 would read as
        // "ranked last" when the truth is "never ranked".
        let leaderboard_lines: Vec<String> = tally::borda(&ballots, &anon.anon_map)
            .into_iter()
            .filter(|row| row.n > 0)
            .enumerate()
            .map(|(place, row)| {
                format!(
                    "{}. {} — {:.2}, {} vote(s)",
                    place + 1,
                    self.leaderboard_name(row.seat_idx),
                    row.score,
                    row.n
                )
            })
            .collect();

        let input = prompts::ChairmanInput {
            question: self.question.clone(),
            answers: answers
                .iter()
                .map(|(seat_idx, answer)| prompts::SeatBrief {
                    seat_idx: *seat_idx,
                    name: self.seat_name(*seat_idx),
                    role: self.role_of(*seat_idx),
                    answer: answer.clone(),
                })
                .collect(),
            critiques: critique_counts(critiques, &anon.anon_map),
            leaderboard_lines,
            agreement_level: agreement.level.clone(),
            // Nothing is revised yet; the revise rounds arrive with the next packet.
            changes: Vec::new(),
        };
        let seats: std::collections::BTreeSet<usize> = answers.keys().copied().collect();
        let judge = |raw: &str| -> Result<formats::Synthesis, String> {
            let synthesis = formats::parse_synthesis(raw)?;
            formats::validate_synthesis(&synthesis, &seats, &agreement.level)?;
            Ok(synthesis)
        };

        let first = match self
            .ask_chairman(prompts::chairman_prompt(&input, None))
            .await
        {
            Ok(raw) => raw,
            Err((status, error)) => return self.settle(rounds_run, status, error).await,
        };
        let (synthesis, synthesis_status) = match judge(&first) {
            Ok(synthesis) => (synthesis, SYNTHESIS_OK),
            Err(reason) => {
                if self.was_settled().await {
                    return;
                }
                match self
                    .ask_chairman(prompts::chairman_prompt(&input, Some(&reason)))
                    .await
                {
                    Ok(second) => match judge(&second) {
                        Ok(synthesis) => (synthesis, SYNTHESIS_OK),
                        Err(reason) => (formats::degraded(&second, &reason), SYNTHESIS_DEGRADED),
                    },
                    Err((status, _)) if status == STATUS_CANCELLED => {
                        return self.settle(rounds_run, status, None).await;
                    }
                    // The retry's RUN failed, but the first reply is still text the chairman
                    // wrote: kept as degraded, which is more than an `error` council would show.
                    Err((_, error)) => (
                        formats::degraded(
                            &first,
                            &format!(
                                "{reason}; the retry failed: {}",
                                error.as_deref().unwrap_or("no reason given")
                            ),
                        ),
                        SYNTHESIS_DEGRADED,
                    ),
                }
            }
        };

        let encoded = serde_json::to_string(&synthesis).ok();
        if let Err(error) = store::set_synthesis(
            &self.state.pool,
            &self.id,
            encoded.as_deref(),
            synthesis_status,
        )
        .await
        {
            tracing::warn!(council = %self.id, %error, "could not record the synthesis");
        }
        self.settle(rounds_run, STATUS_DONE, None).await;
    }

    /// The leaderboard's name for a seat: the agent with its model beside it, or the bare model.
    fn leaderboard_name(&self, seat_idx: usize) -> String {
        match self.members.get(seat_idx) {
            Some(seat) => match &seat.agent {
                Some(agent) => format!("{} ({})", agent.name, seat.model_ref),
                None => seat.model_ref.clone(),
            },
            None => format!("seat {seat_idx}"),
        }
    }

    /// One chairman run: its reply, or how the council settles when the run itself did not answer.
    async fn ask_chairman(&self, prompt: String) -> Result<String, (&'static str, Option<String>)> {
        let outcome = self
            .run_seat(Slot::Chairman, &self.chairman, prompt, false)
            .await;
        if outcome.status == SEAT_OK {
            Ok(outcome.answer)
        } else if outcome.status == SEAT_CANCELLED {
            Err((STATUS_CANCELLED, None))
        } else {
            Err((
                STATUS_ERROR,
                Some(
                    outcome
                        .error
                        .unwrap_or_else(|| "the chairman produced no synthesis".to_string()),
                ),
            ))
        }
    }

    /// Settles the council, first writer wins, and says so in the feed when this call was it.
    async fn settle(&self, rounds_run: i64, status: &str, error: Option<String>) {
        if status != STATUS_CANCELLED {
            // Guarded on `running` like every position write, so it is a no-op for a council a
            // cancel has already settled.
            self.move_to(rounds_run, PHASE_DONE).await;
        }
        match finish(&self.state.pool, &self.id, status, error.as_deref()).await {
            Ok(true) => {
                let _ = crate::feed::append(
                    &self.state.pool,
                    None,
                    "council_finished",
                    &format!("council {status}"),
                    None,
                    Some(&crate::feed::Subject::Council(self.id.clone())),
                )
                .await;
            }
            // Somebody settled it first — a cancel, almost always. Their verdict stands.
            Ok(false) => {}
            Err(error) => {
                tracing::error!(council = %self.id, %error, "could not settle the council")
            }
        }
    }

    /// Runs one seat and returns how it ended, with the record written on both sides of it.
    ///
    /// **The run id is persisted BEFORE the model is asked anything**, and that ordering is the
    /// whole of two properties. Somebody reading the record mid-phase sees phase 1 filling in
    /// rather than a blank; and `cancel` finds the seats to terminate by reading exactly these
    /// columns, so a council whose ids landed only at the end of the phase would be a council
    /// nobody could stop while it was working.
    ///
    /// `with_tools` is the phase-1 flag. It decides the tool policy AND whether an MCP config is
    /// passed at all, because those are one decision said twice — a policy of `None` beside a config
    /// file would advertise a server the CLI is forbidden to reach.
    async fn run_seat(
        &self,
        slot: Slot,
        seat: &CouncilSeat,
        prompt: String,
        with_tools: bool,
    ) -> SeatOutcome {
        // The seat's own instructions, ahead of the phase's. Prepended to the prompt rather than
        // sent as a system prompt because `RunRequest` has none, and because
        // `team::specialist_prompt` already answers this exact question this exact way — the
        // persona is the first thing the agent reads.
        //
        // This is the seat's OWN persona and it never travels. Phase 2 shows peers' ANSWERS and
        // nothing else, so a seat knows who it is and never who the others are. What is not
        // defensible, and is said rather than pretended away: a strong persona writes recognisably,
        // and a reader may guess. That is already true between different models today. The system
        // does not print the name; it does not promise the style will not give it away.
        let prompt = match &seat.agent {
            Some(agent) if !agent.prompt.trim().is_empty() => {
                format!("{}\n\n{prompt}", agent.prompt)
            }
            _ => prompt,
        };
        // Two narrowings meeting at an `&&`: the phase says no to tools after phase 1, and an agent
        // whose `tool_policy` is `none` says no to them full stop. Neither direction can ADD one —
        // `mcp_only` is exactly what a seat gets today and `unrestricted` never reaches the
        // catalogue.
        let with_tools = with_tools && seat.allows_tools();

        let (run_id, session_id) = match self.open_run(&prompt).await {
            Ok(opened) => opened,
            Err(error) => {
                // No run row, so nothing to attach; the seat is recorded as failed by its caller's
                // slot all the same.
                let outcome = SeatOutcome {
                    status: SEAT_ERROR,
                    answer: String::new(),
                    error: Some(error.to_string()),
                };
                self.record(&slot, None, &outcome).await;
                return outcome;
            }
        };

        self.record(
            &slot,
            Some(run_id),
            &SeatOutcome {
                status: SEAT_PENDING,
                answer: String::new(),
                error: None,
            },
        )
        .await;

        let outcome = match seat.kind {
            SeatKind::Cloud => {
                self.run_cloud_seat(run_id, session_id, seat, prompt, with_tools)
                    .await
            }
            SeatKind::Local => self.run_local_seat(run_id, seat, prompt, with_tools).await,
        };
        // The REPLY, not the stream it arrived in: a CLI seat's stdout is its whole `stream-json`
        // transcript, and what a peer is shown, a critique is parsed from and the chairman reads is
        // the text the model wrote. A runner that already returns plain text is passed through.
        let outcome = if outcome.status == SEAT_OK {
            let answer = crate::runner::extract_reply(&outcome.answer).unwrap_or(outcome.answer);
            SeatOutcome { answer, ..outcome }
        } else {
            outcome
        };
        self.record(&slot, Some(run_id), &outcome).await;
        outcome
    }

    /// Writes one seat's state: its step, or the chairman's run on the council.
    async fn record(&self, slot: &Slot, run_id: Option<i64>, outcome: &SeatOutcome) {
        let written = match slot {
            Slot::Step {
                round,
                seat_idx,
                phase,
                shown,
            } => {
                let (status, error, payload) = step_record(phase, shown, outcome);
                store::upsert_step(
                    &self.state.pool,
                    &self.id,
                    *round,
                    *seat_idx as i64,
                    phase,
                    run_id,
                    status,
                    error.as_deref(),
                    payload.as_deref(),
                )
                .await
            }
            // The chairman has no seat row; its run hangs off the council itself. Nothing to write
            // when the row could not even be opened.
            Slot::Chairman => match run_id {
                Some(run_id) => set_chairman_run(&self.state.pool, &self.id, run_id).await,
                None => Ok(()),
            },
        };
        if let Err(error) = written {
            tracing::warn!(council = %self.id, %error, "could not record a seat's state");
        }
    }

    /// Inserts the seat's `runs` row before anything is spawned, so the row exists for the hook to
    /// resolve a mode from and for the reconciliation to find if the daemon dies here.
    ///
    /// A `session_id` is assigned by the daemon rather than waited for, exactly as
    /// `runs::create_run_inner` and `assistant::send_message` do it: `budget.rs` keys spend on that
    /// column, so a seat whose stream never announced an id would be money charged against nothing.
    ///
    /// The seat's MODEL is not written here, because `runs` has no column for one — it lives on
    /// `council_seats.model_ref`, which is the row that knows what a seat is.
    async fn open_run(&self, prompt: &str) -> sqlx::Result<(i64, String)> {
        let session_id = crate::auth::generate_uuid_v4();
        let id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, created_at)
             VALUES (?, 'running', ?, ?, ?)",
        )
        .bind(prompt)
        .bind(COUNCIL_MODE)
        .bind(&session_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.state.pool)
        .await?
        .last_insert_rowid();
        Ok((id, session_id))
    }

    async fn run_cloud_seat(
        &self,
        run_id: i64,
        session_id: String,
        seat: &CouncilSeat,
        prompt: String,
        with_tools: bool,
    ) -> SeatOutcome {
        // Section 6 gives council seats machine knowledge only. Append it to the prompt because
        // state.runner may be Codex, and leave no trace because a seat is not an outcome (D15).
        let prompt = match crate::brief::for_prompt(
            &self.state.pool,
            &crate::knowledge::Context::for_project(None),
            &prompt,
            "council",
        )
        .await
        .and_then(|briefing| briefing.block)
        {
            Some(block) => format!("{prompt}{block}"),
            None => prompt,
        };
        let request = crate::runner::RunRequest {
            prompt,
            // The council's OWN key, never `state.token`. `auth::COUNCIL_ROUTES` is what it reaches.
            env: crate::runs::run_env(&self.token, run_id, None, crate::speed::Capacity::solo()),
            cwd: None,
            permission: crate::runner::Permission::Default,
            resume_session_id: None,
            mcp_config: with_tools.then(|| mcp_config_path(&self.id)),
            mcp_job: None,
            // The council writes its config with `build_mcp_config(&exe)` above, so a seat that is
            // given tools is offered the whole surface and pays for the whole surface, which
            // `runner::authored_prompt` reads off the line above.
            tool_policy: if with_tools {
                crate::runner::ToolPolicy::McpOnly
            } else {
                crate::runner::ToolPolicy::None
            },
            progress_timeout: None,
            max_turns: Some(crate::runner::DEFAULT_MAX_TURNS),
            session_id: Some(session_id),
            fork_session: false,
            include_partial_messages: false,
            // Not because a seat is ever steered — `messages` is `None` and no second turn is ever
            // sent — but because this is the only way to keep the prompt OFF the command line.
            // `cli_args` pushes the prompt as a positional argument unless this is set, and Windows
            // caps a command line at 32 767 characters; a phase-2 prompt carries every peer's whole
            // answer, so it passes that ceiling on any question worth asking. The first real council
            // died exactly there: phase 1 succeeded on all three seats (62 KB and 57 KB from the two
            // cloud models), and every cloud seat then failed phase 2 and phase 3 with
            // `ERROR_FILENAME_EXCED_RANGE` — os error 206, whose name says filename and whose
            // meaning is argv. No test could have found it: they all drive a scripted runner that
            // never builds an argv.
            //
            // `steerable: true` with `messages: None` is a documented state, not a borrowed one —
            // the writer task ends after the opening turn and closes stdin, which `run_prompt`'s own
            // comment calls "the one-turn run the argv path performs".
            images: Vec::new(),
            steerable: true,
            // The classifier never sees a seat: `hooks.rs` answers before it, because a
            // `pending_approval` would terminate the seat and mint an approval that resumes into a
            // worktree a council does not have.
            classifier_governs_tools: false,
            messages: None,
            ambient_mcp: false,
            model: Some(seat.model_ref.clone()),
            effort: None,
            fallback_model: Vec::new(),
            add_dirs: Vec::new(),
            max_budget_usd: None,
            agents: Vec::new(),
            append_system_prompt: None,
            denied_tools: Vec::new(),
            session_name: None,
            context_window: None,
            // The wildcard: a seat's `mcp_config` is written per council and already advertises
            // only `COUNCIL_TOOLS`, so there is nothing here left to narrow.
            allowed_mcp_tools: None,
        };

        let (result_tx, result_rx) = tokio::sync::oneshot::channel::<SeatOutcome>();
        let pool = self.state.pool.clone();
        let runner = self.state.runner.clone();
        let timeout = self.timeout;
        crate::runs::spawn_registered(&self.state, run_id, async move {
            // Captured, so an abort drops it and the awaiting driver learns the seat was
            // cancelled — the same mechanism, and the same reason, as every other guard here.
            let result_tx = result_tx;
            // Inside the task, so a cancel aborts a slow adviser along with the seat. Only the
            // effort can move; `off` asks nothing. The decision is kept and reported below with
            // this seat's final status: every stage opens its own run, so one decision is one seat
            // turn and is reported once.
            let mut request = request;
            let decision = crate::seat_advice::route_council_seat(
                &pool,
                runner.router(),
                run_id,
                &mut request,
            )
            .await;
            let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            let outcome = tokio::time::timeout(
                timeout,
                runner.run_prompt(request, session_tx, transcript.clone()),
            )
            .await;
            let completed_at = chrono::Utc::now().to_rfc3339();

            let seat_outcome = match outcome {
                Err(_) => {
                    let partial = transcript.lock().unwrap().clone();
                    let _ = sqlx::query(
                        "UPDATE runs SET status = 'timed_out', stdout = ?, completed_at = ?
                         WHERE id = ? AND status = 'running'",
                    )
                    .bind(&partial)
                    .bind(&completed_at)
                    .bind(run_id)
                    .execute(&pool)
                    .await;
                    SeatOutcome {
                        status: SEAT_TIMEOUT,
                        answer: partial,
                        error: Some("the seat ran out of wall clock".to_string()),
                    }
                }
                Ok(Ok(run)) if run.exit_code == 0 => {
                    let _ = sqlx::query(
                        "UPDATE runs SET status = 'completed', exit_code = 0, stdout = ?,
                                cost_usd = ?, input_tokens = ?, output_tokens = ?,
                                cache_read_tokens = ?, cache_creation_tokens = ?, num_turns = ?,
                                completed_at = ?
                         WHERE id = ? AND status = 'running'",
                    )
                    .bind(&run.stdout)
                    .bind(run.cost_usd)
                    .bind(run.input_tokens)
                    .bind(run.output_tokens)
                    .bind(run.cache_read_tokens)
                    .bind(run.cache_creation_tokens)
                    .bind(run.num_turns)
                    .bind(&completed_at)
                    .bind(run_id)
                    .execute(&pool)
                    .await;
                    SeatOutcome {
                        status: SEAT_OK,
                        answer: run.stdout,
                        error: None,
                    }
                }
                Ok(Ok(run)) => {
                    // A non-zero exit is still a run that spent money, so its cost is recorded
                    // exactly as a successful one's is. The council counts against the budget, and
                    // a failed seat that cost nothing on paper would understate what was spent.
                    //
                    // `num_turns` and the token columns travel here too, and did not used to: a
                    // seat that answers once and then dies at its turn ceiling —
                    // `crate::runner::TURN_CEILING_EXIT_CODE`, a non-zero exit, so it lands in
                    // THIS arm — carries the completed turn's own count and usage in `RunOutcome`
                    // exactly as the `completed` arm above does, and this `UPDATE` was the one
                    // place that dropped them on the floor instead of binding them.
                    //
                    // Cache creation among them, in both arms: the window a seat spent building its
                    // cache is spend like any other, and a row without it reads as cheaper than it was.
                    let _ = sqlx::query(
                        "UPDATE runs SET status = 'failed', exit_code = ?, stdout = ?, stderr = ?,
                                cost_usd = ?, input_tokens = ?, output_tokens = ?,
                                cache_read_tokens = ?, cache_creation_tokens = ?, num_turns = ?,
                                completed_at = ?
                         WHERE id = ? AND status = 'running'",
                    )
                    .bind(run.exit_code)
                    .bind(&run.stdout)
                    .bind(&run.stderr)
                    .bind(run.cost_usd)
                    .bind(run.input_tokens)
                    .bind(run.output_tokens)
                    .bind(run.cache_read_tokens)
                    .bind(run.cache_creation_tokens)
                    .bind(run.num_turns)
                    .bind(&completed_at)
                    .bind(run_id)
                    .execute(&pool)
                    .await;
                    SeatOutcome {
                        status: SEAT_ERROR,
                        answer: run.stdout,
                        error: Some(tail_of(&run.stderr)),
                    }
                }
                Ok(Err(error)) => {
                    let _ = sqlx::query(
                        "UPDATE runs SET status = 'failed', stderr = ?, completed_at = ?
                         WHERE id = ? AND status = 'running'",
                    )
                    .bind(error.to_string())
                    .bind(&completed_at)
                    .bind(run_id)
                    .execute(&pool)
                    .await;
                    SeatOutcome {
                        status: SEAT_ERROR,
                        answer: String::new(),
                        error: Some(error.to_string()),
                    }
                }
            };
            // A cancel aborts this task before it gets here, which is why a cancelled seat is
            // never reported.
            crate::seat_advice::report_council_seat(
                runner.router(),
                decision,
                seat_outcome.status,
                &seat_outcome.answer,
            );
            let _ = result_tx.send(seat_outcome);
        });

        // A dropped sender means the task was aborted, which here means cancelled — the one way a
        // seat's future stops without writing an outcome.
        result_rx.await.unwrap_or(SeatOutcome {
            status: SEAT_CANCELLED,
            answer: String::new(),
            error: None,
        })
    }

    async fn run_local_seat(
        &self,
        run_id: i64,
        seat: &CouncilSeat,
        prompt: String,
        with_tools: bool,
    ) -> SeatOutcome {
        // The FACTORY's client, where this built its own `runner::OllamaChat` against
        // `runner::OLLAMA_BASE_URL`. `assistants::Assistants::local_chat` is the one place that
        // decides which client a local model gets, and asking it here is what stops the two halves
        // of one daemon drifting onto different engines: on an install whose `local_engine` is
        // `openai_compatible`, the owner's chat reached the server they configured while every seat reached
        // an Ollama that may not be running, may not hold the model, and may not be the machine
        // that was paid for. A seat that fails for that reason — or answers as some other model —
        // looks from outside exactly like a seat that simply had nothing to say.
        let chat = match self.state.assistants.local_chat(&seat.model_ref) {
            Ok(chat) => chat,
            Err(refusal) => {
                // An outcome, never a panic and never an `unwrap`. This runs inside a spawned
                // task's caller and the council's whole design is that a seat which failed casts
                // no votes (`council.rs:14`), so the remaining seats go on without this one. The
                // refusal's OWN sentence travels — `Refusal::message` is what the chat route
                // already shows an operator for the same misconfiguration, and a second, looser
                // wording invented here would describe the same problem differently depending on
                // which door somebody came through.
                let message = refusal.message(crate::chats::Brain::Local);
                // The row is closed here rather than left `running`, exactly as the transport
                // error arm below closes it: nothing was spawned, so nothing else ever will.
                let _ = sqlx::query(
                    "UPDATE runs SET status = 'failed', stderr = ?, cost_usd = 0, completed_at = ?
                     WHERE id = ? AND status = 'running'",
                )
                .bind(&message)
                .bind(chrono::Utc::now().to_rfc3339())
                .bind(run_id)
                .execute(&self.state.pool)
                .await;
                return SeatOutcome {
                    status: SEAT_ERROR,
                    answer: String::new(),
                    error: Some(message),
                };
            }
        };
        // The council's list, not the chat's: `LOCAL_TOOLS` carries `create_run` and `create_job`,
        // which is exactly what a seat must not have. Phases 2 and 3 get an empty box — no tools is
        // no tools whichever machine answers.
        //
        // UNCHANGED by the move to the factory above, and deliberately so: `local_chat` hands out
        // the engine and nothing else, so the toolbox stays `LocalToolBox::for_council` here. It
        // is this half — not the client — that keeps `create_run` and `create_job` away from a
        // seat, and taking the chat route's box along with its client would have handed them over.
        let tools = crate::mcp_tools::LocalToolBox::for_council(
            daemon_url(),
            self.token.clone(),
            self.state.pool.clone(),
        );
        let no_tools = crate::local_agent::NoTools;

        let (result_tx, result_rx) = tokio::sync::oneshot::channel::<SeatOutcome>();
        let pool = self.state.pool.clone();
        let timeout = self.timeout;
        crate::runs::spawn_registered(&self.state, run_id, async move {
            let result_tx = result_tx;
            let tool_box: &dyn crate::local_agent::ToolBox =
                if with_tools { &tools } else { &no_tools };
            // Owned out here rather than read off the returned `Turn`, for the reason `run_turn`
            // gives: a wall-clock timeout drops that future and a transport error propagates past
            // it, and in both cases a seat that HAS read mail would be written down clean.
            let taint = std::sync::atomic::AtomicBool::new(false);
            let turn = tokio::time::timeout(
                timeout,
                crate::local_agent::run_turn(
                    // `as_ref`, because what the factory hands back is a `Box<dyn LocalChat>` and
                    // the loop takes the trait object itself - the box is the seam, not the value.
                    chat.as_ref(),
                    tool_box,
                    crate::local_agent::SYSTEM_PROMPT,
                    &[],
                    &prompt,
                    &taint,
                ),
            )
            .await;
            let completed_at = chrono::Utc::now().to_rfc3339();
            // A seat can never act, so the latch cannot stop anything here the way it stops a chat
            // turn — but the row is what the REST of the system reads, and a council seat that read
            // mail must be as legible as any other run that did.
            if taint.load(std::sync::atomic::Ordering::SeqCst)
                && let Err(error) = crate::runs::mark_untrusted_context(&pool, run_id).await
            {
                tracing::warn!(
                    run_id,
                    %error,
                    "could not mark a council seat as having read third-party text"
                );
            }

            let seat_outcome = match turn {
                Err(_) => {
                    let _ = sqlx::query(
                        "UPDATE runs SET status = 'timed_out', completed_at = ?
                         WHERE id = ? AND status = 'running'",
                    )
                    .bind(&completed_at)
                    .bind(run_id)
                    .execute(&pool)
                    .await;
                    SeatOutcome {
                        status: SEAT_TIMEOUT,
                        answer: String::new(),
                        error: Some("the seat ran out of wall clock".to_string()),
                    }
                }
                Ok(Ok(turn)) => {
                    // `cost_usd = 0` and not NULL. A local seat spends nothing, and NULL is what
                    // `budget.rs` time-approximates a cost for — an unmeasured run — so leaving it
                    // unset would charge the window for electricity.
                    let _ = sqlx::query(
                        "UPDATE runs SET status = 'completed', exit_code = 0, stdout = ?,
                                cost_usd = 0, completed_at = ?
                         WHERE id = ? AND status = 'running'",
                    )
                    .bind(&turn.answer)
                    .bind(&completed_at)
                    .bind(run_id)
                    .execute(&pool)
                    .await;
                    SeatOutcome {
                        status: SEAT_OK,
                        answer: turn.answer,
                        error: None,
                    }
                }
                Ok(Err(error)) => {
                    let _ = sqlx::query(
                        "UPDATE runs SET status = 'failed', stderr = ?, cost_usd = 0,
                                completed_at = ?
                         WHERE id = ? AND status = 'running'",
                    )
                    .bind(error.to_string())
                    .bind(&completed_at)
                    .bind(run_id)
                    .execute(&pool)
                    .await;
                    SeatOutcome {
                        status: SEAT_ERROR,
                        answer: String::new(),
                        error: Some(error.to_string()),
                    }
                }
            };
            let _ = result_tx.send(seat_outcome);
        });

        result_rx.await.unwrap_or(SeatOutcome {
            status: SEAT_CANCELLED,
            answer: String::new(),
            error: None,
        })
    }
}

/// PURE: what one step's row says, given how its run ended — `(status, error, payload)`.
///
/// An answer that came back `ok` is stored as `{"answer": text}`. A critique that came back `ok` is
/// parsed against the labels the seat was shown: what parses is stored as the ballot, already
/// narrowed to those labels; what does not is `invalid`, with the parser's reason — the run
/// finished, so its id stays on the step and the transcript stays reachable. Every other ending
/// keeps its own status and error, and carries no payload.
fn step_record(
    phase: &str,
    shown: &[String],
    outcome: &SeatOutcome,
) -> (&'static str, Option<String>, Option<String>) {
    if outcome.status != SEAT_OK {
        return (outcome.status, outcome.error.clone(), None);
    }
    if phase == store::PHASE_CRITIQUE {
        return match formats::parse_critique(&outcome.answer, shown) {
            Ok(critique) => (SEAT_OK, None, serde_json::to_string(&critique).ok()),
            Err(reason) => (store::STEP_INVALID, Some(reason), None),
        };
    }
    let payload = formats::AnswerPayload {
        answer: outcome.answer.clone(),
    };
    (SEAT_OK, None, serde_json::to_string(&payload).ok())
}

/// PURE: how the critiques landed on each answer — stance counts and the reasons given against it,
/// by the seat that wrote the answer.
///
/// A review of a label that names no seat, or of the critic's own answer, is ignored for the reason
/// `tally::borda` ignores it: the critic was never shown it.
fn critique_counts(
    critiques: &BTreeMap<usize, formats::Critique>,
    anon_map: &BTreeMap<String, usize>,
) -> Vec<prompts::SeatCritiques> {
    let mut counts: BTreeMap<usize, prompts::SeatCritiques> = BTreeMap::new();
    for (critic, critique) in critiques {
        for review in &critique.reviews {
            let Some(&seat) = anon_map.get(&review.label) else {
                continue;
            };
            if seat == *critic {
                continue;
            }
            let entry = counts
                .entry(seat)
                .or_insert_with(|| prompts::SeatCritiques {
                    seat_idx: seat,
                    agree: 0,
                    disagree: 0,
                    unsure: 0,
                    disagree_whys: Vec::new(),
                });
            for point in &review.points {
                match point.stance {
                    formats::Stance::Agree => entry.agree += 1,
                    formats::Stance::Disagree => {
                        entry.disagree += 1;
                        entry.disagree_whys.push(point.why.clone());
                    }
                    formats::Stance::Unsure => entry.unsure += 1,
                }
            }
        }
    }
    counts.into_values().collect()
}

/// The last of a stderr stream, for the seat's error column.
///
/// Bounded because a CLI that fails at startup can produce a great deal of it, and the column is
/// read in a table beside seven others. The TAIL rather than the head: what killed a process is at
/// the end of what it said.
fn tail_of(text: &str) -> String {
    const LIMIT: usize = 600;
    let trimmed = text.trim();
    if trimmed.chars().count() <= LIMIT {
        return trimmed.to_string();
    }
    let tail: String = trimmed
        .chars()
        .skip(trimmed.chars().count() - LIMIT)
        .collect();
    format!("…{tail}")
}

/// Terminates every seat run still in flight and settles the council as `cancelled`.
///
/// The order is deliberate and is the third cancellation rule: the RECORD is settled first, then
/// the runs are terminated. Terminating first would let the driver reach its next phase boundary,
/// read a council still marked `running`, and start phase 2 over the corpses of phase 1.
///
/// Returns whether this call is the one that settled it.
pub async fn cancel(state: &crate::state::AppState, id: &str) -> sqlx::Result<bool> {
    let settled = finish(&state.pool, id, STATUS_CANCELLED, None).await?;

    let chairman_run = get_council_row(&state.pool, id)
        .await?
        .and_then(|row| row.chairman_run_id);
    // Every step's run, whatever its round and phase. Not conditional on `rounds`: a cancel that had
    // to know the shape of the council it was stopping would be one shape away from leaving a
    // process running.
    let mut opened: Vec<i64> = store::steps_of(&state.pool, id)
        .await?
        .into_iter()
        .filter_map(|step| step.run_id)
        .collect();
    opened.extend(chairman_run);
    for run_id in opened {
        // `finalize_termination` is the atomic-handle arbiter: it is a no-op for a run that has
        // already ended, so calling it for every run this council ever opened is correct as well as
        // simple.
        crate::runs::finalize_termination(state, run_id, "cancelled").await;
    }
    // After the runs, so a step whose seat future records `cancelled` on its own and one this
    // settles end up the same; a step that already ended keeps how it ended.
    store::cancel_pending_steps(&state.pool, id).await?;

    if settled {
        let _ = crate::feed::append(
            &state.pool,
            None,
            "council_finished",
            "council cancelled",
            None,
            Some(&crate::feed::Subject::Council(id.to_owned())),
        )
        .await;
    }
    Ok(settled)
}

/// Marks every council left `running` by a previous life as `error`, at startup.
///
/// Its counterpart for the seats is `runs::reconcile_orphaned_runs`, which has already run by the
/// time this does — so by now no council has a live seat, and "still `running`" means "abandoned"
/// with no further test needed. A council left this way is not merely untidy: it is `running` for
/// ever in the list, with a phase that will never advance and no driver to advance it.
pub async fn reconcile(pool: &sqlx::SqlitePool) -> sqlx::Result<u64> {
    let reconciled: Vec<(String,)> = sqlx::query_as(
        "UPDATE council_runs SET status = ?, error = ?
         WHERE status = ?
         RETURNING id",
    )
    .bind(STATUS_ERROR)
    .bind("the daemon stopped while this council was deliberating")
    .bind(STATUS_RUNNING)
    .fetch_all(pool)
    .await?;
    // The steps those councils left `pending` will never be finished by anybody either; settled as
    // `error` rather than drawn as in progress for ever. A step that ended keeps how it ended.
    store::error_orphan_steps(pool).await?;
    Ok(reconciled.len() as u64)
}

/// The statuses that mean a council is over.
///
/// A positive list rather than `<> 'running'`, and the difference is which way the mistake falls. A
/// status added later and forgotten here simply never ages out — a row too many. Spelled as the
/// negative, the same oversight would delete councils in a state nobody had thought about yet.
pub const TERMINAL_COUNCIL_STATUSES: [&str; 3] = [STATUS_DONE, STATUS_ERROR, STATUS_CANCELLED];

/// How long a finished council stays in the record.
///
/// Ninety days, matching the FEED rather than the thirty a run keeps its transcript, and the gap
/// between the two windows is the point. A council's bulk was never in these tables: the answers
/// live in the transcripts of the `runs` rows the seats opened, and those are emptied on their own
/// window by `runs::prune_transcripts`. So a council past a month is ALREADY a question, a
/// leaderboard and a set of seats whose `answer` reads `null` — one short line about a deliberation
/// that happened, which is what a feed entry is and why it gets a feed entry's window.
///
/// A bound at all, because this was the one table that grew with use and freed nothing: every
/// council ever asked, plus a row per seat, kept for as long as the install exists.
pub const DEFAULT_COUNCIL_RETENTION_DAYS: i64 = 90;

/// The window, overridable the way `runs`, `feed` and `worktree` allow theirs to be.
pub(crate) fn retention_days() -> i64 {
    std::env::var("NUCLEOS_COUNCIL_RETENTION_DAYS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_COUNCIL_RETENTION_DAYS)
}

/// Removes finished councils past the window, and their seats with them. Returns how many went.
///
/// The row is DELETED, which is where this parts company with `runs::prune_transcripts` — that one
/// keeps the row and empties the transcript, because a feed entry, a job item or a proposal still
/// points at a run. NOTHING points at a council. A council stripped of its content would be a row
/// that answers every question with `null` for ever, which is worse than its absence.
///
/// Only terminal councils. A row still `running` is one a driver may be part-way through writing,
/// and deleting it would leave `store::upsert_step` and `finish` updating nothing while the seats went on
/// answering. Councils abandoned by a stopped daemon are settled by [`reconcile`] at startup, so
/// they reach this sweep as `error` — the reconciliation is what makes "only terminal" safe rather
/// than a way for a crashed council to become immortal.
///
/// The seats go through the `council_seats_follow_councils` trigger rather than a second statement
/// here. `a_finished_council_and_its_seats_go_past_the_window` is what proves the cascade fires for
/// a multi-row delete and not only for the single-row one `seats_do_not_outlive_their_council`
/// exercises.
pub async fn prune(
    pool: &sqlx::SqlitePool,
    retain_days: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<u64> {
    if retain_days <= 0 {
        // Zero would delete every council on the machine at the next sweep, which is not a retention
        // policy but a typo with a plausible-looking value. The reading `feed::prune` and
        // `runs::prune_transcripts` both make.
        return Ok(0);
    }

    // RFC 3339 built in Rust and not SQLite's `datetime('now', '-N days')`, for the reason
    // `web::prune` sets out at length and `runs::prune_transcripts` repeats: `created_at` is
    // `2026-05-10T12:00:00+00:00`, `datetime()` returns `2026-05-10 12:00:00`, and TEXT comparison
    // puts `T` (0x54) after the space (0x20) — so within the cutoff's own day a council hours too
    // old compares as newer and survives every sweep for ever.
    // `council_retention_is_exact_at_the_boundary` is the test that fails if this is ever changed
    // back; the coarse one beside it would not notice.
    let cutoff = (now - chrono::Duration::days(retain_days)).to_rfc3339();

    // Aged from `created_at` because a council has no `completed_at` and does not earn a column for
    // one: the whole deliberation is bounded by `timeout_seconds`, so start and end differ by
    // minutes against a window measured in months.
    //
    // `AssertSqlSafe` because sqlx otherwise takes only `&'static str`. The one interpolated thing
    // is a row of `?` generated from a constant's length — every status and the cutoff are bound —
    // so nothing caller-supplied reaches the SQL text (the justification `runs::prune_transcripts`
    // and `vcs::reap_requests_of_ended_runs` give).
    let placeholders = vec!["?"; TERMINAL_COUNCIL_STATUSES.len()].join(", ");
    let mut delete = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DELETE FROM council_runs WHERE status IN ({placeholders}) AND created_at < ?"
    )));
    for status in TERMINAL_COUNCIL_STATUSES {
        delete = delete.bind(status);
    }
    Ok(delete.bind(&cutoff).execute(pool).await?.rows_affected())
}

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
    /// How far the LAST critique round's ballots agree. `None` when no critique round exists.
    pub agreement: Option<tally::Agreement>,
    /// The last critique round's Borda leaderboard — THE leaderboard. Computed from the stored
    /// ballots on every read, never read off the legacy `leaderboard` column, so it cannot drift
    /// from the votes it claims to summarise.
    pub leaderboard: Vec<tally::BordaRow>,
    /// Every critique round's leaderboard, in round order, so a client can show how standings moved.
    pub leaderboard_by_round: Vec<Vec<tally::BordaRow>>,
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

pub async fn post_council(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::Json(request): axum::Json<CreateCouncilRequest>,
) -> Result<
    (axum::http::StatusCode, axum::Json<CreateCouncilResponse>),
    (axum::http::StatusCode, String),
> {
    match start(&state, &request.question, request.roster).await {
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

    let leaderboard_by_round: Vec<Vec<tally::BordaRow>> = ballots_by_round
        .values()
        .map(|ballots| tally::borda(ballots, &anon_map))
        .collect();
    let leaderboard = leaderboard_by_round.last().cloned().unwrap_or_default();
    let agreement = ballots_by_round
        .values()
        .last()
        .map(|ballots| tally::agreement(ballots, &anon_map));

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
        agreement,
        leaderboard,
        leaderboard_by_round,
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

/// A completed run's text, or `None` for a run that produced none.
///
/// An empty transcript reads as `None` rather than as `Some("")`, because to a client those mean
/// "nothing yet" and "the seat answered with nothing", and only one of them is true here.
async fn transcript_of(pool: &sqlx::SqlitePool, run_id: i64) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT stdout FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .flatten()
        .filter(|text| !text.trim().is_empty())
}

fn internal(error: sqlx::Error) -> (axum::http::StatusCode, String) {
    tracing::warn!(%error, "council: a database read failed");
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "could not read the council".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seats(count: usize) -> Vec<usize> {
        (0..count).collect()
    }

    /// The same seed and the same seats must give the same map, or `anon_map` on the row describes
    /// a shuffle nobody can recompute and phase 2 becomes unauditable.
    #[test]
    fn anonymize_is_deterministic_for_a_seed() {
        let first = anonymize("council-abc", &seats(5));
        let second = anonymize("council-abc", &seats(5));
        assert_eq!(first, second);

        // And the order the caller happened to collect the seats in is not part of the input.
        let shuffled_input = anonymize("council-abc", &[3, 0, 4, 1, 2]);
        assert_eq!(first, shuffled_input);

        // A different council shuffles differently, which is the property that stops a seat from
        // learning the mapping once and knowing it for ever.
        let elsewhere = anonymize("council-xyz", &seats(5));
        assert_ne!(first.anon_map, elsewhere.anon_map);
    }

    /// The rule phase 2 rests on. A seat that could see its own answer could rank itself first,
    /// and the leaderboard would measure confidence rather than quality.
    #[test]
    fn no_seat_ever_sees_its_own_response() {
        let anonymized = anonymize("council-abc", &seats(6));
        for (viewer, visible) in &anonymized.for_seat {
            for label in visible {
                assert_ne!(
                    anonymized.anon_map[label], *viewer,
                    "seat {viewer} was shown its own answer under {label}"
                );
            }
            assert_eq!(
                visible.len(),
                5,
                "a seat must see every peer and only its own answer is withheld"
            );
        }
    }

    /// Two seats under one label would make every vote for that label ambiguous, and
    /// `aggregate_rankings` would silently attribute both to whichever won the map.
    #[test]
    fn every_label_maps_to_exactly_one_seat() {
        let anonymized = anonymize("council-abc", &seats(8));
        let mapped: std::collections::BTreeSet<usize> =
            anonymized.anon_map.values().copied().collect();
        assert_eq!(mapped.len(), 8);
        assert_eq!(anonymized.anon_map.len(), 8);
        assert_eq!(
            anonymized.anon_map.keys().cloned().collect::<Vec<_>>(),
            vec!["A", "B", "C", "D", "E", "F", "G", "H"]
        );
    }

    #[test]
    fn labels_stay_distinct_past_the_alphabet() {
        assert_eq!(label_for(0), "A");
        assert_eq!(label_for(25), "Z");
        assert_eq!(label_for(26), "AA");
        assert_eq!(label_for(27), "AB");
        let distinct: std::collections::BTreeSet<String> = (0..60).map(label_for).collect();
        assert_eq!(distinct.len(), 60);
    }

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    /// A resolved seat, for the writes that take one.
    fn seat(kind: SeatKind, model_ref: &str) -> CouncilSeat {
        CouncilSeat::of_model(kind, model_ref)
    }

    /// A DECLARED seat in the old form, for rosters and overrides.
    fn spec(kind: SeatKind, model_ref: &str) -> SeatSpec {
        SeatSpec {
            kind: Some(kind),
            model_ref: Some(model_ref.to_string()),
            agent: None,
        }
    }

    /// A declared seat in the new form.
    fn agent_spec(id: &str) -> SeatSpec {
        SeatSpec {
            agent: Some(id.to_string()),
            ..SeatSpec::default()
        }
    }

    fn agent_request(name: &str) -> crate::agent::AgentRequest {
        crate::agent::AgentRequest {
            name: name.to_string(),
            speciality: format!("the speciality of {name}"),
            prompt: format!("The standing instructions of {name}."),
            engine: "claude".to_string(),
            model: Some("claude-opus-5".to_string()),
            tool_policy: "mcp_only".to_string(),
        }
    }

    /// Puts one agent in the catalogue and returns its id.
    async fn catalogue(pool: &sqlx::SqlitePool, request: crate::agent::AgentRequest) -> String {
        crate::agent::create(pool, request).await.unwrap().id
    }

    #[tokio::test]
    async fn a_council_and_its_seats_are_written_together() {
        let pool = test_pool().await;
        insert_council(
            &pool,
            "c1",
            "why?",
            &seat(SeatKind::Cloud, "claude-opus-4-8"),
            &[
                seat(SeatKind::Cloud, "claude-opus-4-8"),
                seat(SeatKind::Local, "qwen3.5:4b"),
            ],
            1,
        )
        .await
        .unwrap();

        let row = get_council_row(&pool, "c1").await.unwrap().unwrap();
        assert_eq!(row.status, STATUS_RUNNING);
        assert_eq!(row.stage, 1);
        assert_eq!(row.anon_seed, "c1", "the seed is the council's own id");
        assert_eq!(row.chairman_ref, "claude-opus-4-8");
        assert_eq!(row.anon_map, None);
        // Where a council starts: round 0, the answer phase.
        assert_eq!(row.current_round, 0);
        assert_eq!(row.current_phase, store::PHASE_ANSWER);

        let seats = get_seat_rows(&pool, "c1").await.unwrap();
        assert_eq!(seats.len(), 2);
        assert_eq!(seats[0].seat_idx, 0);
        assert_eq!(seats[1].kind, "local");
        assert_eq!(seats[1].model_ref, "qwen3.5:4b");
        // A council inserted without roles seats every member as itself.
        assert!(seats.iter().all(|seat| seat.role.is_none()));

        pool.close().await;
    }

    /// The first terminal verdict wins, so a cancel is not overwritten by the failure it caused.
    #[tokio::test]
    async fn only_the_first_verdict_settles_a_council() {
        let pool = test_pool().await;
        insert_council(
            &pool,
            "c1",
            "why?",
            &seat(SeatKind::Cloud, "m"),
            &[seat(SeatKind::Cloud, "m")],
            1,
        )
        .await
        .unwrap();

        assert!(finish(&pool, "c1", STATUS_CANCELLED, None).await.unwrap());
        assert!(
            !finish(&pool, "c1", STATUS_ERROR, Some("the chairman failed"))
                .await
                .unwrap()
        );

        let row = get_council_row(&pool, "c1").await.unwrap().unwrap();
        assert_eq!(row.status, STATUS_CANCELLED);
        assert_eq!(row.error, None);

        pool.close().await;
    }

    /// A settled council does not move phase, or a task that has not noticed the cancellation would
    /// keep advancing a record somebody already closed.
    #[tokio::test]
    async fn a_settled_council_does_not_advance() {
        let pool = test_pool().await;
        insert_council(
            &pool,
            "c1",
            "why?",
            &seat(SeatKind::Cloud, "m"),
            &[seat(SeatKind::Cloud, "m")],
            1,
        )
        .await
        .unwrap();
        finish(&pool, "c1", STATUS_CANCELLED, None).await.unwrap();

        store::set_position(&pool, "c1", 1, store::PHASE_CRITIQUE)
            .await
            .unwrap();
        let row = get_council_row(&pool, "c1").await.unwrap().unwrap();
        assert_eq!(
            (row.current_round, row.current_phase.as_str()),
            (0, store::PHASE_ANSWER)
        );

        pool.close().await;
    }

    #[tokio::test]
    async fn councils_are_listed_newest_first() {
        let pool = test_pool().await;
        for id in ["c1", "c2", "c3"] {
            insert_council(
                &pool,
                id,
                "why?",
                &seat(SeatKind::Cloud, "m"),
                &[seat(SeatKind::Cloud, "m")],
                1,
            )
            .await
            .unwrap();
        }

        let listed = list_council_rows(&pool, 2, 0).await.unwrap();
        assert_eq!(listed.len(), 2);
        // Same-second timestamps are the ordinary case in a test and a real one in a script; the id
        // tie-break is what keeps the page stable rather than arbitrary.
        assert_eq!(listed[0].id, "c3");
        assert_eq!(listed[1].id, "c2");
        assert_eq!(list_council_rows(&pool, 2, 2).await.unwrap()[0].id, "c1");

        pool.close().await;
    }

    // ── The three phases, driven end to end ──────────────────────────────────────────────────
    //
    // Against a runner that answers by PHASE rather than by call order, which is what the Python
    // this was ported from did in its own harness: the phase a prompt belongs to is legible from
    // the prompt, and keying on it makes a test that reads like the thing it describes rather than
    // like a queue somebody has to count.

    /// What one scripted seat does.
    #[derive(Clone)]
    enum Scripted {
        Answers(String),
        /// The CLI ran and exited non-zero: the model refused, or the tool it wanted was denied.
        Fails(String),
        /// The CLI answered once and then hit its turn ceiling on a later turn: a real, non-zero
        /// exit — `crate::runner::TURN_CEILING_EXIT_CODE` — carrying the completed turn's own
        /// `cost_usd`/`num_turns`/token counts, exactly as `runner.rs`'s own ceiling test proves
        /// `RunOutcome` looks in that case. Distinct from `Fails`, whose fixture leaves every one of
        /// those `None` and so cannot tell a caller that drops them from one that does not.
        DiesAtTheCeiling,
        /// The launch itself failed — no work done, nothing spent.
        WillNotLaunch(String),
        /// Never returns, so the seat's own wall clock is what ends it.
        Hangs,
    }

    #[derive(Default)]
    struct ScriptedRunner {
        /// The answer phase: every prompt that carries none of the three phase markers.
        stage1: std::sync::Mutex<std::collections::VecDeque<Scripted>>,
        /// Prompts carrying `prompts::CRITIQUE_MARKER`.
        critique: std::sync::Mutex<std::collections::VecDeque<Scripted>>,
        /// Prompts carrying `prompts::REVISE_MARKER` — only a council of more than one round.
        revise: std::sync::Mutex<std::collections::VecDeque<Scripted>>,
        /// Prompts carrying `prompts::CHAIRMAN_MARKER`, one reply per attempt — a queue because the
        /// chairman is retried once on an unreadable synthesis. Empty means a valid synthesis.
        chairman: std::sync::Mutex<std::collections::VecDeque<Scripted>>,
        /// Every request, in the order it arrived, for the assertions about HOW a seat was launched.
        seen: std::sync::Mutex<Vec<SeenRequest>>,
    }

    struct SeenRequest {
        prompt: String,
        tool_policy: crate::runner::ToolPolicy,
        mcp_config: Option<std::path::PathBuf>,
        token: Option<String>,
        /// Whether the prompt travelled on stdin rather than on the command line.
        steerable: bool,
    }

    impl ScriptedRunner {
        fn next_for(&self, prompt: &str) -> Scripted {
            // Routed on the constants `prompts` exports and not on prose, so a reworded prompt
            // cannot silently send one phase's reply to another. The chairman is asked FIRST
            // because its prompt quotes answers and critiques that could carry any other text.
            if prompt.contains(prompts::CHAIRMAN_MARKER) {
                self.chairman
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| Scripted::Answers(synthesis_reply("the synthesis")))
            } else if prompt.contains(prompts::REVISE_MARKER) {
                self.revise
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Scripted::Answers(String::new()))
            } else if prompt.contains(prompts::CRITIQUE_MARKER) {
                self.critique
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Scripted::Answers(String::new()))
            } else {
                self.stage1
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Scripted::Answers(String::new()))
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::runner::CommandRunner for ScriptedRunner {
        async fn run_prompt(
            &self,
            request: crate::runner::RunRequest,
            _session_tx: tokio::sync::mpsc::UnboundedSender<String>,
            _transcript: std::sync::Arc<std::sync::Mutex<String>>,
        ) -> std::io::Result<crate::runner::RunOutcome> {
            let scripted = self.next_for(&request.prompt);
            self.seen.lock().unwrap().push(SeenRequest {
                prompt: request.prompt.clone(),
                tool_policy: request.tool_policy,
                mcp_config: request.mcp_config.clone(),
                token: request
                    .env
                    .iter()
                    .find(|(key, _)| key == "NUCLEOS_DAEMON_TOKEN")
                    .map(|(_, value)| value.clone()),
                steerable: request.steerable,
            });

            let blank = crate::runner::RunOutcome {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                session_id: None,
                cost_usd: Some(0.25),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
            };
            match scripted {
                Scripted::Answers(text) => Ok(crate::runner::RunOutcome {
                    stdout: text,
                    ..blank
                }),
                Scripted::Fails(stderr) => Ok(crate::runner::RunOutcome {
                    exit_code: 1,
                    stderr,
                    ..blank
                }),
                Scripted::DiesAtTheCeiling => Ok(crate::runner::RunOutcome {
                    exit_code: crate::runner::TURN_CEILING_EXIT_CODE,
                    stderr: "nucleos: stopped after 3 turns; this run's ceiling was 3\n"
                        .to_string(),
                    cost_usd: Some(0.05),
                    input_tokens: Some(11),
                    output_tokens: Some(22),
                    cache_read_tokens: Some(0),
                    cache_creation_tokens: Some(7),
                    num_turns: Some(1),
                    ..blank
                }),
                Scripted::WillNotLaunch(reason) => Err(std::io::Error::other(reason)),
                Scripted::Hangs => {
                    std::future::pending::<()>().await;
                    unreachable!("a hung seat is ended by its clock or by a cancel")
                }
            }
        }
    }

    fn roster(members: usize) -> CouncilConfig {
        CouncilConfig {
            // One second, so a hung seat is a fast test rather than a ten-minute one.
            timeout_seconds: 1,
            rounds: crate::config::DEFAULT_COUNCIL_ROUNDS,
            consumers: crate::config::CouncilConsumers::default(),
            chairman: spec(SeatKind::Cloud, "the-chairman"),
            members: (0..members)
                .map(|index| spec(SeatKind::Cloud, &format!("model-{index}")))
                .collect(),
        }
    }

    async fn council_state(
        runner: std::sync::Arc<ScriptedRunner>,
        config: Option<CouncilConfig>,
    ) -> crate::state::AppState {
        crate::state::AppState {
            token: crate::auth::Token("control-token".into()),
            pool: test_pool().await,
            telegram_doctrine: None,
            runner,
            triage_runner: None,
            local_triage_disabled: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            files_root: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: std::sync::Arc::new(crate::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            quota: std::sync::Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: std::sync::Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(CouncilRuntime::new(
                config,
                Some("council-key".to_string()),
            )),
            run_handles: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_messages: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_tails: Default::default(),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    /// Waits for the driver to settle the council, or gives up loudly.
    ///
    /// Polling rather than a signal, because polling is what the shell does too: if a council can
    /// only be observed through a channel the test holds, the test is not exercising the surface
    /// anybody actually reads.
    async fn settled(state: &crate::state::AppState, id: &str) -> CouncilRow {
        for _ in 0..600 {
            let row = get_council_row(&state.pool, id).await.unwrap().unwrap();
            if row.status != STATUS_RUNNING {
                return row;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("the council never settled");
    }

    /// Every routed seat reports its outcome once, against its own decision: an answering seat is
    /// `pass`, a seat that failed is `error`, and no decision is reported twice.
    #[tokio::test]
    async fn each_routed_seat_reports_its_outcome_once_to_the_router() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("the first answer".into()),
            Scripted::Fails("the model refused".into()),
            Scripted::Answers("the third answer".into()),
        ]
        .into();
        *runner.critique.lock().unwrap() = [
            Scripted::Answers(critique_json(&["A", "B"])),
            Scripted::Answers(critique_json(&["A", "B"])),
        ]
        .into();

        // One stub for both endpoints: `/v1/route` mints a fresh decision per seat and echoes the
        // seat's own `model_ref`, and every outcome lands on `received`.
        let asked = std::sync::Arc::new(AtomicUsize::new(0));
        let (sent, mut received) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
        let counter = asked.clone();
        let app = axum::Router::new()
            .route(
                "/v1/route",
                axum::routing::post(move |axum::Json(request): axum::Json<serde_json::Value>| {
                    let n = counter.fetch_add(1, Ordering::SeqCst);
                    async move {
                        axum::Json(serde_json::json!({
                            "decision_id": format!("rt_{n}"),
                            "runner": "claude",
                            "model": request["models"][0],
                            "effort": "low",
                        }))
                    }
                }),
            )
            .route(
                "/v1/route/{id}/outcome",
                axum::routing::post(
                    move |axum::extract::Path(id): axum::extract::Path<String>,
                          axum::Json(body): axum::Json<serde_json::Value>| {
                        let sent = sent.clone();
                        async move {
                            let status = body["status"].as_str().unwrap_or_default().to_owned();
                            let _ = sent.send((id, status));
                            axum::http::StatusCode::OK
                        }
                    },
                ),
            );
        let url = crate::router_client::test_support::serve(app).await;
        let inner: std::sync::Arc<dyn crate::runner::CommandRunner> = runner.clone();
        let router = std::sync::Arc::new(crate::route_advice::Router::new(
            crate::route_advice::RouterConfig {
                mode: crate::route_advice::Mode::Off,
                url,
                surfaces: [("council", crate::route_advice::Mode::Shadow)]
                    .into_iter()
                    .collect(),
                ..crate::route_advice::RouterConfig::off()
            },
            crate::route_advice::Available {
                kind: crate::route_advice::RunnerKind::Claude,
                runner: inner.clone(),
                default_model: "claude-sonnet-5".into(),
                models: vec!["claude-*".into()],
            },
            Vec::new(),
        ));
        let mut state = council_state(runner.clone(), Some(roster(3))).await;
        state.runner = std::sync::Arc::new(crate::route_advice::RoutedRunner { inner, router });

        let id = start(&state, "why?", None).await.unwrap();
        assert_eq!(settled(&state, &id).await.status, STATUS_DONE);

        let mut reports = Vec::new();
        while let Ok(Some(report)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), received.recv()).await
        {
            reports.push(report);
        }
        let routed = asked.load(Ordering::SeqCst);
        assert!(routed > 0, "no seat was routed");
        assert_eq!(
            reports.len(),
            routed,
            "one report per decision: {reports:?}"
        );
        let ids: std::collections::BTreeSet<&str> =
            reports.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids.len(), reports.len(), "a decision was reported twice");
        let errors = reports
            .iter()
            .filter(|(_, status)| status == "error")
            .count();
        assert_eq!(errors, 1, "{reports:?}");
        assert!(
            reports
                .iter()
                .all(|(_, status)| status == "pass" || status == "error"),
            "{reports:?}"
        );
        // Every launch of every phase — answers, critiques and the chairman — went through the one
        // routed seat-launch helper: the router was asked once per request the runner saw.
        let launched = runner.seen.lock().unwrap().len();
        assert_eq!(routed, launched, "a phase launched around the router");
        assert!(
            prompts(&runner)
                .iter()
                .any(|prompt| prompt.contains(prompts::CRITIQUE_MARKER)),
            "the critique phase ran, so its launches are among the routed ones"
        );
    }

    /// A seat that failed is information about the model, not a reason to abandon the question.
    #[tokio::test]
    async fn a_failed_seat_is_recorded_and_the_council_still_synthesizes() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("the first answer".into()),
            Scripted::Fails("the model refused".into()),
            Scripted::Answers("the third answer".into()),
        ]
        .into();
        *runner.critique.lock().unwrap() = [
            Scripted::Answers(critique_json(&["A", "B"])),
            Scripted::Answers(critique_json(&["A", "B"])),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start(&state, "why?", None).await.unwrap();
        let row = settled(&state, &id).await;

        assert_eq!(row.status, STATUS_DONE);
        let steps = steps_for(&state, &id).await;
        assert_eq!(
            step_at(&steps, 0, 0, store::PHASE_ANSWER).unwrap().status,
            SEAT_OK
        );
        let failed = step_at(&steps, 1, 0, store::PHASE_ANSWER).unwrap();
        assert_eq!(failed.status, SEAT_ERROR);
        assert_eq!(failed.error.as_deref(), Some("the model refused"));
        assert_eq!(
            step_at(&steps, 2, 0, store::PHASE_ANSWER).unwrap().status,
            SEAT_OK
        );

        // Two valid answers, so the critique ran — and only for the seats that had one to be
        // judged against, which is the same set.
        assert_eq!(
            step_at(&steps, 0, 1, store::PHASE_CRITIQUE).unwrap().status,
            SEAT_OK
        );
        assert_eq!(
            step_at(&steps, 2, 1, store::PHASE_CRITIQUE).unwrap().status,
            SEAT_OK
        );
        // The failed seat is not in the shuffle, so it was never shown anything and never asked.
        assert!(step_at(&steps, 1, 1, store::PHASE_CRITIQUE).is_none());

        let anon: BTreeMap<String, usize> =
            serde_json::from_str(row.anon_map.as_deref().unwrap()).unwrap();
        assert_eq!(anon.len(), 2);
        assert!(!anon.values().any(|seat| *seat == 1));
        assert!(row.chairman_run_id.is_some());
    }

    /// The bug this was filed over, reproduced at a council seat: a seat that answers once and then
    /// dies at its turn ceiling is a non-zero exit, same as `Fails` above — and that branch's own
    /// `UPDATE` bound `cost_usd` but not `num_turns` or the token columns, silently dropping numbers
    /// `RunOutcome` actually carried. `RunDetail.tsx` reads this seat's row exactly as it reads any
    /// other run's, so what is missing here is the same "none recorded" the owner reported.
    #[tokio::test]
    async fn a_seat_that_dies_at_the_ceiling_still_reports_the_turn_before_it() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("the first answer".into()),
            Scripted::DiesAtTheCeiling,
            Scripted::Answers("the third answer".into()),
        ]
        .into();
        *runner.critique.lock().unwrap() = [
            Scripted::Answers(critique_json(&["A", "B"])),
            Scripted::Answers(critique_json(&["A", "B"])),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start(&state, "why?", None).await.unwrap();
        let row = settled(&state, &id).await;

        assert_eq!(row.status, STATUS_DONE);
        let steps = steps_for(&state, &id).await;
        let died = step_at(&steps, 1, 0, store::PHASE_ANSWER).unwrap();
        assert_eq!(died.status, SEAT_ERROR);
        let run_id = died
            .run_id
            .expect("a seat that launched has a run row, whatever it ended with");

        /// `exit_code, cost_usd, num_turns, input_tokens, output_tokens, cache_creation_tokens`.
        type SeatNumbers = (
            Option<i32>,
            Option<f64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        );
        let (exit_code, cost_usd, num_turns, input_tokens, output_tokens, cache_creation_tokens): SeatNumbers = sqlx::query_as(
            "SELECT exit_code, cost_usd, num_turns, input_tokens, output_tokens, cache_creation_tokens
             FROM runs WHERE id = ?",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();

        assert_eq!(exit_code, Some(crate::runner::TURN_CEILING_EXIT_CODE));
        assert_eq!(
            cost_usd,
            Some(0.05),
            "the completed turn's cost must not be read back NULL"
        );
        assert_eq!(
            num_turns,
            Some(1),
            "the completed turn's count must not be read back NULL"
        );
        assert_eq!(input_tokens, Some(11));
        assert_eq!(output_tokens, Some(22));
        assert_eq!(
            cache_creation_tokens,
            Some(7),
            "the cache the seat built is spend too, and must not be read back NULL"
        );
    }

    /// One answer has nothing to be ranked against, and the phase would ask a seat to order an
    /// empty set. Skipping is RECORDED — `skipped` and `pending` mean different things.
    #[tokio::test]
    async fn a_single_valid_response_skips_stage_two() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("the only answer".into()),
            Scripted::Fails("no".into()),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(2))).await;

        let id = start(&state, "why?", None).await.unwrap();
        let row = settled(&state, &id).await;

        assert_eq!(row.status, STATUS_DONE);
        // No critique round ran, and the record says so rather than claiming one.
        assert_eq!(row.rounds_run, 0);
        for step in steps_for(&state, &id).await {
            if step.phase == store::PHASE_CRITIQUE {
                assert_eq!(step.status, SEAT_SKIPPED, "{step:?}");
            }
        }
        assert!(
            runner
                .seen
                .lock()
                .unwrap()
                .iter()
                .all(|request| !request.prompt.contains(prompts::CRITIQUE_MARKER))
        );
    }

    /// The chairman still runs, and is told the truth: nobody answered. That sentence is the
    /// council's whole product in this case, and it is worth more than a blank record.
    #[tokio::test]
    async fn zero_valid_responses_still_reaches_the_chairman() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Fails("no".into()),
            Scripted::WillNotLaunch("the binary is missing".into()),
        ]
        .into();
        *runner.chairman.lock().unwrap() =
            [Scripted::Answers(synthesis_reply("nobody answered"))].into();
        let state = council_state(runner.clone(), Some(roster(2))).await;

        let id = start(&state, "why?", None).await.unwrap();
        let row = settled(&state, &id).await;

        assert_eq!(row.status, STATUS_DONE);
        let chairman_prompt = runner
            .seen
            .lock()
            .unwrap()
            .iter()
            .find(|request| request.prompt.contains(prompts::CHAIRMAN_MARKER))
            .map(|request| request.prompt.clone())
            .expect("the chairman ran");
        assert!(chairman_prompt.contains("No seat produced a valid answer."));
    }

    /// A seat that ran out of clock and a seat that refused are different facts about a model, and
    /// a record that spelled them the same way would lose the more useful of the two.
    #[tokio::test]
    async fn a_timed_out_seat_is_distinct_from_a_failed_one() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() =
            [Scripted::Hangs, Scripted::Fails("the model refused".into())].into();
        let state = council_state(runner.clone(), Some(roster(2))).await;

        let id = start(&state, "why?", None).await.unwrap();
        settled(&state, &id).await;

        let steps = steps_for(&state, &id).await;
        assert_eq!(
            step_at(&steps, 0, 0, store::PHASE_ANSWER).unwrap().status,
            SEAT_TIMEOUT
        );
        assert_eq!(
            step_at(&steps, 1, 0, store::PHASE_ANSWER).unwrap().status,
            SEAT_ERROR
        );
        // And the run rows keep the same distinction, so anything reading `runs` sees it too.
        let statuses: Vec<String> =
            sqlx::query_scalar("SELECT status FROM runs WHERE mode = ? ORDER BY id")
                .bind(COUNCIL_MODE)
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(statuses[0], "timed_out");
        assert_eq!(statuses[1], "failed");
    }

    /// Cancelling settles the record FIRST and then kills the seats, which is the order that stops
    /// the driver from reading a council still marked `running` and starting phase 2 over the
    /// corpses of phase 1.
    #[tokio::test]
    async fn cancelling_terminates_the_seats_and_removes_the_mcp_config() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [Scripted::Hangs, Scripted::Hangs].into();
        // A clock long enough that only the cancel can end this.
        let mut config = roster(2);
        config.timeout_seconds = 600;
        let state = council_state(runner.clone(), Some(config)).await;

        let id = start(&state, "why?", None).await.unwrap();
        let config_path = mcp_config_path(&id);
        assert!(config_path.exists(), "the seats were given an MCP config");

        // Wait for both seats to be in flight, so the cancel has something to terminate.
        for _ in 0..300 {
            if state.run_handles.lock().unwrap().len() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(state.run_handles.lock().unwrap().len(), 2);

        assert!(cancel(&state, &id).await.unwrap());
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_CANCELLED);
        // `cancelled`, not `error`. Somebody stopped this; nothing broke.
        assert_eq!(row.error, None);
        // The critique never started: the council is still where the answers left it.
        assert_eq!(row.current_phase, store::PHASE_ANSWER);
        assert!(
            prompts(&runner)
                .iter()
                .all(|prompt| !prompt.contains(prompts::CRITIQUE_MARKER))
        );

        for _ in 0..300 {
            if !config_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !config_path.exists(),
            "the MCP config must go on every exit, cancellation included"
        );

        let statuses: Vec<String> = sqlx::query_scalar("SELECT status FROM runs WHERE mode = ?")
            .bind(COUNCIL_MODE)
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert!(
            statuses.iter().all(|status| status == "cancelled"),
            "{statuses:?}"
        );
        assert!(state.run_handles.lock().unwrap().is_empty());
    }

    /// Every prompt the council sent, in order.
    fn prompts(runner: &ScriptedRunner) -> Vec<String> {
        runner
            .seen
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.prompt.clone())
            .collect()
    }

    fn is_synthesis(prompt: &str) -> bool {
        prompt.contains(prompts::CHAIRMAN_MARKER)
    }

    #[tokio::test]
    async fn a_cloud_seat_is_told_what_the_house_knows_and_leaves_no_trace() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("the first answer".into()),
            Scripted::Answers("the second answer".into()),
        ]
        .into();
        *runner.critique.lock().unwrap() = [
            Scripted::Answers(critique_json(&["A"])),
            Scripted::Answers(critique_json(&["A"])),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(2))).await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', 'machine', NULL, 'owner', 'memory',
                     'zanzibar house rule', 'body', 'active',
                     '2026-08-19T00:00:00+00:00')",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let id = start(&state, "zanzibar?", None).await.unwrap();
        let row = settled(&state, &id).await;

        assert_eq!(row.status, STATUS_DONE);
        assert!(
            prompts(&runner)
                .iter()
                .all(|prompt| prompt.contains("zanzibar house rule"))
        );
        let traces: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_knowledge")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(traces, 0);
    }

    /// One `runs` row per seat, whichever machine answers it. The row is what carries cost,
    /// cancellation and orphan reconciliation, and a seat that skipped it would need all three
    /// written again.
    #[tokio::test]
    async fn a_mixed_roster_lands_one_run_per_seat() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [Scripted::Answers("from the cloud".into())].into();
        let config = CouncilConfig {
            timeout_seconds: 1,
            rounds: crate::config::DEFAULT_COUNCIL_ROUNDS,
            consumers: crate::config::CouncilConsumers::default(),
            chairman: spec(SeatKind::Cloud, "the-chairman"),
            members: vec![
                spec(SeatKind::Cloud, "a-cloud-model"),
                // A model no Ollama has. Whether one is listening or not, this seat ends quickly
                // and lands its row — which is the only thing being asserted.
                spec(SeatKind::Local, "a-model-that-does-not-exist"),
            ],
        };
        let state = council_state(runner.clone(), Some(config)).await;

        let id = start(&state, "why?", None).await.unwrap();
        settled(&state, &id).await;

        let seats = get_seat_rows(&state.pool, &id).await.unwrap();
        assert_eq!(seats.len(), 2);
        assert_eq!(seats[0].kind, "cloud");
        assert_eq!(seats[1].kind, "local");
        let steps = steps_for(&state, &id).await;
        assert!(
            step_at(&steps, 0, 0, store::PHASE_ANSWER)
                .unwrap()
                .run_id
                .is_some()
        );
        assert!(
            step_at(&steps, 1, 0, store::PHASE_ANSWER)
                .unwrap()
                .run_id
                .is_some(),
            "a local seat is a run like any other"
        );

        // Exactly one cloud invocation for the member, plus the chairman's. The local seat never
        // reaches the CLI runner.
        assert_eq!(
            runner
                .seen
                .lock()
                .unwrap()
                .iter()
                .filter(|request| !is_synthesis(&request.prompt))
                .count(),
            1
        );
    }

    /// The three properties of HOW a seat is launched, which are three safety decisions rather than
    /// details of the request.
    #[tokio::test]
    async fn a_seat_is_launched_scoped_and_only_phase_one_has_tools() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("first".into()),
            Scripted::Answers("second".into()),
        ]
        .into();
        *runner.critique.lock().unwrap() = [
            Scripted::Answers(critique_json(&["A"])),
            Scripted::Answers(critique_json(&["A"])),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(2))).await;

        let id = start(&state, "why?", None).await.unwrap();
        settled(&state, &id).await;

        let seen = runner.seen.lock().unwrap();
        for request in seen.iter() {
            // Never the control token. `auth::COUNCIL_ROUTES` is what this key reaches, and it is
            // the one barrier that holds without the cooperative hook firing.
            assert_eq!(request.token.as_deref(), Some("council-key"));
            assert_ne!(request.token.as_deref(), Some("control-token"));
        }

        let ranking_or_synthesis = seen.iter().filter(|request| {
            request.prompt.contains(prompts::CRITIQUE_MARKER)
                || request.prompt.contains(prompts::CHAIRMAN_MARKER)
        });
        for request in ranking_or_synthesis {
            // No tools after phase 1. A seat that could go and find targeted evidence AFTER seeing
            // its peers' answers would turn the ranking into a measure of who had time left.
            assert_eq!(request.tool_policy, crate::runner::ToolPolicy::None);
            assert_eq!(
                request.mcp_config, None,
                "a policy of None beside a config file would advertise a server the CLI may not reach"
            );
        }

        let answering = seen
            .iter()
            .find(|request| request.prompt == "why?")
            .expect("phase 1 asks the question as written");
        assert_eq!(answering.tool_policy, crate::runner::ToolPolicy::McpOnly);
        assert!(answering.mcp_config.is_some());
    }

    /// The regression guard for the defect the FIRST real council died of.
    ///
    /// A phase-2 prompt carries every peer's whole answer, and `cli_args` puts the prompt on the
    /// command line unless the request is steerable. Windows caps a command line at 32 767
    /// characters, so two cloud seats answering at 62 KB and 57 KB took every later phase over it
    /// and each one failed with os error 206 — `ERROR_FILENAME_EXCED_RANGE`, which names a filename
    /// and means an argv.
    ///
    /// This asserts the FLAG and not the length, because the length is not the property: a prompt
    /// on stdin has no ceiling to be under, and a test that merely checked a size would pass right
    /// up until somebody asked a longer question. `messages` stays `None` — nothing steers a seat,
    /// and the writer task closing stdin after the opening turn IS the one-turn run.
    #[tokio::test]
    async fn no_seat_carries_its_prompt_on_the_command_line() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        // Long enough that the real thing would have been refused by the operating system.
        let long = "x".repeat(40_000);
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers(long.clone()),
            Scripted::Answers(long.clone()),
        ]
        .into();
        *runner.critique.lock().unwrap() = [
            Scripted::Answers(critique_json(&["A"])),
            Scripted::Answers(critique_json(&["A"])),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(2))).await;

        let id = start(&state, "why?", None).await.unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_DONE);

        let seen = runner.seen.lock().unwrap();
        assert!(
            seen.len() >= 3,
            "phase 1, phase 2 and the chairman all launched"
        );
        for request in seen.iter() {
            assert!(
                request.steerable,
                "a seat whose prompt goes on the command line dies at 32 767 characters"
            );
        }
    }

    /// Refused before the first seat, never between phases. A council stopped after phase 1 has
    /// paid for every answer and produced no synthesis, which is the worst point to stop at.
    #[tokio::test]
    async fn an_exhausted_budget_refuses_before_spending() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        let state = council_state(runner.clone(), Some(roster(2))).await;
        crate::budget::set_budget_config(
            &state.pool,
            &crate::budget::BudgetConfig {
                limit_usd: Some(0.01),
                period: crate::budget::BudgetPeriod::Monthly,
                hourly_limit_usd: None,
                per_run_reserve_usd: 0.5,
                time_cost_per_hour_usd: 3.0,
            },
        )
        .await
        .unwrap();

        let error = start(&state, "why?", None).await.unwrap_err();
        match &error {
            StartError::BudgetExhausted(reason) => {
                // The refusal names the limit and the spend rather than saying a bare no.
                assert!(reason.contains("0.01"), "{reason}");
                assert!(reason.contains("limit"), "{reason}");
            }
            other => panic!("expected a budget refusal, got {other:?}"),
        }

        // And nothing was written or spawned: no record, no runs, no MCP config left behind.
        assert!(
            list_council_rows(&state.pool, 10, 0)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(runner.seen.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn quota_brake_refusal_names_the_quota() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        let mut state = council_state(runner, Some(roster(2))).await;
        let now = chrono::Utc::now();
        let address = crate::quota::test_support::stub_sidecar(
            crate::quota::test_support::live_answer("claude", "5h", 1.0, None, now),
        )
        .await;
        crate::quota::test_support::arm(&state.pool, true, 85, 90).await;
        state.quota = std::sync::Arc::new(crate::quota::QuotaRuntime::new(
            crate::quota_client::QuotaClient::new(&address, "bearer".into()),
            "claude".into(),
        ));

        let error = start(&state, "why?", None).await.unwrap_err();
        match error {
            StartError::QuotaExhausted(reason) => assert!(reason.contains("quota"), "{reason}"),
            other => panic!("expected a quota refusal, got {other:?}"),
        }
    }

    /// A council left `running` by a crash would sit in the list for ever at a phase nothing will
    /// advance, because the driver that would have advanced it died with the process.
    #[tokio::test]
    async fn a_council_left_running_is_reconciled_at_startup() {
        let pool = test_pool().await;
        insert_council(
            &pool,
            "c1",
            "why?",
            &seat(SeatKind::Cloud, "m"),
            &[seat(SeatKind::Cloud, "m")],
            1,
        )
        .await
        .unwrap();
        insert_council(
            &pool,
            "c2",
            "why?",
            &seat(SeatKind::Cloud, "m"),
            &[seat(SeatKind::Cloud, "m")],
            1,
        )
        .await
        .unwrap();
        finish(&pool, "c2", STATUS_DONE, None).await.unwrap();

        assert_eq!(reconcile(&pool).await.unwrap(), 1);

        let interrupted = get_council_row(&pool, "c1").await.unwrap().unwrap();
        assert_eq!(interrupted.status, STATUS_ERROR);
        // With a reason, because "error" with no explanation is indistinguishable from a council
        // whose chairman refused.
        assert!(interrupted.error.is_some());
        // A council that had already finished is left exactly as it was.
        assert_eq!(
            get_council_row(&pool, "c2").await.unwrap().unwrap().status,
            STATUS_DONE
        );
        // Idempotent: a second startup finds nothing to do.
        assert_eq!(reconcile(&pool).await.unwrap(), 0);

        pool.close().await;
    }

    // ── The surface ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn post_creates_and_returns_the_id() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [Scripted::Answers("an answer".into())].into();
        let state = council_state(runner, Some(roster(1))).await;

        let (status, body) = post_council(
            axum::extract::State(state.clone()),
            axum::Json(CreateCouncilRequest {
                question: "  why?  ".to_string(),
                roster: None,
            }),
        )
        .await
        .unwrap();

        // 202: the record exists and the deliberation has not happened yet.
        assert_eq!(status, axum::http::StatusCode::ACCEPTED);
        let row = get_council_row(&state.pool, &body.id)
            .await
            .unwrap()
            .unwrap();
        // Trimmed, and otherwise exactly as written: a seat asked a paraphrase answers the
        // paraphrase.
        assert_eq!(row.question, "why?");

        assert!(matches!(
            post_council(
                axum::extract::State(state.clone()),
                axum::Json(CreateCouncilRequest {
                    question: "   ".to_string(),
                    roster: None,
                }),
            )
            .await,
            Err((axum::http::StatusCode::BAD_REQUEST, _))
        ));

        settled(&state, &body.id).await;
    }

    /// A record read halfway through is the ordinary case, not a corner one — the shell polls this
    /// route while the council is still deliberating.
    #[tokio::test]
    async fn get_shows_stage_one_before_stage_two_exists() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() =
            [Scripted::Answers("the first".into()), Scripted::Hangs].into();
        let mut config = roster(2);
        config.timeout_seconds = 600;
        let state = council_state(runner, Some(config)).await;

        let id = start(&state, "why?", None).await.unwrap();
        // Wait for the first seat to land its answer while the second still hangs.
        for _ in 0..300 {
            let steps = steps_for(&state, &id).await;
            if step_at(&steps, 0, 0, store::PHASE_ANSWER).is_some_and(|step| step.status == SEAT_OK)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let view = get_council(
            axum::extract::State(state.clone()),
            axum::extract::Path(id.clone()),
        )
        .await
        .unwrap();
        assert_eq!(view.status, STATUS_RUNNING);
        assert_eq!(view.current_round, 0);
        assert_eq!(view.current_phase, store::PHASE_ANSWER);
        // The answer comes back as TEXT, read out of the run that produced it. Nothing in this
        // response tells the client a `runs` table exists.
        assert_eq!(
            step_answer(&view.seats[0], 0, store::PHASE_ANSWER).as_deref(),
            Some("the first")
        );
        assert_eq!(step_answer(&view.seats[1], 0, store::PHASE_ANSWER), None);
        assert!(view.leaderboard.is_empty());
        assert!(view.leaderboard_by_round.is_empty());
        assert_eq!(view.agreement, None);
        assert_eq!(view.synthesis, None);

        cancel(&state, &id).await.unwrap();

        assert!(matches!(
            get_council(
                axum::extract::State(state.clone()),
                axum::extract::Path("no-such-council".to_string()),
            )
            .await,
            Err((axum::http::StatusCode::NOT_FOUND, _))
        ));
    }

    /// An override is for ONE question. A roster somebody wanted once must not become the roster
    /// every later council inherits.
    #[tokio::test]
    async fn a_roster_override_does_not_write_configuration() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [Scripted::Answers("an answer".into())].into();
        let state = council_state(runner, Some(roster(3))).await;

        let id = start(
            &state,
            "why?",
            Some(RosterOverride {
                chairman: spec(SeatKind::Cloud, "a-different-chairman"),
                members: vec![spec(SeatKind::Cloud, "just-this-one")],
            }),
        )
        .await
        .unwrap();
        let row = settled(&state, &id).await;

        assert_eq!(row.chairman_ref, "a-different-chairman");
        let seats = get_seat_rows(&state.pool, &id).await.unwrap();
        assert_eq!(seats.len(), 1);
        assert_eq!(seats[0].model_ref, "just-this-one");

        // The configured roster is untouched, so the next council convenes the three it names.
        assert_eq!(state.council.config().unwrap().members.len(), 3);
        assert_eq!(
            state
                .council
                .config()
                .unwrap()
                .chairman
                .model_ref
                .as_deref(),
            Some("the-chairman")
        );

        // And the daemon's own limits still apply to a roster that never touched the file.
        assert!(matches!(
            start(
                &state,
                "why?",
                Some(RosterOverride {
                    chairman: spec(SeatKind::Cloud, "c"),
                    members: Vec::new(),
                }),
            )
            .await,
            Err(StartError::Invalid(_))
        ));
        assert!(matches!(
            start(
                &state,
                "why?",
                Some(RosterOverride {
                    chairman: spec(SeatKind::Cloud, "c"),
                    members: (0..=crate::config::MAX_COUNCIL_SEATS)
                        .map(|_| spec(SeatKind::Cloud, "m"))
                        .collect(),
                }),
            )
            .await,
            Err(StartError::Invalid(_))
        ));
        // A local seat this daemon cannot answer with is refused rather than re-routed to the
        // cloud, which is the whole point of writing `local` in the first place.
        assert_eq!(
            start(
                &state,
                "why?",
                Some(RosterOverride {
                    chairman: spec(SeatKind::Cloud, "c"),
                    members: vec![spec(SeatKind::Local, "qwen3.5:4b")],
                }),
            )
            .await
            .unwrap_err(),
            StartError::NoLocalModel
        );
    }

    // -----------------------------------------------------------------------------------------
    // Seats filled from the house catalogue
    // -----------------------------------------------------------------------------------------

    /// The two forms sit in one roster, and neither is on the way out.
    ///
    /// The roster lives at [`CONFIG_DISPLAY_PATH`], outside any checkout — it is one person's
    /// configuration on one machine, not a fact of this repository, and nothing here can migrate
    /// it. A form retired here would not error on the machines still using it; it would give them
    /// a council that silently stops existing at the next daemon start. So this asserts
    /// coexistence, not migration.
    #[tokio::test]
    async fn a_roster_may_name_an_agent_in_one_seat_and_a_model_in_the_next() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("from the agent".into()),
            Scripted::Answers("from the model".into()),
        ]
        .into();
        let state = council_state(
            runner,
            Some(CouncilConfig {
                timeout_seconds: 1,
                rounds: crate::config::DEFAULT_COUNCIL_ROUNDS,
                consumers: crate::config::CouncilConsumers::default(),
                chairman: spec(SeatKind::Cloud, "the-chairman"),
                members: vec![agent_spec("cetico"), spec(SeatKind::Cloud, "a-plain-model")],
            }),
        )
        .await;
        let mut asked = agent_request("Cetico");
        asked.model = Some("claude-sonnet-5".to_string());
        catalogue(&state.pool, asked).await;

        let id = start(&state, "why?", None).await.unwrap();
        settled(&state, &id).await;

        let seats = get_seat_rows(&state.pool, &id).await.unwrap();
        assert_eq!(seats[0].agent_id.as_deref(), Some("cetico"));
        assert_eq!(seats[0].model_ref, "claude-sonnet-5");
        assert_eq!(
            seats[1].agent_id, None,
            "a seat declared as a model belongs to nobody"
        );
        assert_eq!(seats[1].model_ref, "a-plain-model");

        let row = get_council_row(&state.pool, &id).await.unwrap().unwrap();
        assert_eq!(row.chairman_agent_id, None);
    }

    /// Two sources for one fact is where they eventually disagree, and the disagreement would be
    /// silent — whichever half lost would still be sitting in the file, read by whoever edits it
    /// next. So a seat naming both is refused rather than resolved in favour of one.
    #[tokio::test]
    async fn a_seat_naming_both_an_agent_and_a_model_is_refused_rather_than_chosen_between() {
        let state = council_state(
            std::sync::Arc::new(ScriptedRunner::default()),
            Some(roster(1)),
        )
        .await;
        catalogue(&state.pool, agent_request("Cetico")).await;

        let refusal = start(
            &state,
            "why?",
            Some(RosterOverride {
                chairman: spec(SeatKind::Cloud, "c"),
                members: vec![SeatSpec {
                    kind: Some(SeatKind::Cloud),
                    model_ref: Some("a-model".to_string()),
                    agent: Some("cetico".to_string()),
                }],
            }),
        )
        .await;
        assert!(
            matches!(&refusal, Err(StartError::Invalid(why)) if why.contains("both an agent and a model")),
            "got {refusal:?}"
        );
    }

    /// The structural decision of this slice, asserted from both ends.
    ///
    /// `load_council_config` is pure and runs at startup, so it checks the FORM and accepts a roster
    /// naming an agent nobody has created. `start` checks the REFERENCE against a catalogue the
    /// owner edits with the daemon running. Validating at load would be a promise about a different
    /// afternoon; the cost is that the refusal arrives at the question, and it costs nothing else —
    /// no row, no key, no spend.
    #[tokio::test]
    async fn a_roster_naming_a_missing_agent_loads_and_is_refused_at_the_question() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("council.yaml");
        std::fs::write(
            &path,
            "chairman: { kind: cloud, ref: the-chairman }\nmembers:\n  - { agent: cetico }\n",
        )
        .unwrap();
        let config = crate::config::load_council_config(&path, false)
            .expect("the form is right; only the reference is missing");

        let runner = std::sync::Arc::new(ScriptedRunner::default());
        let state = council_state(runner, Some(config)).await;

        let refusal = start(&state, "why?", None).await;
        assert!(
            matches!(&refusal, Err(StartError::Invalid(why))
                if why.contains("cetico") && why.contains("catalogue")),
            "got {refusal:?}"
        );
        let councils: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM council_runs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            councils, 0,
            "a refusal before the budget check writes nothing"
        );

        // And the same roster works the moment the catalogue does.
        catalogue(&state.pool, agent_request("Cetico")).await;
        let id = start(&state, "why?", None).await.unwrap();
        settled(&state, &id).await;
    }

    /// A seat's row says WHICH MODEL ANSWERED, and keeps saying it after the agent is edited.
    ///
    /// The same argument `0065_council.sql` makes for copying the roster onto the row instead of
    /// reading it back from configuration. A council from March whose seat pointed at `cetico` has
    /// to keep reporting what actually spoke, whatever `cetico` has been reconfigured to since.
    #[tokio::test]
    async fn a_seat_records_the_model_that_answered_and_not_only_the_agent() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [Scripted::Answers("an answer".into())].into();
        let state = council_state(
            runner,
            Some(CouncilConfig {
                timeout_seconds: 1,
                rounds: crate::config::DEFAULT_COUNCIL_ROUNDS,
                consumers: crate::config::CouncilConsumers::default(),
                chairman: spec(SeatKind::Cloud, "the-chairman"),
                members: vec![agent_spec("cetico")],
            }),
        )
        .await;
        let mut asked = agent_request("Cetico");
        asked.model = Some("claude-sonnet-5".to_string());
        catalogue(&state.pool, asked).await;

        let id = start(&state, "why?", None).await.unwrap();
        settled(&state, &id).await;

        let mut moved = agent_request("Cetico");
        moved.model = Some("something-else-entirely".to_string());
        crate::agent::update(&state.pool, "cetico", moved)
            .await
            .unwrap();

        let seats = get_seat_rows(&state.pool, &id).await.unwrap();
        assert_eq!(
            seats[0].model_ref, "claude-sonnet-5",
            "history is what answered, not what the agent points at today"
        );
        assert_eq!(seats[0].agent_id.as_deref(), Some("cetico"));
    }

    /// An agent may leave its model unset and let the CLI choose — legitimate in a team, which keeps
    /// no record of what ran. A seat keeps exactly that record, in a `NOT NULL` column, so the seat
    /// is refused before anything is spent rather than written down blank.
    #[tokio::test]
    async fn an_agent_that_names_no_model_cannot_take_a_seat() {
        let state = council_state(
            std::sync::Arc::new(ScriptedRunner::default()),
            Some(roster(1)),
        )
        .await;
        let mut asked = agent_request("Cetico");
        asked.model = None;
        catalogue(&state.pool, asked).await;

        let refusal = start(
            &state,
            "why?",
            Some(RosterOverride {
                chairman: spec(SeatKind::Cloud, "c"),
                members: vec![agent_spec("cetico")],
            }),
        )
        .await;
        assert!(
            matches!(&refusal, Err(StartError::Invalid(why)) if why.contains("names no model")),
            "got {refusal:?}"
        );
    }

    /// `tool_policy: none` is the first seat with no tools, and it only ever NARROWS.
    ///
    /// `mcp_only` is exactly what every seat gets today and `unrestricted` never reaches the
    /// catalogue, so nothing an agent can declare hands a seat something a seat does not already
    /// have. The config file goes with the policy: a `None` policy beside an MCP config would
    /// advertise a server the CLI is forbidden to reach.
    #[tokio::test]
    async fn an_agent_with_no_tool_policy_takes_a_seat_with_no_tools() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [Scripted::Answers("an answer".into())].into();
        let state = council_state(
            runner.clone(),
            Some(CouncilConfig {
                timeout_seconds: 1,
                rounds: crate::config::DEFAULT_COUNCIL_ROUNDS,
                consumers: crate::config::CouncilConsumers::default(),
                chairman: spec(SeatKind::Cloud, "the-chairman"),
                members: vec![agent_spec("cetico")],
            }),
        )
        .await;
        let mut asked = agent_request("Cetico");
        asked.tool_policy = "none".to_string();
        catalogue(&state.pool, asked).await;

        let id = start(&state, "why?", None).await.unwrap();
        settled(&state, &id).await;

        let seen = runner.seen.lock().unwrap();
        assert!(
            matches!(seen[0].tool_policy, crate::runner::ToolPolicy::None),
            "phase 1 is the phase that HAS tools, and this agent asked for none"
        );
        assert!(seen[0].mcp_config.is_none());
    }

    /// Phase 2 is a blind vote, and giving seats identities put three new strings at risk of
    /// leaking into it: an agent's name, its speciality, and its prompt.
    ///
    /// None of the three travels. A seat's own instructions go in front of its own prompt — it knows
    /// who IT is — and what it is shown of its peers is their ANSWERS under shuffled labels, exactly
    /// as before. This walks every request the whole council made and asserts each one carries at
    /// most its own persona.
    ///
    /// What this does NOT promise, said rather than pretended away: a strong persona writes
    /// recognisably. That is already true between different models today. The system does not print
    /// the name.
    #[tokio::test]
    async fn no_seat_is_ever_shown_another_seats_name_speciality_or_prompt() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("the first answer".into()),
            Scripted::Answers("the second answer".into()),
        ]
        .into();
        *runner.critique.lock().unwrap() = [
            Scripted::Answers(critique_json(&["A", "B"])),
            Scripted::Answers(critique_json(&["B", "A"])),
        ]
        .into();
        let state = council_state(
            runner.clone(),
            Some(CouncilConfig {
                timeout_seconds: 1,
                rounds: crate::config::DEFAULT_COUNCIL_ROUNDS,
                consumers: crate::config::CouncilConsumers::default(),
                chairman: spec(SeatKind::Cloud, "the-chairman"),
                members: vec![agent_spec("cetico"), agent_spec("economista")],
            }),
        )
        .await;
        for (name, speciality, prompt) in [
            ("Cetico", "doubts the premise", "Argue the case against."),
            ("Economista", "counts the money", "Argue from the cost."),
        ] {
            catalogue(
                &state.pool,
                crate::agent::AgentRequest {
                    name: name.to_string(),
                    speciality: speciality.to_string(),
                    prompt: prompt.to_string(),
                    engine: "claude".to_string(),
                    model: Some("claude-opus-5".to_string()),
                    tool_policy: "mcp_only".to_string(),
                },
            )
            .await;
        }

        let id = start(&state, "why?", None).await.unwrap();
        settled(&state, &id).await;

        let seen = runner.seen.lock().unwrap();
        assert!(seen.len() >= 5, "two answers, two critiques and a chairman");
        for request in seen.iter() {
            // The chairman is the one reader the anonymity ends for, on purpose: it is told who
            // said what (`prompts::chairman_prompt`). Every SEAT prompt is held to the rule.
            let names_allowed = is_synthesis(&request.prompt);
            for forbidden in [
                "Cetico",
                "Economista",
                "doubts the premise",
                "counts the money",
            ] {
                if names_allowed && (forbidden == "Cetico" || forbidden == "Economista") {
                    continue;
                }
                assert!(
                    !request.prompt.contains(forbidden),
                    "`{forbidden}` reached a seat's prompt:\n{}",
                    request.prompt
                );
            }
            // A persona is the seat's own, so at most ONE of the two may appear, and only at the
            // very start where `run_seat` puts it.
            let carried: Vec<&str> = ["Argue the case against.", "Argue from the cost."]
                .into_iter()
                .filter(|persona| request.prompt.contains(persona))
                .collect();
            assert!(
                carried.len() <= 1,
                "a seat was shown a peer's instructions:\n{}",
                request.prompt
            );
            if let Some(persona) = carried.first() {
                assert!(
                    request.prompt.starts_with(persona),
                    "a persona belongs at the front of its own prompt and nowhere else"
                );
            }
        }
    }

    /// The regression that says this slice is not a migration in disguise: a roster written the way
    /// every roster was written yesterday behaves exactly as it did, down to the columns.
    #[tokio::test]
    async fn a_roster_of_plain_models_runs_with_no_agent_anywhere_on_the_record() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [Scripted::Answers("an answer".into())].into();
        let state = council_state(runner, Some(roster(2))).await;

        let id = start(&state, "why?", None).await.unwrap();
        settled(&state, &id).await;

        let row = get_council_row(&state.pool, &id).await.unwrap().unwrap();
        assert_eq!(row.chairman_agent_id, None);
        assert!(
            get_seat_rows(&state.pool, &id)
                .await
                .unwrap()
                .iter()
                .all(|seat| seat.agent_id.is_none())
        );

        let view = get_council(axum::extract::State(state.clone()), axum::extract::Path(id))
            .await
            .unwrap();
        assert!(view.seats.iter().all(|seat| seat.agent_name.is_none()));
        assert_eq!(view.chairman_agent_name, None);
    }

    /// The name a client reads is looked up when the view is built, never copied onto the row: a
    /// name is editable, and a stored copy is a second version of the truth that looks authoritative
    /// because it is older. The corollary is that a deleted agent leaves the id with no name beside
    /// it, which is the honest rendering of what the record says.
    #[tokio::test]
    async fn the_view_names_the_agent_and_survives_it_being_deleted() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [Scripted::Answers("an answer".into())].into();
        let state = council_state(
            runner,
            Some(CouncilConfig {
                timeout_seconds: 1,
                rounds: crate::config::DEFAULT_COUNCIL_ROUNDS,
                consumers: crate::config::CouncilConsumers::default(),
                chairman: spec(SeatKind::Cloud, "the-chairman"),
                members: vec![agent_spec("cetico")],
            }),
        )
        .await;
        catalogue(&state.pool, agent_request("Cetico")).await;

        let id = start(&state, "why?", None).await.unwrap();
        settled(&state, &id).await;

        let view = get_council(
            axum::extract::State(state.clone()),
            axum::extract::Path(id.clone()),
        )
        .await
        .unwrap();
        assert_eq!(view.seats[0].agent_name.as_deref(), Some("Cetico"));

        // A council roster is a file, not a table, so there is no foreign key to stand on and
        // `agent::delete` does not learn to refuse this. The record degrades instead of lying.
        crate::agent::delete(&state.pool, "cetico").await.unwrap();
        let view = get_council(axum::extract::State(state), axum::extract::Path(id))
            .await
            .unwrap();
        assert_eq!(view.seats[0].agent_id.as_deref(), Some("cetico"));
        assert_eq!(view.seats[0].agent_name, None);
    }

    #[tokio::test]
    async fn cancel_leaves_cancelled_and_not_error() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [Scripted::Hangs].into();
        let mut config = roster(1);
        config.timeout_seconds = 600;
        let state = council_state(runner, Some(config)).await;

        let id = start(&state, "why?", None).await.unwrap();
        let first = post_council_cancel(
            axum::extract::State(state.clone()),
            axum::extract::Path(id.clone()),
        )
        .await
        .unwrap();
        assert!(first.cancelled);

        // A second cancel is not an error and says plainly that it changed nothing.
        let second = post_council_cancel(
            axum::extract::State(state.clone()),
            axum::extract::Path(id.clone()),
        )
        .await
        .unwrap();
        assert!(!second.cancelled);

        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_CANCELLED);
        assert_eq!(row.error, None);

        // Convened and cancelled are two lines of one council's story, keyed by the council's id.
        let lines: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT kind, subject FROM feed WHERE kind IN ('council_started', 'council_finished')
             ORDER BY id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        let council = format!("council:{id}");
        assert_eq!(
            lines,
            [
                ("council_started".to_string(), Some(council.clone())),
                ("council_finished".to_string(), Some(council)),
            ]
        );

        assert!(matches!(
            post_council_cancel(
                axum::extract::State(state.clone()),
                axum::extract::Path("no-such-council".to_string()),
            )
            .await,
            Err((axum::http::StatusCode::NOT_FOUND, _))
        ));
    }

    /// The roster is the product's file, so it sits where the workflow library sits.
    ///
    /// This asserts the absence as hard as the presence. `.ai/` is the agent harness's directory
    /// inside one checkout, and a daemon that reads its roster from there has a different council
    /// per working copy and none at all when started from anywhere else — which is what this move
    /// ended. A later refactor that reaches back for a repo-relative path fails here.
    #[test]
    fn the_roster_lives_beside_the_workflow_library_and_not_in_ai() {
        let Some(path) = config_path() else {
            // No home directory on this machine: there is nothing to assert about a path that does
            // not exist, and `main.rs` treats the same `None` as "no council" rather than an error.
            return;
        };
        let text = path.to_string_lossy().replace('\\', "/");
        assert!(text.ends_with(".nucleos/council.yaml"), "{text}");
        assert!(!text.contains("/.ai/"), "{text}");

        // The library made the same choice, and the two must not drift apart into two ideas of
        // where a person's own files live.
        let library = crate::workflows::library_root().unwrap();
        assert_eq!(path.parent(), library.parent());

        // What a refusal prints is the readable form, never the absolute one.
        assert_eq!(CONFIG_DISPLAY_PATH, "~/.nucleos/council.yaml");
        assert!(!StartError::NotConfigured.to_string().contains(".ai/"));
    }

    /// A daemon with no council says so, rather than failing in a way the caller has to guess at.
    #[tokio::test]
    async fn without_configuration_the_routes_say_so() {
        let state = council_state(std::sync::Arc::new(ScriptedRunner::default()), None).await;

        let (status, message) = post_council(
            axum::extract::State(state.clone()),
            axum::Json(CreateCouncilRequest {
                question: "why?".to_string(),
                roster: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert!(message.contains(CONFIG_DISPLAY_PATH), "{message}");

        // The reads still work: a daemon whose council was switched off yesterday still has to be
        // able to show the ones it ran the day before.
        assert!(
            list_councils(
                axum::extract::State(state.clone()),
                axum::extract::Query(ListQuery {
                    limit: None,
                    offset: None,
                }),
            )
            .await
            .unwrap()
            .is_empty()
        );
    }

    /// `LIMIT -1` in SQLite means no limit at all, and the number arrives in a query string.
    #[tokio::test]
    async fn the_list_clamps_what_a_caller_asks_for() {
        let state = council_state(std::sync::Arc::new(ScriptedRunner::default()), None).await;
        for index in 0..3 {
            insert_council(
                &state.pool,
                &format!("c{index}"),
                "why?",
                &seat(SeatKind::Cloud, "m"),
                &[seat(SeatKind::Cloud, "m")],
                1,
            )
            .await
            .unwrap();
        }

        for asked in [Some(-1), Some(0), None, Some(500)] {
            let listed = list_councils(
                axum::extract::State(state.clone()),
                axum::extract::Query(ListQuery {
                    limit: asked,
                    offset: Some(-5),
                }),
            )
            .await
            .unwrap();
            assert!(!listed.is_empty(), "limit {asked:?} returned nothing");
            assert!(listed.len() <= 3);
        }
    }

    /// Deleting a council takes its seats with it: a seat row whose council is gone records nothing
    /// anybody can read.
    #[tokio::test]
    async fn seats_do_not_outlive_their_council() {
        let pool = test_pool().await;
        insert_council(
            &pool,
            "c1",
            "why?",
            &seat(SeatKind::Cloud, "m"),
            &[seat(SeatKind::Cloud, "m")],
            1,
        )
        .await
        .unwrap();

        sqlx::query("DELETE FROM council_runs WHERE id = ?")
            .bind("c1")
            .execute(&pool)
            .await
            .unwrap();

        assert!(get_seat_rows(&pool, "c1").await.unwrap().is_empty());
        pool.close().await;
    }

    fn at(stamp: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(stamp)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// A council stamped at a chosen moment and in a chosen state, neither of which `insert_council`
    /// takes — it stamps `now` and starts `running`, both correctly.
    async fn aged_council(pool: &sqlx::SqlitePool, id: &str, status: &str, created_at: &str) {
        insert_council(
            pool,
            id,
            "why?",
            &seat(SeatKind::Cloud, "m"),
            &[seat(SeatKind::Cloud, "m")],
            1,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE council_runs SET created_at = ?, status = ? WHERE id = ?")
            .bind(created_at)
            .bind(status)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// The sweep, and the cascade under a delete that takes more than one row.
    #[tokio::test]
    async fn a_finished_council_and_its_seats_go_past_the_window() {
        let pool = test_pool().await;
        aged_council(&pool, "old", STATUS_DONE, "2026-01-01T00:00:00+00:00").await;
        aged_council(&pool, "recent", STATUS_DONE, "2026-08-07T00:00:00+00:00").await;

        assert_eq!(
            prune(&pool, 90, at("2026-08-08T12:00:00+00:00"))
                .await
                .unwrap(),
            1
        );

        assert!(get_council_row(&pool, "old").await.unwrap().is_none());
        assert!(
            get_seat_rows(&pool, "old").await.unwrap().is_empty(),
            "the trigger has to fire inside a bulk delete, not only the single-row one"
        );
        assert!(get_council_row(&pool, "recent").await.unwrap().is_some());
        assert_eq!(get_seat_rows(&pool, "recent").await.unwrap().len(), 1);

        pool.close().await;
    }

    /// A `running` row may still be being written by its driver. `reconcile` is what settles the
    /// ones a stopped daemon abandoned, and only after that may they age out — so "only terminal"
    /// costs nothing and is not a way for a crashed council to become immortal.
    #[tokio::test]
    async fn a_running_council_is_never_pruned() {
        let pool = test_pool().await;
        aged_council(&pool, "stuck", STATUS_RUNNING, "2020-01-01T00:00:00+00:00").await;

        assert_eq!(
            prune(&pool, 90, at("2026-08-08T12:00:00+00:00"))
                .await
                .unwrap(),
            0
        );
        assert!(get_council_row(&pool, "stuck").await.unwrap().is_some());

        reconcile(&pool).await.unwrap();
        assert_eq!(
            prune(&pool, 90, at("2026-08-08T12:00:00+00:00"))
                .await
                .unwrap(),
            1
        );

        pool.close().await;
    }

    /// Copied from `runs::transcript_retention_is_exact_at_the_boundary`, and for its reason:
    /// `created_at` is RFC 3339 and SQLite's `datetime()` is not, and the two are compared as TEXT.
    /// Inside the cutoff's own day `T` sorts after the space, so a `datetime()` cutoff spares a
    /// day's worth of councils on every sweep, for ever. The coarse test above would not notice.
    #[tokio::test]
    async fn council_retention_is_exact_at_the_boundary() {
        let pool = test_pool().await;
        // One second either side of a 90-day cutoff taken from 2026-08-08T12:00:00Z.
        aged_council(&pool, "past", STATUS_DONE, "2026-05-10T11:59:59+00:00").await;
        aged_council(&pool, "inside", STATUS_DONE, "2026-05-10T12:00:01+00:00").await;

        assert_eq!(
            prune(&pool, 90, at("2026-08-08T12:00:00+00:00"))
                .await
                .unwrap(),
            1
        );

        assert!(get_council_row(&pool, "past").await.unwrap().is_none());
        assert!(get_council_row(&pool, "inside").await.unwrap().is_some());

        pool.close().await;
    }

    /// Zero is not a retention policy, it is a typo that empties the council history on the machine.
    #[tokio::test]
    async fn a_zero_or_negative_window_prunes_no_council() {
        let pool = test_pool().await;
        aged_council(&pool, "ancient", STATUS_DONE, "2020-01-01T00:00:00+00:00").await;

        let now = at("2026-08-08T12:00:00+00:00");
        assert_eq!(prune(&pool, 0, now).await.unwrap(), 0);
        assert_eq!(prune(&pool, -1, now).await.unwrap(), 0);
        assert!(get_council_row(&pool, "ancient").await.unwrap().is_some());

        pool.close().await;
    }

    // -----------------------------------------------------------------------------------------
    // Which engine a local seat actually reaches
    // -----------------------------------------------------------------------------------------

    /// A loopback `POST /chat/completions` answering one fixed sentence in the shape
    /// `openai_compatible::assistant_message` reads — the council's own copy of
    /// `assistants::stub_completions_recording`, and it records nothing because what matters here
    /// is not what was asked but what came back. The sentence exists nowhere else in this module,
    /// so a seat that wrote it down can only have got it from this address.
    async fn stub_openai_compatible_seat(answer: &str) -> String {
        let answer = answer.to_string();
        let app = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(move || {
                let answer = answer.clone();
                async move {
                    axum::Json(serde_json::json!({
                        "choices": [{ "message": { "role": "assistant", "content": answer } }]
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}")
    }

    /// An `Assistants` whose local route is ONE OpenAI-compatible address, recording every model it
    /// was asked to build a chat for.
    ///
    /// None of the three doubles in `assistants.rs` can stand in: each answers `assistant_for`,
    /// which hands back a whole `LocalAssistant`, and a council seat does not want one — it wants
    /// the chat, and `local_chat` is the seam that gives it the same one a chat turn would get.
    struct SeatAssistants {
        base_url: String,
        /// Every model `local_chat` was asked for, in call order.
        asked_for: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl crate::assistants::Assistants for SeatAssistants {
        fn local_chat(
            &self,
            model: &str,
        ) -> Result<Box<dyn crate::local_agent::LocalChat>, crate::assistants::Refusal> {
            self.asked_for
                .lock()
                .expect("the double's recorder is never held across an await")
                .push(model.to_string());
            Ok(Box::new(
                crate::openai_compatible::OpenAiCompatibleChat::with_client(
                    reqwest::Client::new(),
                    self.base_url.clone(),
                    model.to_string(),
                    // `None`, deliberately: a loopback OpenAI-compatible server asks for no key, which
                    // is the case `OpenAiCompatibleChat::new` refuses and `with_client` exists to express.
                    None,
                ),
            ))
        }

        /// A seat never asks for one, and a double that invented an answer here would be
        /// describing a route this test says nothing about.
        fn assistant_for(
            &self,
            _brain: crate::chats::Brain,
            _model: Option<&str>,
        ) -> Result<std::sync::Arc<crate::local_agent::LocalAssistant>, crate::assistants::Refusal>
        {
            Err(crate::assistants::Refusal::NotServedByThisFactory)
        }

        /// Local only — the honest answer for a factory holding one local address, and what
        /// `resolve_seat` reads before it lets a local seat be filled at all.
        fn serves(&self, brain: crate::chats::Brain) -> Result<(), crate::assistants::Refusal> {
            match brain {
                crate::chats::Brain::Local => Ok(()),
                _ => Err(crate::assistants::Refusal::RouteNotConfigured),
            }
        }

        // As trivial as `serves` above: this double serves the local route, so it serves every
        // model somebody points it at.
        async fn can_serve(
            &self,
            _brain: crate::chats::Brain,
            _model: &str,
        ) -> Result<(), crate::assistants::Refusal> {
            Ok(())
        }

        // Trivial for the same reason `can_serve` above is: nothing on the council's path asks
        // what a model declares, and an empty map — nothing known about anybody — is honest.
        async fn declared_for(
            &self,
            _brain: crate::chats::Brain,
            _models: &[String],
        ) -> std::collections::HashMap<String, crate::capabilities::Declared> {
            std::collections::HashMap::new()
        }
    }

    /// A local seat asks the factory for its chat, exactly as a chat turn does.
    ///
    /// Without this, `run_local_seat` keeps building its own `runner::OllamaChat` against
    /// `runner::OLLAMA_BASE_URL` — and on an install whose `local_engine` is `openai_compatible`, the two
    /// halves of the same daemon then run on DIFFERENT engines: the owner's chat reaches the
    /// OpenAI-compatible server they configured, while every council seat quietly reaches an
    /// Ollama that may not be running, may not hold the model, or may not be the machine that was
    /// paid for. Nothing about that failure is visible from outside — the seat simply fails, or
    /// answers as a different model — which is why what is asserted here is that the seat's own
    /// recorded answer is the one only the configured engine could have produced.
    ///
    /// Through `start` rather than by calling `run_local_seat` directly: the run row is where a
    /// seat's answer is written down, and only the whole path writes it.
    #[tokio::test]
    async fn a_local_seat_is_served_by_the_configured_openai_compatible_engine() {
        const ANSWER: &str = "the answer that exists only on this loopback server";
        let base_url = stub_openai_compatible_seat(ANSWER).await;

        let config = CouncilConfig {
            timeout_seconds: 10,
            rounds: crate::config::DEFAULT_COUNCIL_ROUNDS,
            consumers: crate::config::CouncilConsumers::default(),
            chairman: spec(SeatKind::Cloud, "the-chairman"),
            members: vec![spec(SeatKind::Local, "a-frontier-moe")],
        };
        let mut state =
            council_state(std::sync::Arc::new(ScriptedRunner::default()), Some(config)).await;
        let assistants = std::sync::Arc::new(SeatAssistants {
            base_url,
            asked_for: std::sync::Mutex::new(Vec::new()),
        });
        state.assistants = assistants.clone();

        let id = start(&state, "why?", None).await.unwrap();
        settled(&state, &id).await;

        let seats = get_seat_rows(&state.pool, &id).await.unwrap();
        assert_eq!(seats.len(), 1);
        let steps = steps_for(&state, &id).await;
        let answered = step_at(&steps, 0, 0, store::PHASE_ANSWER).expect("the seat has a step");
        assert_eq!(
            answered.status, SEAT_OK,
            "the local seat did not answer: {:?}",
            answered.error
        );
        let answer: Option<String> = sqlx::query_scalar("SELECT stdout FROM runs WHERE id = ?")
            .bind(
                answered
                    .run_id
                    .expect("a local seat lands a run row like any other"),
            )
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            answer.as_deref(),
            Some(ANSWER),
            "the seat's answer must have come from the engine the factory serves, not from \
             whatever `runner::OLLAMA_BASE_URL` happens to be"
        );

        // And it asked for the seat's OWN model, not the route's configured default: a seat's row
        // records which model answered, and a factory handed a different name would make that
        // record a fiction.
        let asked_for = assistants
            .asked_for
            .lock()
            .expect("the double's recorder is never held across an await")
            .clone();
        assert_eq!(asked_for, vec!["a-frontier-moe".to_string()]);
    }

    /// The one thing the stub above cannot say: whether a REAL frontier MoE, served by a real
    /// OpenAI-compatible server on this machine, actually answers a council seat end to end.
    ///
    /// Without it, every claim this module makes about the OpenAI transport rests on a stub that
    /// answers instantly, in one exchange, out of a body this repository wrote itself — and the
    /// three things a real server does differently (a first token that arrives only once the
    /// weights are paged in, a `/chat/completions` body assembled by somebody else's template, a
    /// model that may answer with a tool call the council's own box has to refuse) are exactly the
    /// three a stub cannot produce.
    ///
    /// `#[ignore]` for hardware and not for taste, and the reason is worth stating plainly: the
    /// owner's machine has 15.8 GB of RAM, a frontier MoE's expert pool does not fit in it, and so
    /// the server this test dials cannot be started here at all. That is a fact about this desk
    /// rather than about this crate — the same kind of caveat
    /// `capabilities::um_ollama_real_declara_um_array_de_capacidades` carries for its own live
    /// Ollama — so the body below is written as a test that WOULD pass the day the hardware
    /// exists, never as a `todo!()` standing in for one.
    ///
    /// Run it deliberately, from the repository root, with the server already serving the model:
    ///   cargo test -p nucleos-core -- --ignored --nocapture council::tests::a_real_frontier_moe_answers_a_council_seat
    #[tokio::test]
    #[ignore = "needs a loopback OpenAI-compatible server (FreeToken / vLLM / llama.cpp / LM Studio) already serving the configured `local_assistant_model`, on a machine with enough RAM to hold a frontier MoE's expert pool — the 15.8 GB on this one is not enough"]
    async fn a_real_frontier_moe_answers_a_council_seat() {
        // The file the daemon reads, on purpose: the question is about THIS machine's model.
        let root = crate::machine_config::root().expect("this machine must have a home directory");
        let models =
            crate::config::load_models_config(&root.join(crate::config::MODELS_CONFIG_FILE))
                .expect("the daemon's own models config must parse");
        let engine = models
            .local_engine()
            .expect("`local_engine` must resolve for this test to mean anything");
        assert_eq!(
            engine.engine,
            crate::config::LocalEngine::OpenAiCompatible,
            "this is the OpenAI transport's end-to-end check: set `local_engine: openai_compatible` and \
             `local_base_url` to the server holding the MoE before running it"
        );
        let model = models
            .local_assistant_model
            .clone()
            .expect("`local_assistant_model` must name the model this test asks");

        let config = CouncilConfig {
            // Ten minutes, where every stubbed roster in this module gets one second: a real MoE's
            // first token arrives once its experts are paged in, and a seat that timed out waiting
            // for that would be written down as a broken transport rather than as a slow disk.
            timeout_seconds: 600,
            rounds: crate::config::DEFAULT_COUNCIL_ROUNDS,
            consumers: crate::config::CouncilConsumers::default(),
            chairman: spec(SeatKind::Cloud, "the-chairman"),
            members: vec![spec(SeatKind::Local, &model)],
        };
        let mut state =
            council_state(std::sync::Arc::new(ScriptedRunner::default()), Some(config)).await;
        // The REAL factory, built off the same file the daemon reads. A double here would exercise
        // this test's own wiring and say nothing whatever about the server.
        state.assistants = std::sync::Arc::new(
            crate::assistants::ConfiguredAssistants::new(
                Some(model.clone()),
                None,
                None,
                daemon_url(),
                "council-key".to_string(),
                state.pool.clone(),
                engine.base_url.clone(),
            )
            .with_local_engine(engine.engine, engine.declared_context_tokens),
        );

        let id = start(
            &state,
            "Name one thing a council of models can do that one model cannot.",
            None,
        )
        .await
        .unwrap();

        // `settled` gives up after twelve seconds, which is a stub's budget and not a model's.
        // The same poll, on the clock this seat was actually given.
        let mut settled_row = None;
        for _ in 0..1_300 {
            let current = get_council_row(&state.pool, &id).await.unwrap().unwrap();
            if current.status != STATUS_RUNNING {
                settled_row = Some(current);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        let row = settled_row.expect("the council never settled inside the seat's own wall clock");
        assert_ne!(row.status, STATUS_RUNNING);

        let seats = get_seat_rows(&state.pool, &id).await.unwrap();
        assert_eq!(seats.len(), 1);
        let steps = steps_for(&state, &id).await;
        let answered = step_at(&steps, 0, 0, store::PHASE_ANSWER).expect("the seat has a step");
        assert_eq!(
            answered.status, SEAT_OK,
            "the real server refused the seat: {:?}",
            answered.error
        );
        let answer: Option<String> = sqlx::query_scalar("SELECT stdout FROM runs WHERE id = ?")
            .bind(
                answered
                    .run_id
                    .expect("a local seat lands a run row like any other"),
            )
            .fetch_one(&state.pool)
            .await
            .unwrap();
        // Prose, not a particular sentence: what a real model says is its own business, and a test
        // that pinned the words would fail on the next model rather than on the next defect.
        assert!(
            answer
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty()),
            "a real model must answer the seat with prose, not with nothing: {answer:?}"
        );
    }

    // ---- P5: the view reads `council_rounds` ----

    /// The step of `seat` at (`round`, `phase`), if the view served one.
    fn step_of<'a>(seat: &'a SeatView, round: i64, phase: &str) -> Option<&'a StepView> {
        seat.steps
            .iter()
            .find(|step| step.round as i64 == round && step.phase == phase)
    }

    /// The text served for one step, if any.
    fn step_answer(seat: &SeatView, round: i64, phase: &str) -> Option<String> {
        step_of(seat, round, phase).and_then(|step| step.answer.clone())
    }

    /// A finished run whose transcript is `stdout`, as a seat's or the chairman's would be.
    async fn run_with_stdout(pool: &sqlx::SqlitePool, stdout: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, created_at, stdout)
             VALUES ('p', 'done', ?, ?, ?, ?)",
        )
        .bind(COUNCIL_MODE)
        .bind(crate::auth::generate_uuid_v4())
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(stdout)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// A council of `members` plain-model seats (`model-0`, `model-1`, ...), with no steps yet.
    async fn bare_council(pool: &sqlx::SqlitePool, id: &str, members: usize, rounds: i64) {
        let roster: Vec<CouncilSeat> = (0..members)
            .map(|index| seat(SeatKind::Cloud, &format!("model-{index}")))
            .collect();
        insert_council(
            pool,
            id,
            "why?",
            &seat(SeatKind::Cloud, "the-chairman"),
            &roster,
            rounds,
        )
        .await
        .unwrap();
    }

    fn critique_json(ranking: &[&str]) -> String {
        serde_json::json!({ "reviews": [], "ranking": ranking }).to_string()
    }

    /// `voter seat -> ranking labels`, the shape `tally` reads a round's ballots in.
    fn ballots(votes: &[(usize, &[&str])]) -> BTreeMap<usize, Vec<String>> {
        votes
            .iter()
            .map(|(voter, labels)| (*voter, labels.iter().map(|l| l.to_string()).collect()))
            .collect()
    }

    fn abc() -> BTreeMap<String, usize> {
        [("A", 0), ("B", 1), ("C", 2)]
            .into_iter()
            .map(|(label, seat)| (label.to_string(), seat))
            .collect()
    }

    async fn view_of(state: &crate::state::AppState, id: &str) -> CouncilView {
        get_council(
            axum::extract::State(state.clone()),
            axum::extract::Path(id.to_string()),
        )
        .await
        .unwrap()
        .0
    }

    /// Steps are served per seat, in the order they happen — answer, then each round's critique and
    /// revise — whatever order they were written in, and the council's progress columns come with
    /// them, on the detail and on the list.
    #[tokio::test]
    async fn view_serves_steps_per_seat_in_order() {
        let state = council_state(std::sync::Arc::new(ScriptedRunner::default()), None).await;
        let pool = &state.pool;
        bare_council(pool, "c1", 2, 2).await;
        sqlx::query(
            "UPDATE council_seats SET role = 'devil' WHERE council_id = 'c1' AND seat_idx = 1",
        )
        .execute(pool)
        .await
        .unwrap();

        // Written backwards on purpose: the order served must not be the order stored.
        for seat_idx in [1, 0] {
            store::upsert_step(
                pool,
                "c1",
                2,
                seat_idx,
                store::PHASE_CRITIQUE,
                None,
                SEAT_OK,
                None,
                Some(&critique_json(&["A"])),
            )
            .await
            .unwrap();
            store::upsert_step(
                pool,
                "c1",
                1,
                seat_idx,
                store::PHASE_REVISE,
                None,
                SEAT_ERROR,
                Some("it broke"),
                None,
            )
            .await
            .unwrap();
            store::upsert_step(
                pool,
                "c1",
                1,
                seat_idx,
                store::PHASE_CRITIQUE,
                Some(41),
                SEAT_OK,
                None,
                Some(&critique_json(&["B", "A"])),
            )
            .await
            .unwrap();
            store::upsert_step(
                pool,
                "c1",
                0,
                seat_idx,
                store::PHASE_ANSWER,
                None,
                SEAT_OK,
                None,
                Some(r#"{"answer":"hi"}"#),
            )
            .await
            .unwrap();
        }
        store::set_position(pool, "c1", 2, store::PHASE_CRITIQUE)
            .await
            .unwrap();
        store::set_progress(pool, "c1", 1, true).await.unwrap();

        let row = get_council_row(pool, "c1").await.unwrap().unwrap();
        assert_eq!(row.rounds_run, 1);
        assert!(row.stopped_early);
        assert_eq!(row.current_round, 2);
        assert_eq!(row.current_phase, store::PHASE_CRITIQUE);
        assert_eq!(row.synthesis_json, None);
        assert_eq!(row.synthesis_status, None);

        let view = view_of(&state, "c1").await;
        assert_eq!(view.rounds, 2);
        assert_eq!(view.rounds_run, 1);
        assert!(view.stopped_early);
        assert_eq!(view.current_round, 2);
        assert_eq!(view.current_phase, store::PHASE_CRITIQUE);
        assert_eq!(view.seats.len(), 2);
        assert_eq!(view.seats[0].role, None);
        assert_eq!(view.seats[1].role.as_deref(), Some("devil"));
        assert_eq!(view.seats[1].model_ref, "model-1");
        for seat in &view.seats {
            let order: Vec<(i64, &str)> = seat
                .steps
                .iter()
                .map(|step| (step.round as i64, step.phase.as_str()))
                .collect();
            assert_eq!(
                order,
                vec![
                    (0, store::PHASE_ANSWER),
                    (1, store::PHASE_CRITIQUE),
                    (1, store::PHASE_REVISE),
                    (2, store::PHASE_CRITIQUE),
                ]
            );
            let critique = &seat.steps[1];
            assert_eq!(critique.run_id, Some(41));
            assert_eq!(critique.status, SEAT_OK);
            assert_eq!(
                critique.critique.as_ref().map(|c| c.ranking.clone()),
                Some(vec!["B".to_string(), "A".to_string()])
            );
            assert_eq!(critique.answer, None);
            let revise = &seat.steps[2];
            assert_eq!(revise.status, SEAT_ERROR);
            assert_eq!(revise.error.as_deref(), Some("it broke"));
            assert_eq!(revise.critique, None);
        }

        let listed = list_councils(
            axum::extract::State(state.clone()),
            axum::extract::Query(ListQuery {
                limit: None,
                offset: None,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].rounds, 2);
        assert_eq!(listed[0].rounds_run, 1);
        assert_eq!(listed[0].current_round, 2);
        assert_eq!(listed[0].current_phase, store::PHASE_CRITIQUE);
    }

    /// The leaderboard and the agreement are computed from the stored ballots when the view is
    /// read — per critique round, the last one being THE leaderboard — and never read off the
    /// legacy `leaderboard` column. A critique that failed casts no ballot.
    #[tokio::test]
    async fn view_computes_borda_and_agreement_on_read() {
        let state = council_state(std::sync::Arc::new(ScriptedRunner::default()), None).await;
        let pool = &state.pool;
        bare_council(pool, "c1", 3, 2).await;
        set_anon_map(pool, "c1", &abc()).await.unwrap();
        // The legacy `leaderboard` column is gone (0154 drops it), so there is no stale copy left
        // for the view to serve by mistake: the ballots below are the only source.
        assert!(
            sqlx::query("SELECT leaderboard FROM council_runs WHERE id = 'c1'")
                .fetch_optional(pool)
                .await
                .is_err(),
            "council_runs.leaderboard must no longer exist"
        );

        let round_one: &[(usize, &[&str])] = &[(0, &["C", "B"]), (1, &["C", "A"])];
        let round_two: &[(usize, &[&str])] =
            &[(0, &["B", "C"]), (1, &["A", "C"]), (2, &["A", "B"])];
        for (voter, labels) in round_one {
            store::upsert_step(
                pool,
                "c1",
                1,
                *voter as i64,
                store::PHASE_CRITIQUE,
                None,
                SEAT_OK,
                None,
                Some(&critique_json(labels)),
            )
            .await
            .unwrap();
        }
        // Seat 2's first critique failed: no payload, no ballot.
        store::upsert_step(
            pool,
            "c1",
            1,
            2,
            store::PHASE_CRITIQUE,
            None,
            SEAT_ERROR,
            Some("boom"),
            None,
        )
        .await
        .unwrap();
        for (voter, labels) in round_two {
            store::upsert_step(
                pool,
                "c1",
                2,
                *voter as i64,
                store::PHASE_CRITIQUE,
                None,
                SEAT_OK,
                None,
                Some(&critique_json(labels)),
            )
            .await
            .unwrap();
        }

        let view = view_of(&state, "c1").await;
        let expected_one = tally::borda(&ballots(round_one), &abc());
        let expected_two = tally::borda(&ballots(round_two), &abc());
        assert_eq!(
            view.leaderboard_by_round,
            vec![expected_one, expected_two.clone()]
        );
        assert_eq!(view.leaderboard, expected_two);
        assert_eq!(
            view.leaderboard[0].seat_idx, 0,
            "every ballot put seat 0 first"
        );
        assert_eq!(
            view.agreement,
            Some(tally::agreement(&ballots(round_two), &abc()))
        );
        assert_eq!(view.agreement.as_ref().unwrap().ballots, 3);
        assert_eq!(view.anon_map, abc());
    }

    /// A council recorded before rounds existed is read in the shape `0154` copied it into: answers
    /// with no payload (the prose is in the transcript) and critiques that are a bare ballot with no
    /// reviews. It still gets its text and a Borda leaderboard.
    #[tokio::test]
    async fn view_reads_a_migrated_council_with_borda() {
        let state = council_state(std::sync::Arc::new(ScriptedRunner::default()), None).await;
        let pool = &state.pool;
        bare_council(pool, "old", 3, 1).await;
        set_anon_map(pool, "old", &abc()).await.unwrap();
        let votes: &[(usize, &[&str])] = &[(0, &["B", "C"]), (1, &["C", "A"]), (2, &["B", "A"])];
        for seat_idx in 0..3i64 {
            let run = run_with_stdout(pool, &format!("old answer {seat_idx}")).await;
            store::upsert_step(
                pool,
                "old",
                0,
                seat_idx,
                store::PHASE_ANSWER,
                Some(run),
                SEAT_OK,
                None,
                None,
            )
            .await
            .unwrap();
        }
        for (voter, labels) in votes {
            // Byte for byte what the migration writes: `json_object('reviews', json('[]'), ...)`.
            let payload = format!(
                r#"{{"reviews":[],"ranking":[{}]}}"#,
                labels
                    .iter()
                    .map(|l| format!("\"{l}\""))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            store::upsert_step(
                pool,
                "old",
                1,
                *voter as i64,
                store::PHASE_CRITIQUE,
                None,
                SEAT_OK,
                None,
                Some(&payload),
            )
            .await
            .unwrap();
        }
        finish(pool, "old", STATUS_DONE, None).await.unwrap();

        let view = view_of(&state, "old").await;
        for (index, seat) in view.seats.iter().enumerate() {
            assert_eq!(
                step_answer(seat, 0, store::PHASE_ANSWER),
                Some(format!("old answer {index}"))
            );
            let critique = step_of(seat, 1, store::PHASE_CRITIQUE)
                .and_then(|step| step.critique.clone())
                .expect("a migrated ballot reads as a critique");
            assert!(critique.reviews.is_empty());
        }
        let expected = tally::borda(&ballots(votes), &abc());
        assert_eq!(view.leaderboard, expected);
        assert_eq!(view.leaderboard_by_round, vec![expected]);
        // B is first on two ballots and second on none.
        assert_eq!(view.leaderboard[0].seat_idx, 1);
        assert!(view.agreement.is_some());
    }

    /// An answer or revision's text is its payload's `answer` when the step carries one, and the
    /// run's transcript only when it does not; a revision also serves whether it changed and why.
    #[tokio::test]
    async fn view_prefers_payload_text_and_falls_back_to_transcript() {
        let state = council_state(std::sync::Arc::new(ScriptedRunner::default()), None).await;
        let pool = &state.pool;
        bare_council(pool, "c1", 3, 2).await;

        let run0 = run_with_stdout(pool, "from the transcript, seat 0").await;
        let run1 = run_with_stdout(pool, "from the transcript, seat 1").await;
        // Seat 0: payload and transcript both present — the payload wins.
        store::upsert_step(
            pool,
            "c1",
            0,
            0,
            store::PHASE_ANSWER,
            Some(run0),
            SEAT_OK,
            None,
            Some(r#"{"answer":"from the payload"}"#),
        )
        .await
        .unwrap();
        // Seat 1: no payload — the transcript is read.
        store::upsert_step(
            pool,
            "c1",
            0,
            1,
            store::PHASE_ANSWER,
            Some(run1),
            SEAT_OK,
            None,
            None,
        )
        .await
        .unwrap();
        // Seat 2: neither — nothing, not an empty string.
        store::upsert_step(
            pool,
            "c1",
            0,
            2,
            store::PHASE_ANSWER,
            None,
            SEAT_PENDING,
            None,
            None,
        )
        .await
        .unwrap();

        let rev0 = run_with_stdout(pool, "revision transcript, seat 0").await;
        let rev1 = run_with_stdout(pool, "revision transcript, seat 1").await;
        store::upsert_step(
            pool,
            "c1",
            1,
            0,
            store::PHASE_REVISE,
            Some(rev0),
            SEAT_OK,
            None,
            Some(r#"{"answer":"revised in the payload","changed":true,"why":"seat B was right"}"#),
        )
        .await
        .unwrap();
        store::upsert_step(
            pool,
            "c1",
            1,
            1,
            store::PHASE_REVISE,
            Some(rev1),
            SEAT_OK,
            None,
            None,
        )
        .await
        .unwrap();
        store::upsert_step(
            pool,
            "c1",
            1,
            2,
            store::PHASE_REVISE,
            None,
            SEAT_OK,
            None,
            Some(r#"{"changed":false,"why":"nothing moved me"}"#),
        )
        .await
        .unwrap();

        let view = view_of(&state, "c1").await;
        let seats = &view.seats;
        assert_eq!(
            step_answer(&seats[0], 0, store::PHASE_ANSWER).as_deref(),
            Some("from the payload")
        );
        assert_eq!(
            step_answer(&seats[1], 0, store::PHASE_ANSWER).as_deref(),
            Some("from the transcript, seat 1")
        );
        assert_eq!(step_answer(&seats[2], 0, store::PHASE_ANSWER), None);
        // An answer is not a revision: it carries no `changed`/`why`.
        let answer = step_of(&seats[0], 0, store::PHASE_ANSWER).unwrap();
        assert_eq!(answer.changed, None);
        assert_eq!(answer.why, None);
        assert_eq!(answer.critique, None);

        let revised = step_of(&seats[0], 1, store::PHASE_REVISE).unwrap();
        assert_eq!(revised.answer.as_deref(), Some("revised in the payload"));
        assert_eq!(revised.changed, Some(true));
        assert_eq!(revised.why.as_deref(), Some("seat B was right"));
        assert_eq!(
            step_answer(&seats[1], 1, store::PHASE_REVISE).as_deref(),
            Some("revision transcript, seat 1")
        );
        let kept = step_of(&seats[2], 1, store::PHASE_REVISE).unwrap();
        assert_eq!(kept.changed, Some(false));
        assert_eq!(kept.why.as_deref(), Some("nothing moved me"));

        // No critique round at all: nothing to tally.
        assert!(view.leaderboard.is_empty());
        assert!(view.leaderboard_by_round.is_empty());
        assert_eq!(view.agreement, None);
    }

    /// A structured synthesis is served twice: as the struct, and as markdown composed with each
    /// seat's NAME — the agent's when an agent took the seat, the model's otherwise. The chairman's
    /// raw transcript is not what is served once a structured one exists.
    #[tokio::test]
    async fn view_synthesis_is_composed_from_synthesis_json() {
        let state = council_state(std::sync::Arc::new(ScriptedRunner::default()), None).await;
        let pool = &state.pool;
        bare_council(pool, "c1", 2, 1).await;
        let agent_id = catalogue(pool, agent_request("Cetico")).await;
        sqlx::query(
            "UPDATE council_seats SET agent_id = ? WHERE council_id = 'c1' AND seat_idx = 0",
        )
        .bind(&agent_id)
        .execute(pool)
        .await
        .unwrap();
        let chairman = run_with_stdout(pool, "the chairman's raw transcript").await;
        set_chairman_run(pool, "c1", chairman).await.unwrap();

        let synthesis = formats::Synthesis {
            answer: "Do the thing.".to_string(),
            consensus: vec!["it is worth doing".to_string()],
            disagreements: vec![formats::Disagreement {
                topic: "when".to_string(),
                positions: vec![
                    formats::Position {
                        seats: vec![0],
                        view: "now".to_string(),
                    },
                    formats::Position {
                        seats: vec![1],
                        view: "later".to_string(),
                    },
                ],
            }],
            minority: None,
            confidence: formats::Confidence {
                level: "high".to_string(),
                why: "they agree".to_string(),
            },
            open_questions: vec![],
            degraded_reason: None,
        };
        let json = serde_json::to_string(&synthesis).unwrap();
        store::set_synthesis(pool, "c1", Some(&json), "ok")
            .await
            .unwrap();
        finish(pool, "c1", STATUS_DONE, None).await.unwrap();

        let names = |seat: usize| match seat {
            0 => "Cetico".to_string(),
            _ => "model-1".to_string(),
        };
        let expected = formats::compose_markdown(&synthesis, &names);
        assert!(expected.contains("Cetico") && expected.contains("model-1"));

        let row = get_council_row(pool, "c1").await.unwrap().unwrap();
        assert_eq!(row.synthesis_json.as_deref(), Some(json.as_str()));
        assert_eq!(row.synthesis_status.as_deref(), Some("ok"));
        let (text, structured) = synthesis_text(pool, &row).await;
        assert_eq!(text.as_deref(), Some(expected.as_str()));
        assert_eq!(structured.as_ref(), Some(&synthesis));
        assert_eq!(
            synthesis_of(pool, &row).await.as_deref(),
            Some(expected.as_str())
        );

        let view = view_of(&state, "c1").await;
        assert_eq!(view.synthesis.as_deref(), Some(expected.as_str()));
        assert_eq!(view.synthesis_structured, Some(synthesis));
        assert_eq!(view.synthesis_status.as_deref(), Some("ok"));
    }

    /// With no readable `synthesis_json` — a council recorded before it existed, or one whose
    /// column will not parse — the synthesis is the chairman run's transcript, as it always was;
    /// and with no chairman run, or one that wrote nothing, there is none at all.
    #[tokio::test]
    async fn view_synthesis_of_falls_back_to_the_transcript_and_then_none() {
        let state = council_state(std::sync::Arc::new(ScriptedRunner::default()), None).await;
        let pool = &state.pool;

        bare_council(pool, "legacy", 1, 1).await;
        let chairman = run_with_stdout(pool, "the old synthesis").await;
        set_chairman_run(pool, "legacy", chairman).await.unwrap();
        let row = get_council_row(pool, "legacy").await.unwrap().unwrap();
        assert_eq!(
            synthesis_text(pool, &row).await,
            (Some("the old synthesis".to_string()), None)
        );
        assert_eq!(
            synthesis_of(pool, &row).await.as_deref(),
            Some("the old synthesis")
        );
        let view = view_of(&state, "legacy").await;
        assert_eq!(view.synthesis.as_deref(), Some("the old synthesis"));
        assert_eq!(view.synthesis_structured, None);
        assert_eq!(view.synthesis_status, None);

        // A column that will not parse is no synthesis; the transcript still is.
        bare_council(pool, "garbled", 1, 1).await;
        let chairman = run_with_stdout(pool, "the raw words").await;
        set_chairman_run(pool, "garbled", chairman).await.unwrap();
        store::set_synthesis(pool, "garbled", Some("not json at all"), "degraded")
            .await
            .unwrap();
        let row = get_council_row(pool, "garbled").await.unwrap().unwrap();
        assert_eq!(
            synthesis_text(pool, &row).await,
            (Some("the raw words".to_string()), None)
        );

        // No chairman run: no synthesis, and not an empty one.
        bare_council(pool, "none", 1, 1).await;
        let row = get_council_row(pool, "none").await.unwrap().unwrap();
        assert_eq!(synthesis_text(pool, &row).await, (None, None));
        assert_eq!(synthesis_of(pool, &row).await, None);

        // A chairman run that wrote nothing reads as none too.
        bare_council(pool, "silent", 1, 1).await;
        let chairman = run_with_stdout(pool, "   ").await;
        set_chairman_run(pool, "silent", chairman).await.unwrap();
        let row = get_council_row(pool, "silent").await.unwrap().unwrap();
        assert_eq!(synthesis_of(pool, &row).await, None);
        assert_eq!(view_of(&state, "silent").await.synthesis, None);
    }

    // ---- P6: the round driver — answer, critique, structured chairman ----
    //
    // Written first (RED). The driver these bind to: `start_with(state, question, roster, rounds,
    // roles)`, one `council_rounds` step per (seat, round, phase) written through `store`, the
    // chairman's synthesis parsed and validated with ONE retry, and cancel/reconcile settling the
    // steps as well as the council.

    /// Every step of a council, as stored.
    async fn steps_for(state: &crate::state::AppState, id: &str) -> Vec<store::StepRow> {
        store::steps_of(&state.pool, id).await.unwrap()
    }

    /// The step of `seat_idx` at (`round`, `phase`), if one was written.
    fn step_at<'a>(
        steps: &'a [store::StepRow],
        seat_idx: i64,
        round: i64,
        phase: &str,
    ) -> Option<&'a store::StepRow> {
        steps
            .iter()
            .find(|step| step.seat_idx == seat_idx && step.round == round && step.phase == phase)
    }

    /// A chairman reply that parses AND validates at any agreement level: it states a minority,
    /// so the "a dissent may be absent only when strong or insufficient" rule never refuses it.
    fn synthesis_reply(answer: &str) -> String {
        format!(
            "```json\n{}\n```",
            serde_json::json!({
                "answer": answer,
                "consensus": ["the point everybody made"],
                "disagreements": [],
                "minority": "the view one seat held alone",
                "confidence": { "level": "moderate", "why": "two of three agreed" },
                "open_questions": [],
            })
        )
    }

    /// The prompts that carry `marker`, in the order they were sent.
    fn prompts_with(runner: &ScriptedRunner, marker: &str) -> Vec<String> {
        prompts(runner)
            .into_iter()
            .filter(|prompt| prompt.contains(marker))
            .collect()
    }

    fn roles(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(seat, role)| (seat.to_string(), role.to_string()))
            .collect()
    }

    #[tokio::test]
    async fn driver_one_round_answers_critiques_and_synthesises() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("answer zero".into()),
            Scripted::Answers("answer one".into()),
            Scripted::Answers("answer two".into()),
        ]
        .into();
        // Each seat ranks every label; `parse_critique` keeps only the two it was shown.
        *runner.critique.lock().unwrap() = [
            Scripted::Answers(critique_json(&["A", "B", "C"])),
            Scripted::Answers(critique_json(&["A", "B", "C"])),
            Scripted::Answers(critique_json(&["C", "B", "A"])),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start(&state, "why?", None).await.unwrap();
        let row = settled(&state, &id).await;

        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);
        assert_eq!(row.rounds_run, 1, "one critique round ran");
        assert!(!row.stopped_early);
        assert!(row.chairman_run_id.is_some());

        let steps = steps_for(&state, &id).await;
        for (seat_idx, text) in ["answer zero", "answer one", "answer two"]
            .into_iter()
            .enumerate()
        {
            let seat_idx = seat_idx as i64;
            let answer = step_at(&steps, seat_idx, 0, store::PHASE_ANSWER)
                .unwrap_or_else(|| panic!("seat {seat_idx} has no answer step: {steps:?}"));
            assert_eq!(answer.status, SEAT_OK);
            assert!(answer.run_id.is_some());
            let payload: serde_json::Value =
                serde_json::from_str(answer.payload.as_deref().expect("an answer payload"))
                    .unwrap();
            assert_eq!(payload, serde_json::json!({ "answer": text }));

            let critique = step_at(&steps, seat_idx, 1, store::PHASE_CRITIQUE)
                .unwrap_or_else(|| panic!("seat {seat_idx} has no critique step: {steps:?}"));
            assert_eq!(critique.status, SEAT_OK);
            assert!(critique.run_id.is_some());
            let ballot: formats::Critique =
                serde_json::from_str(critique.payload.as_deref().expect("a critique payload"))
                    .unwrap();
            assert_eq!(
                ballot.ranking.len(),
                2,
                "a seat's ballot keeps only the labels it was shown"
            );
        }
        // No revise in this packet, whatever the round count.
        assert!(steps.iter().all(|step| step.phase != store::PHASE_REVISE));

        // Every critique prompt shows the two peers' answers and never the reader's own.
        let critiques = prompts_with(&runner, prompts::CRITIQUE_MARKER);
        assert_eq!(critiques.len(), 3);
        for prompt in &critiques {
            let shown = ["answer zero", "answer one", "answer two"]
                .into_iter()
                .filter(|answer| prompt.contains(answer))
                .count();
            assert_eq!(
                shown, 2,
                "a critique must show exactly the peers:\n{prompt}"
            );
            assert!(
                !prompt.contains("model-"),
                "a critique names no model:\n{prompt}"
            );
        }

        // The chairman is told who said what, the tally, and the agreement level.
        let chairman = prompts_with(&runner, prompts::CHAIRMAN_MARKER);
        assert_eq!(chairman.len(), 1, "a valid synthesis is not retried");
        for name in ["model-0", "model-1", "model-2"] {
            assert!(
                chairman[0].contains(name),
                "{name} missing:\n{}",
                chairman[0]
            );
        }
        assert!(
            !chairman[0].contains("No ranking could be read out of the critiques."),
            "three valid ballots make a leaderboard:\n{}",
            chairman[0]
        );
        assert!(chairman[0].contains("Agreement level: "));

        assert_eq!(row.synthesis_status.as_deref(), Some("ok"));
        let synthesis: formats::Synthesis =
            serde_json::from_str(row.synthesis_json.as_deref().expect("a synthesis")).unwrap();
        assert_eq!(synthesis.answer, "the synthesis");
        assert_eq!(synthesis.degraded_reason, None);

        // Three answers, three critiques, one chairman.
        assert_eq!(runner.seen.lock().unwrap().len(), 7);
    }

    #[tokio::test]
    async fn driver_invalid_critique_is_an_abstention() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("answer zero".into()),
            Scripted::Answers("answer one".into()),
            Scripted::Answers("answer two".into()),
        ]
        .into();
        *runner.critique.lock().unwrap() = [
            Scripted::Answers(critique_json(&["A", "B", "C"])),
            Scripted::Answers("I would rather not rank anybody.".into()),
            Scripted::Answers(critique_json(&["C", "B", "A"])),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start(&state, "why?", None).await.unwrap();
        let row = settled(&state, &id).await;

        // An unreadable critique is one ballot fewer, not a failed council.
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);
        assert_eq!(row.synthesis_status.as_deref(), Some("ok"));

        let steps = steps_for(&state, &id).await;
        let critiques: Vec<&store::StepRow> = steps
            .iter()
            .filter(|step| step.phase == store::PHASE_CRITIQUE)
            .collect();
        assert_eq!(critiques.len(), 3, "{steps:?}");
        let invalid: Vec<&&store::StepRow> = critiques
            .iter()
            .filter(|step| step.status == store::STEP_INVALID)
            .collect();
        assert_eq!(invalid.len(), 1, "{critiques:?}");
        assert!(
            invalid[0]
                .error
                .as_deref()
                .is_some_and(|reason| !reason.trim().is_empty()),
            "an invalid step says why it could not be read: {:?}",
            invalid[0]
        );
        // The run itself finished: the step keeps it, so the transcript stays reachable.
        assert!(invalid[0].run_id.is_some());
        assert_eq!(
            critiques
                .iter()
                .filter(|step| step.status == SEAT_OK)
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn driver_failed_answer_leaves_every_later_phase() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("answer zero".into()),
            Scripted::Fails("the model refused".into()),
            Scripted::Answers("answer two".into()),
        ]
        .into();
        *runner.critique.lock().unwrap() = [
            Scripted::Answers(critique_json(&["A", "B"])),
            Scripted::Answers(critique_json(&["A", "B"])),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start(&state, "why?", None).await.unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);

        // The failed seat has its answer step and nothing after it.
        let steps = steps_for(&state, &id).await;
        let of_failed: Vec<&store::StepRow> =
            steps.iter().filter(|step| step.seat_idx == 1).collect();
        assert_eq!(of_failed.len(), 1, "{of_failed:?}");
        assert_eq!(of_failed[0].phase, store::PHASE_ANSWER);
        assert_eq!(of_failed[0].status, SEAT_ERROR);

        // Only the two seats with an answer were asked to critique, and neither was shown a third.
        let critiques = prompts_with(&runner, prompts::CRITIQUE_MARKER);
        assert_eq!(critiques.len(), 2);
        // The chairman reads the final answers, and the failed seat has none.
        let chairman = prompts_with(&runner, prompts::CHAIRMAN_MARKER);
        assert_eq!(chairman.len(), 1);
        assert!(chairman[0].contains("Seat 0 — "), "{}", chairman[0]);
        assert!(chairman[0].contains("Seat 2 — "), "{}", chairman[0]);
        assert!(!chairman[0].contains("Seat 1 — "), "{}", chairman[0]);
    }

    #[tokio::test]
    async fn driver_chairman_invalid_twice_settles_done_degraded() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("answer zero".into()),
            Scripted::Answers("answer one".into()),
        ]
        .into();
        *runner.chairman.lock().unwrap() = [
            Scripted::Answers("first attempt, no JSON at all".into()),
            Scripted::Answers("second attempt, still prose".into()),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(2))).await;

        let id = start(&state, "why?", None).await.unwrap();
        let row = settled(&state, &id).await;

        // A synthesis that will not parse is shown as it came, not turned into a failed council.
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);
        assert_eq!(
            prompts_with(&runner, prompts::CHAIRMAN_MARKER).len(),
            2,
            "retried exactly once"
        );
        assert_eq!(row.synthesis_status.as_deref(), Some("degraded"));
        let synthesis: formats::Synthesis =
            serde_json::from_str(row.synthesis_json.as_deref().expect("a degraded synthesis"))
                .unwrap();
        assert_eq!(synthesis.answer, "second attempt, still prose");
        assert!(
            synthesis
                .degraded_reason
                .as_deref()
                .is_some_and(|reason| !reason.trim().is_empty()),
            "{synthesis:?}"
        );
    }

    #[tokio::test]
    async fn driver_chairman_retry_carries_the_error() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("answer zero".into()),
            Scripted::Answers("answer one".into()),
        ]
        .into();
        // Parses, and fails validation: the retry has to carry the VALIDATION error too.
        *runner.chairman.lock().unwrap() = [
            Scripted::Answers(synthesis_reply("   ")),
            Scripted::Answers(synthesis_reply("the second, valid synthesis")),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(2))).await;

        let id = start(&state, "why?", None).await.unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);

        let chairman = prompts_with(&runner, prompts::CHAIRMAN_MARKER);
        assert_eq!(chairman.len(), 2);
        assert!(!chairman[0].contains("Your previous synthesis was refused"));
        assert!(
            chairman[1].contains("Your previous synthesis was refused"),
            "{}",
            chairman[1]
        );
        assert!(
            chairman[1].contains("synthesis has an empty answer"),
            "the retry names what was wrong:\n{}",
            chairman[1]
        );
        // The same question plus the one fact that changed.
        assert!(chairman[1].starts_with(chairman[0].as_str()));

        assert_eq!(row.synthesis_status.as_deref(), Some("ok"));
        let synthesis: formats::Synthesis =
            serde_json::from_str(row.synthesis_json.as_deref().unwrap()).unwrap();
        assert_eq!(synthesis.answer, "the second, valid synthesis");
    }

    #[tokio::test]
    async fn driver_cancel_marks_pending_steps_cancelled() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [Scripted::Hangs, Scripted::Hangs].into();
        let mut config = roster(2);
        config.timeout_seconds = 600;
        let state = council_state(runner.clone(), Some(config)).await;

        let id = start(&state, "why?", None).await.unwrap();
        // Both seats in flight, and each one's step written `pending` with its run beside it —
        // the run id on the step is what `cancel` terminates by.
        for _ in 0..300 {
            let steps = steps_for(&state, &id).await;
            if state.run_handles.lock().unwrap().len() == 2
                && steps.len() == 2
                && steps.iter().all(|step| step.run_id.is_some())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let in_flight = steps_for(&state, &id).await;
        assert_eq!(in_flight.len(), 2, "{in_flight:?}");
        for step in &in_flight {
            assert_eq!(step.phase, store::PHASE_ANSWER);
            assert_eq!(step.status, SEAT_PENDING, "{step:?}");
            assert!(step.run_id.is_some(), "{step:?}");
        }

        assert!(cancel(&state, &id).await.unwrap());
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_CANCELLED);

        // Settled however the seats' own futures unwind: never `error`, never left `pending`.
        let mut steps = Vec::new();
        for _ in 0..300 {
            steps = steps_for(&state, &id).await;
            if steps.iter().all(|step| step.status == SEAT_CANCELLED) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        for step in &steps {
            assert_eq!(step.status, SEAT_CANCELLED, "{step:?}");
            let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(step.run_id.unwrap())
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(status, "cancelled", "the step's run was terminated");
        }
        assert!(prompts_with(&runner, prompts::CRITIQUE_MARKER).is_empty());
        assert!(prompts_with(&runner, prompts::CHAIRMAN_MARKER).is_empty());
    }

    #[tokio::test]
    async fn driver_reconcile_errors_orphan_pending_steps() {
        let pool = test_pool().await;
        insert_council(
            &pool,
            "c1",
            "why?",
            &seat(SeatKind::Cloud, "m"),
            &[seat(SeatKind::Cloud, "m"), seat(SeatKind::Cloud, "m")],
            1,
        )
        .await
        .unwrap();
        // A crash in the middle of the answers: one seat had finished, one had not.
        store::upsert_step(
            &pool,
            "c1",
            0,
            0,
            store::PHASE_ANSWER,
            Some(1),
            SEAT_OK,
            None,
            Some(r#"{"answer":"done"}"#),
        )
        .await
        .unwrap();
        store::upsert_step(
            &pool,
            "c1",
            0,
            1,
            store::PHASE_ANSWER,
            Some(2),
            SEAT_PENDING,
            None,
            None,
        )
        .await
        .unwrap();

        reconcile(&pool).await.unwrap();

        assert_eq!(
            get_council_row(&pool, "c1").await.unwrap().unwrap().status,
            STATUS_ERROR
        );
        let steps = store::steps_of(&pool, "c1").await.unwrap();
        let finished = step_at(&steps, 0, 0, store::PHASE_ANSWER).unwrap();
        assert_eq!(
            finished.status, SEAT_OK,
            "a finished step keeps how it ended"
        );
        let orphan = step_at(&steps, 1, 0, store::PHASE_ANSWER).unwrap();
        assert_eq!(
            orphan.status, SEAT_ERROR,
            "nothing will ever finish this step"
        );
        assert!(orphan.error.is_some());

        pool.close().await;
    }

    #[tokio::test]
    async fn driver_start_refuses_unknown_role_and_out_of_roster_seat() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        let state = council_state(runner.clone(), Some(roster(2))).await;

        for (pairs, what) in [
            (roles(&[("0", "jester")]), "a role outside the closed set"),
            (roles(&[("0", "Skeptic")]), "a role is case-sensitive"),
            (roles(&[("2", "skeptic")]), "a seat past the roster"),
            (
                roles(&[("seat-0", "skeptic")]),
                "a key that is not a seat index",
            ),
            (roles(&[("-1", "skeptic")]), "a negative seat"),
        ] {
            match start_with(&state, "why?", None, None, pairs).await {
                Err(StartError::Invalid(reason)) => {
                    assert!(
                        !reason.trim().is_empty(),
                        "{what}: refused without a reason"
                    )
                }
                other => panic!("{what}: expected Invalid, got {other:?}"),
            }
        }

        // Refused before anything was written or spent.
        assert!(
            list_council_rows(&state.pool, 10, 0)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(runner.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn driver_start_refuses_rounds_outside_one_to_three() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        let state = council_state(runner.clone(), Some(roster(2))).await;

        for rounds in [0, crate::config::MAX_COUNCIL_ROUNDS + 1, 99] {
            match start_with(&state, "why?", None, Some(rounds), BTreeMap::new()).await {
                Err(StartError::Invalid(reason)) => {
                    assert!(!reason.trim().is_empty(), "rounds {rounds}")
                }
                other => panic!("rounds {rounds}: expected Invalid, got {other:?}"),
            }
        }
        assert!(
            list_council_rows(&state.pool, 10, 0)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(runner.seen.lock().unwrap().is_empty());

        // The request's count, when valid, is the one the council records — not the file's.
        let id = start_with(
            &state,
            "why?",
            None,
            Some(crate::config::MAX_COUNCIL_ROUNDS),
            BTreeMap::new(),
        )
        .await
        .unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.rounds, i64::from(crate::config::MAX_COUNCIL_ROUNDS));

        // No count asked for: the file's.
        let id = start_with(&state, "why?", None, None, BTreeMap::new())
            .await
            .unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.rounds, i64::from(crate::config::DEFAULT_COUNCIL_ROUNDS));
    }

    #[tokio::test]
    async fn driver_roles_reach_every_phase_of_their_seat() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("answer zero".into()),
            Scripted::Answers("answer one".into()),
        ]
        .into();
        *runner.critique.lock().unwrap() = [
            Scripted::Answers(critique_json(&["A", "B"])),
            Scripted::Answers(critique_json(&["A", "B"])),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(2))).await;

        let id = start_with(&state, "why?", None, None, roles(&[("1", "skeptic")]))
            .await
            .unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);

        // Recorded on the seat, and only on that seat.
        let seats = get_seat_rows(&state.pool, &id).await.unwrap();
        assert_eq!(seats[0].role, None);
        assert_eq!(seats[1].role.as_deref(), Some("skeptic"));

        let paragraph = formats::Role::Skeptic.paragraph();
        let sent = prompts(&runner);
        // The answer phase: the role in front of the question for seat 1, nothing for seat 0.
        let answers: Vec<&String> = sent
            .iter()
            .filter(|prompt| {
                !prompt.contains(prompts::CRITIQUE_MARKER)
                    && !prompt.contains(prompts::CHAIRMAN_MARKER)
            })
            .collect();
        assert_eq!(answers.len(), 2);
        assert!(answers.iter().any(|prompt| prompt.as_str() == "why?"));
        assert!(
            answers
                .iter()
                .any(|prompt| prompt.as_str() == format!("{paragraph}\n\nwhy?")),
            "{answers:?}"
        );
        // The critique phase: the same seat critiques in the same role.
        let critiques = prompts_with(&runner, prompts::CRITIQUE_MARKER);
        assert_eq!(critiques.len(), 2);
        assert_eq!(
            critiques
                .iter()
                .filter(|prompt| prompt.starts_with(paragraph))
                .count(),
            1,
            "{critiques:?}"
        );
        assert_eq!(
            critiques
                .iter()
                .filter(|prompt| prompt.contains(paragraph))
                .count(),
            1,
            "a role is its own seat's, never shown to a peer"
        );
        // The chairman is told which seat played which part.
        let chairman = prompts_with(&runner, prompts::CHAIRMAN_MARKER);
        assert_eq!(chairman.len(), 1);
        assert!(
            chairman[0].contains("Seat 1 — model-1, skeptic"),
            "{}",
            chairman[0]
        );
        assert!(!chairman[0].contains(paragraph));
    }
}
