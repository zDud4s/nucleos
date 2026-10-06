//! Who may write THIS MACHINE's settings, and what a valid one looks like.
//!
//! [`crate::ownership`] answers the same question for a file in a PROJECT, and its header explains
//! at length why these files are deliberately absent from that table: they are this machine's, so a
//! route under `/projects/{id}` that wrote one would edit a single daemon's configuration through a
//! URL naming a project, and would do it identically whichever project was named. That reasoning
//! stands. This module is the other half of it — the same fence, around the files whose owner is
//! the daemon rather than any project.
//!
//! # Where they live: `~/.nucleos/`, and no longer the daemon's working directory
//!
//! They used to be `.ai/<file>.yaml` relative to wherever the daemon was launched, which made two
//! mistakes at once. `.ai/` is the agent workflow harness's directory, not the product's, so the
//! product's settings sat inside somebody else's folder; and "relative to the working directory"
//! meant a machine with twenty worktrees had twenty candidate copies of every setting, of which the
//! daemon read whichever one it happened to be started beside. [`root`] is one directory per
//! person, the same one the council's roster and the workflow library already used, and every row
//! below is a file name relative to it. [`migrate_legacy`] copies the old files over once, at
//! startup, and never deletes one.
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
//! Nine of these ten loaders are fail-soft on purpose: `load_email_config` and its neighbours
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
//! `nucleos-models.yaml` is the one with a nuance rather than a flat answer, and it gets a
//! sentence of its own below.

use std::path::{Path, PathBuf};

use crate::ownership::Validator;

/// The directory under the home directory that holds every row of [`SETTINGS`].
const ROOT_DIR: &str = ".nucleos";

/// [`root`] as a person is shown it, and the only spelling any message uses.
///
/// Deliberately not the absolute path: the absolute one is what the daemon opens, this is what
/// somebody can be told to go and edit on any machine without the sentence carrying a username.
/// `council::CONFIG_DISPLAY_PATH` made the same choice first, for the same reason.
pub const ROOT_DISPLAY: &str = "~/.nucleos";

/// Where this machine's settings live, or `None` when there is no home directory to hang them off.
///
/// `None` is not an error anybody can fix from here: every loader treats it as "the file is
/// absent" and the pillar starts on its defaults, which is what an absent file has always meant.
/// The settings routes refuse by name rather than guess at another directory.
pub fn root() -> Option<PathBuf> {
    crate::commands::home().map(|home| home.join(ROOT_DIR))
}

/// `file` as a person is shown it: `~/.nucleos/<file>`. See [`ROOT_DISPLAY`].
pub fn display_path(file: &str) -> String {
    format!("{ROOT_DISPLAY}/{file}")
}

/// The file names, one per row, so `main.rs` loads each from the same string the table serves.
pub const EMAIL_FILE: &str = "email.yaml";
pub const VOICE_FILE: &str = "voice.yaml";
pub const CALENDAR_FILE: &str = "calendar.yaml";
pub const WEB_FILE: &str = "web.yaml";
pub const BROWSER_FILE: &str = "browser.yaml";
pub const TELEGRAM_FILE: &str = "telegram.yaml";
pub const GITHUB_FILE: &str = "github.yaml";
pub const COUNCIL_FILE: &str = "council.yaml";
pub const ROUTER_FILE: &str = "router.yaml";
pub const DEVTIME_FILE: &str = "devtime.yaml";

/// One of this machine's settings files, with everything a caller needs to act on it safely.
///
/// `&'static str` throughout, unlike [`crate::ownership::Claim`]'s `Cow`: that table grows a
/// per-project half read off a manifest at request time, and this one cannot. The set of files the
/// daemon parses is a property of the build, so a row that had to be allocated would be a row
/// describing something this module does not have.
pub struct Setting {
    /// A file name relative to [`root`] — the same string `main.rs` joins onto it to load the file,
    /// and the row's identity on the wire. Compared against a caller's path only after
    /// [`crate::ownership::normalise`]. A person is shown [`display_path`] of it, never this alone.
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

/// The llm-router adviser's file. `parse_config` is the grammar and also the loopback fence, so the
/// door refuses a router URL off this machine exactly as the daemon would at startup.
fn validate_router(contents: &str) -> Result<(), String> {
    crate::route_advice::parse_config(contents).map(|_| ())
}

/// Parses, and then asks the one question parsing cannot: whether the local route this file
/// describes is a route the daemon will actually build.
///
/// All three of `local_engine`'s refusals deserialise perfectly -- `openai_compatible` with no address, an
/// engine name that is neither, an address on some other machine -- so a door that stopped at
/// serde accepted the write, reported success, and left the daemon to refuse the same file on its
/// next start. That is the failure the module header calls the one that looks most like success.
///
/// It belongs here for the reason [`validate_council`] above gives for keeping liveness OUT: these
/// three are wrong about the file however this machine is configured and whatever is running right
/// now. "No local server is up" stays a startup question; "that is not an engine, and that is not
/// this machine" is one the text answers by itself.
fn validate_models(contents: &str) -> Result<(), String> {
    let parsed = crate::config::parse_models_config(contents)?;
    // The refusal's own sentence, never a paraphrase: it names the key to go and edit, which is
    // the whole reason `LocalEngineRefusal::message` carries the offending value.
    parsed
        .local_engine()
        .map(|_| ())
        .map_err(|refusal| refusal.message())
}

/// This machine's settings files. Ten rows, and the count is asserted in the tests for the same
/// reason the project registry asserts two: a row added without reading the header above is a row
/// that has not answered the membership rule.
pub static SETTINGS: &[Setting] = &[
    Setting {
        path: EMAIL_FILE,
        area: "email",
        what: "whether this machine reads a mailbox, which one, how often, what interrupts you, and how long a classified message keeps its body",
        takes_effect: "when the daemon restarts — the mailbox is polled by a sidecar started with this file's contents",
        validate: validate_email,
    },
    Setting {
        path: VOICE_FILE,
        area: "voice",
        what: "whether this machine opens a microphone, the three chords that reach it, the commands that transcribe and speak, and the terms it is told it hears badly",
        takes_effect: "when the daemon restarts — the chords are registered with the operating system at startup",
        validate: validate_voice,
    },
    Setting {
        path: CALENDAR_FILE,
        area: "calendar",
        what: "the zone an event means when nobody says, and the window a proposal may land in — emphatically not when you are busy, which comes from real events only",
        takes_effect: "when the daemon restarts",
        validate: validate_calendar,
    },
    Setting {
        path: WEB_FILE,
        area: "web",
        what: "whether this machine searches the web, through whom, and the one list that decides whose extracted text reaches an agent raw",
        takes_effect: "when the daemon restarts — the search sidecar is started with this file's contents",
        validate: validate_web,
    },
    Setting {
        path: BROWSER_FILE,
        area: "browser",
        what: "whether this machine drives a browser, and the ceilings on live tabs, profiles and disk it may spend doing it",
        takes_effect: "when the daemon restarts",
        validate: validate_browser,
    },
    Setting {
        path: TELEGRAM_FILE,
        area: "telegram",
        what: "the standing instructions a Telegram turn is launched with when the chat has none of its own",
        takes_effect: "when the daemon restarts",
        validate: validate_telegram,
    },
    Setting {
        path: GITHUB_FILE,
        area: "github",
        what: "whether the GitHub pillar is on at all, and the two lists of what a run may read and do there without asking first",
        takes_effect: "when the daemon restarts — and note that this file EXISTING is itself the pillar's opt-in",
        validate: validate_github,
    },
    Setting {
        path: COUNCIL_FILE,
        area: "council",
        what: "who sits on this machine's council, which model or agent answers for each seat, and how long a seat gets",
        takes_effect: "when the daemon restarts",
        validate: validate_council,
    },
    Setting {
        path: ROUTER_FILE,
        area: "router",
        what: "whether this machine asks a loopback llm-router which model and effort a run, a seat or a triage item should use, per surface, and which runners it may move a run to",
        takes_effect: "when the daemon restarts — the router is read once at startup; an absent file leaves routing off",
        validate: validate_router,
    },
    Setting {
        path: crate::config::MODELS_CONFIG_FILE,
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

/* --------------------------------------------------------------- migration -- */

/// Where [`SETTINGS`] used to live, relative to the daemon's working directory.
const LEGACY_DIR: &str = ".ai";

/// Copies each settings file from where it used to live into [`root`], once, and never over one
/// that is already there. Returns the files it copied, in table order.
///
/// `legacy_base` is the directory the daemon was started from — the only place the old relative
/// `.ai/<file>` could have meant anything — and both directories are arguments so a test can point
/// them at temporary ones rather than at a real home.
///
/// **Copy, never move.** The old file is left where it was: it may be tracked in a checkout, and a
/// daemon that deleted a file out of somebody's working tree at startup would be making a change to
/// a repository nobody asked for. After the copy the old one is simply no longer read.
///
/// **Never overwrite.** A file already in the root is somebody's current settings, and the copy is
/// opened with `create_new` so that holds even against a writer racing this one.
///
/// **The council is skipped.** Its roster moved to `~/.nucleos/council.yaml` long before the rest,
/// deliberately without a migration, and every daemon since has ignored `.ai/council.yaml` — so one
/// found there now is a roster nobody has been running, and copying it would resurrect it.
///
/// Failures are logged and skipped, file by file: an optional pillar that stays on its defaults is
/// the same outcome a missing file has always had, and it must not stop the daemon from starting.
pub fn migrate_legacy(root: &Path, legacy_base: &Path) -> Vec<&'static str> {
    let mut copied = Vec::new();
    for setting in SETTINGS {
        if setting.path == COUNCIL_FILE {
            continue;
        }
        let target = root.join(setting.path);
        let source = legacy_base.join(LEGACY_DIR).join(setting.path);
        if target.exists() || !source.is_file() {
            continue;
        }
        let copy = || -> std::io::Result<()> {
            let contents = std::fs::read(&source)?;
            std::fs::create_dir_all(root)?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            std::io::Write::write_all(&mut file, &contents)
        };
        match copy() {
            Ok(()) => {
                tracing::info!(
                    file = setting.path,
                    to = %display_path(setting.path),
                    "copied a settings file from the working directory's .ai/ into this machine's settings; the old file was left where it was and is no longer read"
                );
                copied.push(setting.path);
            }
            Err(error) => tracing::warn!(
                %error,
                file = setting.path,
                "could not copy a settings file from the working directory's .ai/; the pillar starts on its defaults"
            ),
        }
    }
    copied
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
/// Every config type in `config.rs` says it where the temptation was closest: `email.yaml` holds
/// the mailbox but not its password, `web.yaml` holds the provider but not its key,
/// `nucleos-models.yaml` names the hosted model but not the OpenRouter key. Those files are plain
/// text a person opens, copies and pastes into a bug report — and until they moved to
/// `~/.nucleos/` they sat inside a checkout, one `git add` away from somebody's history. Keeping
/// the split is what lets `GET /config/machine` serve file contents verbatim.
///
/// # Why the app may write them at all
///
/// Four of the six are settable today only by `nucleos-core --set-*`, which reads from stdin
/// rather than argv — a Windows command line is readable by any process running as the same user
/// and is recorded verbatim in PSReadLine's history, so a token passed as an argument is a token on
/// disk in cleartext at the exact moment somebody was securely storing it. That reasoning is about
/// ARGV, and it does not reach a request body over loopback to the one process that already holds
/// every one of these. The fifth, `web-search-api-key`, has no setter at all: `main.rs` reads it
/// and nothing in the repository writes it, so the web pillar is reachable today only by opening
/// Credential Manager by hand.
/// The sixth, `typesafe-api-key` (spec A), is set only here: there is no `--set-*` flag for it,
/// because the app is where the judge is switched on.
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
        what: "the mailbox's app password; without it the email sidecar does not start, whatever `~/.nucleos/email.yaml` says",
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
    Secret {
        key: "anthropic-api-key",
        area: "models",
        what: "Anthropic's key, used only to list its newest models in the chat picker; without it the picker uses the built-in catalogue. Takes effect after a daemon restart",
    },
    Secret {
        key: "openai-api-key",
        area: "models",
        what: "OpenAI's key, used only to list its newest models in the chat picker; without it the picker uses the built-in catalogue. Takes effect after a daemon restart",
    },
    Secret {
        key: crate::judge::TYPESAFE_KEY,
        area: "models",
        what: "TypeSafe's key for the autopilot's judge (the Jev); without it a project in observe or enforce decides exactly as it would with the judge off, and nothing is sent",
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
            10,
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

    /// Spec A D3: the judge's key is set and forgotten through the same doors as every other
    /// credential, beside the models' settings file, under the name the client reads.
    #[test]
    fn the_judges_key_is_a_credential_this_app_may_set() {
        let secret = secret_for(crate::judge::TYPESAFE_KEY).expect("listed");
        assert_eq!(secret.area, "models");
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

    /// An empty file is valid for eight of the ten, and that property is what makes the page
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

    /// The llm-router's file is a row, and its door refuses what the daemon would refuse: a URL off
    /// this machine, because the head of a prompt travels in the request.
    #[test]
    fn the_router_file_is_a_row_and_its_door_refuses_a_remote_router() {
        let row = setting_for(ROUTER_FILE).expect("router.yaml is registered");
        assert_eq!(row.area, "router");
        assert!(
            (row.validate)(
                "mode: off
"
            )
            .is_ok()
        );
        assert!(
            (row.validate)(
                "mode: shadow
url: http://example.com:8080
"
            )
            .is_err()
        );
        assert!(
            (row.validate)(
                "mode: sideways
"
            )
            .is_err()
        );
    }

    /// The shared path guard is reached, so traversal and the backslash are refused here exactly as
    /// they are for a project's files rather than by a second rule that could disagree.
    #[test]
    fn the_shared_path_guard_is_what_answers() {
        assert!(
            setting_for("./email.yaml").is_some(),
            "the same file, spelled the long way"
        );
        for path in [
            "../email.yaml",
            "x\\..\\email.yaml",
            "/email.yaml",
            "C:/Users/someone/.nucleos/email.yaml",
            "~/.nucleos/email.yaml",
            // The old spelling names nothing now: the row is a file under the root, and a caller
            // still sending `.ai/` would be writing somewhere the daemon no longer reads.
            ".ai/email.yaml",
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
            "project.yaml",
            "workflows/x/bundle.yaml",
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

    /// A models file that parses, with `extra` appended.
    ///
    /// `claude_model` and `codex_model` are the two keys [`crate::config::ModelsConfig`] requires,
    /// so a fixture without them is refused before anything below is reached — by serde, saying
    /// "missing field `claude_model`", which is not what either test is about.
    fn models_file(extra: &str) -> String {
        format!("claude_model: sonnet\ncodex_model: gpt-5-codex\n{extra}")
    }

    /// The models door refuses a local engine the daemon would refuse at startup.
    ///
    /// `parse_models_config` alone answers only whether the YAML deserialises, and all three of
    /// `local_engine`'s refusals deserialise perfectly: `openai_compatible` with no address, an engine name
    /// that is neither, an address on some other machine. Without this the settings page accepts
    /// the write, reports success, and the daemon then refuses the same file on its next start —
    /// which is the failure the module header calls the one that looks most like success.
    ///
    /// It belongs at the door for the reason [`validate_council`] gives for NOT putting liveness
    /// there: these three are wrong about the FILE however this machine is configured and whatever
    /// is running right now. "No local server is up" stays a startup question; "this is not an
    /// engine, and that is not this machine" is a question the text answers by itself.
    #[test]
    fn the_models_door_refuses_a_local_engine_the_daemon_would_refuse_at_startup() {
        let refused = validate_models(&models_file("local_engine: openai_compatible\n"))
            .expect_err("`openai_compatible` with no address is refused");
        assert_eq!(
            refused,
            crate::config::LocalEngineRefusal::NoBaseUrl.message(),
            "the door says what startup would have said, not a second wording of it"
        );

        assert!(
            validate_models(&models_file("local_engine: llamafile\n")).is_err(),
            "an engine this daemon does not serve is refused"
        );
        assert!(
            validate_models(&models_file(
                "local_engine: openai_compatible\nlocal_base_url: http://127.0.0.1.example.com/v1\n"
            ))
            .is_err(),
            "an address that only begins like the loopback is refused"
        );
    }

    /// An untouched file still passes the same door.
    ///
    /// The guard above is only worth having if it refuses what is wrong and nothing else: every
    /// install that exists today names no engine at all, and a door that started refusing those
    /// would make `nucleos-models.yaml` uneditable on every machine in the field.
    #[test]
    fn a_models_file_that_names_no_local_engine_is_still_accepted() {
        assert!(
            validate_models(&models_file("")).is_ok(),
            "the two required keys and nothing else is a valid file"
        );
        assert!(
            validate_models(&models_file("local_assistant_model: qwen3:8b\n")).is_ok(),
            "today's shape, which resolves to Ollama at its own default address"
        );
        assert!(
            validate_models(&models_file("local_engine: ollama\n")).is_ok(),
            "naming the engine it already used changes nothing"
        );
    }

    /// Every row is a bare file name under the root, and a person is shown it as `~/.nucleos/...`.
    ///
    /// The absence is asserted as hard as the presence, like the council's own test: a row that
    /// reached back for `.ai/` would be a setting that exists once per checkout again.
    #[test]
    fn every_row_is_a_file_under_the_root_and_is_shown_with_a_tilde() {
        for setting in SETTINGS {
            assert!(
                !setting.path.contains(['/', '\\']),
                "{} must be a file name relative to the root",
                setting.path
            );
            assert_eq!(
                display_path(setting.path),
                format!("~/.nucleos/{}", setting.path)
            );
        }
        // The council was here first, and the two must not drift into two ideas of one directory.
        assert_eq!(
            display_path(COUNCIL_FILE),
            crate::council::CONFIG_DISPLAY_PATH
        );
        assert_eq!(
            display_path(crate::config::MODELS_CONFIG_FILE),
            crate::config::MODELS_CONFIG_DISPLAY_PATH
        );
        if let (Some(root), Some(council)) = (root(), crate::council::config_path()) {
            assert_eq!(council, root.join(COUNCIL_FILE));
        }
    }

    /// Two temporary directories standing in for the home root and the directory the daemon was
    /// started from, so no test here ever touches a real `~/.nucleos`.
    fn migration_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("home").join(".nucleos");
        let cwd = temp.path().join("checkout");
        std::fs::create_dir_all(cwd.join(".ai")).unwrap();
        (temp, root, cwd)
    }

    /// A file only the old place has is copied, the root is created for it, and the old one stays.
    #[test]
    fn a_legacy_file_is_copied_into_the_root_and_left_where_it_was() {
        let (_temp, root, cwd) = migration_dirs();
        std::fs::write(cwd.join(".ai/email.yaml"), "enabled: true\n").unwrap();
        std::fs::write(cwd.join(".ai/nucleos-models.yaml"), "claude_model: x\n").unwrap();

        let copied = migrate_legacy(&root, &cwd);

        assert_eq!(copied, vec![EMAIL_FILE, crate::config::MODELS_CONFIG_FILE]);
        assert_eq!(
            std::fs::read_to_string(root.join("email.yaml")).unwrap(),
            "enabled: true\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("nucleos-models.yaml")).unwrap(),
            "claude_model: x\n"
        );
        assert!(
            cwd.join(".ai/email.yaml").exists(),
            "copy, never move: the old file may be tracked in a checkout"
        );
        assert!(
            !root.join("voice.yaml").exists(),
            "nothing is invented for a file the old place did not have"
        );
    }

    /// A file already in the root is somebody's current settings, and it wins.
    #[test]
    fn a_file_already_in_the_root_is_never_overwritten() {
        let (_temp, root, cwd) = migration_dirs();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("web.yaml"), "enabled: false\n").unwrap();
        std::fs::write(cwd.join(".ai/web.yaml"), "enabled: true\n").unwrap();

        assert!(migrate_legacy(&root, &cwd).is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("web.yaml")).unwrap(),
            "enabled: false\n"
        );
    }

    /// The council's `.ai/` roster is a file every daemon since the move has ignored, so it is not
    /// resurrected. See [`migrate_legacy`].
    #[test]
    fn the_councils_old_roster_is_not_migrated() {
        let (_temp, root, cwd) = migration_dirs();
        std::fs::write(cwd.join(".ai/council.yaml"), "members: []\n").unwrap();

        assert!(migrate_legacy(&root, &cwd).is_empty());
        assert!(
            !root.exists(),
            "with nothing to copy, the root is not even created"
        );
    }

    /// A second start copies nothing: the first copy is now the file that wins.
    #[test]
    fn migrating_twice_copies_once() {
        let (_temp, root, cwd) = migration_dirs();
        std::fs::write(cwd.join(".ai/github.yaml"), "enabled: true\n").unwrap();

        assert_eq!(migrate_legacy(&root, &cwd), vec![GITHUB_FILE]);
        std::fs::write(cwd.join(".ai/github.yaml"), "enabled: false\n").unwrap();
        assert!(migrate_legacy(&root, &cwd).is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("github.yaml")).unwrap(),
            "enabled: true\n"
        );
    }
}
