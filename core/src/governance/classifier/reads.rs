//! PURE: read-only shapes that need more than the normalized command to judge — the shell that will
//! run it, or the workspace it runs in.
//!
//! `grammar.rs` answers from the command text alone and serves every shell through
//! `is_safe_command`. These cannot: `git -C <dir>` is judged by where `<dir>` lands, which needs the
//! raw (unfolded) path and the run's `cwd`, and the PowerShell cmdlets below are PowerShell's, so
//! they are asked only when PowerShell is what runs the line — the same reason
//! `shapes_output_only` is kept out of `is_safe_command`.

use std::path::Path;

use super::grammar::{quoted_words, single_quoted, unquoted};
use super::paths::target_lands_inside;
use super::shell_shape::{is_safe_command, normalize_command, shell_form_is_readable};

/// `git -C <dir> <read>`: a git read run from a directory inside the workspace.
///
/// `-C` only says where git starts, so the line is exactly as safe as `cd <dir> && git <read>`,
/// which is already allowed when the `cd` lands inside — and this asks the containment question
/// `cd` is asked (`target_lands_inside`), never a second spelling of it. What follows `<dir>` must
/// itself be a compiled git read (`is_safe_command`); the queue's operations and `git add`/`commit`
/// get nothing from this, and a second `-C` or a `-c` config override is not a read at all.
///
/// Read off the RAW segment, and that is the point: normalizing lowercases, which folds `-C` into
/// `-c` — `git -c core.pager=<cmd> log` runs a program — and folds the path, which would widen the
/// workspace behind the containment check's back.
pub(super) fn reads_from_inside_the_workspace(
    segment: &str,
    cwd: Option<&Path>,
    shell: crate::command_reader::Shell,
) -> bool {
    let Some(cwd) = cwd else {
        return false;
    };
    let Some(words) = quoted_words(segment) else {
        return false;
    };
    let [git, flag, directory, _, ..] = &words[..] else {
        return false;
    };
    if !git.eq_ignore_ascii_case("git")
        || *flag != "-C"
        || !target_lands_inside(&unquoted(directory), cwd, shell)
    {
        return false;
    }
    // `quoted_words` hands back slices of `segment`, so the rest of the line is everything after
    // the directory's last byte, quotes and all.
    let after = directory.as_ptr() as usize - segment.as_ptr() as usize + directory.len();
    is_safe_command(&normalize_command(&format!("git{}", &segment[after..])))
}

/// PowerShell's own reads: `Test-Path`, `Get-ChildItem` (and `gci`), `Select-String` (and `sls`),
/// and a bare string literal, which PowerShell prints.
///
/// Every one of them is a read, and each is admitted only with LITERAL arguments: the whole segment
/// is refused as soon as it holds a `$` (a variable, or `$(...)`), a parenthesis (an expression
/// evaluated first), an `@` (a splat or an array), a brace (a script block) or a backtick (the
/// escape character). Without those PowerShell has nothing left to evaluate, so what the cmdlet is
/// given is what is written. Each cmdlet then takes only the parameters named below; an unknown
/// one, or a prefix of a known one (`-f`, which PowerShell would resolve), is refused.
///
/// A `|` inside a quoted pattern is NOT admitted, and not by choice: the PowerShell reading
/// (`command_reader::blind_walk`) is quote-blind and cuts the line there, which leaves each half
/// with an unclosed quote. `Select-String 'a|b'` keeps asking.
///
/// Read off the normalized command, which is right for PowerShell: its cmdlet and parameter names
/// are case-insensitive, and the paths it reads are on a case-insensitive file system.
pub(super) fn reads_in_powershell(command: &str) -> bool {
    if !shell_form_is_readable(command) || command.contains(['$', '(', ')', '@', '{', '}', '`']) {
        return false;
    }
    let Some(words) = quoted_words(command) else {
        return false;
    };
    match &words[..] {
        ["test-path", arguments @ ..] => tests_paths(arguments),
        ["get-childitem" | "gci", arguments @ ..] => lists_a_directory(arguments),
        ["select-string" | "sls", arguments @ ..] => selects_strings(arguments),
        [literal] => is_string_literal(literal),
        _ => false,
    }
}

/// `'---'` or `"-- specs"`: one quoted string and nothing else. The caller has already refused
/// `$` and the backtick, which are the only ways a double-quoted string can run anything.
fn is_string_literal(word: &str) -> bool {
    single_quoted(word).is_some()
        || word
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .is_some_and(|inner| !inner.contains('"'))
}

/// PURE: whether a word names only literal file-system paths (a comma-separated list allowed).
///
/// A `:` is admitted only as a drive letter (`C:/`, `C:\`): anywhere else it names a provider, and
/// `env:`, `HKLM:` and `cert:` are not files — `Get-ChildItem env:` prints every environment
/// variable, tokens included. A UNC path (`\\host\share`, `//host/share`) is refused because merely
/// touching one sends this machine's credentials to that host.
fn literal_paths(word: &str) -> bool {
    let value = unquoted(word);
    value.split(',').all(|path| {
        let colon_is_a_drive = |at: usize| {
            at == 1
                && path.as_bytes()[0].is_ascii_alphabetic()
                && matches!(path.as_bytes().get(2), Some(b'/' | b'\\'))
        };
        !path.is_empty()
            && !path.starts_with('-')
            && !path.starts_with("\\\\")
            && !path.starts_with("//")
            && path.match_indices(':').all(|(at, _)| colon_is_a_drive(at))
    })
}

/// `Test-Path <path>[,<path>...] [-PathType Leaf|Container|Any]`.
fn tests_paths(arguments: &[&str]) -> bool {
    let mut paths = 0;
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        let admitted = match *word {
            "-pathtype" => words
                .next()
                .is_some_and(|kind| matches!(*kind, "leaf" | "container" | "any")),
            "-path" | "-literalpath" => words.next().is_some_and(|path| literal_paths(path)),
            "-erroraction" | "-ea" => words.next().is_some_and(|action| quiet_on_error(action)),
            _ => {
                paths += 1;
                literal_paths(word)
            }
        };
        if !admitted {
            return false;
        }
    }
    paths <= 1
}

/// `Get-ChildItem [<path>] [-Force] [-Recurse] [-File] [-Directory] [-Name] [-Filter <name>]
/// [-Attributes <word>] [-Depth <n>] [-ErrorAction SilentlyContinue]`.
fn lists_a_directory(arguments: &[&str]) -> bool {
    let mut paths = 0;
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        let admitted = match *word {
            "-force" | "-recurse" | "-file" | "-directory" | "-name" | "-hidden" => true,
            "-filter" | "-path" | "-literalpath" => {
                words.next().is_some_and(|path| literal_paths(path))
            }
            "-attributes" => words.next().is_some_and(|attributes| {
                unquoted(attributes)
                    .chars()
                    .all(|c| c.is_ascii_alphabetic() || matches!(c, '!' | '+' | ','))
            }),
            "-depth" => words.next().is_some_and(|depth| is_a_number(depth)),
            "-erroraction" | "-ea" => words.next().is_some_and(|action| quiet_on_error(action)),
            _ => {
                paths += 1;
                literal_paths(word)
            }
        };
        if !admitted {
            return false;
        }
    }
    paths <= 1
}

/// `Select-String [-Pattern] <pattern> [[-Path] <path>] [-SimpleMatch] [-NotMatch]
/// [-CaseSensitive] [-List] [-Quiet] [-AllMatches] [-Context N[,N]]` — as a filter on a pipeline
/// or as a reader of one file.
///
/// The pattern may hold any regex character inside its quotes; it is matched, never run.
fn selects_strings(arguments: &[&str]) -> bool {
    let mut pattern = false;
    let mut path = false;
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        let admitted = match *word {
            "-simplematch" | "-notmatch" | "-casesensitive" | "-list" | "-quiet"
            | "-allmatches" => true,
            "-pattern" if !pattern => {
                pattern = true;
                words
                    .next()
                    .is_some_and(|value| !unquoted(value).starts_with('-'))
            }
            "-path" | "-literalpath" if !path => {
                path = true;
                words.next().is_some_and(|value| literal_paths(value))
            }
            "-context" => words
                .next()
                .is_some_and(|context| context.split(',').all(is_a_number)),
            "-erroraction" | "-ea" => words.next().is_some_and(|action| quiet_on_error(action)),
            _ if word.starts_with('-') => false,
            // Positional: the pattern first, then the path.
            _ if !pattern => {
                pattern = true;
                true
            }
            _ if !path => {
                path = true;
                literal_paths(word)
            }
            _ => false,
        };
        if !admitted {
            return false;
        }
    }
    pattern
}

fn is_a_number(word: &str) -> bool {
    !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_digit())
}

/// `-ErrorAction` values that only decide whether a failure is printed.
fn quiet_on_error(action: &str) -> bool {
    matches!(
        unquoted(action).as_str(),
        "silentlycontinue" | "ignore" | "continue" | "stop"
    )
}

#[cfg(test)]
mod tests {
    use super::super::{Unrecognized, classify};
    use serde_json::json;
    use std::path::Path;

    const WORKSPACE: &str = "C:/Projects/nucleos";

    /// The decision for one command line, judged from the workspace the blocked examples ran in,
    /// with nobody's policy and no confinement widening — so an `allow` here is this file's.
    fn decided(tool: &str, command: &str) -> String {
        classify(
            tool,
            &json!({ "command": command }),
            Some(Path::new(WORKSPACE)),
            &crate::integrations::github::Policy::empty(),
            &crate::project::policy::ShellRules::default(),
            Unrecognized::AsksAPerson,
        )
        .decision
        .decision
    }

    fn assert_allowed(tool: &str, commands: &[&str]) {
        let refused: Vec<(&str, String)> = commands
            .iter()
            .map(|command| (*command, decided(tool, command)))
            .filter(|(_, got)| got != "allow")
            .collect();
        assert!(
            refused.is_empty(),
            "{tool}: expected allow, got: {refused:?}"
        );
    }

    /// Refusals are asserted as "not allowed" rather than as one decision: a line that also trips a
    /// compiled refusal (`rm -rf` inside an awk string) is denied, and either answer is right here.
    fn assert_not_allowed(tool: &str, commands: &[&str]) {
        let allowed: Vec<&str> = commands
            .iter()
            .copied()
            .filter(|command| decided(tool, command) == "allow")
            .collect();
        assert!(
            allowed.is_empty(),
            "{tool}: allowed, and must not be: {allowed:?}"
        );
    }

    #[test]
    fn uniq_with_one_operand_at_most_is_a_read() {
        assert_allowed(
            "Bash",
            &[
                "uniq -c",
                "sort f | uniq -c | sort -rn",
                "uniq -d f",
                "uniq -ci f",
                "uniq --count --ignore-case f",
                "uniq f",
            ],
        );
        assert_not_allowed(
            "Bash",
            &[
                // The second operand is an output file.
                "uniq a b",
                "uniq -c a b",
                // `-f` takes a value, and an unknown flag might.
                "uniq -f 1 a",
                "uniq --output a",
                "uniq $f",
                "uniq a '-c' b",
            ],
        );
    }

    #[test]
    fn git_branch_lists_by_filter_and_never_creates_or_deletes() {
        assert_allowed(
            "Bash",
            &[
                "git branch --no-merged master",
                "git branch --merged",
                "git branch --merged=master",
                "git branch --contains HEAD",
                "git branch -a --no-merged master",
                "git branch -vv --list 'fix/*'",
                "git branch --list fix/x feat/y",
                "git branch -r -v",
                "git branch --format='%(refname:short)' --merged master",
                "git worktree list",
                "git worktree list --porcelain",
                "git merge-base master HEAD",
                "git merge-base --is-ancestor master HEAD",
            ],
        );
        assert_not_allowed(
            "Bash",
            &[
                "git branch -D x",
                "git branch -d x",
                "git branch -d --merged x",
                "git branch newname",
                "git branch -v newname",
                "git branch -m a b",
                "git branch -c a b",
                "git branch -f x HEAD",
                "git branch -u origin/x",
                "git branch --set-upstream-to=origin/x",
                "git branch -l x",
                "git branch --format x",
                "git branch --list $x",
            ],
        );
    }

    #[test]
    fn git_dash_c_reads_only_from_inside_the_workspace() {
        assert_allowed(
            "Bash",
            &[
                "git -C C:/Projects/nucleos worktree list",
                "git -C core log --oneline -5",
                "git -C /c/Projects/nucleos/core status --short",
                "git -C . branch --no-merged master",
            ],
        );
        assert_allowed("PowerShell", &[r"git -C C:\Projects\nucleos worktree list"]);
        assert_not_allowed(
            "Bash",
            &[
                "git -C C:/Windows status",
                "git -C .. status",
                "git -C ~ status",
                "git -C $HOME status",
                // A read only by its first word.
                "git -C . add x",
                "git -C . commit -m x",
                "git -C . branch -D x",
                "git -C . -c core.pager=x log",
                "git -C . -C C:/Windows status",
                // Lowercase `-c` is a config override, not a directory.
                "git -c core.pager=x log",
            ],
        );
        // A queue operation behind `-C` is not a read either.
        assert_not_allowed("Bash", &["git -C . push origin master", "git -C . merge x"]);
    }

    #[test]
    fn sed_prints_several_addresses_and_regex_ranges() {
        assert_allowed(
            "Bash",
            &[
                "sed -n '1,45p;60,140p' core/src/http.rs",
                r"sed -n '/#\[cfg(test)\]/,$p' core/src/map/states.rs",
                "sed -n '/^## A/,/^## B/p' f",
                "sed -n '/fn main/,+20p' f",
                "sed -n '10,/^}/p' f",
                "sed -n '1p;5p;$p' f",
                // The `;w` is inside the regex, where it is text to match.
                "sed -n '/a;w out/,$p' f",
                // Unchanged from before.
                "sed -n '1,40p' Cargo.toml",
                "sed -n 1,40p Cargo.toml",
            ],
        );
        assert_not_allowed(
            "Bash",
            &[
                "sed -n '1p;w out' f",
                "sed -n '1,5p;w out' f",
                "sed -n '/a/,/b/w out' f",
                "sed -n '/a/,$p;1e id' f",
                "sed -n '/[/]/,$p' f",
                "sed -n '/a/,/b/ip' f",
                "sed -n '1,5p;' f",
                "sed -n '/x/r /etc/passwd' f",
                "sed -n 's/a/b/w out' f",
                "sed -i '1d' f",
                // A quoted option is still an option: this edits `f` in place.
                "sed -n 1p f '-i'",
                // `$` is the shell's outside single quotes.
                r#"sed -n "/a/,$p" f"#,
                // A lone regex address is `grep`, and stays unchanged.
                "sed -n '/fn main/p' f",
            ],
        );
    }

    #[test]
    fn awk_prints_by_pattern_and_never_acts() {
        assert_allowed(
            "Bash",
            &[
                "awk 'NR>=408' src/embed.rs",
                "awk 'NR==1450,NR==1600' core/src/runs.rs",
                "awk 'NR>=10 && NR<=20' f",
                "awk 'NR<5'",
                "awk '/^## Sandbox: pool de repositórios/,/^## [^S]/' docs/decisions.md",
                "awk '/start/,0' f",
                "awk '/[[:space:]]x/' f",
            ],
        );
        assert_not_allowed(
            "Bash",
            &[
                r#"awk '{print > "x"}' f"#,
                r#"awk 'BEGIN{system("rm -rf x")}'"#,
                "awk -v a=1 'NR>a' f",
                "awk -f prog.awk f",
                "awk -i inplace 'NR>1' f",
                "awk 'NR>1' a=1",
                "awk 'NR>1' a b",
                r#"awk "NR>1" f"#,
                "awk 'NR>1 {print}' f",
                r#"awk 'NR>1 || system("id")' f"#,
                "awk 'ENVIRON' f",
                r#"awk '"cmd" | getline' f"#,
                "awk '/a/ ~ /b/' f",
                "awk NR>1 f",
            ],
        );
    }

    #[test]
    fn powershell_reads_with_literal_arguments_are_allowed() {
        assert_allowed(
            "PowerShell",
            &[
                "Test-Path C:/Projects/.nucleos-run/nucleos-core.exe",
                "Test-Path a.txt,b.txt -PathType Leaf",
                "Get-ChildItem docs/plans",
                "Get-ChildItem node_modules -Force",
                "Get-ChildItem docs/local/loops -ErrorAction SilentlyContinue",
                "gci core -Recurse -Filter *.rs -Name",
                "Select-String 'run-9006'",
                "Select-String -Path docs/memory.md -Pattern 'jev-compaction'",
                r"git worktree list | Select-String '\[master\]'",
                "Select-String -Pattern 'x' -Path f -SimpleMatch -Context 2,3",
                // Not `"== specs"`: the segmenter drops a leading word holding `=` as an
                // environment assignment, quotes or not, so that line still asks.
                r#""-- specs""#,
                "'---'",
            ],
        );
        assert_not_allowed(
            "PowerShell",
            &[
                "Test-Path $p",
                "Test-Path env:GITHUB_TOKEN",
                r"Test-Path \\host\share",
                "Get-ChildItem $p -Force",
                "Get-ChildItem env:",
                "Get-ChildItem a b",
                "Get-ChildItem -f x",
                "Get-ChildItem x | Remove-Item",
                r#"Select-String "$x""#,
                "Select-String 'a|b' f",
                "Select-String 'a' f g",
                "Select-String 'a' -Path f -Encoding x",
                r#""$(Remove-Item x)""#,
                r#""a" "b""#,
                "Get-ChildItem @args",
                "Test-Path (Get-Item x)",
            ],
        );
        // PowerShell's cmdlets are PowerShell's: under Bash these names are unknown programs.
        assert_not_allowed("Bash", &["Test-Path x", "Select-String 'a'"]);
    }

    /// The compound line that prompted all of this, spelled as it was run.
    #[test]
    fn a_compound_powershell_read_from_the_workspace_is_allowed() {
        assert_eq!(
            decided(
                "PowerShell",
                r#"Set-Location C:\Projects\nucleos; git status --short; "---"; git worktree list | Select-String '\[master\]'"#,
            ),
            "allow"
        );
        assert_eq!(
            decided(
                "PowerShell",
                r#"Set-Location C:\Windows; git status --short; "---""#,
            ),
            "pending_approval"
        );
    }
}
