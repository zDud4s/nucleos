//! What kind of command a shell line is: a test run, a check, a build, or something else.
//!
//! PURE. The compiled defaults cover the common toolchains; a project extends them with the
//! `command_kinds:` key of its `autopilot.yaml`.

use crate::command_reader::{self, Shell};
use crate::config::CommandKinds;
use crate::governance::classifier::{
    matches_command_prefix, normalize_command, program_name, shell_words,
};

/// What a command line does, as far as scheduling and output handling care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Test,
    Check,
    Build,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Test => "test",
            Kind::Check => "check",
            Kind::Build => "build",
            Kind::Other => "other",
        }
    }
}

/// The first segment of a line that is not `Other`, with the program that decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    pub kind: Kind,
    pub program: String,
    pub segment: String,
}

/// How many wrapper layers (`npx`, `npm exec`) are peeled before giving up.
const MAX_PEEL: usize = 3;

/// PURE: the first segment of `command` that is a test, check or build, if any.
pub fn recognize(command: &str, shell: Shell, project: &CommandKinds) -> Option<Recognized> {
    let mut segments = command_reader::segments(command, shell);
    if segments.is_empty() && !command.trim().is_empty() {
        segments.push(command.trim());
    }
    for segment in segments {
        let words = shell_words(segment);
        let words = skip_assignments(&words);
        let Some(first) = words.first() else {
            continue;
        };
        let program = bare_program(first);
        let kind = project_kind(segment, project)
            .unwrap_or_else(|| classify_program(&program, &words[1..], 0));
        if kind != Kind::Other {
            return Some(Recognized {
                kind,
                program,
                segment: segment.to_string(),
            });
        }
    }
    None
}

/// PURE: [`recognize`]'s kind, or `Other`.
pub fn kind_of(command: &str, shell: Shell, project: &CommandKinds) -> Kind {
    recognize(command, shell, project).map_or(Kind::Other, |found| found.kind)
}

/// Leading `NAME=value` words belong to the shell, not to the program.
fn skip_assignments(words: &[String]) -> &[String] {
    let skipped = words.iter().take_while(|word| is_assignment(word)).count();
    &words[skipped..]
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

/// The program a word names: directory, case and `.exe`/`.cmd`/`.bat` taken off.
fn bare_program(word: &str) -> String {
    let name = program_name(word).to_ascii_lowercase();
    for extension in [".exe", ".cmd", ".bat"] {
        if let Some(stem) = name.strip_suffix(extension) {
            return stem.to_string();
        }
    }
    name
}

/// The project's own lists, tried before the defaults: test, then check, then build.
fn project_kind(segment: &str, project: &CommandKinds) -> Option<Kind> {
    let normalized = normalize_command(segment);
    if matches_command_prefix(&normalized, &project.test) {
        Some(Kind::Test)
    } else if matches_command_prefix(&normalized, &project.check) {
        Some(Kind::Check)
    } else if matches_command_prefix(&normalized, &project.build) {
        Some(Kind::Build)
    } else {
        None
    }
}

fn is_flag(word: &str) -> bool {
    word.starts_with('-')
}

/// The first word that is not a flag.
fn first_positional(args: &[String]) -> Option<&str> {
    args.iter().map(String::as_str).find(|word| !is_flag(word))
}

fn classify_program(program: &str, args: &[String], depth: usize) -> Kind {
    match program {
        "cargo" => cargo_kind(args),
        "npm" | "pnpm" | "yarn" | "bun" => node_kind(args, depth),
        "npx" | "bunx" => peel(args, depth),
        "pytest" | "py.test" | "vitest" | "jest" | "mocha" | "tox" | "nox" | "ctest" => Kind::Test,
        "python" | "python3" | "py" => python_kind(args),
        "tsc" | "mypy" | "eslint" => Kind::Check,
        "go" => match first_positional(args) {
            Some("test") => Kind::Test,
            Some("vet") => Kind::Check,
            Some("build" | "install") => Kind::Build,
            _ => Kind::Other,
        },
        "make" | "gmake" => match first_positional(args) {
            Some("test") => Kind::Test,
            Some("check") => Kind::Check,
            _ => Kind::Build,
        },
        "gradle" | "gradlew" => match first_positional(args) {
            Some(task) if task.to_ascii_lowercase().contains("test") => Kind::Test,
            Some(task) if task.eq_ignore_ascii_case("check") => Kind::Check,
            _ => Kind::Build,
        },
        "mvn" | "mvnw" => {
            let tests = args
                .iter()
                .map(String::as_str)
                .any(|word| matches!(word, "test" | "verify" | "integration-test"));
            if tests { Kind::Test } else { Kind::Build }
        }
        "dotnet" => match first_positional(args) {
            Some("test") => Kind::Test,
            Some("build") => Kind::Build,
            _ => Kind::Other,
        },
        _ => Kind::Other,
    }
}

/// `cargo [+toolchain] [global flags] <subcommand>`.
fn cargo_kind(args: &[String]) -> Kind {
    const VALUE_FLAGS: &[&str] = &["-C", "--manifest-path", "--config", "--color", "-Z"];
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        if word.starts_with('+') {
            continue;
        }
        if VALUE_FLAGS.contains(&word) {
            words.next();
            continue;
        }
        if is_flag(word) {
            continue;
        }
        return match word {
            "test" | "t" | "nextest" | "bench" => Kind::Test,
            "check" | "c" | "clippy" | "fmt" => Kind::Check,
            "build" | "b" | "doc" => Kind::Build,
            _ => Kind::Other,
        };
    }
    Kind::Other
}

/// `npm`, `pnpm`, `yarn` and `bun`: a script run directly, through `run`, or an `exec` wrapper.
fn node_kind(args: &[String], depth: usize) -> Kind {
    let Some(position) = args.iter().position(|word| !is_flag(word)) else {
        return Kind::Other;
    };
    let sub = args[position].as_str();
    let rest = &args[position + 1..];
    match sub {
        "test" | "t" | "tst" => Kind::Test,
        "lint" | "typecheck" | "check" | "tsc" => Kind::Check,
        "build" => Kind::Build,
        "run" | "run-script" => match first_positional(rest) {
            Some(script) if script.starts_with("test") => Kind::Test,
            Some("lint" | "typecheck" | "check" | "tsc") => Kind::Check,
            Some("build") => Kind::Build,
            _ => Kind::Other,
        },
        "exec" => peel(rest, depth),
        _ => Kind::Other,
    }
}

/// Judge the program a wrapper (`npx`, `npm exec`) launches.
fn peel(args: &[String], depth: usize) -> Kind {
    if depth >= MAX_PEEL {
        return Kind::Other;
    }
    let Some(position) = args.iter().position(|word| !is_flag(word) && word != "--") else {
        return Kind::Other;
    };
    classify_program(
        &bare_program(&args[position]),
        &args[position + 1..],
        depth + 1,
    )
}

/// `python -m pytest` and `python -m unittest`.
fn python_kind(args: &[String]) -> Kind {
    let module = args
        .iter()
        .position(|word| word == "-m")
        .and_then(|at| args.get(at + 1));
    match module.map(String::as_str) {
        Some("pytest" | "unittest") => Kind::Test,
        _ => Kind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_reader::Shell;
    use crate::config::CommandKinds;

    fn none() -> CommandKinds {
        CommandKinds::default()
    }

    fn kind(command: &str, shell: Shell) -> Kind {
        kind_of(command, shell, &none())
    }

    fn both(command: &str, expected: Kind) {
        assert_eq!(kind(command, Shell::Posix), expected, "posix: {command}");
        assert_eq!(
            kind(command, Shell::PowerShell),
            expected,
            "powershell: {command}"
        );
    }

    #[test]
    fn recognizes_default_test_check_and_build_commands() {
        // cargo
        both("cargo test", Kind::Test);
        both("cargo t", Kind::Test);
        both("cargo nextest run", Kind::Test);
        both("cargo bench", Kind::Test);
        both("cargo test -p nucleos-core --lib foo", Kind::Test);
        both("cargo +nightly test", Kind::Test);
        both("cargo -C core test", Kind::Test);
        both("cargo --manifest-path core/Cargo.toml test", Kind::Test);
        both("cargo check", Kind::Check);
        both("cargo c", Kind::Check);
        both("cargo clippy --all-targets", Kind::Check);
        both("cargo fmt --all", Kind::Check);
        both("cargo build", Kind::Build);
        both("cargo b --release", Kind::Build);
        both("cargo doc", Kind::Build);
        // npm family
        both("npm test", Kind::Test);
        both("npm t", Kind::Test);
        both("npm run test", Kind::Test);
        both("npm run test:unit", Kind::Test);
        both("pnpm test", Kind::Test);
        both("yarn test", Kind::Test);
        both("bun test", Kind::Test);
        both("npm run lint", Kind::Check);
        both("npm run typecheck", Kind::Check);
        both("yarn lint", Kind::Check);
        both("npm run build", Kind::Build);
        both("pnpm build", Kind::Build);
        both("npx vitest run", Kind::Test);
        both("npx tsc -b", Kind::Check);
        both("npm exec tsc -- -b", Kind::Check);
        both("pnpm exec vitest", Kind::Test);
        // python
        both("pytest -q", Kind::Test);
        both("py.test tests/", Kind::Test);
        both("python -m pytest", Kind::Test);
        both("python3 -m unittest", Kind::Test);
        both("py -m pytest", Kind::Test);
        both("mypy src", Kind::Check);
        both("eslint .", Kind::Check);
        both("tsc -b", Kind::Check);
        both("tox", Kind::Test);
        both("nox -s tests", Kind::Test);
        both("vitest run", Kind::Test);
        both("jest", Kind::Test);
        both("mocha", Kind::Test);
        both("ctest", Kind::Test);
        // go
        both("go test -race ./...", Kind::Test);
        both("go vet ./...", Kind::Check);
        both("go build ./...", Kind::Build);
        both("go install ./cmd/x", Kind::Build);
        // make
        both("make test", Kind::Test);
        both("make -j4 check", Kind::Check);
        both("make", Kind::Build);
        both("make all", Kind::Build);
        both("gmake test", Kind::Test);
        // gradle
        both("gradle test", Kind::Test);
        both("./gradlew test", Kind::Test);
        both("gradlew check", Kind::Check);
        both("gradle assemble", Kind::Build);
        both("gradle connectedAndroidTest", Kind::Test);
        // maven
        both("mvn test", Kind::Test);
        both("mvn verify", Kind::Test);
        both("./mvnw integration-test", Kind::Test);
        both("mvn package", Kind::Build);
        both("mvn clean install", Kind::Build);
        // dotnet
        both("dotnet test", Kind::Test);
        both("dotnet build", Kind::Build);
    }

    #[test]
    fn the_program_is_recognized_through_a_path_and_an_extension() {
        let found = recognize("C:\\tools\\Cargo.EXE test", Shell::PowerShell, &none())
            .expect("a cargo test through a path is recognized");
        assert_eq!(found.kind, Kind::Test);
        assert_eq!(found.program, "cargo");
        let found =
            recognize("npm.cmd run build", Shell::PowerShell, &none()).expect("npm.cmd is npm");
        assert_eq!(found.kind, Kind::Build);
        assert_eq!(found.program, "npm");
    }

    #[test]
    fn a_project_list_extends_the_defaults() {
        let project = CommandKinds {
            test: vec!["just t".to_string()],
            check: vec!["just lint".to_string()],
            build: vec!["just ship".to_string()],
        };
        assert_eq!(kind_of("just t", Shell::Posix, &project), Kind::Test);
        assert_eq!(
            kind_of("just t --all", Shell::PowerShell, &project),
            Kind::Test
        );
        assert_eq!(kind_of("just lint", Shell::Posix, &project), Kind::Check);
        assert_eq!(kind_of("just ship", Shell::Posix, &project), Kind::Build);
        // Not a prefix of the entry: a different recipe.
        assert_eq!(kind_of("just deploy", Shell::Posix, &project), Kind::Other);
        // Without the list the same line is nothing.
        assert_eq!(kind("just t", Shell::Posix), Kind::Other);
        // The defaults still hold with a list in place.
        assert_eq!(kind_of("cargo test", Shell::Posix, &project), Kind::Test);
        // The project list wins over a default: `cargo run` is Other by default.
        let own = CommandKinds {
            build: vec!["cargo run".to_string()],
            ..CommandKinds::default()
        };
        assert_eq!(kind_of("cargo run", Shell::Posix, &own), Kind::Build);
        assert_eq!(
            recognize("just t", Shell::Posix, &project)
                .expect("recognized")
                .segment,
            "just t"
        );
    }

    #[test]
    fn ordinary_commands_are_other() {
        for command in [
            "ls",
            "ls -la",
            "git status",
            "git commit -m test",
            "cargo run",
            "cargo install ripgrep",
            "npm install",
            "npm ci",
            "npm run dev",
            "go run .",
            "go mod tidy",
            "dotnet run",
            "echo test",
            "cat test.txt",
            "python script.py",
            "python -m http.server",
            "",
            "   ",
        ] {
            assert_eq!(
                kind(command, Shell::Posix),
                Kind::Other,
                "posix: {command:?}"
            );
            assert_eq!(
                kind(command, Shell::PowerShell),
                Kind::Other,
                "powershell: {command:?}"
            );
            assert!(
                recognize(command, Shell::Posix, &none()).is_none(),
                "no recognition for {command:?}"
            );
        }
    }

    #[test]
    fn the_kind_is_found_in_any_segment() {
        let found = recognize("cd core && cargo test -p x", Shell::Posix, &none())
            .expect("the second segment is a test run");
        assert_eq!(found.kind, Kind::Test);
        assert_eq!(found.program, "cargo");
        assert_eq!(found.segment, "cargo test -p x");

        assert_eq!(kind("cd x && cargo test", Shell::Posix), Kind::Test);
        assert_eq!(kind("cd x; cargo test", Shell::PowerShell), Kind::Test);
        assert_eq!(kind("cd x; cargo test", Shell::Posix), Kind::Test);

        // Leading environment assignments belong to the shell, not the program.
        let found = recognize("FOO=1 cargo test", Shell::Posix, &none())
            .expect("an assignment before the program is skipped");
        assert_eq!(found.kind, Kind::Test);
        assert_eq!(found.program, "cargo");
        assert_eq!(kind("FOO=1 BAR=2 npm run build", Shell::Posix), Kind::Build);

        // The first segment that is not Other decides.
        let found =
            recognize("cargo build && cargo test", Shell::Posix, &none()).expect("recognized");
        assert_eq!(found.kind, Kind::Build);
        assert_eq!(found.segment, "cargo build");

        // Nothing recognizable in any segment.
        assert_eq!(kind("cd x && ls && git status", Shell::Posix), Kind::Other);
    }

    #[test]
    fn as_str_names_each_kind() {
        assert_eq!(Kind::Test.as_str(), "test");
        assert_eq!(Kind::Check.as_str(), "check");
        assert_eq!(Kind::Build.as_str(), "build");
        assert_eq!(Kind::Other.as_str(), "other");
    }
}
