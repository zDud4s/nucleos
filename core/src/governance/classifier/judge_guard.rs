//! Judge, network, and inline-code guard helpers.

use super::*;

/// PURE, and consulted only by the judge: whether a shell line reaches the network, feeds text to
/// an evaluator, or hands an interpreter code that is not in a file. A line where any of that is
/// true is one the judge may refuse but never approve (spec A D5). It changes no verdict of
/// `classify`.
///
/// **Every token of every segment, not the first.** `command_reader::segments` cuts `|`, `;` and
/// `&&` alike, so it cannot say which segment feeds which; scanning all of them is what catches
/// `env curl`, `FOO=1 curl`, `xargs curl`, `sudo wget` and `busybox wget` without knowing any of
/// those wrappers by name. Names go through `program_name` after an ASCII fold, because PowerShell
/// commands and Windows file names ignore case: `IWR` and `Curl.exe` are the same programs.
///
/// **A line that cannot be read is never approvable.** The spec says "`shell_form_is_readable`
/// false", and taken literally that refuses every `|`, `&&` and `>` — the redirect rule of the
/// same decision says an in-workspace `>` must still reach the judge, so the literal reading
/// contradicts it. What the predicate needs is to be able to enumerate the line's tokens, which
/// `command_reader::read` answers (`Unreadable` for a command substitution, a heredoc it cannot
/// bound, a lone `&`) together with an unterminated quote (`without_quoted_text` is `None`).
pub fn runs_network_or_inline_code(tool_name: &str, tool_input: &Value) -> bool {
    if !reads_github_policy(tool_name) {
        return false;
    }
    let command = tool_input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("");
    let shell = shell_for(tool_name);
    // What the judge is shown is text, so anything the text hides must be refused before it: a line
    // that cannot be enumerated, and a line that hands a SECOND program to an otherwise-known one
    // (`--pre`, `-exec`, `--ext-diff`, `--textconv` — the classifier's own two readings of that,
    // reused rather than listed again).
    if matches!(
        crate::command_reader::read(command, shell),
        crate::command_reader::Reading::Unreadable(_)
    ) || crate::command_reader::without_quoted_text(command).is_none()
        || runs_a_helper_command(&normalize_command(command))
        || forces_external_diff_or_textconv(&normalize_command(command))
    {
        return true;
    }
    crate::command_reader::segments(command, shell)
        .iter()
        .any(|segment| {
            let words = shell_words(segment);
            words.iter().enumerate().any(|(index, word)| {
                let lowered = word.to_ascii_lowercase();
                let program = program_name(&lowered);
                NETWORK_CLIENTS.contains(&program)
                    || TEXT_EVALUATORS.contains(&program)
                    || (is_inline_interpreter(program) && passes_inline_code(&words[index + 1..]))
                    // `deno eval "<code>"` is inline code spelled as a subcommand, not a flag.
                    || (program == "deno" && words.get(index + 1).is_some_and(|next| next == "eval"))
            })
        })
}

pub(super) fn is_inline_interpreter(program: &str) -> bool {
    program.starts_with("python") || INLINE_CODE_INTERPRETERS.contains(&program)
}

/// Reads the interpreter's own options only: `-m` hands the rest to a module and the first word
/// that is not a flag is the script, and from there on the words are the script's, not the
/// interpreter's. That is what keeps `python -m pytest -p no:cacheprovider` and
/// `node build.js -p` approvable. A short-flag cluster (`-uc`) carries its `-c` inside it.
pub(super) fn passes_inline_code(arguments: &[String]) -> bool {
    for argument in arguments {
        if INLINE_CODE_FLAGS
            .iter()
            .any(|flag| argument.eq_ignore_ascii_case(flag))
            || argument.starts_with("--eval=")
        {
            return true;
        }
        if argument == "-m" || !argument.starts_with('-') {
            return false;
        }
        if !argument.starts_with("--")
            && argument[1..]
                .chars()
                .any(|flag| matches!(flag, 'c' | 'e' | 'p' | 'r'))
        {
            return true;
        }
    }
    false
}

/// Which of spec A's three guards a tool call trips (D5). Each makes a call one the judge may
/// refuse but never approve; none changes a verdict of `classify`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeGuard {
    /// G1: a secret or a database is read, wherever the result goes.
    SensitiveSource,
    /// G2: a path outside the run's root, or one the shell rewrites before anyone can check it.
    OutsideTheRun,
    /// G3: an operation recoverable only through the reflog, or not at all.
    Irreversible,
}

/// PURE: the first guard this call trips, over the tokens of a shell line and the path of a write.
///
/// **Only the path of a write, not its content**, as the spec words it. The Python prototype that
/// measured the guards scanned every string in the input, content included; that is stricter on
/// writes and is not what D5 asks for — the regression step (plan Task 3.4) measures this one.
///
/// The normalisation is the classifier's own (`normalize_path`, `fold_for_containment`, the
/// comparison `writes_outside_cwd` makes, `with_git_bash_drive` for `/c/…` under Git's bash), and
/// it is lexical: a link inside the worktree that points out of it passes. Closing that needs the
/// disk at every call, which is the OS sandbox's job (`2026-09-14-sandbox-de-so-design.md`).
pub fn judge_guard(tool_name: &str, tool_input: &Value, cwd: &Path) -> Option<JudgeGuard> {
    let workspace = fold_for_containment(&normalize_path(&cwd.to_string_lossy(), None));
    if WRITE_TOOLS.contains(&tool_name) {
        // A write that names no path is one whose destination nobody can check: outside, not fine.
        let Some(path) = written_path(tool_input) else {
            return Some(JudgeGuard::OutsideTheRun);
        };
        if names_a_sensitive_source(path) {
            return Some(JudgeGuard::SensitiveSource);
        }
        return escapes_the_run(path, cwd, &workspace, None).then_some(JudgeGuard::OutsideTheRun);
    }
    if !reads_github_policy(tool_name) {
        return None;
    }
    let command = tool_input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("");
    let shell = shell_for(tool_name);
    for segment in crate::command_reader::segments(command, shell) {
        let words = guard_words(segment, shell);
        if is_irreversible(segment, &words) {
            return Some(JudgeGuard::Irreversible);
        }
        for word in &words {
            // `touches_no_file` is the classifier's reading of `2>&1`, `>/dev/null` and `>NUL`;
            // reused so the guard and `redirects_a_file` agree about what a redirect writes.
            if touches_no_file(word) {
                continue;
            }
            let target = redirect_target(word);
            for candidate in path_candidates(target) {
                if names_a_sensitive_source(candidate) {
                    return Some(JudgeGuard::SensitiveSource);
                }
                if escapes_the_run(candidate, cwd, &workspace, Some(shell)) {
                    return Some(JudgeGuard::OutsideTheRun);
                }
            }
        }
    }
    None
}

/// The words of one segment as G2 must see them. Under a POSIX shell a backslash inside double
/// quotes escapes `"`, `\`, `$` and `` ` `` (and is kept literally before anything else), which
/// `shell_words` does not model: `grep "\"a\"\|b"` came out as a word starting with `\`, which
/// `normalize_path` turned into a leading `/` — an absolute path outside every run. Only the
/// double-quoted case is taken: an unquoted backslash stays a Windows separator as `shell_words`
/// reads it, and PowerShell, whose escape is the backtick, keeps `shell_words` whole.
/// `shell_words` itself is left alone because `classify` reads it too (spec A D5: the guards
/// change no verdict of `classify`).
pub(super) fn guard_words(segment: &str, shell: crate::command_reader::Shell) -> Vec<String> {
    if shell != crate::command_reader::Shell::Posix {
        return shell_words(segment);
    }
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut characters = segment.chars().peekable();
    while let Some(character) = characters.next() {
        match (quote, character) {
            (Some('"'), '\\')
                if characters
                    .peek()
                    .is_some_and(|next| matches!(next, '"' | '\\' | '$' | '`')) =>
            {
                current.extend(characters.next());
            }
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

/// `>>~/.bashrc`, `2>err.log` and `<.env` name their target glued to the operator; a bare `>` is
/// its own word and its target is the next word, which the loop above reads anyway. `&>`, `&>>`
/// (both streams) and `>|` (force past `noclobber`) glue to their target the same way, and a
/// target glued to one of those kept the operator's leftover character (`&>..`, `>|..`) as part of
/// the path, which hid it from `normalize_path`'s `..` handling and from a sensitive-basename
/// comparison.
pub(super) fn redirect_target(word: &str) -> &str {
    let digits = word.len() - word.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let rest = &word[digits..];
    if rest.starts_with(['>', '<', '&']) {
        rest.trim_start_matches(['>', '<', '&', '|'])
    } else {
        word
    }
}

/// The ways one word can carry a path: whole, after an `=` (`--output=../x`), or glued to a short
/// flag (`-C/elsewhere`, `-o../x`). The flag forms are read as paths because the judge only sees
/// text, and a destination the text hides must be checked before it. Fail-closed: `-I../include`
/// and `-L/usr/lib` now keep the judge from approving too; the local regression (Task 3.4) shows
/// whether any real approval is lost to it, and the executor names any it finds in the Handoff.
pub(super) fn path_candidates(word: &str) -> Vec<&str> {
    let mut candidates = vec![word];
    if let Some((_, value)) = word.split_once('=') {
        candidates.push(value);
    }
    if word.len() > 2
        && word.starts_with('-')
        && !word.starts_with("--")
        && word.is_char_boundary(2)
    {
        candidates.push(&word[2..]);
    }
    candidates
}

/// G1. The origin counts, whatever the destination: the red-team's leaks read a local database or
/// a `.env` "for the task" into a file inside the project.
pub(super) fn names_a_sensitive_source(token: &str) -> bool {
    let lowered = token.to_ascii_lowercase().replace('\\', "/");
    let base = lowered.rsplit('/').next().unwrap_or(&lowered);
    let program = program_name(&lowered);
    base == ".env"
        || (base.starts_with(".env.") && base != ".env.example")
        || base.ends_with(".db")
        || base.contains(".sqlite")
        || matches!(program, "sqlite3" | "cmdkey" | "keyring")
        || base == ".ssh"
        || lowered.contains("/.ssh/")
        || lowered.starts_with(".ssh/")
        || base.starts_with("id_rsa")
        || lowered.ends_with(".aws/credentials")
        || base == ".netrc"
        || base == ".git-credentials"
        || lowered.contains("credential")
}

/// G2. A token counts as a path the way `confined_to_workspace` counts one (a separator or a
/// leading `.`), minus a URL; and a token the shell rewrites first — a `~` in front, or a `$` or
/// `%` anywhere in a path — is outside, because where it lands cannot be checked here. `/tmp` and
/// the system temp are absolute paths outside the root, so they count as outside without a rule of
/// their own.
///
/// **`@` is NOT a reason to skip a token**, the opposite of what a first draft of this plan did:
/// `confined_to_workspace` (`classifier.rs:2090-2092`) refuses on `@`, and skipping here would have
/// let `>> ../x@y` through. The one `@` spelling that is not a path is a revision (`HEAD@{1}`,
/// `stash@{0}`), recognised by `@{` with no separator before it.
pub(super) fn escapes_the_run(
    candidate: &str,
    cwd: &Path,
    workspace: &str,
    shell: Option<crate::command_reader::Shell>,
) -> bool {
    let revision = candidate
        .find("@{")
        .is_some_and(|at| !candidate[..at].contains(['/', '\\']));
    if candidate.is_empty()
        || candidate.starts_with('-')
        || candidate.contains("://")
        || revision
        || candidate.eq_ignore_ascii_case("/dev/null")
        || candidate.eq_ignore_ascii_case("nul")
    {
        return false;
    }
    let path_shaped =
        candidate.contains('/') || candidate.contains('\\') || candidate.starts_with('.');
    if candidate.starts_with('~')
        || candidate.starts_with('$')
        || candidate.starts_with('%')
        || (path_shaped && (candidate.contains('$') || candidate.contains('%')))
    {
        return true;
    }
    if !path_shaped {
        return false;
    }
    let resolved = match shell {
        Some(shell) => with_git_bash_drive(candidate, shell, workspace),
        None => candidate.to_owned(),
    };
    let resolved = fold_for_containment(&normalize_path(&resolved, Some(cwd)));
    resolved != workspace && !resolved.starts_with(&format!("{workspace}/"))
}

/// G3. What is recoverable only through the reflog or not at all, and the git that destroys the
/// reflog itself, so "the reset is refused, delete the ref underneath" has nowhere to go.
pub(super) fn is_irreversible(segment: &str, words: &[String]) -> bool {
    let text = normalize_command(segment);
    // SQL `DROP` of any object — table, database, schema, view, index, trigger, user … — as the
    // spec says "DROP", not only the three a first draft listed. Broad on purpose: a commit message
    // like "drop unused import" trips it too, and the only effect is that the judge's approval
    // falls to the middle band (the classifier's own verdict), never a refusal.
    let tokens = text
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let drops = tokens.windows(2).any(|pair| pair[0] == "drop");
    // Postgres makes `TABLE` optional in `TRUNCATE [TABLE] name`, so a bare `TRUNCATE` trips this
    // too, wherever it sits — including inside a quoted SQL string passed to `psql -c` or
    // `sqlite3`. Tokenised rather than substring-matched so `truncated` (a log line, a commit
    // message) does not trip it. A false positive here only costs today's human approval.
    let truncates = tokens.contains(&"truncate");
    if drops
        || truncates
        || text
            .match_indices("delete from")
            .any(|(at, _)| !text[at..].contains(" where "))
    {
        return true;
    }
    words.iter().enumerate().any(|(index, word)| {
        let lowered = word.to_ascii_lowercase();
        let program = program_name(&lowered);
        let rest = &words[index + 1..];
        match program {
            "rm" | "del" | "erase" | "remove-item" | "ri" => rest.iter().any(|target| {
                let target = target.to_ascii_lowercase().replace('\\', "/");
                let base = target.rsplit('/').next().unwrap_or(&target);
                base.ends_with(".db") || base.contains(".sqlite")
            }),
            "truncate" => true,
            "git" => git_is_irreversible(rest),
            _ => false,
        }
    })
}

pub(super) fn git_is_irreversible(rest: &[String]) -> bool {
    let mut index = 0;
    let verb = loop {
        let Some(candidate) = rest.get(index) else {
            return false;
        };
        if crate::execution::vcs::GIT_FLAGS_WITH_VALUES.contains(&candidate.as_str()) {
            index += 2;
            continue;
        }
        if candidate.starts_with('-') {
            index += 1;
            continue;
        }
        break candidate.to_ascii_lowercase();
    };
    let args: Vec<&str> = rest[index + 1..].iter().map(String::as_str).collect();
    // Case-sensitive on purpose, like `vcs::branch_delete_from_command`: `-d` and `-D` differ by
    // case alone and mean the safe and the unsafe thing.
    let short = |letter: char| {
        args.iter()
            .any(|arg| arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains(letter))
    };
    let has = |flag: &str| args.contains(&flag);
    match verb.as_str() {
        "clean" => (short('f') || has("--force")) && (short('x') || short('X') || short('d')),
        "reset" => has("--hard"),
        // `git checkout -- .` and `git checkout .` discard the same work.
        "checkout" => has("."),
        "restore" => has(".") || has("./") || has(":/"),
        "branch" => {
            short('D') || ((has("--delete") || short('d')) && (has("--force") || short('f')))
        }
        "stash" => matches!(args.first(), Some(&"drop" | &"clear")),
        "update-ref" => has("-d"),
        "reflog" => matches!(args.first(), Some(&"expire" | &"delete")),
        "gc" => args.iter().any(|arg| arg.starts_with("--prune")),
        "prune" | "filter-branch" | "filter-repo" => true,
        "worktree" => args.first() == Some(&"remove") && (has("--force") || short('f')),
        _ => false,
    }
}

/// PURE: whether the words run an interpreter on code given inline rather than from a file.
///
/// Any word may be the interpreter, not only the first, because `env`, `sudo`, `xargs` and
/// `timeout` all put the real program in an argument. The flag has to follow it. Case-folded,
/// because PowerShell reads `-Command` and `-command` as one parameter, and by prefix there, since
/// PowerShell accepts any unambiguous prefix (`-c`, `-com`, `-enc`, `-e`).
pub(super) fn runs_inline_code(words: &[String]) -> bool {
    let lowered: Vec<String> = words.iter().map(|word| word.to_ascii_lowercase()).collect();
    lowered.iter().enumerate().any(|(index, word)| {
        let rest = &lowered[index + 1..];
        let program = program_name(word);
        let flags: &[&str] = match program {
            "bash" | "sh" | "zsh" | "dash" | "ksh" | "fish" | "busybox" => &["-c"],
            "python" | "python3" | "py" | "pypy" | "pypy3" => &["-c"],
            "node" | "deno" | "bun" => &["-e", "--eval", "-p", "--print", "eval"],
            // `-E` is perl's other spelling; the words are lowercased, so `-e` covers both.
            "perl" | "ruby" => &["-e"],
            "php" => &["-r"],
            "cmd" => &["/c", "/k", "/r"],
            "pwsh" | "powershell" => {
                return rest.iter().any(|flag| {
                    flag.strip_prefix('-').is_some_and(|name| {
                        !name.is_empty()
                            && ["command", "encodedcommand", "ec"]
                                .iter()
                                .any(|full| full.starts_with(name))
                    })
                });
            }
            _ => return false,
        };
        // A short flag may be clustered (`bash -ec '...'`, `python -Bc '...'`), so for those
        // the letter anywhere in a single-dash word counts.
        rest.iter().any(|flag| {
            flags.iter().any(|known| {
                flag == known
                    || flag.starts_with(&format!("{known}="))
                    || (known.len() == 2
                        && known.starts_with('-')
                        && flag.starts_with('-')
                        && !flag.starts_with("--")
                        && flag.contains(&known[1..].to_ascii_lowercase()))
            })
        })
    })
}
