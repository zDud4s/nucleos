//! Which shell tool calls are heavy, and how much of the admission book each one takes.
//!
//! PURE. A call is heavy when `governance::command_kind` calls one of its segments a test, a
//! check or a build, after the wrappers that only change HOW a program runs (`time`, `nice`,
//! `env`, `command`, `nohup`, `exec`, `timeout`, `stdbuf`) and their option values are peeled.
//! An unfiltered full test suite weighs `full_suite_weight`; every other heavy segment weighs
//! `heavy_weight`; a chain takes its heaviest segment. This module decides nothing about waiting.

use crate::command_reader::{self, Shell};
use crate::config::{AdmissionConfig, CommandKinds};
use crate::governance::classifier::{program_name, shell_for, shell_words};
use crate::governance::command_kind::{Kind, kind_of};

/// PURE: the admission weight of one tool call, or `None` when it is not a heavy shell command.
pub fn weight_of_call(
    tool_name: &str,
    tool_input: &serde_json::Value,
    project: &CommandKinds,
    config: &AdmissionConfig,
) -> Option<u32> {
    if !matches!(tool_name, "Bash" | "PowerShell") {
        return None;
    }
    let command = tool_input.get("command")?.as_str()?;
    let shell = shell_for(tool_name);
    let mut segments = command_reader::segments(command, shell);
    if segments.is_empty() && !command.trim().is_empty() {
        segments.push(command.trim());
    }
    segments
        .into_iter()
        .filter_map(|segment| segment_weight(segment, shell, project, config))
        .max()
}

fn segment_weight(
    segment: &str,
    shell: Shell,
    project: &CommandKinds,
    config: &AdmissionConfig,
) -> Option<u32> {
    let all = shell_words(segment);
    let words = unwrap_wrappers(&all);
    let first = words.first()?;
    // Judge the original text when nothing was peeled, so quoting and backslashes survive.
    let kind = if words.len() == all.len() {
        kind_of(segment, shell, project)
    } else {
        kind_of(&words.join(" "), shell, project)
    };
    match kind {
        Kind::Other => None,
        Kind::Check | Kind::Build => Some(config.heavy_weight),
        Kind::Test => {
            if is_full_suite(&bare_program(first), &words[1..]) {
                Some(config.full_suite_weight)
            } else {
                Some(config.heavy_weight)
            }
        }
    }
}

fn bare_program(word: &str) -> String {
    let name = program_name(word).to_ascii_lowercase();
    for extension in [".exe", ".cmd", ".bat"] {
        if let Some(stem) = name.strip_suffix(extension) {
            return stem.to_string();
        }
    }
    name
}

fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

fn skip_assignments(words: &[String]) -> &[String] {
    let skipped = words.iter().take_while(|word| is_assignment(word)).count();
    &words[skipped..]
}

/// Leading options of a wrapper; `value_flags` take the next word as their value.
fn skip_options<'a>(words: &'a [String], value_flags: &[&str]) -> &'a [String] {
    let mut at = 0;
    while let Some(word) = words.get(at) {
        if word == "--" {
            at += 1;
            break;
        }
        if !word.starts_with('-') || word.len() < 2 {
            break;
        }
        at += if value_flags.contains(&word.as_str()) {
            2
        } else {
            1
        };
    }
    &words[at.min(words.len())..]
}

/// Peel `time`, `nice`, `env`, `command`, `nohup`, `exec`, `timeout` and `stdbuf`, with their
/// options and option values, until a word that is not a wrapper leads the slice.
fn unwrap_wrappers(mut words: &[String]) -> &[String] {
    loop {
        words = skip_assignments(words);
        let Some(first) = words.first() else {
            return words;
        };
        let rest = &words[1..];
        words = match bare_program(first).as_str() {
            "time" => skip_options(rest, &["-f", "--format", "-o", "--output"]),
            "nice" => skip_options(rest, &["-n", "--adjustment"]),
            "env" => skip_options(
                rest,
                &["-u", "--unset", "-C", "--chdir", "-S", "--split-string"],
            ),
            "command" => match rest.first().map(String::as_str) {
                // `command -v x` looks a program up; it runs nothing.
                Some("-v" | "-V") => return &[],
                _ => skip_options(rest, &[]),
            },
            "nohup" => rest,
            "exec" => skip_options(rest, &["-a"]),
            "timeout" => {
                let rest = skip_options(rest, &["-k", "--kill-after", "-s", "--signal"]);
                // The duration is the first positional.
                &rest[rest.len().min(1)..]
            }
            "stdbuf" => skip_options(rest, &["-i", "-o", "-e"]),
            _ => return words,
        };
    }
}

/// Flags that name a subset of the tests, whatever the runner.
const FILTER_FLAGS: &[&str] = &[
    "-k",
    "-m",
    "-t",
    "-g",
    "-run",
    "-skip",
    "--grep",
    "--testNamePattern",
    "--testPathPattern",
    "--deselect",
    "--filter",
    "--tests",
];

fn flag_name(word: &str) -> &str {
    word.split_once('=').map_or(word, |(name, _)| name)
}

/// No filter flag and no positional (bar `ignored`) among `args`; `value_flags` swallow a value.
fn filter_free(args: &[String], value_flags: &[&str], ignored: &[&str]) -> bool {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if word == "--" {
            continue;
        }
        if word.starts_with('-') {
            let name = flag_name(word);
            if FILTER_FLAGS.contains(&name) {
                return false;
            }
            if value_flags.contains(&name) && !word.contains('=') {
                words.next();
            }
        } else if !ignored.contains(&word.as_str()) {
            return false;
        }
    }
    true
}

const PYTEST_VALUE_FLAGS: &[&str] = &[
    "-p",
    "-c",
    "-o",
    "-n",
    "--maxfail",
    "--rootdir",
    "--junitxml",
    "--durations",
    "--color",
    "--tb",
    "--cov",
    "--ignore",
];

/// Whether `program args` runs a whole test suite rather than a named part of it. Unknown
/// programs (a project's own `command_kinds`) are never called full.
fn is_full_suite(program: &str, args: &[String]) -> bool {
    match program {
        "cargo" => cargo_full(args),
        "npm" | "pnpm" | "yarn" | "bun" => node_full(args),
        "npx" | "bunx" => peeled_full(args),
        "pytest" | "py.test" => filter_free(args, PYTEST_VALUE_FLAGS, &[]),
        "python" | "python3" | "py" => match args.iter().position(|word| word == "-m") {
            Some(at) => filter_free(args.get(at + 2..).unwrap_or(&[]), PYTEST_VALUE_FLAGS, &[]),
            None => false,
        },
        "vitest" | "jest" | "mocha" | "tox" | "nox" | "ctest" => {
            filter_free(args, &[], &["run", "watch"])
        }
        "go" => go_full(args),
        "gradle" | "gradlew" => !args
            .iter()
            .any(|word| word == "--tests" || word.starts_with("-Dtest")),
        "mvn" | "mvnw" => !args
            .iter()
            .any(|word| word.starts_with("-Dtest") || word == "-pl" || word == "--projects"),
        "make" | "gmake" => true,
        "dotnet" => {
            !args.iter().any(|word| flag_name(word) == "--filter")
                && args.iter().filter(|word| !word.starts_with('-')).count() <= 1
        }
        _ => false,
    }
}

/// `npx`/`npm exec`: judge the program the wrapper launches.
fn peeled_full(args: &[String]) -> bool {
    match args
        .iter()
        .position(|word| !word.starts_with('-') && word != "--")
    {
        Some(at) => is_full_suite(&bare_program(&args[at]), &args[at + 1..]),
        None => false,
    }
}

fn node_full(args: &[String]) -> bool {
    let Some(at) = args.iter().position(|word| !word.starts_with('-')) else {
        return false;
    };
    let rest = &args[at + 1..];
    match args[at].as_str() {
        "run" | "run-script" => match rest.iter().position(|word| !word.starts_with('-')) {
            Some(script) => filter_free(&rest[script + 1..], &[], &[]),
            None => false,
        },
        "exec" => peeled_full(rest),
        _ => filter_free(rest, &[], &[]),
    }
}

/// `cargo [+toolchain] [global flags] test|bench|nextest ...`.
fn cargo_full(args: &[String]) -> bool {
    const GLOBAL_VALUE_FLAGS: &[&str] = &["-C", "--manifest-path", "--config", "--color", "-Z"];
    const VALUE_FLAGS: &[&str] = &[
        "-p",
        "--package",
        "--exclude",
        "--features",
        "-F",
        "--target",
        "--profile",
        "-j",
        "--jobs",
        "--target-dir",
        "--message-format",
    ];
    const NARROWING_FLAGS: &[&str] = &["--test", "--bin", "--example", "--bench"];
    let mut words = args.iter();
    let mut subcommand = None;
    while let Some(word) = words.next() {
        if word.starts_with('+') {
            continue;
        }
        if GLOBAL_VALUE_FLAGS.contains(&word.as_str()) {
            words.next();
            continue;
        }
        if word.starts_with('-') {
            continue;
        }
        subcommand = Some(word.as_str());
        break;
    }
    let mut ignore_run = subcommand == Some("nextest");
    let mut after_separator = false;
    while let Some(word) = words.next() {
        if after_separator {
            if !word.starts_with('-') {
                return false;
            }
            continue;
        }
        if word == "--" {
            after_separator = true;
            continue;
        }
        if word.starts_with('-') {
            let name = flag_name(word);
            if NARROWING_FLAGS.contains(&name) {
                return false;
            }
            if (VALUE_FLAGS.contains(&name) || GLOBAL_VALUE_FLAGS.contains(&name))
                && !word.contains('=')
            {
                words.next();
            }
        } else if ignore_run && word == "run" {
            ignore_run = false;
        } else {
            return false;
        }
    }
    true
}

/// `go test`: full only when every package argument is `./...` (or `...`) and no `-run`/`-skip`.
fn go_full(args: &[String]) -> bool {
    const VALUE_FLAGS: &[&str] = &[
        "-count",
        "-timeout",
        "-p",
        "-parallel",
        "-tags",
        "-cpu",
        "-coverprofile",
        "-covermode",
        "-bench",
        "-benchtime",
        "-shuffle",
        "-o",
        "-exec",
        "-vet",
        "-mod",
        "-C",
    ];
    let Some(at) = args.iter().position(|word| !word.starts_with('-')) else {
        return false;
    };
    let mut words = args[at + 1..].iter();
    let mut packages = 0;
    while let Some(word) = words.next() {
        if word.starts_with('-') {
            let name = flag_name(word);
            if FILTER_FLAGS.contains(&name) {
                return false;
            }
            if VALUE_FLAGS.contains(&name) && !word.contains('=') {
                words.next();
            }
        } else if word == "./..." || word == "..." {
            packages += 1;
        } else {
            return false;
        }
    }
    packages > 0
}

#[cfg(test)]
mod tests {
    use super::weight_of_call;
    use crate::config::{AdmissionConfig, CommandKinds};
    use serde_json::json;

    fn weight(tool: &str, command: &str) -> Option<u32> {
        weight_with(tool, command, &CommandKinds::default())
    }

    fn weight_with(tool: &str, command: &str, project: &CommandKinds) -> Option<u32> {
        weight_of_call(
            tool,
            &json!({ "command": command }),
            project,
            &AdmissionConfig::default(),
        )
    }

    fn bash(command: &str) -> Option<u32> {
        weight("Bash", command)
    }

    #[test]
    fn build_and_check_commands_are_heavy_with_weight_one() {
        for command in [
            "cargo build",
            "cargo build --release",
            "cargo check",
            "cargo clippy --all-targets -- -D warnings",
            "npm run build",
            "go build ./...",
            "go vet ./...",
            "tsc -b",
        ] {
            assert_eq!(bash(command), Some(1), "{command}");
        }
    }

    #[test]
    fn every_listed_ecosystem_is_heavy() {
        for command in [
            "cargo test",
            "npm test",
            "pnpm test",
            "yarn test",
            "pytest",
            "go test ./...",
            "gradle test",
            "mvn test",
            "make test",
            "dotnet test",
            "dotnet build",
            "mvn package",
        ] {
            assert!(bash(command).is_some(), "{command} must be heavy");
        }
    }

    #[test]
    fn an_unfiltered_full_suite_weighs_the_full_suite_weight() {
        let full = AdmissionConfig::default().full_suite_weight;
        assert_eq!(full, 2);
        for command in [
            "cargo test",
            "cargo test --workspace",
            "cargo test --release",
            "npm test",
            "pnpm test",
            "yarn test",
            "pytest",
            "pytest -q",
            "go test ./...",
            "go test -race ./...",
            "gradle test",
            "mvn test",
            "make test",
        ] {
            assert_eq!(bash(command), Some(full), "{command}");
        }
        // The weight comes from the config, not from a constant.
        let config = AdmissionConfig {
            full_suite_weight: 5,
            heavy_weight: 3,
            ..AdmissionConfig::default()
        };
        let input = json!({ "command": "cargo test" });
        assert_eq!(
            weight_of_call("Bash", &input, &CommandKinds::default(), &config),
            Some(5)
        );
        let input = json!({ "command": "cargo build" });
        assert_eq!(
            weight_of_call("Bash", &input, &CommandKinds::default(), &config),
            Some(3)
        );
    }

    #[test]
    fn a_filtered_suite_weighs_one() {
        for command in [
            "cargo test some_name",
            "cargo test -p nucleos-core --lib some_name",
            "cargo test -- some_name",
            "npm test -- -t some_name",
            "pytest -k some_name",
            "pytest tests/test_one.py",
            "go test -run SomeName ./...",
            "go test ./pkg/one",
        ] {
            assert_eq!(bash(command), Some(1), "{command}");
        }
    }

    #[test]
    fn wrappers_and_their_option_values_are_unwrapped() {
        for (command, expected) in [
            ("time cargo build", 1),
            ("time cargo test", 2),
            ("nice -n 10 cargo build", 1),
            ("nice -n 10 cargo test", 2),
            ("env FOO=1 cargo build", 1),
            ("env -u BAR FOO=1 cargo test", 2),
            ("command cargo build", 1),
            ("nohup cargo build", 1),
            ("exec cargo check", 1),
            ("timeout 600 cargo test", 2),
            ("timeout -k 5 600 cargo build", 1),
            ("timeout --signal=KILL 600 cargo test some_name", 1),
            ("stdbuf -oL cargo build", 1),
            ("nohup nice -n 5 time cargo test", 2),
        ] {
            assert_eq!(bash(command), Some(expected), "{command}");
        }
    }

    #[test]
    fn a_light_command_is_not_heavy() {
        for command in [
            "ls -la",
            "git status",
            "cat Cargo.toml",
            "cargo run",
            "npm install",
            "echo cargo test",
            "time ls",
            "nice -n 10 git log",
            "timeout 600 sleep 1",
            "",
        ] {
            assert_eq!(bash(command), None, "{command:?}");
        }
    }

    #[test]
    fn a_chained_command_takes_its_heaviest_segment() {
        assert_eq!(bash("cd core && cargo build"), Some(1));
        assert_eq!(bash("cd core && cargo test"), Some(2));
        assert_eq!(bash("cargo build && cargo test"), Some(2));
        assert_eq!(bash("cargo test && cargo build"), Some(2));
        assert_eq!(bash("cargo build; cargo test some_name"), Some(1));
        assert_eq!(bash("cargo test some_name && cargo test"), Some(2));
        assert_eq!(bash("cd core && ls && git status"), None);
    }

    #[test]
    fn a_powershell_command_is_recognised() {
        assert_eq!(weight("PowerShell", "cargo build"), Some(1));
        assert_eq!(weight("PowerShell", "cargo test"), Some(2));
        assert_eq!(
            weight("PowerShell", "cd core; cargo test some_name"),
            Some(1)
        );
        assert_eq!(weight("PowerShell", "cd core; cargo test"), Some(2));
        assert_eq!(weight("PowerShell", "C:\\tools\\cargo.exe check"), Some(1));
        assert_eq!(weight("PowerShell", "Get-ChildItem"), None);
    }

    #[test]
    fn a_non_shell_tool_is_never_heavy() {
        let config = AdmissionConfig::default();
        let kinds = CommandKinds::default();
        for tool in [
            "Read",
            "Edit",
            "Write",
            "Grep",
            "Task",
            "mcp__nucleos__verify",
        ] {
            let input = json!({ "command": "cargo test" });
            assert_eq!(
                weight_of_call(tool, &input, &kinds, &config),
                None,
                "{tool}"
            );
        }
        // A shell tool without a readable `command` is not heavy either.
        for input in [
            json!({}),
            json!({ "command": 7 }),
            json!({ "cmd": "cargo test" }),
            json!(null),
        ] {
            assert_eq!(
                weight_of_call("Bash", &input, &kinds, &config),
                None,
                "{input}"
            );
        }
    }

    #[test]
    fn a_project_extension_makes_a_command_heavy() {
        let project = CommandKinds {
            build: vec!["just ship".to_string()],
            check: vec!["just lint".to_string()],
            ..CommandKinds::default()
        };
        assert_eq!(weight_with("Bash", "just ship", &project), Some(1));
        assert_eq!(weight_with("Bash", "just lint --all", &project), Some(1));
        assert_eq!(
            weight_with("PowerShell", "cd x; just ship", &project),
            Some(1)
        );
        // Without the project list the same line is light.
        assert_eq!(weight("Bash", "just ship"), None);
        // The compiled defaults still hold with a list in place.
        assert_eq!(weight_with("Bash", "cargo test", &project), Some(2));
    }
}
