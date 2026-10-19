//! Which credential the Claude CLI is spending, said to the owner when it changes.
//!
//! The CLI picks its own credential: the subscription it is logged in with, and an API key only
//! when there is no login. Measured 2026-10-19 on CLI 2.1.296: a key in the process environment,
//! in the user settings' `env` block, or in `--settings` did not displace a live login. The four
//! conflict resolutions of that morning failed with "Invalid API key" because the login was GONE,
//! and the CLI fell through to a stale key nobody knew it was holding.
//!
//! The daemon does not change that order — the owner's rule is the CLI's rule, subscription first —
//! and it does not force a key when the subscription hits its limit either: the only switch that
//! does (`--bare`) also drops the hooks that gate every tool a run calls. What it does is notice.
//! A run's `init` line names the credential in `apiKeySource` (`"none"` is the subscription), so a
//! run billing a key is seen at its first line, and a 429 at the end of a turn is the limit.
//!
//! Spoken on a CHANGE, never per run: a lost login turns every run into a key-billed one, and one
//! notification per run would bury the one that matters. The state is the daemon's memory, so a
//! restart that finds itself on a key says so once more.

use std::sync::OnceLock;

use sqlx::SqlitePool;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

/// Runs started authenticating with an API key, after the subscription or at the daemon's start.
pub const ON_API_KEY_KIND: &str = "run_on_api_key";
/// Runs are back on the subscription after a stretch on an API key.
pub const ON_SUBSCRIPTION_KIND: &str = "run_on_subscription";
/// A turn ended on the API's 429: the credential's usage limit is reached.
pub const RATE_LIMITED_KIND: &str = "run_rate_limited";

/// What a run told the watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthSignal {
    /// The `init` line's `apiKeySource`.
    Source(String),
    /// A turn ended; `rate_limited` when it ended on a 429.
    TurnEnded { rate_limited: bool },
}

/// The watch's memory. `on_key` is `None` until the first `init` since the daemon started.
#[derive(Debug, Default)]
pub struct AuthWatch {
    on_key: Option<bool>,
    limited: bool,
}

impl AuthWatch {
    /// The feed row a signal calls for, as `(kind, summary)`, or `None` when nothing changed.
    pub fn on(&mut self, signal: &AuthSignal) -> Option<(&'static str, String)> {
        match signal {
            AuthSignal::Source(source) => {
                let on_key = source != "none";
                let was = self.on_key.replace(on_key);
                match (was, on_key) {
                    (Some(true), true) | (Some(false) | None, false) => None,
                    (Some(false) | None, true) => Some((
                        ON_API_KEY_KIND,
                        format!(
                            "Claude runs are authenticating with an API key ({source}), not the \
                             subscription, so they are billed to that key. The CLI only does this \
                             when it has no subscription login: run `claude` and /login."
                        ),
                    )),
                    (Some(true), false) => Some((
                        ON_SUBSCRIPTION_KIND,
                        "Claude runs are back on the subscription; the API key is no longer \
                         billed."
                            .to_string(),
                    )),
                }
            }
            AuthSignal::TurnEnded {
                rate_limited: false,
            } => {
                self.limited = false;
                None
            }
            AuthSignal::TurnEnded { rate_limited: true } => {
                if std::mem::replace(&mut self.limited, true) {
                    return None;
                }
                let credential = match self.on_key {
                    Some(true) => "the API key",
                    _ => "the subscription",
                };
                Some((
                    RATE_LIMITED_KIND,
                    format!(
                        "Claude answered 429: {credential}'s usage limit is reached. Runs fail \
                         until the window resets; nothing switches credential on its own."
                    ),
                ))
            }
        }
    }
}

/// `apiKeySource` of an `init` line, when the CLI wrote one.
pub fn api_key_source(init: &serde_json::Value) -> Option<&str> {
    init.get("apiKeySource").and_then(|source| source.as_str())
}

static SINK: OnceLock<UnboundedSender<AuthSignal>> = OnceLock::new();

/// Starts the watch: from now on every signal a Claude run reports is weighed, and a change is
/// written to the feed, where the owner's notification policy carries it to Telegram and the
/// desktop. Once per process; a second call is ignored.
pub fn watch_claude_auth(pool: SqlitePool) {
    let (tx, mut rx) = unbounded_channel();
    if SINK.set(tx).is_err() {
        return;
    }
    tokio::spawn(async move {
        let mut watch = AuthWatch::default();
        while let Some(signal) = rx.recv().await {
            if let Some((kind, summary)) = watch.on(&signal)
                && let Err(error) =
                    crate::feed::append(&pool, None, kind, &summary, None, None).await
            {
                tracing::warn!(%error, kind, "auth watch could not write the feed");
            }
        }
    });
}

/// Reports a signal. A no-op until [`watch_claude_auth`] ran, which is every test.
pub(super) fn report(signal: AuthSignal) {
    if let Some(sink) = SINK.get() {
        let _ = sink.send(signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(s: &str) -> AuthSignal {
        AuthSignal::Source(s.to_string())
    }

    #[test]
    fn the_subscription_at_start_is_not_news() {
        let mut watch = AuthWatch::default();
        assert_eq!(watch.on(&source("none")), None);
        assert_eq!(watch.on(&source("none")), None);
    }

    #[test]
    fn a_key_at_start_is_said_once() {
        let mut watch = AuthWatch::default();
        let (kind, summary) = watch.on(&source("ANTHROPIC_API_KEY")).unwrap();
        assert_eq!(kind, ON_API_KEY_KIND);
        assert!(summary.contains("ANTHROPIC_API_KEY"), "{summary}");
        assert_eq!(watch.on(&source("ANTHROPIC_API_KEY")), None);
    }

    #[test]
    fn leaving_and_coming_back_are_both_said() {
        let mut watch = AuthWatch::default();
        assert_eq!(watch.on(&source("none")), None);
        assert_eq!(
            watch.on(&source("apiKeyHelper")).map(|(kind, _)| kind),
            Some(ON_API_KEY_KIND)
        );
        assert_eq!(
            watch.on(&source("none")).map(|(kind, _)| kind),
            Some(ON_SUBSCRIPTION_KIND)
        );
    }

    #[test]
    fn a_limit_is_said_once_until_a_turn_gets_through() {
        let mut watch = AuthWatch::default();
        watch.on(&source("none"));
        let limited = AuthSignal::TurnEnded { rate_limited: true };
        let (kind, summary) = watch.on(&limited).unwrap();
        assert_eq!(kind, RATE_LIMITED_KIND);
        assert!(summary.contains("the subscription"), "{summary}");
        assert_eq!(watch.on(&limited), None);
        assert_eq!(
            watch.on(&AuthSignal::TurnEnded {
                rate_limited: false
            }),
            None
        );
        assert!(watch.on(&limited).is_some());
    }

    #[test]
    fn a_limit_on_a_key_names_the_key() {
        let mut watch = AuthWatch::default();
        watch.on(&source("ANTHROPIC_API_KEY"));
        let (_, summary) = watch
            .on(&AuthSignal::TurnEnded { rate_limited: true })
            .unwrap();
        assert!(summary.contains("the API key"), "{summary}");
    }

    #[test]
    fn the_source_is_read_from_the_init_line() {
        let init = serde_json::json!({"type": "system", "subtype": "init", "apiKeySource": "none"});
        assert_eq!(api_key_source(&init), Some("none"));
        assert_eq!(api_key_source(&serde_json::json!({"type": "system"})), None);
    }
}
