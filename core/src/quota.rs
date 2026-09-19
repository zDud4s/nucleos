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
//! **Phase 1 draws; it does not act.** Warnings (D11) and the brake (phase 4) are separate phases on
//! purpose, so the first thing that ships can be watched for a while before anything is allowed to
//! stop the owner's work on its word.

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
        Ok(live) => {
            let cached = live.cached;
            let providers: Vec<Provider> = live.providers.into_iter().map(from_reading).collect();
            // Recorded before answering, and a write that fails does NOT fail the answer: the table
            // is a fallback, so losing it costs the next reader a fresh figure and must not cost
            // this one the figure already in hand.
            if let Err(error) = record(pool, &providers, now).await {
                tracing::warn!(%error, "the quota reading could not be stored");
            }
            QuotaReport {
                providers,
                source: Source::Sidecar,
                cached,
                unreachable: None,
            }
        }
        Err(error) => {
            // The sidecar's own message, which by construction carries no token — see
            // `sidecars/quota/claude`, where that property has a test of its own.
            stored_report(pool, &error.to_string()).await
        }
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
