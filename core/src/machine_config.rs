//! Who may write THIS MACHINE's settings, and what a valid one looks like.
//!
//! [`crate::ownership`] answers the same question for a file in a PROJECT, and its header explains
//! at length why nine files are deliberately absent from that table: they are loaded relative to
//! the daemon's own working directory, so a route under `/projects/{id}` that wrote one would edit
//! a single daemon's configuration through a URL naming a project, and would do it identically
//! whichever project was named. That reasoning stands. This module is the other half of it — the
//! same fence, around the files whose owner is the daemon rather than any project.
//!
//! Everything that made the project registry work is kept, because the reasons are unchanged:
//!
//! - **The boundary is DATA.** A table, one row per file, each carrying the sentence that justifies
//!   it, served to the page so the fence can be read and argued with.
//! - **The núcleo owns a file if the núcleo PARSES it.** A claim and a validator arrive together or
//!   not at all; a claim without a parser is permission to write bytes nobody checks.
//! - **The path guard is shared, not re-implemented.** [`crate::ownership::normalise`] is what
//!   decides that a backslash is refused rather than translated, and two copies of that rule would
//!   be two answers to "which file does this string name" on two platforms.
//!
//! # The parsers had to be built before the table could exist
//!
//! Eight of these nine loaders are fail-soft on purpose: `load_email_config` and its neighbours
//! answer a malformed file with defaults and a warning, so a typo in an optional pillar cannot stop
//! the daemon from starting. That is right for startup and useless as a door — a validator that
//! cannot say no admits anything. So each loader now delegates to a `parse_*_config` that returns
//! `Result`, and the loader decides what to do with the `Err`. One grammar, two callers with
//! different appetites for failure, which is what keeps the file the door accepts and the file the
//! daemon reads the same file.
//!
//! # Every row says a restart is needed, and that is a fact and not an oversight
//!
//! `main.rs` reads eight of these exactly once, at startup, and holds the result in `AppState`.
//! Writing the file therefore changes nothing about the running daemon. A settings page that hid
//! this would be worse than no settings page: it would report success for an edit that did not take
//! effect, which is the failure that looks most like success. So `takes_effect` is a field, it is
//! served with the row, and the page is expected to say it out loud.
//!
//! `.ai/nucleos-models.yaml` is the one with a nuance rather than a flat answer, and it gets a
//! sentence of its own below.

use crate::ownership::Validator;

/// One of this machine's settings files, with everything a caller needs to act on it safely.
///
/// `&'static str` throughout, unlike [`crate::ownership::Claim`]'s `Cow`: that table grows a
/// per-project half read off a manifest at request time, and this one cannot. The set of files the
/// daemon parses is a property of the build, so a row that had to be allocated would be a row
/// describing something this module does not have.
pub struct Setting {
    /// Relative to the daemon's working directory, forward slashes — the same spelling `main.rs`
    /// uses to load it, and the reason `main.rs` keeps saying that it matters where the daemon was
    /// launched from. Compared against a caller's path only after [`crate::ownership::normalise`].
    pub path: &'static str,
    /// The area this file configures, as a stable wire value the page keys its own words off.
    pub area: &'static str,
    /// What editing it changes, in one sentence, for the page that shows the fence.
    pub what: &'static str,
    /// When a write to it starts mattering, in one sentence, for the page that must not lie about
    /// it. See the module header.
    pub takes_effect: &'static str,
    /// The parser the candidate must satisfy before a byte is written. Never `None` here — the
    /// middle state that field exists for in the project registry is a workflow's claim, and a
    /// workflow declares no rows in this table.
    pub validate: Validator,
}

fn validate_email(contents: &str) -> Result<(), String> {
    crate::config::parse_email_config(contents).map(|_| ())
}

fn validate_voice(contents: &str) -> Result<(), String> {
    crate::config::parse_voice_config(contents).map(|_| ())
}

fn validate_calendar(contents: &str) -> Result<(), String> {
    crate::config::parse_calendar_config(contents).map(|_| ())
}

fn validate_web(contents: &str) -> Result<(), String> {
    crate::config::parse_web_config(contents).map(|_| ())
}

fn validate_browser(contents: &str) -> Result<(), String> {
    crate::config::parse_browser_config(contents).map(|_| ())
}

fn validate_telegram(contents: &str) -> Result<(), String> {
    crate::config::parse_telegram_config(contents).map(|_| ())
}

fn validate_github(contents: &str) -> Result<(), String> {
    crate::config::parse_github_config(contents).map(|_| ())
}

/// `true`, and [`crate::config::parse_council_config`]'s own doc says why: the door refuses what is
/// wrong about the roster however this machine is configured, and leaves "no local model is up
/// right now" to startup, which is where that is known. Refusing a local seat because Ollama
/// happens to be down would make the file uneditable on exactly the machine that needs it edited.
fn validate_council(contents: &str) -> Result<(), String> {
    crate::config::parse_council_config(contents, true).map(|_| ())
}

fn validate_models(contents: &str) -> Result<(), String> {
    crate::config::parse_models_config(contents).map(|_| ())
}

/// This machine's settings files. Nine rows, and the count is asserted in the tests for the same
/// reason the project registry asserts two: a row added without reading the header above is a row
/// that has not answered the membership rule.
pub static SETTINGS: &[Setting] = &[
    Setting {
        path: ".ai/email.yaml",
        area: "email",
        what: "whether this machine reads a mailbox, which one, how often, what interrupts you, and how long a classified message keeps its body",
        takes_effect: "when the daemon restarts — the mailbox is polled by a sidecar started with this file's contents",
        validate: validate_email,
    },
    Setting {
        path: ".ai/voice.yaml",
        area: "voice",
        what: "whether this machine opens a microphone, the three chords that reach it, the commands that transcribe and speak, and the terms it is told it hears badly",
        takes_effect: "when the daemon restarts — the chords are registered with the operating system at startup",
        validate: validate_voice,
    },
    Setting {
        path: ".ai/calendar.yaml",
        area: "calendar",
        what: "the zone an event means when nobody says, and the window a proposal may land in — emphatically not when you are busy, which comes from real events only",
        takes_effect: "when the daemon restarts",
        validate: validate_calendar,
    },
    Setting {
        path: ".ai/web.yaml",
        area: "web",
        what: "whether this machine searches the web, through whom, and the one list that decides whose extracted text reaches an agent raw",
        takes_effect: "when the daemon restarts — the search sidecar is started with this file's contents",
        validate: validate_web,
    },
    Setting {
        path: ".ai/browser.yaml",
        area: "browser",
        what: "whether this machine drives a browser, and the ceilings on live tabs, profiles and disk it may spend doing it",
        takes_effect: "when the daemon restarts",
        validate: validate_browser,
    },
    Setting {
        path: ".ai/telegram.yaml",
        area: "telegram",
        what: "the standing instructions a Telegram turn is launched with when the chat has none of its own",
        takes_effect: "when the daemon restarts",
        validate: validate_telegram,
    },
    Setting {
        path: ".ai/github.yaml",
        area: "github",
        what: "whether the GitHub pillar is on at all, and the two lists of what a run may read and do there without asking first",
        takes_effect: "when the daemon restarts — and note that this file EXISTING is itself the pillar's opt-in",
        validate: validate_github,
    },
    Setting {
        path: ".ai/council.yaml",
        area: "council",
        what: "who sits on this machine's council, which model or agent answers for each seat, and how long a seat gets",
        takes_effect: "when the daemon restarts",
        validate: validate_council,
    },
    Setting {
        path: crate::config::MODELS_CONFIG_PATH,
        area: "models",
        what: "which model each route runs on, which agent CLI answers a run, and the rows a conversation's model picker offers",
        // The one row whose answer is not flat, and the nuance is worth the longer sentence: the
        // picker is re-read per request by `GET /assistant/models`, so a choice added here appears
        // at once — while `claude_model`, `primary_runner` and the local and hosted routes are used
        // to BUILD runners at startup and do not move until the daemon does.
        takes_effect: "the model picker refreshes at once; the routes themselves are built at startup and move when the daemon restarts",
        validate: validate_models,
    },
];

/// The row for `rel`, or `None` for a path this machine does not configure.
///
/// Pure and total, like [`crate::ownership::owner_of`]: everything not in the table is simply not
/// this module's, and saying so is an answer rather than a refusal to answer.
pub fn setting_for(rel: &str) -> Option<&'static Setting> {
    let path = crate::ownership::normalise(rel)?;
    SETTINGS.iter().find(|setting| setting.path == path)
}

/* ------------------------------------------------------------ the other half -- */

/// One credential this machine holds, named so the page can ask for it without ever being told it.
///
/// The second half of this machine's settings, and it is a separate table rather than a field on
/// [`Setting`] because it is a different KIND of thing with a different store and a different rule:
/// a settings file is read and written as text, and a credential may only ever be written. The
/// asymmetry is the point — see [`SECRETS`].
pub struct Secret {
    /// The key in the OS credential store, under the service name `secrets.rs` pins.
    pub key: &'static str,
    /// Which pillar this credential belongs to, matching a [`Setting::area`] wherever one exists,
    /// so the page can put the key beside the file it goes with.
    pub area: &'static str,
    /// What it is and what stops working without it, in one sentence.
    pub what: &'static str,
}

/// The credentials the app may set, and the deliberate absence of the one it may not.
///
/// # Why these live in the credential store and not in the files above
///
/// Every config type in `config.rs` says it where the temptation was closest: `.ai/email.yaml`
/// holds the mailbox but not its password, `.ai/web.yaml` holds the provider but not its key,
/// `.ai/nucleos-models.yaml` names the hosted model but not the OpenRouter key. Those files are
/// versioned and this is a single-user desktop; a secret in one of them is a secret in somebody's
/// git history. Keeping the split is what lets `GET /config/machine` serve file contents verbatim.
///
/// # Why the app may write them at all
///
/// Four of the five are settable today only by `nucleos-core --set-*`, which reads from stdin
/// rather than argv — a Windows command line is readable by any process running as the same user
/// and is recorded verbatim in PSReadLine's history, so a token passed as an argument is a token on
/// disk in cleartext at the exact moment somebody was securely storing it. That reasoning is about
/// ARGV, and it does not reach a request body over loopback to the one process that already holds
/// every one of these. The fifth, `web-search-api-key`, has no setter at all: `main.rs` reads it
/// and nothing in the repository writes it, so the web pillar is reachable today only by opening
/// Credential Manager by hand.
///
/// # Why `daemon-token` is not here
///
/// It is the app's own key to the daemon. Overwriting it through the API would lock out the caller
/// making the request — the shell included — and the recovery is a restart plus a credential the
/// app can no longer be told. It is minted at startup and rotated by overwrite, and that stays a
/// thing done from a terminal by somebody who understands they are cutting the line they are
/// standing on. `SECRETS` not containing it is the whole of that rule, and a test pins it.
pub static SECRETS: &[Secret] = &[
    Secret {
        key: "github-token",
        area: "github",
        what: "the token every `gh` call is handed as GH_TOKEN; without it the GitHub pillar can read nothing and do nothing",
    },
    Secret {
        key: "email-imap-password",
        area: "email",
        what: "the mailbox's app password; without it the email sidecar does not start, whatever `.ai/email.yaml` says",
    },
    Secret {
        key: "telegram-token",
        area: "telegram",
        what: "the bot token; without it the Telegram sidecar does not start",
    },
    Secret {
        key: "web-search-api-key",
        area: "web",
        what: "the search provider's key; without it the web pillar cannot search, and until now nothing in this app or its CLI could set one",
    },
    Secret {
        key: "openrouter-api-key",
        area: "models",
        what: "OpenRouter's key; without it a hosted chat turn is refused before any request leaves the machine, however `hosted_assistant_model` is set",
    },
];

/// The row for `key`, or `None` for a credential this app does not set.
///
/// Total, like [`setting_for`]: `daemon-token` is not an error to ask about, it is simply not on
/// this list, and that is an answer.
pub fn secret_for(key: &str) -> Option<&'static Secret> {
    SECRETS.iter().find(|secret| secret.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The membership rule, asserted rather than trusted: every row can say no, and says both of
    /// the sentences the page needs.
    ///
    /// The count is pinned so a tenth row cannot arrive without somebody reading the header. That
    /// is the same guard [`crate::ownership`] puts on its own table, and it is here for the same
    /// reason: the dangerous edit to a registry is an addition, not a change.
    #[test]
    fn every_setting_can_refuse_and_says_what_it_changes() {
        assert_eq!(
            SETTINGS.len(),
            9,
            "see the module header before adding a row"
        );
        for setting in SETTINGS {
            assert!(
                (setting.validate)("enabled: [this is not a bool}\n").is_err(),
                "{} must be able to refuse",
                setting.path
            );
            assert!(
                !setting.what.is_empty(),
                "{} must say what it changes",
                setting.path
            );
            assert!(
                !setting.takes_effect.is_empty(),
                "{} must say when a write starts mattering",
                setting.path
            );
        }
    }

    /// The daemon's own token is absent, and every other row says what it is for.
    ///
    /// The absence is the rule, not an oversight: `daemon-token` is the app's key to the daemon,
    /// and writing it through the API would cut the line the caller is standing on. Pinned here so
    /// that adding it becomes a deliberate act somebody has to delete a test for.
    #[test]
    fn the_daemon_token_is_not_a_credential_this_app_may_set() {
        assert!(secret_for("daemon-token").is_none());
        assert!(SECRETS.iter().all(|secret| secret.key != "daemon-token"));
        for secret in SECRETS {
            assert!(
                !secret.what.is_empty(),
                "{} must say what it is for",
                secret.key
            );
        }
    }

    /// Every credential names an area that has a settings file, so the page can put the key beside
    /// the file it goes with rather than in a list of its own.
    #[test]
    fn every_credential_belongs_beside_a_settings_file() {
        for secret in SECRETS {
            assert!(
                SETTINGS.iter().any(|setting| setting.area == secret.area),
                "{} names the area {}, which has no settings file",
                secret.key,
                secret.area
            );
        }
    }

    /// The two registries do not overlap, and this is the assertion that keeps it true.
    ///
    /// A path in both would be a file with two doors and two answers about who its author is —
    /// exactly the confusion the project registry's header introduced its table to end.
    #[test]
    fn no_file_is_claimed_by_both_registries() {
        for setting in SETTINGS {
            assert!(
                matches!(
                    crate::ownership::owner_of(crate::ownership::CLAIMS, setting.path),
                    crate::ownership::Owner::Repository
                ),
                "{} is this machine's and must not be a project's too",
                setting.path
            );
        }
    }

    /// An empty file is valid for seven of the nine, and that property is what makes the page
    /// usable: a settings surface has to be able to write a file that turns a pillar off.
    ///
    /// The two exceptions are named here rather than tolerated, because each is a real difference
    /// in what its file MEANS and the page has to render both:
    ///
    /// - **council** — an empty roster is not a council. `faults` refusing it is the whole of that
    ///   file's contract, and the absent file already means "no council" without anyone writing one.
    /// - **models** — `claude_model` and `codex_model` carry no `serde(default)`, so a file that
    ///   exists must name them. `ModelsConfig::default()` is what an ABSENT file means; it is not
    ///   what an empty one means, and the loader has always drawn that line.
    ///
    /// Discovered by this test rather than asserted from the outset: the first version expected all
    /// nine to accept an empty file, and models said otherwise.
    #[test]
    fn an_empty_file_is_accepted_by_everything_that_defaults() {
        for setting in SETTINGS {
            let verdict = (setting.validate)("");
            match setting.area {
                "council" => assert!(verdict.is_err(), "an empty roster is not a council"),
                "models" => assert!(
                    verdict.is_err(),
                    "a models file that exists must name the two models it is built from"
                ),
                _ => assert!(
                    verdict.is_ok(),
                    "{} must accept an empty file, got {verdict:?}",
                    setting.path
                ),
            }
        }
    }

    /// The shared path guard is reached, so traversal and the backslash are refused here exactly as
    /// they are for a project's files rather than by a second rule that could disagree.
    #[test]
    fn the_shared_path_guard_is_what_answers() {
        assert!(
            setting_for("./.ai/email.yaml").is_some(),
            "the same file, spelled the long way"
        );
        for path in [
            "../.ai/email.yaml",
            ".ai\\email.yaml",
            "/.ai/email.yaml",
            "C:/Projects/nucleos/.ai/email.yaml",
            "",
        ] {
            assert!(
                setting_for(path).is_none(),
                "{path} must not resolve to a row"
            );
        }
    }

    /// Everything else on disk is not this module's, including the neighbours a glob would sweep in
    /// — the `.ai/` workflow harness's files, which the núcleo has never opened, and the two files
    /// that ARE a project's and have their own door.
    #[test]
    fn a_path_outside_the_table_is_not_this_machines_setting() {
        for path in [
            ".ai/project.yaml",
            ".ai/models.yaml",
            ".ai/pricing.yaml",
            ".ai/autopilot.yaml",
            ".ai/workflows.yaml",
            "core/src/http.rs",
            "README.md",
        ] {
            assert!(
                setting_for(path).is_none(),
                "{path} must not be this machine's setting"
            );
        }
    }
}
