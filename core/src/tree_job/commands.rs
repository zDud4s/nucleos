//! The closed list of heavy commands the daemon runs for a leaf, scoped to its branch (R5).

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
