//! PURE: the read-only command shapes defined by a grammar rather than by a prefix.
//!
//! A prefix list says "this program is harmless"; these say "this exact spelling of a program is".
//! They live beside `classifier.rs` and not in it because that file is at its size ceiling.

/// Whether a normalized command is read-only by the shape of its arguments alone.
///
/// Called from `is_safe_command`, so it inherits the shape guards (`shell_form_is_readable`) that
/// every compiled permission clears first.
pub(super) fn reads_by_grammar(command: &str) -> bool {
    prints_lines_by_address(command)
        || prints_help_only(command)
        || prints_lines_by_pattern(command)
        || folds_adjacent_lines(command)
        || lists_branches(command)
}

/// PURE: a command's words, split where the shell splits them, with their quotes KEPT — or `None`
/// when that split cannot be proved.
///
/// Kept rather than stripped (the opposite of `shell_words`) because the grammars below have to know
/// HOW a word was quoted: `$` is an address or a regex anchor only inside single quotes, and is a
/// variable everywhere else.
///
/// `None` for an unterminated quote, and for a backslash in front of a quote anywhere but inside
/// single quotes. A POSIX shell reads `\'` and `\"` as literal characters while this walk would read
/// them as quotes, so the two would disagree about where every word after them begins — and a
/// grammar that counts operands, as `uniq`'s does, must never count fewer than the shell does.
pub(super) fn quoted_words(command: &str) -> Option<Vec<&str>> {
    let mut words = Vec::new();
    let mut start = None;
    let mut quote: Option<char> = None;
    let mut previous = '\0';
    for (at, character) in command.char_indices() {
        match quote {
            Some(open) if character == open => {
                if open == '"' && previous == '\\' {
                    return None;
                }
                quote = None;
            }
            Some(_) => {}
            None if character.is_whitespace() => {
                if let Some(from) = start.take() {
                    words.push(&command[from..at]);
                }
            }
            None => {
                start.get_or_insert(at);
                if matches!(character, '\'' | '"') {
                    if previous == '\\' {
                        return None;
                    }
                    quote = Some(character);
                }
            }
        }
        previous = character;
    }
    if quote.is_some() {
        return None;
    }
    if let Some(from) = start {
        words.push(&command[from..]);
    }
    Some(words)
}

/// PURE: the text inside a word that is ONE single-quoted string and nothing else.
pub(super) fn single_quoted(word: &str) -> Option<&str> {
    word.strip_prefix('\'')
        .and_then(|inner| inner.strip_suffix('\''))
        .filter(|inner| !inner.contains('\''))
}

/// PURE: what the program will receive for a word, as far as whether it is an option goes.
///
/// `sed -n 1p f '-i'` hands sed a bare `-i` — an in-place edit — and a check that reads the raw
/// word sees a quote first and calls it a file. Every operand below is judged on this instead.
pub(super) fn unquoted(word: &str) -> String {
    word.chars().filter(|c| !matches!(c, '\'' | '"')).collect()
}

/// PURE: a `/regex/` at the start of `text`, returned as what follows its closing slash.
///
/// The end of the regex is the one fact that matters, and sed and awk disagree about it: GNU sed
/// ends at the first unescaped `/`, while gawk lets a `/` stand inside a bracket expression. So a
/// `/` inside brackets is refused outright, and a bracket that is not closed before the end is
/// refused with it — with neither, both programs end the regex exactly where this does, and the
/// text after it is the text the grammar goes on to read.
fn regex_literal(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'/') {
        return None;
    }
    let mut at = 1;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'/' => return (at > 1).then(|| &text[at + 1..]),
            b'[' => at = bracket_end(bytes, at)?,
            _ => at += 1,
        }
    }
    None
}

/// PURE: the index just past the `]` that closes the bracket expression opening at `open`.
fn bracket_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut at = open + 1;
    if bytes.get(at) == Some(&b'^') {
        at += 1;
    }
    // A `]` straight after the opener is a member, not the close.
    if bytes.get(at) == Some(&b']') {
        at += 1;
    }
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b']' => return Some(at + 1),
            b'/' => return None,
            b'\\' => at += 2,
            // `[:alpha:]` and its kin; any other nested `[` is a shape not worth modelling.
            b'[' if bytes.get(at + 1) == Some(&b':') => {
                let close = text_find(&bytes[at + 2..], b":]")?;
                at += 2 + close + 2;
            }
            b'[' => return None,
            _ => at += 1,
        }
    }
    None
}

fn text_find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// PURE: `text` without its leading run of ASCII digits, or `None` when it has none.
fn after_digits(text: &str) -> Option<&str> {
    let rest = text.trim_start_matches(|c: char| c.is_ascii_digit());
    (rest.len() < text.len()).then_some(rest)
}

/// `sed -n` whose script only prints lines by their address — `'1,40p'`, `'5p'`, `'$p'`,
/// `'10,$p'`, `'1,45p;60,140p'`, `'/re/,$p'` — which is `head`, `tail` and `grep` in another
/// spelling.
///
/// Measured on job 27, 2026-09-14: an implement node was refused
/// `cd "<worktree>/core" && sed -n '1,40p' Cargo.toml`, a read of the first forty lines of a
/// manifest. `sed` stays off the prefix list for the reasons written there — `-i` edits in place,
/// `w` writes, `e` executes — and this admits the spellings that can do none of them. It is a
/// grammar rather than a blocklist: `-n`, one script of `p` commands, then files. No other flag in
/// any position (GNU takes options after operands, so `sed -n 1p f -i` edits in place), so no `-e`,
/// no `-f` and no `--in-place` — and a QUOTED `'-i'` is the same flag, so operands are read
/// unquoted.
///
/// `$` is an address only inside single quotes. Anywhere else the shell expands `$p` before sed
/// ever sees it, and the script becomes whatever that variable holds. A regex address is admitted
/// under single quotes too, for the same reason, and only as the start or end of a RANGE: a lone
/// `/re/p` is `grep`, which is already on the list.
///
/// Read off the normalized command, as the lists are: lowercasing folds `P` into `p`, and `P`
/// only prints as well.
fn prints_lines_by_address(command: &str) -> bool {
    let Some(words) = quoted_words(command) else {
        return false;
    };
    let mut words = words.into_iter();
    if words.next() != Some("sed") {
        return false;
    }
    let mut quiet = false;
    let mut script = None;
    for word in words {
        let value = unquoted(word);
        match value.as_str() {
            "-n" | "--quiet" | "--silent" => quiet = true,
            _ if value.starts_with('-') => return false,
            _ if script.is_none() => script = Some(word),
            _ => {}
        }
    }
    quiet && script.is_some_and(prints_by_address)
}

/// PURE: whether one `sed` script is one or more `p` commands, separated by `;`, each by one line
/// address or by a range of two.
fn prints_by_address(script: &str) -> bool {
    let (mut rest, single) = match single_quoted(script) {
        Some(inner) => (inner, true),
        None => (
            script
                .strip_prefix('"')
                .and_then(|inner| inner.strip_suffix('"'))
                .unwrap_or(script),
            false,
        ),
    };
    loop {
        let Some((after, by_regex)) = sed_address(rest, single) else {
            return false;
        };
        rest = after;
        match rest.strip_prefix(',') {
            Some(second) => {
                rest = match second.strip_prefix('+') {
                    Some(count) => match after_digits(count) {
                        Some(after) => after,
                        None => return false,
                    },
                    None => match sed_address(second, single) {
                        Some((after, _)) => after,
                        None => return false,
                    },
                };
            }
            // A lone regex address is `grep`, and stays with it.
            None if by_regex => return false,
            None => {}
        }
        rest = match rest.strip_prefix('p') {
            Some(after) => after,
            None => return false,
        };
        if rest.is_empty() {
            return true;
        }
        rest = match rest.strip_prefix(';') {
            Some(after) if !after.is_empty() => after,
            _ => return false,
        };
    }
}

/// PURE: one sed address at the start of `text` — a line number, `$` or `/re/` (the last two only
/// when the script is single-quoted) — as what follows it and whether it was a regex.
fn sed_address(text: &str, single_quoted: bool) -> Option<(&str, bool)> {
    if let Some(after) = after_digits(text) {
        return Some((after, false));
    }
    if !single_quoted {
        return None;
    }
    if let Some(after) = text.strip_prefix('$') {
        return Some((after, false));
    }
    regex_literal(text).map(|after| (after, true))
}

/// `awk` whose program is a pattern with no action — `'NR>=408'`, `'NR==10,NR==20'`,
/// `'/^## A/,/^## B/'` — which prints the lines it matches and does nothing else.
///
/// `awk` stays off the prefix list because its ACTIONS are a language: `system()` runs a command,
/// `print > f` writes one, `getline` reads from a command. A program without braces has no action,
/// and awk's default action is to print the record. So this is a grammar of patterns only, built up
/// from three terms — a regex, `NR` compared to a number, a bare number — joined by `&&`, with at
/// most one `,` making a range. Nothing else parses, so there is no function to call and no
/// variable to read: `ENVIRON`, `system` and `getline` are not refused, they are simply not words
/// this grammar has.
///
/// The program must be ONE single-quoted word, because inside double quotes the shell expands `$`
/// first. No flag is admitted in any position — `-f` reads a program from a file, `-v` assigns,
/// `-i inplace` edits — and at most one operand, which may not be an `x=y` assignment either.
///
/// Read off the normalized command, so `NR` arrives as `nr`. The fold cannot widen anything: under
/// any casing the variable is either the record number or an unset name that compares as zero.
fn prints_lines_by_pattern(command: &str) -> bool {
    let Some(words) = quoted_words(command) else {
        return false;
    };
    let [program, script, operands @ ..] = &words[..] else {
        return false;
    };
    *program == "awk"
        && operands.len() <= 1
        && operands.iter().all(|operand| {
            let value = unquoted(operand);
            !value.is_empty() && !value.starts_with('-') && !value.contains('=')
        })
        && single_quoted(script).is_some_and(|body| {
            awk_pattern(body)
                .and_then(|rest| match rest.trim_start().strip_prefix(',') {
                    Some(second) => awk_pattern(second),
                    None => Some(rest),
                })
                .is_some_and(|rest| rest.trim().is_empty())
        })
}

/// PURE: one awk pattern — terms joined by `&&` — at the start of `text`, as what follows it.
fn awk_pattern(text: &str) -> Option<&str> {
    let mut rest = awk_term(text.trim_start())?;
    while let Some(next) = rest.trim_start().strip_prefix("&&") {
        rest = awk_term(next.trim_start())?;
    }
    Some(rest)
}

/// PURE: one awk term: `/re/`, a number, or `nr` compared to a number.
fn awk_term(text: &str) -> Option<&str> {
    if text.starts_with('/') {
        return regex_literal(text);
    }
    if let Some(after) = after_digits(text) {
        return Some(after);
    }
    let rest = text.strip_prefix("nr")?.trim_start();
    let rest = [">=", "<=", "==", "!=", ">", "<"]
        .iter()
        .find_map(|operator| rest.strip_prefix(operator))?;
    after_digits(rest.trim_start())
}

/// `uniq` with at most ONE file operand, and only the flags that change what it prints:
/// `-c`, `-d`, `-u`, `-i` and their long names.
///
/// Left off the prefix list because a SECOND positional argument is an output file — `uniq in out`
/// writes `out` — and counting operands means knowing which of its flags take values. This admits
/// only flags that take none, so every other word is an operand and there can be at most one.
/// Folding `-D` into `-d` changes nothing: `-D` prints every repeated line, and also only prints.
fn folds_adjacent_lines(command: &str) -> bool {
    let Some(words) = quoted_words(command) else {
        return false;
    };
    let [program, arguments @ ..] = &words[..] else {
        return false;
    };
    if *program != "uniq" || command.contains('$') {
        return false;
    }
    let mut operands = 0;
    for word in arguments {
        let value = unquoted(word);
        let admitted = match value.strip_prefix("--") {
            Some(long) => matches!(long, "count" | "repeated" | "unique" | "ignore-case"),
            None => match value.strip_prefix('-') {
                Some(short) => !short.is_empty() && short.chars().all(|c| "cdui".contains(c)),
                None => {
                    operands += 1;
                    operands <= 1
                }
            },
        };
        if !admitted {
            return false;
        }
    }
    true
}

/// `git branch` asked to LIST — `--no-merged master`, `--merged`, `--contains <ref>`,
/// `--list 'fix/*'`, `-a -v`, `--format=...` — and never to create, delete, rename or retrack.
///
/// The exact spellings in `SAFE_EXACT_COMMANDS` cover the flag-only forms; this is the form with a
/// NAME in it, and a name is precisely what turns `git branch` from a listing into a creation. git
/// reads a positional word as a pattern only in list mode, which `--list` asks for and the
/// filters `--merged`, `--no-merged`, `--contains`, `--no-contains` and `--points-at` imply
/// (`builtin/branch.c`). So a positional word is admitted only beside one of those, and every flag
/// must come from the listing set: `-d`, `-m`, `-c`, `-f`, `-u`, `-t` and every `--set-upstream`,
/// `--unset-upstream` or `--edit-description` are simply absent. `-l` is absent too: before git
/// 2.19 it meant `--create-reflog`.
///
/// No `$` anywhere: `--format` takes `%(...)` atoms, and a `$` there would be the shell's.
fn lists_branches(command: &str) -> bool {
    let Some(words) = quoted_words(command) else {
        return false;
    };
    let [git, branch, arguments @ ..] = &words[..] else {
        return false;
    };
    if *git != "git" || *branch != "branch" || arguments.is_empty() || command.contains('$') {
        return false;
    }
    const FILTERS: &[&str] = &[
        "--merged",
        "--no-merged",
        "--contains",
        "--no-contains",
        "--points-at",
    ];
    let mut listing = false;
    let mut positional = false;
    for word in arguments {
        let value = unquoted(word);
        let name = value
            .split_once('=')
            .map_or(value.as_str(), |(name, _)| name);
        if value == "--list" || FILTERS.contains(&name) {
            listing = true;
        } else if !value.starts_with('-') {
            positional = true;
        } else if !is_listing_flag(&value) {
            return false;
        }
    }
    listing || !positional
}

/// PURE: whether a `git branch` flag only changes how the listing looks or which part it shows.
fn is_listing_flag(flag: &str) -> bool {
    if matches!(flag, "--all" | "--remotes" | "--verbose" | "--show-current")
        || flag.starts_with("--format=")
    {
        return true;
    }
    // `-a`, `-r`, `-v`, `-vv` and their clusters (`-av`).
    flag.strip_prefix('-')
        .is_some_and(|short| !short.is_empty() && short.chars().all(|c| "arv".contains(c)))
}

/// `python <script>.py --help` (or `-h`) and nothing else: exactly three tokens.
///
/// It runs the script's import-time code, which is the same surface `pytest` and `cargo test`
/// already have in `SAFE_COMMAND_PREFIXES`; owner-confirmed 2026-10-11 (intent outcome 3). The
/// script must be a relative `.py` path that stays under the working directory, with no expansion
/// character in it, so the name cannot become something else once the shell has seen it. `python
/// x.py` without `--help` stays unrecognized, and so does `-c`, `-m` or any extra argument.
///
/// Read off the normalized (lowercased) command, as the lists are.
fn prints_help_only(command: &str) -> bool {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let [program, script, flag] = tokens[..] else {
        return false;
    };
    matches!(program, "python" | "python3" | "py")
        && matches!(flag, "--help" | "-h")
        && is_relative_python_script(script)
}

fn is_relative_python_script(script: &str) -> bool {
    !script.starts_with('-')
        && script.ends_with(".py")
        && !script.starts_with(['/', '\\'])
        && script.as_bytes().get(1) != Some(&b':')
        && !script.split(['/', '\\']).any(|part| part == "..")
        && !script.contains(['{', '}', '*', '?', '$', '~', '@'])
}
