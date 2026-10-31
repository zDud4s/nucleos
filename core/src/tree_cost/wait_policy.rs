//! What a session should do during a long wait: park, keep its prompt cache warm, or rotate.
//!
//! Pure decision only, with no I/O: the `wait_policy` table and the `cache_waits` log live
//! elsewhere. [`decide_wait`] is the ordered rule of spec 6.3 with the keep-warm inequality of
//! spec 6.2, not a price comparison between the three actions. The inequality's formula is
//! normative; the examples beside it in 6.2 are orders of magnitude and disagree with it on `n`.

use axum::extract::State;
use axum::response::IntoResponse;
use sqlx::SqlitePool;

use crate::state::AppState;

use super::report::cache_report;

/// One wait to decide on.
#[derive(Debug, Clone)]
pub struct WaitInput {
    pub context_tokens: i64,
    pub expected_wait_s: i64,
    /// Effective cache TTL. Zero or less means no cache to keep, so the wait parks.
    pub ttl_s: i64,
    /// Falls back to [`WaitSettings::resume_probability`] when `None`.
    pub resume_probability: Option<f64>,
    /// Cache write price, as a multiple of the input price.
    pub write_multiplier: f64,
    /// False when an event can wake the work without a live session, which always parks.
    pub needs_live_session: bool,
}

/// The tunable thresholds of the policy.
#[derive(Debug, Clone)]
pub struct WaitSettings {
    pub rotate_context_tokens: i64,
    /// Rotation needs a wait above this multiple of the TTL.
    pub rotate_min_wait_over_ttl: f64,
    pub keep_warm_enabled: bool,
    pub resume_probability: f64,
    /// Cache read price, as a multiple of the input price.
    pub read_multiplier: f64,
}

impl Default for WaitSettings {
    fn default() -> Self {
        Self {
            rotate_context_tokens: 60_000,
            rotate_min_wait_over_ttl: 1.0,
            keep_warm_enabled: false,
            resume_probability: 0.5,
            // The migration seed has no `read_multiplier` key, so this is the value in force
            // whenever the settings are loaded from the `wait_policy` table.
            read_multiplier: 0.1,
        }
    }
}

/// The outcome of [`decide_wait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitAction {
    /// End the live session; an event wakes the work later.
    Park,
    /// Ping before each TTL expiry so that resuming reads the cache instead of rewriting it.
    KeepWarm,
    /// Close with a handoff and resume from a small brief.
    Rotate,
}

impl WaitAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Park => "park",
            Self::KeepWarm => "keep_warm",
            Self::Rotate => "rotate",
        }
    }
}

/// Records a wait decision before the caller parks, keeps warm, or rotates the session.
pub async fn record(
    pool: &SqlitePool,
    session_id: Option<&str>,
    input: &WaitInput,
    action: WaitAction,
) -> sqlx::Result<i64> {
    Ok(sqlx::query(
        "INSERT INTO cache_waits \
         (session_id, context_tokens, expected_wait_s, ttl_s, action, decided_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(input.context_tokens)
    .bind(input.expected_wait_s)
    .bind(input.ttl_s)
    .bind(action.as_str())
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?
    .last_insert_rowid())
}

/// Closes a recorded wait after the caller either did or did not return in time.
pub async fn close(pool: &SqlitePool, id: i64, returned: bool) -> sqlx::Result<()> {
    sqlx::query("UPDATE cache_waits SET returned = ?, returned_at = ? WHERE id = ?")
        .bind(if returned { 1_i64 } else { 0_i64 })
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Loads the persisted policy, retaining conservative defaults for absent or unreadable values.
pub async fn load_settings(pool: &SqlitePool) -> sqlx::Result<WaitSettings> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM wait_policy")
        .fetch_all(pool)
        .await?;
    let mut settings = WaitSettings::default();
    for (key, value) in rows {
        match key.as_str() {
            "rotate_context_tokens" => {
                if let Ok(parsed) = value.parse() {
                    settings.rotate_context_tokens = parsed;
                }
            }
            "rotate_min_wait_over_ttl" => {
                if let Ok(parsed) = value.parse() {
                    settings.rotate_min_wait_over_ttl = parsed;
                }
            }
            "keep_warm_enabled" => settings.keep_warm_enabled = value == "true",
            "resume_probability" => {
                if let Ok(parsed) = value.parse() {
                    settings.resume_probability = parsed;
                }
            }
            "read_multiplier" => {
                if let Ok(parsed) = value.parse() {
                    settings.read_multiplier = parsed;
                }
            }
            _ => {}
        }
    }
    Ok(settings)
}

/// Applies the measured cache report to a proposed wait before evaluating the policy.
pub fn advice(
    input: &WaitInput,
    settings: &WaitSettings,
    report: &super::report::CacheReport,
) -> WaitAction {
    if !report.has_data {
        return WaitAction::Park;
    }
    let Some(ttl_s) = report.effective_ttl_s.value else {
        return WaitAction::Park;
    };
    let report_input = WaitInput {
        ttl_s,
        resume_probability: input.resume_probability.or(report.p_resume),
        ..input.clone()
    };
    decide_wait(&report_input, settings)
}

/// `GET /cost/wait-advice` returns the safe action when no wait estimate was supplied.
pub async fn wait_advice_handler(State(state): State<AppState>) -> impl IntoResponse {
    let settings = load_settings(&state.pool).await.unwrap_or_default();
    let report = cache_report(&state.pool).await.ok();
    let input = WaitInput {
        context_tokens: 0,
        expected_wait_s: 0,
        ttl_s: 0,
        resume_probability: None,
        write_multiplier: 1.0,
        needs_live_session: true,
    };
    let action = report
        .as_ref()
        .map(|report| advice(&input, &settings, report))
        .unwrap_or(WaitAction::Park);
    axum::Json(serde_json::json!({ "action": action.as_str() }))
}

/// Decides one wait. The first matching rule wins, so Rotate beats a keep-warm that would pass.
pub fn decide_wait(inp: &WaitInput, set: &WaitSettings) -> WaitAction {
    if !inp.needs_live_session || inp.ttl_s <= 0 {
        return WaitAction::Park;
    }

    let wait_outlasts_ttl =
        inp.expected_wait_s as f64 > set.rotate_min_wait_over_ttl * inp.ttl_s as f64;
    if inp.context_tokens > set.rotate_context_tokens && wait_outlasts_ttl {
        return WaitAction::Rotate;
    }

    if !set.keep_warm_enabled {
        return WaitAction::Park;
    }

    let pings = keep_warm_pings(inp.expected_wait_s, inp.ttl_s);
    if pings <= 0 {
        return WaitAction::Park;
    }

    let probability = inp.resume_probability.unwrap_or(set.resume_probability);
    let read = set.read_multiplier;
    if (pings as f64) * read < probability * (inp.write_multiplier - read) {
        WaitAction::KeepWarm
    } else {
        WaitAction::Park
    }
}

/// `n = ceil(W / (0.9 T)) - 1`, in integers as `ceil(10 W / 9 T) - 1` so that a wait that is an
/// exact multiple of `0.9 T` picks up no float noise. `ttl_s` must be positive. Saturating, since
/// an absurd wait must decide rather than overflow.
fn keep_warm_pings(expected_wait_s: i64, ttl_s: i64) -> i64 {
    let wait = expected_wait_s.saturating_mul(10);
    let period = ttl_s.saturating_mul(9);
    let round_up = i64::from(wait % period > 0);
    wait / period + round_up - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::tree_cost::measure::TtlDistribution;
    use crate::tree_cost::report::{CacheReport, Figure};

    /// Below the default rotation threshold, so only the keep-warm rule can decide.
    const SMALL_CONTEXT: i64 = 10_000;
    /// Above the default rotation threshold of 60 000.
    const BIG_CONTEXT: i64 = 100_000;
    const ONE_HOUR_S: i64 = 3_600;

    fn input(
        context_tokens: i64,
        expected_wait_s: i64,
        ttl_s: i64,
        resume_probability: Option<f64>,
        write_multiplier: f64,
    ) -> WaitInput {
        WaitInput {
            context_tokens,
            expected_wait_s,
            ttl_s,
            resume_probability,
            write_multiplier,
            needs_live_session: true,
        }
    }

    fn keep_warm_on() -> WaitSettings {
        WaitSettings {
            keep_warm_enabled: true,
            ..WaitSettings::default()
        }
    }

    fn report(has_data: bool, effective_ttl_s: Option<i64>, p_resume: Option<f64>) -> CacheReport {
        CacheReport {
            has_data,
            ttl_distribution: TtlDistribution::default(),
            gap_counts: Figure {
                value: vec![],
                estimated: false,
            },
            miss_rates: Figure {
                value: vec![],
                estimated: false,
            },
            effective_ttl_s: Figure {
                value: effective_ttl_s,
                estimated: false,
            },
            p_resume,
        }
    }

    #[tokio::test]
    async fn record_and_close_a_wait_roundtrip() {
        let pool = crate::testdb::fresh_pool().await;
        let wait = input(SMALL_CONTEXT, ONE_HOUR_S, ONE_HOUR_S, Some(0.75), 2.0);

        let id = record(&pool, Some("session-1"), &wait, WaitAction::KeepWarm)
            .await
            .unwrap();
        close(&pool, id, true).await.unwrap();

        let row: (String, i64, i64, i64, String, Option<i64>, Option<String>) = sqlx::query_as(
            "SELECT session_id, context_tokens, expected_wait_s, ttl_s, action, returned, returned_at \
             FROM cache_waits WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "session-1");
        assert_eq!(row.1, SMALL_CONTEXT);
        assert_eq!(row.2, ONE_HOUR_S);
        assert_eq!(row.3, ONE_HOUR_S);
        assert_eq!(row.4, "keep_warm");
        assert_eq!(row.5, Some(1));
        assert!(row.6.is_some());
    }

    #[tokio::test]
    async fn settings_default_keep_warm_off() {
        let pool = crate::testdb::fresh_pool().await;

        let settings = load_settings(&pool).await.unwrap();

        assert!(!settings.keep_warm_enabled);
        assert_eq!(settings.rotate_context_tokens, 60_000);
        assert_eq!(settings.rotate_min_wait_over_ttl, 1.0);
        assert_eq!(settings.resume_probability, 0.5);
        assert_eq!(settings.read_multiplier, 0.1);
    }

    #[test]
    fn advice_uses_effective_ttl_from_report_when_present() {
        let wait = input(SMALL_CONTEXT, 10_800, 0, None, 2.0);
        let report = report(true, Some(ONE_HOUR_S), Some(0.20));

        assert_eq!(advice(&wait, &keep_warm_on(), &report), WaitAction::KeepWarm);
    }

    #[test]
    fn advice_falls_back_to_park_when_report_has_no_data() {
        let wait = input(SMALL_CONTEXT, 10_800, ONE_HOUR_S, Some(1.0), 2.0);
        let report = report(false, None, None);

        assert_eq!(advice(&wait, &keep_warm_on(), &report), WaitAction::Park);
    }

    #[test]
    fn keep_warm_one_hour_ttl_three_hours_needs_p_above_a_tenth() {
        // n = ceil(10800 / 3240) - 1 = 3, so keep-warm needs 0.3 < 1.9 * P, P above about 0.158.
        // The name comes from the plan and the spec's order-of-magnitude example; the formula is
        // normative. The margins are deliberate: 0.05 and 0.20 stay on the same side of the
        // threshold for any plausible rounding, so this pins the rule and not its float noise.
        let set = keep_warm_on();
        let wait = |p| input(SMALL_CONTEXT, 10_800, ONE_HOUR_S, Some(p), 2.0);

        assert_eq!(decide_wait(&wait(0.05), &set), WaitAction::Park);
        assert_eq!(decide_wait(&wait(0.20), &set), WaitAction::KeepWarm);
    }

    #[test]
    fn keep_warm_one_hour_ttl_twelve_hours_needs_p_above_half() {
        // n = ceil(43200 / 3240) - 1 = 13, so keep-warm needs 1.3 < 1.9 * P, P above about 0.684.
        let set = keep_warm_on();
        let wait = |p| input(SMALL_CONTEXT, 43_200, ONE_HOUR_S, Some(p), 2.0);

        assert_eq!(decide_wait(&wait(0.40), &set), WaitAction::Park);
        assert_eq!(decide_wait(&wait(0.75), &set), WaitAction::KeepWarm);
    }

    #[test]
    fn five_minute_ttl_never_keeps_warm_for_an_hour() {
        // n = ceil(3600 / 270) - 1 = 13 and w - r = 1.15: 1.3 is not below 1.15 even at P = 1.
        let inp = input(SMALL_CONTEXT, ONE_HOUR_S, 300, Some(1.0), 1.25);

        assert_eq!(decide_wait(&inp, &keep_warm_on()), WaitAction::Park);
    }

    #[test]
    fn rotate_wins_for_large_context_when_wait_exceeds_ttl() {
        // n = ceil(7200 / 3240) - 1 = 2, and 0.2 < 1.9: keep-warm would also pass, Rotate goes first.
        let inp = input(BIG_CONTEXT, 7_200, ONE_HOUR_S, Some(1.0), 2.0);

        assert_eq!(decide_wait(&inp, &keep_warm_on()), WaitAction::Rotate);
    }

    #[test]
    fn park_is_default_when_no_live_session_needed() {
        // The same big context and long wait that rotate in the test above.
        let inp = WaitInput {
            needs_live_session: false,
            ..input(BIG_CONTEXT, 7_200, ONE_HOUR_S, Some(1.0), 2.0)
        };

        assert_eq!(decide_wait(&inp, &keep_warm_on()), WaitAction::Park);
    }

    #[test]
    fn keep_warm_disabled_in_default_settings() {
        let set = WaitSettings::default();
        assert_eq!(set.rotate_context_tokens, 60_000);
        assert_eq!(set.rotate_min_wait_over_ttl, 1.0);
        assert!(!set.keep_warm_enabled);
        assert_eq!(set.resume_probability, 0.5);
        assert_eq!(set.read_multiplier, 0.1);

        // n = ceil(3600 / 3240) - 1 = 1, and 0.1 < 1.9: only the disabled flag keeps this parked,
        // which the enabled control below proves.
        let inp = input(SMALL_CONTEXT, ONE_HOUR_S, ONE_HOUR_S, Some(1.0), 2.0);
        assert_eq!(decide_wait(&inp, &set), WaitAction::Park);
        assert_eq!(decide_wait(&inp, &keep_warm_on()), WaitAction::KeepWarm);
    }

    #[test]
    fn boundary_equality_does_not_keep_warm() {
        // 0.25 and 1.25 are exact in binary, so with n = 1 both sides of the comparison are exact:
        // n * r = 0.25 and w - r = 1.0.
        let set = WaitSettings {
            read_multiplier: 0.25,
            ..keep_warm_on()
        };
        let wait = |p| input(SMALL_CONTEXT, ONE_HOUR_S, ONE_HOUR_S, Some(p), 1.25);

        assert_eq!(decide_wait(&wait(0.25), &set), WaitAction::Park);
        assert_eq!(decide_wait(&wait(0.26), &set), WaitAction::KeepWarm);
    }

    #[test]
    fn rotate_needs_strictly_larger_context_and_wait() {
        // Default settings keep keep-warm off, so whatever does not rotate parks.
        let set = WaitSettings::default();
        let threshold = set.rotate_context_tokens;
        let decide = |context, wait_s| {
            decide_wait(
                &input(context, wait_s, ONE_HOUR_S, Some(1.0), 2.0),
                &set,
            )
        };

        // Context exactly at the threshold, with a wait well over the TTL.
        assert_eq!(decide(threshold, 2 * ONE_HOUR_S), WaitAction::Park);
        // Wait exactly at the TTL, with a context well over the threshold.
        assert_eq!(decide(BIG_CONTEXT, ONE_HOUR_S), WaitAction::Park);
        // One past both limits rotates, so the two cases above fail for the reason they state.
        assert_eq!(decide(threshold + 1, ONE_HOUR_S + 1), WaitAction::Rotate);
    }

    #[test]
    fn input_probability_overrides_the_settings_default() {
        // n = 3, so the threshold is about 0.158: 0.05 parks and 0.20 keeps warm.
        let wait = |p| input(SMALL_CONTEXT, 10_800, ONE_HOUR_S, p, 2.0);
        let settings_with = |p| WaitSettings {
            resume_probability: p,
            ..keep_warm_on()
        };

        // Without an input probability, the settings one decides.
        assert_eq!(decide_wait(&wait(None), &settings_with(0.05)), WaitAction::Park);
        assert_eq!(decide_wait(&wait(None), &settings_with(0.20)), WaitAction::KeepWarm);
        // With one, it beats the settings in both directions.
        assert_eq!(decide_wait(&wait(Some(0.20)), &settings_with(0.05)), WaitAction::KeepWarm);
        assert_eq!(decide_wait(&wait(Some(0.05)), &settings_with(0.20)), WaitAction::Park);
    }
}
