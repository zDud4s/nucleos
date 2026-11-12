//! Which credential the Claude CLI is spending, said to the owner when it changes.
//!
//! The CLI picks its own credential. Measured 2026-10-19 on CLI 2.1.296, a key in the process
//! environment did not displace a live login; the four conflict resolutions of that morning failed
//! with "Invalid API key" because the login was GONE and the CLI fell through to a stale key.
//! CORRECTED 2026-10-28: a key in the user settings' `env` block DOES displace a live login — with
//! `ANTHROPIC_API_KEY` there, `claude auth status` (logged in, max) reported
//! `apiKeySource=ANTHROPIC_API_KEY` and chat turns failed "Invalid API key". The CLI injects that
//! block itself, so the spawn cannot strip it from the child env ([`inherited_key_to_strip`] only
//! covers the daemon's own environment); [`settings_env_sets_api_key`] lets the watch say so.
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

/// Whether the spawn should remove `ANTHROPIC_API_KEY` from the child's environment: only when a
/// subscription login is present and the request did not set the key explicitly.
pub fn inherited_key_to_strip(has_login: bool, request_sets_key: bool) -> bool {
    has_login && !request_sets_key
}

/// Whether a `~/.claude/settings.json` text carries `ANTHROPIC_API_KEY` in its `env` block. The CLI
/// applies that block itself, so the daemon cannot strip it; it can only report it.
pub fn settings_env_sets_api_key(settings: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(settings)
        .ok()
        .and_then(|v| v.get("env")?.get("ANTHROPIC_API_KEY").cloned())
        .is_some_and(|key| key.as_str().is_none_or(|s| !s.is_empty()))
}

/// Whether the CLI has a subscription login on disk (`~/.claude/.credentials.json`). Where the
/// login lives in a keychain (macOS) this reads false and the spawn leaves the env alone.
pub fn has_subscription_login() -> bool {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .is_some_and(|home| {
            std::path::Path::new(&home)
                .join(".claude")
                .join(".credentials.json")
                .is_file()
        })
}

/// The user settings' text, when readable; for [`settings_env_sets_api_key`].
pub fn read_user_settings() -> Option<String> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    std::fs::read_to_string(
        std::path::Path::new(&home)
            .join(".claude")
            .join("settings.json"),
    )
    .ok()
}

/// Appends the settings hint to a key-auth summary when the key comes from the environment
/// (`apiKeySource` is `ANTHROPIC_API_KEY`) and the user settings' `env` block sets it.
pub fn with_settings_hint(
    summary: String,
    kind: &str,
    signal: &AuthSignal,
    settings_sets_key: bool,
) -> String {
    let from_env_key =
        matches!(signal, AuthSignal::Source(source) if source == "ANTHROPIC_API_KEY");
    if kind == ON_API_KEY_KIND && from_env_key && settings_sets_key {
        format!(
            "{summary} ANTHROPIC_API_KEY is set in ~/.claude/settings.json env and displaces \
             your login; remove it."
        )
    } else {
        summary
    }
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
            if let Some((kind, summary)) = watch.on(&signal) {
                let settings_sets_key =
                    read_user_settings().is_some_and(|text| settings_env_sets_api_key(&text));
                let summary = with_settings_hint(summary, kind, &signal, settings_sets_key);
                if let Err(error) =
                    crate::feed::append(&pool, None, kind, &summary, None, None).await
                {
                    tracing::warn!(%error, kind, "auth watch could not write the feed");
                }
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
    fn key_is_stripped_only_with_a_login_and_no_explicit_key() {
        assert!(inherited_key_to_strip(true, false));
        assert!(!inherited_key_to_strip(false, false));
        assert!(!inherited_key_to_strip(true, true));
    }

    #[test]
    fn settings_env_key_is_detected() {
        assert!(settings_env_sets_api_key(
            r#"{"env":{"ANTHROPIC_API_KEY":"sk-x"}}"#
        ));
        assert!(!settings_env_sets_api_key(r#"{"env":{"OTHER":"1"}}"#));
        assert!(!settings_env_sets_api_key(
            r#"{"env":{"ANTHROPIC_API_KEY":""}}"#
        ));
        assert!(!settings_env_sets_api_key("not json"));
    }

    #[test]
    fn settings_hint_only_for_an_env_key_set_in_settings() {
        let signal = source("ANTHROPIC_API_KEY");
        let base = "base.".to_string();
        let hinted = with_settings_hint(base.clone(), ON_API_KEY_KIND, &signal, true);
        assert!(hinted.starts_with("base."), "{hinted}");
        assert!(hinted.contains("~/.claude/settings.json"), "{hinted}");
        assert_eq!(
            with_settings_hint(base.clone(), ON_API_KEY_KIND, &signal, false),
            base
        );
        assert_eq!(
            with_settings_hint(base.clone(), ON_API_KEY_KIND, &source("apiKeyHelper"), true),
            base
        );
        assert_eq!(
            with_settings_hint(base.clone(), ON_SUBSCRIPTION_KIND, &signal, true),
            base
        );
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
