//! Built-ins and discovered commands, resolved by precedence.
//!
//! One list, ordered built-in, project, personal, plugin; a name already taken by an earlier entry
//! shadows the later one. A shadowed entry stays in `entries` (the `/help` card shows it, marked)
//! and is never returned by `resolve` or `listing`.

use crate::commands::{self, Command, Kind, Source};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A command the daemon implements itself.
struct Builtin {
    name: &'static str,
    description: &'static str,
    hint: Option<&'static str>,
    aliases: &'static [&'static str],
}

/// Every built-in. A phase that adds a command adds one entry here and a branch in its runner.
const BUILTINS: &[Builtin] = &[Builtin {
    name: "help",
    description: "List the commands this conversation can run, or describe one",
    hint: Some("[command]"),
    aliases: &["commands"],
}];

/// Skills that belong to the NucleOS workflow bundle, which the daemon materializes. Expanding one
/// is checked against what the workflow needs (a project, tools, the bundle on disk).
pub const BUNDLE_SKILLS: &[&str] = &["orchestrate"];

/// One command and, when an earlier entry holds its name, who that is.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub command: Command,
    pub shadowed_by: Option<Source>,
}

impl Builtin {
    fn command(&self) -> Command {
        Command {
            name: self.name.to_string(),
            description: Some(self.description.to_string()),
            hint: self.hint.map(str::to_string),
            source: Source::Builtin,
            kind: Kind::Command,
            path: PathBuf::new(),
            partial: Vec::new(),
            aliases: self.aliases.iter().map(|alias| alias.to_string()).collect(),
        }
    }
}

/// Where a source sits in the precedence order.
fn rank(source: Source) -> u8 {
    match source {
        Source::Builtin => 0,
        Source::Project => 1,
        Source::Personal => 2,
        Source::Plugin => 3,
    }
}

/// The bundle skills present in `cwd` that `available` left out.
///
/// A bundle skill is the workflow's own and its front matter does not say `user-invocable`, so
/// `available` skips it; it is still typed by a person, and a command that is never listed is never
/// routed here by the shell. Each one found on disk is offered as a project skill. One that does
/// say `user-invocable` is already in `found` and is not added twice.
fn bundle_skills(cwd: Option<&Path>, found: &[Command]) -> Vec<Command> {
    let Some(cwd) = cwd else {
        return Vec::new();
    };
    BUNDLE_SKILLS
        .iter()
        .copied()
        .filter(|name| {
            !found.iter().any(|command| {
                command.source == Source::Project
                    && command.kind == Kind::Skill
                    && command.name.eq_ignore_ascii_case(name)
            })
        })
        .filter_map(|name| {
            let path = cwd
                .join(".claude")
                .join("skills")
                .join(name)
                .join("SKILL.md");
            // `is_file` follows a junction, which is how a skill library is usually linked in.
            if !path.is_file() {
                return None;
            }
            let front = commands::described(&std::fs::read_to_string(&path).unwrap_or_default());
            // The same reasons, in the same order, `available` gives a skill's front matter.
            let mut partial = Vec::new();
            if front.allowed_tools.is_some() {
                partial.push("allowed-tools".to_string());
            }
            if front.model.is_some() {
                partial.push("model".to_string());
            }
            Some(Command {
                name: name.to_string(),
                description: front.description,
                hint: front.hint,
                source: Source::Project,
                kind: Kind::Skill,
                path,
                partial,
                aliases: Vec::new(),
            })
        })
        .collect()
}

/// Every command, shadowed ones included, in precedence order.
///
/// Within one source a `.md` command comes before a skill of the same name, which `available`
/// already yields (a folder's commands are read before its skills) and the stable sort keeps. A
/// bundle skill is appended after them, so within the project source it comes last.
pub fn entries(cwd: Option<&Path>, home: Option<&Path>) -> Vec<Entry> {
    let mut all: Vec<Command> = BUILTINS.iter().map(Builtin::command).collect();
    all.extend(commands::available(cwd, home));
    let bundle = bundle_skills(cwd, &all);
    all.extend(bundle);
    all.sort_by_key(|command| rank(command.source));

    // Lower-cased name or alias -> the source that holds it.
    let mut taken: HashMap<String, Source> = HashMap::new();
    all.into_iter()
        .map(|command| {
            let name = command.name.to_lowercase();
            let shadowed_by = taken.get(&name).copied();
            if shadowed_by.is_none() {
                taken.insert(name, command.source);
                for alias in &command.aliases {
                    taken.insert(alias.to_lowercase(), command.source);
                }
            }
            Entry {
                command,
                shadowed_by,
            }
        })
        .collect()
}

/// The command a typed name runs: case-insensitive, an alias counts, a shadowed entry never does.
pub fn resolve(cwd: Option<&Path>, home: Option<&Path>, name: &str) -> Option<Command> {
    let wanted = name.to_lowercase();
    entries(cwd, home)
        .into_iter()
        .filter(|entry| entry.shadowed_by.is_none())
        .map(|entry| entry.command)
        .find(|command| {
            command.name.to_lowercase() == wanted
                || command
                    .aliases
                    .iter()
                    .any(|alias| alias.to_lowercase() == wanted)
        })
}

/// What the picker offers: every entry that can be run.
pub fn listing(cwd: Option<&Path>, home: Option<&Path>) -> Vec<Command> {
    entries(cwd, home)
        .into_iter()
        .filter(|entry| entry.shadowed_by.is_none())
        .map(|entry| entry.command)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Kind, Source};
    use std::path::Path;

    fn write(root: &Path, relative: &str, body: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    const SKILL: &str = "---\ndescription: a skill\nuser-invocable: true\n---\n\nBody.\n";

    #[test]
    fn registry_builtin_beats_project_personal_plugin() {
        let cwd = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        // `help` is a built-in; a project file of the same name must not take it over.
        write(cwd.path(), ".claude/commands/help.md", "mine");
        // `ship` exists in project and personal: the project one wins.
        write(cwd.path(), ".claude/commands/ship.md", "project ship");
        write(home.path(), ".claude/commands/ship.md", "personal ship");
        // A plugin command is only reachable by its prefixed name, and comes last in the listing.
        write(
            home.path(),
            ".claude/plugins/cache/mkt/plug/1.0/commands/go.md",
            "plugin go",
        );

        let (cwd, home) = (Some(cwd.path()), Some(home.path()));
        assert_eq!(resolve(cwd, home, "help").unwrap().source, Source::Builtin);
        assert_eq!(resolve(cwd, home, "ship").unwrap().source, Source::Project);
        assert_eq!(
            resolve(cwd, home, "plug:go").unwrap().source,
            Source::Plugin
        );
        assert!(resolve(cwd, home, "nope").is_none());

        // Names are compared without case.
        assert_eq!(resolve(cwd, home, "HELP").unwrap().source, Source::Builtin);
        assert_eq!(resolve(cwd, home, "Ship").unwrap().source, Source::Project);

        // The listing is ordered built-in, project, personal, plugin.
        let order: Vec<Source> = entries(cwd, home)
            .iter()
            .map(|e| e.command.source)
            .collect();
        let mut sorted = order.clone();
        sorted.sort_by_key(|s| match s {
            Source::Builtin => 0,
            Source::Project => 1,
            Source::Personal => 2,
            Source::Plugin => 3,
        });
        assert_eq!(order, sorted);
    }

    #[test]
    fn registry_md_beats_skill_same_name() {
        let cwd = tempfile::tempdir().unwrap();
        write(cwd.path(), ".claude/commands/deploy.md", "command deploy");
        write(cwd.path(), ".claude/skills/deploy/SKILL.md", SKILL);

        let found = resolve(Some(cwd.path()), None, "deploy").unwrap();
        assert_eq!(found.kind, Kind::Command);

        // The skill is still known, marked as shadowed by the same source.
        let all = entries(Some(cwd.path()), None);
        let skill = all
            .iter()
            .find(|e| e.command.name == "deploy" && e.command.kind == Kind::Skill)
            .expect("the skill should still be listed in the full entries");
        assert_eq!(skill.shadowed_by, Some(Source::Project));
    }

    #[test]
    fn registry_shadowed_marked_and_excluded_from_listing() {
        let cwd = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(cwd.path(), ".claude/commands/help.md", "mine");
        write(cwd.path(), ".claude/commands/ship.md", "project ship");
        write(home.path(), ".claude/commands/ship.md", "personal ship");
        write(home.path(), ".claude/commands/only-mine.md", "x");
        let (cwd, home) = (Some(cwd.path()), Some(home.path()));

        // The full entries keep every one, each shadowed one naming who shadows it.
        let all = entries(cwd, home);
        let shadow_of = |name: &str, source: Source| {
            all.iter()
                .find(|e| e.command.name == name && e.command.source == source)
                .unwrap_or_else(|| panic!("{name} from {source:?} missing"))
                .shadowed_by
        };
        assert_eq!(shadow_of("help", Source::Project), Some(Source::Builtin));
        assert_eq!(shadow_of("ship", Source::Personal), Some(Source::Project));
        assert_eq!(shadow_of("ship", Source::Project), None);
        assert_eq!(shadow_of("help", Source::Builtin), None);
        assert_eq!(shadow_of("only-mine", Source::Personal), None);

        // The picker listing leaves them out: one `help`, one `ship`.
        let names: Vec<(String, Source)> = listing(cwd, home)
            .into_iter()
            .map(|c| (c.name, c.source))
            .collect();
        assert_eq!(names.iter().filter(|(n, _)| n == "help").count(), 1);
        assert!(names.contains(&("help".to_string(), Source::Builtin)));
        assert_eq!(names.iter().filter(|(n, _)| n == "ship").count(), 1);
        assert!(names.contains(&("ship".to_string(), Source::Project)));
        assert!(names.contains(&("only-mine".to_string(), Source::Personal)));
    }

    #[test]
    fn registry_aliases_are_not_entries() {
        let (cwd, home) = (None, None);
        // `/commands` is the help built-in under another name.
        let via_alias = resolve(cwd, home, "commands").expect("alias should resolve");
        let direct = resolve(cwd, home, "help").unwrap();
        assert_eq!(via_alias.name, "help");
        assert_eq!(via_alias, direct);
        assert_eq!(direct.aliases, vec!["commands".to_string()]);
        assert_eq!(direct.source, Source::Builtin);

        // Neither the listing nor the entries have a row called `commands`.
        assert!(listing(cwd, home).iter().all(|c| c.name != "commands"));
        assert!(
            entries(cwd, home)
                .iter()
                .all(|e| e.command.name != "commands")
        );
        assert_eq!(
            listing(cwd, home)
                .iter()
                .filter(|c| c.name == "help")
                .count(),
            1
        );
    }

    /// The workflow's skill, as the bundle ships it: no `user-invocable` key.
    const ORCHESTRATE: &str = "---\nname: orchestrate\ndescription: Run the workflow\n---\n\nGo.\n";

    #[test]
    fn registry_bundle_skill_listed_without_user_invocable() {
        let cwd = tempfile::tempdir().unwrap();
        write(
            cwd.path(),
            ".claude/skills/orchestrate/SKILL.md",
            ORCHESTRATE,
        );
        let cwd = Some(cwd.path());

        let found = resolve(cwd, None, "orchestrate").expect("the bundle skill resolves");
        assert_eq!(found.source, Source::Project);
        assert_eq!(found.kind, Kind::Skill);
        assert_eq!(found.description.as_deref(), Some("Run the workflow"));
        assert!(found.path.ends_with(".claude/skills/orchestrate/SKILL.md"));
        assert_eq!(
            resolve(cwd, None, "Orchestrate").map(|c| c.name),
            Some("orchestrate".to_string())
        );

        let listed = listing(cwd, None);
        assert_eq!(
            listed.iter().filter(|c| c.name == "orchestrate").count(),
            1,
            "{listed:?}"
        );
    }

    #[test]
    fn registry_bundle_skill_absent_without_file_or_cwd() {
        let cwd = tempfile::tempdir().unwrap();
        assert!(resolve(Some(cwd.path()), None, "orchestrate").is_none());
        assert!(resolve(None, None, "orchestrate").is_none());
    }

    #[test]
    fn registry_bundle_skill_not_doubled_when_user_invocable() {
        let cwd = tempfile::tempdir().unwrap();
        write(cwd.path(), ".claude/skills/orchestrate/SKILL.md", SKILL);
        let all = entries(Some(cwd.path()), None);
        assert_eq!(
            all.iter()
                .filter(|e| e.command.name == "orchestrate")
                .count(),
            1
        );
    }

    #[test]
    fn registry_bundle_skill_shadowed_by_project_command() {
        let cwd = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(
            cwd.path(),
            ".claude/skills/orchestrate/SKILL.md",
            ORCHESTRATE,
        );
        // The person's own project command of the same name takes the name.
        write(cwd.path(), ".claude/commands/orchestrate.md", "my own");
        let (cwd, home) = (Some(cwd.path()), Some(home.path()));

        let found = resolve(cwd, home, "orchestrate").unwrap();
        assert_eq!((found.source, found.kind), (Source::Project, Kind::Command));
        let skill = entries(cwd, home)
            .into_iter()
            .find(|e| e.command.name == "orchestrate" && e.command.kind == Kind::Skill)
            .expect("the bundle skill is still an entry");
        assert_eq!(skill.shadowed_by, Some(Source::Project));
    }

    #[test]
    fn registry_bundle_skill_beats_personal_command() {
        let cwd = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(
            cwd.path(),
            ".claude/skills/orchestrate/SKILL.md",
            ORCHESTRATE,
        );
        // A personal command of the same name loses to the project's bundle skill.
        write(home.path(), ".claude/commands/orchestrate.md", "personal");

        let found = resolve(Some(cwd.path()), Some(home.path()), "orchestrate").unwrap();
        assert_eq!((found.source, found.kind), (Source::Project, Kind::Skill));
    }
}
