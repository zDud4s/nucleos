//! Expanding a custom command or skill into the prompt a model receives.
//!
//! The same text for every engine, which is the point: only the Claude CLI expands a slash command
//! itself, so the daemon does it and sends the result as an ordinary message. Pure: the file's text
//! comes in, so the caller decides when to read it and nothing is cached here.

use super::parse::Parsed;
use crate::commands::{Kind, described};
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

/// A command's text ready for a model, and what it asked for that this expansion does not do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expanded {
    pub text: String,
    /// `allowed-tools`, `model`, `bang`, `at-file`, in that order: each one a thing the file
    /// asked the CLI to act on, which here reaches the model as literal text.
    pub partial: Vec<String>,
}

/// A backtick command the CLI would run before the model sees the prompt.
static BANG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!`[^`]+`").expect("the bang pattern is valid"));

/// A file reference the CLI would inline: `@` at the start of a word, so an e-mail address is not one.
static AT_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)(^|\s)@[\w./~-]+").expect("the at-file pattern is valid"));

/// The text after a leading `---` ... `---` block, or all of it when there is none (or the fence
/// never closes, which `described` treats as no front matter as well).
fn body_of(file_text: &str) -> &str {
    let mut offset = 0;
    let mut lines = file_text.split_inclusive('\n');
    match lines.next() {
        Some(first) if first.trim() == "---" => offset += first.len(),
        _ => return file_text,
    }
    for line in lines {
        offset += line.len();
        if line.trim() == "---" {
            return &file_text[offset..];
        }
    }
    file_text
}

/// Replaces `$ARGUMENTS` and `$1..$9` in one pass, so an argument that itself contains `$1` is
/// not substituted again. `$10` is `$1` followed by a `0`. Returns whether any placeholder was seen.
fn substitute(body: &str, parsed: &Parsed) -> (String, bool) {
    let mut out = String::with_capacity(body.len());
    let mut used = false;
    let mut rest = body;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("$ARGUMENTS") {
            out.push_str(&parsed.rest);
            used = true;
            rest = after;
        } else if let Some(digit) = tail[1..]
            .chars()
            .next()
            .and_then(|c| c.to_digit(10))
            .filter(|d| *d >= 1)
        {
            if let Some(arg) = parsed.argv.get(digit as usize - 1) {
                out.push_str(arg);
            }
            used = true;
            rest = &tail[2..];
        } else {
            out.push('$');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    (out, used)
}

/// Expands `file_text` (a command file or a `SKILL.md`) for the arguments in `parsed`.
///
/// `base_dir` is the skill's own folder and is only named for `Kind::Skill`.
pub fn expand(file_text: &str, parsed: &Parsed, kind: Kind, base_dir: &Path) -> Expanded {
    let front = described(file_text);
    let body = body_of(file_text).trim();

    let mut partial = Vec::new();
    if front.allowed_tools.is_some() {
        partial.push("allowed-tools".to_string());
    }
    if front.model.is_some() {
        partial.push("model".to_string());
    }
    if BANG.is_match(body) {
        partial.push("bang".to_string());
    }
    if AT_FILE.is_match(body) {
        partial.push("at-file".to_string());
    }

    let (mut text, used) = substitute(body, parsed);
    if !used && !parsed.rest.is_empty() {
        text.push_str("\n\nARGUMENTS: ");
        text.push_str(&parsed.rest);
    }
    if kind == Kind::Skill {
        text = format!(
            "Base directory for this skill: {}\n\n{text}",
            base_dir.display()
        );
    }
    Expanded { text, partial }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::slash::parse::{Parsed, parse};
    use crate::commands::Kind;
    use std::path::Path;

    fn typed(text: &str) -> Parsed {
        parse(text).unwrap().unwrap()
    }

    fn command(body: &str, text: &str) -> Expanded {
        expand(body, &typed(text), Kind::Command, Path::new(""))
    }

    #[test]
    fn expand_arguments_placeholder() {
        let out = command("Fix $ARGUMENTS now", "/fix issue 12");
        assert_eq!(out.text, "Fix issue 12 now");
        assert!(out.partial.is_empty());

        // Every occurrence is replaced, and the raw text goes in quotes and all.
        let twice = command("$ARGUMENTS and $ARGUMENTS", r#"/x "a b""#);
        assert_eq!(twice.text, r#""a b" and "a b""#);

        // The placeholder with no arguments becomes empty.
        assert_eq!(command("Run [$ARGUMENTS]", "/x").text, "Run []");
    }

    #[test]
    fn expand_positional_placeholders() {
        let out = command("first=$1 second=$2 third=$3", r#"/x "a b" c"#);
        // A quoted argument is one argument; a missing one is empty.
        assert_eq!(out.text, "first=a b second=c third=");

        // `$10` is `$1` followed by a literal 0.
        let tenth = command("$10", "/x one");
        assert_eq!(tenth.text, "one0");

        // `$9` is the last positional.
        let nine = command("$9", "/x 1 2 3 4 5 6 7 8 9");
        assert_eq!(nine.text, "9");
    }

    #[test]
    fn expand_appends_arguments_when_no_placeholder() {
        let out = command("Do the thing.", "/x foo bar");
        assert_eq!(out.text, "Do the thing.\n\nARGUMENTS: foo bar");

        // Nothing typed, nothing appended.
        assert_eq!(command("Do the thing.", "/x").text, "Do the thing.");

        // A body that already uses a placeholder gets no trailing line.
        let placed = command("Do $1.", "/x foo bar");
        assert_eq!(placed.text, "Do foo.");
        assert!(!placed.text.contains("ARGUMENTS:"));
    }

    #[test]
    fn expand_skill_gets_base_dir_header() {
        let base = Path::new("/tmp/skills/review");
        let file = "---\nname: review\n---\nLook at the diff.";

        let with_args = expand(file, &typed("/review one two"), Kind::Skill, base);
        assert!(
            with_args
                .text
                .starts_with("Base directory for this skill: /tmp/skills/review\n\n"),
            "header missing: {:?}",
            with_args.text
        );
        assert!(with_args.text.contains("Look at the diff."));
        assert!(
            !with_args.text.contains("name: review"),
            "front matter must be stripped"
        );
        assert!(with_args.text.ends_with("\n\nARGUMENTS: one two"));

        // No arguments: no ARGUMENTS line.
        let bare = expand(file, &typed("/review"), Kind::Skill, base);
        assert!(
            bare.text
                .starts_with("Base directory for this skill: /tmp/skills/review")
        );
        assert!(!bare.text.contains("ARGUMENTS:"));

        // A placeholder in the body replaces the trailing line.
        let placed = expand("Review $ARGUMENTS", &typed("/review 12"), Kind::Skill, base);
        assert!(placed.text.contains("Review 12"));
        assert!(!placed.text.contains("ARGUMENTS:"));
    }

    #[test]
    fn expand_reports_partial_reasons() {
        let file = "---\ndescription: d\nallowed-tools: Bash\nmodel: opus\n---\nRun !`ls` and read @src/main.rs\n";
        let out = command(file, "/x");
        assert_eq!(
            out.partial,
            vec![
                "allowed-tools".to_string(),
                "model".to_string(),
                "bang".to_string(),
                "at-file".to_string()
            ]
        );
        // Front matter is not part of the prompt; the rest goes to the model literally.
        assert!(!out.text.contains("allowed-tools"));
        assert!(out.text.contains("!`ls`"));
        assert!(out.text.contains("@src/main.rs"));

        // Each reason on its own.
        assert_eq!(
            command("---\nmodel: opus\n---\nGo", "/x").partial,
            vec!["model".to_string()]
        );
        assert_eq!(
            command("see @README.md", "/x").partial,
            vec!["at-file".to_string()]
        );
        assert_eq!(
            command("run !`date`", "/x").partial,
            vec!["bang".to_string()]
        );

        // An at-sign inside a word (an email address) is not a file reference.
        assert!(command("mail me@example.com", "/x").partial.is_empty());
        assert!(command("plain body", "/x").partial.is_empty());
    }
}
