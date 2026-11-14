//! What a leaf run does all day and the classifier used to park, each judged as narrowly as the
//! Edit/Write tools it stands in for (CLASSIFIER_VERSION 26).
//!
//! Everything here is PURE: it reads the words of one segment (and, for a heredoc body, the raw
//! command) and answers a shape question. None of it widens on its own: `classify_shell_command`
//! asks, and every function that names a path to be WRITTEN is paired with `write_target_ok`, which
//! asks the Write chain's own guards about that path, so a shell edit is never allowed on ground an
//! `Edit` of the same path would be refused on.
//!
//! The shape is deliberately small. A form not recognised here falls through to the rest of the
//! classifier, which parks it as it always did: this file can only turn a park into an allow for a
//! form it names exactly.

use std::path::Path;

use serde_json::json;

use super::paths::shell_words;
use super::shell_shape::{flag_words, has_ansi_c_or_locale_quote, is_safe_command};
use super::write_guards::{
    targets_file_that_runs_on_next_command, targets_self_governing_file, write_denied_by_project,
    writes_nucleos_data, writes_outside_cwd,
};

/// Directories a shell edit never writes into, even inside the worktree: governance, hooks, git
/// internals and cargo's config. A `Write` to most of these is already refused by the chain below;
/// this names them again so `.git` and `.cargo` (which the chain does not all cover) hold too.
const PROTECTED_COMPONENTS: &[&str] = &[".claude", ".githooks", ".git", ".cargo", ".nucleos"];

/// Whether `path` may be written by a shell edit: the Write chain's guards, in the same order, plus
/// the protected directories above. `false` for anything the shell would rewrite first (`~`, `$`,
/// `%`, a glob, a backtick) or that climbs (`..`): the allow side cannot see where those land.
pub(super) fn write_target_ok(
    path: &str,
    cwd: &Path,
    rules: &crate::project::policy::ShellRules,
) -> bool {
    if path.is_empty()
        || path.starts_with('-')
        || path.contains(['$', '%', '`', '*', '?', '~', '\n', '\r', '\0', '[', '{'])
    {
        return false;
    }
    let slashed = path.replace('\\', "/");
    if slashed.split('/').any(|part| {
        part == ".."
            || PROTECTED_COMPONENTS
                .iter()
                .any(|name| part.eq_ignore_ascii_case(name))
    }) {
        return false;
    }
    let input = json!({ "file_path": path });
    let cwd = Some(cwd);
    !writes_outside_cwd(&input, cwd)
        && !writes_nucleos_data(&input, cwd)
        && !write_denied_by_project("Write", &input, cwd, rules)
        && !targets_self_governing_file(&input, cwd)
        && !targets_file_that_runs_on_next_command(&input, cwd)
}

// ---------------------------------------------------------------------------------------------
// Shell edits: `sed -i`, `tee`, `cat > f <<EOF`, `python - <<EOF`.
// ---------------------------------------------------------------------------------------------

/// The paths a segment WRITES when it is one of the edit shapes this file knows, else `None`.
///
/// `segment` is one simple command as the reader cut it (a heredoc segment holds only its opener);
/// `command` is the raw line, which holds the heredoc body. `Some(vec![])` is a recognised shape
/// that writes nothing (a `python -` that only prints). The caller still runs `write_target_ok` on
/// every path: this only says what the shape writes.
pub(super) fn shell_edit_targets(segment: &str, command: &str) -> Option<Vec<String>> {
    if has_ansi_c_or_locale_quote(segment) || has_ansi_c_or_locale_quote(command) {
        return None;
    }
    if segment.contains("<<") {
        return heredoc_edit_targets(segment, command);
    }
    let words = shell_words(segment);
    match words.first().map(String::as_str)? {
        "sed" => sed_in_place_targets(&words),
        "tee" => tee_targets(&words),
        _ => None,
    }
}

/// `sed -i [-E|-r|-n] 's<d>pat<d>repl<d>flags' file...`: one substitution, flags limited to the
/// harmless ones (never `e`, which runs the result, nor `w`, which writes a second file), no `-e`
/// or `-f` script, no backup suffix.
fn sed_in_place_targets(words: &[String]) -> Option<Vec<String>> {
    let mut in_place = false;
    let mut script: Option<&str> = None;
    let mut files = Vec::new();
    for word in &words[1..] {
        if script.is_none() && (word == "-i" || word == "--in-place") {
            in_place = true;
        } else if script.is_none() && matches!(word.as_str(), "-E" | "-r" | "-n") {
        } else if script.is_none() && !word.starts_with('-') {
            script = Some(word);
        } else if script.is_some() && !word.starts_with('-') {
            files.push(word.clone());
        } else {
            return None;
        }
    }
    if !in_place || files.is_empty() || !is_plain_substitution(script?) {
        return None;
    }
    Some(files)
}

/// `s<d>pattern<d>replacement<d>flags` with exactly four parts and harmless flags.
fn is_plain_substitution(script: &str) -> bool {
    let mut chars = script.chars();
    if chars.next() != Some('s') {
        return false;
    }
    let Some(delimiter) = chars.next() else {
        return false;
    };
    if delimiter.is_alphanumeric() || delimiter.is_whitespace() || delimiter == '\\' {
        return false;
    }
    if script.contains(['\n', '\r']) {
        return false;
    }
    let parts: Vec<&str> = script.split(delimiter).collect();
    parts.len() == 4
        && parts[3]
            .chars()
            .all(|flag| matches!(flag, 'g' | 'p' | 'I' | 'i') || flag.is_ascii_digit())
}

/// `tee [-a] file...`: the files are what it writes; any other flag is not modelled.
fn tee_targets(words: &[String]) -> Option<Vec<String>> {
    let mut files = Vec::new();
    for word in &words[1..] {
        if word == "-a" || word == "--append" {
            continue;
        }
        if word.starts_with('-') {
            return None;
        }
        files.push(word.clone());
    }
    if files.is_empty() { None } else { Some(files) }
}

/// A segment holding a heredoc opener: `cat > f <<EOF` (and `>>`, and `cat <<EOF > f`) or
/// `python - <<EOF`.
fn heredoc_edit_targets(segment: &str, command: &str) -> Option<Vec<String>> {
    // One heredoc on the line, so the body is unambiguous.
    if command.matches("<<").count() != 1 || segment.matches("<<").count() != 1 {
        return None;
    }
    let at = segment.find("<<")?;
    let after = &segment[at + 2..];
    if after.starts_with('<') {
        return None;
    }
    let (dash, after) = match after.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, after),
    };
    let after = after.trim_start();
    let token_end = after.find(char::is_whitespace).unwrap_or(after.len());
    let token = &after[..token_end];
    let quoted = token.starts_with(['\'', '"']);
    let delimiter = token.trim_matches(['\'', '"']);
    if delimiter.is_empty()
        || !delimiter
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let mut words = shell_words(&segment[..at]);
    words.extend(shell_words(&after[token_end..]));
    let body = heredoc_body(command, delimiter, dash)?;
    match words.first().map(String::as_str)? {
        "cat" => {
            if !quoted && body.contains(['$', '`']) {
                return None;
            }
            cat_target(&words).map(|path| vec![path])
        }
        "python" | "python3" | "py" => {
            if words.len() != 2 || words[1] != "-" {
                return None;
            }
            python_body_paths(&body)
        }
        _ => None,
    }
}

/// `cat > f` / `cat >> f` / `cat >f` with nothing else on the command.
fn cat_target(words: &[String]) -> Option<String> {
    match &words[1..] {
        [op, path] if op == ">" || op == ">>" => Some(path.clone()),
        [joined] => joined
            .strip_prefix(">>")
            .or_else(|| joined.strip_prefix('>'))
            .filter(|path| !path.is_empty() && !path.starts_with(['>', '&']))
            .map(str::to_owned),
        _ => None,
    }
}

/// The heredoc body of the single `<<` in `command`: the lines after the opener's line, up to the
/// terminator (`<<-` strips leading tabs from the terminator). `None` when it never terminates.
fn heredoc_body(command: &str, delimiter: &str, dash: bool) -> Option<String> {
    let rest = &command[command.find("<<")?..];
    let body_text = &rest[rest.find('\n')? + 1..];
    let mut body = String::new();
    for line in body_text.split('\n') {
        let line = line.trim_end_matches('\r');
        let candidate = if dash {
            line.trim_start_matches('\t')
        } else {
            line
        };
        if candidate == delimiter {
            return Some(body);
        }
        body.push_str(line);
        body.push('\n');
    }
    None
}

/// Words a script may not contain at all: anything that reaches the OS, builds names at run time,
/// or reaches an attribute by string.
const PYTHON_FORBIDDEN: &[&str] = &[
    "__",
    "exec",
    "eval",
    "compile",
    "getattr",
    "setattr",
    "delattr",
    "globals",
    "locals",
    "vars(",
    "input(",
    "breakpoint",
    "subprocess",
    "shutil",
    "importlib",
    "builtins",
    "os.",
    "sys",
    "ctypes",
    "socket",
    "urllib",
    "pickle",
    "marshal",
    "system",
    "popen",
    "spawn",
    "..",
    ".parent",
    ".joinpath",
    ".with_name",
    ".with_suffix",
    ".unlink",
    ".rename",
    ".replace(",
    ".rmdir",
    ".symlink_to",
    ".hardlink_to",
    ".chmod",
    ".expanduser",
    ".home(",
    ".cwd(",
    ".resolve",
    ".absolute",
];

const PYTHON_IMPORTS: &[&str] = &[
    "import re",
    "import json",
    "import pathlib",
    "from pathlib import Path",
];

/// The literal paths a `python -` body opens or constructs, or `None` when the body is anything but
/// a small file-editing script: only the imports above, none of `PYTHON_FORBIDDEN`, no `/` join, and
/// every `open(` and `Path(` taking a quoted literal as its first argument.
fn python_body_paths(body: &str) -> Option<Vec<String>> {
    let mut code = String::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.contains("import") {
            if !PYTHON_IMPORTS.contains(&trimmed) {
                return None;
            }
            continue;
        }
        code.push_str(line);
        code.push('\n');
    }
    if PYTHON_FORBIDDEN.iter().any(|word| code.contains(word)) || has_path_join(&code) {
        return None;
    }
    let mut paths = Vec::new();
    for name in ["open", "Path"] {
        let mut from = 0;
        while let Some(found) = code[from..].find(name) {
            let start = from + found;
            let end = start + name.len();
            from = end;
            // The name is part of a longer word (`reopen`, `Pathology`) only if a word character
            // touches it; `open` NOT followed by `(` is an alias and refused.
            let before = code[..start].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let tail = &code[end..];
            if tail.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let call = tail.strip_prefix('(')?;
            paths.push(first_literal_argument(call)?);
        }
    }
    Some(paths)
}

/// Whether the body divides one thing by another right after a call or a literal: `Path("a") / x`.
fn has_path_join(code: &str) -> bool {
    let chars: Vec<char> = code.chars().collect();
    for (at, &c) in chars.iter().enumerate() {
        if matches!(c, ')' | '"' | '\'') {
            let next = chars[at + 1..].iter().find(|c| !c.is_whitespace());
            if next == Some(&'/') {
                return true;
            }
        }
    }
    false
}

/// The quoted literal that opens `call` (the text after `(`), when a `)` or `,` closes it.
fn first_literal_argument(call: &str) -> Option<String> {
    let call = call.trim_start();
    let quote = call.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let inner = &call[1..];
    let end = inner.find(quote)?;
    let literal = &inner[..end];
    if literal.contains(['\\', '\n', '{']) {
        return None;
    }
    let after = inner[end + 1..].trim_start();
    if after.starts_with(')') || after.starts_with(',') {
        Some(literal.to_owned())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Safe-list additions that need more than a prefix.
// ---------------------------------------------------------------------------------------------

/// Reads this file adds to the safe list: `date`, `gofmt -l`, and
/// `git -C <path> show|log|status|diff`. `segment` is the raw text; the shape guards the other
/// reads clear (`shell_form_is_readable`) are the caller's.
pub(super) fn is_added_read(segment: &str) -> bool {
    let words = shell_words(segment);
    let Some(program) = words.first().map(String::as_str) else {
        return false;
    };
    let rest = &words[1..];
    match program {
        "date" => rest
            .iter()
            .all(|w| w == "-u" || w == "--utc" || w.starts_with('+')),
        "gofmt" => {
            rest.first().is_some_and(|w| w == "-l") && rest[1..].iter().all(|w| !w.starts_with('-'))
        }
        "git" => is_git_dash_c_read(&words),
        _ => false,
    }
}

/// `git -C <path> show|log|status|diff <args>`: the path is one plain word, and the rest must be
/// something the ordinary safe list allows for that subcommand.
fn is_git_dash_c_read(words: &[String]) -> bool {
    let [_git, flag, path, sub, rest @ ..] = words else {
        return false;
    };
    if flag != "-C"
        || path.is_empty()
        || path.starts_with('-')
        || path.contains(['$', '%', '`', '~', '*', '?'])
        || path.replace('\\', "/").split('/').any(|part| part == "..")
        || !matches!(sub.as_str(), "show" | "log" | "status" | "diff")
    {
        return false;
    }
    if rest
        .iter()
        .any(|w| w.starts_with("--output") || w.starts_with("--ext-diff") || w == "-c")
    {
        return false;
    }
    let plain = format!("git {sub} {}", rest.join(" "));
    let normalized = super::shell_shape::normalize_command(&plain);
    flag_words(&normalized)
        .iter()
        .all(|f| !f.starts_with("--output"))
        && is_safe_command(&normalized)
}

/// `git checkout -b <name> [<start>]` and `git switch -c <name> [<start>]`: a new local branch.
pub(super) fn is_new_branch(segment: &str) -> bool {
    let words = shell_words(segment);
    let plain = |w: &String| {
        !w.starts_with('-')
            && !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
    };
    match words.as_slice() {
        [git, sub, flag, name, rest @ ..] if git == "git" => {
            ((sub == "checkout" && flag == "-b") || (sub == "switch" && flag == "-c"))
                && plain(name)
                && rest.len() <= 1
                && rest.iter().all(plain)
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------------------------
// `cd` elsewhere followed by pure reads, and `git merge` in the run's own worktree.
// ---------------------------------------------------------------------------------------------

/// A bare `cd <dir>` whose directory the allow side can read: one word, no flag, nothing the shell
/// rewrites, and not a place that holds credentials.
pub(super) fn is_plain_cd(segment: &str) -> bool {
    let words = shell_words(segment);
    let [cd, target] = words.as_slice() else {
        return false;
    };
    if cd != "cd" || target.starts_with('-') || target.contains(['$', '%', '`', '~', '*', '?']) {
        return false;
    }
    !target.replace('\\', "/").split('/').any(|part| {
        [
            ".ssh", ".aws", ".gnupg", ".config", ".nucleos", ".claude", ".git",
        ]
        .iter()
        .any(|name| part.eq_ignore_ascii_case(name))
    })
}

/// Whether a segment the classifier already judged `read-local` is one of the pure reads allowed
/// after a `cd` elsewhere. Looked at by its first words, so `mkdir` or `cargo fmt` (also
/// `read-local` inside the workspace) are not.
pub(super) fn is_pure_read(normalized: &str) -> bool {
    let mut words = normalized.split(' ');
    match words.next() {
        Some("ls" | "cat" | "head" | "tail" | "wc" | "grep") => true,
        Some("git") => matches!(
            words.next(),
            Some("status" | "log" | "show" | "diff" | "branch")
        ),
        _ => false,
    }
}

/// A shell edit of files inside the run's own worktree, judged like an `Edit`/`Write` of the same
/// paths: `Some(ReadLocal)` only when the shape is one this file names exactly AND every path it
/// writes clears `write_target_ok`. `None` means "not this", and the ordinary chain (which parks
/// the shape) judges the segment. POSIX with a workspace only.
pub(super) fn shell_edit_verdict(
    command: &str,
    segment: &str,
    cwd: Option<&Path>,
    rules: &crate::project::policy::ShellRules,
    shell: crate::command_reader::Shell,
) -> Option<super::Segment> {
    let cwd = cwd?;
    if !matches!(shell, crate::command_reader::Shell::Posix) {
        return None;
    }
    let targets = shell_edit_targets(segment, command)?;
    targets
        .iter()
        .all(|path| write_target_ok(path, cwd, rules))
        .then_some(super::Segment::ReadLocal)
}

/// `git merge ...` as the program's first two words: a merge into the checked-out branch, which for
/// a run is its own worktree's. `git -C dir merge` and `sudo git merge` are not this.
pub(super) fn is_plain_git_merge(normalized: &str) -> bool {
    let mut words = normalized.split(' ');
    words.next() == Some("git") && words.next() == Some("merge")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_substitution_script_must_have_four_parts_and_harmless_flags() {
        assert!(is_plain_substitution("s/a/b/g"));
        assert!(is_plain_substitution("s|a|b|"));
        assert!(!is_plain_substitution("s/a/b/e"));
        assert!(!is_plain_substitution("s/a/b/w out.txt"));
        assert!(!is_plain_substitution("s/a/b/g;d"));
        assert!(!is_plain_substitution("d"));
    }

    #[test]
    fn a_python_body_with_a_computed_path_is_not_a_plain_edit() {
        assert_eq!(
            python_body_paths("import re\nopen(\"a.txt\", \"w\").write(\"x\")\n"),
            Some(vec!["a.txt".to_owned()])
        );
        assert_eq!(python_body_paths("p = \"a\"\nopen(p, \"w\")\n"), None);
        assert_eq!(python_body_paths("import os\n"), None);
        assert_eq!(python_body_paths("Path(\"a\") / \"b\"\n"), None);
        assert_eq!(python_body_paths("f = open\n"), None);
    }
}
