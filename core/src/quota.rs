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
//! **This module draws, speaks, reads, and now brakes** (design D9, D10): the optional `enabled`
//! setting (off by factory default) gates only the action, while every consultation still reads the
//! quota so the phase-3 warning fires even with the brake off. The brake fails OPEN, fixed: an
//! absent, stale, or unmeasured reading cannot prove a window spent—the opposite direction from
//! `budget.rs`, which fails closed.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

/// Source tag carried by a pause originating from the quota brake.
pub const PAUSE_SOURCE: &str = "quota";
/// Longest acceptable age for a measured quota reading.
pub const QUOTA_FRESH_FOR: Duration = Duration::minutes(10);

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
/// Warning thresholds stay fixed: the independently configurable brake settings live in
/// [`BrakePolicy`].
const WARN_AT: f64 = 0.75;
const EXHAUSTED_AT: f64 = 0.95;

/// Where the owner gets told, in per cent (`warn_at_percent`, design D9's default).
///
/// The warning uses a fixed threshold; braking has separate owner-configurable thresholds.
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
/// Feed kind emitted once when an enabled brake must run blind.
pub const BLIND_FEED_KIND: &str = "quota_blind";

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
    /// because the brake's owner settings live in the database and a copy in TypeScript would be a
    /// second policy that changes on a different schedule.
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

/// The quota pillar's runtime: its sidecar client and the active runner's provider.
///
/// The provider list remains whatever the sidecar answers; a runtime copy would be a second list
/// that only changes when the daemon restarts.
#[derive(Debug, Clone)]
pub struct QuotaRuntime {
    pub client: Option<crate::quota_client::QuotaClient>,
    pub provider: String,
}

impl QuotaRuntime {
    pub fn new(client: crate::quota_client::QuotaClient, provider: String) -> Self {
        Self {
            client: Some(client),
            provider,
        }
    }

    /// No sidecar: `GET /quota` answers from the table, or says it has nothing.
    ///
    /// Named rather than derived, and `#[cfg(test)]`, for the two reasons
    /// `web::WebRuntime::disabled` gives: a derived default would invent a client pointing at
    /// nothing, so "off" should be a thing somebody chose — and production always builds a real
    /// one, so left ungated this is dead code in the daemon.
    #[cfg(any(test, feature = "testkit"))]
    pub fn disabled() -> Self {
        Self {
            client: None,
            provider: "claude".into(),
        }
    }
}

/// Owner-configurable quota-brake settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrakePolicy {
    pub enabled: bool,
    pub pause_above_percent_5h: i64,
    pub pause_above_percent_7d: i64,
}

/// Loads the singleton quota-brake policy.
pub async fn load_brake_policy(pool: &SqlitePool) -> sqlx::Result<BrakePolicy> {
    let row: (i64, i64, i64) = sqlx::query_as("SELECT quota_brake_enabled, quota_pause_above_percent_5h, quota_pause_above_percent_7d FROM autopilot_global LIMIT 1").fetch_one(pool).await?;
    Ok(BrakePolicy {
        enabled: row.0 != 0,
        pause_above_percent_5h: row.1,
        pause_above_percent_7d: row.2,
    })
}

/// Replaces the singleton quota-brake policy.
pub async fn set_brake_policy(pool: &SqlitePool, policy: BrakePolicy) -> sqlx::Result<()> {
    sqlx::query("UPDATE autopilot_global SET quota_brake_enabled = ?, quota_pause_above_percent_5h = ?, quota_pause_above_percent_7d = ?")
        .bind(i64::from(policy.enabled)).bind(policy.pause_above_percent_5h).bind(policy.pause_above_percent_7d).execute(pool).await?;
    Ok(())
}

/// Pure outcome of judging one provider's quota windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Clear,
    Blind(String),
    Over {
        reason: String,
        resets_at: Option<DateTime<Utc>>,
    },
}
/// Result returned to a prospective autonomous run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaDecision {
    Allow,
    Pause {
        reason: String,
        resets_at: Option<DateTime<Utc>>,
    },
}

/// Judges the active provider's fresh, measured quota windows against the policy.
pub fn judge(
    policy: &BrakePolicy,
    provider: &str,
    providers: &[Provider],
    now: DateTime<Utc>,
) -> Verdict {
    let Some(reading) = providers.iter().find(|item| item.provider == provider) else {
        return Verdict::Blind(format!("quota: no reading for active provider {provider}"));
    };
    let read_at = DateTime::parse_from_rfc3339(&reading.read_at)
        .ok()
        .map(|at| at.with_timezone(&Utc));
    let mut blind = reading.fidelity == Fidelity::Unmeasured
        || read_at.is_none_or(|at| now.signed_duration_since(at) > QUOTA_FRESH_FOR);
    if blind {
        return Verdict::Blind(format!(
            "quota: active provider {provider} has no fresh measured reading"
        ));
    }
    let mut overs = Vec::new();
    for window in &reading.windows {
        let threshold = match window.window.as_str() {
            "5h" => policy.pause_above_percent_5h,
            "7d" => policy.pause_above_percent_7d,
            _ => continue,
        };
        // `Window::stale` is computed from the reading clock on both the live and stored paths.
        // Trust that carried fact here so this pure decision does not reinterpret a fixture's
        // already-judged reading against a different caller clock.
        if window.stale {
            blind = true;
            continue;
        }
        if window.used_fraction * 100.0 >= threshold as f64 {
            let reset = window
                .resets_at
                .as_deref()
                .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                .map(|at| at.with_timezone(&Utc));
            overs.push((
                reset,
                format!(
                    "quota: {provider}'s {} window is at or above {threshold}%",
                    window.window
                ),
            ));
        }
    }
    if !overs.is_empty() {
        let reason = overs[0].1.clone();
        let resets_at = if overs.iter().any(|(reset, _)| reset.is_none()) {
            None
        } else {
            overs.into_iter().filter_map(|(reset, _)| reset).max()
        };
        return Verdict::Over { reason, resets_at };
    }
    if blind {
        Verdict::Blind(format!(
            "quota: active provider {provider} has no fresh measured reading"
        ))
    } else {
        Verdict::Clear
    }
}

async fn clear_blind_marker(pool: &SqlitePool) -> sqlx::Result<()> {
    sqlx::query("UPDATE autopilot_global SET quota_blind_announced_at = NULL WHERE quota_blind_announced_at IS NOT NULL").execute(pool).await?;
    Ok(())
}
async fn announce_blind_once(
    pool: &SqlitePool,
    summary: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    let changed = sqlx::query("UPDATE autopilot_global SET quota_blind_announced_at = ? WHERE quota_blind_announced_at IS NULL").bind(now.to_rfc3339()).execute(&mut *tx).await?.rows_affected();
    if changed == 1 {
        crate::feed::append_on(&mut tx, None, BLIND_FEED_KIND, summary, None, None).await?;
    }
    tx.commit().await
}

/// Reads the quota and, when enabled, decides whether it permits a new autonomous run.
pub async fn quota_permits_new_run(
    pool: &SqlitePool,
    runtime: Option<&QuotaRuntime>,
    provider: &str,
    now: DateTime<Utc>,
) -> QuotaDecision {
    let providers = match runtime {
        Some(runtime) => report(runtime, pool, now).await.providers,
        None => match stored(pool, now).await {
            Ok(readings) => readings,
            Err(error) => {
                tracing::warn!(%error, "quota brake could not read stored quota");
                return QuotaDecision::Allow;
            }
        },
    };
    let policy = match load_brake_policy(pool).await {
        Ok(policy) => policy,
        Err(error) => {
            tracing::warn!(%error, "quota brake policy could not be read");
            return QuotaDecision::Allow;
        }
    };
    if !policy.enabled {
        return QuotaDecision::Allow;
    }
    match judge(&policy, provider, &providers, now) {
        Verdict::Clear => {
            if let Err(error) = clear_blind_marker(pool).await {
                tracing::warn!(%error, "quota blind marker could not be cleared");
            }
            QuotaDecision::Allow
        }
        Verdict::Over { reason, resets_at } => {
            if let Err(error) = clear_blind_marker(pool).await {
                tracing::warn!(%error, "quota blind marker could not be cleared");
            }
            QuotaDecision::Pause { reason, resets_at }
        }
        Verdict::Blind(summary) => {
            if let Err(error) = announce_blind_once(pool, &summary, now).await {
                tracing::warn!(%error, "quota blind announcement could not be written");
            }
            QuotaDecision::Allow
        }
    }
}

/// Applies the budget brake first, then the fail-open quota brake.
pub async fn permits_new_run(
    state: &crate::state::AppState,
    now: DateTime<Utc>,
) -> crate::budget::BudgetDecision {
    let budget = crate::budget::budget_permits_new_run(&state.pool, now).await;
    if !matches!(budget, crate::budget::BudgetDecision::Allow) {
        return budget;
    }
    match quota_permits_new_run(&state.pool, Some(&state.quota), &state.quota.provider, now).await {
        QuotaDecision::Allow => crate::budget::BudgetDecision::Allow,
        QuotaDecision::Pause { reason, resets_at } => crate::budget::BudgetDecision::Pause {
            reason,
            kind: if resets_at.is_some() {
                crate::budget::PauseKind::Transient
            } else {
                crate::budget::PauseKind::Window
            },
            source: PAUSE_SOURCE,
        },
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
        return stored_report(pool, "the quota sidecar is not running", now).await;
    };

    match client.report().await {
        Ok(live) => live_report(pool, live, now).await,
        Err(error) => {
            // The sidecar's own message, which by construction carries no token — see
            // `sidecars/quota/claude`, where that property has a test of its own.
            stored_report(pool, &error.to_string(), now).await
        }
    }
}

/// Everything that happens to a reading the sidecar has just answered: store it, judge it, return
/// it.
///
/// Split out of [`report`] so that it can be tested without an HTTP server. That is not tidiness:
/// every test of `report` uses [`QuotaRuntime::disabled`], which returns at the guard above before
/// any of these calls, so with the body inline the storing and the warning are wired by lines no
/// test executes — and deleting either call left the suite green.
async fn live_report(
    pool: &SqlitePool,
    live: crate::quota_client::QuotaReport,
    now: chrono::DateTime<chrono::Utc>,
) -> QuotaReport {
    let cached = live.cached;
    let providers: Vec<Provider> = live
        .providers
        .into_iter()
        .map(|raw| from_reading(raw, now))
        .collect();
    // Recorded before answering, and a write that fails does NOT fail the answer: the table is a
    // fallback, so losing it costs the next reader a fresh figure and must not cost this one the
    // figure already in hand.
    if let Err(error) = record(pool, &providers, now).await {
        tracing::warn!(%error, "the quota reading could not be stored");
    }
    let providers = degrade_to_last_good(pool, providers, now).await;
    // Judged here, on the live path only, because this is the one place a NEW reading arrives —
    // every caller of this module comes through it, so a warning cannot be skipped by a future
    // second caller forgetting to ask for one.
    //
    // AFTER `degrade_to_last_good`, and the order is load-bearing: the owner is warned about the
    // figures the notch is about to draw, never about a set nobody was shown. A degraded provider
    // carries its last good windows at full fidelity with `stale: true`, and [`is_outdated`] is
    // what stops that from interrupting anybody — judging before the degradation would have read
    // the same provider as merely `unmeasured` and reached the same silence by luck rather than by
    // the guard D3 asks for.
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

/// The start of every `detail` the Claude sidecar writes when the owner's CREDENTIAL is the
/// problem, as opposed to the vendor or the network.
///
/// Prose is the only signal the sidecar sends for this: `reading.Unavailable` carries a name, a
/// detail and a time, and no machine-readable reason. So these are matched as prefixes of the
/// sidecar's own sentences, and `the_credential_markers_are_still_what_the_sidecar_writes` reads
/// `sidecars/quota/claude/claude.go` to fail the day either sentence is reworded. A marker that
/// silently stopped matching would quietly turn "sign in again" into a stale ring.
///
/// - `no usable Claude credential` is `ErrNoToken`, which prefixes every missing, unreadable or
///   expired token.
/// - `the usage endpoint refused the credential` is a 401/403: a token the vendor revoked.
const CREDENTIAL_MARKERS: [&str; 2] = [
    "no usable Claude credential",
    "the usage endpoint refused the credential",
];

/// Whether an unmeasured reading says the owner has to sign in again (design §5, first row).
fn is_credential_failure(detail: &str) -> bool {
    CREDENTIAL_MARKERS
        .iter()
        .any(|marker| detail.starts_with(marker))
}

/// Serve the last good figures, marked stale, for a provider the sidecar could not read this time.
///
/// Design §5: a 429 or a 5xx from the vendor degrades to the last good reading marked `stale`. The
/// sidecar reports such a failure INSIDE a successful answer, as an `unmeasured` provider with a
/// reason, so the sidecar being reachable is not the same as the figures being fresh. Without this,
/// one rate-limited call would dash the Claude rings for a whole poll, and the next good call would
/// fill them again.
///
/// Two cases keep the provider `unmeasured` all the same. A provider with nothing stored has no
/// last good figure to degrade to. And a credential failure is not a bad moment at the vendor: it
/// is D3's "present, with no quota source", which the same §5 table draws as a dashed ring so the
/// owner sees that signing in again is theirs to do. A stale figure there would look like the
/// vendor having a slow day, for as long as nobody noticed.
///
/// The sidecar's `detail` travels with the stored windows, so the reason the figure is old stays
/// on screen beside it.
///
/// **Note for the brake.** A degraded provider keeps the fidelity it was stored with —
/// usually [`Fidelity::Official`] — and the ONLY mark that it is not a current reading is
/// `stale: true` on each window. So a brake that gates on a minimum fidelity alone would act on a
/// figure that is hours old at full fidelity, which is precisely what D3's "only trusts what is
/// measured" forbids. Gate on `stale` as well as on fidelity. Raising the degraded provider's
/// fidelity instead was rejected: the number really was measured officially, and lying about its
/// provenance would make the ring's own tooltip wrong to keep the brake simple.
async fn degrade_to_last_good(
    pool: &SqlitePool,
    providers: Vec<Provider>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Provider> {
    let degradable =
        |p: &Provider| p.fidelity == Fidelity::Unmeasured && !is_credential_failure(&p.detail);
    if !providers.iter().any(degradable) {
        return providers;
    }
    let last_good = match stored(pool, now).await {
        Ok(last_good) => last_good,
        Err(error) => {
            tracing::warn!(%error, "the stored quota readings could not be read");
            return providers;
        }
    };
    providers
        .into_iter()
        .map(|live| {
            if !degradable(&live) {
                return live;
            }
            match last_good.iter().find(|p| p.provider == live.provider) {
                Some(good) => Provider {
                    windows: good
                        .windows
                        .iter()
                        .cloned()
                        .map(|w| Window {
                            state: state_of(good.fidelity, w.used_fraction, true),
                            stale: true,
                            ..w
                        })
                        .collect(),
                    detail: live.detail,
                    severity: live.severity,
                    ..good.clone()
                },
                None => live,
            }
        })
        .collect()
}

async fn stored_report(
    pool: &SqlitePool,
    why: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> QuotaReport {
    let providers = match stored(pool, now).await {
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

/// Whether a window has rolled over, whatever the reading said when it was taken.
///
/// `stale` is a function of the clock, not a property frozen into a reading. A window recorded at
/// 0.97 with a 16:40 reset, whose sidecar then died at 16:00, still says 0.97 at 18:00, and
/// serving the flag it was stored with would keep calling a period that has ended `exhausted`.
/// The brake reads this table, so that is not a cosmetic lie.
///
/// The boundary is `<=`: a window whose reset is exactly `now` has reopened and is therefore stale
/// here, the stricter of the two readings — chosen because this side re-derives the flag and a
/// wrong answer costs a shout about a limit that no longer binds. The Go sidecar's
/// `reading.MarkStale` uses `<` and may disagree for the one instant they straddle; that is
/// harmless and not worth a shared constant, because `from_reading` and `stored` both run this
/// function over whatever the sidecar sent, so the núcleo's answer is always the one the shell sees.
fn is_stale(
    flagged: bool,
    resets_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    flagged || resets_at.is_some_and(|at| at <= now)
}

/// Turn one sidecar reading into the shape the shell is sent.
///
/// Staleness is re-derived here too, though the sidecar already marks it: the sidecar answers from
/// a cache of up to a minute, and the process that decides the state should judge it against the
/// clock its caller passed in, not the one the reading was taken under.
fn from_reading(
    raw: crate::quota_client::ProviderReading,
    now: chrono::DateTime<chrono::Utc>,
) -> Provider {
    let fidelity = Fidelity::parse(&raw.fidelity);
    Provider {
        provider: raw.provider,
        fidelity,
        read_at: raw.read_at.to_rfc3339(),
        windows: raw
            .windows
            .into_iter()
            .map(|w| {
                let stale = is_stale(w.stale, w.resets_at, now);
                Window {
                    state: state_of(fidelity, w.used_fraction, stale),
                    window: w.window,
                    used_fraction: w.used_fraction,
                    resets_at: w.resets_at.map(|at| at.to_rfc3339()),
                    stale,
                }
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

/// The last stored reading of every provider, with staleness judged against `now`.
pub async fn stored(
    pool: &SqlitePool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<Provider>, sqlx::Error> {
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
        // A reset that does not parse cannot prove the window has rolled over, so it leaves the
        // stored flag to decide. `record` writes RFC3339 and nothing else writes this column.
        let resets_at = row
            .resets_at
            .as_deref()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
            .map(|at| at.with_timezone(&chrono::Utc));
        let stale = is_stale(row.stale != 0, resets_at, now);
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
/// [`is_stale`] against the caller's clock, on a `Window` rather than on the parts — the same
/// question the ring is coloured by, asked in the same words. It is delegated and not re-spelled
/// deliberately: this was written while `stale` was still frozen at record time, so it re-derived
/// the answer itself rather than trust the field, and the phase-1 fix made that the rule for
/// everyone. Two spellings of one rule is how the ring and the warning would come to disagree about
/// the same window, each of them right about its own version.
///
/// Why the warning asks at all, when `state_of` has already read the same thing: a warning is an
/// interruption. A figure past its own reset describes a period that has reopened, and saying "your
/// 5h window is spent" about a window that is not is the exact failure the fidelity ladder exists to
/// prevent — arriving through the one field that looked trustworthy.
///
/// No reset instant is not evidence of age: the capture of 2026-09-19 carried a populated window
/// with `resets_at: null`, and such a window falls back on the flag.
fn is_outdated(window: &Window, now: chrono::DateTime<chrono::Utc>) -> bool {
    is_stale(
        window.stale,
        window
            .resets_at
            .as_deref()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
            .map(|at| at.with_timezone(&chrono::Utc)),
        now,
    )
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
/// `5h` and `7d` are the two the sidecar sends today (`sidecars/quota/reading/reading.go`), and the
/// shape — a count and a unit — is read generically so that a third window a later sidecar invents
/// is understood rather than silently mishandled. A name this build cannot read answers `None`, and
/// every caller treats that as "do not act on a length I had to guess".
///
/// Hours and days, and deliberately not minutes. `m` is the one unit where guessing is wrong in the
/// expensive direction: a provider that writes `1m` for a MONTHLY window would have its claim
/// expire after sixty seconds and ping once a poll for thirty days — the exact failure
/// `quota_warnings` exists to prevent, arriving through the rule written to prevent another one.
/// An unreadable name costs silence; a misread one costs a ping a minute.
///
/// **Nothing here may panic on a name it dislikes.** The string is whatever the sidecar put in its
/// JSON and it reaches this function inside the `GET /quota` handler, so `split_at` on a byte index
/// that is not a char boundary (`"1月"`) or `Duration::hours` on a count it cannot hold
/// (`"999999999999d"`) would kill the request and blank the notch with no line saying why. Hence the
/// trailing CHARACTER rather than the trailing byte, and the `try_` constructors, which are the
/// `Option` this function already promised to return.
fn window_length(window_name: &str) -> Option<chrono::Duration> {
    let unit = window_name.chars().next_back()?;
    let count: i64 = window_name[..window_name.len() - unit.len_utf8()]
        .parse()
        .ok()?;
    match unit {
        'h' => chrono::Duration::try_hours(count),
        'd' => chrono::Duration::try_days(count),
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
/// again.
///
/// **The second rule is the FALLBACK for the first, not a second opinion on top of it.** Two
/// readable reset instants settle the question by themselves — the window either rolled or it did
/// not — and letting age speak as well can only ever add a wrong `true`: a window whose real
/// duration outlives what its name says (`5h` whose reset is a year out, which is what
/// `codex_test.go` guards against by pinning `resets_at` to epoch SECONDS) would be re-armed by the
/// clock while its own instant says it never rolled. That is a duplicate ping inside a live window,
/// which is the one thing this table exists to prevent. So age is consulted only when the instants
/// cannot be compared, which is exactly the hole it was added to fill.
///
/// Either instant may be the missing one, and both directions land here on purpose: a claim written
/// with a reset against a reading that arrives without one (the sidecar degraded), and a claim
/// written without one against a reading that now has one. Neither can be compared, so both are
/// decided by age — and the cost, up to one window's silence before the new instance gets its word,
/// is paid in the direction this module errs in.
fn claim_has_expired(claim: &Claim, window: &Window, now: chrono::DateTime<chrono::Utc>) -> bool {
    let instant = |at: Option<&str>| {
        at.and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
            .map(|at| at.with_timezone(&chrono::Utc))
    };
    match (
        instant(claim.resets_at.as_deref()),
        instant(window.resets_at.as_deref()),
    ) {
        (Some(claimed), Some(fresh)) => fresh > claimed,
        _ => match (
            window_length(&window.window),
            instant(Some(&claim.last_alerted_at)),
        ) {
            (Some(length), Some(said_at)) => now.signed_duration_since(said_at) >= length,
            // A window whose name this build cannot read, or a stamp it cannot parse. Holding the
            // claim errs towards silence, which is the direction a warning should err in.
            _ => false,
        },
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
///
/// Takes a connection rather than the pool so the caller can put this statement and the feed line
/// it authorises inside one transaction; see [`warn`], which is why that matters.
async fn claim_the_right_to_warn(
    conn: &mut sqlx::SqliteConnection,
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
    .execute(conn)
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
/// the brake reads the same two guards for a heavier reason.
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
            // The claim and the line it authorises, or neither. `feed::append_on` exists for
            // exactly this ("a writer that must be atomic with its feed entry"), and without it the
            // two are a trade: claim first and a failed write loses the crossing for good, because
            // the claim is already committed and the next poll reads it as said; speak first and a
            // crash between the two says it twice. `token_efficiency.rs` has to live with that
            // trade — it speaks through `notify::deliver_or_defer`, which cannot be folded into a
            // transaction — and this module copied its shape before noticing it does not have to.
            let mut claiming = pool.begin().await?;
            if !claim_the_right_to_warn(
                &mut claiming,
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
            // beside its own claim: a `?` here would abandon every remaining provider and window in
            // this round — in practice the second provider and the `7d` window — over one failed
            // write. What is NOT the same is what the failure costs. The transaction is dropped
            // unfinished, so the claim goes down with the line and the next poll finds the window
            // still unannounced and tries again. Nothing is lost and nothing is said twice.
            if let Err(error) = crate::feed::append_on(
                &mut claiming,
                None,
                FEED_KIND,
                &summarise(&provider.provider, window, threshold),
                None,
                None,
            )
            .await
            {
                tracing::warn!(%error, provider = %provider.provider, window = %window.window, "a quota warning was claimed but not written; the claim was rolled back with it");
                continue;
            }
            if let Err(error) = claiming.commit().await {
                tracing::warn!(%error, provider = %provider.provider, window = %window.window, "a quota warning could not be committed");
                continue;
            }
            spoken += 1;
        }
    }
    Ok(spoken)
}

#[cfg(any(test, feature = "testkit"))]
pub mod test_support {
    use super::*;

    pub async fn stub_sidecar(answer: serde_json::Value) -> String {
        let app = axum::Router::new().route(
            "/quota",
            axum::routing::get(move || {
                let answer = answer.clone();
                async move { axum::Json(answer) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        address.to_string()
    }

    pub async fn arm(pool: &SqlitePool, enabled: bool, p5h: i64, p7d: i64) {
        set_brake_policy(
            pool,
            BrakePolicy {
                enabled,
                pause_above_percent_5h: p5h,
                pause_above_percent_7d: p7d,
            },
        )
        .await
        .unwrap();
    }

    pub fn live_answer(
        provider: &str,
        window: &str,
        used: f64,
        resets_at: Option<&str>,
        read_at: chrono::DateTime<chrono::Utc>,
    ) -> serde_json::Value {
        serde_json::json!({
            "providers": [{
                "provider": provider,
                "fidelity": "official",
                "read_at": read_at.to_rfc3339(),
                "severity": "normal",
                "windows": [{
                    "window": window,
                    "used_fraction": used,
                    "resets_at": resets_at,
                    "stale": false
                }]
            }],
            "cached": false
        })
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{arm, live_answer, stub_sidecar};
    use super::*;

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn state_with_quota(pool: SqlitePool, quota: QuotaRuntime) -> crate::state::AppState {
        crate::state::AppState {
            token: crate::auth::Token("test-token".into()),
            pool,
            telegram_doctrine: None,
            runner: std::sync::Arc::new(crate::runner::FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            run_handles: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_messages: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_tails: Default::default(),
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: std::sync::Arc::new(crate::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            quota: std::sync::Arc::new(quota),
            judge: std::sync::Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
        }
    }

    /// The reset every helper below writes, and the clock they are read under.
    ///
    /// Far apart on purpose: staleness is now decided against the reader's clock, so a fixture
    /// whose reset sits near the real `Utc::now()` would start calling itself stale on the day the
    /// machine's date caught up with it, and the bands would be asserted against the wrong state.
    const RESET: &str = "2026-09-19T16:40:00+00:00";
    const BEFORE_RESET: &str = "2026-09-19T15:00:00+00:00";
    const AFTER_RESET: &str = "2026-09-19T18:00:00+00:00";

    fn at(moment: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(moment)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn window(used: f64, stale: bool) -> Window {
        Window {
            window: "5h".into(),
            used_fraction: used,
            resets_at: Some(RESET.into()),
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
        record(&pool, &measured(0.56), at(BEFORE_RESET))
            .await
            .unwrap();

        let back = stored(&pool, at(BEFORE_RESET)).await.unwrap();

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
        record(&pool, &measured(0.10), at(BEFORE_RESET))
            .await
            .unwrap();
        record(&pool, &measured(0.80), at(BEFORE_RESET))
            .await
            .unwrap();

        let back = stored(&pool, at(BEFORE_RESET)).await.unwrap();
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
        record(&pool, &measured(0.56), at(BEFORE_RESET))
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
            at(BEFORE_RESET),
        )
        .await
        .unwrap();

        let back = stored(&pool, at(BEFORE_RESET)).await.unwrap();
        assert_eq!(back.len(), 1, "the last measured figure was forgotten");
        assert!((back[0].windows[0].used_fraction - 0.56).abs() < f64::EPSILON);
    }

    /// **The reading is stale when it is read, not when it was taken.**
    ///
    /// The window that motivated this: recorded at 0.97 with a 16:40 reset, the sidecar dies at
    /// 16:00, and at 18:00 the table still answered `exhausted` — shouting about a limit that had
    /// reopened two hours earlier. Phase 4's brake reads exactly this table.
    #[tokio::test]
    async fn a_reset_that_passes_between_recording_and_reading_makes_the_window_stale() {
        let pool = pool().await;
        record(&pool, &measured(0.97), at(BEFORE_RESET))
            .await
            .unwrap();

        let before = stored(&pool, at(BEFORE_RESET)).await.unwrap();
        assert_eq!(before[0].windows[0].state, "exhausted");

        let after = stored(&pool, at(AFTER_RESET)).await.unwrap();
        assert!(
            after[0].windows[0].stale,
            "a window past its reset was served fresh"
        );
        assert_eq!(
            after[0].windows[0].state, "stale",
            "a period that has ended was still called exhausted"
        );
    }

    /// The same clock decides a live reading, because the sidecar answers from a cache of its own.
    #[test]
    fn a_live_window_past_its_reset_is_stale_however_the_sidecar_flagged_it() {
        let reading = crate::quota_client::ProviderReading {
            provider: "claude".into(),
            fidelity: "official".into(),
            read_at: at(BEFORE_RESET),
            windows: vec![crate::quota_client::WindowReading {
                window: "5h".into(),
                used_fraction: 0.97,
                resets_at: Some(at(RESET)),
                stale: false,
            }],
            detail: String::new(),
            severity: String::new(),
        };

        let fresh = from_reading(reading.clone(), at(BEFORE_RESET));
        assert_eq!(fresh.windows[0].state, "exhausted");

        let rolled = from_reading(reading, at(AFTER_RESET));
        assert!(rolled.windows[0].stale);
        assert_eq!(rolled.windows[0].state, "stale");
    }

    fn unmeasured(detail: &str) -> Vec<Provider> {
        vec![Provider {
            provider: "claude".into(),
            fidelity: Fidelity::Unmeasured,
            read_at: BEFORE_RESET.into(),
            windows: Vec::new(),
            detail: detail.into(),
            severity: String::new(),
        }]
    }

    /// **A bad moment at the vendor degrades; it does not dash the ring** (design §5).
    ///
    /// A 429 or a 5xx reaches the núcleo as a perfectly reachable sidecar reporting an `unmeasured`
    /// provider. Serving that as-is emptied the Claude rings for one poll and filled them again on
    /// the next, which is a flicker that says nothing true.
    #[tokio::test]
    async fn a_vendor_failure_serves_the_last_good_figures_marked_stale() {
        let pool = pool().await;
        record(&pool, &measured(0.56), at(BEFORE_RESET))
            .await
            .unwrap();

        let degraded = degrade_to_last_good(
            &pool,
            unmeasured("the usage endpoint answered 429"),
            at(BEFORE_RESET),
        )
        .await;

        assert_eq!(degraded.len(), 1);
        assert_eq!(
            degraded[0].fidelity,
            Fidelity::Official,
            "the stored reading was official"
        );
        assert!((degraded[0].windows[0].used_fraction - 0.56).abs() < f64::EPSILON);
        assert_eq!(degraded[0].windows[0].state, "stale");
        assert_eq!(
            degraded[0].detail, "the usage endpoint answered 429",
            "the reason the figure is old has to stay beside it"
        );
    }

    /// **A credential failure stays unmeasured** (design D3 and §5's first row).
    ///
    /// "Sign in again in Claude Code" is something only the owner can do, and a stale-looking
    /// figure would read as the vendor having a slow day for as long as nobody noticed.
    #[tokio::test]
    async fn an_expired_token_is_not_degraded_to_a_stale_figure() {
        let pool = pool().await;
        record(&pool, &measured(0.56), at(BEFORE_RESET))
            .await
            .unwrap();

        let kept = degrade_to_last_good(
            &pool,
            unmeasured(
                "no usable Claude credential: it expired 2m ago — sign in again in Claude Code",
            ),
            at(BEFORE_RESET),
        )
        .await;

        assert_eq!(kept[0].fidelity, Fidelity::Unmeasured);
        assert!(
            kept[0].windows.is_empty(),
            "an unmeasured provider must carry no number"
        );
    }

    /// Nothing stored is nothing to degrade to, and inventing a zero is the one thing this feature
    /// must never do.
    #[tokio::test]
    async fn a_provider_with_nothing_stored_stays_unmeasured() {
        let pool = pool().await;

        let kept = degrade_to_last_good(
            &pool,
            unmeasured("the usage endpoint answered 503"),
            at(BEFORE_RESET),
        )
        .await;

        assert_eq!(kept[0].fidelity, Fidelity::Unmeasured);
        assert!(kept[0].windows.is_empty());
    }

    /// The credential signal is the sidecar's own prose, so this reads the sidecar.
    ///
    /// `reading.Unavailable` carries no machine-readable reason — only a sentence — and a marker
    /// that quietly stopped matching would turn "sign in again" into a stale ring nobody acts on.
    /// This fails on the commit that rewords the sentence, in the repository where both live.
    #[test]
    fn the_credential_markers_are_still_what_the_sidecar_writes() {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../sidecars/quota/claude/claude.go");
        let go = std::fs::read_to_string(&source).expect("the quota sidecar's claude package");
        for marker in CREDENTIAL_MARKERS {
            assert!(
                go.contains(marker),
                "{marker:?} is no longer what {} writes, so a credential failure now degrades to a \
                 stale figure instead of asking the owner to sign in",
                source.display()
            );
        }
    }

    /// With no sidecar and an empty table, the answer says so rather than drawing nothing.
    #[tokio::test]
    async fn no_sidecar_answers_from_the_table_and_names_the_reason() {
        let pool = pool().await;
        let report = report(&QuotaRuntime::disabled(), &pool, at(BEFORE_RESET)).await;

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

    /// A window whose own reset says it has not rolled is never re-armed by the clock.
    ///
    /// The reading is a `5h` window whose reset is a month out — what a provider writing epoch
    /// MILLIseconds would produce, which is the mistake `sidecars/quota/codex/codex_test.go` exists
    /// to pin down. Six hours later the name says the window is over and the instant says it is not,
    /// and the instant wins: re-arming here would be a second ping inside one live window, which is
    /// the failure `quota_warnings` was built to prevent.
    #[tokio::test]
    async fn a_window_whose_reset_has_not_moved_is_not_re_armed_by_its_name() {
        let pool = pool().await;
        let mut providers = measured(0.82);
        providers[0].windows[0].resets_at = Some("2026-10-19T12:00:00+00:00".into());

        assert_eq!(warn(&pool, &providers, noon()).await.unwrap(), 1);
        let six_hours = noon() + chrono::Duration::hours(6);
        assert_eq!(
            warn(&pool, &providers, six_hours).await.unwrap(),
            0,
            "a name is not evidence against the instant the reading carried"
        );
        assert_eq!(feed_kinds(&pool).await.len(), 1);
    }

    /// A reset that disappears between readings costs silence, never a duplicate.
    ///
    /// The sidecar degrades: it answered with a reset instant, then answers without one (a real
    /// answer — the capture of 2026-09-19 had one). The two instants can no longer be compared, so
    /// the claim is decided by age, and the new window goes unannounced until the old claim outlives
    /// its own length. That wait is the price of not guessing, and it is paid in the direction this
    /// module errs in.
    #[tokio::test]
    async fn a_reset_that_disappears_buys_silence_and_not_a_second_ping() {
        let pool = pool().await;
        let mut providers = measured(0.82);
        providers[0].windows[0].resets_at = Some("2026-09-19T13:00:00+00:00".into());
        assert_eq!(warn(&pool, &providers, noon()).await.unwrap(), 1);

        // The window rolled at 13:00, and the reading that follows carries no instant at all.
        providers[0].windows[0].resets_at = None;
        let two_hours = noon() + chrono::Duration::hours(2);
        assert_eq!(
            warn(&pool, &providers, two_hours).await.unwrap(),
            0,
            "nothing here can tell the new window from the old one yet"
        );

        let five_hours = noon() + chrono::Duration::hours(5);
        assert_eq!(warn(&pool, &providers, five_hours).await.unwrap(), 1);
        assert_eq!(feed_kinds(&pool).await.len(), 2);
    }

    /// A line that cannot be written takes its claim down with it.
    ///
    /// The feed is made unwritable, which is the one failure the claim cannot survive on its own:
    /// claimed but unsaid, the window reads as announced for ever and the crossing is lost in
    /// silence. Inside one transaction there is nothing to lose — the next poll finds the window
    /// exactly as it left it.
    #[tokio::test]
    async fn a_warning_that_cannot_be_written_leaves_no_claim_behind() {
        let pool = pool().await;
        sqlx::query("DROP TABLE feed").execute(&pool).await.unwrap();

        assert_eq!(warn(&pool, &measured(0.82), noon()).await.unwrap(), 0);

        let claims: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM quota_warnings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            claims, 0,
            "a claim is worth nothing without the line it bought"
        );
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
        // `m` is refused rather than guessed: a provider that means one MONTH by `1m` would have
        // its claim expire every sixty seconds, which is a ping a poll for thirty days.
        assert_eq!(window_length("1m"), None);
        // A name is whatever the sidecar put in its JSON, and it is read inside an HTTP handler, so
        // neither a multi-byte tail nor a count no duration can hold may abort the request.
        assert_eq!(window_length("1月"), None);
        assert_eq!(window_length("999999999999d"), None);
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

        // One connection for both, because the test pool holds exactly one and the guard being
        // asserted is the row's, not the connection's: `previous` is what each reader SAW, and the
        // second reader having seen the same nothing is the whole scenario.
        let mut conn = pool.acquire().await.unwrap();
        let first = claim_the_right_to_warn(&mut conn, "claude", "5h", None, 80, reset, noon())
            .await
            .unwrap();
        // The same `None`: the second reader saw no row either, because it read before the first
        // one wrote.
        let second = claim_the_right_to_warn(&mut conn, "claude", "5h", None, 80, reset, noon())
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

    /// A figure served because the vendor said no is never a reason to interrupt anybody.
    ///
    /// The seam the two phases meet at, and the one neither of them could test alone. A rate-limited
    /// provider is degraded to its last good windows, which keep the fidelity they were stored with
    /// — `official`, the highest rung of the ladder — and are marked `stale`. So by the time `warn`
    /// sees it, the one thing standing between an hours-old 99% and the owner's phone is
    /// [`is_outdated`] reading that flag. Fidelity alone would have let it through, which is exactly
    /// the warning `degrade_to_last_good` leaves for phase 4's brake.
    #[tokio::test]
    async fn a_degraded_provider_is_drawn_and_never_announced() {
        let pool = pool().await;
        record(&pool, &measured(0.99), at(BEFORE_RESET))
            .await
            .unwrap();

        let degraded = degrade_to_last_good(
            &pool,
            unmeasured("the usage endpoint answered 429"),
            at(BEFORE_RESET),
        )
        .await;
        assert_eq!(
            degraded[0].fidelity,
            Fidelity::Official,
            "the seam only exists while a degraded provider keeps its stored fidelity"
        );
        assert!(degraded[0].windows[0].stale);

        assert_eq!(warn(&pool, &degraded, at(BEFORE_RESET)).await.unwrap(), 0);
        assert!(
            feed_kinds(&pool).await.is_empty(),
            "an official figure nobody measured this poll must not interrupt the owner"
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

        let report = stored_report(&pool, "the quota sidecar is not running", noon()).await;

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
            stored(&pool, noon()).await.unwrap().len(),
            1,
            "the live reading was not stored"
        );
    }

    /// The fallback is only worth having if it actually carries the figures across.
    #[tokio::test]
    async fn with_the_sidecar_down_the_last_stored_figures_are_what_is_drawn() {
        let pool = pool().await;
        record(&pool, &measured(0.56), at(BEFORE_RESET))
            .await
            .unwrap();

        let report = report(&QuotaRuntime::disabled(), &pool, at(BEFORE_RESET)).await;

        assert_eq!(report.source, Source::Stored);
        assert_eq!(report.providers.len(), 1);
        assert!((report.providers[0].windows[0].used_fraction - 0.56).abs() < f64::EPSILON);
    }

    /// A stub sidecar on a loopback port, answering one canned `GET /quota`.
    ///
    /// `QuotaClient` owns a `reqwest::Client` rather than sitting behind a trait, so the cheapest
    /// honest fake is a real socket — the same shape, and for the same reason, as
    /// `browser_client.rs`'s stub.
    /// **The degradation is wired into `report()`**, not merely available beside it.
    ///
    /// The three cases above call `degrade_to_last_good` directly, so deleting its one call site in
    /// `report()` left all of them green while the shell went back to receiving a dashed ring on
    /// every rate-limited poll. This is the case that goes red for that deletion: it enters through
    /// the route's own function, with the sidecar reporting the failure the way it really does —
    /// inside a 200, as an `unmeasured` provider carrying a reason.
    #[tokio::test]
    async fn report_serves_the_stored_figures_marked_stale_when_a_provider_comes_back_unmeasured() {
        let pool = pool().await;
        record(&pool, &measured(0.56), at(BEFORE_RESET))
            .await
            .unwrap();
        let address = stub_sidecar(serde_json::json!({
            "providers": [{
                "provider": "claude",
                "fidelity": "unmeasured",
                "read_at": BEFORE_RESET,
                "windows": [],
                "detail": "the usage endpoint answered 429",
            }],
            "cached": false,
        }))
        .await;
        let runtime = QuotaRuntime::new(
            crate::quota_client::QuotaClient::new(&address, "bearer".into()),
            "claude".into(),
        );

        let report = report(&runtime, &pool, at(BEFORE_RESET)).await;

        assert_eq!(report.source, Source::Sidecar);
        assert_eq!(report.providers.len(), 1);
        let provider = &report.providers[0];
        assert_eq!(
            provider.fidelity,
            Fidelity::Official,
            "the route answered with the live unmeasured reading instead of the stored one"
        );
        assert_eq!(provider.windows.len(), 1);
        assert!((provider.windows[0].used_fraction - 0.56).abs() < f64::EPSILON);
        assert!(
            provider.windows[0].stale,
            "an old figure was served as fresh"
        );
        assert_eq!(provider.windows[0].state, "stale");
        assert_eq!(
            provider.detail, "the usage endpoint answered 429",
            "the reason the figure is old has to reach the shell with it"
        );
    }

    #[test]
    fn quota_brake_never_pauses_on_an_unmeasured_stale_old_or_absent_reading() {
        let now = chrono::Utc::now();
        let policy = BrakePolicy {
            enabled: true,
            pause_above_percent_5h: 85,
            pause_above_percent_7d: 90,
        };
        let cases = vec![
            vec![Provider {
                provider: "claude".into(),
                fidelity: Fidelity::Unmeasured,
                read_at: now.to_rfc3339(),
                windows: vec![window(1.0, false)],
                detail: String::new(),
                severity: String::new(),
            }],
            vec![Provider {
                provider: "claude".into(),
                fidelity: Fidelity::Official,
                read_at: now.to_rfc3339(),
                windows: vec![window(1.0, true)],
                detail: String::new(),
                severity: String::new(),
            }],
            vec![Provider {
                provider: "claude".into(),
                fidelity: Fidelity::Official,
                read_at: (now - QUOTA_FRESH_FOR - chrono::Duration::seconds(1)).to_rfc3339(),
                windows: vec![window(1.0, false)],
                detail: String::new(),
                severity: String::new(),
            }],
            vec![],
        ];
        for providers in cases {
            assert!(!matches!(
                judge(&policy, "claude", &providers, now),
                Verdict::Over { .. }
            ));
        }
    }

    #[test]
    fn quota_brake_pauses_at_the_threshold_of_a_fresh_measured_window() {
        let now = chrono::Utc::now();
        let policy = BrakePolicy {
            enabled: true,
            pause_above_percent_5h: 85,
            pause_above_percent_7d: 90,
        };
        for (window_name, threshold) in [("5h", 0.85), ("7d", 0.90)] {
            let provider = Provider {
                provider: "claude".into(),
                fidelity: Fidelity::Official,
                read_at: now.to_rfc3339(),
                windows: vec![Window {
                    window: window_name.into(),
                    used_fraction: threshold,
                    resets_at: None,
                    stale: false,
                    state: "ok",
                }],
                detail: String::new(),
                severity: String::new(),
            };
            let Verdict::Over { reason, .. } = judge(&policy, "claude", &[provider], now) else {
                panic!("fresh {window_name} at its threshold must pause")
            };
            assert!(reason.contains("quota"));
        }
    }

    #[test]
    fn quota_brake_only_counts_the_active_runners_provider() {
        let now = chrono::Utc::now();
        let policy = BrakePolicy {
            enabled: true,
            pause_above_percent_5h: 85,
            pause_above_percent_7d: 90,
        };
        let providers = vec![
            Provider {
                provider: "claude".into(),
                fidelity: Fidelity::Official,
                read_at: now.to_rfc3339(),
                windows: vec![window(0.1, false)],
                detail: String::new(),
                severity: String::new(),
            },
            Provider {
                provider: "codex".into(),
                fidelity: Fidelity::Official,
                read_at: now.to_rfc3339(),
                windows: vec![window(1.0, false)],
                detail: String::new(),
                severity: String::new(),
            },
        ];
        assert!(!matches!(
            judge(&policy, "claude", &providers, now),
            Verdict::Over { .. }
        ));
        let Verdict::Over { reason, .. } = judge(&policy, "codex", &providers, now) else {
            panic!("the active provider must count")
        };
        assert!(reason.contains("quota"));
    }

    #[tokio::test]
    async fn quota_brake_disabled_never_pauses_but_still_warns() {
        let pool = pool().await;
        arm(&pool, false, 85, 90).await;
        let now = chrono::Utc::now();
        let address = stub_sidecar(live_answer("claude", "5h", 1.0, None, now)).await;
        let runtime = QuotaRuntime::new(
            crate::quota_client::QuotaClient::new(&address, "bearer".into()),
            "claude".into(),
        );
        assert!(matches!(
            quota_permits_new_run(&pool, Some(&runtime), "claude", now).await,
            QuotaDecision::Allow
        ));
        assert_eq!(feed_kinds(&pool).await, vec![FEED_KIND]);
        assert!(
            !feed_kinds(&pool)
                .await
                .iter()
                .any(|kind| kind == BLIND_FEED_KIND)
        );
    }

    #[tokio::test]
    async fn quota_brake_running_blind_says_so_once_and_names_the_quota() {
        let pool = pool().await;
        arm(&pool, true, 85, 90).await;
        let now = chrono::Utc::now();
        let address = stub_sidecar(serde_json::json!({"providers": [], "cached": false})).await;
        let runtime = QuotaRuntime::new(
            crate::quota_client::QuotaClient::new(&address, "bearer".into()),
            "claude".into(),
        );
        for _ in 0..2 {
            assert!(matches!(
                quota_permits_new_run(&pool, Some(&runtime), "claude", now).await,
                QuotaDecision::Allow
            ));
        }
        let summaries: Vec<String> = sqlx::query_scalar("SELECT summary FROM feed WHERE kind = ?")
            .bind(BLIND_FEED_KIND)
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].starts_with("quota: "));
    }

    #[tokio::test]
    async fn quota_brake_consult_fires_the_warning() {
        let pool = pool().await;
        arm(&pool, true, 85, 90).await;
        let now = chrono::Utc::now();
        let address = stub_sidecar(live_answer("claude", "5h", 0.82, None, now)).await;
        let runtime = QuotaRuntime::new(
            crate::quota_client::QuotaClient::new(&address, "bearer".into()),
            "claude".into(),
        );
        assert!(matches!(
            quota_permits_new_run(&pool, Some(&runtime), "claude", now).await,
            QuotaDecision::Allow
        ));
        assert_eq!(feed_kinds(&pool).await, vec![FEED_KIND]);
    }

    #[tokio::test]
    async fn quota_brake_pause_carries_the_quota_source() {
        let pool = pool().await;
        arm(&pool, true, 85, 90).await;
        let now = chrono::Utc::now();
        let address = stub_sidecar(live_answer("claude", "5h", 1.0, None, now)).await;
        let state = state_with_quota(
            pool,
            QuotaRuntime::new(
                crate::quota_client::QuotaClient::new(&address, "bearer".into()),
                "claude".into(),
            ),
        )
        .await;
        let pause = permits_new_run(&state, now).await;
        let crate::budget::BudgetDecision::Pause { reason, source, .. } = pause else {
            panic!("quota pause must be a pause")
        };
        assert_eq!(source, PAUSE_SOURCE);
        assert!(reason.contains("quota"));
    }

    #[tokio::test]
    async fn quota_brake_settings_default_off_85_90_and_round_trip() {
        let pool = pool().await;
        assert_eq!(
            load_brake_policy(&pool).await.unwrap(),
            BrakePolicy {
                enabled: false,
                pause_above_percent_5h: 85,
                pause_above_percent_7d: 90
            }
        );
        arm(&pool, true, 86, 91).await;
        assert_eq!(
            load_brake_policy(&pool).await.unwrap(),
            BrakePolicy {
                enabled: true,
                pause_above_percent_5h: 86,
                pause_above_percent_7d: 91
            }
        );
    }
}
