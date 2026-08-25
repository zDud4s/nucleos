//! Watches what runs SPEND per unit of work, and says so when the answer stops making sense.
//!
//! The telemetry this reads has existed since 0031 and has never had a reader: `budget.rs` loads the
//! token counts and marks them `#[allow(dead_code)]`, because without a price table they must not
//! influence its conservative time approximation. So a regression in efficiency — a prompt that grew
//! by a megabyte, a cache that quietly stopped being hit — is invisible today, and the one instance
//! anybody has actually found was found by accident.
//!
//! **This module only observes.** It never pauses a run, never rewrites a prompt, never touches
//! `BudgetDecision`. That is a deliberate boundary and not an unfinished one: braking on a heuristic
//! built from four unvalidated thresholds would stop real work on a guess, and the guess is exactly
//! what has not been calibrated yet. Reading the feed is how those thresholds earn the right to do
//! anything more.
//!
//! **Why the suppression is half the module.** A feed row IS a notification — the Telegram sidecar
//! forwards every one it finds (`notify.rs`). Evaluating at the end of every run and writing what it
//! finds would ping the owner several times an hour about a metric that is noisy per-run by nature:
//! the first request of a session writes the cache and reads nothing back, and one large file
//! legitimately read makes one run's context enormous. So a signal must HOLD across consecutive
//! evaluations before it earns a word, and having spoken, it stays quiet for a window. The state
//! that decides this lives in `efficiency_signals` (0067) rather than in memory, because the daemon
//! restarts and a streak that resets on restart would let a permanent regression stay permanently
//! below the threshold.
//!
//! **What it deliberately does NOT report.** Anything the daemon already answers. Chats are
//! compacted in place by the CLI at their own window; every other mode hands off at four fifths of
//! `HANDOFF_CONTEXT_LIMIT_FLOOR` and records a successor as it goes. So the context signal reports
//! the handoff FAILING to happen, not the swelling — a detector that names a threshold enforced
//! elsewhere is announcing somebody else's solved problem to somebody who cannot act on it. The same
//! rule kills the obvious prompt-to-output ratio: the `result` event reports the CUMULATIVE session
//! total (see `budget.rs`), so any such ratio compares a whole session's reading to one message's
//! writing, and calls a worktree run that ends with a three-line summary waste.
//!
//! `evaluate` is pure and holds the whole judgement; `observe_run` is the thin part that reads a row,
//! calls it, and decides whether anyone hears about it. Everything worth testing is on the pure side.

use chrono::{DateTime, Duration, Utc};

/// Prompts shorter than this are not cached by the API at all, and no error says so.
///
/// The real minimum is per-model and NOT monotonic with model age — 512 tokens on Opus 5, 1024 on
/// Opus 4.8 and Sonnet 5, 2048 on Opus 4.7, 4096 on Opus 4.6 and Haiku 4.5. Picking the largest of
/// them is the only safe choice while nothing in the schema records WHICH model ran a given run:
/// `runs.answered_by` stores `cloud` or `local`, not a model id. Erring high makes the signal go
/// quiet on short prompts; erring low would make it accuse the API of a defect in the exact range
/// where the API is behaving exactly as documented.
const MIN_CACHEABLE_PREFIX_TOKENS: i64 = 4096;

/// Context a non-chat run reached without any handoff having taken it away.
///
/// Deliberately AT `runs.rs`'s `HANDOFF_CONTEXT_LIMIT_FLOOR` and not below it. The handoff fires at
/// four fifths of that floor — 160_000 — so a run still climbing at 200_000 with no successor
/// recorded did not merely grow: the mechanism that exists to stop it growing did not fire. That is
/// the reportable fact. The swelling itself is not, because the daemon already answers it, and a
/// detector that names a threshold enforced elsewhere is reporting somebody else's solved problem.
const CONTEXT_SWELLING_TOKENS: i64 = 200_000;

/// Turns spent by a run that then did not succeed, above which the spend bought nothing at all.
const OUTPUT_STARVED_TURNS: i64 = 20;

/// How many recent runs of the same shape the drift baseline is built from.
const BASELINE_WINDOW: i64 = 50;

/// Below this many samples the median itself is unstable, and comparing against an unstable median
/// manufactures anomalies rather than finding them.
const MIN_BASELINE_SAMPLE: usize = 20;

/// A dispersion below this is not dispersion, it is a coincidence.
///
/// MAD goes to zero the moment more than half the window holds the identical value — which happens
/// easily on a quiet daemon replaying the same job. At MAD 0 every other value is infinitely
/// anomalous, so the detector would fire on the first run that differs by a single token. Below the
/// floor the correct reading is that this population cannot be judged, not that everything in it is.
const MAD_FLOOR_TOKENS: i64 = 1_000;

/// How many MADs above the median a run must sit to count as drift.
///
/// Five, because under ordinary noise 5·MAD is roughly 3.4 standard deviations — rare enough that
/// pointing at it is worth doing, common enough that a real regression reaches it quickly. Median
/// and MAD rather than mean and stddev because a handful of enormous runs would drag a mean up until
/// it stopped being able to see them; the median needs more than half the window to move at all.
const DRIFT_MADS: i64 = 5;

/// How far back the baseline window may reach, so the query's cost is bounded by time and not only
/// by how many rows happen to match.
const BASELINE_MAX_AGE_DAYS: i64 = 30;

/// How many consecutive evaluations a signal must survive before it is allowed to speak.
const STREAK_BEFORE_ALERT: i64 = 4;

/// How long a signal stays quiet after speaking, however true it keeps being.
const SILENCE_WINDOW_HOURS: i64 = 24;

/// The feed `kind` every alert from this module carries, so it can be filtered as one thing.
const FEED_KIND: &str = "token_efficiency";

/// What one detector concluded about one run.
///
/// Three states and not two, and the third is the one that matters. `Quiet` means the detector
/// looked and found nothing wrong; `Undecidable` means this run cannot answer the question — a
/// measure is missing, or the signal does not apply to a run of this kind. Collapsing them would
/// let unmeasurable runs erase the evidence of a real regression: a progress-deadline kill reports
/// no usage at all, and every Codex run reports no cache writes, so a streak that reset on those
/// would never reach the alerting threshold on a daemon where they are common.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Firing,
    Quiet,
    Undecidable,
}

impl From<bool> for Verdict {
    fn from(held: bool) -> Self {
        if held {
            Verdict::Firing
        } else {
            Verdict::Quiet
        }
    }
}

/// One inefficiency worth naming. The variants are the detectors, not the severities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// The prompt was large enough to cache and neither read nor wrote a single cached token.
    CacheCold,
    /// A run climbed past the handoff ceiling and no handoff happened.
    ContextSwelling,
    /// The run spent many turns and did not succeed.
    OutputStarved,
    /// This run cost far more than the recent runs of its own shape.
    CostDrift,
}

impl Signal {
    /// The stable key stored in `efficiency_signals.signal`.
    fn key(self) -> &'static str {
        match self {
            Signal::CacheCold => "cache_cold",
            Signal::ContextSwelling => "context_swelling",
            Signal::OutputStarved => "output_starved",
            Signal::CostDrift => "cost_drift",
        }
    }
}

/// Everything one finished run reported about what it spent.
///
/// `Option` throughout and none of it defaulted to zero: a run that reported nothing must not read
/// back as a run that measured zero. Unknown is the single most common reason a signal stays silent,
/// and that is correct — the local model reports no usage at all, and Codex reports what it read
/// from the cache but never what it wrote there.
///
/// The column names are the field names, which is what lets `SELECT_MEASURES` read straight into
/// this and keeps the query from carrying a positional order nothing checks.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct Measures {
    pub mode: String,
    /// How the run ended, and the only thing that separates "produced nothing" from "was stopped".
    /// Read from here and never from `exit_code`: a run killed by the progress deadline carries
    /// `PROGRESS_TIMEOUT_EXIT_CODE`, which on its own distinguishes nothing.
    pub status: String,
    pub project_id: Option<String>,
    /// Which agent session this run belongs to. Load-bearing for the baseline and nothing else: a
    /// resumed run reports the CUMULATIVE session total, so its predecessors are partial re-counts
    /// of itself and comparing it against them would find drift in every handoff chain.
    pub session_id: Option<String>,
    /// The run this one handed off to when context pressure ended it, if any. This is the non-chat
    /// counterpart of the conversation rotation in `assistant.rs`, and the reason `ContextSwelling`
    /// reports the handoff FAILING to happen rather than the swelling itself.
    pub successor_run_id: Option<i64>,
    pub input_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_creation_tokens: Option<i64>,
    pub num_turns: Option<i64>,
    pub context_fill: Option<i64>,
}

impl Measures {
    /// The whole prompt, which is the sum of three numbers and not the first one.
    ///
    /// `input_tokens` is only the UNCACHED remainder; the cached part is billed separately as reads
    /// and writes. Any ratio taken from `input_tokens` alone is wrong by omission, and any of the
    /// three being unknown makes the total unknown rather than smaller.
    fn total_prompt_tokens(&self) -> Option<i64> {
        let input = self.input_tokens?;
        let read = self.cache_read_tokens?;
        let created = self.cache_creation_tokens?;
        input.checked_add(read)?.checked_add(created)
    }

    /// The population a streak is counted over: project AND mode.
    ///
    /// Mode belongs here because the baseline already partitions by it, and a streak that spans a
    /// wider population than its own baseline is a different measurement wearing the same name. It
    /// is also what makes the streak reachable at all — a job's three nodes are all `worktree` in
    /// one project, and one interleaved `shadow` run would otherwise reset the count every time.
    fn scope(&self) -> String {
        match &self.project_id {
            Some(project_id) => format!("project:{project_id}|{}", self.mode),
            None => format!("global|{}", self.mode),
        }
    }

    /// Whether the run reached its own end rather than being stopped from outside.
    ///
    /// A run somebody cancelled, a run the daemon interrupted at startup, a run the deadline killed
    /// — each produced little for a reason that has nothing to do with efficiency, and calling that
    /// waste would blame the run for the interruption.
    fn ended_on_its_own_terms(&self) -> bool {
        !matches!(
            self.status.as_str(),
            "cancelled" | "interrupted" | "timed_out"
        )
    }
}

/// What the recent runs of this shape normally cost.
///
/// Median and MAD rather than mean and standard deviation: token totals are heavily skewed, and both
/// of the classical statistics are dragged by the very outliers the detector is looking for. MAD
/// tolerates half the window being garbage before it moves at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Baseline {
    pub median_total: Option<i64>,
    pub mad: Option<i64>,
    pub sample: usize,
}

impl Baseline {
    /// PURE: the whole statistic, so the numbers can be checked without a database.
    pub fn from_totals(totals: &[i64]) -> Self {
        let mut sorted = totals.to_vec();
        sorted.sort_unstable();
        let Some(median_total) = median(&sorted) else {
            return Self::default();
        };

        let mut deviations = sorted
            .iter()
            .map(|total| (total - median_total).abs())
            .collect::<Vec<_>>();
        deviations.sort_unstable();

        Self {
            median_total: Some(median_total),
            mad: median(&deviations),
            sample: sorted.len(),
        }
    }
}

/// PURE: the middle of an already-sorted slice, averaging the two middles on an even count.
fn median(sorted: &[i64]) -> Option<i64> {
    match sorted.len() {
        0 => None,
        len if len % 2 == 1 => Some(sorted[len / 2]),
        len => Some((sorted[len / 2 - 1] + sorted[len / 2]) / 2),
    }
}

/// The tunable part, separated from the judgement so a test can state its own numbers instead of
/// being written around whatever the constants happen to be this month.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    pub min_cacheable_prefix_tokens: i64,
    pub context_swelling_tokens: i64,
    pub output_starved_turns: i64,
    pub min_baseline_sample: usize,
    pub mad_floor_tokens: i64,
    pub drift_mads: i64,
    pub streak_before_alert: i64,
    pub silence_window: Duration,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            min_cacheable_prefix_tokens: MIN_CACHEABLE_PREFIX_TOKENS,
            context_swelling_tokens: CONTEXT_SWELLING_TOKENS,
            output_starved_turns: OUTPUT_STARVED_TURNS,
            min_baseline_sample: MIN_BASELINE_SAMPLE,
            mad_floor_tokens: MAD_FLOOR_TOKENS,
            drift_mads: DRIFT_MADS,
            streak_before_alert: STREAK_BEFORE_ALERT,
            silence_window: Duration::hours(SILENCE_WINDOW_HOURS),
        }
    }
}

/// PURE: which signals this one run raises. No pool, no clock, no I/O — this is the whole detector.
///
/// Every signal is judged on every run and each carries its own verdict: they are independent
/// readings, and returning only what fired would erase the difference between a signal that was
/// measured and found fine and one this run could not answer at all.
pub fn evaluate(
    measures: &Measures,
    baseline: &Baseline,
    thresholds: &Thresholds,
) -> [(Signal, Verdict); 4] {
    [
        (Signal::CacheCold, cache_verdict(measures, thresholds)),
        (
            Signal::ContextSwelling,
            context_verdict(measures, thresholds),
        ),
        (Signal::OutputStarved, starved_verdict(measures, thresholds)),
        (
            Signal::CostDrift,
            drift_verdict(measures, baseline, thresholds),
        ),
    ]
}

/// A prompt that never touched the cache in either direction.
///
/// Both halves must be KNOWN and zero: reads alone cannot distinguish a run that paid to fill the
/// cache (expected, and billed at 1.25x or 2x precisely because somebody has to buy the first copy)
/// from one that missed the prefix entirely.
fn cache_verdict(measures: &Measures, thresholds: &Thresholds) -> Verdict {
    let (Some(read), Some(created), Some(total)) = (
        measures.cache_read_tokens,
        measures.cache_creation_tokens,
        measures.total_prompt_tokens(),
    ) else {
        // The shape a local-model run has, and every Codex run: that stream reports what it read
        // from the cache and never what it wrote there.
        return Verdict::Undecidable;
    };

    // Below the minimum cacheable length there is no cache to have exploited, so "did this run
    // exploit the cache?" has no answer rather than the answer `no`. Undecidable and not quiet:
    // quiet resets a streak, and a run too small to be cacheable is no evidence that a cold cache
    // got warmer. It matters more the moment this threshold is ever tuned downward.
    if total < thresholds.min_cacheable_prefix_tokens {
        return Verdict::Undecidable;
    }

    Verdict::from(read == 0 && created == 0)
}

/// Context that grew past the ceiling WITHOUT anything having carried it over.
///
/// Not the swelling itself, which the daemon already answers twice over: a conversation is compacted
/// in place by the CLI at its own window, and every other mode hands off at four fifths of
/// `HANDOFF_CONTEXT_LIMIT_FLOOR` — 160_000 — recording a successor run as it goes. Reporting the
/// swelling would name a threshold the daemon enforces itself, about a run whose successor is
/// sitting in the same feed. What nothing else notices is the mechanism failing to fire.
fn context_verdict(measures: &Measures, thresholds: &Thresholds) -> Verdict {
    // Chats are compacted in place and never record a successor, so the handoff evidence this
    // signal reads does not exist for them. Not answerable here, rather than answered `no`.
    if measures.mode == "assistant" {
        return Verdict::Undecidable;
    }
    let Some(fill) = measures.context_fill else {
        return Verdict::Undecidable;
    };
    if measures.successor_run_id.is_some() {
        return Verdict::Quiet;
    }

    Verdict::from(fill >= thresholds.context_swelling_tokens)
}

/// Many turns spent, and then the run did not succeed.
///
/// Every one of those turns re-sent the transcript, so this is the most expensive way there is to
/// arrive at nothing. Deliberately NOT a prompt-to-output ratio: the `result` event reports the
/// CUMULATIVE session total (`budget.rs`), so any ratio taken against it compares a whole session's
/// reading to one message's writing, and a worktree run that works up to the handoff line and ends
/// with a three-line summary is a normal run that such a ratio calls waste.
fn starved_verdict(measures: &Measures, thresholds: &Thresholds) -> Verdict {
    if !measures.ended_on_its_own_terms() {
        return Verdict::Undecidable;
    }
    let Some(turns) = measures.num_turns else {
        return Verdict::Undecidable;
    };

    Verdict::from(turns >= thresholds.output_starved_turns && measures.status != "completed")
}

/// A run that cost far more than the recent runs of its own shape.
///
/// The only signal that needs a past, and the only one whose silence is mostly about whether that
/// past is trustworthy: too few samples and the median moves under its own noise, too little
/// dispersion and every value looks extreme next to it. Both are unanswerable questions, not
/// answers — resetting a streak on either would let a thin window erase a real regression.
fn drift_verdict(measures: &Measures, baseline: &Baseline, thresholds: &Thresholds) -> Verdict {
    let (Some(total), Some(median_total), Some(mad)) = (
        measures.total_prompt_tokens(),
        baseline.median_total,
        baseline.mad,
    ) else {
        return Verdict::Undecidable;
    };
    if baseline.sample < thresholds.min_baseline_sample || mad < thresholds.mad_floor_tokens {
        return Verdict::Undecidable;
    }

    // Saturating rather than wrapping: the values come from a `result` line the daemon does not
    // author, and in a debug build an overflow here would panic on the run-finalisation path.
    let ceiling = median_total.saturating_add(thresholds.drift_mads.saturating_mul(mad));
    Verdict::from(total > ceiling)
}

/// PURE: whether a signal that has held for `streak` evaluations gets to say so now.
///
/// Both gates, in the order they matter. A signal that has never spoken has no window to wait out,
/// which is why `None` here means "allowed" rather than "unknown".
pub fn should_alert(
    streak: i64,
    last_alerted_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    thresholds: &Thresholds,
) -> bool {
    if streak < thresholds.streak_before_alert {
        return false;
    }
    match last_alerted_at {
        None => true,
        Some(last) => now.signed_duration_since(last) >= thresholds.silence_window,
    }
}

/// What the owner actually reads. The numbers are in it because "cache is cold" without them is a
/// claim nobody can check.
fn summarise(signal: Signal, measures: &Measures, baseline: &Baseline, streak: i64) -> String {
    let scope = measures.scope();
    match signal {
        Signal::CacheCold => {
            let total = describe(measures.total_prompt_tokens());
            format!(
                "token efficiency ({scope}): {streak} runs in a row sent a prompt of {total} tokens \
                 and neither read nor wrote a single cached token"
            )
        }
        Signal::ContextSwelling => {
            let fill = describe(measures.context_fill);
            format!(
                "token efficiency ({scope}): {streak} runs in a row reached {fill} context tokens \
                 without handing off — the handoff should have fired at 160000"
            )
        }
        Signal::OutputStarved => {
            let turns = describe(measures.num_turns);
            format!(
                "token efficiency ({scope}): {streak} runs in a row spent {turns} turns and ended \
                 `{status}`",
                status = measures.status
            )
        }
        Signal::CostDrift => {
            let total = describe(measures.total_prompt_tokens());
            let median_total = describe(baseline.median_total);
            let mad = describe(baseline.mad);
            format!(
                "token efficiency ({scope}): {streak} runs in a row cost {total} prompt tokens \
                 against a recent median of {median_total} (MAD {mad}, {sample} runs)",
                sample = baseline.sample
            )
        }
    }
}

/// A number the owner can check, or an honest question mark where there was none.
fn describe(value: Option<i64>) -> String {
    value.map_or_else(|| "?".to_string(), |value| value.to_string())
}

const SELECT_MEASURES: &str = "SELECT mode, status, project_id, session_id, successor_run_id, input_tokens, \
     cache_read_tokens, cache_creation_tokens, num_turns, context_fill FROM runs WHERE id = ?";

/// The recent totals of runs shaped like this one.
///
/// Ordered and bounded to ride `runs_by_mode_completed` (0067): `mode` fixes the leading column and
/// `completed_at >= ?` makes the rest a range scan instead of a walk back through the whole table.
/// The age bound is doing real work here — without it the window is bounded only by how many rows
/// happen to match the project filter, which on a busy daemon with many projects is unbounded.
///
/// `project_id IS ?` and not `= ?`: SQLite's `IS` is null-safe, so global-scope runs compare equal
/// to each other instead of every comparison against NULL evaluating to NULL and matching nothing.
///
/// The three `IS NOT NULL` clauses keep unmeasured runs out of the population rather than letting
/// SQL turn them into zero — a local-model run reports nothing, and averaging its silence in would
/// drag the median down until every cloud run looked like drift.
///
/// `GROUP BY` the session, taking the largest total in each: a resumed run reports the CUMULATIVE
/// session total, so summing a session's rows counts the same tokens repeatedly. `budget.rs` dedupes
/// spend the same way and for the same reason. The `COALESCE` gives every session-less run a key of
/// its own so it still counts once. `session_id <> ?` then drops the judged run's OWN session, whose
/// earlier turns are partial re-counts of the run being judged and sit below it by construction.
///
/// The `LIMIT` sits INSIDE the subquery, and that placement is what keeps this cheap. Grouping first
/// and ordering by `MAX(completed_at)` would order by an aggregate, which no index can satisfy —
/// SQLite would materialise and sort every row in the 30-day window before the limit applied, at the
/// end of every run, which is the one moment 0067 names as the worst place to spend time. Limiting
/// first lets the partial index be walked backwards and abandoned after N matches; the dedupe then
/// collapses that N into however many sessions it held. The sample is therefore "the last N runs of
/// this shape, counted once per session", which is the population meant all along.
const SELECT_BASELINE_TOTALS: &str = "SELECT MAX(total) FROM ( \
       SELECT input_tokens + cache_read_tokens + cache_creation_tokens AS total, \
              COALESCE(session_id, 'run:' || id) AS session_key \
         FROM runs \
        WHERE mode = ? AND project_id IS ? AND id <> ? \
          AND (session_id IS NULL OR session_id <> ?) \
          AND completed_at IS NOT NULL AND completed_at >= ? \
          AND input_tokens IS NOT NULL AND cache_read_tokens IS NOT NULL \
          AND cache_creation_tokens IS NOT NULL \
        ORDER BY completed_at DESC \
        LIMIT ?) \
      GROUP BY session_key";

/// Creates or advances the streak and reports back what it became, in ONE statement.
///
/// Read-modify-write across two statements is not safe here: WAL plus a five-connection pool means
/// two runs finishing milliseconds apart both read streak 3, both compute 4, and both alert — two
/// Telegram pings for the signal whose whole purpose is to produce one. The scheduler, autopilot and
/// an interactive run finish independently, so that interleaving is ordinary rather than exotic.
///
/// `last_alerted_at` is deliberately untouched here: advancing the streak and claiming the right to
/// speak are separate decisions, and only the second has to be contended for.
const ADVANCE_SIGNAL_STATE: &str = "INSERT INTO efficiency_signals (signal, scope, streak, last_alerted_at, last_evaluated_at) \
     VALUES (?, ?, ?, NULL, ?) \
     ON CONFLICT (signal, scope) DO UPDATE SET \
     streak = CASE WHEN ? = 1 THEN efficiency_signals.streak + 1 ELSE 0 END, \
     last_evaluated_at = excluded.last_evaluated_at \
     RETURNING streak, last_alerted_at";

/// Claims the right to speak, guarded on the stamp the caller read.
///
/// The same compare-and-swap shape `runs.rs` uses for a run's terminal write, and for the same
/// reason: exactly one writer may win, and only the winner writes the row that becomes a
/// notification. `IS` rather than `=` because the stamp is NULL until the signal first speaks.
const CLAIM_ALERT: &str = "UPDATE efficiency_signals SET last_alerted_at = ? \
                           WHERE signal = ? AND scope = ? AND last_alerted_at IS ?";

/// Loads what runs of this shape have recently cost. Excludes the run being judged, so a run cannot
/// raise the bar it is about to be measured against.
async fn read_baseline(
    pool: &sqlx::SqlitePool,
    measures: &Measures,
    run_id: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<Baseline> {
    let since = (now - Duration::days(BASELINE_MAX_AGE_DAYS)).to_rfc3339();
    // Empty rather than NULL when this run has no session: `session_id <> ''` then excludes nothing,
    // where binding NULL would have made the comparison NULL and dropped every sessioned run.
    let own_session = measures.session_id.as_deref().unwrap_or("");
    let totals: Vec<i64> = sqlx::query_scalar(SELECT_BASELINE_TOTALS)
        .bind(&measures.mode)
        .bind(&measures.project_id)
        .bind(run_id)
        .bind(own_session)
        .bind(&since)
        .bind(BASELINE_WINDOW)
        .fetch_all(pool)
        .await?;

    Ok(Baseline::from_totals(&totals))
}

/// Reads one finished run, judges it, and updates what the detector remembers about each signal.
///
/// Deliberately thin, and deliberately called best-effort from the caller: this runs on the path
/// that finalises a run, and an efficiency observation must never be the reason a run fails to
/// record that it finished.
pub async fn observe_run(pool: &sqlx::SqlitePool, run_id: i64) -> sqlx::Result<()> {
    let measures: Option<Measures> = sqlx::query_as(SELECT_MEASURES)
        .bind(run_id)
        .fetch_optional(pool)
        .await?;
    // A row a concurrent terminator has already moved on from is silence, not a failure.
    let Some(measures) = measures else {
        return Ok(());
    };

    let thresholds = Thresholds::default();
    let now = Utc::now();
    // Only drift reads the baseline, and only when this run has a total to compare. Skipping the
    // 30-day scan otherwise keeps the unmeasurable run — which is the common one on a Codex or
    // local-model daemon — off the query entirely instead of paying for a result it discards.
    let baseline = if measures.total_prompt_tokens().is_some() {
        read_baseline(pool, &measures, run_id, now).await?
    } else {
        Baseline::default()
    };
    let scope = measures.scope();
    let stamp = now.to_rfc3339();

    for (signal, verdict) in evaluate(&measures, &baseline, &thresholds) {
        // The row is left exactly as it was. A run that cannot answer the question has not answered
        // it in the negative, and zeroing a streak here would let unmeasurable runs erase the
        // evidence of a regression that is still there.
        if verdict == Verdict::Undecidable {
            continue;
        }
        let held = verdict == Verdict::Firing;

        let advanced: Option<(i64, Option<String>)> = sqlx::query_as(ADVANCE_SIGNAL_STATE)
            .bind(signal.key())
            .bind(&scope)
            .bind(i64::from(held))
            .bind(&stamp)
            .bind(i64::from(held))
            .fetch_optional(pool)
            .await?;
        let Some((streak, last_alerted_at)) = advanced else {
            continue;
        };

        if !held {
            continue;
        }
        let last_alerted = last_alerted_at
            .as_deref()
            .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
            .map(|stamp| stamp.with_timezone(&Utc));
        if !should_alert(streak, last_alerted, now, &thresholds) {
            continue;
        }

        // Claim first, speak second. If another run got here in the same instant, its UPDATE moved
        // the stamp and this one matches zero rows — so the feed row, which IS the notification, is
        // written by exactly one of them.
        let claimed = sqlx::query(CLAIM_ALERT)
            .bind(&stamp)
            .bind(signal.key())
            .bind(&scope)
            .bind(&last_alerted_at)
            .execute(pool)
            .await?
            .rows_affected()
            == 1;
        if !claimed {
            continue;
        }

        // Through the waiting room rather than straight to the feed. Governance alerts — kill
        // switch, budget — are immediate by definition; an efficiency observation is not
        // governance, and it can wait for the calendar to say the person is free.
        //
        // Logged rather than propagated: a `?` here would abandon every signal after this one for
        // this run, and the streaks they were about to record with it.
        if let Err(error) = crate::notify::deliver_or_defer(
            pool,
            FEED_KIND,
            &summarise(signal, &measures, &baseline, streak),
        )
        .await
        {
            tracing::warn!(%error, signal = signal.key(), "efficiency alert not delivered");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cacheable_run() -> Measures {
        Measures {
            mode: "real".to_string(),
            status: "completed".to_string(),
            project_id: None,
            session_id: None,
            successor_run_id: None,
            input_tokens: Some(50_000),
            cache_read_tokens: Some(0),
            cache_creation_tokens: Some(0),
            num_turns: None,
            context_fill: None,
        }
    }

    /// Judged without a past, which is how every signal except drift is judged. Only what FIRES —
    /// the tests that care about the difference between quiet and unanswerable read `verdict`.
    fn judge(measures: &Measures) -> Vec<Signal> {
        firing(&evaluate(
            measures,
            &Baseline::default(),
            &Thresholds::default(),
        ))
    }

    fn firing(verdicts: &[(Signal, Verdict)]) -> Vec<Signal> {
        verdicts
            .iter()
            .filter(|(_, verdict)| *verdict == Verdict::Firing)
            .map(|(signal, _)| *signal)
            .collect()
    }

    fn verdict(measures: &Measures, signal: Signal) -> Verdict {
        evaluate(measures, &Baseline::default(), &Thresholds::default())
            .into_iter()
            .find(|(candidate, _)| *candidate == signal)
            .expect("every signal is judged on every run")
            .1
    }

    /// A baseline steady enough to be worth comparing against: 40 runs, all near 10_000, with just
    /// enough spread to clear the MAD floor.
    fn steady_baseline() -> Baseline {
        let totals = (0..40)
            .map(|index| 10_000 + (index % 8) * 1_000)
            .collect::<Vec<_>>();
        Baseline::from_totals(&totals)
    }

    #[test]
    fn cache_cold_fires_when_nothing_read_or_written() {
        let firing = judge(&cacheable_run());

        assert_eq!(firing, vec![Signal::CacheCold]);
    }

    #[test]
    fn a_run_that_paid_to_fill_the_cache_is_not_cold() {
        let measures = Measures {
            cache_creation_tokens: Some(50_000),
            input_tokens: Some(0),
            ..cacheable_run()
        };

        // The whole reason 0066 exists. Reads are zero here and the run is behaving perfectly: it
        // bought the first copy so the next run could read it.
        assert!(judge(&measures).is_empty());
    }

    #[test]
    fn cache_cold_is_silent_below_the_cacheable_minimum() {
        let measures = Measures {
            input_tokens: Some(500),
            ..cacheable_run()
        };

        // Below the minimum prefix the API does not cache and does not say so. Firing here would
        // report a defect in the one range where there is none — and answering `no` would be almost
        // as wrong, because a run too small to be cacheable is no evidence either way.
        assert_eq!(verdict(&measures, Signal::CacheCold), Verdict::Undecidable);
    }

    #[test]
    fn unknown_measures_signal_nothing() {
        // The shape a local-model run has: it reports no usage at all. Three separate absences,
        // each of which alone must be enough to disqualify the reading. `Undecidable` and not
        // `Quiet`, because a missing measure is a question this run cannot answer.
        for measures in [
            Measures {
                input_tokens: None,
                ..cacheable_run()
            },
            Measures {
                cache_read_tokens: None,
                ..cacheable_run()
            },
            // Every Codex run: that stream reports cache reads and never cache writes.
            Measures {
                cache_creation_tokens: None,
                ..cacheable_run()
            },
        ] {
            assert_eq!(
                verdict(&measures, Signal::CacheCold),
                Verdict::Undecidable,
                "{measures:?}"
            );
            assert!(judge(&measures).is_empty(), "{measures:?}");
        }
    }

    #[test]
    fn an_unmeasurable_run_is_undecidable_and_a_measured_one_is_quiet() {
        // The distinction the whole three-state verdict exists for. Both are "not firing"; only the
        // second is evidence, and only the second may reset a streak.
        let warm = Measures {
            cache_read_tokens: Some(49_000),
            ..cacheable_run()
        };
        assert_eq!(verdict(&warm, Signal::CacheCold), Verdict::Quiet);

        let unmeasured = Measures {
            input_tokens: None,
            ..cacheable_run()
        };
        assert_eq!(
            verdict(&unmeasured, Signal::CacheCold),
            Verdict::Undecidable
        );
    }

    #[test]
    fn context_swelling_reports_the_handoff_that_did_not_happen() {
        let swollen = Measures {
            mode: "worktree".to_string(),
            context_fill: Some(500_000),
            ..Measures::default()
        };

        assert_eq!(judge(&swollen), vec![Signal::ContextSwelling]);

        // The handoff fires at four fifths of `HANDOFF_CONTEXT_LIMIT_FLOOR` — 160_000 — and records
        // a successor. A run that has one swelled and was ANSWERED; reporting it would name a
        // threshold the daemon enforces itself, about a run whose successor is in the same feed.
        let handed_off = Measures {
            successor_run_id: Some(77),
            ..swollen.clone()
        };
        assert_eq!(
            verdict(&handed_off, Signal::ContextSwelling),
            Verdict::Quiet
        );

        // Chats rotate in place and never record a successor, so the evidence this signal reads
        // does not exist for them.
        let chat = Measures {
            mode: "assistant".to_string(),
            ..swollen
        };
        assert_eq!(
            verdict(&chat, Signal::ContextSwelling),
            Verdict::Undecidable
        );
    }

    #[test]
    fn cost_drift_needs_a_sample() {
        let expensive = Measures {
            input_tokens: Some(1_000_000),
            ..cacheable_run()
        };
        let thresholds = Thresholds::default();

        // Nineteen runs is a median that still moves under its own noise. Twenty is the line drawn
        // in `MIN_BASELINE_SAMPLE`, and the same run crosses it without anything else changing.
        let short = Baseline::from_totals(
            &(0..19)
                .map(|i| 10_000 + (i % 8) * 1_000)
                .collect::<Vec<_>>(),
        );
        // Undecidable, not quiet: a window too thin to trust is an unanswered question, and letting
        // it reset the streak would mean a fresh mode could never accumulate one.
        assert_eq!(
            evaluate(&expensive, &short, &thresholds)[3].1,
            Verdict::Undecidable
        );
        assert!(
            firing(&evaluate(&expensive, &steady_baseline(), &thresholds))
                .contains(&Signal::CostDrift)
        );
    }

    #[test]
    fn a_zero_mad_signals_nothing() {
        let thresholds = Thresholds::default();
        // Forty identical runs — a quiet daemon replaying the same job. The median is solid and the
        // dispersion is nothing, so every other value is infinitely far from it.
        let motionless = Baseline::from_totals(&[10_000; 40]);
        assert_eq!(motionless.mad, Some(0));

        let barely_different = Measures {
            input_tokens: Some(10_001),
            ..cacheable_run()
        };

        // Without the floor this is drift by any distance-in-MADs reading, which is exactly the
        // failure the floor exists to prevent.
        assert_eq!(
            evaluate(&barely_different, &motionless, &thresholds)[3].1,
            Verdict::Undecidable
        );
    }

    #[test]
    fn a_run_at_the_median_is_not_drift() {
        let ordinary = Measures {
            input_tokens: Some(13_000),
            ..cacheable_run()
        };

        // Quiet, not undecidable — this one was measured against a trustworthy window and found
        // ordinary, which is the answer that may reset a streak.
        assert_eq!(
            evaluate(&ordinary, &steady_baseline(), &Thresholds::default())[3].1,
            Verdict::Quiet
        );
    }

    #[test]
    fn an_interrupted_run_is_not_inefficient() {
        let thrashed = Measures {
            status: "failed".to_string(),
            num_turns: Some(30),
            ..cacheable_run()
        };

        assert!(judge(&thrashed).contains(&Signal::OutputStarved));

        // Read from `status` and never from `exit_code`: a run the progress deadline killed carries
        // `PROGRESS_TIMEOUT_EXIT_CODE`, which on its own distinguishes nothing at all. Undecidable
        // rather than quiet — a run stopped from outside produced little for a reason that has
        // nothing to do with efficiency, so it is no evidence either way.
        for status in ["cancelled", "interrupted", "timed_out"] {
            let stopped = Measures {
                status: status.to_string(),
                ..thrashed.clone()
            };
            assert_eq!(
                verdict(&stopped, Signal::OutputStarved),
                Verdict::Undecidable,
                "{status} produced little because it was stopped, not because it wasted anything"
            );
        }
    }

    #[test]
    fn a_long_run_that_succeeded_is_not_starved() {
        let long_and_useful = Measures {
            status: "completed".to_string(),
            num_turns: Some(30),
            ..cacheable_run()
        };

        // Thirty turns that ended in success are thirty turns of work. The signal is about turns
        // that bought nothing, and it deliberately carries no prompt-to-output ratio: the `result`
        // event reports the CUMULATIVE session total, so a worktree run that works up to the
        // handoff line and ends with a three-line summary would read as waste under any such ratio.
        assert_eq!(
            verdict(&long_and_useful, Signal::OutputStarved),
            Verdict::Quiet
        );
    }

    #[test]
    fn the_silence_window_blocks_a_second_alert() {
        let thresholds = Thresholds::default();
        let now = Utc::now();
        let spoke_recently = now - Duration::hours(1);
        let spoke_yesterday = now - Duration::hours(25);

        assert!(should_alert(4, None, now, &thresholds));
        assert!(!should_alert(4, Some(spoke_recently), now, &thresholds));
        assert!(should_alert(4, Some(spoke_yesterday), now, &thresholds));
        // The streak gate comes first: a signal that stopped holding cannot speak however long it
        // has been quiet.
        assert!(!should_alert(3, Some(spoke_yesterday), now, &thresholds));
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

    /// A finished run whose numbers make `CacheCold` true.
    async fn insert_cold_run(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, created_at, completed_at, input_tokens, \
             cache_read_tokens, cache_creation_tokens) \
             VALUES ('p', 'completed', 'real', ?, ?, 50000, 0, 0) RETURNING id",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn feed_rows(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = ?")
            .bind(FEED_KIND)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_streak_below_the_threshold_writes_nothing() {
        let pool = test_pool().await;

        for _ in 0..(STREAK_BEFORE_ALERT - 1) {
            let run_id = insert_cold_run(&pool).await;
            observe_run(&pool, run_id).await.unwrap();
        }

        // The signal has been true every single time and still nobody has been told, which is the
        // entire point: one cold run is noise, and so are three.
        assert_eq!(feed_rows(&pool).await, 0);

        let run_id = insert_cold_run(&pool).await;
        observe_run(&pool, run_id).await.unwrap();
        assert_eq!(feed_rows(&pool).await, 1);
    }

    #[tokio::test]
    async fn the_silence_window_survives_the_database() {
        let pool = test_pool().await;

        // Past the threshold and speaking.
        for _ in 0..STREAK_BEFORE_ALERT {
            let run_id = insert_cold_run(&pool).await;
            observe_run(&pool, run_id).await.unwrap();
        }
        assert_eq!(feed_rows(&pool).await, 1);

        // Still true, still climbing, and silent. `should_alert` is pure and tested on its own; what
        // this covers is the half that is not — that the stamp reached the table, came back, and
        // that the CAS which claims the right to speak refuses the second claimant.
        for _ in 0..3 {
            let run_id = insert_cold_run(&pool).await;
            observe_run(&pool, run_id).await.unwrap();
        }
        assert_eq!(feed_rows(&pool).await, 1);

        let streak: i64 = sqlx::query_scalar(
            "SELECT streak FROM efficiency_signals \
              WHERE signal = 'cache_cold' AND scope = 'global|real'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            streak, 7,
            "the streak keeps counting while the window holds"
        );
    }

    #[tokio::test]
    async fn an_unmeasurable_run_leaves_the_streak_alone() {
        let pool = test_pool().await;

        for _ in 0..2 {
            let run_id = insert_cold_run(&pool).await;
            observe_run(&pool, run_id).await.unwrap();
        }

        // The shape a progress-deadline kill has: it never emitted a `result` event, so nothing
        // about its cache usage is known. It is not evidence that the cache got warmer.
        let unmeasured: i64 = sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, created_at, completed_at) \
             VALUES ('p', 'timed_out', 'real', ?, ?) RETURNING id",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(Utc::now().to_rfc3339())
        .fetch_one(&pool)
        .await
        .unwrap();
        observe_run(&pool, unmeasured).await.unwrap();

        let streak: i64 = sqlx::query_scalar(
            "SELECT streak FROM efficiency_signals \
              WHERE signal = 'cache_cold' AND scope = 'global|real'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            streak, 2,
            "an unmeasurable run must not erase what the measurable ones established"
        );
    }

    #[tokio::test]
    async fn a_streak_survives_a_restart() {
        let pool = test_pool().await;

        // Separate `observe_run` calls hold nothing between them — whatever the streak is, it came
        // back out of the table. A counter living in the process would let a permanent regression
        // stay permanently below the threshold, one daemon restart at a time.
        for _ in 0..2 {
            let run_id = insert_cold_run(&pool).await;
            observe_run(&pool, run_id).await.unwrap();
        }

        let streak: i64 = sqlx::query_scalar(
            "SELECT streak FROM efficiency_signals WHERE signal = 'cache_cold' AND scope = 'global|real'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(streak, 2);
    }

    #[tokio::test]
    async fn a_signal_that_stops_holding_loses_its_streak() {
        let pool = test_pool().await;

        let cold = insert_cold_run(&pool).await;
        observe_run(&pool, cold).await.unwrap();

        let warm: i64 = sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, created_at, input_tokens, \
             cache_read_tokens, cache_creation_tokens) \
             VALUES ('p', 'completed', 'real', ?, 100, 49900, 0) RETURNING id",
        )
        .bind(Utc::now().to_rfc3339())
        .fetch_one(&pool)
        .await
        .unwrap();
        observe_run(&pool, warm).await.unwrap();

        let streak: i64 = sqlx::query_scalar(
            "SELECT streak FROM efficiency_signals WHERE signal = 'cache_cold' AND scope = 'global|real'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(streak, 0);
    }

    #[tokio::test]
    async fn the_baseline_reads_recent_runs_of_the_same_shape() {
        let pool = test_pool().await;
        let stamp = Utc::now().to_rfc3339();

        // A population wide enough to clear both guards, plus one run of a different mode that must
        // not be counted: `real` and `assistant` runs are not the same kind of work and pooling them
        // would make either look like drift against the other.
        for index in 0..25 {
            insert_run(&pool, "real", 10_000 + (index % 8) * 1_000, &stamp).await;
        }
        insert_run(&pool, "assistant", 9_000_000, &stamp).await;

        let expensive = insert_run(&pool, "real", 5_000_000, &stamp).await;
        let measures: Measures = sqlx::query_as(SELECT_MEASURES)
            .bind(expensive)
            .fetch_one(&pool)
            .await
            .unwrap();

        let baseline = read_baseline(&pool, &measures, expensive, Utc::now())
            .await
            .unwrap();

        // 25, not 26 — the run being judged is excluded, so it cannot raise the bar it is about to
        // be measured against.
        assert_eq!(baseline.sample, 25);
        assert_eq!(baseline.median_total, Some(13_000));
        assert!(
            firing(&evaluate(&measures, &baseline, &Thresholds::default()))
                .contains(&Signal::CostDrift)
        );
    }

    /// A finished run of a given mode whose measured total is exactly `total`.
    async fn insert_run(pool: &sqlx::SqlitePool, mode: &str, total: i64, stamp: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, created_at, completed_at, input_tokens, \
             cache_read_tokens, cache_creation_tokens) \
             VALUES ('p', 'completed', ?, ?, ?, ?, 0, 0) RETURNING id",
        )
        .bind(mode)
        .bind(stamp)
        .bind(stamp)
        .bind(total)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_missing_run_is_not_an_error() {
        let pool = test_pool().await;

        // Called best-effort from the finalisation path, against a row a concurrent terminator may
        // already have moved on. Silence is the correct answer, not a failure.
        observe_run(&pool, 4242).await.unwrap();
    }
}
