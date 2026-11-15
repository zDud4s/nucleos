//! The provider adapters, and the production registry that finds one by engine.

pub mod claude;
pub mod codex;
#[cfg(any(test, feature = "testkit"))]
pub mod fake;

use chrono::{DateTime, Duration, Utc};

use super::provider::{LimitHit, ProviderAdapter};

/// How long a 429 (the API rate-limiting this account right now) keeps the account out of the
/// pick when no reset is known: a short mark, not a window.
pub const RATE_LIMIT_MARK: Duration = Duration::minutes(5);
/// How long a usage-limit hit with no readable reset keeps the account out. Marks held in memory
/// never expire without a reset, so one is always supplied.
pub const USAGE_LIMIT_FALLBACK: Duration = Duration::hours(5);
/// Window name of a 429 mark; not a quota window, so measured usage never matches it.
pub const RATE_LIMIT_WINDOW: &str = "rate_limit";

/// The window a usage-limit message names, as the pool names windows (`5h`, `7d`); `usage` when
/// the text names none.
fn limit_window(lower: &str) -> &'static str {
    if lower.contains("weekly") || lower.contains("7-day") || lower.contains("7 day") {
        "7d"
    } else if lower.contains("5-hour") || lower.contains("5 hour") {
        "5h"
    } else {
        "usage"
    }
}

/// The text of the last stream-json `result` event, when it is an error.
fn last_error_text(stdout: &str) -> Option<String> {
    let result = stdout.lines().rev().find_map(|line| {
        serde_json::from_str::<serde_json::Value>(line.trim())
            .ok()
            .filter(|e| e.get("type").and_then(|k| k.as_str()) == Some("result"))
    })?;
    if result.get("is_error").and_then(|f| f.as_bool()) != Some(true) {
        return None;
    }
    result.get("result")?.as_str().map(str::to_owned)
}

/// Reads a `claude -p --output-format stream-json` failure as an account limit. Reuses the
/// runner's detectors (`usage_limit_reset`, `failed_on_rate_limit`), so the pool and the runner
/// never disagree on what a limit looks like.
///
/// A usage limit (5-hour, weekly) carries its parsed `resets_at`, or `now + USAGE_LIMIT_FALLBACK`
/// when the message names none. A 429 is a short mark (`RATE_LIMIT_MARK`). Anything else is not
/// a limit: `None`.
pub fn classify_claude_stream(stdout: &str, now: DateTime<Utc>) -> Option<LimitHit> {
    use crate::execution::runner::{failed_on_rate_limit, usage_limit_reset};
    if let Some(reset) = usage_limit_reset(stdout, now) {
        let text = last_error_text(stdout)
            .unwrap_or_default()
            .to_ascii_lowercase();
        return Some(LimitHit {
            window: Some(limit_window(&text).to_owned()),
            resets_at: Some(reset.unwrap_or(now + USAGE_LIMIT_FALLBACK)),
        });
    }
    failed_on_rate_limit(stdout).then(|| LimitHit {
        window: Some(RATE_LIMIT_WINDOW.to_owned()),
        resets_at: Some(now + RATE_LIMIT_MARK),
    })
}

/// "try again in 2 days 3 hours 5 minutes" / "retry after 30 seconds" -> the duration, when the
/// (lowercase) text has one. Units: day(s), hour(s)/hr, minute(s)/min, second(s)/sec.
fn parse_retry_in(lower: &str) -> Option<Duration> {
    let start = lower
        .find("again in ")
        .map(|i| i + "again in ".len())
        .or_else(|| lower.find("retry after ").map(|i| i + "retry after ".len()))?;
    let mut total = Duration::zero();
    let mut found = false;
    let mut pending: Option<i64> = None;
    for word in lower[start..]
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if let Ok(n) = word.parse::<i64>() {
            pending = Some(n);
            continue;
        }
        let Some(n) = pending.take() else {
            if found {
                break;
            }
            continue;
        };
        total += match word {
            w if w.starts_with("day") => Duration::days(n),
            w if w.starts_with("hour") || w == "hr" || w == "hrs" => Duration::hours(n),
            w if w.starts_with("min") => Duration::minutes(n),
            w if w.starts_with("sec") => Duration::seconds(n),
            _ => break,
        };
        found = true;
    }
    found.then_some(total)
}

/// Reads a Codex failure (either stream) as an account limit. Codex prints "You've hit your usage
/// limit ... try again in 4 days 2 hours" (or a 429 / rate-limit error); there is no machine
/// readable reset on stdout, so the reset is read from the "try again in" text when present. A
/// usage limit without a readable reset falls back to `USAGE_LIMIT_FALLBACK`; a plain 429 or
/// rate limit is a short mark.
pub fn classify_codex_text(stdout: &str, stderr: &str, now: DateTime<Utc>) -> Option<LimitHit> {
    let lower = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    let usage = lower.contains("usage limit") || lower.contains("hit your limit");
    let rate = (lower.contains("429") && lower.contains("too many requests"))
        || lower.contains("rate limit")
        || lower.contains("rate_limit_exceeded");
    let retry = parse_retry_in(&lower);
    if usage {
        return Some(LimitHit {
            window: Some(limit_window(&lower).to_owned()),
            resets_at: Some(retry.map_or(now + USAGE_LIMIT_FALLBACK, |d| now + d)),
        });
    }
    rate.then(|| LimitHit {
        window: Some(RATE_LIMIT_WINDOW.to_owned()),
        resets_at: Some(retry.map_or(now + RATE_LIMIT_MARK, |d| now + d)),
    })
}

static CLAUDE: claude::ClaudeAdapter = claude::ClaudeAdapter;
static CODEX: codex::CodexAdapter = codex::CodexAdapter;

/// The one table both `adapter_for` and `pooled_engines` read, so the two cannot drift.
static REGISTRY: &[(&str, &dyn ProviderAdapter)] =
    &[(claude::ENGINE, &CLAUDE), (codex::ENGINE, &CODEX)];

/// The production registry. None for engines outside the pool (local, agy, gemini-api).
pub fn adapter_for(engine: &str) -> Option<&'static dyn ProviderAdapter> {
    REGISTRY
        .iter()
        .find(|(name, _)| *name == engine)
        .map(|(_, adapter)| *adapter)
}

/// The engines the pool can hold an account for, in registry order.
pub fn pooled_engines() -> Vec<&'static str> {
    REGISTRY.iter().map(|(name, _)| *name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEEKLY: &str = r#"{"type":"result","subtype":"success","is_error":true,"result":"You've hit your weekly limit · resets Nov 20, 9am (Europe/Lisbon)"}"#;
    const FIVE_HOUR: &str =
        r#"{"type":"result","is_error":true,"result":"5-hour limit reached ∙ resets 3pm"}"#;
    const NO_RESET: &str = r#"{"type":"result","is_error":true,"result":"You've hit your limit"}"#;
    const RATE_429: &str = r#"{"type":"result","is_error":true,"terminal_reason":"api_error","api_error_status":429,"result":"API Error: 429"}"#;
    const OVERLOADED: &str = r#"{"type":"result","is_error":true,"terminal_reason":"api_error","api_error_status":529,"result":"API Error: overloaded"}"#;

    fn now() -> DateTime<Utc> {
        "2026-11-15T10:00:00Z".parse().unwrap()
    }

    #[test]
    fn claude_weekly_limit_carries_its_parsed_reset() {
        let hit = classify_claude_stream(WEEKLY, now()).unwrap();
        assert_eq!(hit.window.as_deref(), Some("7d"));
        let reset = hit.resets_at.unwrap();
        assert!(reset > now() && reset < now() + Duration::days(8));
    }

    #[test]
    fn claude_five_hour_limit_is_the_5h_window() {
        let hit = classify_claude_stream(FIVE_HOUR, now()).unwrap();
        assert_eq!(hit.window.as_deref(), Some("5h"));
        assert!(hit.resets_at.unwrap() > now());
    }

    #[test]
    fn claude_limit_without_a_reset_gets_the_fallback() {
        let hit = classify_claude_stream(NO_RESET, now()).unwrap();
        assert_eq!(hit.resets_at, Some(now() + USAGE_LIMIT_FALLBACK));
    }

    #[test]
    fn claude_429_is_a_short_mark_and_529_is_not_a_limit() {
        let hit = classify_claude_stream(RATE_429, now()).unwrap();
        assert_eq!(hit.window.as_deref(), Some(RATE_LIMIT_WINDOW));
        assert_eq!(hit.resets_at, Some(now() + RATE_LIMIT_MARK));
        assert!(classify_claude_stream(OVERLOADED, now()).is_none());
        assert!(classify_claude_stream("", now()).is_none());
    }

    #[test]
    fn codex_usage_limit_reads_the_try_again_in_text() {
        let hit = classify_codex_text(
            "",
            "ERROR: You've hit your usage limit. Try again in 2 days 3 hours 5 minutes.",
            now(),
        )
        .unwrap();
        assert_eq!(
            hit.resets_at,
            Some(now() + Duration::days(2) + Duration::hours(3) + Duration::minutes(5))
        );
        let bare = classify_codex_text("You've hit your usage limit.", "", now()).unwrap();
        assert_eq!(bare.resets_at, Some(now() + USAGE_LIMIT_FALLBACK));
    }

    #[test]
    fn codex_rate_limit_is_short_and_other_errors_are_not_limits() {
        let hit = classify_codex_text("", "429 Too Many Requests", now()).unwrap();
        assert_eq!(hit.resets_at, Some(now() + RATE_LIMIT_MARK));
        assert!(classify_codex_text("", "connection reset", now()).is_none());
    }

    #[test]
    fn only_pooled_engines_have_an_adapter() {
        assert!(adapter_for("claude").is_some());
        assert!(adapter_for("codex").is_some());
        assert!(adapter_for("local").is_none());
        assert!(adapter_for("agy").is_none());
    }

    #[test]
    fn every_pooled_engine_has_an_adapter() {
        let engines = pooled_engines();
        assert_eq!(engines, vec!["claude", "codex"]);
        for engine in engines {
            assert!(adapter_for(engine).is_some(), "{engine} has no adapter");
        }
    }
}
