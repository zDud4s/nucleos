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
//! **files by name**, never a tree: a `package.json`, a `Makefile`, a Cargo config, the
//! project's own `.ai/project.yaml` when it has one, and the existence of four folders. It cannot be
//! pointed at a directory to have its contents enumerated, which is the capability worth not
//! building.
//!
//! The one exception is [`propose_tests_map`], which does enumerate: it walks up to two levels
//! below the root looking for `go.mod` and `package.json`, and the directory names it finds come
//! back in group names and `sources`. That is safe only because it starts from the root of a
//! project already registered, and so is called by the `/projects/{id}/tests-map` route alone. It
//! is never reached from `inspect_folder` or `get_project_detect`, which take a path nobody has
//! vouched for: the paragraph above stays true of that path.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;

use crate::tests_map::{Group, MAP_FILE, Seed, Tests, TestsMap, Tool, VERSION, Warm};

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

/// The command this app proposes as the project's gate — the one whose exit code says *green* — and
/// where the proposal came from.
///
/// A proposal and never a setting: onboarding (`crate::onboarding`) shows it in an editable field
/// and stores only what a person confirmed there. Guessing the quality bar and writing it down
/// unasked would put a claim in the project nobody made.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GateProposal {
    pub command: String,
    /// Shown beside the field, for the reason [`Suggestion::source`] is: somebody deciding whether
    /// this is their bar has to be able to tell which file said so.
    pub source: String,
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
    /// What the gate could be, when anything here suggests one. See [`propose_gate`].
    pub gate: Option<GateProposal>,
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
    let gate = propose_gate(root, &commands);

    Detected {
        root: root.to_string_lossy().into_owned(),
        is_git: root.join(".git").exists(),
        remote: None,
        branch: None,
        head: None,
        harnesses,
        commands,
        commands_omitted,
        gate,
        taken_by: None,
    }
}

/// The command a project's gate could be, from what the folder already says — or nothing.
///
/// In order, and the order is who is best placed to know:
///
/// 1. **The project's own declared full suite**, `commands.test_full` in `.ai/project.yaml`, when
///    the project keeps that file. It is the project writing down, in its own words, what *green*
///    means, and a proposal cannot do better than repeat it. **Only a hint**: a project without the
///    file, or with a `test_full` this cannot read as ONE command, simply falls through — nothing in
///    NucleOS requires that file or the workflow it belongs to. One command and not a list, because
///    a gate is spawned directly and never through a shell, so two entries could not be chained.
/// 2. **A command called `test`**, then **one called `check`**, among the ones [`suggestions`]
///    found. The conventional names for "run everything that has to pass".
fn propose_gate(root: &Path, commands: &[Suggestion]) -> Option<GateProposal> {
    if let Some(command) = read_bounded(&root.join(".ai").join("project.yaml"))
        .as_deref()
        .and_then(declared_full_suite)
    {
        return Some(GateProposal {
            command,
            source: ".ai/project.yaml (commands.test_full)".to_string(),
        });
    }
    ["test", "check"].iter().find_map(|name| {
        commands
            .iter()
            .find(|suggestion| suggestion.name == *name)
            .map(|suggestion| GateProposal {
                command: suggestion.command.clone(),
                source: suggestion.source.to_string(),
            })
    })
}

/// `commands.test_full` out of a `project.yaml`, when it is one command: a string, or a list of
/// exactly one. Anything else — no key, a list of several, a file that is not YAML — is no hint.
fn declared_full_suite(text: &str) -> Option<String> {
    let value: serde_yaml::Value = serde_yaml::from_str(text).ok()?;
    let declared = value.get("commands")?.get("test_full")?;
    let command = match declared {
        serde_yaml::Value::String(command) => command.as_str(),
        serde_yaml::Value::Sequence(entries) if entries.len() == 1 => entries[0].as_str()?,
        _ => return None,
    };
    let command = command.trim();
    (!command.is_empty() && !command.contains(['\n', '\r'])).then(|| command.to_string())
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

/// A test map this folder's build files suggest, as the YAML somebody would commit, and the files
/// it was read from. Always a proposal: it is shown with a copy button and never written (spec
/// §3.3) — the map decides what verifies a change, so it is the owner's claim to make.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TestsMapProposal {
    pub yaml: String,
    pub sources: Vec<String>,
}

/// How many directories the module search may open. Two levels of a monorepo is dozens, not
/// hundreds; the ceiling is for the folder that turns out to be somebody's home directory.
const MAX_MODULE_DIRS: usize = 200;

/// Folders a module search never enters: build output, dependencies, and anything hidden.
fn skipped_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "node_modules" | "target" | "vendor" | "dist" | "build"
        )
}

/// Directories (relative, forward slashes, `""` for the root) holding `file`, at the root and up
/// to two levels below it, in name order.
fn dirs_with(root: &Path, file: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut opened = 0;
    let mut frontier = vec![String::new()];
    for _depth in 0..=2 {
        let mut next = Vec::new();
        for dir in frontier {
            let at = if dir.is_empty() {
                root.to_path_buf()
            } else {
                root.join(&dir)
            };
            if at.join(file).is_file() {
                found.push(dir.clone());
            }
            opened += 1;
            if opened > MAX_MODULE_DIRS {
                return found;
            }
            let Ok(entries) = std::fs::read_dir(&at) else {
                continue;
            };
            let mut children: Vec<String> = entries
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
                .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
                .filter(|name| !skipped_dir(name))
                .map(|name| {
                    if dir.is_empty() {
                        name
                    } else {
                        format!("{dir}/{name}")
                    }
                })
                .collect();
            children.sort();
            next.extend(children);
        }
        frontier = next;
    }
    found
}

/// The proposal for `root`. A folder with no build file anybody recognises gets a map with no
/// groups — which is a real answer: every change is then unclaimed, and runs the gate, as today.
pub fn propose_tests_map(root: &Path) -> TestsMapProposal {
    let mut tests = Tests::default();
    let mut sources = Vec::new();
    cargo_groups(root, &mut tests, &mut sources);
    go_groups(root, &mut tests, &mut sources);
    npm_groups(root, &mut tests, &mut sources);
    python_groups(root, &mut tests, &mut sources);
    for name in ["README.md", "LICENSE", "CHANGELOG.md", "docs", ".gitignore"] {
        if root.join(name).exists() {
            let pattern = if root.join(name).is_dir() {
                format!("{name}/")
            } else {
                name.to_string()
            };
            tests.no_test.push(pattern);
        }
    }
    let map = TestsMap {
        version: VERSION,
        tests,
    };
    let body = serde_yaml::to_string(&map).unwrap_or_default();
    let header = if sources.is_empty() {
        format!(
            "# Proposed by NucleOS: no build file recognised. Review it, then commit it as {MAP_FILE}.\n"
        )
    } else {
        format!(
            "# Proposed by NucleOS from {}. Review it, then commit it as {MAP_FILE}.\n",
            sources.join(", ")
        )
    };
    TestsMapProposal {
        yaml: header + &body,
        sources,
    }
}

fn add_group(tests: &mut Tests, name: String, group: Group) {
    // A directory or crate name may carry characters a group name may not (`pkgs/foo.bar`); a
    // proposal the parser then refused would be no proposal at all.
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // A name two ecosystems both produce keeps the first; the owner renames when accepting.
    tests.groups.entry(name).or_insert(group);
}

fn group(paths: Vec<String>, command: String) -> Group {
    Group {
        paths,
        check: None,
        command,
        select: None,
        reads: None,
        cache: true,
        env: Vec::new(),
        include_ignored: false,
    }
}

fn tool(allow: &[&str]) -> Tool {
    Tool {
        allow: allow.iter().map(|s| s.to_string()).collect(),
    }
}

/// Cargo: a group per workspace member (or one for a single package), `cargo test -p <name>`.
/// The manifest is read line by line, as `cargo_aliases` does, because this crate carries no TOML
/// parser and a proposal does not justify one: a manifest this cannot read proposes nothing.
fn cargo_groups(root: &Path, tests: &mut Tests, sources: &mut Vec<String>) {
    let Some(text) = read_bounded(&root.join("Cargo.toml")) else {
        return;
    };
    let mut members = toml_string_array(&text, "workspace", "members");
    if members.is_empty() && toml_string(&text, "package", "name").is_some() {
        members.push(".".to_string());
    }
    let mut expanded = Vec::new();
    for member in members {
        match member.strip_suffix("/*") {
            Some(parent) => {
                for dir in dirs_with(&root.join(parent), "Cargo.toml") {
                    if !dir.is_empty() && !dir.contains('/') {
                        expanded.push(format!("{parent}/{dir}"));
                    }
                }
            }
            None if !member.contains('*') => expanded.push(member),
            None => {}
        }
    }
    let shared: Vec<String> = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]
        .iter()
        .filter(|name| root.join(name).is_file())
        .map(|name| name.to_string())
        .collect();
    let clippy = root.join("clippy.toml").is_file() || root.join(".clippy.toml").is_file();
    let member_dirs: Vec<String> = expanded
        .iter()
        .filter(|member| member.as_str() != ".")
        .map(|member| format!("{member}/"))
        .collect();
    let mut any = false;
    for member in expanded {
        let manifest = if member == "." {
            root.join("Cargo.toml")
        } else {
            root.join(&member).join("Cargo.toml")
        };
        let Some(name) =
            read_bounded(&manifest).and_then(|text| toml_string(&text, "package", "name"))
        else {
            continue;
        };
        // Every member's directory, not only this one's: a member that depends on another by path
        // is broken by a change there, and the map is a conservative superset (spec D6). Narrowing
        // this to the real dependency graph is F4's `{crates}`.
        let mut paths: Vec<String> = if member == "." {
            ["src/", "tests/", "benches/", "examples/", "build.rs"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            member_dirs.clone()
        };
        paths.extend(shared.iter().cloned());
        let mut cargo = group(paths, format!("cargo test -p {name}"));
        cargo.check = Some(if clippy {
            format!("cargo clippy -p {name} --all-targets -- -D warnings")
        } else {
            format!("cargo check -p {name} --all-targets")
        });
        add_group(tests, name, cargo);
        any = true;
    }
    if any {
        sources.push("Cargo.toml".to_string());
        tests.tools.insert(
            "cargo".into(),
            tool(&[
                "fmt", "metadata", "tree", "add", "remove", "update", "search",
            ]),
        );
        tests.tools.insert("rustc".into(), Tool::default());
        tests.warm.insert(
            "CARGO_TARGET_DIR".into(),
            Warm {
                dir: "target".into(),
                seed: Seed::None,
            },
        );
    }
}

/// Go: a group per module. The root module gets `select: go test {dirs}`; a nested one runs
/// through `go -C <dir>`, where `{dirs}` (relative to the repository root) would point at the
/// wrong place, so it has no `select` and runs `./...` whole.
fn go_groups(root: &Path, tests: &mut Tests, sources: &mut Vec<String>) {
    let modules = dirs_with(root, "go.mod");
    // With nested modules the root group's `**/*.go` also claims their files, and `go test
    // ./nested/dir` from the root fails (outside the main module) — so the root runs whole.
    let nested = modules.iter().any(|dir| !dir.is_empty());
    for dir in &modules {
        let (name, module) = if dir.is_empty() {
            let mut module = group(
                vec!["**/*.go".into(), "go.mod".into(), "go.sum".into()],
                "go test ./...".into(),
            );
            if !nested {
                module.select = Some("go test {dirs}".into());
            }
            module.check = Some("go vet ./...".into());
            ("go".to_string(), module)
        } else {
            let mut module = group(vec![format!("{dir}/")], format!("go -C {dir} test ./..."));
            module.check = Some(format!("go -C {dir} vet ./..."));
            (format!("go-{}", dir.replace('/', "-")), module)
        };
        add_group(tests, name, module);
        sources.push(if dir.is_empty() {
            "go.mod".into()
        } else {
            format!("{dir}/go.mod")
        });
    }
    if !modules.is_empty() {
        tests
            .tools
            .insert("go".into(), tool(&["mod", "list", "env", "version", "fmt"]));
        tests.warm.insert(
            "GOCACHE".into(),
            Warm {
                dir: "shared:gocache".into(),
                seed: Seed::None,
            },
        );
    }
}

/// npm with vitest or jest: a group per package that has one of them. The root package gets a
/// `select` (`vitest related`, `jest --findRelatedTests`); a nested one runs `npm --prefix <dir>
/// test`, for the reason nested Go modules get no `select`.
fn npm_groups(root: &Path, tests: &mut Tests, sources: &mut Vec<String>) {
    let mut any = false;
    let packages = dirs_with(root, "package.json");
    // As for Go: a root `select` would be handed files that belong to a nested package.
    let nested = packages.iter().any(|dir| !dir.is_empty());
    for dir in packages {
        let at = if dir.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&dir)
        };
        let Some(text) = read_bounded(&at.join("package.json")) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let has = |dependency: &str| {
            ["dependencies", "devDependencies"].iter().any(|key| {
                json.get(key)
                    .and_then(|deps| deps.get(dependency))
                    .is_some()
            })
        };
        let runner = if has("vitest") {
            "vitest"
        } else if has("jest") {
            "jest"
        } else {
            continue;
        };
        let (name, package) = if dir.is_empty() {
            let mut package = group(
                vec![
                    "**/*.ts".into(),
                    "**/*.tsx".into(),
                    "**/*.js".into(),
                    "**/*.jsx".into(),
                    "package.json".into(),
                    "package-lock.json".into(),
                ],
                "npm test".into(),
            );
            if !nested {
                package.select = Some(if runner == "vitest" {
                    "npx vitest related --run {files}".into()
                } else {
                    "npx jest --findRelatedTests {files}".into()
                });
            }
            if at.join("tsconfig.json").is_file() {
                package.check = Some("npx tsc -b".into());
            }
            ("npm".to_string(), package)
        } else {
            (
                format!("npm-{}", dir.replace('/', "-")),
                group(vec![format!("{dir}/")], format!("npm --prefix {dir} test")),
            )
        };
        add_group(tests, name, package);
        sources.push(if dir.is_empty() {
            "package.json".into()
        } else {
            format!("{dir}/package.json")
        });
        tests.tools.insert(runner.into(), Tool::default());
        any = true;
    }
    if any {
        tests.tools.insert("tsc".into(), Tool::default());
        tests.tools.insert(
            "npm".into(),
            tool(&["install", "ci", "ls", "view", "outdated"]),
        );
        tests.tools.insert("npx".into(), Tool::default());
    }
}

/// pytest, from the root only: `pytest.ini`, a `conftest.py`, or a `[tool.pytest` table in
/// `pyproject.toml`.
fn python_groups(root: &Path, tests: &mut Tests, sources: &mut Vec<String>) {
    let pyproject = read_bounded(&root.join("pyproject.toml"));
    let source = if root.join("pytest.ini").is_file() {
        "pytest.ini"
    } else if pyproject
        .as_deref()
        .is_some_and(|text| text.contains("[tool.pytest"))
    {
        "pyproject.toml"
    } else if root.join("conftest.py").is_file() {
        "conftest.py"
    } else {
        return;
    };
    let mut python = group(
        vec!["**/*.py".into(), "pyproject.toml".into()],
        "python -m pytest".into(),
    );
    python.select = Some("python -m pytest {dirs}".into());
    if root.join("ruff.toml").is_file()
        || pyproject
            .as_deref()
            .is_some_and(|text| text.contains("[tool.ruff"))
    {
        python.check = Some("ruff check .".into());
        tests.tools.insert("ruff".into(), Tool::default());
    }
    add_group(tests, "python".into(), python);
    sources.push(source.into());
    tests.tools.insert("pytest".into(), Tool::default());
    if root.join("mypy.ini").is_file()
        || pyproject
            .as_deref()
            .is_some_and(|text| text.contains("[tool.mypy"))
    {
        tests.tools.insert("mypy".into(), Tool::default());
    }
}

/// `key = "value"` inside `[table]`, line by line. Enough for `[package] name`.
fn toml_string(text: &str, table: &str, key: &str) -> Option<String> {
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == format!("[{table}]");
            continue;
        }
        if inside
            && let Some((name, value)) = line.split_once('=')
            && name.trim() == key
        {
            return Some(value.trim().trim_matches('"').to_string()).filter(|v| !v.is_empty());
        }
    }
    None
}

/// `key = ["a", "b"]` inside `[table]`, on one line or spread over several.
fn toml_string_array(text: &str, table: &str, key: &str) -> Vec<String> {
    let mut inside = false;
    let mut collecting: Option<String> = None;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(buffer) = collecting.as_mut() {
            buffer.push_str(line);
            if line.contains(']') {
                return quoted(buffer);
            }
            continue;
        }
        if line.starts_with('[') {
            inside = line == format!("[{table}]");
            continue;
        }
        if inside
            && let Some((name, value)) = line.split_once('=')
            && name.trim() == key
        {
            if value.contains(']') {
                return quoted(value);
            }
            collecting = Some(value.to_string());
        }
    }
    Vec::new()
}

fn quoted(text: &str) -> Vec<String> {
    text.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
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

    /// The project's own declared full suite wins over a guess from a script's name, and is only
    /// ever a hint: a list of several, or no file, falls through to `test`, then `check`.
    #[test]
    fn the_gate_is_proposed_from_the_projects_own_word_first_and_a_conventional_name_after() {
        let temp = folder();
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"scripts": {"check": "tsc", "test": "vitest run"}}"#,
        )
        .unwrap();
        assert_eq!(
            inspect_folder(temp.path()).gate,
            Some(GateProposal {
                command: "npm run test".to_string(),
                source: "package.json".to_string(),
            })
        );

        std::fs::create_dir_all(temp.path().join(".ai")).unwrap();
        std::fs::write(
            temp.path().join(".ai/project.yaml"),
            "commands:\n  test_full:\n    - bash scripts/gates.sh all\n",
        )
        .unwrap();
        assert_eq!(
            inspect_folder(temp.path()).gate,
            Some(GateProposal {
                command: "bash scripts/gates.sh all".to_string(),
                source: ".ai/project.yaml (commands.test_full)".to_string(),
            })
        );

        // Two commands cannot be one gate, so the declaration is no hint and the name wins again.
        std::fs::write(
            temp.path().join(".ai/project.yaml"),
            "commands:\n  test_full:\n    - a\n    - b\n",
        )
        .unwrap();
        assert_eq!(
            inspect_folder(temp.path()).gate.unwrap().command,
            "npm run test"
        );

        assert_eq!(inspect_folder(folder().path()).gate, None);
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

    fn proposed(temp: &tempfile::TempDir) -> crate::tests_map::TestsMap {
        let proposal = propose_tests_map(temp.path());
        // Whatever is proposed must be a map this daemon accepts as written.
        crate::tests_map::parse(&proposal.yaml)
            .unwrap_or_else(|e| panic!("{e:?}\n{}", proposal.yaml))
    }

    #[test]
    fn a_cargo_workspace_gets_a_group_per_member() {
        let temp = folder();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\n  \"core\", # the daemon\n  \"crates/*\",\n]\n",
        )
        .unwrap();
        for (dir, name) in [("core", "nucleos-core"), ("crates/a", "a")] {
            std::fs::create_dir_all(temp.path().join(dir)).unwrap();
            std::fs::write(
                temp.path().join(dir).join("Cargo.toml"),
                format!("[package]\nname = \"{name}\"\n"),
            )
            .unwrap();
        }
        std::fs::write(temp.path().join("Cargo.lock"), "").unwrap();
        let map = proposed(&temp);
        let core = &map.tests.groups["nucleos-core"];
        assert_eq!(core.command, "cargo test -p nucleos-core");
        assert!(
            core.paths.contains(&"core/".to_string())
                && core.paths.contains(&"Cargo.lock".to_string())
        );
        // A change in another member runs this one's tests too: path dependencies are invisible here.
        assert!(core.paths.contains(&"crates/a/".to_string()));
        assert!(map.tests.groups.contains_key("a"));
        assert!(map.tests.tools["cargo"].allow.contains(&"fmt".to_string()));
        assert_eq!(map.tests.warm["CARGO_TARGET_DIR"].dir, "target");
    }

    #[test]
    fn nested_go_modules_run_through_dash_c_and_the_root_one_runs_whole() {
        let temp = folder();
        std::fs::write(temp.path().join("go.mod"), "module x\n").unwrap();
        std::fs::create_dir_all(temp.path().join("sidecars/echo")).unwrap();
        std::fs::write(temp.path().join("sidecars/echo/go.mod"), "module echo\n").unwrap();
        let map = proposed(&temp);
        assert!(
            map.tests.groups["go"].select.is_none(),
            "nested modules: the root has no select"
        );
        let echo = &map.tests.groups["go-sidecars-echo"];
        assert_eq!(echo.command, "go -C sidecars/echo test ./...");
        assert!(echo.select.is_none());
    }

    #[test]
    fn a_lone_go_module_gets_a_select() {
        let temp = folder();
        std::fs::write(temp.path().join("go.mod"), "module x\n").unwrap();
        assert_eq!(
            proposed(&temp).tests.groups["go"].select.as_deref(),
            Some("go test {dirs}")
        );
    }

    #[test]
    fn a_package_with_vitest_gets_vitest_related() {
        let temp = folder();
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"devDependencies": {"vitest": "^4"}}"#,
        )
        .unwrap();
        std::fs::write(temp.path().join("tsconfig.json"), "{}").unwrap();
        let map = proposed(&temp);
        let npm = &map.tests.groups["npm"];
        assert_eq!(
            npm.select.as_deref(),
            Some("npx vitest related --run {files}")
        );
        assert_eq!(npm.check.as_deref(), Some("npx tsc -b"));
        assert!(map.tests.tools.contains_key("vitest"));
    }

    #[test]
    fn a_package_without_a_test_runner_proposes_no_group() {
        let temp = folder();
        std::fs::write(
            temp.path().join("package.json"),
            r#"{"scripts": {"build": "tsc"}}"#,
        )
        .unwrap();
        assert!(proposed(&temp).tests.groups.is_empty());
    }

    #[test]
    fn pytest_is_recognised_from_pyproject() {
        let temp = folder();
        std::fs::write(
            temp.path().join("pyproject.toml"),
            "[tool.pytest.ini_options]\n",
        )
        .unwrap();
        let map = proposed(&temp);
        assert_eq!(
            map.tests.groups["python"].select.as_deref(),
            Some("python -m pytest {dirs}")
        );
    }

    #[test]
    fn a_folder_with_nothing_recognisable_proposes_an_empty_map_that_says_so() {
        let temp = folder();
        std::fs::write(temp.path().join("README.md"), "x").unwrap();
        let proposal = propose_tests_map(temp.path());
        assert!(proposal.sources.is_empty());
        assert!(
            proposal
                .yaml
                .starts_with("# Proposed by NucleOS: no build file recognised")
        );
        let map = proposed(&temp);
        assert!(map.tests.groups.is_empty());
        assert_eq!(map.tests.no_test, ["README.md"]);
    }

    #[test]
    fn a_directory_name_becomes_a_valid_group_name() {
        let temp = folder();
        std::fs::create_dir_all(temp.path().join("pkgs/foo.bar")).unwrap();
        std::fs::write(temp.path().join("pkgs/foo.bar/go.mod"), "module m\n").unwrap();
        assert!(proposed(&temp).tests.groups.contains_key("go-pkgs-foo-bar"));
    }

    #[test]
    fn the_module_search_skips_dependencies_and_hidden_folders() {
        let temp = folder();
        for dir in ["node_modules/x", ".git/y", "target/z"] {
            std::fs::create_dir_all(temp.path().join(dir)).unwrap();
            std::fs::write(temp.path().join(dir).join("go.mod"), "module m\n").unwrap();
        }
        assert!(dirs_with(temp.path(), "go.mod").is_empty());
    }
}
