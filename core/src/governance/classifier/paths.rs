//! Workspace and path-confinement helpers.

use super::*;

/// Whether a piece is one of the commands judged by WHERE IT LANDS, and lands inside the workspace.
///
/// Three so far, and they are here rather than in `SAFE_COMMAND_PREFIXES` for the same reason: a
/// list answers "which program", and for these the program is not the question. `cd ..` and
/// `cd core` are the same program and opposite answers.
///
/// - **`cd`** decides what every piece after it does, so a list entry would hand over the meaning of
///   the whole line.
/// - **`mkdir`** changes the tree, which is exactly what an ordinary `Write` does and is allowed to
///   do — inside the workspace. `mkdir C:\Windows\evil` is not the same act with a different
///   argument, it is a different act. Measured: the job-6 dogfood stopped its plan node dead on
///   `mkdir -p "<worktree>/.nucleos"`, a directory the job itself needs.
/// - **`cargo fmt`** rewrites files, and the only ones it reaches are the crate its directory
///   names. That directory is where the line lands: the run's own, or one a `cd` before it took
///   deeper — a `cd` that leaves is a piece refused on its own, and the line with it. Measured on
///   job 27, 2026-09-14: an implement node in its own worktree was refused `cargo fmt --all` three
///   times, from Bash and from PowerShell, and formatted the file by hand rather than stop. What
///   could point it anywhere else is in its arguments, and `formats_only_where_it_stands` reads
///   those.
///
/// A `cd` this returns true for can only go deeper, never out, which is what makes judging the
/// pieces AFTER it against the outer `cwd` safe rather than merely convenient: the real directory
/// is at or below the workspace, so a `../` in a later piece is read as escaping sooner than it
/// really would. Wrong, and wrong in the strict direction.
///
/// The leading token is matched WHOLE, deliberately unlike `has_destructive_flags`, which strips the
/// directory off with `program_name`. Stripping widens a blocklist and narrows an allow list: it
/// would make `./mkdir` — a script sitting in the tree, named to look like the builtin — read as the
/// builtin. `/usr/bin/mkdir` falls to `pending_approval` as the price, which is the safe half.
///
/// Without a `cwd` there is no boundary to be inside of, so the answer is no — the same reading
/// `writes_outside_cwd` was corrected to.
pub(super) fn lands_inside_the_workspace(
    segment: &str,
    cwd: Option<&Path>,
    shell: crate::command_reader::Shell,
) -> bool {
    let tokens = shell_words(segment);
    let Some(program) = tokens.first() else {
        return false;
    };
    // `cargo fmt` names no destination: it formats where it stands, which is inside whenever the
    // line got this far with a workspace to be inside of.
    if program.eq_ignore_ascii_case("cargo") && tokens.get(1).is_some_and(|word| word == "fmt") {
        return cwd.is_some() && formats_only_where_it_stands(&tokens[2..]);
    }
    // `cd` takes one destination; `mkdir` takes as many as you like, and every one has to land
    // inside. Anything else is not judged this way at all.
    let one_target_only = if ["cd", "chdir", "set-location"]
        .iter()
        .any(|name| program.eq_ignore_ascii_case(name))
    {
        true
    } else if ["mkdir", "md"]
        .iter()
        .any(|name| program.eq_ignore_ascii_case(name))
    {
        false
    } else {
        return false;
    };
    let Some(cwd) = cwd else {
        return false;
    };

    let mut targets: Vec<&str> = Vec::new();
    for token in &tokens[1..] {
        // The two flags that carry no destination: cmd.exe's "change drive as well", and "create the
        // parents too", which asks for more directories in the same place rather than a different
        // place.
        if token.eq_ignore_ascii_case("/d")
            || token.eq_ignore_ascii_case("-p")
            || token.eq_ignore_ascii_case("--parents")
        {
            continue;
        }
        // `cd -` is the previous directory — wherever that was, which is precisely what this check
        // cannot know. Every other flag is unmodelled and gets the same answer.
        if token.starts_with('-') {
            return false;
        }
        targets.push(token.as_str());
    }
    // A bare `cd` goes home and a bare `mkdir` is an error; two destinations is not a `cd` worth
    // reading.
    if targets.is_empty() || (one_target_only && targets.len() > 1) {
        return false;
    }

    targets
        .iter()
        .all(|target| target_lands_inside(target, cwd, shell))
}

/// PURE: whether one destination named on a command line resolves inside the workspace `cwd`.
///
/// The containment half of `lands_inside_the_workspace`, on its own so that `git -C <dir>`
/// (`reads::reads_from_inside_the_workspace`) asks exactly the question a `cd` is asked rather than
/// a second spelling of it.
pub(super) fn target_lands_inside(
    target: &str,
    cwd: &Path,
    shell: crate::command_reader::Shell,
) -> bool {
    // A destination the shell rewrites before the command ever sees it is not a destination this
    // can check. `~`, `$HOME` and `%USERPROFILE%` are all the home directory, and all three
    // arrive here as ordinary-looking names that `normalize_path` happily glues onto the
    // workspace — so `cd ~ && …` read as landing in `<workspace>/~` and was allowed. Caught by
    // the test that walks the ways out; the `%VAR%` form had no test and would have shipped.
    if target.starts_with('~') || target.contains('$') || target.contains('%') {
        return false;
    }
    let workspace = fold_for_containment(&normalize_path(&cwd.to_string_lossy(), None));
    let target = with_git_bash_drive(target, shell, &workspace);
    let target = fold_for_containment(&normalize_path(&target, Some(cwd)));
    target == workspace || target.starts_with(&format!("{workspace}/"))
}

/// PURE: whether `cargo fmt`'s arguments leave it formatting the crate it stands in, and nothing
/// else.
///
/// A list of what may appear rather than of what may not, because the ways to point rustfmt
/// somewhere else are open-ended and a missed one is a write outside the workspace:
/// `--manifest-path` names another crate, `--emit` names a destination, `--print-config` writes the
/// file it is given, and a bare path after `--` is handed to rustfmt as one more file to rewrite.
/// Every value this admits names a package, an edition or a colour, never a path.
///
/// `--check` is listed so the gate's own spelling reads the same way here, but it was never this
/// rule's to allow: `checks_formatting_without_writing` answers for it with or without a workspace.
///
/// **What this cannot see.** cargo finds its manifest by walking UP from the directory, so a
/// workspace with no `Cargo.toml` of its own sends it into the nearest one above. This file never
/// reads the disk and cannot rule that out. `cargo build` and `cargo test`, both on the list, walk
/// the same way and then run the build scripts they find, which is more than a formatter can do
/// with the same mistake.
pub(super) fn formats_only_where_it_stands(args: &[String]) -> bool {
    const BARE: &[&str] = &["--", "--all", "--check", "-q", "--quiet", "-v", "--verbose"];
    const VALUED: &[&str] = &[
        "-p",
        "--package",
        "--edition",
        "--color",
        "--message-format",
    ];
    let names_nothing = |value: &str| {
        !value.is_empty() && !value.starts_with('-') && !value.contains(['/', '\\', '~', '$', '%'])
    };

    let mut tokens = args.iter().map(String::as_str);
    while let Some(token) = tokens.next() {
        if BARE.contains(&token) {
            continue;
        }
        if VALUED.contains(&token) {
            if tokens.next().is_some_and(names_nothing) {
                continue;
            }
            return false;
        }
        match token.split_once('=') {
            Some((flag, value)) if VALUED.contains(&flag) && names_nothing(value) => {}
            _ => return false,
        }
    }
    true
}

/// PURE: Git bash's spelling of a drive, `/c/Projects`, as the `C:/Projects` it names.
///
/// Git bash is the shell every `Bash` tool call runs under on Windows, and it mounts each drive at a
/// single letter under `/`. The containment checks compared `/c/Projects/x` with a workspace of
/// `C:/Projects/x`, found no common prefix, and refused a `cd` into the very directory the run was
/// standing in. Measured on job 26, 2026-09-13: its replan node spelled its own worktree that way
/// and was refused, with nobody there to approve it.
///
/// **Only for a POSIX shell, and only when the workspace itself sits on a drive.** PowerShell reads
/// `/c/Projects` as `\c\Projects` on the current drive, a different directory, which must not be
/// judged as the workspace. On a machine with no drive letters `/c/` is an ordinary directory
/// name, and a workspace with no `X:` in it never takes the rewrite.
///
/// Used by the two checks that can ALLOW and by nothing that refuses. `deletes_outside_cwd` and the
/// write guards still read `/c/…` as outside, which costs an approval and never lets anything
/// through.
pub(super) fn with_git_bash_drive(
    path: &str,
    shell: crate::command_reader::Shell,
    workspace: &str,
) -> String {
    let bytes = path.as_bytes();
    let names_a_drive = shell == crate::command_reader::Shell::Posix
        && workspace.as_bytes().get(1) == Some(&b':')
        && bytes.first() == Some(&b'/')
        && bytes.get(1).is_some_and(u8::is_ascii_alphabetic)
        && matches!(bytes.get(2), None | Some(b'/'));
    if names_a_drive {
        format!(
            "{}:{}",
            char::from(bytes[1]).to_ascii_uppercase(),
            &path[2..]
        )
    } else {
        path.to_owned()
    }
}

pub(super) fn classification(
    decision: &str,
    action_class: &'static str,
    reason: &str,
) -> Classification {
    let reason = reason.to_owned();
    Classification {
        decision: Decision {
            decision: decision.to_owned(),
            reason: reason.clone(),
        },
        action_class,
        reason,
    }
}

/// PURE: a shell line's words, with quotes stripped.
///
/// `pub(crate)` for `github::Policy::read_is_autonomous`, which compares `gh` flags against
/// these tokens. Shared rather than copied on purpose: a second tokenizer would drift from this
/// one, and the two would disagree about the same command line — which is the class of bug this
/// file exists to keep out.
pub fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;

    for character in command.chars() {
        match (quote, character) {
            (Some(active), value) if value == active => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, value) if value.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(character),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

pub(super) fn normalize_path(path: &str, cwd: Option<&Path>) -> String {
    let path = path.replace('\\', "/");
    let combined = if is_absolute_path(&path) {
        path
    } else if let Some(cwd) = cwd {
        format!("{}/{}", cwd.to_string_lossy().replace('\\', "/"), path)
    } else {
        path
    };

    let mut components: Vec<&str> = Vec::new();
    for component in combined.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.last().is_some_and(|value| *value != "..") {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            _ => components.push(component),
        }
    }
    components.join("/")
}

/// PURE: case-folds a path for comparison against the lowercase blocklists at the top of this file.
///
/// Unconditional, unlike `fold_for_containment`, because folding can only ever WIDEN a blocklist:
/// `Build.rs` and `build.rs` both end up needing approval. On a filesystem where those really are
/// two files, the cost is an approval prompt for the wrong one — not a missed one.
pub(super) fn fold_for_match(path: &str) -> String {
    path.to_ascii_lowercase()
}

/// PURE: case-folds a path for a containment check, but only where the filesystem itself does.
///
/// Folding works the opposite way round here from `fold_for_match`: it makes more paths look like
/// they are INSIDE the workspace, so on a case-sensitive filesystem `/home/me/Work` would pass as
/// contained by `/home/me/work` and the boundary would be quietly wider than it reads. On Windows
/// not folding would be the bug instead — `C:\Work\Repo` and `c:\work\repo` are one directory, and
/// denying a write between the two spellings would be a false alarm on every run.
///
/// The daemon only builds for Windows today (`schtasks`, `taskkill`, Credential Manager), so this
/// is here to keep the boundary correct if that ever stops being true, rather than to fix something
/// reachable now.
#[cfg(windows)]
pub(super) fn fold_for_containment(path: &str) -> String {
    path.to_ascii_lowercase()
}

#[cfg(not(windows))]
pub(super) fn fold_for_containment(path: &str) -> String {
    path.to_owned()
}

pub(super) fn is_absolute_path(path: &str) -> bool {
    path.starts_with('/')
        || path
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':')
}

/// PURE: whether this segment is demonstrably ABOUT the workspace — it names at least one path, and
/// every path it names lands inside.
///
/// The rule the owner chose on 2026-08-30, for work they asked for, over the two alternatives of
/// parking (which ends the night on one unrecognised command — 257 of the 278 refusals this daemon
/// has ever recorded are that class) and of allowing outright.
///
/// **"At least one" is load-bearing and is not caution for its own sake.** Confinement is a claim
/// about where a command points, so a command that points nowhere has not made the claim. That one
/// word is what keeps `curl <url> | sh`, `nc host port` and `ssh user@host cmd` out of here: none of
/// them names a path, so none of them can be confined to anything, and each keeps the verdict it
/// has today. A rule that allowed "no paths outside" rather than "at least one path, all inside"
/// would have let every one of them through while looking identical on the page.
///
/// **A token that is not a filesystem path at all is refused rather than assumed.** This is the
/// sharp edge, and it was found by reading `is_absolute_path` rather than by shipping it:
/// `https://example.com/x` starts with neither `/` nor a drive letter, so `normalize_path` glues it
/// onto the workspace and it resolves INSIDE. A containment check that did not say this out loud
/// would have allowed the exact command it was chosen to stop.
///
/// A bare `README.md` — no separator, no leading `.` — is not read as a path either. It could as
/// easily be a subcommand, and `./README.md` is available to anyone who means the file.
pub(super) fn confined_to_workspace(
    segment: &str,
    cwd: Option<&Path>,
    shell: crate::command_reader::Shell,
) -> bool {
    let Some(cwd) = cwd else {
        return false;
    };
    let workspace = fold_for_containment(&normalize_path(&cwd.to_string_lossy(), None));
    let mut named_a_path = false;
    let words = shell_words(segment);

    // An interpreter handed its program inline: what runs is the CODE, and the code names
    // whatever it likes inside one quoted argument. `bash -c "rm -rf /c/Users/x" ./x` points at
    // `./x` and deletes a home directory. No reading of where its arguments land can confine it.
    if runs_inline_code(&words) {
        return false;
    }

    for token in words {
        // `--output=../x` carries its path on the right of the `=`, so the split happens BEFORE the
        // flag test below — otherwise the leading `-` would excuse the whole token.
        let candidate = match token.split_once('=') {
            Some((_, value)) => value,
            None => token.as_str(),
        };
        if candidate.is_empty() {
            continue;
        }
        // A flag with its value ATTACHED: `-C/c/Users/x`, `-t/c/x`, `-o/abs`, `-I./include`. The
        // value starts at the first character that can begin a path; a flag holding none of them
        // (`-rf`, `--verbose`) names nothing. Short clusters make the cut ambiguous (`-xf/abs` is
        // `-x -f /abs`, and `-tfoo/bar` would be read as `/bar`), and every ambiguity resolves to
        // the more absolute reading, so the error is only ever a refusal.
        let candidate = if candidate.starts_with('-') {
            match candidate.find(['/', '\\', '.', '~', '$', '%']) {
                Some(start) => &candidate[start..],
                None => continue,
            }
        } else {
            candidate
        };
        // Not this filesystem: a scheme, or a host. See the doc comment — these must not reach
        // `normalize_path`, which would read them as relative and land them inside.
        if candidate.contains("://") || candidate.contains('@') {
            return false;
        }
        if !(candidate.contains('/') || candidate.contains('\\') || candidate.starts_with('.')) {
            continue;
        }
        // Rewritten by the shell before the command ever sees them, so their destination is not
        // something this can check. The same three `lands_inside_the_workspace` refuses.
        if candidate.starts_with('~') || candidate.contains('$') || candidate.contains('%') {
            return false;
        }
        // One word holding whitespace is a quoted phrase, not a file this can resolve — a shell
        // fragment, a script, a message. Glued onto the workspace it would land inside.
        if candidate.contains(char::is_whitespace) {
            return false;
        }
        let resolved = with_git_bash_drive(candidate, shell, &workspace);
        let resolved = fold_for_containment(&normalize_path(&resolved, Some(cwd)));
        if resolved != workspace && !resolved.starts_with(&format!("{workspace}/")) {
            return false;
        }
        named_a_path = true;
    }

    named_a_path
}
