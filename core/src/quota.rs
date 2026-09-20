//! §spec notch-de-quota
//!
//! How much of each assistant's usage limit the owner has burned, and what that is called.
//!
//! The domain, deliberately apart from `budget.rs` (design D10). The two measure different things —
//! `budget.rs` counts dollars per period out of this daemon's own `runs` rows, this counts a
//! vendor's windows out of an external reading — and they fail in opposite directions. Grafting a
//! second brake onto the first would have turned each of that function's eleven call sites into a
//! place asking two questions with two failure directions tangled in one return value.
//!
//! This module holds no HTTP: `quota_client.rs` is the transport, exactly as `web_client.rs` is for
//! `web.rs`. And it holds no provider credential — see design D2, whose whole point is that the
//! token stays inside the sidecar.
//!
//! **This module draws and it speaks; it does not act.** Phase 3 added the warning (D11) — a feed
//! line when a measured window crosses one of the owner's thresholds — and the brake is still
//! phase 4's. The order is the point: a threshold gets to interrupt the owner long before it gets
//! to stop their work, so the numbers can be watched being wrong at the cost of a ping rather than
//! at the cost of a night's autonomous work.

use serde::Serialize;
use sqlx::SqlitePool;

/// The vocabulary of the `quota` state domain, named once in the language the shell paints from.
///
/// Read by `shell/src/ui/state-map-completeness.test.ts`, which asserts that `statesOf("quota")` is
/// exactly this set. That test is why the array is a `pub const` with a declared length and a
/// literal on one line: the test greps this file, and a value it cannot read is a state nobody is
/// checking. A ring whose state has no entry in the map falls back to a neutral tone, which is the
/// exact failure `readState` exists to prevent.
pub const QUOTA_STATES: [&str; 5] = ["ok", "warn", "exhausted", "stale", "unmeasured"];

/// The five, named. Indices into [`QUOTA_STATES`] rather than five more literals, so the array
/// above is the only place in this process where these words are spelled — a second spelling is
/// how the Rust side and the shell's map drift apart without either of them being wrong on its own.
const OK: &str = QUOTA_STATES[0];
const WARN: &str = QUOTA_STATES[1];
const EXHAUSTED: &str = QUOTA_STATES[2];
const STALE: &str = QUOTA_STATES[3];
const UNMEASURED: &str = QUOTA_STATES[4];

/// Where a window stops being comfortable.
///
/// Constants in phase 1 and settings in phase 4 (the `QuotaPolicy` migration), in that order and not
/// the other way round: a threshold the owner can move is only worth building once somebody has
/// watched the fixed one for a while and can say what it should have been.
const WARN_AT: f64 = 0.75;
const EXHAUSTED_AT: f64 = 0.95;

/// Where the owner gets told, in per cent (`warn_at_percent`, design D9's default).
///
/// A constant here, and a row of the `QuotaPolicy` table in phase 4 — the order the module doc
/// above already committed to, and the order D9 itself implies by making this a setting of a policy
/// whose migration belongs to the brake. A settings page for a threshold nobody has yet watched
/// fire is a page built before its question is known.
///
/// Separate from [`WARN_AT`] and [`EXHAUSTED_AT`] on purpose, and this is the one thing about this
/// pair worth reading twice: those two colour a ring that somebody is looking at, so they may be
/// generous; these two interrupt somebody who is not, so they are not the same numbers and must not
/// become one set by tidying. 100 is a threshold and not a rounding artefact — the window is spent,
/// which is precisely when a person far from the screen wants to hear about it.
///
/// Ascending, and [`crossed_threshold`] relies on it.
const WARN_AT_PERCENT: [i64; 2] = [80, 100];

/// The feed kind of a quota warning.
///
/// Named here, above the `#[cfg(test)]` cut and in the file that emits it, because
/// `shell/src/ui/state-map-completeness.test.ts` rebuilds the map of kinds per file: it reads
/// `const NAME: &str = "…"` beside the `append` that uses it, and a kind whose spelling lives in
/// another module lands in that test's `unresolved` list instead of being checked at all.
const FEED_KIND: &str = "quota_warning";

/// How much the number is worth, and therefore what a later phase may do with it (design D3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Fidelity {
    /// The provider's own endpoint said so.
    Official,
    /// Computed from files the provider left on this machine. True when last written.
    Derived,
    /// The provider is there and no quota source could be read. Never a zero — absence and zero per
    /// cent are different facts, and a brake must never act on this one.
    Unmeasured,
}

impl Fidelity {
    /// Read the sidecar's word, defaulting to [`Fidelity::Unmeasured`].
    ///
    /// An unrecognised value is unmeasured rather than an error, and the direction is the safe one:
    /// a fidelity this build cannot name is one it cannot judge, so it must not be trusted by
    /// whatever later reads it. The opposite default would let a sidecar typo authorise a brake.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "official" => Fidelity::Official,
            "derived" => Fidelity::Derived,
            _ => Fidelity::Unmeasured,
        }
    }

    pub fn as_db_str(self) -> &'static str {
        match self {
            Fidelity::Official => "official",
            Fidelity::Derived => "derived",
            Fidelity::Unmeasured => "unmeasured",
        }
    }
}

/// One limit period of one provider, as the shell draws it.
#[derive(Debug, Clone, Serialize)]
pub struct Window {
    /// `5h` or `7d`.
    pub window: String,
    pub used_fraction: f64,
    pub resets_at: Option<String>,
    pub stale: bool,
    /// One of [`QUOTA_STATES`]. Computed HERE and sent to the shell rather than recomputed there,
    /// because the thresholds become the owner's settings in phase 4 and a copy of them in
    /// TypeScript would be a second policy that changes on a different schedule.
    pub state: &'static str,
}

/// One provider's quota.
#[derive(Debug, Clone, Serialize)]
pub struct Provider {
    pub provider: String,
    pub fidelity: Fidelity,
    pub read_at: String,
    pub windows: Vec<Window>,
    /// Why this reading is `unmeasured`, in the owner's words. Empty otherwise.
    pub detail: String,
    /// The vendor's own word for how bad this is. Shown and never acted on: this design's states
    /// come from the owner's thresholds, and borrowing a vendor's vocabulary would eventually hand
    /// a brake to a word nobody here controls.
    pub severity: String,
}

/// Where the answer came from, which is a fact the notch shows rather than hides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// A live answer from the sidecar.
    Sidecar,
    /// The sidecar could not be reached and these are the last readings that were stored. The
    /// figures are real; their age is what the reader has to weigh, which is why `read_at` travels
    /// on every provider.
    Stored,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuotaReport {
    pub providers: Vec<Provider>,
    pub source: Source,
    /// The sidecar answered out of its own TTL window rather than calling the vendor. Passed
    /// through because `read_at` alone cannot tell "measured a second ago" from "measured a minute
    /// ago and held", and the notch shows the age of a figure rather than implying it is fresh.
    pub cached: bool,
    /// Present only when the sidecar could not be reached. The notch says so instead of drawing an
    /// empty ring, because an empty ring and an unanswered question look identical and mean
    /// opposite things.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreachable: Option<String>,
}

/// The quota pillar's runtime: the client for the sidecar, and nothing else.
///
/// No thresholds and no provider list. The thresholds are constants above until phase 4 puts them in
/// the database, and the provider list is whatever the sidecar answers — a runtime copy would be a
/// second list that only changes when the daemon restarts.
#[derive(Debug, Clone)]
pub struct QuotaRuntime {
    pub client: Option<crate::quota_client::QuotaClient>,
}

impl QuotaRuntime {
    pub fn new(client: crate::quota_client::QuotaClient) -> Self {
        Self {
            client: Some(client),
        }
    }

    /// No sidecar: `GET /quota` answers from the table, or says it has nothing.
    ///
    /// Named rather than derived, and `#[cfg(test)]`, for the two reasons
    /// `web::WebRuntime::disabled` gives: a derived default would invent a client pointing at
    /// nothing, so "off" should be a thing somebody chose — and production always builds a real
    /// one, so left ungated this is dead code in the daemon.
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self { client: None }
    }
}

/// PURE: what this window is called in the `quota` state domain.
///
/// The order of the arms is the decision. Fidelity comes first because an unmeasured reading has no
/// number worth colouring — painting it `exhausted` because its absent figure defaulted to zero, or
/// `ok` for the same reason, are both a lie with a colour on it. `stale` comes next because a window
/// past its own reset describes a period that has ended, whatever the figure was.
pub fn state_of(fidelity: Fidelity, used_fraction: f64, stale: bool) -> &'static str {
    if fidelity == Fidelity::Unmeasured {
        return UNMEASURED;
    }
    if stale {
        return STALE;
    }
    if used_fraction >= EXHAUSTED_AT {
        return EXHAUSTED;
    }
    if used_fraction >= WARN_AT {
        return WARN;
    }
    OK
}

/// Ask the sidecar, store what it said, and fall back to the table when it cannot be reached.
///
/// The fallback is the reason the table exists. A daemon that has just restarted, or a sidecar still
/// starting, would otherwise draw an empty notch — which reads as "nothing is burned" and is the
/// single most misleading thing this feature could say.
pub async fn report(
    runtime: &QuotaRuntime,
    pool: &SqlitePool,
    now: chrono::DateTime<chrono::Utc>,
) -> QuotaReport {
    let Some(client) = runtime.client.as_ref() else {
        return stored_report(pool, "the quota sidecar is not running").await;
    };

    match client.report().await {
        Ok(live) => live_report(pool, live, now).await,
        Err(error) => {
            // The sidecar's own message, which by construction carries no token — see
            // `sidecars/quota/claude`, where that property has a test of its own.
            stored_report(pool, &error.to_string()).await
        }
    }
}

/// Everything that happens to a reading the sidecar has just answered: store it, judge it, return
/// it.
///
/// Split out of [`report`] so that it can be tested without an HTTP server. That is not tidiness:
/// every test of `report` uses [`QuotaRuntime::disabled`], which returns at the guard above before
/// either of these two calls, so with the body inline the storing and the warning are wired by
/// lines no test executes — and deleting either call left the suite green.
async fn live_report(
    pool: &SqlitePool,
    live: crate::quota_client::QuotaReport,
    now: chrono::DateTime<chrono::Utc>,
) -> QuotaReport {
    let cached = live.cached;
    let providers: Vec<Provider> = live.providers.into_iter().map(from_reading).collect();
    // Recorded before answering, and a write that fails does NOT fail the answer: the table is a
    // fallback, so losing it costs the next reader a fresh figure and must not cost this one the
    // figure already in hand.
    if let Err(error) = record(pool, &providers, now).await {
        tracing::warn!(%error, "the quota reading could not be stored");
    }
    // Judged here, on the live path only, because this is the one place a NEW reading arrives —
    // every caller of this module comes through it, so a warning cannot be skipped by a future
    // second caller forgetting to ask for one.
    //
    // The stored fallback deliberately does not warn: those figures were already judged when they
    // were read, and re-judging them would warn about a window that may well have rolled over while
    // the sidecar was down.
    //
    // Best-effort, exactly like the write above: a warning that cannot be recorded must not cost
    // the reader the figure already in hand. `observe_run` is called the same way, for the same
    // reason.
    if let Err(error) = warn(pool, &providers, now).await {
        tracing::warn!(%error, "the quota warning was not delivered");
    }
    QuotaReport {
        providers,
        source: Source::Sidecar,
        cached,
        unreachable: None,
    }
}

async fn stored_report(pool: &SqlitePool, why: &str) -> QuotaReport {
    let providers = match stored(pool).await {
        Ok(providers) => providers,
        Err(error) => {
            tracing::warn!(%error, "the stored quota readings could not be read");
            Vec::new()
        }
    };
    QuotaReport {
        providers,
        source: Source::Stored,
        // A stored reading is not a cache hit at the sidecar: it is the sidecar not having been
        // asked at all. Saying `true` here would have the notch blame the wrong side.
        cached: false,
        unreachable: Some(why.to_string()),
    }
}

/// Turn one sidecar reading into the shape the shell is sent.
fn from_reading(raw: crate::quota_client::ProviderReading) -> Provider {
    let fidelity = Fidelity::parse(&raw.fidelity);
    Provider {
        provider: raw.provider,
        fidelity,
        read_at: raw.read_at.to_rfc3339(),
        windows: raw
            .windows
            .into_iter()
            .map(|w| Window {
                state: state_of(fidelity, w.used_fraction, w.stale),
                window: w.window,
                used_fraction: w.used_fraction,
                resets_at: w.resets_at.map(|at| at.to_rfc3339()),
                stale: w.stale,
            })
            .collect(),
        detail: raw.detail,
        severity: raw.severity,
    }
}

/// Replace each measured window's stored reading.
///
/// Unmeasured providers write NOTHING, and do not erase what is there either. The table's job is to
/// answer "what was the last real figure"; a provider whose token expired an hour ago has not
/// changed its burn, it has stopped being readable, and forgetting the figure would turn a
/// recoverable outage into a notch that has to start again from empty.
pub async fn record(
    pool: &SqlitePool,
    providers: &[Provider],
    _now: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    for provider in providers {
        if provider.fidelity == Fidelity::Unmeasured {
            continue;
        }
        for window in &provider.windows {
            sqlx::query(
                "INSERT INTO quota_readings
                     (provider, window_name, used_fraction, resets_at, fidelity, stale, read_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (provider, window_name) DO UPDATE SET
                     used_fraction = excluded.used_fraction,
                     resets_at     = excluded.resets_at,
                     fidelity      = excluded.fidelity,
                     stale         = excluded.stale,
                     read_at       = excluded.read_at",
            )
            .bind(&provider.provider)
            .bind(&window.window)
            // Clamped at the boundary rather than trusted, because this column has a CHECK and a
            // rejected write would lose the whole reading over one out-of-range figure. The unit
            // conversion that makes this safe lives in the sidecar; this is the belt.
            .bind(window.used_fraction.clamp(0.0, 1.0))
            .bind(window.resets_at.as_deref())
            .bind(provider.fidelity.as_db_str())
            .bind(i64::from(window.stale))
            .bind(&provider.read_at)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// One row of `quota_readings`, named rather than read as a seven-place tuple: a tuple of three
/// `String`s and an `Option<String>` is one reordered `SELECT` away from swapping the provider with
/// the fidelity, and nothing would say so.
#[derive(sqlx::FromRow)]
struct Row {
    provider: String,
    window_name: String,
    used_fraction: f64,
    resets_at: Option<String>,
    fidelity: String,
    stale: i64,
    read_at: String,
}

/// The last stored reading of every provider.
pub async fn stored(pool: &SqlitePool) -> Result<Vec<Provider>, sqlx::Error> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT provider, window_name, used_fraction, resets_at, fidelity, stale, read_at
           FROM quota_readings
          ORDER BY provider, window_name",
    )
    .fetch_all(pool)
    .await?;

    let mut providers: Vec<Provider> = Vec::new();
    for row in rows {
        let fidelity = Fidelity::parse(&row.fidelity);
        let stale = row.stale != 0;
        let window = Window {
            state: state_of(fidelity, row.used_fraction, stale),
            window: row.window_name,
            used_fraction: row.used_fraction,
            resets_at: row.resets_at,
            stale,
        };
        let (provider, read_at) = (row.provider, row.read_at);
        match providers.iter_mut().find(|p| p.provider == provider) {
            Some(existing) => existing.windows.push(window),
            None => providers.push(Provider {
                provider,
                fidelity,
                read_at,
                windows: vec![window],
                detail: String::new(),
                // Neither travels into the table: `detail` explains an `unmeasured` reading, which
                // is never stored, and `severity` is the vendor's word about a moment that has
                // passed. Restoring either from a row would put stale prose beside a figure the
                // reader is already being asked to weigh by its age.
                severity: String::new(),
            }),
        }
    }
    Ok(providers)
}

/// PURE: the highest threshold this figure has crossed and nobody has announced yet.
///
/// `already_announced` is the highest threshold already said FOR THIS WINDOW INSTANCE — not for
/// this window — which is what lets the same 80% be news again after the window rolls over.
///
/// The highest and not the lowest: a reading that jumps straight from 40% to 100% (a council
/// firing three seats at once is exactly that shape) should say the true thing once, not walk the
/// ladder with a ping per rung.
fn crossed_threshold(used_fraction: f64, already_announced: Option<i64>) -> Option<i64> {
    let used_percent = used_fraction * 100.0;
    WARN_AT_PERCENT.iter().rev().copied().find(|threshold| {
        used_percent >= *threshold as f64 && already_announced.is_none_or(|said| *threshold > said)
    })
}

/// PURE: whether this reading describes a period that has already ended.
///
/// **The reset instant decides, and the stored flag is only the second opinion.** `stale` is fixed
/// at the moment the reading is taken and is not recomputed when the reset passes, so a figure read
/// at 16:39 is still flagged fresh at 17:00 although its window is gone. Warning from it would tell
/// the owner that a window is spent when it has in fact reopened — the exact failure the fidelity
/// ladder exists to prevent, arriving through the one field that looked trustworthy.
///
/// No reset instant is not evidence of age: the capture of 2026-09-19 carried a populated window
/// with `resets_at: null`, and such a window falls back on the flag.
fn is_outdated(window: &Window, now: chrono::DateTime<chrono::Utc>) -> bool {
    if window.stale {
        return true;
    }
    window
        .resets_at
        .as_deref()
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .is_some_and(|resets_at| resets_at.with_timezone(&chrono::Utc) < now)
}

/// What the owner reads on their phone.
///
/// Names the quota in the first word, because design D9 obligation (a) is that a line from this
/// module must never be mistaken for one from `budget.rs`: the residual question is not *why did
/// work continue*, it is *which of the two ceilings let it*.
///
/// Carries the measured figure as well as the threshold. "you passed 80%" is a claim the reader
/// cannot check; "you passed 80% — 82% of it is gone" is one they can, and the difference between
/// the two numbers is how far past the line the reading already was when it arrived.
fn summarise(provider: &str, window: &Window, threshold: i64) -> String {
    let measured = (window.used_fraction * 100.0).floor() as i64;
    let reset = match window.resets_at.as_deref() {
        Some(at) => format!(", and it resets at {at}"),
        // Said rather than left out. A window whose reset nobody reported is one the owner cannot
        // wait out by a clock, and silence here would read as "resets imminently".
        None => ", and no reset instant came with it".to_string(),
    };
    format!(
        "quota: {provider}'s {} window has passed {threshold}% — {measured}% of it is gone{reset}",
        window.window
    )
}

/// The claim one provider window holds, read before it is contended for.
struct Claim {
    resets_at: Option<String>,
    alerted_percent: i64,
    last_alerted_at: String,
}

async fn read_claim(
    pool: &SqlitePool,
    provider: &str,
    window_name: &str,
) -> Result<Option<Claim>, sqlx::Error> {
    let row: Option<(Option<String>, i64, String)> = sqlx::query_as(
        "SELECT window_resets_at, alerted_percent, last_alerted_at FROM quota_warnings
          WHERE provider = ? AND window_name = ?",
    )
    .bind(provider)
    .bind(window_name)
    .fetch_optional(pool)
    .await?;
    Ok(
        row.map(|(resets_at, alerted_percent, last_alerted_at)| Claim {
            resets_at,
            alerted_percent,
            last_alerted_at,
        }),
    )
}

/// PURE: how long the window this name describes lasts.
///
/// `5h` and `7d` are the two the sidecar sends today, and the shape — a count and a unit — is read
/// generically so that a third window a later sidecar invents is understood rather than silently
/// mishandled. A name this build cannot read answers `None`, and every caller treats that as "do
/// not act on a length I had to guess".
fn window_length(window_name: &str) -> Option<chrono::Duration> {
    let (count, unit) = window_name.split_at(window_name.len().checked_sub(1)?);
    let count: i64 = count.parse().ok()?;
    match unit {
        "h" => Some(chrono::Duration::hours(count)),
        "d" => Some(chrono::Duration::days(count)),
        "m" => Some(chrono::Duration::minutes(count)),
        _ => None,
    }
}

/// PURE: whether the claim on this window has stopped covering the reading in hand.
///
/// Two ways out, and the second is the one that matters.
///
/// **A strictly later reset is a new window.** Not "a different reset": a reset that moves
/// BACKWARDS is the same window reported differently — a `derived` Codex figure recomputed from
/// another rollout, or two readings taken across a clock adjustment — and re-arming on it would ping
/// twice for one burn. Worse, a reset that merely jitters in its spelling would re-arm on every
/// poll, which is a ping a minute: exactly what this table exists to prevent.
///
/// **A claim older than its own window has outlived the window it was made for.** This is what
/// keeps a reset-less window from being silenced for ever. `resets_at` is genuinely absent
/// sometimes — the capture of 2026-09-19 had one, and the Claude sidecar has a fixture of it — and
/// with only the first rule such a window would warn once in the lifetime of the database and never
/// again. Nothing is double-warned by this: a reading whose reset has passed never reaches here,
/// because [`is_outdated`] drops it first.
fn claim_has_expired(claim: &Claim, window: &Window, now: chrono::DateTime<chrono::Utc>) -> bool {
    let instant = |at: Option<&str>| {
        at.and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
            .map(|at| at.with_timezone(&chrono::Utc))
    };
    if let (Some(claimed), Some(fresh)) = (
        instant(claim.resets_at.as_deref()),
        instant(window.resets_at.as_deref()),
    ) && fresh > claimed
    {
        return true;
    }
    match (
        window_length(&window.window),
        instant(Some(&claim.last_alerted_at)),
    ) {
        (Some(length), Some(said_at)) => now.signed_duration_since(said_at) >= length,
        // A window whose name this build cannot read, or a stamp it cannot parse. Holding the claim
        // errs towards silence, which is the direction a warning should err in.
        _ => false,
    }
}

/// Claims the right to say this, guarded on the row the caller read. `true` means speak.
///
/// One statement, and a compare-and-swap rather than a read followed by a write: the notch polls,
/// and two readings landing in the same instant both see the same row, both compute the same
/// crossing, and both would write the feed line whose whole purpose is to be one. The same shape
/// `token_efficiency.rs` uses on `last_alerted_at`, and `runs.rs` on a run's terminal write.
///
/// `IS` and not `=` throughout, because both guarded values are nullable and `= NULL` matches
/// nothing. `previous: None` binds NULL against a column declared `NOT NULL`, which by construction
/// matches no row — so a caller that read no row loses to whoever inserted one in the meantime,
/// which is the outcome wanted.
async fn claim_the_right_to_warn(
    pool: &SqlitePool,
    provider: &str,
    window_name: &str,
    previous: Option<&Claim>,
    threshold: i64,
    resets_at: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<bool, sqlx::Error> {
    let affected = sqlx::query(
        "INSERT INTO quota_warnings
             (provider, window_name, window_resets_at, alerted_percent, last_alerted_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (provider, window_name) DO UPDATE SET
             window_resets_at = excluded.window_resets_at,
             alerted_percent  = excluded.alerted_percent,
             last_alerted_at  = excluded.last_alerted_at
          WHERE quota_warnings.alerted_percent IS ?
            AND quota_warnings.window_resets_at IS ?",
    )
    .bind(provider)
    .bind(window_name)
    .bind(resets_at)
    .bind(threshold)
    .bind(now.to_rfc3339())
    .bind(previous.map(|claim| claim.alerted_percent))
    .bind(previous.and_then(|claim| claim.resets_at.as_deref()))
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected == 1)
}

/// Warn about every measured window that has just crossed a threshold. Returns how many lines went
/// out, which is what the tests assert and what a caller may log.
///
/// **Only measured and only current readings warn — it fails open, in the direction of silence**
/// (design D3/D9/G4). Unmeasured carries no number; outdated carries one about a period that has
/// ended. Both are the same mistake seen twice: a warning is an interruption, and interrupting
/// somebody with a figure that was never true costs more than the warning was worth. The brake in
/// phase 4 reads the same two guards for a heavier reason.
pub async fn warn(
    pool: &SqlitePool,
    providers: &[Provider],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<usize, sqlx::Error> {
    let mut spoken = 0;
    for provider in providers {
        if provider.fidelity == Fidelity::Unmeasured {
            continue;
        }
        for window in &provider.windows {
            if is_outdated(window, now) {
                continue;
            }
            let previous = read_claim(pool, &provider.provider, &window.window).await?;
            // A claim belongs to the window instance it was written for, and stops counting once
            // that instance is over — see `claim_has_expired`, which is where the two ways of being
            // over are argued.
            let already = previous
                .as_ref()
                .filter(|claim| !claim_has_expired(claim, window, now))
                .map(|claim| claim.alerted_percent);
            let Some(threshold) = crossed_threshold(window.used_fraction, already) else {
                continue;
            };
            if !claim_the_right_to_warn(
                pool,
                &provider.provider,
                &window.window,
                previous.as_ref(),
                threshold,
                window.resets_at.as_deref(),
                now,
            )
            .await?
            {
                continue;
            }

            // Straight to the feed, NOT through `notify::deliver_or_defer`'s waiting room.
            //
            // The waiting room holds a notification until the calendar says the person is free,
            // which is right for an efficiency observation and wrong for this: a window that is
            // 100% gone stops being worth saying the moment it resets, so a warning held through a
            // two-hour meeting either arrives about a limit that has since reopened or arrives
            // while the run it was meant to save has already died. This is a ceiling, like the kill
            // switch and the budget, and those are immediate by the same argument.
            //
            // Global scope: a quota is the machine's, not a project's — the burn came from every
            // project at once, so filing it under one would hide it from the others.
            //
            // Logged rather than propagated, which is the same choice `token_efficiency.rs` makes
            // beside its own claim and for a reason worth repeating: a `?` here would abandon every
            // remaining provider and window in this round — in practice the second provider and the
            // `7d` window — over one failed write. The claim above is already committed either way,
            // so this line is lost rather than retried; losing one line is the smaller failure, and
            // the alternative (claim after speaking) trades it for a duplicate ping on every crash
            // between the two.
            if let Err(error) = crate::feed::append(
                pool,
                None,
                FEED_KIND,
                &summarise(&provider.provider, window, threshold),
                None,
                None,
            )
            .await
            {
                tracing::warn!(%error, provider = %provider.provider, window = %window.window, "a quota warning was claimed but not written");
                continue;
            }
            spoken += 1;
        }
    }
    Ok(spoken)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> SqlitePool {
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

    fn window(used: f64, stale: bool) -> Window {
        Window {
            window: "5h".into(),
            used_fraction: used,
            resets_at: Some("2026-09-19T16:40:00+00:00".into()),
            stale,
            state: "ok",
        }
    }

    fn measured(used: f64) -> Vec<Provider> {
        vec![Provider {
            provider: "claude".into(),
            fidelity: Fidelity::Official,
            read_at: "2026-09-19T05:00:00+00:00".into(),
            windows: vec![window(used, false)],
            detail: String::new(),
            severity: String::new(),
        }]
    }

    /// **The invariant the brake will be built on** (design D3), asserted here in phase 1 so that it
    /// is already true — and already guarded — when phase 4 arrives to depend on it.
    ///
    /// An unmeasured reading carries no number, so any number found beside it is noise. The whole
    /// fidelity ladder exists so that a percentage nobody measured can never stop the owner's work,
    /// and this is the single line where that is decided.
    #[test]
    fn an_unmeasured_reading_is_never_called_exhausted_however_high_the_number_beside_it() {
        for used in [0.0, 0.5, 0.99, 1.0] {
            assert_eq!(
                state_of(Fidelity::Unmeasured, used, false),
                "unmeasured",
                "a reading nobody measured was coloured by its own noise at {used}"
            );
        }
    }

    /// A window past its own reset describes a period that has ended. Calling it `exhausted` would
    /// have the notch shout about a limit that has since reopened.
    #[test]
    fn a_stale_window_is_stale_before_it_is_anything_else() {
        assert_eq!(state_of(Fidelity::Official, 0.99, true), "stale");
        assert_eq!(state_of(Fidelity::Derived, 0.01, true), "stale");
    }

    /// The bands, at their edges — the only place a threshold is ever wrong.
    #[test]
    fn the_bands_are_read_at_their_edges() {
        assert_eq!(state_of(Fidelity::Official, 0.0, false), "ok");
        assert_eq!(state_of(Fidelity::Official, 0.7499, false), "ok");
        assert_eq!(state_of(Fidelity::Official, 0.75, false), "warn");
        assert_eq!(state_of(Fidelity::Official, 0.9499, false), "warn");
        assert_eq!(state_of(Fidelity::Official, 0.95, false), "exhausted");
        assert_eq!(state_of(Fidelity::Official, 1.0, false), "exhausted");
    }

    /// Every state this module can produce has to be one the shell knows how to paint.
    #[test]
    fn every_state_this_module_produces_is_in_the_named_vocabulary() {
        for fidelity in [Fidelity::Official, Fidelity::Derived, Fidelity::Unmeasured] {
            for used in [0.0, 0.8, 1.0] {
                for stale in [false, true] {
                    let state = state_of(fidelity, used, stale);
                    assert!(
                        QUOTA_STATES.contains(&state),
                        "{state} is not in QUOTA_STATES, so the shell has no tone for it"
                    );
                }
            }
        }
    }

    /// An unrecognised fidelity must be the one that grants nothing.
    #[test]
    fn a_fidelity_this_build_cannot_name_is_treated_as_unmeasured() {
        assert_eq!(Fidelity::parse("manual"), Fidelity::Unmeasured);
        assert_eq!(Fidelity::parse(""), Fidelity::Unmeasured);
        assert_eq!(Fidelity::parse("official"), Fidelity::Official);
    }

    #[tokio::test]
    async fn a_stored_reading_comes_back_as_it_went_in() {
        let pool = pool().await;
        record(&pool, &measured(0.56), chrono::Utc::now())
            .await
            .unwrap();

        let back = stored(&pool).await.unwrap();

        assert_eq!(back.len(), 1);
        assert_eq!(back[0].provider, "claude");
        assert_eq!(back[0].fidelity, Fidelity::Official);
        assert_eq!(back[0].windows.len(), 1);
        assert!((back[0].windows[0].used_fraction - 0.56).abs() < f64::EPSILON);
        assert_eq!(back[0].windows[0].state, "ok");
    }

    /// Reading the same window twice must leave one row, not two. The table answers "the last
    /// figure", and a second row for the same window would make that question ambiguous within a
    /// minute of the daemon starting.
    #[tokio::test]
    async fn a_second_reading_of_the_same_window_replaces_the_first() {
        let pool = pool().await;
        record(&pool, &measured(0.10), chrono::Utc::now())
            .await
            .unwrap();
        record(&pool, &measured(0.80), chrono::Utc::now())
            .await
            .unwrap();

        let back = stored(&pool).await.unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].windows.len(), 1);
        assert!((back[0].windows[0].used_fraction - 0.80).abs() < f64::EPSILON);
        assert_eq!(back[0].windows[0].state, "warn");
    }

    /// An outage must not erase the last real figure.
    ///
    /// A provider whose token expired has not changed its burn — it has stopped being readable. If
    /// an unmeasured reading wrote over the table, the notch would restart from empty after every
    /// expiry, which is the fallback failing at exactly the moment it is the only thing left.
    #[tokio::test]
    async fn an_unmeasured_reading_does_not_erase_what_was_measured_before() {
        let pool = pool().await;
        record(&pool, &measured(0.56), chrono::Utc::now())
            .await
            .unwrap();

        record(
            &pool,
            &[Provider {
                provider: "claude".into(),
                fidelity: Fidelity::Unmeasured,
                read_at: "2026-09-19T05:01:00+00:00".into(),
                windows: Vec::new(),
                detail: "it expired 2m ago — sign in again in Claude Code".into(),
                severity: String::new(),
            }],
            chrono::Utc::now(),
        )
        .await
        .unwrap();

        let back = stored(&pool).await.unwrap();
        assert_eq!(back.len(), 1, "the last measured figure was forgotten");
        assert!((back[0].windows[0].used_fraction - 0.56).abs() < f64::EPSILON);
    }

    /// With no sidecar and an empty table, the answer says so rather than drawing nothing.
    #[tokio::test]
    async fn no_sidecar_answers_from_the_table_and_names_the_reason() {
        let pool = pool().await;
        let report = report(&QuotaRuntime::disabled(), &pool, chrono::Utc::now()).await;

        assert_eq!(report.source, Source::Stored);
        assert!(report.providers.is_empty());
        assert!(
            report.unreachable.is_some(),
            "an unanswered question must not look like an empty quota"
        );
    }

    // ---- The warning (design D11) ----

    /// Well before the reset instant the helpers above carry, so a reading is fresh unless a test
    /// says otherwise.
    fn noon() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-19T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    async fn feed_kinds(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT kind FROM feed ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn lines(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT summary FROM feed ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// The thresholds, at their edges.
    #[test]
    fn a_threshold_is_crossed_at_it_and_not_before_it() {
        assert_eq!(crossed_threshold(0.79, None), None);
        assert_eq!(crossed_threshold(0.80, None), Some(80));
        assert_eq!(crossed_threshold(0.99, None), Some(80));
        assert_eq!(crossed_threshold(1.0, None), Some(100));
        // Above the top of the ladder there is nothing further to say, and a figure the sidecar
        // should never send must not produce a threshold nobody configured.
        assert_eq!(crossed_threshold(1.4, None), Some(100));
    }

    /// The highest crossing is the one worth saying, and a threshold already said is not repeated.
    #[test]
    fn only_a_threshold_not_yet_announced_is_announced() {
        assert_eq!(crossed_threshold(0.85, Some(80)), None);
        assert_eq!(crossed_threshold(1.0, Some(80)), Some(100));
        assert_eq!(crossed_threshold(1.0, Some(100)), None);
        // A figure that falls back below a threshold already announced says nothing: the window did
        // not un-burn, the vendor revised its arithmetic.
        assert_eq!(crossed_threshold(0.85, Some(100)), None);
    }

    #[tokio::test]
    async fn a_measured_window_over_the_threshold_writes_one_feed_line() {
        let pool = pool().await;

        assert_eq!(warn(&pool, &measured(0.82), noon()).await.unwrap(), 1);

        assert_eq!(feed_kinds(&pool).await, vec![FEED_KIND]);
        let summary = &lines(&pool).await[0];
        assert!(
            summary.contains("quota") && summary.contains("claude") && summary.contains("5h"),
            "the line has to name the quota, the provider and the window: {summary}"
        );
    }

    /// The same threshold of the same window says its piece once, however often it is read.
    ///
    /// The notch polls, so this path runs every minute for as long as the window stays over the
    /// line. Without the claim, one burn past 80% would ping the owner's phone all afternoon.
    #[tokio::test]
    async fn the_same_threshold_of_the_same_window_warns_only_once() {
        let pool = pool().await;

        warn(&pool, &measured(0.82), noon()).await.unwrap();
        let again = warn(&pool, &measured(0.91), noon()).await.unwrap();

        assert_eq!(again, 0, "the same threshold spoke twice");
        assert_eq!(feed_kinds(&pool).await.len(), 1);
    }

    /// Crossing the next threshold is news, even though the window has already spoken once.
    #[tokio::test]
    async fn a_higher_threshold_is_worth_a_second_line() {
        let pool = pool().await;

        warn(&pool, &measured(0.82), noon()).await.unwrap();
        assert_eq!(warn(&pool, &measured(1.0), noon()).await.unwrap(), 1);
        assert_eq!(warn(&pool, &measured(1.0), noon()).await.unwrap(), 0);

        assert_eq!(feed_kinds(&pool).await.len(), 2);
    }

    /// Once the window rolls over, its thresholds are armed again.
    ///
    /// The window instance is identified by the reset instant the reading carries, not by a timer
    /// here: a fresh reading with a later reset IS the next window, and that is the only fact this
    /// module has that says so.
    #[tokio::test]
    async fn the_same_threshold_warns_again_once_the_window_has_reset() {
        let pool = pool().await;
        warn(&pool, &measured(0.82), noon()).await.unwrap();

        let mut next = measured(0.83);
        next[0].windows[0].resets_at = Some("2026-09-19T21:40:00+00:00".into());
        let spoken = warn(&pool, &next, noon()).await.unwrap();

        assert_eq!(
            spoken, 1,
            "a brand new window was still holding the old claim"
        );
        assert_eq!(feed_kinds(&pool).await.len(), 2);
        // And the new window's claim is the one now stored. Without this second call the test
        // passes even if the claim were written back with the OLD reset — every later poll would
        // then see a mismatch, re-arm, and ping once a minute.
        assert_eq!(warn(&pool, &next, noon()).await.unwrap(), 0);
        assert_eq!(feed_kinds(&pool).await.len(), 2);
    }

    /// A reset that moves BACKWARDS is the same window reported differently, not a new one.
    ///
    /// A `derived` figure recomputed from another rollout, or two readings taken across a clock
    /// adjustment, both produce this. Re-arming on any change rather than on a later one would ping
    /// twice for one burn — and a reset that merely jitters in its spelling would ping every poll.
    #[tokio::test]
    async fn a_reset_that_moves_backwards_does_not_re_arm_the_threshold() {
        let pool = pool().await;
        warn(&pool, &measured(0.82), noon()).await.unwrap();

        let mut earlier = measured(0.84);
        earlier[0].windows[0].resets_at = Some("2026-09-19T16:10:00+00:00".into());

        assert_eq!(warn(&pool, &earlier, noon()).await.unwrap(), 0);
        assert_eq!(feed_kinds(&pool).await.len(), 1);
    }

    /// A window with no reset instant must not be silenced for the life of the database.
    ///
    /// With the window instance identified by its reset alone, a reset-less window warns once and
    /// never again — and reset-less windows are real: the capture of 2026-09-19 carried one, and
    /// the Claude sidecar has a fixture of it. The claim expires with the window's own length
    /// instead, which is what its name carries.
    #[tokio::test]
    async fn a_reset_less_window_warns_again_once_its_own_length_has_passed() {
        let pool = pool().await;
        let mut providers = measured(0.82);
        providers[0].windows[0].resets_at = None;

        warn(&pool, &providers, noon()).await.unwrap();
        // Four hours later it is still the same five-hour window: still one line.
        let four_hours = noon() + chrono::Duration::hours(4);
        assert_eq!(warn(&pool, &providers, four_hours).await.unwrap(), 0);

        // Six hours later it cannot be the window that was claimed.
        let six_hours = noon() + chrono::Duration::hours(6);
        assert_eq!(warn(&pool, &providers, six_hours).await.unwrap(), 1);
        assert_eq!(feed_kinds(&pool).await.len(), 2);
    }

    #[test]
    fn a_window_name_is_read_as_a_count_and_a_unit_or_not_at_all() {
        assert_eq!(window_length("5h"), Some(chrono::Duration::hours(5)));
        assert_eq!(window_length("7d"), Some(chrono::Duration::days(7)));
        // A name this build cannot read must not be guessed at: every caller treats `None` as "do
        // not act on a length I had to invent".
        assert_eq!(window_length("month"), None);
        assert_eq!(window_length(""), None);
        assert_eq!(window_length("h"), None);
    }

    /// Two readings landing in the same instant produce one line, not two.
    ///
    /// Asserted at the claim rather than by racing two tasks, because the race is what has to be
    /// impossible: both callers read the same row, and the second one's compare-and-swap has to
    /// match nothing. `token_efficiency.rs` guards `last_alerted_at` the same way, for the same
    /// reason — a feed row IS a notification, so two winners are two pings.
    #[tokio::test]
    async fn two_readings_in_the_same_instant_leave_only_one_winner() {
        let pool = pool().await;
        let reset = Some("2026-09-19T16:40:00+00:00");

        let first = claim_the_right_to_warn(&pool, "claude", "5h", None, 80, reset, noon())
            .await
            .unwrap();
        // The same `None`: the second reader saw no row either, because it read before the first
        // one wrote.
        let second = claim_the_right_to_warn(&pool, "claude", "5h", None, 80, reset, noon())
            .await
            .unwrap();

        assert!(first, "the first claim must win");
        assert!(!second, "a claim guarded on a row that has moved must lose");
    }

    /// Fidelity first (design D3/G4): a reading nobody measured has no number to warn about.
    #[tokio::test]
    async fn an_unmeasured_reading_never_warns_however_high_the_number_beside_it() {
        let pool = pool().await;

        let providers = vec![Provider {
            provider: "claude".into(),
            fidelity: Fidelity::Unmeasured,
            read_at: "2026-09-19T12:00:00+00:00".into(),
            windows: vec![window(1.0, false)],
            detail: "it expired 2m ago — sign in again in Claude Code".into(),
            severity: String::new(),
        }];

        assert_eq!(warn(&pool, &providers, noon()).await.unwrap(), 0);
        assert!(feed_kinds(&pool).await.is_empty());
    }

    /// A reading the sidecar itself called stale describes a period that has ended.
    #[tokio::test]
    async fn a_reading_marked_stale_never_warns() {
        let pool = pool().await;
        let mut providers = measured(0.99);
        providers[0].windows[0].stale = true;

        assert_eq!(warn(&pool, &providers, noon()).await.unwrap(), 0);
        assert!(feed_kinds(&pool).await.is_empty());
    }

    /// **Outdated is decided here, from the reset instant, and not only from the stored flag.**
    ///
    /// The flag is fixed at the moment of the reading and is not recomputed when the reset passes
    /// (a known defect of phase 1, being fixed elsewhere). A warning that trusted the flag alone
    /// would shout about a window that has since rolled over — and it would do it from a stored
    /// reading, hours after the figure stopped being true. So this module asks the only question
    /// that cannot go stale: has the reset instant passed?
    #[tokio::test]
    async fn a_window_whose_reset_has_passed_never_warns_whatever_the_flag_says() {
        let pool = pool().await;
        // `stale: false`, and the reset instant is in the past: exactly the shape the phase 1
        // defect produces.
        let providers = measured(0.99);
        let after_the_reset = chrono::DateTime::parse_from_rfc3339("2026-09-19T17:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);

        assert_eq!(warn(&pool, &providers, after_the_reset).await.unwrap(), 0);
        assert!(
            feed_kinds(&pool).await.is_empty(),
            "a window past its reset warned because a flag said it was fresh"
        );
    }

    /// A window with no reset at all is measured and warnable — the 2026-09-19 capture carried
    /// one. Absence of a reset is not evidence that the reading is old.
    #[tokio::test]
    async fn a_window_without_a_reset_instant_still_warns() {
        let pool = pool().await;
        let mut providers = measured(0.82);
        providers[0].windows[0].resets_at = None;

        assert_eq!(warn(&pool, &providers, noon()).await.unwrap(), 1);
        // And it says so, rather than leaving a silence that reads as "resets imminently".
        let summary = lines(&pool).await.remove(0);
        assert!(
            summary.contains("no reset instant"),
            "a window with no reset must say so: {summary}"
        );
    }

    /// The line the owner reads has to say which brake is talking (design D9, obligation (a)):
    /// budget and quota are two different ceilings, and "why did this stop" is unanswerable if the
    /// two sound alike.
    #[tokio::test]
    async fn the_line_names_the_quota_and_the_figure_behind_it() {
        let pool = pool().await;
        warn(&pool, &measured(0.82), noon()).await.unwrap();

        let summary = lines(&pool).await.remove(0);
        assert!(
            summary.contains("82%"),
            "the measured figure is missing: {summary}"
        );
        assert!(
            summary.contains("80%"),
            "the threshold crossed is missing: {summary}"
        );
        assert!(
            summary.starts_with("quota: "),
            "design D9 (a): which of the two brakes is talking has to be the first thing read, and \
             `!contains(\"budget\")` cannot fail — this can"
        );
    }

    /// One provider being unreadable must not cost the other one its warning.
    ///
    /// `warn` walks every provider and every window, so a reading of two providers with two windows
    /// each has three quiet ones and a fourth that speaks. Without this, the whole loop is exercised
    /// only by single-window readings and an early `return` in place of a `continue` would pass.
    #[tokio::test]
    async fn an_unmeasured_provider_does_not_silence_a_measured_one() {
        let pool = pool().await;
        let providers = vec![
            Provider {
                provider: "claude".into(),
                fidelity: Fidelity::Unmeasured,
                read_at: "2026-09-19T12:00:00+00:00".into(),
                windows: vec![window(1.0, false)],
                detail: "it expired 2m ago — sign in again in Claude Code".into(),
                severity: String::new(),
            },
            Provider {
                provider: "codex".into(),
                fidelity: Fidelity::Derived,
                read_at: "2026-09-19T12:00:00+00:00".into(),
                windows: vec![
                    window(0.10, false),
                    Window {
                        window: "7d".into(),
                        used_fraction: 0.90,
                        resets_at: Some("2026-09-24T16:40:00+00:00".into()),
                        stale: false,
                        state: "warn",
                    },
                ],
                detail: String::new(),
                severity: String::new(),
            },
        ];

        assert_eq!(warn(&pool, &providers, noon()).await.unwrap(), 1);

        let summary = lines(&pool).await.remove(0);
        assert!(
            summary.contains("codex") && summary.contains("7d"),
            "the wrong window spoke: {summary}"
        );
    }

    /// The warning rides on the live path and nothing else. A stored reading is an old figure being
    /// redrawn, and warning from it would ping the owner about a window that may have rolled over
    /// while the sidecar was down.
    ///
    /// Asserted against [`live_report`] and [`stored_report`] rather than against [`report`],
    /// because `report` with a disabled runtime returns at its first guard: a test that went
    /// through it would leave the storing and the warning wired by lines it never executes, and
    /// deleting either call would keep the suite green.
    #[tokio::test]
    async fn a_stored_fallback_report_writes_no_warning() {
        let pool = pool().await;
        record(&pool, &measured(0.99), noon()).await.unwrap();

        let report = stored_report(&pool, "the quota sidecar is not running").await;

        assert_eq!(report.source, Source::Stored);
        assert!(
            !report.providers.is_empty(),
            "the stored figure was not drawn"
        );
        assert!(feed_kinds(&pool).await.is_empty());
    }

    /// The whole live path, end to end: the sidecar's answer is stored AND judged.
    ///
    /// Built from the JSON the sidecar actually sends, so the reading crosses the same
    /// deserialisation the daemon uses rather than a hand-built struct that cannot catch a field
    /// renamed on one side.
    #[tokio::test]
    async fn a_live_reading_is_both_stored_and_judged() {
        let pool = pool().await;
        let live: crate::quota_client::QuotaReport = serde_json::from_str(
            r#"{"providers":[{"provider":"claude","fidelity":"official",
                 "read_at":"2026-09-19T12:00:00Z","severity":"normal",
                 "windows":[{"window":"5h","used_fraction":0.82,
                             "resets_at":"2026-09-19T16:40:00Z","stale":false}]}],
                "cached":false}"#,
        )
        .unwrap();

        let report = live_report(&pool, live, noon()).await;

        assert_eq!(report.source, Source::Sidecar);
        assert_eq!(report.providers[0].windows[0].state, "warn");
        assert_eq!(
            feed_kinds(&pool).await,
            vec![FEED_KIND],
            "the live reading was not judged"
        );
        assert_eq!(
            stored(&pool).await.unwrap().len(),
            1,
            "the live reading was not stored"
        );
    }

    /// The fallback is only worth having if it actually carries the figures across.
    #[tokio::test]
    async fn with_the_sidecar_down_the_last_stored_figures_are_what_is_drawn() {
        let pool = pool().await;
        record(&pool, &measured(0.56), chrono::Utc::now())
            .await
            .unwrap();

        let report = report(&QuotaRuntime::disabled(), &pool, chrono::Utc::now()).await;

        assert_eq!(report.source, Source::Stored);
        assert_eq!(report.providers.len(), 1);
        assert!((report.providers[0].windows[0].used_fraction - 0.56).abs() < f64::EPSILON);
    }
}
