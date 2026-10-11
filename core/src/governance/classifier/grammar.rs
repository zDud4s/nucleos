//! PURE: the read-only command shapes defined by a grammar rather than by a prefix.
//!
//! A prefix list says "this program is harmless"; these say "this exact spelling of a program is".
//! They live beside `classifier.rs` and not in it because that file is at its size ceiling.

/// Whether a normalized command is read-only by the shape of its arguments alone.
///
/// Called from `is_safe_command`, so it inherits the shape guards (`shell_form_is_readable`) that
/// every compiled permission clears first.
pub(super) fn reads_by_grammar(command: &str) -> bool {
    prints_lines_by_address(command) || prints_help_only(command)
}

/// `sed -n` whose script only prints lines by their address — `'1,40p'`, `'5p'`, `'$p'`,
/// `'10,$p'` — which is `head` and `tail` in another spelling.
///
/// Measured on job 27, 2026-09-14: an implement node was refused
/// `cd "<worktree>/core" && sed -n '1,40p' Cargo.toml`, a read of the first forty lines of a
/// manifest. `sed` stays off the prefix list for the reasons written there — `-i` edits in place,
/// `w` writes, `e` executes — and this admits the one spelling that can do none of them. It is a
/// grammar rather than a blocklist: `-n`, one script of one or two addresses and a `p`, then files.
/// No other flag in any position (GNU takes options after operands, so `sed -n 1p f -i` edits in
/// place), so no `-e`, no `-f` and no `--in-place`; no second command; no regex address.
///
/// `$` is an address only inside single quotes. Anywhere else the shell expands `$p` before sed
/// ever sees it, and the script becomes whatever that variable holds.
///
/// Read off the normalized command, as the lists are: lowercasing folds `P` into `p`, and `P`
/// only prints as well.
fn prints_lines_by_address(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    if tokens.next() != Some("sed") {
        return false;
    }
    let mut quiet = false;
    let mut script = None;
    for token in tokens {
        match token {
            "-n" | "--quiet" | "--silent" => quiet = true,
            _ if token.starts_with('-') => return false,
            _ if script.is_none() => script = Some(token),
            _ => {}
        }
    }
    quiet && script.is_some_and(prints_by_address)
}

/// PURE: whether one `sed` script is a single `p` by one or two line addresses.
fn prints_by_address(script: &str) -> bool {
    let (body, dollar_is_literal) = match script
        .strip_prefix('\'')
        .and_then(|inner| inner.strip_suffix('\''))
    {
        Some(inner) => (inner, true),
        None => (
            script
                .strip_prefix('"')
                .and_then(|inner| inner.strip_suffix('"'))
                .unwrap_or(script),
            false,
        ),
    };
    let address = |value: &str| {
        (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            || (value == "$" && dollar_is_literal)
    };
    let Some(range) = body.strip_suffix('p') else {
        return false;
    };
    match range.split_once(',') {
        Some((from, to)) => address(from) && address(to),
        None => address(range),
    }
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
