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

mod api;
mod driver;
pub mod formats;
mod launch;
pub mod prompts;
mod rows;
mod seat_run;
pub mod store;
pub mod tally;

pub use api::*;
use driver::*;
use launch::mcp_config_path;
pub use launch::*;
pub use rows::*;
use rows::{
    PHASE_CHAIRMAN, PHASE_DONE, SYNTHESIS_DEGRADED, SYNTHESIS_OK, agent_names,
    insert_council_with_roles, transcript_of,
};

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
    /// started. Between two critique rounds every seat revises; the early-stop rule may end the
    /// council before the last one.
    rounds: i64,
    /// The role each seat plays, by `seat_idx`; `None` for a plain seat.
    roles: Vec<Option<formats::Role>>,
    /// Runner launches made so far, held against `tally::call_ceiling`. The loop below is shaped so
    /// it never reaches the ceiling; this is the wall that keeps a future change to that loop from
    /// turning a bug into an unbounded bill.
    launched: std::sync::atomic::AtomicUsize,
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
        crate::execution::runs::finalize_termination(state, run_id, "cancelled").await;
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
pub fn retention_days() -> i64 {
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
        crate::testdb::fresh_pool().await
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
        /// exit — `crate::execution::runner::TURN_CEILING_EXIT_CODE` — carrying the completed turn's own
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
        tool_policy: crate::execution::runner::ToolPolicy,
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
    impl crate::execution::runner::CommandRunner for ScriptedRunner {
        async fn run_prompt(
            &self,
            request: crate::execution::runner::RunRequest,
            _session_tx: tokio::sync::mpsc::UnboundedSender<String>,
            _transcript: std::sync::Arc<std::sync::Mutex<String>>,
        ) -> std::io::Result<crate::execution::runner::RunOutcome> {
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

            let blank = crate::execution::runner::RunOutcome {
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
                Scripted::Answers(text) => Ok(crate::execution::runner::RunOutcome {
                    stdout: text,
                    ..blank
                }),
                Scripted::Fails(stderr) => Ok(crate::execution::runner::RunOutcome {
                    exit_code: 1,
                    stderr,
                    ..blank
                }),
                Scripted::DiesAtTheCeiling => Ok(crate::execution::runner::RunOutcome {
                    exit_code: crate::execution::runner::TURN_CEILING_EXIT_CODE,
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
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::integrations::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: std::sync::Arc::new(crate::integrations::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::integrations::web::WebRuntime::disabled()),
            quota: std::sync::Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: std::sync::Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::integrations::calendar::CalendarRuntime::default()),
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
        let inner: std::sync::Arc<dyn crate::execution::runner::CommandRunner> = runner.clone();
        let router = std::sync::Arc::new(crate::models::route_advice::Router::new(
            crate::models::route_advice::RouterConfig {
                mode: crate::models::route_advice::Mode::Off,
                url,
                surfaces: [("council", crate::models::route_advice::Mode::Shadow)]
                    .into_iter()
                    .collect(),
                ..crate::models::route_advice::RouterConfig::off()
            },
            crate::models::route_advice::Available {
                kind: crate::models::route_advice::RunnerKind::Claude,
                runner: inner.clone(),
                default_model: "claude-sonnet-5".into(),
                models: vec!["claude-*".into()],
            },
            Vec::new(),
        ));
        let mut state = council_state(runner.clone(), Some(roster(3))).await;
        state.runner =
            std::sync::Arc::new(crate::models::route_advice::RoutedRunner { inner, router });

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

        assert_eq!(
            exit_code,
            Some(crate::execution::runner::TURN_CEILING_EXIT_CODE)
        );
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
            assert_eq!(
                request.tool_policy,
                crate::execution::runner::ToolPolicy::None
            );
            assert_eq!(
                request.mcp_config, None,
                "a policy of None beside a config file would advertise a server the CLI may not reach"
            );
        }

        let answering = seen
            .iter()
            .find(|request| request.prompt == "why?")
            .expect("phase 1 asks the question as written");
        assert_eq!(
            answering.tool_policy,
            crate::execution::runner::ToolPolicy::McpOnly
        );
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
        crate::governance::budget::set_budget_config(
            &state.pool,
            &crate::governance::budget::BudgetConfig {
                limit_usd: Some(0.01),
                period: crate::governance::budget::BudgetPeriod::Monthly,
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
            crate::quota::client::QuotaClient::new(&address, "bearer".into()),
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
                rounds: None,
                roles: None,
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
                    rounds: None,
                    roles: None,
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
            matches!(
                seen[0].tool_policy,
                crate::execution::runner::ToolPolicy::None
            ),
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
                rounds: None,
                roles: None,
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
        ) -> Result<Box<dyn crate::models::local_agent::LocalChat>, crate::assistants::Refusal>
        {
            self.asked_for
                .lock()
                .expect("the double's recorder is never held across an await")
                .push(model.to_string());
            Ok(Box::new(
                crate::models::openai_compatible::OpenAiCompatibleChat::with_client(
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
        ) -> Result<
            std::sync::Arc<crate::models::local_agent::LocalAssistant>,
            crate::assistants::Refusal,
        > {
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
        // The legacy `leaderboard` column is gone (0155 drops it), so there is no stale copy left
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

    /// `agreement_by_round` and `ballot_rounds` walk the same rounds as `leaderboard_by_round`: one
    /// entry per critique round that cast a readable ballot, in round order, the last being
    /// `agreement`. A round whose critiques all failed to read has no entry, and `ballot_rounds`
    /// skips its number — that is how a client knows which round index i stands for.
    #[tokio::test]
    async fn view_reports_agreement_per_ballot_round() {
        let state = council_state(std::sync::Arc::new(ScriptedRunner::default()), None).await;
        let pool = &state.pool;
        bare_council(pool, "c1", 3, 3).await;
        set_anon_map(pool, "c1", &abc()).await.unwrap();

        let round_one: &[(usize, &[&str])] =
            &[(0, &["C", "B"]), (1, &["C", "A"]), (2, &["A", "B"])];
        let round_three: &[(usize, &[&str])] =
            &[(0, &["B", "C"]), (1, &["A", "C"]), (2, &["A", "B"])];
        for (round, votes) in [(1i64, round_one), (3i64, round_three)] {
            for (voter, labels) in votes {
                store::upsert_step(
                    pool,
                    "c1",
                    round,
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
        }
        // Round 2: every seat critiqued and none of it reads. No ballot, so no entry.
        for voter in 0..3i64 {
            store::upsert_step(
                pool,
                "c1",
                2,
                voter,
                store::PHASE_CRITIQUE,
                None,
                store::STEP_INVALID,
                Some("unreadable"),
                Some("not a ballot"),
            )
            .await
            .unwrap();
        }

        let view = view_of(&state, "c1").await;
        assert_eq!(view.ballot_rounds, vec![1, 3]);
        assert_eq!(
            view.agreement_by_round.len(),
            view.leaderboard_by_round.len()
        );
        assert_eq!(
            view.agreement_by_round,
            vec![
                tally::agreement(&ballots(round_one), &abc()),
                tally::agreement(&ballots(round_three), &abc()),
            ]
        );
        assert_eq!(view.agreement_by_round.last(), view.agreement.as_ref());

        // The wire names the shell reads.
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["ballot_rounds"], serde_json::json!([1, 3]));
        assert_eq!(json["agreement_by_round"].as_array().map(Vec::len), Some(2));
    }

    /// `synthesis_run_id` is the chairman's run, visible as soon as that run is attached — before
    /// it has written anything — so a client can tail the verdict live. Nothing attached, nothing
    /// served.
    #[tokio::test]
    async fn view_exposes_the_chairman_run_as_synthesis_run_id() {
        let state = council_state(std::sync::Arc::new(ScriptedRunner::default()), None).await;
        let pool = &state.pool;
        bare_council(pool, "c1", 3, 1).await;
        assert_eq!(view_of(&state, "c1").await.synthesis_run_id, None);

        let run = run_with_stdout(pool, "the verdict so far").await;
        set_chairman_run(pool, "c1", run).await.unwrap();
        let view = view_of(&state, "c1").await;
        assert_eq!(view.synthesis_run_id, Some(run));
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["synthesis_run_id"], serde_json::json!(run));
    }

    /// A council recorded before rounds existed is read in the shape `0155` copied it into: answers
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

    /// The config is written before the council row exists, so a write that fails can no longer
    /// strand a `running` council; the helper is what reports it.
    #[test]
    fn an_unwritable_mcp_config_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("nucleos-mcp-blocked-{}", std::process::id()));
        std::fs::write(&dir, b"a file where a directory is needed").unwrap();
        let blocked = dir.join("config.json");
        assert!(super::launch::write_mcp_config(&blocked).is_err());
        let _ = std::fs::remove_file(&dir);
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

    // --- council-deliberacao P7: revise rounds, early stop, the call ceiling. ---
    //
    // The anonymous labels are a shuffle seeded by the council's id, and a phase's seats are
    // launched together, so no test here knows which seat drew which label or which scripted
    // reply. Every assertion is therefore about a property that holds whatever the shuffle and
    // the arrival order: replies within a phase are identical, or the assertion counts.

    /// A critique that ranks `ranking` and reviews every label of a three-seat council with one
    /// point of `stance`. `parse_critique` keeps only the labels the critic was shown, so each
    /// answer ends up reviewed by exactly the two seats that were shown it.
    fn critique_with_points(ranking: &[&str], stance: &str) -> String {
        let reviews: Vec<serde_json::Value> = ["A", "B", "C"]
            .into_iter()
            .map(|label| {
                serde_json::json!({
                    "label": label,
                    "points": [{
                        "claim": "the claim the reviewer made",
                        "stance": stance,
                        "why": "the reason the reviewer gave",
                    }],
                })
            })
            .collect();
        serde_json::json!({ "reviews": reviews, "ranking": ranking }).to_string()
    }

    /// A revision reply in the `formats::Revision` shape.
    fn revision_json(changed: bool, why: &str, answer: &str) -> String {
        if changed {
            serde_json::json!({ "changed": true, "why": why, "answer": answer }).to_string()
        } else {
            serde_json::json!({ "changed": false }).to_string()
        }
    }

    const ORIGINALS: [&str; 3] = ["answer zero", "answer one", "answer two"];

    fn three_originals() -> std::collections::VecDeque<Scripted> {
        ORIGINALS
            .iter()
            .map(|text| Scripted::Answers(text.to_string()))
            .collect()
    }

    fn replies(count: usize, text: &str) -> std::collections::VecDeque<Scripted> {
        (0..count)
            .map(|_| Scripted::Answers(text.to_string()))
            .collect()
    }

    /// The critique prompts of one round, by position: a council of `members` sends `members`
    /// critique prompts per round, and a round's prompts all arrive before the next round's.
    fn critique_round(runner: &ScriptedRunner, members: usize, round: usize) -> Vec<String> {
        prompts_with(runner, prompts::CRITIQUE_MARKER)
            .into_iter()
            .skip((round - 1) * members)
            .take(members)
            .collect()
    }

    /// The labels a seat's critique step at `round` ranked — the labels it was shown.
    fn ranked_labels(
        steps: &[store::StepRow],
        seat_idx: i64,
        round: i64,
    ) -> std::collections::BTreeSet<String> {
        let step = step_at(steps, seat_idx, round, store::PHASE_CRITIQUE)
            .unwrap_or_else(|| panic!("seat {seat_idx} has no critique at round {round}"));
        let ballot: formats::Critique =
            serde_json::from_str(step.payload.as_deref().expect("a critique payload")).unwrap();
        ballot.ranking.into_iter().collect()
    }

    #[tokio::test]
    async fn driver_two_rounds_revise_and_vote_again() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = three_originals();
        // Round 1 ranks A > B > C and round 2 the reverse, so the Borda order moves between the
        // rounds and nothing about early stopping is in play.
        let mut critiques = replies(3, &critique_with_points(&["A", "B", "C"], "agree"));
        critiques.extend(replies(3, &critique_with_points(&["C", "B", "A"], "agree")));
        *runner.critique.lock().unwrap() = critiques;
        *runner.revise.lock().unwrap() = replies(
            3,
            &revision_json(true, "the critique convinced me", "revised answer"),
        );
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start_with(&state, "why?", None, Some(2), BTreeMap::new())
            .await
            .unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);
        assert_eq!(row.rounds_run, 2, "both critique rounds ran");
        assert!(
            !row.stopped_early,
            "the order moved, so nothing stopped early"
        );

        let steps = steps_for(&state, &id).await;
        for (seat_idx, original) in ORIGINALS.iter().enumerate() {
            let seat_idx = seat_idx as i64;
            // Round 0 is never overwritten by a revision.
            let answer = step_at(&steps, seat_idx, 0, store::PHASE_ANSWER).unwrap();
            let payload: serde_json::Value =
                serde_json::from_str(answer.payload.as_deref().unwrap()).unwrap();
            assert_eq!(payload, serde_json::json!({ "answer": original }));

            // The revision is recorded at the round whose critique it answers, as a Revision.
            let revise = step_at(&steps, seat_idx, 1, store::PHASE_REVISE)
                .unwrap_or_else(|| panic!("seat {seat_idx} has no revise step: {steps:?}"));
            assert_eq!(revise.status, SEAT_OK, "{revise:?}");
            assert!(revise.run_id.is_some());
            let revision: formats::Revision =
                serde_json::from_str(revise.payload.as_deref().expect("a revision payload"))
                    .unwrap();
            assert!(revision.changed);
            assert_eq!(revision.answer.as_deref(), Some("revised answer"));

            // Critiqued again, under the same letters as in round 1.
            assert_eq!(
                step_at(&steps, seat_idx, 2, store::PHASE_CRITIQUE)
                    .unwrap()
                    .status,
                SEAT_OK
            );
            assert_eq!(
                ranked_labels(&steps, seat_idx, 1),
                ranked_labels(&steps, seat_idx, 2),
                "a seat is shown the same letters every round"
            );
        }
        // No revise after the last round's critique.
        assert!(step_at(&steps, 0, 2, store::PHASE_REVISE).is_none());

        // Each seat revises behind its own answer and the points it received — two reviewers,
        // anonymous — and is shown no peer's answer, no model and no standing.
        let revises = prompts_with(&runner, prompts::REVISE_MARKER);
        assert_eq!(revises.len(), 3);
        for prompt in &revises {
            let own = ORIGINALS.iter().filter(|a| prompt.contains(*a)).count();
            assert_eq!(own, 1, "a revision shows its own answer only:\n{prompt}");
            assert!(prompt.contains("the claim the reviewer made"), "{prompt}");
            assert!(prompt.contains("Reviewer 2:"), "{prompt}");
            assert!(!prompt.contains("Reviewer 3:"), "{prompt}");
            assert!(!prompt.contains("model-"), "{prompt}");
            assert!(!prompt.contains("Leaderboard"), "{prompt}");
        }

        // Round 2 critiques the CURRENT answers, i.e. the revised ones.
        let second = critique_round(&runner, 3, 2);
        assert_eq!(second.len(), 3);
        for prompt in &second {
            assert!(prompt.contains("revised answer"), "{prompt}");
            assert!(
                ORIGINALS.iter().all(|a| !prompt.contains(a)),
                "a superseded answer was critiqued again:\n{prompt}"
            );
        }

        // The chairman reads the latest answers and every change with its why.
        let chairman = prompts_with(&runner, prompts::CHAIRMAN_MARKER);
        assert_eq!(chairman.len(), 1);
        assert!(chairman[0].contains("revised answer"), "{}", chairman[0]);
        assert!(!chairman[0].contains("No seat changed its answer."));
        for k in 0..3 {
            assert!(
                chairman[0].contains(&format!("model-{k}: the critique convinced me")),
                "{}",
                chairman[0]
            );
        }

        // 3 answers + 3 critiques + 3 revisions + 3 critiques + 1 chairman.
        assert_eq!(runner.seen.lock().unwrap().len(), 13);
    }

    #[tokio::test]
    async fn driver_three_rounds_stop_early_when_nothing_moves() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = three_originals();
        // The same ballots in every round and no dispute anywhere: round 2 moves nothing.
        *runner.critique.lock().unwrap() = replies(9, &critique_json(&["A", "B", "C"]));
        *runner.revise.lock().unwrap() = replies(6, &revision_json(false, "", ""));
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start_with(&state, "why?", None, Some(3), BTreeMap::new())
            .await
            .unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);
        assert!(
            row.stopped_early,
            "nothing moved, so round 3 is not paid for"
        );
        assert_eq!(row.rounds_run, 2);
        assert_eq!(row.rounds, 3, "the request's count is kept beside what ran");

        assert_eq!(prompts_with(&runner, prompts::CRITIQUE_MARKER).len(), 6);
        assert_eq!(prompts_with(&runner, prompts::REVISE_MARKER).len(), 3);
        assert_eq!(prompts_with(&runner, prompts::CHAIRMAN_MARKER).len(), 1);
        let steps = steps_for(&state, &id).await;
        assert!(
            steps
                .iter()
                .all(|step| step.round <= 2
                    && !(step.round == 2 && step.phase == store::PHASE_REVISE)),
            "nothing after the round that stopped: {steps:?}"
        );
        // 3 answers + 3 critiques + 3 revisions + 3 critiques + 1 chairman.
        assert_eq!(runner.seen.lock().unwrap().len(), 13);
    }

    /// A CLI seat's stdout is its whole `stream-json` transcript, the shape `runner::extract_reply`
    /// unwraps. A critique that arrives inside one must be parsed from the REPLY, not from the
    /// stream: read raw, the first line is a `system` event and the ballot is an escaped string,
    /// so the step would be `invalid` and the vote lost.
    #[tokio::test]
    async fn a_stream_json_seat_reply_becomes_a_valid_critique() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = three_originals();
        let critique = critique_json(&["A", "B", "C"]);
        let stream = [
            serde_json::json!({ "type": "system", "subtype": "init", "session_id": "s" }),
            serde_json::json!({
                "type": "assistant",
                "message": { "content": [{ "type": "text", "text": "thinking it over" }] },
            }),
            serde_json::json!({
                "type": "result",
                "subtype": "success",
                "result": critique,
                "total_cost_usd": 0.01,
            }),
        ]
        .iter()
        .map(|event| event.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        *runner.critique.lock().unwrap() = replies(3, &stream);
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start_with(&state, "why?", None, Some(1), BTreeMap::new())
            .await
            .unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);

        let steps = steps_for(&state, &id).await;
        for seat_idx in 0..3 {
            let step = step_at(&steps, seat_idx, 1, store::PHASE_CRITIQUE)
                .unwrap_or_else(|| panic!("seat {seat_idx} has no critique: {steps:?}"));
            assert_ne!(step.status, store::STEP_INVALID, "{step:?}");
            assert_eq!(step.status, SEAT_OK, "{step:?}");
            assert!(
                !ranked_labels(&steps, seat_idx, 1).is_empty(),
                "the ballot was kept: {step:?}"
            );
        }

        // The ballots were counted: every answer was shown to, and ranked by, the two other seats.
        let view = view_of(&state, &id).await;
        assert_eq!(view.leaderboard.len(), 3, "{:?}", view.leaderboard);
        assert!(
            view.leaderboard.iter().all(|row| row.n == 2),
            "{:?}",
            view.leaderboard
        );
    }

    /// The chairman's first reply will not parse and its retry's RUN fails outright. The first
    /// reply is still text the chairman wrote, so the council settles `done` with it as a degraded
    /// synthesis, and the reason names both failures — not just the last one.
    #[tokio::test]
    async fn a_failed_chairman_retry_settles_degraded_with_both_reasons() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = [
            Scripted::Answers("answer zero".into()),
            Scripted::Answers("answer one".into()),
        ]
        .into();
        let first = "first attempt, no JSON at all";
        *runner.chairman.lock().unwrap() = [
            Scripted::Answers(first.into()),
            Scripted::Fails("the chairman retry crashed".into()),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(2))).await;

        let id = start(&state, "why?", None).await.unwrap();
        let row = settled(&state, &id).await;

        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);
        assert_eq!(row.error, None);
        assert_eq!(
            prompts_with(&runner, prompts::CHAIRMAN_MARKER).len(),
            2,
            "retried exactly once"
        );
        assert_eq!(row.synthesis_status.as_deref(), Some("degraded"));
        let synthesis: formats::Synthesis =
            serde_json::from_str(row.synthesis_json.as_deref().expect("a degraded synthesis"))
                .unwrap();
        assert_eq!(synthesis.answer, first, "the first reply is what is kept");
        let reason = synthesis
            .degraded_reason
            .as_deref()
            .expect("a degraded synthesis says why");
        let parse_failure =
            formats::parse_synthesis(first).expect_err("the first reply does not parse");
        assert!(
            reason.contains(&parse_failure),
            "the parse failure is named: {reason}"
        );
        assert!(
            reason.contains("the retry failed"),
            "the retry is named: {reason}"
        );
        assert!(
            reason.contains("the chairman retry crashed"),
            "the retry's own error is carried: {reason}"
        );
    }

    /// The `round < rounds` guard: a stable order across rounds stops a council EARLY only when a
    /// round is still left to skip. At rounds = 2 the second round is the last one asked for, so
    /// the council ran everything it was given and did not stop early.
    #[tokio::test]
    async fn two_rounds_with_a_stable_order_do_not_stop_early() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = three_originals();
        // The same ballots in both rounds and no dispute anywhere.
        *runner.critique.lock().unwrap() = replies(6, &critique_json(&["A", "B", "C"]));
        *runner.revise.lock().unwrap() = replies(3, &revision_json(false, "", ""));
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start_with(&state, "why?", None, Some(2), BTreeMap::new())
            .await
            .unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);
        assert_eq!(row.rounds, 2);
        assert_eq!(row.rounds_run, 2, "both critique rounds ran");
        assert!(
            !row.stopped_early,
            "the last round asked for is not an early stop"
        );

        assert_eq!(prompts_with(&runner, prompts::CRITIQUE_MARKER).len(), 6);
        assert_eq!(prompts_with(&runner, prompts::REVISE_MARKER).len(), 3);
        assert_eq!(prompts_with(&runner, prompts::CHAIRMAN_MARKER).len(), 1);
        let view = view_of(&state, &id).await;
        assert!(!view.stopped_early);
    }

    #[tokio::test]
    async fn driver_unchanged_revision_keeps_the_previous_answer() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = three_originals();
        let mut critiques = replies(3, &critique_json(&["A", "B", "C"]));
        critiques.extend(replies(3, &critique_json(&["C", "B", "A"])));
        *runner.critique.lock().unwrap() = critiques;
        // Two seats keep their answer, one changes it.
        let mut revisions = replies(2, &revision_json(false, "", ""));
        revisions.push_back(Scripted::Answers(revision_json(
            true,
            "one critique was right",
            "the one revised answer",
        )));
        *runner.revise.lock().unwrap() = revisions;
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start_with(&state, "why?", None, Some(2), BTreeMap::new())
            .await
            .unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);

        let steps = steps_for(&state, &id).await;
        let revises: Vec<&store::StepRow> = steps
            .iter()
            .filter(|step| step.phase == store::PHASE_REVISE)
            .collect();
        assert_eq!(revises.len(), 3, "{steps:?}");
        assert!(
            revises.iter().all(|step| step.status == SEAT_OK),
            "keeping an answer is a valid revision: {revises:?}"
        );

        // Round 2 shows the two kept answers and the one revised answer, nothing else.
        let second = critique_round(&runner, 3, 2);
        assert_eq!(second.len(), 3);
        let kept: std::collections::BTreeSet<&str> = ORIGINALS
            .iter()
            .copied()
            .filter(|a| second.iter().any(|prompt| prompt.contains(a)))
            .collect();
        assert_eq!(kept.len(), 2, "the two unchanged answers stand: {second:?}");
        assert_eq!(
            second
                .iter()
                .filter(|prompt| prompt.contains("the one revised answer"))
                .count(),
            2,
            "the revised answer is shown to both of its peers"
        );

        let chairman = prompts_with(&runner, prompts::CHAIRMAN_MARKER);
        assert_eq!(chairman.len(), 1);
        assert!(chairman[0].contains("the one revised answer"));
        for answer in &kept {
            assert!(chairman[0].contains(answer), "{}", chairman[0]);
        }
        // Exactly one change reaches the chairman.
        assert_eq!(chairman[0].matches("one critique was right").count(), 1);
    }

    #[tokio::test]
    async fn driver_all_revisions_invalid_still_runs_the_next_round() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = three_originals();
        let mut critiques = replies(3, &critique_json(&["A", "B", "C"]));
        critiques.extend(replies(3, &critique_json(&["C", "B", "A"])));
        *runner.critique.lock().unwrap() = critiques;
        *runner.revise.lock().unwrap() = replies(3, "I have thought about it, no JSON though.");
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start_with(&state, "why?", None, Some(2), BTreeMap::new())
            .await
            .unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);
        assert_eq!(
            row.rounds_run, 2,
            "an unreadable revision does not end the council"
        );

        let steps = steps_for(&state, &id).await;
        for seat_idx in 0..3 {
            let revise = step_at(&steps, seat_idx, 1, store::PHASE_REVISE)
                .unwrap_or_else(|| panic!("seat {seat_idx} has no revise step: {steps:?}"));
            assert_eq!(revise.status, store::STEP_INVALID, "{revise:?}");
            assert!(
                revise
                    .error
                    .as_deref()
                    .is_some_and(|reason| !reason.trim().is_empty()),
                "{revise:?}"
            );
            assert!(revise.run_id.is_some(), "the run finished and is kept");
            assert_eq!(
                step_at(&steps, seat_idx, 2, store::PHASE_CRITIQUE)
                    .unwrap_or_else(|| panic!("seat {seat_idx} was not critiqued again"))
                    .status,
                SEAT_OK
            );
        }

        // The previous answers stand, so round 2 critiques the originals again.
        let second = critique_round(&runner, 3, 2);
        assert_eq!(second.len(), 3);
        for prompt in &second {
            assert_eq!(
                ORIGINALS.iter().filter(|a| prompt.contains(*a)).count(),
                2,
                "{prompt}"
            );
        }
        let chairman = prompts_with(&runner, prompts::CHAIRMAN_MARKER);
        assert_eq!(chairman.len(), 1);
        assert!(chairman[0].contains("No seat changed its answer."));
    }

    #[tokio::test]
    async fn driver_round_with_fewer_than_two_critiques_goes_to_the_chairman() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = three_originals();
        let mut critiques = replies(1, &critique_json(&["A", "B", "C"]));
        critiques.extend(replies(2, "No ranking from me."));
        *runner.critique.lock().unwrap() = critiques;
        *runner.revise.lock().unwrap() =
            replies(6, &revision_json(true, "should never be asked", "x"));
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start_with(&state, "why?", None, Some(3), BTreeMap::new())
            .await
            .unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);
        assert_eq!(row.rounds_run, 1);

        // One ballot is not a vote: nobody is asked to revise against it.
        assert!(prompts_with(&runner, prompts::REVISE_MARKER).is_empty());
        assert_eq!(prompts_with(&runner, prompts::CRITIQUE_MARKER).len(), 3);
        assert_eq!(prompts_with(&runner, prompts::CHAIRMAN_MARKER).len(), 1);
        let steps = steps_for(&state, &id).await;
        assert!(
            steps
                .iter()
                .all(|step| step.phase != store::PHASE_REVISE && step.round <= 1),
            "{steps:?}"
        );
    }

    #[tokio::test]
    async fn driver_never_exceeds_the_call_ceiling() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = three_originals();
        // The order flips every round and every seat changes every time, so all three rounds run;
        // the chairman then fails validation twice. That is the most a council can spend.
        let mut critiques = replies(3, &critique_json(&["A", "B", "C"]));
        critiques.extend(replies(3, &critique_json(&["C", "B", "A"])));
        critiques.extend(replies(3, &critique_json(&["A", "B", "C"])));
        *runner.critique.lock().unwrap() = critiques;
        *runner.revise.lock().unwrap() =
            replies(6, &revision_json(true, "moved again", "a new answer"));
        *runner.chairman.lock().unwrap() = [
            Scripted::Answers("first attempt, no JSON at all".into()),
            Scripted::Answers("second attempt, still prose".into()),
        ]
        .into();
        let state = council_state(runner.clone(), Some(roster(3))).await;

        let id = start_with(&state, "why?", None, Some(3), BTreeMap::new())
            .await
            .unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);
        assert_eq!(row.rounds_run, 3);
        assert!(!row.stopped_early);
        assert_eq!(row.synthesis_status.as_deref(), Some("degraded"));

        let calls = runner.seen.lock().unwrap().len();
        let ceiling = tally::call_ceiling(3, 3);
        assert!(
            calls <= ceiling,
            "{calls} calls against a ceiling of {ceiling}"
        );
        // And this path IS the worst case: 3 + 3*3 + 2*3 + 2.
        assert_eq!(calls, ceiling);
        assert_eq!(prompts_with(&runner, prompts::REVISE_MARKER).len(), 6);
        assert_eq!(prompts_with(&runner, prompts::CRITIQUE_MARKER).len(), 9);
        assert_eq!(prompts_with(&runner, prompts::CHAIRMAN_MARKER).len(), 2);
    }

    #[tokio::test]
    async fn driver_cancel_mid_round_stops_the_next_phase() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = three_originals();
        *runner.critique.lock().unwrap() = replies(6, &critique_json(&["A", "B", "C"]));
        *runner.revise.lock().unwrap() = [Scripted::Hangs, Scripted::Hangs, Scripted::Hangs].into();
        let mut config = roster(3);
        config.timeout_seconds = 600;
        let state = council_state(runner.clone(), Some(config)).await;

        let id = start_with(&state, "why?", None, Some(2), BTreeMap::new())
            .await
            .unwrap();
        // Wait until all three revisions are in flight, each with its run on its step.
        for _ in 0..300 {
            let steps = steps_for(&state, &id).await;
            let revising: Vec<&store::StepRow> = steps
                .iter()
                .filter(|step| step.phase == store::PHASE_REVISE)
                .collect();
            if prompts_with(&runner, prompts::REVISE_MARKER).len() == 3
                && revising.len() == 3
                && revising.iter().all(|step| step.run_id.is_some())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(prompts_with(&runner, prompts::REVISE_MARKER).len(), 3);

        assert!(cancel(&state, &id).await.unwrap());
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_CANCELLED);

        let mut steps = Vec::new();
        for _ in 0..300 {
            steps = steps_for(&state, &id).await;
            if steps
                .iter()
                .filter(|step| step.phase == store::PHASE_REVISE)
                .all(|step| step.status == SEAT_CANCELLED)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        for step in steps
            .iter()
            .filter(|step| step.phase == store::PHASE_REVISE)
        {
            assert_eq!(step.status, SEAT_CANCELLED, "{step:?}");
        }
        // Give a driver that ignored the cancel the time to start round 2 — it must not.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(
            prompts_with(&runner, prompts::CRITIQUE_MARKER).len(),
            3,
            "round 2 started after the cancel"
        );
        assert!(prompts_with(&runner, prompts::CHAIRMAN_MARKER).is_empty());
        assert!(
            steps_for(&state, &id)
                .await
                .iter()
                .all(|step| step.round <= 1),
            "no step was written past the cancelled round"
        );
    }

    /// The multi-round counterpart of `each_routed_seat_reports_its_outcome_once_to_the_router`:
    /// the revise phase and the second critique go through the same routed launch, and each
    /// decision is reported exactly once.
    #[tokio::test]
    async fn driver_every_phase_of_a_multi_round_council_is_routed_and_reported_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        *runner.stage1.lock().unwrap() = three_originals();
        let mut critiques = replies(3, &critique_json(&["A", "B", "C"]));
        critiques.extend(replies(3, &critique_json(&["C", "B", "A"])));
        *runner.critique.lock().unwrap() = critiques;
        *runner.revise.lock().unwrap() = replies(
            3,
            &revision_json(true, "the critique convinced me", "revised answer"),
        );

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
        let inner: std::sync::Arc<dyn crate::execution::runner::CommandRunner> = runner.clone();
        let router = std::sync::Arc::new(crate::models::route_advice::Router::new(
            crate::models::route_advice::RouterConfig {
                mode: crate::models::route_advice::Mode::Off,
                url,
                surfaces: [("council", crate::models::route_advice::Mode::Shadow)]
                    .into_iter()
                    .collect(),
                ..crate::models::route_advice::RouterConfig::off()
            },
            crate::models::route_advice::Available {
                kind: crate::models::route_advice::RunnerKind::Claude,
                runner: inner.clone(),
                default_model: "claude-sonnet-5".into(),
                models: vec!["claude-*".into()],
            },
            Vec::new(),
        ));
        let mut state = council_state(runner.clone(), Some(roster(3))).await;
        state.runner =
            std::sync::Arc::new(crate::models::route_advice::RoutedRunner { inner, router });

        let id = start_with(&state, "why?", None, Some(2), BTreeMap::new())
            .await
            .unwrap();
        let row = settled(&state, &id).await;
        assert_eq!(row.status, STATUS_DONE, "{:?}", row.error);
        assert_eq!(row.rounds_run, 2);

        let mut reports = Vec::new();
        while let Ok(Some(report)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), received.recv()).await
        {
            reports.push(report);
        }
        let routed = asked.load(Ordering::SeqCst);
        assert!(routed > 3, "only the answers were routed: {routed}");
        assert_eq!(
            reports.len(),
            routed,
            "one report per decision: {reports:?}"
        );
        let ids: std::collections::BTreeSet<&str> =
            reports.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids.len(), reports.len(), "a decision was reported twice");
        assert!(
            reports.iter().all(|(_, status)| status == "pass"),
            "{reports:?}"
        );
        // Every launch — answers, both critiques, the revisions, the chairman — went through the
        // router: 3 + 3 + 3 + 3 + 1.
        let launched = runner.seen.lock().unwrap().len();
        assert_eq!(routed, launched, "a phase launched around the router");
        assert_eq!(launched, 13);
        assert_eq!(prompts_with(&runner, prompts::REVISE_MARKER).len(), 3);
    }

    // ── P8: the surface carries rounds and roles, and serves the form its defaults ───────────

    /// A request that names rounds and roles gets a council run with exactly those — recorded on
    /// the row and on the seat — rather than the file's, because the handler is the only way the
    /// shell's form reaches `start_with`.
    #[tokio::test]
    async fn post_council_records_rounds_and_roles() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        let state = council_state(runner, Some(roster(2))).await;

        let (status, body) = post_council(
            axum::extract::State(state.clone()),
            axum::Json(CreateCouncilRequest {
                question: "why?".to_string(),
                roster: None,
                rounds: Some(2),
                roles: Some(roles(&[("1", "skeptic")])),
            }),
        )
        .await
        .unwrap();
        assert_eq!(status, axum::http::StatusCode::ACCEPTED);

        let row = settled(&state, &body.id).await;
        // Not the file's DEFAULT_COUNCIL_ROUNDS: the request's.
        assert_eq!(row.rounds, 2);
        let seats = get_seat_rows(&state.pool, &body.id).await.unwrap();
        assert_eq!(seats[0].role, None);
        assert_eq!(seats[1].role.as_deref(), Some("skeptic"));
    }

    /// The JSON body is what a client actually sends, so the fields are proven to deserialize under
    /// these names and to be optional — an older client posting only a question must still work.
    #[test]
    fn post_council_request_takes_rounds_and_roles_from_json() {
        let request: CreateCouncilRequest = serde_json::from_value(serde_json::json!({
            "question": "why?",
            "rounds": 3,
            "roles": { "0": "fact_checker" },
        }))
        .unwrap();
        assert_eq!(request.rounds, Some(3));
        assert_eq!(request.roles, Some(roles(&[("0", "fact_checker")])));

        let bare: CreateCouncilRequest =
            serde_json::from_value(serde_json::json!({ "question": "why?" })).unwrap();
        assert_eq!(bare.rounds, None);
        assert_eq!(bare.roles, None);
    }

    /// A role outside the closed set is the client's mistake, said as a 400 — and nothing is
    /// written or launched, because a typo that ran would bill a council nobody asked for.
    #[tokio::test]
    async fn post_council_refuses_an_unknown_role_with_400() {
        let runner = std::sync::Arc::new(ScriptedRunner::default());
        let state = council_state(runner.clone(), Some(roster(2))).await;

        for (rounds, roles) in [
            (None, roles(&[("0", "Skeptic")])),
            (None, roles(&[("0", "contrarian")])),
            (None, roles(&[("7", "skeptic")])),
            (Some(crate::config::MAX_COUNCIL_ROUNDS + 1), BTreeMap::new()),
        ] {
            let (status, message) = post_council(
                axum::extract::State(state.clone()),
                axum::Json(CreateCouncilRequest {
                    question: "why?".to_string(),
                    roster: None,
                    rounds,
                    roles: Some(roles.clone()),
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(
                status,
                axum::http::StatusCode::BAD_REQUEST,
                "{rounds:?} {roles:?}"
            );
            assert!(!message.trim().is_empty());
        }
        assert!(
            list_council_rows(&state.pool, 10, 0)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(runner.seen.lock().unwrap().is_empty());
    }

    /// The form's defaults, read from the file: the configured rounds, the ceiling, the closed role
    /// set in its declared order, and the roster in the two seat forms the file allows.
    #[tokio::test]
    async fn council_config_serves_form_defaults() {
        let mut config = roster(1);
        config.rounds = 2;
        config.members.push(agent_spec("agent-1"));
        let state =
            council_state(std::sync::Arc::new(ScriptedRunner::default()), Some(config)).await;

        let response = get_council_config(axum::extract::State(state)).await;
        let body = serde_json::to_value(&response.0).unwrap();

        assert_eq!(
            body,
            serde_json::json!({
                "configured": true,
                "default_rounds": 2,
                "max_rounds": crate::config::MAX_COUNCIL_ROUNDS,
                "roles": ["proposer", "skeptic", "devils_advocate", "fact_checker"],
                "default_roster": {
                    "chairman": { "kind": "cloud", "ref": "the-chairman" },
                    "members": [
                        { "kind": "cloud", "ref": "model-0" },
                        { "agent": "agent-1" },
                    ],
                },
            })
        );
        // The role list IS `Role::ALL`, so a fifth role reaches the form without a second edit.
        let wire: Vec<&str> = formats::Role::ALL
            .iter()
            .map(|role| role.as_str())
            .collect();
        assert_eq!(body["roles"], serde_json::json!(wire));
    }

    /// No roster: the form still learns the bounds and the roles, and is told plainly there is no
    /// council rather than handed an empty roster it would try to render.
    #[tokio::test]
    async fn council_config_without_a_roster_says_unconfigured() {
        let state = council_state(std::sync::Arc::new(ScriptedRunner::default()), None).await;

        let response = get_council_config(axum::extract::State(state)).await;
        let body = serde_json::to_value(&response.0).unwrap();

        assert_eq!(body["configured"], serde_json::json!(false));
        assert!(body["default_roster"].is_null(), "{body}");
        assert_eq!(
            body["default_rounds"],
            serde_json::json!(crate::config::DEFAULT_COUNCIL_ROUNDS)
        );
        assert_eq!(
            body["max_rounds"],
            serde_json::json!(crate::config::MAX_COUNCIL_ROUNDS)
        );
        assert_eq!(
            body["roles"],
            serde_json::json!(["proposer", "skeptic", "devils_advocate", "fact_checker"])
        );
    }
}
