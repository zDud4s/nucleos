//! Whether a shell line is a verification use under a project's test map.
//!
//! F2b step 1 (spec 2026-10-05 §4.5/§4.6): a line counts as verification when it runs a
//! `tools:` program minus that tool's `allow` subcommands, a `gate_entrypoints:` script, or the
//! project's gate command / a group's `command` by exact argv, after the §4.6 normalization
//! (env prefixes, `time`/`nice`/`xargs`, shell wrappers, the broker, runners, project wrappers).
//!
//! Observe-only and pure: no disk, no database, no process. It decides nothing — the caller
//! records what it finds.

use crate::classifier::{program_name, shell_words};
use crate::command_reader::{self, Shell};
use crate::tests_map::{self, TestsMap};

/// How many layers of wrapping are peeled before the line is given up on. A real line is one or
/// two deep (`time bash -c "cargo test"`); the bound only stops a pathological one.
const MAX_DEPTH: usize = 4;

/// Flags that take their value as the NEXT word, so the value is not mistaken for a subcommand:
/// `cargo --manifest-path m/Cargo.toml test`, `go -C dir test`, `npm --prefix shell run build`.
const VALUE_FLAGS: &[&str] = &[
    "-C",
    "--manifest-path",
    "--config",
    "--color",
    "-Z",
    "--prefix",
    "--cwd",
];

/// The broker's own options that take a value (`--agent a`, or `--agent=a`).
const BROKER_FLAGS: &[&str] = &["--prio", "--agent", "--kind", "--wait-max"];

/// What kind of verification use a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The project's `gate_command`, by exact argv.
    GateCommand,
    /// A group's `command`, by exact argv.
    GroupCommand,
    /// A script listed under `gate_entrypoints:`.
    Entrypoint,
    /// A program listed under `tools:`, run with a subcommand its `allow` does not cover.
    Tool,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::GateCommand => "gate_command",
            Kind::GroupCommand => "group_command",
            Kind::Entrypoint => "entrypoint",
            Kind::Tool => "tool",
        }
    }
}

/// A verification use found in a line.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub kind: Kind,
    /// The tool key, the entrypoint pattern, the group name, or `gate_command`.
    pub name: String,
    /// The normalized argv that matched, joined by spaces — what remains after the wrappers and
    /// prefixes were peeled off.
    pub segment: String,
}

/// What the map and the gate command say, read once per call.
struct Ctx<'a> {
    map: &'a TestsMap,
    gate: Option<Vec<String>>,
    groups: Vec<(&'a str, Vec<String>)>,
    /// Lowercased program names of the project's `wrappers:`.
    wrappers: Vec<String>,
}

/// One layer taken off an argv.
enum Peel {
    /// The command the layer ran, as words.
    Argv(Vec<String>),
    /// A command line a shell wrapper will re-read, and which shell reads it.
    Line(String, Shell),
    /// Nothing to take off.
    Done,
}

/// PURE: the verification use `command` is under `map`, if any. The first hit across the line's
/// segments wins.
pub fn detect(
    command: &str,
    shell: Shell,
    map: &TestsMap,
    gate_command: Option<&str>,
) -> Option<Hit> {
    let argv = |text: &str| {
        let words = shell_words(text);
        let skip = leading_assignments(&words);
        let words = words[skip..].to_vec();
        (!words.is_empty()).then_some(words)
    };
    let ctx = Ctx {
        map,
        gate: gate_command.and_then(argv),
        groups: map
            .tests
            .groups
            .iter()
            .filter_map(|(name, group)| Some((name.as_str(), argv(&group.command)?)))
            .collect(),
        wrappers: map
            .tests
            .wrappers
            .iter()
            .map(|wrapper| program_name(wrapper).to_ascii_lowercase())
            .collect(),
    };
    scan(command, shell, &ctx, 0)
}

/// PURE: the one map `detect` reads when a run's worktree map and the target's map both exist
/// (spec 2026-10-05 §3.4 defence 5). The agent owns the worktree's file, so it can only add to
/// what the target already guards, never subtract.
///
/// Tools listed in either map stay listed, keys folded to lowercase (the lookup is
/// case-insensitive and takes the first key); a tool both list keeps only the subcommands BOTH
/// allow, and an empty `allow` stays empty (everything is a hit). When both lists were non-empty
/// but disjoint, the intersection keeps a single `""` sentinel: `test_argv` reads an empty `allow`
/// as "every invocation is a hit", and `subcommand_of` never yields an empty subcommand for a real
/// invocation, so the tool stays "non-empty allow, nothing covered" (a flagless or bare call is
/// still not a hit, any real subcommand is). `gate_entrypoints` and
/// `wrappers` are unioned. Groups are unioned by name; the same name with a different `command`
/// keeps both, the worktree's under `worktree:<name>`.
///
/// Only what `detect` reads is merged: every other field is the target's, so this is not a map
/// for any other reader.
pub fn detection_union(target: &TestsMap, worktree: &TestsMap) -> TestsMap {
    let mut union = target.clone();
    let mut tools: std::collections::BTreeMap<String, tests_map::Tool> =
        std::collections::BTreeMap::new();
    for (key, tool) in target.tests.tools.iter().chain(&worktree.tests.tools) {
        match tools.entry(key.to_ascii_lowercase()) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(tool.clone());
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                let allow = &mut slot.get_mut().allow;
                let both_listed = !allow.is_empty() && !tool.allow.is_empty();
                allow.retain(|allowed| tool.allow.contains(allowed));
                if both_listed && allow.is_empty() {
                    allow.push(String::new());
                }
            }
        }
    }
    union.tests.tools = tools;
    for entry in &worktree.tests.gate_entrypoints {
        if !union.tests.gate_entrypoints.contains(entry) {
            union.tests.gate_entrypoints.push(entry.clone());
        }
    }
    for wrapper in &worktree.tests.wrappers {
        if !union.tests.wrappers.contains(wrapper) {
            union.tests.wrappers.push(wrapper.clone());
        }
    }
    for (name, group) in &worktree.tests.groups {
        match union.tests.groups.get(name) {
            None => {
                union.tests.groups.insert(name.clone(), group.clone());
            }
            Some(existing) if existing.command == group.command => {}
            Some(_) => {
                union
                    .tests
                    .groups
                    .insert(format!("worktree:{name}"), group.clone());
            }
        }
    }
    union
}

/// Splits a line and looks at every segment. The reader never refuses; when it hands back nothing
/// for a line that is not blank, the whole line is looked at instead.
fn scan(command: &str, shell: Shell, ctx: &Ctx<'_>, depth: usize) -> Option<Hit> {
    let mut segments = command_reader::segments(command, shell);
    if segments.is_empty() && !command.trim().is_empty() {
        segments.push(command.trim());
    }
    segments
        .into_iter()
        .find_map(|segment| scan_words(shell_words(segment), ctx, depth))
}

/// Tests an argv and, when it is not a hit, peels one layer and tests again, a bounded number of
/// times.
fn scan_words(mut words: Vec<String>, ctx: &Ctx<'_>, mut depth: usize) -> Option<Hit> {
    loop {
        let skip = leading_assignments(&words);
        words.drain(..skip);
        if words.is_empty() {
            return None;
        }
        if let Some(hit) = test_argv(&words, ctx) {
            return Some(hit);
        }
        if depth >= MAX_DEPTH {
            return None;
        }
        depth += 1;
        match peel(&words, ctx) {
            Peel::Argv(next) => words = next,
            Peel::Line(line, shell) => return scan(&line, shell, ctx, depth),
            Peel::Done => return None,
        }
    }
}

/// How many leading words are `NAME=value` assignments.
fn leading_assignments(words: &[String]) -> usize {
    words.iter().take_while(|word| is_assignment(word)).count()
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The program a word names, lowercased: its directory and `.exe` taken off.
fn program_of(word: &str) -> String {
    program_name(word).to_ascii_lowercase()
}

fn tail(words: &[String], from: usize) -> Vec<String> {
    words
        .get(from..)
        .map(<[String]>::to_vec)
        .unwrap_or_default()
}

/// The words after any leading `-x` flags.
fn after_flags(words: &[String]) -> Vec<String> {
    let flags = words
        .iter()
        .take_while(|word| word.starts_with('-'))
        .count();
    tail(words, flags)
}

fn is_python(program: &str) -> bool {
    program == "py" || program.starts_with("python")
}

/// Programs whose first non-flag argument is the script they run.
fn is_interpreter(program: &str) -> bool {
    matches!(
        program,
        "bash" | "sh" | "zsh" | "node" | "pwsh" | "powershell"
    ) || is_python(program)
}

/// Takes one layer off `words` (which is not empty and starts with a program).
fn peel(words: &[String], ctx: &Ctx<'_>) -> Peel {
    let program = program_of(&words[0]);
    let rest = &words[1..];
    match program.as_str() {
        // PowerShell's call operator.
        "&" | "time" => Peel::Argv(rest.to_vec()),
        "env" | "xargs" | "npx" => Peel::Argv(after_flags(rest)),
        "nice" => {
            let mut at = 0;
            while let Some(word) = rest.get(at) {
                if word == "-n" {
                    at += 2;
                } else if word.starts_with('-') {
                    at += 1;
                } else {
                    break;
                }
            }
            Peel::Argv(tail(rest, at))
        }
        "bash" | "sh" | "zsh" => {
            for (at, word) in rest.iter().enumerate() {
                if !word.starts_with('-') {
                    break;
                }
                // `-c`, `-lc`: a short-flag bundle with `c`. `--norc` and the like are not it.
                if !word.starts_with("--") && word.contains('c') {
                    return match rest.get(at + 1) {
                        Some(script) => Peel::Line(script.clone(), Shell::Posix),
                        None => Peel::Done,
                    };
                }
            }
            Peel::Done
        }
        "powershell" | "pwsh" => {
            let flag = rest.iter().position(|word| {
                word.eq_ignore_ascii_case("-command") || word.eq_ignore_ascii_case("-c")
            });
            match flag {
                Some(at) if at + 1 < rest.len() => {
                    Peel::Line(rest[at + 1..].join(" "), Shell::PowerShell)
                }
                _ => Peel::Done,
            }
        }
        "cmd" => match rest.iter().position(|word| word.eq_ignore_ascii_case("/c")) {
            Some(at) if at + 1 < rest.len() => Peel::Line(rest[at + 1..].join(" "), Shell::Posix),
            _ => Peel::Done,
        },
        "uv" | "poetry" if rest.first().is_some_and(|word| word == "run") => {
            Peel::Argv(after_flags(&rest[1..]))
        }
        "pnpm" if rest.first().is_some_and(|word| word == "exec") => {
            Peel::Argv(after_flags(&rest[1..]))
        }
        program if is_python(program) => {
            // Any heavy.py, unlike the classifier, which trusts only the main checkout's: observing
            // a wrapped run is right whoever wrote the broker.
            if rest
                .first()
                .is_some_and(|word| word.replace('\\', "/").ends_with("scripts/heavy.py"))
            {
                return Peel::Argv(after_broker(&rest[1..]));
            }
            for (at, word) in rest.iter().enumerate() {
                if word == "-m" {
                    return Peel::Argv(tail(rest, at + 1));
                }
                if !word.starts_with('-') {
                    break;
                }
            }
            Peel::Done
        }
        program if ctx.wrappers.iter().any(|wrapper| wrapper == program) => {
            Peel::Argv(after_flags(rest))
        }
        _ => Peel::Done,
    }
}

/// What the broker runs: the argv after its own options and an optional `--`.
fn after_broker(rest: &[String]) -> Vec<String> {
    let mut at = 0;
    while let Some(word) = rest.get(at) {
        let name = word.split_once('=').map_or(word.as_str(), |(name, _)| name);
        if !BROKER_FLAGS.contains(&name) {
            break;
        }
        at += if word.contains('=') { 1 } else { 2 };
    }
    if rest.get(at).is_some_and(|word| word == "--") {
        at += 1;
    }
    tail(rest, at)
}

/// Whether `words` is the same command as `expected`: the program by name, the rest word for word.
fn same_argv(words: &[String], expected: &[String]) -> bool {
    match (words.split_first(), expected.split_first()) {
        (Some((program, rest)), Some((wanted, wanted_rest))) => {
            let (program, wanted) = (program_name(program), program_name(wanted));
            program.eq_ignore_ascii_case(wanted) && rest == wanted_rest
        }
        _ => false,
    }
}

/// Tests one argv, in the order: the gate command, a group's command, an entrypoint, a tool.
fn test_argv(words: &[String], ctx: &Ctx<'_>) -> Option<Hit> {
    let hit = |kind: Kind, name: &str| Hit {
        kind,
        name: name.to_string(),
        segment: words.join(" "),
    };

    if ctx.gate.as_ref().is_some_and(|gate| same_argv(words, gate)) {
        return Some(hit(Kind::GateCommand, "gate_command"));
    }
    if let Some((name, _)) = ctx
        .groups
        .iter()
        .find(|(_, command)| same_argv(words, command))
    {
        return Some(hit(Kind::GroupCommand, name));
    }

    let candidate = entry_candidate(words).replace('\\', "/");
    let candidate = candidate.trim_start_matches("./");
    if let Some(entry) = ctx.map.tests.gate_entrypoints.iter().find(|entry| {
        tests_map::matches(entry, candidate) || candidate.ends_with(&format!("/{entry}"))
    }) {
        return Some(hit(Kind::Entrypoint, entry.as_str()));
    }

    let program = program_of(&words[0]);
    let (key, tool) = ctx
        .map
        .tests
        .tools
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(&program))?;
    if tool.allow.is_empty() {
        return Some(hit(Kind::Tool, key.as_str()));
    }
    let subcommand = subcommand_of(&words[1..])?;
    let covered = tool.allow.iter().any(|allowed| allowed == subcommand);
    (!covered).then(|| hit(Kind::Tool, key.as_str()))
}

/// The path a line could be launching as a gate script: its program, or — behind an interpreter —
/// the script the interpreter runs.
fn entry_candidate(words: &[String]) -> &str {
    if is_interpreter(&program_of(&words[0])) {
        words[1..]
            .iter()
            .find(|word| !word.starts_with('-'))
            .map_or(words[0].as_str(), String::as_str)
    } else {
        words[0].as_str()
    }
}

/// The first word that names a subcommand: past `+toolchain` selectors and flags, and past the
/// value of a flag that takes one. `None` for `--version`/`--help`-style lines.
fn subcommand_of(args: &[String]) -> Option<&str> {
    let mut args = args.iter();
    while let Some(word) = args.next() {
        if word.starts_with('+') {
            continue;
        }
        if word.starts_with('-') {
            if VALUE_FLAGS.contains(&word.as_str()) {
                args.next();
            }
            continue;
        }
        return Some(word);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{Hit, Kind, detect, detection_union};
    use crate::command_reader::Shell;
    use crate::tests_map::{self, TestsMap};

    /// Mirrors the `tools:` / `gate_entrypoints:` block of `nucleos.tests.yaml`, plus one group
    /// and one project wrapper.
    const MAP: &str = r#"
version: 1
tests:
  groups:
    core:
      paths: [core/]
      command: bash scripts/gates.sh core
  gate_entrypoints: [scripts/gates.sh, scripts/gate-diff.sh]
  tools:
    cargo: { allow: [fmt, metadata, tree, add, remove, update, search] }
    rustc: {}
    go: { allow: [mod, list, env, version, fmt] }
    tsc: {}
    vitest: {}
    npm: { allow: [install, ci, ls, view, outdated] }
    npx: {}
    pytest: {}
  wrappers: [mywrap]
"#;

    const GATE: Option<&str> = Some("make gate");

    fn map() -> TestsMap {
        tests_map::parse(MAP).expect("the fixture map parses")
    }

    fn posix(command: &str) -> Option<Hit> {
        detect(command, Shell::Posix, &map(), GATE)
    }

    fn powershell(command: &str) -> Option<Hit> {
        detect(command, Shell::PowerShell, &map(), GATE)
    }

    /// The kind and name of a hit, so a test names what it expects without building a `Hit`.
    fn found(hit: Option<Hit>) -> Option<(&'static str, String)> {
        hit.map(|hit| (hit.kind.as_str(), hit.name))
    }

    fn tool(name: &str) -> Option<(&'static str, String)> {
        Some(("tool", name.to_string()))
    }

    #[test]
    fn a_tool_run_is_a_hit_and_names_the_normalized_segment() {
        let hit = posix("cargo test -p nucleos-core --lib foo").expect("a tool run is a hit");
        assert_eq!(hit.kind, Kind::Tool);
        assert_eq!(hit.kind.as_str(), "tool");
        assert_eq!(hit.name, "cargo");
        assert_eq!(hit.segment, "cargo test -p nucleos-core --lib foo");
    }

    #[test]
    fn the_kinds_have_stable_names() {
        assert_eq!(Kind::GateCommand.as_str(), "gate_command");
        assert_eq!(Kind::GroupCommand.as_str(), "group_command");
        assert_eq!(Kind::Entrypoint.as_str(), "entrypoint");
        assert_eq!(Kind::Tool.as_str(), "tool");
    }

    #[test]
    fn an_allowed_subcommand_is_not_a_hit() {
        assert_eq!(posix("cargo fmt --all"), None);
        assert_eq!(posix("npm ci"), None);
        assert_eq!(posix("go mod tidy"), None);
    }

    #[test]
    fn a_tool_with_no_allow_list_is_a_hit_whatever_follows() {
        assert_eq!(found(posix("tsc -b")), tool("tsc"));
        assert_eq!(found(posix("tsc")), tool("tsc"));
    }

    #[test]
    fn flags_before_the_subcommand_do_not_hide_it() {
        assert_eq!(found(posix("cargo +nightly test")), tool("cargo"));
        assert_eq!(
            found(posix("cargo --manifest-path m/Cargo.toml test")),
            tool("cargo")
        );
        assert_eq!(found(posix("go -C dir test ./...")), tool("go"));
        assert_eq!(found(posix("npm --prefix shell run build")), tool("npm"));
        // The same skipping must still find an ALLOWED subcommand and let it through.
        assert_eq!(posix("cargo +nightly fmt"), None);
    }

    #[test]
    fn a_program_is_known_by_its_name_and_not_by_its_path_or_extension() {
        assert_eq!(
            found(posix("/c/Projects/cargo/bin/cargo.exe test")),
            tool("cargo")
        );
        assert_eq!(found(powershell(r"C:\x\cargo.exe build")), tool("cargo"));
    }

    #[test]
    fn a_gate_script_is_an_entrypoint_whichever_way_it_is_launched() {
        let hit = posix("bash scripts/gates.sh shell").expect("an entrypoint is a hit");
        assert_eq!(hit.kind, Kind::Entrypoint);
        assert_eq!(hit.name, "scripts/gates.sh");
        assert_eq!(
            found(posix("./scripts/gate-diff.sh")),
            Some(("entrypoint", "scripts/gate-diff.sh".to_string()))
        );
        assert_eq!(
            found(posix("sh ./scripts/gates.sh all --fast")),
            Some(("entrypoint", "scripts/gates.sh".to_string()))
        );
    }

    #[test]
    fn the_gate_command_and_a_groups_command_match_by_exact_argv() {
        assert_eq!(
            found(posix("make gate")),
            Some(("gate_command", "gate_command".to_string()))
        );
        // An extra word makes it a different command, and `make` is neither tool nor entrypoint.
        assert_eq!(posix("make gate extra"), None);
        assert_eq!(
            found(posix("bash scripts/gates.sh core")),
            Some(("group_command", "core".to_string()))
        );
    }

    #[test]
    fn wrappers_and_prefixes_around_a_tool_are_peeled_off() {
        assert_eq!(found(posix("CARGO_TARGET_DIR=x cargo test")), tool("cargo"));
        assert_eq!(found(posix("env A=b cargo test")), tool("cargo"));
        assert_eq!(found(posix("time cargo test")), tool("cargo"));
        assert_eq!(found(posix(r#"bash -c "cargo test -p x""#)), tool("cargo"));
        assert_eq!(found(posix(r#"pwsh -Command "cargo test""#)), tool("cargo"));
        assert_eq!(found(posix("cmd /c cargo test")), tool("cargo"));
        assert_eq!(found(powershell("& cargo test")), tool("cargo"));
    }

    #[test]
    fn the_broker_a_runner_and_a_project_wrapper_do_not_hide_the_tool() {
        // Any heavy.py counts, not only the main checkout's: observing a wrapped run is right
        // whoever wrote the broker.
        assert_eq!(
            found(posix(
                "python C:/Projects/nucleos/scripts/heavy.py --agent a -- cargo test"
            )),
            tool("cargo")
        );
        assert_eq!(found(posix("python -m pytest -q")), tool("pytest"));
        assert!(posix("npx vitest run").is_some());
        assert_eq!(found(posix("mywrap --quiet cargo test")), tool("cargo"));
    }

    #[test]
    fn every_segment_of_a_compound_line_is_looked_at() {
        assert_eq!(found(posix("cd core && cargo test")), tool("cargo"));
        assert_eq!(found(posix("echo hi; npm run build")), tool("npm"));
        assert_eq!(posix("cd core && cargo fmt"), None);
    }

    #[test]
    fn ordinary_commands_and_mentions_of_a_tool_are_not_hits() {
        assert_eq!(posix("git status"), None);
        assert_eq!(posix("ls -la"), None);
        assert_eq!(posix("cat scripts/gates.sh"), None);
        assert_eq!(posix("grep cargo Cargo.toml"), None);
        assert_eq!(posix(r#"echo "cargo test""#), None);
    }

    /// Parses a map for the union tests.
    fn parsed(text: &str) -> TestsMap {
        tests_map::parse(text).expect("the fixture map parses")
    }

    /// Detects `command` (POSIX, no gate command) against `map`.
    fn hit_in(map: &TestsMap, command: &str) -> Option<(&'static str, String)> {
        found(detect(command, Shell::Posix, map, None))
    }

    #[test]
    fn the_union_keeps_a_tool_only_the_target_lists() {
        let target =
            parsed("version: 1\ntests:\n  tools:\n    cargo: { allow: [fmt] }\n    pytest: {}\n");
        let worktree = parsed("version: 1\ntests:\n  tools: {}\n");
        let union = detection_union(&target, &worktree);

        assert_eq!(hit_in(&union, "cargo test"), tool("cargo"));
        assert_eq!(hit_in(&union, "cargo fmt"), None);
        assert_eq!(hit_in(&union, "pytest -q"), tool("pytest"));
    }

    #[test]
    fn the_union_allow_is_the_intersection_whatever_the_case() {
        let target =
            parsed("version: 1\ntests:\n  tools:\n    cargo: { allow: [fmt] }\n    rustc: {}\n");
        let worktree = parsed(
            "version: 1\ntests:\n  tools:\n    Cargo: { allow: [fmt, test] }\n    rustc: { allow: [build] }\n",
        );
        let union = detection_union(&target, &worktree);

        assert_eq!(hit_in(&union, "cargo test"), tool("cargo"));
        assert_eq!(hit_in(&union, "cargo fmt"), None);
        assert_eq!(hit_in(&union, "rustc build"), tool("rustc"));
    }

    #[test]
    fn the_union_detects_entrypoints_wrappers_and_groups_of_both() {
        let target = parsed(
            "version: 1\ntests:\n  groups:\n    core:\n      paths: [core/]\n      command: bash scripts/gates.sh core\n  gate_entrypoints: [scripts/gates.sh]\n  tools:\n    cargo: {}\n",
        );
        let worktree = parsed(
            "version: 1\ntests:\n  groups:\n    core:\n      paths: [core/]\n      command: make core\n  gate_entrypoints: [scripts/other.sh]\n  wrappers: [mywrap]\n",
        );
        let union = detection_union(&target, &worktree);

        assert!(hit_in(&union, "bash scripts/gates.sh core").is_some());
        assert_eq!(
            hit_in(&union, "bash scripts/other.sh"),
            Some(("entrypoint", "scripts/other.sh".to_string()))
        );
        assert_eq!(hit_in(&union, "mywrap cargo build"), tool("cargo"));
        assert_eq!(
            hit_in(&union, "make core"),
            Some(("group_command", "worktree:core".to_string()))
        );
    }

    #[test]
    fn the_union_of_disjoint_allows_exempts_nothing_but_still_skips_flags_and_a_bare_call() {
        let target = parsed("version: 1\ntests:\n  tools:\n    cargo: { allow: [fmt] }\n");
        let worktree = parsed("version: 1\ntests:\n  tools:\n    cargo: { allow: [clippy] }\n");
        let union = detection_union(&target, &worktree);

        // The union intersects exemptions: no subcommand is exempt in both, so an empty `allow`
        // means every subcommand call is a hit. A call with no subcommand (flags, bare) is not.
        for command in ["cargo fmt", "cargo clippy"] {
            assert_eq!(hit_in(&union, command), tool("cargo"), "{command}");
        }
        for command in ["cargo --version", "cargo --help", "cargo"] {
            assert_eq!(hit_in(&union, command), None, "{command}");
        }
    }
}
