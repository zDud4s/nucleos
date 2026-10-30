//! The closed list of heavy commands the daemon runs for a leaf, scoped to its branch (R5).
//!
//! `validate` is pure and fails closed: a character, flag, package or path it does not recognise
//! parks the run. Characters and flags are allow-lists rather than deny-lists, because a new
//! metacharacter or a new cargo flag must park until somebody has looked at it.

use crate::work_nodes::{normalise_glob, path_contained};

/// A command the daemon may run for this branch, already split into its program and arguments so
/// the runner never needs a shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowed {
    pub argv: Vec<String>,
}

/// A command the daemon refuses to run; the run parks and asks for this decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parked {
    pub needs: String,
}

/// Every package of the workspace, by `[package] name` in its `Cargo.toml`, and its directory.
/// Written by hand so `validate` stays pure, so a crate added or moved in the root `Cargo.toml`
/// parks until it is listed here. `shell` is absent on purpose: it is excluded from the workspace,
/// so `-p shell` does not resolve and `--manifest-path` is its only way in.
const WORKSPACE_PACKAGES: &[(&str, &str)] = &[
    ("nucleos-core", "core"),
    ("nucleos-base", "core/crates/nucleos-base"),
    ("nucleos-app", "core/crates/nucleos-app"),
    ("nucleos-daemon", "core/crates/nucleos-daemon"),
];

/// The flags one parser accepts: `switches` stand alone, `valued` take `name value` or `name=value`.
struct Flags {
    switches: &'static [&'static str],
    valued: &'static [&'static str],
}

/// Off this list on purpose, among others: `--config`, `--target-dir`, `--artifact-dir`, `--fix`,
/// `--target` (it can name a spec file), `-Z`, `-C`, `--workspace`, `--all`, `--exclude`.
const CARGO_FLAGS: Flags = Flags {
    switches: &[
        "--lib",
        "--bins",
        "--tests",
        "--benches",
        "--examples",
        "--doc",
        "--all-targets",
        "--all-features",
        "--no-default-features",
        "--no-deps",
        "--no-run",
        "--no-fail-fast",
        "--keep-going",
        "--locked",
        "--frozen",
        "--offline",
        "--release",
        "--quiet",
        "-q",
        "--verbose",
        "-v",
        "-vv",
    ],
    valued: &[
        "-p",
        "--package",
        "--manifest-path",
        "--bin",
        "--test",
        "--bench",
        "--example",
        "--features",
        "-F",
        "--profile",
        "--jobs",
        "-j",
        "--color",
        "--message-format",
    ],
};

/// What `cargo test -- ...` hands the test binary. `--logfile` is off the list: it writes a file.
const TEST_BINARY_FLAGS: Flags = Flags {
    switches: &[
        "--nocapture",
        "--no-capture",
        "--show-output",
        "--ignored",
        "--include-ignored",
        "--exact",
        "--list",
    ],
    valued: &["--skip", "--test-threads"],
};

/// What `cargo clippy -- ...` hands the compiler: lint levels only, because the compiler's other
/// flags include `-C linker=<program>` and `-o <path>`.
const CLIPPY_LINT_FLAGS: Flags = Flags {
    switches: &[],
    valued: &["-D", "-W", "-A", "-F"],
};

/// Off this list on purpose, among others: `-exec`, `-toolexec`, `-vettool`, `-o`, `-c`, `-args`,
/// `-overlay`, `-modfile`, `-mod`, `-gcflags`, `-ldflags`, and the `*profile`, `-trace` and
/// `-outputdir` family, which write files.
const GO_TEST_FLAGS: Flags = Flags {
    switches: &[
        "-v",
        "-short",
        "-race",
        "-cover",
        "-failfast",
        "-json",
        "-benchmem",
    ],
    valued: &[
        "-run",
        "-skip",
        "-list",
        "-bench",
        "-benchtime",
        "-count",
        "-timeout",
        "-parallel",
        "-p",
        "-cpu",
        "-shuffle",
        "-tags",
    ],
};

/// **Pure.** Whether the leaf may run `cmd`, given the globs its branch owns. Every package, path
/// and manifest the command names must lie inside one glob, and it must name at least one: a
/// command with no scope runs over the whole workspace or the current directory.
pub fn validate(cmd: &str, branch_paths: &[String]) -> Result<Allowed, Parked> {
    allowed_argv(cmd, branch_paths)
        .map(|argv| Allowed { argv })
        .map_err(|reason| Parked {
            needs: format!("a decision on running {cmd:?}, refused because {reason}"),
        })
}

fn allowed_argv(cmd: &str, branch_paths: &[String]) -> Result<Vec<String>, String> {
    if let Some(bad) = cmd.chars().find(|c| !is_plain(*c)) {
        return Err(format!("the character {bad:?} is not plain command text"));
    }
    // A space is the only whitespace `is_plain` lets through.
    let tokens: Vec<&str> = cmd.split(' ').filter(|token| !token.is_empty()).collect();
    let off_the_list = || "only `cargo test|check|clippy` and `go test` are allowed".to_owned();
    let [program, subcommand, args @ ..] = tokens.as_slice() else {
        return Err(off_the_list());
    };
    let scopes = match (*program, *subcommand) {
        ("cargo", "test" | "check" | "clippy") => cargo_scopes(subcommand, args)?,
        ("go", "test") => go_scopes(args)?,
        _ => return Err(off_the_list()),
    };
    if scopes.is_empty() {
        return Err("it names no package or path, so it is not narrowed to the branch".to_owned());
    }
    // A branch glob that does not normalise contains nothing, as in `work_nodes`.
    let globs: Vec<String> = branch_paths
        .iter()
        .filter_map(|glob| normalise_glob(glob).ok())
        .collect();
    for scope in &scopes {
        if !globs.iter().any(|glob| path_contained(scope, glob)) {
            return Err(format!(
                "{scope:?} is outside the branch ({})",
                branch_paths.join(", ")
            ));
        }
    }
    Ok(tokens.iter().map(|token| (*token).to_owned()).collect())
}

/// The only characters a command may hold. Everything a shell reads specially is absent, quotes
/// and the backslash included, so no quoting can smuggle one in.
fn is_plain(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '/' | ':' | '=' | ',')
}

/// The directories one cargo command reaches: each `-p` package and each `--manifest-path`.
fn cargo_scopes(subcommand: &str, args: &[&str]) -> Result<Vec<String>, String> {
    let (own, trailing) = match args.iter().position(|arg| *arg == "--") {
        Some(at) => (&args[..at], &args[at + 1..]),
        None => (args, &args[args.len()..]),
    };
    let parsed = parse_flags("cargo", own, &CARGO_FLAGS)?;
    // Only `cargo test` takes a positional, and it is a test filter rather than a path.
    if subcommand != "test"
        && let Some(extra) = parsed.positional.first()
    {
        return Err(format!("`cargo {subcommand}` takes no argument {extra:?}"));
    }
    check_trailing(subcommand, trailing)?;

    let mut scopes = Vec::new();
    for (flag, value) in &parsed.values {
        match *flag {
            "-p" | "--package" => scopes.push(package_dir(value)?),
            "--manifest-path" => scopes.push(manifest_dir(value)?),
            _ => {}
        }
    }
    Ok(scopes)
}

/// The arguments after `--`, which cargo passes on to another program unread.
fn check_trailing(subcommand: &str, trailing: &[&str]) -> Result<(), String> {
    match subcommand {
        "test" => parse_flags("the test binary", trailing, &TEST_BINARY_FLAGS).map(|_| ()),
        "clippy" => {
            let parsed = parse_flags("clippy", trailing, &CLIPPY_LINT_FLAGS)?;
            // A positional here is an input file for the compiler.
            match parsed.positional.first() {
                Some(extra) => Err(format!("clippy takes no argument {extra:?} after `--`")),
                None => Ok(()),
            }
        }
        _ => match trailing.first() {
            Some(extra) => Err(format!("`cargo {subcommand}` passes on no {extra:?}")),
            None => Ok(()),
        },
    }
}

fn package_dir(name: &str) -> Result<String, String> {
    WORKSPACE_PACKAGES
        .iter()
        .find(|(package, _)| *package == name)
        .map(|(_, dir)| (*dir).to_owned())
        .ok_or_else(|| format!("{name:?} is not a known package of the workspace"))
}

/// The directory of a manifest, because the command runs over the package and not over the one
/// file. The root manifest has no directory to contain and is the whole workspace, so it parks.
fn manifest_dir(raw: &str) -> Result<String, String> {
    clean_path(raw)?
        .strip_suffix("/Cargo.toml")
        .map(str::to_owned)
        .ok_or_else(|| format!("the manifest {raw:?} is not a `Cargo.toml` inside a directory"))
}

/// The directories one `go test` reaches: `-C` itself and each package, resolved against it.
fn go_scopes(args: &[&str]) -> Result<Vec<String>, String> {
    let (base, rest) = split_go_dir(args)?;
    let parsed = parse_flags("go test", rest, &GO_TEST_FLAGS)?;
    let base = base.map(clean_path).transpose()?;
    let mut scopes = parsed
        .positional
        .iter()
        .map(|pattern| go_package_dir(pattern, base.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;
    scopes.extend(base);
    Ok(scopes)
}

/// Takes a leading `-C`. Go only accepts it as the first flag, so anywhere else it is an unknown
/// flag to `parse_flags` and parks.
fn split_go_dir<'a, 'b>(args: &'b [&'a str]) -> Result<(Option<&'a str>, &'b [&'a str]), String> {
    let (value, rest) = match args {
        ["-C", value, rest @ ..] => (*value, rest),
        ["-C"] => return Err("`-C` has no directory".to_owned()),
        [first, rest @ ..] => match first.strip_prefix("-C=") {
            Some(value) => (value, rest),
            None => return Ok((None, args)),
        },
        [] => return Ok((None, args)),
    };
    if value.is_empty() || value.starts_with('-') {
        return Err("`-C` has no directory".to_owned());
    }
    Ok((Some(value), rest))
}

/// The directory a go package argument names. Only the `./` form is a directory: go reads a bare
/// `sidecars/email` as an import path. `...` is a wildcard anywhere in a pattern, so
/// `./sidecars/email...` also matches `sidecars/emailx`, and only a final `/...` stays in the
/// directory.
fn go_package_dir(pattern: &str, base: Option<&str>) -> Result<String, String> {
    if pattern != "." && !pattern.starts_with("./") {
        return Err(format!("the go package {pattern:?} does not start with `./`"));
    }
    let dir = pattern.strip_suffix("/...").unwrap_or(pattern);
    if dir.contains("...") {
        return Err(format!("the go package {pattern:?} has a partial `...`"));
    }
    match base {
        Some(base) => clean_path(&format!("{base}/{dir}")),
        None => clean_path(dir),
    }
}

/// A path relative to the project root, in `normalise_glob` form, so `..`, an absolute path and
/// the root itself all park. A colon parks too: it names a drive or an NTFS stream.
fn clean_path(raw: &str) -> Result<String, String> {
    if raw.contains(':') {
        return Err(format!("the path {raw:?} holds a colon"));
    }
    normalise_glob(raw).map_err(|_| format!("the path {raw:?} is not inside the project"))
}

/// Flags with a value, by name, and the positional arguments of one command line.
struct Parsed<'a> {
    values: Vec<(&'a str, &'a str)>,
    positional: Vec<&'a str>,
}

/// Reads a command line against one flag list. A flag off the list, a valued flag with no value, or
/// a value that looks like another flag all fail.
fn parse_flags<'a>(tool: &str, args: &[&'a str], flags: &Flags) -> Result<Parsed<'a>, String> {
    let mut parsed = Parsed {
        values: Vec::new(),
        positional: Vec::new(),
    };
    let mut rest = args.iter().copied();
    while let Some(arg) = rest.next() {
        if !arg.starts_with('-') {
            parsed.positional.push(arg);
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (arg, None),
        };
        if inline.is_none() && flags.switches.contains(&name) {
            continue;
        }
        if !flags.valued.contains(&name) {
            return Err(format!("{arg:?} is not on the closed list for {tool}"));
        }
        let value = inline
            .or_else(|| rest.next())
            .filter(|value| !value.is_empty() && !value.starts_with('-'))
            .ok_or_else(|| format!("{name:?} has no value"))?;
        parsed.values.push((name, value));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::{Allowed, Parked, validate};

    /// Typed on purpose: it pins `validate(&str, &[String]) -> Result<Allowed, Parked>`.
    fn check(cmd: &str, globs: &[&str]) -> Result<Allowed, Parked> {
        let branch_paths: Vec<String> = globs.iter().map(|glob| (*glob).to_owned()).collect();
        validate(cmd, &branch_paths)
    }

    fn assert_allowed(cmd: &str, globs: &[&str]) {
        match check(cmd, globs) {
            Ok(_) => {}
            Err(parked) => panic!(
                "{cmd:?} must be allowed for {globs:?}, parked with: {}",
                parked.needs
            ),
        }
    }

    /// Every refusal is a parked run with a decision to ask for, so `needs` is never blank.
    fn assert_parked(cmd: &str, globs: &[&str]) {
        match check(cmd, globs) {
            Ok(_) => panic!("{cmd:?} must be parked for {globs:?}"),
            Err(parked) => {
                let needs: &str = &parked.needs;
                assert!(
                    !needs.trim().is_empty(),
                    "{cmd:?} was parked with a blank `needs`"
                );
            }
        }
    }

    /// Asserts the clean command passes first, so a `validate` that parks everything cannot satisfy
    /// a refusal test, then that each suffix makes it park.
    fn assert_suffixes_park(clean: &str, suffixes: &[&str], globs: &[&str]) {
        assert_allowed(clean, globs);
        for suffix in suffixes {
            assert_parked(&format!("{clean}{suffix}"), globs);
        }
    }

    const CORE: &[&str] = &["core/**"];
    const BASE: &[&str] = &["core/crates/nucleos-base/**"];
    const EMAIL: &[&str] = &["sidecars/email/**"];

    #[test]
    fn cargo_test_check_and_clippy_accept_a_package_inside_the_branch() {
        for cmd in [
            "cargo test -p nucleos-core",
            "cargo check -p nucleos-core",
            "cargo clippy -p nucleos-core",
            "cargo test --package nucleos-core",
            "cargo test -p nucleos-core --lib tree_job::commands",
            "cargo clippy -p nucleos-core --all-targets -- -D warnings",
        ] {
            assert_allowed(cmd, CORE);
        }
        assert_allowed("cargo test -p nucleos-base", BASE);
        assert_allowed(
            "cargo test -p nucleos-core",
            &["sidecars/email/**", "core/**"],
        );
    }

    #[test]
    fn go_test_accepts_dash_c_and_paths_inside_the_branch() {
        for cmd in [
            "go test -C sidecars/email ./...",
            "go test -C sidecars/email -run TestSend ./...",
            "go test ./sidecars/email/...",
            "go test ./sidecars/email",
        ] {
            assert_allowed(cmd, EMAIL);
        }
    }

    #[test]
    fn a_package_outside_the_branch_is_parked() {
        assert_parked("cargo test -p nucleos-core", EMAIL);
        assert_parked("cargo check -p nucleos-core", BASE);
        assert_parked("cargo clippy --package nucleos-core", BASE);
        assert_parked("cargo test --package=nucleos-core", BASE);
        // A name the workspace does not have can only be outside the branch.
        assert_parked("cargo test -p serde", CORE);
        assert_parked("cargo test -p", CORE);
    }

    #[test]
    fn a_path_outside_the_branch_is_parked() {
        for cmd in [
            "go test -C sidecars/telegram ./...",
            "go test ./sidecars/telegram/...",
            "go test -C core ./...",
            // A shared name prefix is not containment: the boundary is a whole segment.
            "go test -C sidecars/emailx ./...",
            "go test ./sidecars/emailx/...",
            "go test -C /tmp/elsewhere ./...",
            "go test -C C:/elsewhere ./...",
            "go test -C",
        ] {
            assert_parked(cmd, EMAIL);
        }
    }

    #[test]
    fn every_scope_of_one_command_must_be_inside_the_branch() {
        assert_allowed("cargo test -p nucleos-core -p nucleos-base", CORE);
        assert_parked("cargo test -p nucleos-base -p nucleos-core", BASE);
        assert_parked(
            "cargo test -p nucleos-base --manifest-path core/Cargo.toml",
            BASE,
        );
        assert_allowed(
            "go test ./sidecars/email/... ./sidecars/telegram/...",
            &["sidecars/email/**", "sidecars/telegram/**"],
        );
        assert_parked(
            "go test ./sidecars/email/... ./sidecars/telegram/...",
            EMAIL,
        );
    }

    #[test]
    fn a_command_not_narrowed_to_the_branch_is_parked() {
        // With no scope, each of these runs over the whole workspace or the current directory.
        for cmd in [
            "cargo test",
            "cargo check",
            "cargo clippy --all-targets",
            "cargo test --workspace",
            "cargo test --all",
            "cargo test -p nucleos-core --workspace",
            "cargo test -p nucleos-core --all",
        ] {
            assert_parked(cmd, CORE);
        }
        for cmd in ["go test", "go test ./...", "go test ."] {
            assert_parked(cmd, EMAIL);
        }
        // A branch with no globs holds no path, so nothing can be inside it.
        assert_parked("cargo test -p nucleos-core", &[]);
        assert_parked("go test -C sidecars/email ./...", &[]);
    }

    #[test]
    fn command_separators_are_refused() {
        let separators = [
            "; whoami",
            ";whoami",
            ";",
            " && whoami",
            "&&whoami",
            " || whoami",
            " & whoami",
            "&",
            "\nwhoami",
            "\r\nwhoami",
        ];
        assert_suffixes_park("cargo test -p nucleos-core", &separators, CORE);
        assert_suffixes_park("go test -C sidecars/email ./...", &separators, EMAIL);
    }

    #[test]
    fn pipes_and_redirections_are_refused() {
        let plumbing = [
            " | tee out.txt",
            "|tee out.txt",
            " |& tee out.txt",
            " > out.txt",
            ">out.txt",
            " >> out.txt",
            " < in.txt",
            " 2> err.txt",
            " 2>&1",
        ];
        assert_suffixes_park("cargo test -p nucleos-core", &plumbing, CORE);
        assert_suffixes_park("go test -C sidecars/email ./...", &plumbing, EMAIL);
    }

    #[test]
    fn other_shell_metacharacters_are_refused_even_quoted() {
        let metacharacters = [
            " `whoami`",
            " $(whoami)",
            " $HOME",
            " %PATH%",
            " (whoami)",
            " {a,b}",
            " nucleos*",
            " nucleos?",
            " ~/x",
            " !x",
            " ^x",
            " # comment",
            " \"a;b\"",
            " 'a|b'",
            " \"$(whoami)\"",
        ];
        assert_suffixes_park("cargo test -p nucleos-core", &metacharacters, CORE);
        assert_suffixes_park("go test -C sidecars/email ./...", &metacharacters, EMAIL);
    }

    #[test]
    fn manifest_path_inside_the_branch_is_accepted() {
        for cmd in [
            "cargo test --manifest-path core/Cargo.toml",
            "cargo test --manifest-path=core/Cargo.toml",
            "cargo check --manifest-path core/crates/nucleos-base/Cargo.toml",
            "cargo clippy --manifest-path ./core/Cargo.toml",
            "cargo test -p nucleos-base --manifest-path core/Cargo.toml",
        ] {
            assert_allowed(cmd, CORE);
        }
        // The shell's package is outside the workspace, so a manifest is its only way in.
        assert_allowed(
            "cargo check --manifest-path shell/src-tauri/Cargo.toml",
            &["shell/src-tauri/**"],
        );
    }

    #[test]
    fn manifest_path_outside_the_branch_is_refused() {
        for cmd in [
            "cargo test --manifest-path shell/src-tauri/Cargo.toml",
            "cargo test --manifest-path=shell/src-tauri/Cargo.toml",
            "cargo check --manifest-path sidecars/email/Cargo.toml",
            "cargo clippy --manifest-path core-evil/Cargo.toml",
            // The root manifest is the whole workspace.
            "cargo test --manifest-path Cargo.toml",
            "cargo test --manifest-path /tmp/elsewhere/Cargo.toml",
            "cargo test --manifest-path C:/elsewhere/Cargo.toml",
            "cargo test --manifest-path //server/share/Cargo.toml",
            "cargo test --manifest-path",
            // An in-branch package does not vouch for the manifest beside it.
            "cargo test -p nucleos-core --manifest-path shell/src-tauri/Cargo.toml",
        ] {
            assert_parked(cmd, CORE);
        }
    }

    #[test]
    fn a_dot_dot_segment_never_passes_as_inside_the_branch() {
        for cmd in [
            "cargo test --manifest-path core/../shell/src-tauri/Cargo.toml",
            "cargo test --manifest-path=core/../shell/src-tauri/Cargo.toml",
            "cargo test --manifest-path ../elsewhere/Cargo.toml",
            "cargo test --manifest-path core/../../elsewhere/Cargo.toml",
            "cargo test --manifest-path core\\..\\..\\elsewhere\\Cargo.toml",
            // `normalise_glob` refuses `..` outright, even when the path would land back inside.
            "cargo test --manifest-path core/../core/Cargo.toml",
        ] {
            assert_parked(cmd, CORE);
        }
        for cmd in [
            "go test -C sidecars/email/../telegram ./...",
            "go test -C ../elsewhere ./...",
            "go test ./sidecars/email/../telegram/...",
            "go test ./sidecars/email/../../core/...",
            "go test ../elsewhere/...",
            // The path is relative to `-C`, which is inside; the result is not.
            "go test -C sidecars/email ../telegram/...",
        ] {
            assert_parked(cmd, EMAIL);
        }
    }

    #[test]
    fn commands_off_the_closed_list_are_parked_even_inside_the_branch() {
        for cmd in [
            "cargo build",
            "cargo build -p nucleos-core",
            "cargo run",
            "cargo run -p nucleos-core",
            "cargo fmt -p nucleos-core",
            "cargo install --path core",
            "cargo +nightly test -p nucleos-core",
            "cargo",
            // A prefix or a look-alike program is not on the list either.
            "RUSTFLAGS=-Dwarnings cargo test -p nucleos-core",
            "core/evil/cargo test -p nucleos-core",
            "cargo-evil test -p nucleos-core",
            "python scripts/heavy.py cargo test -p nucleos-core",
            "bash -c whoami",
            "whoami",
            "",
            "   ",
        ] {
            assert_parked(cmd, CORE);
        }
        for cmd in [
            "go build",
            "go build ./sidecars/email/...",
            "go build -C sidecars/email ./...",
            "go run ./sidecars/email",
            "go vet ./sidecars/email/...",
            "go generate ./sidecars/email/...",
            "go",
        ] {
            assert_parked(cmd, EMAIL);
        }
    }

    #[test]
    fn flags_that_run_another_program_or_write_elsewhere_are_parked() {
        for cmd in [
            "cargo test -p nucleos-core --config build.rustc-wrapper=whoami",
            "cargo --config build.rustc-wrapper=whoami test -p nucleos-core",
            "cargo test -p nucleos-core --target-dir elsewhere",
            "cargo clippy -p nucleos-core --fix",
        ] {
            assert_parked(cmd, CORE);
        }
        for cmd in [
            "go test -C sidecars/email -exec whoami ./...",
            "go test -C sidecars/email -toolexec whoami ./...",
            "go test -C sidecars/email -vettool=whoami ./...",
            "go test -C sidecars/email -o elsewhere ./...",
        ] {
            assert_parked(cmd, EMAIL);
        }
    }
}
