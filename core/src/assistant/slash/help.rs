//! The `/help` and `/commands` cards.
//!
//! `/help` lists every command grouped by where it came from, shadowed ones included and marked,
//! because "why does my `/ship` do something else" is the question the card exists to answer.
//! `/help name` describes one.

use super::expand::expand;
use super::parse::Parsed;
use super::registry::{self, Entry};
use super::{CardKind, Outcome, SlashError};
use crate::commands::{self, Source};
use serde_json::{Value, json};
use std::path::Path;

/// How many near names an unknown `/help name` suggests.
const SUGGESTIONS: usize = 5;

/// Answers `/help` and `/commands`: the whole card, or one entry when a name follows.
pub fn run(
    parsed: &Parsed,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Result<Outcome, SlashError> {
    match parsed.argv.first() {
        None => Ok(Outcome::Card {
            kind: CardKind::Help,
            data: overview(cwd, home),
        }),
        Some(name) => describe(name, cwd, home),
    }
}

/// Why this command only partly survives running on every engine: the front matter's reasons, plus
/// the ones only its body shows. An unreadable file keeps the front matter's alone.
fn partial_of(entry: &Entry) -> Vec<String> {
    let command = &entry.command;
    if command.source == Source::Builtin {
        return command.partial.clone();
    }
    let Ok(text) = std::fs::read_to_string(&command.path) else {
        return command.partial.clone();
    };
    let none = Parsed {
        name: command.name.clone(),
        rest: String::new(),
        argv: Vec::new(),
    };
    let base = command.path.parent().unwrap_or(Path::new(""));
    expand(&text, &none, command.kind, base).partial
}

fn entry_json(entry: &Entry) -> Value {
    let command = &entry.command;
    json!({
        "name": command.name,
        "kind": command.kind,
        "hint": command.hint,
        "description": command.description,
        "aliases": command.aliases,
        "partial": partial_of(entry),
        "shadowed_by": entry.shadowed_by,
    })
}

/// `{groups: [{source, entries: [...]}]}`, a group per source that has anything.
fn overview(cwd: Option<&Path>, home: Option<&Path>) -> Value {
    let all = registry::entries(cwd, home);
    let groups: Vec<Value> = [
        Source::Builtin,
        Source::Project,
        Source::Personal,
        Source::Plugin,
    ]
    .into_iter()
    .filter_map(|source| {
        let entries: Vec<Value> = all
            .iter()
            .filter(|entry| entry.command.source == source)
            .map(entry_json)
            .collect();
        (!entries.is_empty()).then(|| json!({ "source": source, "entries": entries }))
    })
    .collect();
    json!({ "groups": groups })
}

/// `{entry, source, partial, path}` for one name; `path` is null for a built-in.
fn describe(name: &str, cwd: Option<&Path>, home: Option<&Path>) -> Result<Outcome, SlashError> {
    let wanted = name.trim_start_matches('/').to_lowercase();
    let found = registry::entries(cwd, home).into_iter().find(|entry| {
        entry.shadowed_by.is_none()
            && (entry.command.name.to_lowercase() == wanted
                || entry
                    .command
                    .aliases
                    .iter()
                    .any(|alias| alias.to_lowercase() == wanted))
    });
    let Some(entry) = found else {
        let listed = registry::listing(cwd, home);
        let suggestions = commands::matching(&listed, &wanted)
            .into_iter()
            .take(SUGGESTIONS)
            .map(|command| command.name)
            .collect();
        return Err(SlashError::Unknown {
            name: name.to_string(),
            suggestions,
        });
    };
    let path = (entry.command.source != Source::Builtin)
        .then(|| entry.command.path.to_string_lossy().into_owned());
    Ok(Outcome::Card {
        kind: CardKind::HelpEntry,
        data: json!({
            "entry": entry_json(&entry),
            "source": entry.command.source,
            "partial": partial_of(&entry),
            "path": path,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::slash::parse::parse;
    use crate::assistant::slash::{CardKind, Outcome, SlashError};
    use std::path::Path;

    fn write(root: &Path, relative: &str, body: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// Runs `text` (a `/help` or `/commands` line) and expects a card.
    fn card(text: &str, cwd: Option<&Path>, home: Option<&Path>) -> (CardKind, serde_json::Value) {
        let parsed = parse(text).unwrap().unwrap();
        match run(&parsed, cwd, home).expect("help should succeed") {
            Outcome::Card { kind, data } => (kind, data),
            other => panic!("expected a card, got {other:?}"),
        }
    }

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
        let cwd = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(
            cwd.path(),
            ".claude/commands/ship.md",
            "---\ndescription: Ship it\nargument-hint: [message]\nmodel: opus\n---\n\nGo.\n",
        );
        // Shadowed by the built-in, but still shown, marked.
        write(cwd.path(), ".claude/commands/help.md", "mine");
        write(home.path(), ".claude/commands/notes.md", "n");
        (cwd, home)
    }

    #[test]
    fn help_card_groups_by_source() {
        let (cwd, home) = fixture();
        let (kind, data) = card("/help", Some(cwd.path()), Some(home.path()));
        assert_eq!(kind, CardKind::Help);

        let groups = data["groups"].as_array().expect("groups should be a list");
        let sources: Vec<&str> = groups
            .iter()
            .map(|g| g["source"].as_str().unwrap())
            .collect();
        assert_eq!(sources, vec!["builtin", "project", "personal"]);

        let entries_of = |source: &str| -> Vec<serde_json::Value> {
            groups.iter().find(|g| g["source"] == source).unwrap()["entries"]
                .as_array()
                .unwrap()
                .clone()
        };
        let by_name = |entries: &[serde_json::Value], name: &str| {
            entries
                .iter()
                .find(|e| e["name"] == name)
                .cloned()
                .unwrap_or_else(|| panic!("{name} missing"))
        };

        let builtin = entries_of("builtin");
        let help = by_name(&builtin, "help");
        assert_eq!(help["kind"], "command");
        assert_eq!(help["aliases"], serde_json::json!(["commands"]));
        assert!(help["shadowed_by"].is_null());

        let project = entries_of("project");
        let ship = by_name(&project, "ship");
        assert_eq!(ship["description"], "Ship it");
        assert_eq!(ship["hint"], "[message]");
        assert_eq!(ship["partial"], serde_json::json!(["model"]));
        // The project's own `help` file is listed in its group, marked as shadowed.
        assert_eq!(by_name(&project, "help")["shadowed_by"], "builtin");

        assert_eq!(by_name(&entries_of("personal"), "notes")["kind"], "command");
    }

    #[test]
    fn help_alias_commands_same_card() {
        let (cwd, home) = fixture();
        let via_help = card("/help", Some(cwd.path()), Some(home.path()));
        let via_alias = card("/commands", Some(cwd.path()), Some(home.path()));
        assert_eq!(via_alias.0, CardKind::Help);
        assert_eq!(via_help, via_alias);

        // The alias with a name gives the entry card, same as `/help name`.
        let named = card("/commands ship", Some(cwd.path()), Some(home.path()));
        assert_eq!(
            named,
            card("/help ship", Some(cwd.path()), Some(home.path()))
        );
    }

    #[test]
    fn help_name_returns_help_entry() {
        let (cwd, home) = fixture();
        let (kind, data) = card("/help ship", Some(cwd.path()), Some(home.path()));
        assert_eq!(kind, CardKind::HelpEntry);
        assert_eq!(data["entry"]["name"], "ship");
        assert_eq!(data["entry"]["description"], "Ship it");
        assert_eq!(data["source"], "project");
        assert_eq!(data["partial"], serde_json::json!(["model"]));
        let path = data["path"]
            .as_str()
            .expect("a custom command shows its file");
        assert!(path.ends_with("ship.md"), "path was {path}");

        // A built-in has no file.
        let (kind, data) = card("/help help", Some(cwd.path()), Some(home.path()));
        assert_eq!(kind, CardKind::HelpEntry);
        assert_eq!(data["entry"]["name"], "help");
        assert_eq!(data["source"], "builtin");
        assert!(data["path"].is_null());

        // An alias names its built-in.
        let (_, data) = card("/help commands", None, None);
        assert_eq!(data["entry"]["name"], "help");
    }

    #[test]
    fn help_name_unknown_is_unknown_command() {
        let parsed = parse("/help nosuchthing").unwrap().unwrap();
        match run(&parsed, None, None) {
            Err(SlashError::Unknown { name, .. }) => assert_eq!(name, "nosuchthing"),
            other => panic!("expected unknown_command, got {other:?}"),
        }
    }
}
