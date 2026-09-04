//! The council: N seats answer one question, rank each other blind, and a chairman synthesises.
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
//! **The average rank is the load-bearing piece.** Ranks are averaged rather than summed because a
//! council degrades: a seat that failed casts no votes, and a seat that nobody could rank receives
//! none. Sums make an absent participant look bad; averages make it look absent.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::config::{CouncilConfig, CouncilSeat, SeatAgent, SeatKind, SeatSpec};

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
/// Phase 2 only: there was nothing to rank.
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

/// One row of the average-rank leaderboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaderboardEntry {
    pub seat_idx: usize,
    pub avg_rank: f64,
    /// How many peers ranked this seat. Reported because an average over one vote and an average
    /// over five are not the same claim, and a table that showed only the average would present
    /// them as one.
    pub n: usize,
}

/// One label-and-rank pair a seat wrote, after filtering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ranking {
    pub anon: String,
    pub rank: i64,
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

/// Whether a byte is a regex `\w` — the definition the word boundaries below are taken against.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// PURE: the label-and-rank pairs a seat wrote, filtered to the labels it was shown.
///
/// The Python did this with `\b(?:Response\s+)?([A-Z])\s*[:.)-]\s*(\d+)\b`. This crate has no
/// `regex` dependency and does not acquire one for nine lines of scanning, so the automaton is
/// written out — the same habit `redact.rs` keeps for its detectors.
///
/// Three rules carried over unchanged, each answering a way a model gets this wrong:
///
/// - **A label the seat was never shown is discarded.** Accepting it would let a model that invents
///   a fourth response bias a three-seat leaderboard, and inventing is exactly what a model asked
///   to produce structure does when it has nothing to say.
/// - **First mention wins.** A model that writes `A: 1` in its reasoning and `A: 3` in its summary
///   has contradicted itself; counting both would let it vote twice.
/// - **Everything else in the text is ignored.** The reply is prose with an ordering in it, not a
///   form, and requiring a form would turn a formatting slip into a lost vote.
pub fn parse_rankings(text: &str, allowed: &[String]) -> Vec<Ranking> {
    let bytes = text.as_bytes();
    let mut rankings: Vec<Ranking> = Vec::new();
    let mut cursor = 0usize;

    while cursor < bytes.len() {
        // `\b` before the optional `Response`: the previous byte must not be a word byte, because
        // the letter in `partB: 1` is part of a word and names nothing.
        let at_boundary = cursor == 0 || !is_word_byte(bytes[cursor - 1]);
        if !at_boundary {
            cursor += 1;
            continue;
        }

        match match_ranking(bytes, cursor) {
            Some((label, rank, end)) => {
                let label = label.to_string();
                let already_voted = rankings.iter().any(|ranking| ranking.anon == label);
                if allowed.contains(&label) && !already_voted {
                    rankings.push(Ranking { anon: label, rank });
                }
                // Continue past the match, as `re.finditer` does, so `A: 1 B: 2` is two pairs and
                // never one overlapping read of the other.
                cursor = end;
            }
            None => cursor += 1,
        }
    }

    rankings
}

/// One attempt at `(?:Response\s+)?([A-Z])\s*[:.)-]\s*(\d+)\b` starting exactly at `start`.
///
/// Returns the label, the rank and where the match ended. The optional prefix is tried first and
/// the bare letter second, which is the order a greedy optional group is tried in.
fn match_ranking(bytes: &[u8], start: usize) -> Option<(char, i64, usize)> {
    const PREFIX: &[u8] = b"Response";

    let after_prefix = if bytes[start..].starts_with(PREFIX) {
        let mut cursor = start + PREFIX.len();
        let space_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        // `\s+` — at least one. `ResponseA: 1` matches nothing here and falls through to the bare
        // branch, which then fails on the `R`, exactly as the regex does.
        (cursor > space_start).then_some(cursor)
    } else {
        None
    };

    for mut cursor in after_prefix.into_iter().chain(std::iter::once(start)) {
        let Some(label) = bytes.get(cursor).copied().filter(u8::is_ascii_uppercase) else {
            continue;
        };
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if !matches!(bytes.get(cursor), Some(b':' | b'.' | b')' | b'-')) {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let digits_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor == digits_start {
            continue;
        }
        // `\b` after the digits: `A: 12x` is not a rank of 12.
        if bytes.get(cursor).copied().is_some_and(is_word_byte) {
            continue;
        }
        let digits = &bytes[digits_start..cursor];
        // Saturating rather than failing. A number too long for an `i64` is not a rank anybody
        // meant, and dropping the pair would silently turn nonsense into a missing vote; sorting it
        // last says the same thing out loud.
        let rank = std::str::from_utf8(digits)
            .ok()
            .and_then(|text| text.parse::<i64>().ok())
            .unwrap_or(i64::MAX);
        return Some((char::from(label), rank, cursor));
    }

    None
}

/// PURE: the average-rank leaderboard, best first.
///
/// `votes` is what each viewer wrote, already filtered by `parse_rankings`. `anon_map` translates a
/// label back to the seat it stood for; a label absent from it is dropped rather than panicking,
/// because the map and the votes are read from two different database columns and a row edited by
/// hand must not take the daemon down.
///
/// Ties break on `seat_idx`, so the order is total and the same council read twice reads the same.
pub fn aggregate_rankings(
    votes: &BTreeMap<usize, Vec<Ranking>>,
    anon_map: &BTreeMap<String, usize>,
) -> Vec<LeaderboardEntry> {
    let mut collected: BTreeMap<usize, Vec<i64>> = BTreeMap::new();
    for ranked in votes.values() {
        for ranking in ranked {
            let Some(seat) = anon_map.get(&ranking.anon) else {
                continue;
            };
            collected.entry(*seat).or_default().push(ranking.rank);
        }
    }

    let mut board: Vec<LeaderboardEntry> = collected
        .into_iter()
        .map(|(seat_idx, ranks)| {
            let total: f64 = ranks.iter().map(|rank| *rank as f64).sum();
            LeaderboardEntry {
                seat_idx,
                avg_rank: total / ranks.len() as f64,
                n: ranks.len(),
            }
        })
        .collect();
    board.sort_by(|left, right| {
        left.avg_rank
            .partial_cmp(&right.avg_rank)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left.seat_idx.cmp(&right.seat_idx))
    });
    board
}

/// PURE: whether phase 2 is worth running.
///
/// Two valid answers is the floor, and it is a floor rather than a preference: with one there is
/// nothing to compare it against, and the single seat would be asked to rank an empty set. Below it
/// the phase is skipped whole and phase 3 still runs, because the answers themselves are the part
/// with value — a council that produced one good answer and no ranking is worth reading, and one
/// that produced a ranking of nothing is not.
pub fn stage2_should_run(valid_responses: usize) -> bool {
    valid_responses >= 2
}

/// The prompt a phase-1 seat receives: the owner's question, exactly as written.
///
/// Nothing is prepended. A seat is an agent with tools that can go and find what it needs, and a
/// preamble explaining that it is on a panel would change the answer being measured into an answer
/// about being measured.
pub fn stage1_prompt(question: &str) -> String {
    question.to_string()
}

/// The prompt a phase-2 seat receives: the question, the peers' answers under their labels, and the
/// instruction to order them.
///
/// `shown` is `(label, answer)` for the labels this seat may see — never its own.
pub fn stage2_prompt(question: &str, shown: &[(String, String)]) -> String {
    let mut prompt = format!(
        "Question:\n{question}\n\n\
         Rank the anonymous peer responses by accuracy and insight.\n\
         Use lines like `A: 1` where 1 is best.\n\n"
    );
    for (label, answer) in shown {
        prompt.push_str(&format!("Response {label}:\n{answer}\n\n"));
    }
    prompt.trim_end().to_string() + "\n"
}

/// The prompt the chairman receives: the question, the valid answers BY SEAT, and the leaderboard.
///
/// The anonymity ends here on purpose. Phase 2 hid the authors so the ranking measured the argument;
/// the chairman is writing the answer and needs to know that the two agreeing responses came from
/// two different models rather than from one model asked twice.
pub fn stage3_prompt(
    question: &str,
    responses: &BTreeMap<usize, String>,
    leaderboard: &[LeaderboardEntry],
) -> String {
    let mut prompt = format!("Question:\n{question}\n\nValid council responses:\n");
    if responses.is_empty() {
        // Said rather than left blank. A chairman handed an empty section writes a synthesis of
        // nothing and presents it as an answer; one told that every seat failed reports that, which
        // is the true and useful thing to say.
        prompt.push_str("No seat produced a valid response.\n\n");
    } else {
        for (seat_idx, answer) in responses {
            prompt.push_str(&format!("Seat {seat_idx}:\n{answer}\n\n"));
        }
    }
    let board = serde_json::to_string_pretty(leaderboard).unwrap_or_else(|_| "[]".to_string());
    prompt.push_str(&format!(
        "Average-rank leaderboard:\n{board}\n\nSynthesize one final chairman answer.\n"
    ));
    prompt
}

/// The council's settings, resolved once at startup.
///
/// `None` inside is the shipped state and means there is no council: `.ai/council.yaml` is absent,
/// unreadable, or names a roster the daemon will not run. Held as one field on `AppState` for the
/// same reason `voice` and `web` are — read together, switched on together, and a `Default` that
/// means "off" so no test that ignores councils has to know this exists.
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
    pub stage1_run_id: Option<i64>,
    pub stage1_status: String,
    pub stage1_error: Option<String>,
    pub stage2_run_id: Option<i64>,
    pub stage2_status: String,
    pub stage2_error: Option<String>,
    /// The JSON as stored. Parsed on the way out to the client, not here, so a row written by a
    /// future version with a field this one does not know about still reads.
    pub rankings: Option<String>,
}

/// One council as it is stored and read back, without its seats.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct CouncilRow {
    pub id: String,
    pub created_at: String,
    pub question: String,
    pub status: String,
    pub stage: i64,
    pub anon_seed: String,
    pub anon_map: Option<String>,
    pub leaderboard: Option<String>,
    pub chairman_kind: String,
    pub chairman_ref: String,
    pub chairman_agent_id: Option<String>,
    pub chairman_run_id: Option<i64>,
    pub error: Option<String>,
}

/// Writes a council and its seats in one transaction.
///
/// One transaction and not two statements, because a council row with no seats is a deliberation
/// nothing will ever drive: `run_council` reads its roster back from `council_seats`, so a crash
/// between the two writes would leave a row stuck at `running` for the reconciliation to find and
/// nothing else.
pub async fn insert_council(
    pool: &sqlx::SqlitePool,
    id: &str,
    question: &str,
    chairman: &CouncilSeat,
    members: &[CouncilSeat],
) -> sqlx::Result<()> {
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "INSERT INTO council_runs
           (id, created_at, question, status, stage, anon_seed, chairman_kind, chairman_ref,
            chairman_agent_id)
         VALUES (?, ?, ?, ?, 1, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(question)
    .bind(STATUS_RUNNING)
    // The council's own id. Stored again under its own name so the shuffle stays recomputable even
    // if what the seed is derived from ever changes.
    .bind(id)
    .bind(chairman.kind.as_db_str())
    // The MODEL, beside the agent and not instead of it. An agent is editable and deletable; what
    // answered in March has to keep reading as what answered in March.
    .bind(&chairman.model_ref)
    .bind(chairman.agent_id())
    .execute(&mut *transaction)
    .await?;

    for (seat_idx, seat) in members.iter().enumerate() {
        sqlx::query(
            "INSERT INTO council_seats
               (council_id, seat_idx, kind, model_ref, agent_id, stage1_status, stage2_status)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(seat_idx as i64)
        .bind(seat.kind.as_db_str())
        .bind(&seat.model_ref)
        .bind(seat.agent_id())
        // Written rather than left to the column's DEFAULT, so the constant this module reads back
        // and the value the database writes cannot drift apart.
        .bind(SEAT_PENDING)
        .bind(SEAT_PENDING)
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

pub async fn get_seat_rows(pool: &sqlx::SqlitePool, id: &str) -> sqlx::Result<Vec<SeatRow>> {
    sqlx::query_as::<_, SeatRow>(
        "SELECT seat_idx, kind, model_ref, agent_id, stage1_run_id, stage1_status, stage1_error,
                stage2_run_id, stage2_status, stage2_error, rankings
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

/// Moves the council to a phase. Guarded on `running`, so a cancel that landed first is not undone
/// by a phase boundary crossed a moment later.
pub async fn set_stage(pool: &sqlx::SqlitePool, id: &str, stage: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE council_runs SET stage = ? WHERE id = ? AND status = ?")
        .bind(stage)
        .bind(id)
        .bind(STATUS_RUNNING)
        .execute(pool)
        .await
        .map(|_| ())
}

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

/// Records the phase-2 leaderboard.
pub async fn set_leaderboard(
    pool: &sqlx::SqlitePool,
    id: &str,
    leaderboard: &[LeaderboardEntry],
) -> sqlx::Result<()> {
    let encoded = serde_json::to_string(leaderboard).unwrap_or_else(|_| "[]".to_string());
    sqlx::query("UPDATE council_runs SET leaderboard = ? WHERE id = ?")
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

/// Records how one seat's phase 1 ended.
pub async fn set_stage1(
    pool: &sqlx::SqlitePool,
    id: &str,
    seat_idx: usize,
    run_id: Option<i64>,
    status: &str,
    error: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE council_seats SET stage1_run_id = ?, stage1_status = ?, stage1_error = ?
         WHERE council_id = ? AND seat_idx = ?",
    )
    .bind(run_id)
    .bind(status)
    .bind(error)
    .bind(id)
    .bind(seat_idx as i64)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Records how one seat's phase 2 ended, and what it voted.
pub async fn set_stage2(
    pool: &sqlx::SqlitePool,
    id: &str,
    seat_idx: usize,
    run_id: Option<i64>,
    status: &str,
    error: Option<&str>,
    rankings: Option<&[Ranking]>,
) -> sqlx::Result<()> {
    let encoded = rankings.map(|ranked| serde_json::to_string(ranked).unwrap_or_default());
    sqlx::query(
        "UPDATE council_seats SET stage2_run_id = ?, stage2_status = ?, stage2_error = ?,
                rankings = ?
         WHERE council_id = ? AND seat_idx = ?",
    )
    .bind(run_id)
    .bind(status)
    .bind(error)
    .bind(encoded)
    .bind(id)
    .bind(seat_idx as i64)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Why a council could not be started. Every variant is a refusal BEFORE anything is spent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// `.ai/council.yaml` is absent, unreadable, or names a roster the daemon will not run.
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
    /// Minting or storing failed and the council would run with a key that authenticates nothing.
    Unavailable(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::NotConfigured => write!(
                formatter,
                "no council is configured — write a roster to .ai/council.yaml and restart"
            ),
            StartError::NoLocalModel => write!(
                formatter,
                "the roster asks for a local seat and no local model is configured"
            ),
            StartError::Invalid(reason) => write!(formatter, "{reason}"),
            StartError::BudgetExhausted(reason) => write!(formatter, "{reason}"),
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
/// costs the owner one clear error and nothing else. It is the same cost `.ai/autopilot.yaml`
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

    if let crate::budget::BudgetDecision::Pause { reason, .. } =
        crate::budget::budget_permits_new_run(&state.pool, chrono::Utc::now()).await
    {
        return Err(StartError::BudgetExhausted(reason));
    }

    let id = crate::auth::generate_uuid_v4();
    insert_council(&state.pool, &id, question, &chairman, &members)
        .await
        .map_err(|error| StartError::Unavailable(error.to_string()))?;

    let mcp_path = mcp_config_path(&id);
    let exe = std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| StartError::Unavailable(error.to_string()))?;
    crate::storage::write_atomic(
        &mcp_path,
        &serde_json::to_vec(&crate::assistant::build_mcp_config(&exe, None))
            .map_err(|error| StartError::Unavailable(error.to_string()))?,
    )
    .map_err(|error| StartError::Unavailable(error.to_string()))?;

    let _ = crate::feed::append(
        &state.pool,
        None,
        "council_started",
        &format!("council convened with {} seats", members.len()),
        None,
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
    };
    tokio::spawn(async move {
        let _guard = guard;
        driver.run().await;
    });

    Ok(id)
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
}

/// Which column of the record a seat's state belongs in.
///
/// An enum rather than a stage number plus an optional index, because the three cases carry
/// different data: phase 2 needs the labels the seat was shown in order to filter its vote, and the
/// chairman has no seat row at all.
enum Slot {
    Stage1(usize),
    /// The viewer, and the labels it was shown — the filter `parse_rankings` applies.
    Stage2(usize, Vec<String>),
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
        let (answers, anon) = self.stage1().await;
        if self.was_settled().await {
            return;
        }

        let leaderboard = self.stage2(&answers, &anon).await;
        if self.was_settled().await {
            return;
        }

        self.stage3(&answers, &leaderboard).await;
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

    /// Phase 1 — every seat answers the owner's question, all at once.
    ///
    /// Returns the valid answers by `seat_idx` and the shuffle over them.
    async fn stage1(&self) -> (BTreeMap<usize, String>, Anonymized) {
        let running = self.members.iter().enumerate().map(|(seat_idx, seat)| {
            let prompt = stage1_prompt(&self.question);
            // With tools: this is the phase where a seat goes and finds what it needs. Phases 2 and
            // 3 get none, so nobody can go looking for ammunition after seeing a peer's answer.
            async move {
                let outcome = self
                    .run_seat(Slot::Stage1(seat_idx), seat, prompt, true)
                    .await;
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

        let valid: Vec<usize> = answers.keys().copied().collect();
        let anon = anonymize(&self.id, &valid);
        if let Err(error) = set_anon_map(&self.state.pool, &self.id, &anon.anon_map).await {
            tracing::warn!(council = %self.id, %error, "could not record the anonymisation map");
        }
        let _ = crate::feed::append(
            &self.state.pool,
            None,
            "council_stage",
            &format!(
                "council phase 1 done: {} of {} seats answered",
                answers.len(),
                self.members.len()
            ),
            None,
        )
        .await;

        (answers, anon)
    }

    /// Phase 2 — each seat orders its peers' answers under shuffled labels.
    ///
    /// Skipped whole below two valid answers, and skipping is recorded rather than left blank: a
    /// seat with `stage2_status = 'skipped'` and one with `'pending'` mean different things, and
    /// only one of them is a council that stopped.
    async fn stage2(
        &self,
        answers: &BTreeMap<usize, String>,
        anon: &Anonymized,
    ) -> Vec<LeaderboardEntry> {
        if !stage2_should_run(answers.len()) {
            for seat_idx in 0..self.members.len() {
                let _ = set_stage2(
                    &self.state.pool,
                    &self.id,
                    seat_idx,
                    None,
                    SEAT_SKIPPED,
                    None,
                    None,
                )
                .await;
            }
            return Vec::new();
        }

        if let Err(error) = set_stage(&self.state.pool, &self.id, 2).await {
            tracing::warn!(council = %self.id, %error, "could not advance the council to phase 2");
        }

        let ranking = anon.for_seat.iter().filter_map(|(viewer, labels)| {
            let seat = self.members.get(*viewer)?;
            let shown: Vec<(String, String)> = labels
                .iter()
                .filter_map(|label| {
                    let seat_idx = anon.anon_map.get(label)?;
                    Some((label.clone(), answers.get(seat_idx)?.clone()))
                })
                .collect();
            let allowed: Vec<String> = shown.iter().map(|(label, _)| label.clone()).collect();
            let prompt = stage2_prompt(&self.question, &shown);
            Some(async move {
                // No tools. A seat that could fetch targeted evidence after reading its peers'
                // answers would turn the ranking into a measure of who had time left.
                let outcome = self
                    .run_seat(Slot::Stage2(*viewer, allowed.clone()), seat, prompt, false)
                    .await;
                (*viewer, allowed, outcome)
            })
        });
        let outcomes = crate::join::all(ranking).await;

        let mut votes: BTreeMap<usize, Vec<Ranking>> = BTreeMap::new();
        for (seat_idx, allowed, outcome) in outcomes {
            if outcome.status == SEAT_OK {
                votes.insert(seat_idx, parse_rankings(&outcome.answer, &allowed));
            }
        }

        let leaderboard = aggregate_rankings(&votes, &anon.anon_map);
        if let Err(error) = set_leaderboard(&self.state.pool, &self.id, &leaderboard).await {
            tracing::warn!(council = %self.id, %error, "could not record the leaderboard");
        }
        let _ = crate::feed::append(
            &self.state.pool,
            None,
            "council_stage",
            &format!("council phase 2 done: {} seats ranked", leaderboard.len()),
            None,
        )
        .await;

        leaderboard
    }

    /// Phase 3 — the chairman writes the answer.
    ///
    /// The failure here is the one that settles the council as `error`, and it is survivable in the
    /// way the others are not: phases 1 and 2 are still readable, and they are the part with value.
    /// The answers and the ranking are worth having without the synthesis; the synthesis is not
    /// worth having without them.
    async fn stage3(&self, answers: &BTreeMap<usize, String>, leaderboard: &[LeaderboardEntry]) {
        if let Err(error) = set_stage(&self.state.pool, &self.id, 3).await {
            tracing::warn!(council = %self.id, %error, "could not advance the council to phase 3");
        }

        let prompt = stage3_prompt(&self.question, answers, leaderboard);
        let outcome = self
            .run_seat(Slot::Chairman, &self.chairman, prompt, false)
            .await;

        let (status, error) = if outcome.status == SEAT_OK {
            (STATUS_DONE, None)
        } else if outcome.status == SEAT_CANCELLED {
            (STATUS_CANCELLED, None)
        } else {
            (
                STATUS_ERROR,
                Some(
                    outcome
                        .error
                        .unwrap_or_else(|| "the chairman produced no synthesis".to_string()),
                ),
            )
        };
        match finish(&self.state.pool, &self.id, status, error.as_deref()).await {
            Ok(true) => {
                let _ = crate::feed::append(
                    &self.state.pool,
                    None,
                    "council_finished",
                    &format!("council {status}"),
                    None,
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
        self.record(&slot, Some(run_id), &outcome).await;
        outcome
    }

    /// Writes one seat's state into the column that belongs to its phase.
    async fn record(&self, slot: &Slot, run_id: Option<i64>, outcome: &SeatOutcome) {
        let written = match slot {
            Slot::Stage1(seat_idx) => {
                set_stage1(
                    &self.state.pool,
                    &self.id,
                    *seat_idx,
                    run_id,
                    outcome.status,
                    outcome.error.as_deref(),
                )
                .await
            }
            Slot::Stage2(seat_idx, allowed) => {
                // The vote is parsed here rather than by the caller, so the rankings land in the
                // same write as the status and a reader never sees `ok` with no vote beside it.
                //
                // An unreadable ranking is a BLANK vote and not an error: the seat answered, and
                // what it wrote had no ordering in it. Recording that as a failure would make a
                // formatting slip indistinguishable from a model that refused.
                let rankings =
                    (outcome.status == SEAT_OK).then(|| parse_rankings(&outcome.answer, allowed));
                set_stage2(
                    &self.state.pool,
                    &self.id,
                    *seat_idx,
                    run_id,
                    outcome.status,
                    outcome.error.as_deref(),
                    rankings.as_deref(),
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
        let request = crate::runner::RunRequest {
            prompt,
            // The council's OWN key, never `state.token`. `auth::COUNCIL_ROUTES` is what it reaches.
            env: crate::runs::run_env(&self.token, run_id, None),
            cwd: None,
            permission: crate::runner::Permission::Default,
            resume_session_id: None,
            mcp_config: with_tools.then(|| mcp_config_path(&self.id)),
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
                                cache_read_tokens = ?, num_turns = ?, completed_at = ?
                         WHERE id = ? AND status = 'running'",
                    )
                    .bind(&run.stdout)
                    .bind(run.cost_usd)
                    .bind(run.input_tokens)
                    .bind(run.output_tokens)
                    .bind(run.cache_read_tokens)
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
                    let _ = sqlx::query(
                        "UPDATE runs SET status = 'failed', exit_code = ?, stdout = ?, stderr = ?,
                                cost_usd = ?, completed_at = ?
                         WHERE id = ? AND status = 'running'",
                    )
                    .bind(run.exit_code)
                    .bind(&run.stdout)
                    .bind(&run.stderr)
                    .bind(run.cost_usd)
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
        let chat = crate::runner::OllamaChat::new(
            crate::runner::OLLAMA_BASE_URL.to_string(),
            seat.model_ref.clone(),
        );
        // The council's list, not the chat's: `LOCAL_TOOLS` carries `create_run` and `create_job`,
        // which is exactly what a seat must not have. Phases 2 and 3 get an empty box — no tools is
        // no tools whichever machine answers.
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
                    &chat,
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

    let seats = get_seat_rows(&state.pool, id).await?;
    let chairman_run = get_council_row(&state.pool, id)
        .await?
        .and_then(|row| row.chairman_run_id);
    let mut opened: Vec<i64> = Vec::new();
    for seat in &seats {
        opened.extend(seat.stage1_run_id);
        opened.extend(seat.stage2_run_id);
    }
    opened.extend(chairman_run);
    for run_id in opened {
        // `finalize_termination` is the atomic-handle arbiter: it is a no-op for a run that has
        // already ended, so calling it for every run this council ever opened is correct as well as
        // simple.
        crate::runs::finalize_termination(state, run_id, "cancelled").await;
    }

    if settled {
        let _ = crate::feed::append(
            &state.pool,
            None,
            "council_finished",
            "council cancelled",
            None,
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
/// and deleting it would leave `set_stage` and `finish` updating nothing while the seats went on
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

/// One seat as a client sees it: the record, plus the answer read out of its run.
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
    pub stage1_status: String,
    pub stage1_error: Option<String>,
    /// The text the seat wrote, read from the transcript of the run that produced it. The client is
    /// never told a `runs` table exists.
    pub answer: Option<String>,
    pub stage2_status: String,
    pub stage2_error: Option<String>,
    pub rankings: Vec<Ranking>,
}

#[derive(Serialize)]
pub struct CouncilView {
    pub id: String,
    pub created_at: String,
    pub question: String,
    pub status: String,
    pub stage: i64,
    pub error: Option<String>,
    pub chairman_kind: String,
    #[serde(rename = "chairman_ref")]
    pub chairman_ref: String,
    pub chairman_agent_id: Option<String>,
    pub chairman_agent_name: Option<String>,
    /// The synthesis, once phase 3 has produced one.
    pub synthesis: Option<String>,
    pub anon_map: BTreeMap<String, usize>,
    pub leaderboard: Vec<LeaderboardEntry>,
    pub seats: Vec<SeatView>,
}

/// One council row without its seats, for the list.
#[derive(Debug, Serialize)]
pub struct CouncilSummary {
    pub id: String,
    pub created_at: String,
    pub question: String,
    pub status: String,
    pub stage: i64,
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
        Err(error @ StartError::BudgetExhausted(_)) => {
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
    let row = get_council_row(&state.pool, &id)
        .await
        .map_err(internal)?
        .ok_or((
            axum::http::StatusCode::NOT_FOUND,
            "no such council".to_string(),
        ))?;
    let seats = get_seat_rows(&state.pool, &id).await.map_err(internal)?;

    let mut answers = BTreeMap::new();
    for run_id in seats.iter().filter_map(|seat| seat.stage1_run_id) {
        if let Some(text) = transcript_of(&state.pool, run_id).await {
            answers.insert(run_id, text);
        }
    }
    let synthesis = match row.chairman_run_id {
        Some(run_id) => transcript_of(&state.pool, run_id).await,
        None => None,
    };

    // One read of a table that holds a handful of rows, rather than one lookup per seat. An agent
    // the roster named and somebody has since deleted is simply absent from the map, and its seat
    // shows the id it pointed at with no name beside it — which is the honest rendering of what the
    // record actually says.
    let names: BTreeMap<String, String> = crate::agent::list(&state.pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|agent| (agent.id, agent.name))
        .collect();

    Ok(axum::Json(CouncilView {
        seats: seats
            .into_iter()
            .map(|seat| SeatView {
                answer: seat
                    .stage1_run_id
                    .and_then(|run_id| answers.get(&run_id).cloned()),
                // A `rankings` column that will not parse becomes an empty vote rather than a 500.
                // The record is worth reading even when one of its JSON columns is not.
                rankings: seat
                    .rankings
                    .as_deref()
                    .and_then(|text| serde_json::from_str(text).ok())
                    .unwrap_or_default(),
                seat_idx: seat.seat_idx,
                kind: seat.kind,
                model_ref: seat.model_ref,
                agent_name: seat
                    .agent_id
                    .as_deref()
                    .and_then(|id| names.get(id).cloned()),
                agent_id: seat.agent_id,
                stage1_status: seat.stage1_status,
                stage1_error: seat.stage1_error,
                stage2_status: seat.stage2_status,
                stage2_error: seat.stage2_error,
            })
            .collect(),
        anon_map: row
            .anon_map
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_default(),
        leaderboard: row
            .leaderboard
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_default(),
        synthesis,
        id: row.id,
        created_at: row.created_at,
        question: row.question,
        status: row.status,
        stage: row.stage,
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
                stage: row.stage,
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

    /// Why the leaderboard averages instead of summing.
    ///
    /// Seat 0 is ranked first by both of the peers that voted; seat 1 is ranked first by the one
    /// peer that voted on it. Summing would put seat 0 at 2 and seat 1 at 1 and call seat 1 better,
    /// purely for having been seen less. Averaging calls them equal, which is what the votes say.
    #[test]
    fn average_rank_compares_under_partial_participation() {
        let anon_map: BTreeMap<String, usize> = [("A".to_string(), 0), ("B".to_string(), 1)]
            .into_iter()
            .collect();
        let votes: BTreeMap<usize, Vec<Ranking>> = [
            (
                1,
                vec![Ranking {
                    anon: "A".into(),
                    rank: 1,
                }],
            ),
            (
                2,
                vec![
                    Ranking {
                        anon: "A".into(),
                        rank: 1,
                    },
                    Ranking {
                        anon: "B".into(),
                        rank: 1,
                    },
                ],
            ),
        ]
        .into_iter()
        .collect();

        let board = aggregate_rankings(&votes, &anon_map);
        assert_eq!(board.len(), 2);
        assert_eq!(board[0].avg_rank, 1.0);
        assert_eq!(board[1].avg_rank, 1.0);
        // The counts are what say the two averages are not equally well supported.
        let by_seat: BTreeMap<usize, usize> = board
            .iter()
            .map(|entry| (entry.seat_idx, entry.n))
            .collect();
        assert_eq!(by_seat[&0], 2);
        assert_eq!(by_seat[&1], 1);
    }

    /// A total order, so the same council read twice reads the same. Without the tie-break the
    /// order would come from a hash map and change between runs.
    #[test]
    fn leaderboard_ties_break_by_seat_idx() {
        let anon_map: BTreeMap<String, usize> = [
            ("A".to_string(), 7),
            ("B".to_string(), 2),
            ("C".to_string(), 5),
        ]
        .into_iter()
        .collect();
        let votes: BTreeMap<usize, Vec<Ranking>> = [(
            0,
            vec![
                Ranking {
                    anon: "A".into(),
                    rank: 2,
                },
                Ranking {
                    anon: "B".into(),
                    rank: 2,
                },
                Ranking {
                    anon: "C".into(),
                    rank: 2,
                },
            ],
        )]
        .into_iter()
        .collect();

        let board = aggregate_rankings(&votes, &anon_map);
        assert_eq!(
            board.iter().map(|entry| entry.seat_idx).collect::<Vec<_>>(),
            vec![2, 5, 7]
        );
    }

    /// A label the council never issued is dropped rather than crashing the aggregation. The map
    /// and the votes are read from two different columns, and a row edited by hand is not a reason
    /// for the daemon to fall over.
    #[test]
    fn a_vote_for_a_label_that_does_not_exist_is_dropped() {
        let anon_map: BTreeMap<String, usize> = [("A".to_string(), 0)].into_iter().collect();
        let votes: BTreeMap<usize, Vec<Ranking>> = [(
            1,
            vec![
                Ranking {
                    anon: "A".into(),
                    rank: 1,
                },
                Ranking {
                    anon: "Z".into(),
                    rank: 1,
                },
            ],
        )]
        .into_iter()
        .collect();

        let board = aggregate_rankings(&votes, &anon_map);
        assert_eq!(board.len(), 1);
        assert_eq!(board[0].seat_idx, 0);
    }

    fn allowed(labels: &[&str]) -> Vec<String> {
        labels.iter().map(|label| (*label).to_string()).collect()
    }

    /// The formats a model actually writes. Requiring one of them would turn a formatting slip into
    /// a lost vote, which is the failure this parser exists to avoid.
    #[test]
    fn parse_rankings_reads_the_accepted_shapes() {
        let allowed = allowed(&["A", "B", "C", "D"]);

        assert_eq!(
            parse_rankings("A: 1\nB: 2\n", &allowed),
            vec![
                Ranking {
                    anon: "A".into(),
                    rank: 1
                },
                Ranking {
                    anon: "B".into(),
                    rank: 2
                }
            ]
        );
        assert_eq!(
            parse_rankings("Response B: 2", &allowed),
            vec![Ranking {
                anon: "B".into(),
                rank: 2
            }]
        );
        assert_eq!(
            parse_rankings("C) 3", &allowed),
            vec![Ranking {
                anon: "C".into(),
                rank: 3
            }]
        );
        assert_eq!(
            parse_rankings("D - 4", &allowed),
            vec![Ranking {
                anon: "D".into(),
                rank: 4
            }]
        );
        assert_eq!(
            parse_rankings("A.1", &allowed),
            vec![Ranking {
                anon: "A".into(),
                rank: 1
            }]
        );

        // Prose around the ordering is the normal case, not the exception.
        assert_eq!(
            parse_rankings(
                "Thinking it over, the strongest was A: 1, and B: 2 came close behind.",
                &allowed
            ),
            vec![
                Ranking {
                    anon: "A".into(),
                    rank: 1
                },
                Ranking {
                    anon: "B".into(),
                    rank: 2
                }
            ]
        );

        // A reply with no ordering in it is a blank vote and not a failure.
        assert!(parse_rankings("They were all about the same.", &allowed).is_empty());
        assert!(parse_rankings("", &allowed).is_empty());
    }

    /// A model that invents a response it was never shown would otherwise bias the leaderboard, and
    /// inventing structure is exactly what a model asked for structure does when it has none.
    #[test]
    fn parse_rankings_discards_labels_the_seat_never_saw() {
        let allowed = allowed(&["A", "B"]);
        assert_eq!(
            parse_rankings("A: 1\nB: 2\nC: 3\nZ: 4\n", &allowed),
            vec![
                Ranking {
                    anon: "A".into(),
                    rank: 1
                },
                Ranking {
                    anon: "B".into(),
                    rank: 2
                }
            ]
        );
    }

    /// A seat that contradicts itself does not get to vote twice.
    #[test]
    fn parse_rankings_takes_the_first_mention_of_a_label() {
        let allowed = allowed(&["A", "B"]);
        assert_eq!(
            parse_rankings("A: 1\nB: 2\nOn reflection, A: 3.", &allowed),
            vec![
                Ranking {
                    anon: "A".into(),
                    rank: 1
                },
                Ranking {
                    anon: "B".into(),
                    rank: 2
                }
            ]
        );
    }

    /// The boundaries, which are what stop ordinary prose from reading as a vote.
    #[test]
    fn parse_rankings_respects_word_boundaries() {
        let allowed = allowed(&["A", "B", "C"]);
        // A capital inside a word names nothing.
        assert!(parse_rankings("partB: 1", &allowed).is_empty());
        // Nor does a rank that runs into a word.
        assert!(parse_rankings("A: 12x", &allowed).is_empty());
        // `ResponseA` has no space, so neither branch of the pattern matches.
        assert!(parse_rankings("ResponseA: 1", &allowed).is_empty());
        // Lowercase is not a label.
        assert!(parse_rankings("a: 1", &allowed).is_empty());
        // Two pairs on one line are two pairs.
        assert_eq!(parse_rankings("A: 1 B: 2 C: 3", &allowed).len(), 3);
    }

    /// With fewer than two answers there is nothing to compare, and the phase would ask a seat to
    /// order an empty set.
    #[test]
    fn stage2_is_skipped_below_two_valid_responses() {
        assert!(!stage2_should_run(0));
        assert!(!stage2_should_run(1));
        assert!(stage2_should_run(2));
        assert!(stage2_should_run(8));
    }

    /// The chairman must be told that every seat failed, rather than handed an empty section it
    /// will synthesise something confident out of.
    #[test]
    fn the_chairman_is_told_when_no_seat_answered() {
        let prompt = stage3_prompt("why?", &BTreeMap::new(), &[]);
        assert!(prompt.contains("No seat produced a valid response."));
        assert!(prompt.contains("Synthesize one final chairman answer."));
    }

    /// The chairman sees who said what; phase 2 did not. Two agreeing answers from two models is a
    /// different fact from one model asked twice, and only the chairman needs it.
    #[test]
    fn the_chairman_sees_the_seats_by_name_and_the_ranker_does_not() {
        let responses: BTreeMap<usize, String> =
            [(0, "first".to_string()), (2, "third".to_string())]
                .into_iter()
                .collect();
        let chairman = stage3_prompt("why?", &responses, &[]);
        assert!(chairman.contains("Seat 0:"));
        assert!(chairman.contains("Seat 2:"));

        let ranker = stage2_prompt(
            "why?",
            &[
                ("A".to_string(), "first".to_string()),
                ("B".to_string(), "third".to_string()),
            ],
        );
        assert!(ranker.contains("Response A:"));
        assert!(!ranker.contains("Seat "));
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
        )
        .await
        .unwrap();

        let row = get_council_row(&pool, "c1").await.unwrap().unwrap();
        assert_eq!(row.status, STATUS_RUNNING);
        assert_eq!(row.stage, 1);
        assert_eq!(row.anon_seed, "c1", "the seed is the council's own id");
        assert_eq!(row.chairman_ref, "claude-opus-4-8");
        assert_eq!(row.anon_map, None);
        assert_eq!(row.leaderboard, None);

        let seats = get_seat_rows(&pool, "c1").await.unwrap();
        assert_eq!(seats.len(), 2);
        assert_eq!(seats[0].seat_idx, 0);
        assert_eq!(seats[1].kind, "local");
        assert_eq!(seats[1].model_ref, "qwen3.5:4b");
        assert_eq!(seats[0].stage1_status, SEAT_PENDING);

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
        )
        .await
        .unwrap();
        finish(&pool, "c1", STATUS_CANCELLED, None).await.unwrap();

        set_stage(&pool, "c1", 3).await.unwrap();
        assert_eq!(
            get_council_row(&pool, "c1").await.unwrap().unwrap().stage,
            1
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

    #[tokio::test]
    async fn a_seats_phases_are_recorded_independently() {
        let pool = test_pool().await;
        insert_council(
            &pool,
            "c1",
            "why?",
            &seat(SeatKind::Cloud, "m"),
            &[seat(SeatKind::Cloud, "m"), seat(SeatKind::Cloud, "m")],
        )
        .await
        .unwrap();

        set_stage1(&pool, "c1", 0, Some(11), SEAT_OK, None)
            .await
            .unwrap();
        set_stage1(&pool, "c1", 1, Some(12), SEAT_TIMEOUT, Some("wall clock"))
            .await
            .unwrap();
        set_stage2(
            &pool,
            "c1",
            0,
            Some(21),
            SEAT_OK,
            None,
            Some(&[Ranking {
                anon: "B".into(),
                rank: 1,
            }]),
        )
        .await
        .unwrap();
        set_stage2(&pool, "c1", 1, None, SEAT_SKIPPED, None, None)
            .await
            .unwrap();

        let seats = get_seat_rows(&pool, "c1").await.unwrap();
        assert_eq!(seats[0].stage1_status, SEAT_OK);
        assert_eq!(seats[0].stage1_run_id, Some(11));
        // A seat that ran out of clock is not a seat that refused, and the two must stay legible
        // apart in the record.
        assert_eq!(seats[1].stage1_status, SEAT_TIMEOUT);
        assert_eq!(seats[1].stage1_error.as_deref(), Some("wall clock"));
        assert_eq!(
            seats[0].rankings.as_deref(),
            Some(r#"[{"anon":"B","rank":1}]"#)
        );
        assert_eq!(seats[1].stage2_status, SEAT_SKIPPED);
        assert_eq!(seats[1].rankings, None);

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
        /// The launch itself failed — no work done, nothing spent.
        WillNotLaunch(String),
        /// Never returns, so the seat's own wall clock is what ends it.
        Hangs,
    }

    #[derive(Default)]
    struct ScriptedRunner {
        stage1: std::sync::Mutex<std::collections::VecDeque<Scripted>>,
        stage2: std::sync::Mutex<std::collections::VecDeque<Scripted>>,
        chairman: std::sync::Mutex<Option<Scripted>>,
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
            if prompt.contains("Rank the anonymous peer responses") {
                self.stage2
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Scripted::Answers(String::new()))
            } else if prompt.contains("Synthesize one final chairman answer") {
                self.chairman
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or(Scripted::Answers("the synthesis".to_string()))
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
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: std::sync::Arc::new(crate::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
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
        *runner.stage2.lock().unwrap() = [
            Scripted::Answers("A: 1\nB: 2".into()),
            Scripted::Answers("A: 1\nB: 2".into()),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start(&state, "why?", None).await.unwrap();
        let row = settled(&state, &id).await;

        assert_eq!(row.status, STATUS_DONE);
        assert_eq!(row.stage, 3);
        let seats = get_seat_rows(&state.pool, &id).await.unwrap();
        assert_eq!(seats[0].stage1_status, SEAT_OK);
        assert_eq!(seats[1].stage1_status, SEAT_ERROR);
        assert_eq!(seats[1].stage1_error.as_deref(), Some("the model refused"));
        assert_eq!(seats[2].stage1_status, SEAT_OK);

        // Two valid answers, so phase 2 ran — and only for the seats that had one to be ranked
        // against, which is the same set.
        assert_eq!(seats[0].stage2_status, SEAT_OK);
        assert_eq!(seats[2].stage2_status, SEAT_OK);
        // The failed seat is not in the shuffle, so it was never shown anything and never asked.
        assert_eq!(seats[1].stage2_status, SEAT_PENDING);

        let anon: BTreeMap<String, usize> =
            serde_json::from_str(row.anon_map.as_deref().unwrap()).unwrap();
        assert_eq!(anon.len(), 2);
        assert!(!anon.values().any(|seat| *seat == 1));
        assert!(row.chairman_run_id.is_some());
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
        // The leaderboard is written as an empty list rather than left NULL, because "phase 2 ran
        // and nobody was ranked" and "phase 2 never ran" are different, and only the second is this.
        assert_eq!(row.leaderboard, None);
        for seat in get_seat_rows(&state.pool, &id).await.unwrap() {
            assert_eq!(seat.stage2_status, SEAT_SKIPPED);
        }
        assert!(
            runner
                .seen
                .lock()
                .unwrap()
                .iter()
                .all(|request| !request.prompt.contains("Rank the anonymous peer"))
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
        *runner.chairman.lock().unwrap() = Some(Scripted::Answers("nobody answered".into()));
        let state = council_state(runner.clone(), Some(roster(2))).await;

        let id = start(&state, "why?", None).await.unwrap();
        let row = settled(&state, &id).await;

        assert_eq!(row.status, STATUS_DONE);
        let chairman_prompt = runner
            .seen
            .lock()
            .unwrap()
            .iter()
            .find(|request| {
                request
                    .prompt
                    .contains("Synthesize one final chairman answer")
            })
            .map(|request| request.prompt.clone())
            .expect("the chairman ran");
        assert!(chairman_prompt.contains("No seat produced a valid response."));
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

        let seats = get_seat_rows(&state.pool, &id).await.unwrap();
        assert_eq!(seats[0].stage1_status, SEAT_TIMEOUT);
        assert_eq!(seats[1].stage1_status, SEAT_ERROR);
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
        // Phase 2 never started.
        assert_eq!(row.stage, 1);

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

    /// One `runs` row per seat, whichever machine answers it. The row is what carries cost,
    /// cancellation and orphan reconciliation, and a seat that skipped it would need all three
    /// written again.
    #[tokio::test]
    async fn a_mixed_roster_lands_one_run_per_seat() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [Scripted::Answers("from the cloud".into())].into();
        let config = CouncilConfig {
            timeout_seconds: 1,
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
        assert!(seats[0].stage1_run_id.is_some());
        assert!(
            seats[1].stage1_run_id.is_some(),
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
                .filter(|request| !request.prompt.contains("Synthesize"))
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
        *runner.stage2.lock().unwrap() = [
            Scripted::Answers("A: 1".into()),
            Scripted::Answers("A: 1".into()),
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
            request.prompt.contains("Rank the anonymous peer")
                || request.prompt.contains("Synthesize one final")
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
        *runner.stage2.lock().unwrap() = [
            Scripted::Answers("A: 1".into()),
            Scripted::Answers("A: 1".into()),
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
        )
        .await
        .unwrap();
        insert_council(
            &pool,
            "c2",
            "why?",
            &seat(SeatKind::Cloud, "m"),
            &[seat(SeatKind::Cloud, "m")],
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
            let seats = get_seat_rows(&state.pool, &id).await.unwrap();
            if seats[0].stage1_status == SEAT_OK {
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
        assert_eq!(view.stage, 1);
        // The answer comes back as TEXT, read out of the run that produced it. Nothing in this
        // response tells the client a `runs` table exists.
        assert_eq!(view.seats[0].answer.as_deref(), Some("the first"));
        assert_eq!(view.seats[1].answer, None);
        assert!(view.leaderboard.is_empty());
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
    /// `.ai/council.yaml` is under `.ai/`, which is gitignored — it is per-developer configuration,
    /// not a fact of this repository. A form retired here would not error on the machines still
    /// using it; it would give them a council that silently stops existing at the next daemon
    /// start. So this asserts coexistence, not migration.
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
        *runner.stage2.lock().unwrap() = [
            Scripted::Answers("A: 1\nB: 2".into()),
            Scripted::Answers("A: 2\nB: 1".into()),
        ]
        .into();
        let state = council_state(
            runner.clone(),
            Some(CouncilConfig {
                timeout_seconds: 1,
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
        assert!(seen.len() >= 5, "two phase-1, two phase-2 and a chairman");
        for request in seen.iter() {
            for forbidden in [
                "Cetico",
                "Economista",
                "doubts the premise",
                "counts the money",
            ] {
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

        assert!(matches!(
            post_council_cancel(
                axum::extract::State(state.clone()),
                axum::extract::Path("no-such-council".to_string()),
            )
            .await,
            Err((axum::http::StatusCode::NOT_FOUND, _))
        ));
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
        assert!(message.contains(".ai/council.yaml"), "{message}");

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
}
