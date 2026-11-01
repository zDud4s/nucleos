//! The location of this machine's legacy settings files, and the credentials it holds.
//!
//! This machine's settings are rows in the database now (`crate::settings`); `~/.nucleos/*.yaml` is
//! what the one-time importer reads, and what it leaves behind as `*.yaml.imported`. [`root`] is that
//! directory, and the `*_FILE` constants are the names the importer looks for in it.
//!
//! What is left of the old fence is the second half of the table: [`SECRETS`], the credentials a
//! person may set and forget but never read back. A credential is a different kind of thing with a
//! different store, which is why it never was a field on a settings row.

use std::path::PathBuf;

/// The directory under the home directory that holds the legacy settings files the importer reads.
const ROOT_DIR: &str = ".nucleos";

/// [`root`] as a person is shown it, and the only spelling any message uses.
///
/// Deliberately not the absolute path: the absolute one is what the daemon opens, this is what
/// somebody can be told to go and edit on any machine without the sentence carrying a username.
/// `council::CONFIG_DISPLAY_PATH` made the same choice first, for the same reason.
pub const ROOT_DISPLAY: &str = "~/.nucleos";

/// `~/.nucleos`, this machine's per-person directory, or `None` when there is no home directory to
/// hang it off.
///
/// `None` is not an error anybody can fix from here: the importer has nothing to read, and every
/// document starts on its defaults, which is what an absent file has always meant.
pub fn root() -> Option<PathBuf> {
    crate::commands::home().map(|home| home.join(ROOT_DIR))
}

/// `file` as a person is shown it: `~/.nucleos/<file>`. See [`ROOT_DISPLAY`].
pub fn display_path(file: &str) -> String {
    format!("{ROOT_DISPLAY}/{file}")
}

/// The legacy file names, one per document: the settings registry's `origin_file` names each by the
/// same string, and that is what the importer looks for.
pub const EMAIL_FILE: &str = "email.yaml";
pub const VOICE_FILE: &str = "voice.yaml";
pub const CALENDAR_FILE: &str = "calendar.yaml";
pub const WEB_FILE: &str = "web.yaml";
pub const BROWSER_FILE: &str = "browser.yaml";
/// The Telegram channel's own settings, under the `channels/` subdirectory of [`root`].
pub const CHANNEL_TELEGRAM_FILE: &str = "channels/telegram.yaml";
pub const GITHUB_FILE: &str = "github.yaml";
pub const COUNCIL_FILE: &str = "council.yaml";
pub const ROUTER_FILE: &str = "router.yaml";
pub const AGY_FILE: &str = "agy.yaml";
pub const DEVTIME_FILE: &str = "devtime.yaml";
pub const VERIFY_FILE: &str = "verify.yaml";

/* -------------------------------------------------------------- the credentials -- */

/// One credential this machine holds, named so the page can ask for it without ever being told it.
///
/// A separate table rather than a field on a settings document because it is a different KIND of
/// thing with a different store and a different rule: a settings document is read and written as
/// text, and a credential may only ever be written. The asymmetry is the point — see [`SECRETS`].
pub struct Secret {
    /// The key in the OS credential store, under the service name `secrets.rs` pins.
    pub key: &'static str,
    /// Which pillar this credential belongs to, matching a settings document's name wherever one
    /// exists, so the page can put the key beside the document it goes with.
    pub area: &'static str,
    /// What it is and what stops working without it, in one sentence.
    pub what: &'static str,
}

/// The credentials the app may set, and the deliberate absence of the one it may not.
///
/// # Why these live in the credential store and not in the settings documents
///
/// Every config type in `config.rs` says it where the temptation was closest: `email.yaml` holds
/// the mailbox but not its password, `web.yaml` holds the provider but not its key,
/// `nucleos-models.yaml` names the hosted model but not the OpenRouter key. Those files are plain
/// text a person opens, copies and pastes into a bug report — and until they moved to
/// `~/.nucleos/` they sat inside a checkout, one `git add` away from somebody's history. Keeping
/// the split is what lets `GET /settings/{scope}/{name}` serve a document's body verbatim.
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
        area: "channels/telegram",
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
/// Total: `daemon-token` is not an error to ask about, it is simply not on this list, and that is
/// an answer.
pub fn secret_for(key: &str) -> Option<&'static Secret> {
    SECRETS.iter().find(|secret| secret.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bot token is listed under the channel's own area, beside the rest of its settings.
    #[test]
    fn the_telegram_token_secret_is_listed_under_the_channels_telegram_area() {
        let secret = secret_for("telegram-token").expect("telegram-token is a listed secret");
        assert_eq!(secret.area, "channels/telegram");
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

    /// Every credential names an area that has a settings document, so the page can put the key beside
    /// the document it goes with rather than in a list of its own.
    #[test]
    fn every_credential_belongs_beside_a_settings_file() {
        for secret in SECRETS {
            assert!(
                crate::settings::documents()
                    .iter()
                    .any(|document| document.name == secret.area),
                "{} names the area {}, which has no settings document",
                secret.key,
                secret.area
            );
        }
    }

    /// Every production channel manifest has exactly one document, named by its `settings_area` and
    /// imported from `<settings_file>.yaml`. The Telegram token's area above depends on that name.
    #[test]
    fn every_channel_manifest_has_one_document_under_its_settings_area() {
        for manifest in crate::channels::registry::PROD_MANIFESTS {
            let rows: Vec<_> = crate::settings::documents()
                .iter()
                .filter(|document| document.name == manifest.settings_area)
                .collect();
            assert_eq!(rows.len(), 1, "{} needs exactly one document", manifest.id);
            assert_eq!(
                rows[0].origin_file,
                format!("{}.yaml", manifest.settings_file),
                "{} document must name the manifest's file",
                manifest.id
            );
        }
        let telegram = crate::settings::doc_type("channels/telegram").expect("registered");
        assert_eq!(telegram.origin_file, CHANNEL_TELEGRAM_FILE);
    }

    /// A machine document's origin file is not a project's file too: one name with two doors and two
    /// answers about who its author is is what the project registry's header introduced its table to end.
    #[test]
    fn no_file_is_claimed_by_both_registries() {
        for document in crate::settings::documents()
            .iter()
            .filter(|document| document.kind == crate::settings::Kind::Machine)
        {
            assert!(
                matches!(
                    crate::ownership::owner_of(crate::ownership::CLAIMS, document.origin_file),
                    crate::ownership::Owner::Repository
                ),
                "{} is this machine's and must not be a project's too",
                document.origin_file
            );
        }
    }

    /// Every machine document's origin file is a relative file under the root (a channel's sits one
    /// directory down, in `channels/`), shown with a tilde. The absence is asserted as hard as the
    /// presence: a file that reached back for `.ai/` would be a setting that exists once per checkout
    /// again.
    #[test]
    fn every_machine_document_is_a_file_under_the_root_and_is_shown_with_a_tilde() {
        for document in crate::settings::documents()
            .iter()
            .filter(|document| document.kind == crate::settings::Kind::Machine)
        {
            assert!(
                !document.origin_file.contains('\\')
                    && !document.origin_file.starts_with('/')
                    && !document
                        .origin_file
                        .split('/')
                        .any(|part| part.is_empty() || part == ".."),
                "{} must be a file path relative to the root",
                document.origin_file
            );
            assert_eq!(
                display_path(document.origin_file),
                format!("~/.nucleos/{}", document.origin_file)
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
}
