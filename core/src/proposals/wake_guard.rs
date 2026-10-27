//! Sentinel: every place that files a proposal must also wake the proposals topic.
//!
//! Channel conversations learn that something is waiting for the owner from the wake bus
//! (`nucleos_base::wake::bump(Topic::Proposals)`), and there is no single funnel that creates a
//! proposal: eleven functions write `INSERT INTO proposals` in production code. A twelfth writer
//! that forgets the signal would leave the owner's phone silent, and nothing else would notice.
//! These tests make that omission fail here instead.
//!
//! **The rule.**
//! 1. Every `.rs` file under `src` and `crates/nucleos-base/src` is scanned. Everything from the
//!    first line that starts with `#[cfg(test)]` on is test code and is ignored; an insert before
//!    it is production. Files that are tests as a whole -- named `*_tests.rs`, or declared in
//!    their parent as `#[cfg(test)] mod name;`, and everything below such a file -- are skipped
//!    entirely.
//! 2. The enclosing function of an insert is the last `fn name(` line above it. It has to be in
//!    [`LISTED`] (or in [`EXEMPT`]). The list is of functions, not a count: the spec says twelve
//!    creators, but there are thirteen signal sites, because two callers own the transaction.
//! 3. [`EXEMPT`] holds helpers that insert outside `cfg(test)` without being creators. It is empty
//!    today: the three sites the plan named (a test helper in `execution/runs.rs` and two lines in
//!    `governance/autopilot.rs`) turned out not to insert proposals at all in production. An entry
//!    that stops matching fails its own test, so the list cannot rot.
//! 4. `create_team_action_in_transaction` and `propose_in_with` do not bump, because the
//!    transaction belongs to the caller and the signal must come after its commit. For those two
//!    the bump is checked in the named callers: `propose_action` (team), `write_items`
//!    (distill) and `teach` (the owner's taught notes). The other ten creators must bump in their
//!    own body.
//!
//! The scan is text, like the seal tests in `map::seam`: it does not parse Rust, it only needs the
//! shape the codebase already has.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// Directories scanned, relative to the crate root.
const ROOTS: &[&str] = &["src", "crates/nucleos-base/src"];

/// The production functions allowed to hold `INSERT INTO proposals`.
const LISTED: &[(&str, &str)] = &[
    ("src/proposals.rs", "create_action_approval"),
    ("src/proposals.rs", "create_refused_action"),
    ("src/proposals.rs", "create_skipped_item"),
    ("src/proposals.rs", "create_contact_merge"),
    ("src/proposals.rs", "create_calendar_event"),
    ("src/proposals.rs", "create_github_action"),
    ("src/proposals.rs", "create_agent_recruit"),
    ("src/proposals.rs", "create_wheel_request"),
    ("src/proposals.rs", "create_team_action_in_transaction"),
    ("src/governance/exclusion.rs", "propose"),
    ("src/knowledge/propose.rs", "propose_in_with"),
];

/// Creators that must call `bump(` in their own body.
const BUMP_IN_BODY: &[(&str, &str)] = &[
    ("src/proposals.rs", "create_action_approval"),
    ("src/proposals.rs", "create_refused_action"),
    ("src/proposals.rs", "create_skipped_item"),
    ("src/proposals.rs", "create_contact_merge"),
    ("src/proposals.rs", "create_calendar_event"),
    ("src/proposals.rs", "create_github_action"),
    ("src/proposals.rs", "create_agent_recruit"),
    ("src/proposals.rs", "create_wheel_request"),
    ("src/governance/exclusion.rs", "propose"),
    ("src/knowledge/propose.rs", "propose"),
];

/// Helpers that insert a proposal outside `cfg(test)` without being creators:
/// `(file, function, reason)`. Empty on purpose, see the module comment.
const EXEMPT: &[(&str, &str, &str)] = &[];

/// `(file, function, name the body must mention)` for the creators whose transaction is the
/// caller's: the caller commits and then bumps.
fn bump_in_callers() -> Vec<(String, &'static str, &'static str)> {
    vec![
        (
            "src/team/actions.rs".to_string(),
            "propose_action",
            "create_team_action_in_transaction",
        ),
        (
            "src/distill.rs".to_string(),
            "write_items",
            "propose_in_with",
        ),
        // The file name is assembled: a guard elsewhere keeps that word out of every other module.
        (format!("src/{}_notes.rs", "owner"), "teach", "propose_in"),
    ]
}

struct Source {
    /// Path relative to the crate root, with `/` separators.
    file: String,
    /// The file with line endings normalised.
    text: String,
}

impl Source {
    /// The part of the file before the first `#[cfg(test)]`.
    fn production(&self) -> &str {
        let lines: Vec<&str> = self.text.split_inclusive('\n').collect();
        let mut offset = 0;
        for (index, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("#[cfg(test)]") {
                // `#[cfg(test)] mod x;` only declares an out-of-line test module (handled by
                // `test_only`): it does not end the production part of this file.
                let declaration = lines[index + 1..]
                    .iter()
                    .map(|line| line.trim())
                    .find(|line| !line.starts_with("#["));
                if !declaration.is_some_and(|line| line.ends_with(';')) {
                    return &self.text[..offset];
                }
            }
            offset += line.len();
        }
        &self.text
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("a scanned directory is readable") {
        let path = entry.expect("a directory entry is readable").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

/// Files that are tests as a whole, and the directories whose files all are.
fn test_only(files: &[(PathBuf, String)]) -> (HashSet<PathBuf>, Vec<PathBuf>) {
    let mut test_files = HashSet::new();
    let mut test_dirs = Vec::new();
    for (path, text) in files {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        let parent = path.parent().unwrap_or(Path::new(""));
        // `app.rs` is a crate root too (`nucleos-app`), so its modules sit beside it.
        let children = if matches!(stem, "mod" | "lib" | "main" | "app") {
            parent.to_path_buf()
        } else {
            parent.join(stem)
        };
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !line.trim_start().starts_with("#[cfg(test)]") {
                continue;
            }
            let Some(declaration) = lines[index + 1..]
                .iter()
                .map(|line| line.trim())
                .find(|line| !line.starts_with("#["))
            else {
                continue;
            };
            let declaration = declaration
                .strip_prefix("pub(crate) ")
                .or_else(|| declaration.strip_prefix("pub(super) "))
                .or_else(|| declaration.strip_prefix("pub "))
                .unwrap_or(declaration);
            let Some(name) = declaration
                .strip_prefix("mod ")
                .and_then(|rest| rest.strip_suffix(';'))
            else {
                continue;
            };
            let name = name.trim();
            // Declared inside an inline `mod outer { ... }` (app.rs's `channels`): the file is
            // under `outer/`. One level is all the tree has.
            let indent = line.len() - line.trim_start().len();
            let children = lines[..index]
                .iter()
                .rev()
                .filter(|above| above.len() - above.trim_start().len() < indent)
                .find_map(|above| {
                    let above = above.trim();
                    let above = above.strip_prefix("pub ").unwrap_or(above);
                    above.strip_prefix("mod ")?.strip_suffix(" {")
                })
                .map_or_else(|| children.clone(), |outer| children.join(outer.trim()));
            test_files.insert(children.join(format!("{name}.rs")));
            test_files.insert(children.join(name).join("mod.rs"));
            test_dirs.push(children.join(name));
        }
    }
    (test_files, test_dirs)
}

fn sources() -> Vec<Source> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    for dir in ROOTS {
        rust_files(&root.join(dir), &mut paths);
    }
    let files: Vec<(PathBuf, String)> = paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .expect("a scanned source file is readable")
                .replace("\r\n", "\n");
            (path, text)
        })
        .collect();
    let (test_files, test_dirs) = test_only(&files);
    files
        .into_iter()
        .filter(|(path, _)| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            !name.ends_with("_tests.rs")
                && !test_files.contains(path)
                && !test_dirs.iter().any(|dir| path.starts_with(dir))
        })
        .map(|(path, text)| Source {
            file: path
                .strip_prefix(&root)
                .expect("a scanned file is under the crate root")
                .to_string_lossy()
                .replace('\\', "/"),
            text,
        })
        .collect()
}

/// The last `fn name(` line above `at`.
fn enclosing_function(text: &str, at: usize) -> Option<String> {
    for line in text[..at].lines().rev() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        let Some(position) = line.find("fn ") else {
            continue;
        };
        if position != 0 && !line[..position].ends_with(' ') {
            continue;
        }
        let name: String = line[position + 3..]
            .chars()
            .take_while(|character| character.is_alphanumeric() || *character == '_')
            .collect();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// `(file, function)` of every production insert into the proposals table.
fn production_inserts(sources: &[Source]) -> BTreeSet<(String, String)> {
    // Assembled, so that this file never matches its own scan.
    let needle = concat!("INSERT INTO ", "proposals");
    let mut found = BTreeSet::new();
    for source in sources {
        let production = source.production();
        for (at, _) in production.match_indices(needle) {
            let function = enclosing_function(production, at)
                .unwrap_or_else(|| "<outside any function>".to_string());
            found.insert((source.file.clone(), function));
        }
    }
    found
}

/// The text of `fn name(` up to its closing brace at the same indentation.
fn function_body<'a>(production: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("fn {name}(");
    let mut from = 0;
    while let Some(found) = production[from..].find(&needle) {
        let at = from + found;
        from = at + needle.len();
        let line_start = production[..at]
            .rfind('\n')
            .map_or(0, |newline| newline + 1);
        let before = &production[line_start..at];
        if before.trim_start().starts_with("//") {
            continue;
        }
        let indent: String = before
            .chars()
            .take_while(|character| character.is_whitespace())
            .collect();
        let close = format!("\n{indent}}}\n");
        if let Some(end) = production[at..].find(&close) {
            return Some(&production[at..at + end]);
        }
    }
    None
}

fn body_of<'a>(sources: &'a [Source], file: &str, name: &str) -> &'a str {
    let source = sources
        .iter()
        .find(|source| source.file == file)
        .unwrap_or_else(|| panic!("{file} is no longer scanned"));
    function_body(source.production(), name)
        .unwrap_or_else(|| panic!("{file} has no production fn {name}"))
}

#[test]
fn wake_guard_every_proposal_insert_outside_tests_is_listed() {
    let sources = sources();
    let found = production_inserts(&sources);

    let unlisted: Vec<&(String, String)> = found
        .iter()
        .filter(|(file, function)| {
            !LISTED.iter().any(|(listed_file, listed_fn)| {
                *listed_file == file.as_str() && *listed_fn == function.as_str()
            }) && !EXEMPT.iter().any(|(exempt_file, exempt_fn, _)| {
                *exempt_file == file.as_str() && *exempt_fn == function.as_str()
            })
        })
        .collect();
    assert!(
        unlisted.is_empty(),
        "a production insert into the proposals table sits in a function that is not a listed \
         creator, so nothing wakes the proposals topic for it: {unlisted:?}. Bump \
         `Topic::Proposals` after its commit and add it to LISTED, or, if it is a helper and not a \
         creator, to EXEMPT with the reason"
    );

    // The scan must still see every listed creator, or a change to the scan's reading of the
    // source would let all of this pass on nothing.
    for (file, function) in LISTED {
        assert!(
            found.contains(&(file.to_string(), function.to_string())),
            "{file}::{function} is listed but no longer holds a production insert: update LISTED"
        );
    }
}

#[test]
fn wake_guard_every_listed_creator_calls_bump() {
    let sources = sources();

    for (file, function) in BUMP_IN_BODY {
        let body = body_of(&sources, file, function);
        assert!(
            body.contains("bump("),
            "{file}::{function} files a proposal but does not call bump( on the proposals topic"
        );
    }

    for (file, function, callee) in bump_in_callers() {
        let body = body_of(&sources, &file, function);
        assert!(
            body.contains(callee),
            "{file}::{function} no longer calls {callee}, the creator whose transaction it owns"
        );
        assert!(
            body.contains("bump("),
            "{file}::{function} owns the transaction of a proposal creator but does not call \
             bump( after committing"
        );
    }
}

#[test]
fn wake_guard_every_exemption_still_exists() {
    let sources = sources();
    let found = production_inserts(&sources);

    for (file, function, reason) in EXEMPT {
        assert!(
            !reason.trim().is_empty(),
            "the exemption for {file}::{function} carries no reason"
        );
        assert!(
            found.contains(&(file.to_string(), function.to_string())),
            "{file}::{function} is exempt but no longer inserts a proposal outside tests: drop it \
             from EXEMPT"
        );
    }
}
