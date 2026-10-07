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
        collect_skills(&cwd.join(".claude/skills"), "", Source::Project, &mut found);
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
    collect_skills(
        &home.join(".claude/skills"),
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
                // A plugin's skills are typed `plugin:skill`, the same shape its commands take.
                // None on this machine declares itself invocable, so this adds nothing today — and
                // it is here so that the day one does, it is not a second thing to remember.
                collect_skills(
                    &version.join("skills"),
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
/// Whether a path is a directory, THROUGH a link.
///
/// `std::fs::metadata` and never `DirEntry::metadata`, which is the bug this function exists to
/// stop happening again: the `DirEntry` method does not traverse links — it answers about the link
/// itself — so on Windows a junction comes back `is_dir() == false` and is skipped as though it were
/// a file. Every user-invocable skill on the machine this was found on is a junction into a folder
/// kept elsewhere, so the picker offered five plugin commands and none of the seventeen skills, and
/// the code carried a comment claiming it had handled exactly this.
///
/// Following links is right HERE and wrong in `mentions.rs`, which refuses to. That module walks a
/// project and promises containment: a link inside it pointing anywhere else is how a walk rooted at
/// a checkout ends up reading a home directory. This one reads paths a person put into their own
/// `.claude/` on purpose, one level deep, taking nothing but front matter — and the CLI resolves
/// them too, so refusing would mean offering fewer commands than the tool actually has.
fn is_directory(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

fn folders(at: &Path) -> Vec<PathBuf> {
    let Ok(reader) = std::fs::read_dir(at) else {
        return Vec::new();
    };
    reader
        .flatten()
        .filter(|item| is_directory(&item.path()))
        .map(|item| item.path())
        .collect()
}

/// Reads one `commands/` tree into `found`, naming each file `<prefix><namespace>name`.
/// Every user-invocable skill under a `.claude/` folder, offered by the name it is typed as.
///
/// Skills are the other half of what a slash opens, and on this CLI they are most of it: the help
/// for `--disable-slash-commands` reads "Disable all skills", and a machine can easily have dozens
/// of skills and not one `commands/` folder. Listing only `commands/` is why the picker came up
/// empty on a machine full of things to run.
///
/// One level deep and no namespaces. A command's folder is a namespace the CLI types with a colon;
/// a skill's folder IS the skill, and the file inside it is always `SKILL.md`. Walking deeper would
/// find the reference files a skill ships beside itself and offer each of them as its own command.
///
/// Named by the DIRECTORY and not by the front matter's `name`. The two normally agree and where
/// they disagree the directory is what works — this repository's own instructions say `/graphify`
/// is typed for a skill whose front matter calls itself `graphify-windows`.
fn collect_skills(root: &Path, prefix: &str, source: Source, found: &mut Vec<Command>) {
    let Ok(reader) = std::fs::read_dir(root) else {
        return;
    };
    for item in reader.flatten() {
        if found.len() >= CEILING {
            return;
        }
        // Through the link, which `DirEntry::metadata` does not do — see `is_directory`. This is
        // where getting it wrong cost the most: a skill library kept elsewhere and junctioned into
        // `.claude/skills` is the ordinary setup, and every one of those skills was invisible.
        if !is_directory(&item.path()) {
            continue;
        }
        let Some(name) = item.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let body = std::fs::read_to_string(item.path().join("SKILL.md")).unwrap_or_default();
        let front = described(&body);
        if !front.user_invocable {
            continue;
        }
        found.push(Command {
            name: format!("{prefix}{name}"),
            description: front.description,
            hint: front.hint,
            source,
        });
    }
}

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
            // Through the link too. A namespace folder can be one as easily as a skill can, and a
            // walk that stopped at it would drop every command underneath. Cycles are not a
            // hazard here even so: `DEPTH` bounds the descent and `CEILING` bounds the count, so a
            // link pointing back at an ancestor ends the walk rather than continuing it forever.
            if is_directory(&item.path()) {
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
            let front = described(&body);
            let (description, hint) = (front.description, front.hint);
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
/// What a command or skill's front matter says about itself.
///
/// A struct rather than the pair this returned, because skills brought a third fact and a growing
/// tuple is a growing chance of two `Option<String>` being read the wrong way round at a call site.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FrontMatter {
    pub description: Option<String>,
    pub hint: Option<String>,
    /// Whether a person may invoke this by typing its name.
    ///
    /// Only skills carry it, and it is the whole reason skills can be listed at all: of the ones on
    /// this machine, the personal ones mostly say `true` and every project skill says nothing —
    /// those are the workflow's own, invoked by an orchestrator and not by a person. Offering them
    /// would put doors in the picker that do not open, which is the failure `available` already
    /// refuses for uninstalled plugins.
    pub user_invocable: bool,
}

pub fn described(body: &str) -> FrontMatter {
    let mut lines = body.lines();
    if lines.next().map(str::trim) != Some("---") {
        return FrontMatter::default();
    }
    let mut description = None;
    let mut hint = None;
    let mut user_invocable = false;
    for line in lines.take(FRONT_MATTER_LINES) {
        if line.trim() == "---" {
            return FrontMatter {
                description,
                hint,
                user_invocable,
            };
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
            // Skills declare this; commands have no such key and never set it. Only `true` counts,
            // so a value nobody can parse leaves a skill out of the picker rather than in it.
            "user-invocable" => user_invocable = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    // No closing fence within reach: malformed, and nothing read from it can be trusted.
    FrontMatter::default()
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

    /// Points `link` at `target` as a directory link, or answers `false` if this account cannot.
    ///
    /// A junction on Windows rather than a symlink, matching `worktree.rs`: `CreateSymbolicLink`
    /// needs elevation or Developer Mode and `mklink /J` needs neither, so the junction is both the
    /// case a normal Windows account actually has and the one a test can build. Rust reports it
    /// through `FileType::is_symlink`, which is exactly what made it invisible.
    fn link_directory(link: &Path, target: &Path) -> bool {
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .arg("/c")
                .arg("mklink")
                .arg("/J")
                .arg(link)
                .arg(target)
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
    }

    /// A skill kept elsewhere and linked into `.claude/skills` is still a skill.
    ///
    /// This is the failure the picker actually had, and it read as "slash commands do not work":
    /// `DirEntry::metadata` answers about the LINK, not about what it points at, so on Windows a
    /// junction is `is_dir() == false` and every linked skill was skipped as though it were a file.
    /// A skill library kept in one folder and linked into `.claude/skills` is the ordinary way to
    /// have one — on the machine this was found on, seventeen of twenty-three skills were junctions
    /// and not one of them was offered.
    #[test]
    fn a_skill_linked_in_from_elsewhere_is_offered_like_any_other() {
        let dir = tempfile::tempdir().unwrap();
        // The real skill, somewhere that is not a `.claude/skills` folder.
        write(
            dir.path(),
            "elsewhere/impeccable/SKILL.md",
            "---\nname: impeccable\ndescription: Make it impeccable\nuser-invocable: true\n---\n\nDo it.\n",
        );
        let home = dir.path().join("home");
        // Joined component by component, so every separator is the platform's own. A `/` inside one
        // of these strings survives into the argument `mklink` is handed, and cmd reads `/skills` as
        // a switch — which fails the link, takes the skip path below, and leaves a test that passes
        // while asserting nothing. That happened once already.
        let skills = home.join(".claude").join("skills");
        std::fs::create_dir_all(&skills).unwrap();

        if !link_directory(
            &skills.join("impeccable"),
            &dir.path().join("elsewhere").join("impeccable"),
        ) {
            // Nothing is asserted on an account that cannot make a link, and saying so is better
            // than a green tick that proved nothing.
            eprintln!("skipped: this account cannot create a directory link");
            return;
        }
        assert!(
            std::fs::symlink_metadata(skills.join("impeccable"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "the fixture must really be a link, or this test proves nothing"
        );

        let found = available(None, Some(&home));

        assert_eq!(
            found.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["impeccable"]
        );
        assert_eq!(found[0].description.as_deref(), Some("Make it impeccable"));
    }

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

    const SKILL: &str = "---\nname: audit\ndescription: Check the work\nargument-hint: \"[area]\"\nuser-invocable: true\n---\n\nDo the audit.\n";

    /// The bug this was written for: a machine with dozens of skills and not one `commands/` folder
    /// showed an empty picker. Slash opens both on this CLI — its own help for
    /// `--disable-slash-commands` reads "Disable all skills" — and the daemon looked at one of them.
    #[test]
    fn a_user_invocable_skill_is_offered_like_a_command() {
        let home = tempfile::tempdir().unwrap();
        write(home.path(), ".claude/skills/audit/SKILL.md", SKILL);

        let found = available(None, Some(home.path()));

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].name, "audit");
        assert_eq!(found[0].description.as_deref(), Some("Check the work"));
        assert_eq!(found[0].hint.as_deref(), Some("[area]"));
        assert_eq!(found[0].source, Source::Personal);
    }

    /// And one that does not say so is left out. Every skill this project ships is of that kind —
    /// they are the workflow's own, driven by an orchestrator — and putting them in the picker
    /// would offer sixteen doors that do not open to anybody who types.
    #[test]
    fn a_skill_that_does_not_declare_itself_invocable_is_not_offered() {
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".claude/skills/maintenance/SKILL.md",
            "---\nname: maintenance\ndescription: Internal\ntools: Read\n---\n\nInternal.\n",
        );

        assert!(available(None, Some(home.path())).is_empty());
    }

    /// The directory is the name, not the front matter's. They normally agree; where they disagree
    /// the directory is what a person types — this repository's own instructions say `/graphify`
    /// for a skill whose front matter calls itself `graphify-windows`.
    #[test]
    fn a_skill_is_named_by_its_folder_and_not_by_its_front_matter() {
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            ".claude/skills/graphify/SKILL.md",
            "---\nname: graphify-windows\ndescription: Draw it\nuser-invocable: true\n---\n",
        );

        let found = available(None, Some(home.path()));

        assert_eq!(found[0].name, "graphify");
    }

    /// A project's skills travel with the checkout, the same split `collect` already draws between
    /// a project's commands and a personal one's.
    #[test]
    fn a_projects_skill_is_offered_beside_the_personal_ones() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        write(home.path(), ".claude/skills/audit/SKILL.md", SKILL);
        write(
            cwd.path(),
            ".claude/skills/deploy/SKILL.md",
            "---\ndescription: Ship it\nuser-invocable: true\n---\n",
        );

        let found = available(Some(cwd.path()), Some(home.path()));

        let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["deploy", "audit"], "project first");
        assert_eq!(found[0].source, Source::Project);
    }

    /// A skill folder holds the reference files it ships beside `SKILL.md`. Walking into them would
    /// offer each as its own command, which is the trap `collect`'s `.md`-only rule already avoids
    /// for the other half of this picker.
    #[test]
    fn what_a_skill_ships_beside_itself_is_not_offered_as_a_command() {
        let home = tempfile::tempdir().unwrap();
        write(home.path(), ".claude/skills/audit/SKILL.md", SKILL);
        write(
            home.path(),
            ".claude/skills/audit/references/checklist.md",
            "---\ndescription: not a command\nuser-invocable: true\n---\n",
        );

        let found = available(None, Some(home.path()));

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].name, "audit");
    }

    /// A folder with no `SKILL.md` is not a skill, and must not be offered as an empty one.
    #[test]
    fn a_folder_that_is_not_a_skill_is_ignored() {
        let home = tempfile::tempdir().unwrap();
        write(home.path(), ".claude/skills/notes/todo.md", "just a note");

        assert!(available(None, Some(home.path())).is_empty());
    }

    #[test]
    fn front_matter_is_read_and_a_body_without_any_is_still_a_command() {
        assert_eq!(
            described(PROBE),
            FrontMatter {
                description: Some("Ship it".to_string()),
                hint: Some("[message]".to_string()),
                // A command file has no such key, and absence must read as "not invocable by
                // name" rather than as a default that would put every command in twice.
                user_invocable: false,
            }
        );
        assert_eq!(described("Just a body.\n"), FrontMatter::default());
        // A closing fence that never comes is a malformed file, not a licence to read the body as
        // front matter — a whole command body arriving as a "description" would fill the picker.
        assert_eq!(
            described("---\ndescription: dangling\n\nbody"),
            FrontMatter::default()
        );
        // Only `true` counts. A value nobody can parse leaves a skill OUT of the picker rather
        // than in it, which is the safe direction: an absent door beats one that does not open.
        assert!(described("---\nname: a\nuser-invocable: true\n---\n").user_invocable);
        assert!(described("---\nname: a\nuser-invocable: TRUE\n---\n").user_invocable);
        assert!(!described("---\nname: a\nuser-invocable: yes\n---\n").user_invocable);
        assert!(!described("---\nname: a\n---\n").user_invocable);
    }
}
