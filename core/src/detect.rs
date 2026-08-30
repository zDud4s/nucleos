//! §spec workspace-de-projeto
//!
//! What is already in a folder somebody is about to hand this app.
//!
//! §9's second step, and the one that decides whether the app is hostile to what exists. A project
//! worth adding has been developed for a while: it has a git history, it has commands its people
//! type every day, and — increasingly — it has a written-down way of working. **This repository has
//! one, in `.ai/`, and it built NucleOS.** An app that asked for all of that to be recreated in its
//! own forms before it would admit the project exists would be asking somebody to do a day's work
//! to describe a thing that is sitting right there.
//!
//! So this reads and **proposes**. Nothing here is stored: what gets stored is what somebody ticked,
//! which is the same rule §8 settled for commands. A list that changed by itself because a
//! `package.json` was edited would be noise, and there would be nowhere in it to mark which one is
//! the gate.
//!
//! # This reads a path nobody has vouched for, and that is worth saying out loud
//!
//! Every other reader in this daemon is rooted: `inspect` under a project's root, `files` under the
//! files root. This one takes an absolute path off a request, because pointing at a folder is what
//! adding a project IS, and there is no root it could be under yet.
//!
//! Two things bound it. The route is in no scope table, so `auth::permits` — default-deny — leaves
//! it to Admin and Control, which is the key of the person sitting at the machine. And it reads
//! **four files by name**, never a tree: a `package.json`, a `Makefile`, a Cargo config and the
//! existence of four folders. It cannot be pointed at a directory to have its contents enumerated,
//! which is the capability worth not building.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;

/// The most of any one file this will read before deciding it has seen enough.
///
/// A `package.json` with a scripts block is a few kilobytes. A file above this is a lockfile, a
/// bundle or somebody's mistake, and reading it whole to find a `scripts` key nobody put in it is
/// work with no reader.
const MAX_FILE: u64 = 512 * 1024;

/// How many commands are proposed at most.
///
/// A monorepo's root `package.json` with ninety scripts is real, and a wizard that listed all of
/// them would be a wizard nobody finishes. The overflow is reported rather than dropped silently.
const MAX_SUGGESTIONS: usize = 40;

/// One command this project appears to have, offered for somebody to accept.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suggestion {
    pub name: String,
    pub command: String,
    /// Where it was found — `package.json`, `Makefile`, `.cargo/config.toml`. Shown, because a
    /// name like `check` means different things in each and somebody has to be able to tell.
    pub source: &'static str,
}

/// A way of working this project already has written down.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Harness {
    /// Relative to the root, forward slashes.
    pub path: String,
    /// What it is, in a sentence, for the person deciding whether to adopt it.
    pub what: &'static str,
    /// How many files are in it, bounded. A folder with two files in it and one with two hundred
    /// are different propositions, and the count is the cheapest way to say which this is.
    pub files: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Detected {
    /// The folder, as the filesystem resolved it. Sent back so what gets registered is what was
    /// looked at, rather than the string somebody typed.
    pub root: String,
    pub is_git: bool,
    /// Where `origin` points, when there is one. A project with no remote is ordinary.
    pub remote: Option<String>,
    pub branch: Option<String>,
    /// The last commit, as `<short sha> <date> <subject>`. One line, because the point is only to
    /// show that this folder has a history and roughly how recent it is.
    pub head: Option<String>,
    pub harnesses: Vec<Harness>,
    pub commands: Vec<Suggestion>,
    /// How many commands were found beyond the ceiling. Never silent: a truncated list that did not
    /// say so would read as the whole of what is there.
    pub commands_omitted: usize,
    /// A project this daemon already has at this root, if any.
    ///
    /// The one thing here that is not about the folder. Adding a project twice under two names is
    /// the mistake this prevents, and it is invisible from the folder alone.
    pub taken_by: Option<String>,
}

/// The folders this app recognises as a way of working, and what each one is.
///
/// Named individually rather than matched by a pattern, because each needs its own sentence and
/// because a pattern would sweep in `.vscode` and `.idea`, which are editor settings and not a way
/// of developing anything.
const HARNESSES: &[(&str, &str)] = &[
    (
        ".ai",
        "a written-down pipeline: which agent does what, in what order, and what has to pass",
    ),
    (".claude", "skills, commands and settings for Claude Code"),
    (".agents", "agent definitions this project keeps with it"),
    (
        ".githooks",
        "git hooks this repository installs rather than inherits",
    ),
];

/// Everything that can be learnt about a folder without being told anything about it.
///
/// `is_git` false is not an error. A folder that is not a repository can still be a project the
/// daemon watches — `set_project_mode` only insists on a repository for `active` — so this reports
/// what is there and lets the step that registers it decide.
pub fn inspect_folder(root: &Path) -> Detected {
    let mut harnesses = Vec::new();
    for (path, what) in HARNESSES {
        let at = root.join(path);
        if at.is_dir() {
            harnesses.push(Harness {
                path: (*path).to_string(),
                what,
                files: count_files(&at, 0),
            });
        }
    }

    let (commands, commands_omitted) = suggestions(root);

    Detected {
        root: root.to_string_lossy().into_owned(),
        is_git: root.join(".git").exists(),
        remote: None,
        branch: None,
        head: None,
        harnesses,
        commands,
        commands_omitted,
        taken_by: None,
    }
}

/// Files under a folder, stopping at a depth and a count.
///
/// Bounded because this runs against a path nobody has vouched for. `.claude/` with a plugin cache
/// in it is tens of thousands of files, and a count nobody reads is not worth a walk that stalls a
/// request.
fn count_files(dir: &Path, depth: usize) -> usize {
    const MAX_DEPTH: usize = 4;
    const CEILING: usize = 500;
    if depth > MAX_DEPTH {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut found = 0;
    for entry in entries.flatten() {
        if found >= CEILING {
            break;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => found += count_files(&entry.path(), depth + 1),
            Ok(kind) if kind.is_file() => found += 1,
            _ => {}
        }
    }
    found.min(CEILING)
}

/// Read one file, or nothing, without reading something enormous by accident.
fn read_bounded(path: &Path) -> Option<String> {
    let size = std::fs::metadata(path).ok()?.len();
    if size > MAX_FILE {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Every command this project appears to have, from the three places projects declare them.
///
/// **Only the root of each**, never a walk. A monorepo has a `package.json` per workspace, and a
/// wizard that proposed four hundred scripts would be one nobody reads to the end — while the root
/// one is where `test` and `lint` actually live.
fn suggestions(root: &Path) -> (Vec<Suggestion>, usize) {
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();

    if let Some(text) = read_bounded(&root.join("package.json")) {
        for name in npm_scripts(&text) {
            push(
                &mut found,
                &mut seen,
                Suggestion {
                    command: format!("npm run {name}"),
                    name,
                    source: "package.json",
                },
            );
        }
    }

    // Cargo aliases live in the repository's own `.cargo/config.toml`, which is the one that
    // travels with it. The one in `$CARGO_HOME` is the person's and belongs to no project.
    if let Some(text) = read_bounded(&root.join(".cargo").join("config.toml")) {
        for name in cargo_aliases(&text) {
            push(
                &mut found,
                &mut seen,
                Suggestion {
                    command: format!("cargo {name}"),
                    name,
                    source: ".cargo/config.toml",
                },
            );
        }
    }

    if let Some(text) = read_bounded(&root.join("Makefile")) {
        for name in make_targets(&text) {
            push(
                &mut found,
                &mut seen,
                Suggestion {
                    command: format!("make {name}"),
                    name,
                    source: "Makefile",
                },
            );
        }
    }

    let omitted = found.len().saturating_sub(MAX_SUGGESTIONS);
    found.truncate(MAX_SUGGESTIONS);
    (found, omitted)
}

/// Add a suggestion unless something already offers that name.
///
/// A repository with a `test` npm script and a `test` Makefile target has one thing called `test`
/// as far as anybody typing it is concerned, and two rows with one name would collide the moment
/// they were declared — `project_commands` keys on the name.
fn push(found: &mut Vec<Suggestion>, seen: &mut BTreeSet<String>, suggestion: Suggestion) {
    if seen.insert(suggestion.name.clone()) {
        found.push(suggestion);
    }
}

/// The keys of `scripts` in a `package.json`.
///
/// Through `serde_json` rather than a regex, because a `package.json` is JSON and a regex over it
/// would find `"scripts"` inside a description string. Anything that is not an object of strings is
/// no scripts at all rather than an error: this is somebody else's file and it can be anything.
fn npm_scripts(text: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(scripts) = value.get("scripts").and_then(|s| s.as_object()) else {
        return Vec::new();
    };
    scripts
        .iter()
        .filter(|(_, command)| command.is_string())
        .map(|(name, _)| name.clone())
        .filter(|name| usable(name))
        .collect()
}

/// The keys of `[alias]` in a Cargo config.
///
/// Read by hand rather than with a TOML parser, and that is a deliberately small dependency
/// decision: this crate has no TOML parser, one section of one file is a handful of lines, and
/// adding a parser to read a suggestion list would be a tree pulled in for a wizard. A file this
/// misreads produces a wrong suggestion somebody does not tick, which is the cheapest possible
/// failure — nothing here is stored without being confirmed.
fn cargo_aliases(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == "[alias]";
            continue;
        }
        if !inside || line.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = line.split_once('=') {
            let name = name.trim().trim_matches('"');
            if usable(name) {
                found.push(name.to_string());
            }
        }
    }
    found
}

/// The targets of a Makefile.
///
/// Rules only: a line starting at column zero with a name, a colon, and no `=` before it — which is
/// what tells `build: deps` from `CFLAGS := -O2`. Pattern rules and `.PHONY` are skipped by
/// [`usable`], which refuses anything with a `%` or a leading dot.
fn make_targets(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t', '#']) {
            continue;
        }
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        // `A := b` and `A ::= b` are assignments whose colon comes before the equals.
        if rest.starts_with('=') || name.contains('=') {
            continue;
        }
        let name = name.trim();
        if usable(name) {
            found.push(name.to_string());
        }
    }
    found
}

/// Whether a name can be a command's name at all.
///
/// The same bounds `project_commands::validate` enforces, checked here so that nothing is proposed
/// which would be refused the moment somebody accepted it — a suggestion that cannot be taken is
/// worse than no suggestion.
fn usable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= crate::project_commands::MAX_NAME
        && !name.starts_with('.')
        && !name.contains('%')
        && !name.contains(' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn the_scripts_a_project_already_declares_are_proposed_with_where_they_came_from() {
        let temp = folder();
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"name": "x", "scripts": {"test": "vitest run", "build": "tsc"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(temp.path().join(".cargo")).unwrap();
        std::fs::write(
            temp.path().join(".cargo/config.toml"),
            "[build]\ntarget = \"x\"\n\n[alias]\nxtask = \"run --package xtask --\"\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join("Makefile"),
            "CFLAGS := -O2\n\nland:\n\tgit push\n\n%.o: %.c\n\tcc\n",
        )
        .unwrap();

        let found = inspect_folder(temp.path());
        let names: Vec<_> = found.commands.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"test") && names.contains(&"build"));
        assert!(names.contains(&"xtask"));
        assert!(names.contains(&"land"));
        // An assignment is not a target, and a pattern rule is not a command anybody types.
        assert!(!names.contains(&"CFLAGS"));
        assert!(!names.iter().any(|name| name.contains('%')));

        let xtask = found.commands.iter().find(|s| s.name == "xtask").unwrap();
        assert_eq!(xtask.command, "cargo xtask");
        assert_eq!(xtask.source, ".cargo/config.toml");
    }

    /// Two files offering one name is one command as far as anybody typing it is concerned, and two
    /// rows would collide the moment they were declared — `project_commands` keys on the name.
    #[test]
    fn one_name_is_proposed_once_however_many_files_offer_it() {
        let temp = folder();
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"scripts": {"test": "vitest"}}"#,
        )
        .unwrap();
        std::fs::write(temp.path().join("Makefile"), "test:\n\tmake it\n").unwrap();

        let found = inspect_folder(temp.path());
        assert_eq!(
            found.commands.iter().filter(|s| s.name == "test").count(),
            1
        );
        // The first source wins, which is the order they are read in and the one the page shows.
        assert_eq!(
            found
                .commands
                .iter()
                .find(|s| s.name == "test")
                .unwrap()
                .source,
            "package.json"
        );
    }

    /// Somebody else's file can be anything. A `package.json` that is not JSON, or whose `scripts`
    /// is a string, is no scripts rather than a failed detection.
    #[test]
    fn a_file_that_is_not_what_it_claims_proposes_nothing_rather_than_failing() {
        let temp = folder();
        std::fs::write(temp.path().join("package.json"), "not json at all").unwrap();
        assert!(inspect_folder(temp.path()).commands.is_empty());

        std::fs::write(
            temp.path().join("package.json"),
            r#"{"scripts": "all of them"}"#,
        )
        .unwrap();
        assert!(inspect_folder(temp.path()).commands.is_empty());

        // And a script whose value is not a string is not a command either.
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"scripts": {"a": {"b": 1}, "ok": "run"}}"#,
        )
        .unwrap();
        let found = inspect_folder(temp.path());
        assert_eq!(found.commands.len(), 1);
        assert_eq!(found.commands[0].name, "ok");
    }

    /// **The one that matters most.** This repository's own way of working is a folder, and the app
    /// has to recognise it rather than ask for it to be described again.
    #[test]
    fn the_written_down_way_of_working_a_project_already_has_is_recognised() {
        let temp = folder();
        std::fs::create_dir_all(temp.path().join(".ai/scripts")).unwrap();
        std::fs::write(temp.path().join(".ai/workflow.md"), "the pipeline").unwrap();
        std::fs::write(temp.path().join(".ai/scripts/gate.py"), "print()").unwrap();
        std::fs::create_dir_all(temp.path().join(".vscode")).unwrap();

        let found = inspect_folder(temp.path());
        assert_eq!(found.harnesses.len(), 1);
        assert_eq!(found.harnesses[0].path, ".ai");
        assert_eq!(found.harnesses[0].files, 2);
        assert!(found.harnesses[0].what.contains("pipeline"));
    }

    /// A folder that is not a repository is reported as one, not refused. `set_project_mode` only
    /// insists on a repository for `active`, so this reports and the step that registers decides.
    #[test]
    fn a_folder_with_no_repository_in_it_is_a_finding_and_not_a_failure() {
        let temp = folder();
        let found = inspect_folder(temp.path());
        assert!(!found.is_git);
        assert!(found.harnesses.is_empty());
        assert!(found.commands.is_empty());
        assert_eq!(found.commands_omitted, 0);
    }

    /// A truncated list that did not say so would read as the whole of what is there.
    #[test]
    fn a_list_cut_short_says_how_much_was_left_out() {
        let temp = folder();
        let scripts: Vec<String> = (0..MAX_SUGGESTIONS + 5)
            .map(|n| format!("\"s{n}\": \"echo {n}\""))
            .collect();
        std::fs::write(
            temp.path().join("package.json"),
            format!("{{\"scripts\": {{{}}}}}", scripts.join(",")),
        )
        .unwrap();

        let found = inspect_folder(temp.path());
        assert_eq!(found.commands.len(), MAX_SUGGESTIONS);
        assert_eq!(found.commands_omitted, 5);
    }

    /// A name the command registry would refuse is not proposed, because a suggestion that cannot
    /// be accepted is worse than no suggestion.
    #[test]
    fn nothing_is_proposed_that_could_not_then_be_declared() {
        let temp = folder();
        let long = "a".repeat(crate::project_commands::MAX_NAME + 1);
        std::fs::write(
            temp.path().join("package.json"),
            format!(
                "{{\"scripts\": {{\"{long}\": \"x\", \"has space\": \"y\", \"fine\": \"z\"}}}}"
            ),
        )
        .unwrap();

        let found = inspect_folder(temp.path());
        assert_eq!(found.commands.len(), 1);
        assert_eq!(found.commands[0].name, "fine");
    }
}
