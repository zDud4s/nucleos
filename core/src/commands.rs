//! The slash commands a conversation can offer, read off disk.
//!
//! Verified against the CLI before any of this was written, because the whole feature turns on one
//! question nothing in this repository could answer: a command typed into the composer is sent as
//! ordinary prompt text, so does `claude -p "/thing"` expand it or pass it through? It expands it —
//! the run answers `Launching skill: thing` and the file's body arrives as the prompt. Without that
//! the picker below would insert text the model reads literally.

use std::path::{Path, PathBuf};

/// Where a command came from, which is the only thing that explains two commands sharing a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// `.claude/commands/` inside the conversation's own directory.
    Project,
    /// `~/.claude/commands/`, wherever the conversation runs.
    Personal,
    /// Shipped by an installed plugin, and named `plugin:command`.
    Plugin,
}

/// One command a conversation could be about to run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Command {
    /// What is typed after the slash, `dir:name` for a nested one and `plugin:name` for a plugin's.
    pub name: String,
    /// The `description` from the file's front matter, when it has one.
    pub description: Option<String>,
    /// The `argument-hint` from the front matter — what this command expects after its name.
    pub hint: Option<String>,
    pub source: Source,
}

/// How deep a `commands/` folder is walked. Namespaces are one level in the CLI, and a walk with
/// no floor under it is a walk somebody's stray backup folder can make expensive.
const DEPTH: usize = 4;

/// How many command files are read at most, across every source.
const CEILING: usize = 400;

/// The longest front matter this will read before deciding a file has none.
///
/// A command body is prose and can be long; front matter is a handful of keys. Reading past this
/// looking for a closing fence is reading the body, which is how a whole command ends up displayed
/// as its own description.
const FRONT_MATTER_LINES: usize = 40;

/// Where personal and plugin commands live, or `None` when this machine has no home directory.
///
/// The same door `sessions::default_root` opens, one level up: both of them are reading the
/// editor's own folder, and a second way of finding it is a second thing to be wrong.
pub fn home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

/// Every command this conversation could run, project first.
///
/// Three sources, in the order the CLI resolves them, and each one is a different promise: the
/// project's commands belong to this checkout, the personal ones travel with the person, and a
/// plugin's are whatever it shipped. Ordered so that a name defined in more than one place shows
/// the closest one first — which is also the one the CLI runs.
///
/// Absence at any source is nothing, never an error. A directory with no `.claude/commands/` in it
/// is the ordinary case, and a conversation that refused to offer anything because one folder was
/// missing would be refusing over the normal state of the world.
pub fn available(cwd: Option<&Path>, home: Option<&Path>) -> Vec<Command> {
    let mut found = Vec::new();

    if let Some(cwd) = cwd {
        collect(
            &cwd.join(".claude/commands"),
            "",
            Source::Project,
            &mut found,
        );
    }
    let Some(home) = home else {
        return found;
    };
    collect(
        &home.join(".claude/commands"),
        "",
        Source::Personal,
        &mut found,
    );

    // Installed plugins only. The `marketplaces/` tree beside this one lists what COULD be added,
    // and a command from a plugin nobody installed is a command the CLI refuses — the picker would
    // be offering doors that are not there.
    //
    // This reads a layout Claude Code owns rather than one it documents. It degrades the right way:
    // a shape that changed reads as no plugin commands, never as wrong ones, because every level
    // below is walked rather than guessed at.
    let cache = home.join(".claude/plugins/cache");
    for marketplace in folders(&cache) {
        for plugin in folders(&marketplace) {
            let Some(name) = plugin.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            // One level of version directory under the plugin, which is where the files live.
            for version in folders(&plugin) {
                collect(
                    &version.join("commands"),
                    &format!("{name}:"),
                    Source::Plugin,
                    &mut found,
                );
            }
        }
    }

    found
}

/// The directories directly inside `at`, or nothing when it cannot be read.
fn folders(at: &Path) -> Vec<PathBuf> {
    let Ok(reader) = std::fs::read_dir(at) else {
        return Vec::new();
    };
    reader
        .flatten()
        .filter(|item| item.metadata().map(|meta| meta.is_dir()).unwrap_or(false))
        .map(|item| item.path())
        .collect()
}

/// Reads one `commands/` tree into `found`, naming each file `<prefix><namespace>name`.
fn collect(root: &Path, prefix: &str, source: Source, found: &mut Vec<Command>) {
    let mut pending = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((directory, namespace, depth)) = pending.pop() {
        let Ok(reader) = std::fs::read_dir(&directory) else {
            continue;
        };
        for item in reader.flatten() {
            if found.len() >= CEILING {
                return;
            }
            let Some(name) = item.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Ok(metadata) = item.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                if depth + 1 < DEPTH {
                    // A folder is a namespace, and the CLI types it with a colon.
                    pending.push((item.path(), format!("{namespace}{name}:"), depth + 1));
                }
                continue;
            }
            // `.md` and nothing else. This folder collects notes, backups and whatever an editor
            // leaves behind, and each of those offered as a command is a command that does nothing.
            let Some(stem) = name.strip_suffix(".md") else {
                continue;
            };
            let body = std::fs::read_to_string(item.path()).unwrap_or_default();
            let (description, hint) = described(&body);
            found.push(Command {
                name: format!("{prefix}{namespace}{stem}"),
                description,
                hint,
                source,
            });
        }
    }
}

/// The commands worth offering for what has been typed after the slash.
///
/// Matched against the whole name, namespace included, because the namespace is part of what gets
/// typed: somebody reaching for `/ops:deploy` starts by typing `ops`. An empty query is everything,
/// which is what pressing `/` asks for.
pub fn matching(commands: &[Command], query: &str) -> Vec<Command> {
    let needle = query.trim().to_lowercase();
    let mut hits: Vec<Command> = commands
        .iter()
        .filter(|command| needle.is_empty() || command.name.to_lowercase().contains(&needle))
        .cloned()
        .collect();
    // A name that STARTS with what was typed before one that merely contains it, then alphabetical
    // — the same rule the file picker follows, because it is the same gesture.
    hits.sort_by_key(|command| {
        let name = command.name.to_lowercase();
        (
            u8::from(!(needle.is_empty() || name.starts_with(&needle))),
            name,
        )
    });
    hits
}

/// The `description` and `argument-hint` out of a command file's front matter.
///
/// A file without front matter is a command all the same: the body IS the command, and a
/// description is a label on it. So anything unparseable answers `None` rather than guessing — a
/// dangling fence in particular, where reading on would hand back a whole command body as its own
/// one-line description and fill the picker with it.
pub fn described(body: &str) -> (Option<String>, Option<String>) {
    let mut lines = body.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let mut description = None;
    let mut hint = None;
    for line in lines.take(FRONT_MATTER_LINES) {
        if line.trim() == "---" {
            return (description, hint);
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "description" => description = Some(value),
            "argument-hint" => hint = Some(value),
            _ => {}
        }
    }
    // No closing fence within reach: malformed, and nothing read from it can be trusted.
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, body: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    const PROBE: &str =
        "---\ndescription: Ship it\nargument-hint: [message]\n---\n\nCommit and push.\n";

    #[test]
    fn a_project_command_is_offered_by_the_name_it_is_typed_as() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".claude/commands/commit.md", PROBE);

        let found = available(Some(dir.path()), None);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "commit");
        assert_eq!(found[0].description.as_deref(), Some("Ship it"));
        assert_eq!(found[0].hint.as_deref(), Some("[message]"));
        assert_eq!(found[0].source, Source::Project);
    }

    /// A folder under `commands/` is a namespace, and the CLI types it with a colon. Offering
    /// `/deploy` for a file at `commands/ops/deploy.md` would offer a command that does not exist.
    #[test]
    fn a_command_in_a_folder_carries_its_folder_as_a_namespace() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".claude/commands/ops/deploy.md", "Deploy it.\n");

        let found = available(Some(dir.path()), None);

        assert_eq!(found[0].name, "ops:deploy");
        // No front matter is not a failure: the body IS the command, and a description is a label.
        assert_eq!(found[0].description, None);
    }

    /// Personal commands travel with the person and are available in every directory, so they are
    /// read from home rather than from the conversation's own tree.
    #[test]
    fn a_personal_command_is_offered_wherever_the_conversation_runs() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(home.path(), ".claude/commands/notes.md", "Take notes.\n");

        let found = available(Some(project.path()), Some(home.path()));

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "notes");
        assert_eq!(found[0].source, Source::Personal);
    }

    /// An installed plugin's commands are typed `plugin:command`, and only the INSTALLED ones
    /// count: the marketplace tree beside the cache lists plugins that were never added, and
    /// offering those would offer commands the CLI refuses.
    #[test]
    fn an_installed_plugins_commands_are_offered_under_the_plugins_name() {
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".claude/plugins/cache/official/superpowers/5.0.2/commands/brainstorm.md",
            "---\ndescription: Think first\n---\n\nBrainstorm.\n",
        );
        write(
            home.path(),
            ".claude/plugins/marketplaces/official/plugins/never-installed/commands/ghost.md",
            "Ghost.\n",
        );

        let found = available(None, Some(home.path()));

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "superpowers:brainstorm");
        assert_eq!(found[0].description.as_deref(), Some("Think first"));
        assert_eq!(found[0].source, Source::Plugin);
    }

    /// A conversation with no directory still has the person's own commands. Answering with
    /// nothing would be answering about the project rather than about what can be run.
    #[test]
    fn a_conversation_without_a_directory_still_has_personal_commands() {
        let home = tempfile::tempdir().unwrap();
        write(home.path(), ".claude/commands/notes.md", "Take notes.\n");

        let found = available(None, Some(home.path()));

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, Source::Personal);
    }

    /// Only `.md`. The folder collects notes, backups and whatever an editor leaves behind, and
    /// every one of those offered as a command is a command that does nothing.
    #[test]
    fn anything_that_is_not_a_markdown_file_is_not_a_command() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".claude/commands/commit.md", PROBE);
        write(dir.path(), ".claude/commands/commit.md.bak", PROBE);
        write(dir.path(), ".claude/commands/README.txt", "not a command");

        let found = available(Some(dir.path()), None);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "commit");
    }

    /// Typing narrows, and an empty query is the whole list — pressing `/` shows what there is.
    #[test]
    fn matching_narrows_by_name_and_answers_an_empty_query_with_everything() {
        let all = vec![
            Command {
                name: "commit".into(),
                description: None,
                hint: None,
                source: Source::Project,
            },
            Command {
                name: "ops:deploy".into(),
                description: None,
                hint: None,
                source: Source::Project,
            },
            Command {
                name: "notes".into(),
                description: None,
                hint: None,
                source: Source::Personal,
            },
        ];

        assert_eq!(matching(&all, "").len(), 3);
        let hit = matching(&all, "depl");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].name, "ops:deploy");
        // The namespace is part of what is typed, so it has to be searchable too.
        assert_eq!(matching(&all, "ops").len(), 1);
        assert!(matching(&all, "COMMIT").len() == 1, "case must not matter");
    }

    #[test]
    fn front_matter_is_read_and_a_body_without_any_is_still_a_command() {
        assert_eq!(
            described(PROBE),
            (Some("Ship it".to_string()), Some("[message]".to_string()))
        );
        assert_eq!(described("Just a body.\n"), (None, None));
        // A closing fence that never comes is a malformed file, not a licence to read the body as
        // front matter — a whole command body arriving as a "description" would fill the picker.
        assert_eq!(
            described("---\ndescription: dangling\n\nbody"),
            (None, None)
        );
    }
}
