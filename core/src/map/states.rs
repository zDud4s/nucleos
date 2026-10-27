//! The state machines a project's code draws, read as text.
//!
//! Every compare-and-set statement of the shape "UPDATE table, SET a status column to a value,
//! WHERE the same column holds another value" is a transition of that column's lifecycle. This
//! module reads those statements out of any scanned text file and groups them per (table, column),
//! so the lifecycle is extracted from the code and never drawn by hand.
//!
//! **Two sentinels.** A transition whose guard names no literal state comes from `"*"` (any
//! state); one whose new value is bound at runtime goes to `"?"` (computed). Neither is listed in
//! a machine's `states`.
//!
//! **Language-agnostic on purpose.** The rule is a text rule: Rust, Go raw strings and `.sql`
//! files all read the same way, and the enclosing function is whichever `fn`, `def`, `function` or
//! `func` came last before the statement. No parser, no per-repository configuration.
//!
//! **Enum machines.** A Rust enum named `*State` whose variants' docs say "Arrives from `A`" or
//! "Leaves for `B`" draws its declared edges too ([`enum_machines`]), listed under the enum's name
//! with the column `enum`. Those edges are the docs' words and not executed code, and they carry
//! no function; the SQL machines stay the ones read off statements.
//!
//! **What is not read.** A Rust `#[cfg(test)]` module tail, test files, and lines that start as a
//! comment.
//!
//! **Accepted approximations**, documented rather than hidden: a doc comment's continuation line
//! that starts with the statement keyword can read as a transition, and a subquery on another
//! table that uses an unqualified status column can contribute a spurious "from". Guards inside
//! `EXISTS (...)`, negated guards and columns qualified by another table are never a "from".

use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::LazyLock;

/// Stands for "any state": the statement names no literal state to move from.
pub const ANY: &str = "*";
/// Stands for "computed": the new value is bound at runtime, so no literal is known.
pub const COMPUTED: &str = "?";

/// How far past the keyword a statement is followed before it is cut off.
const STATEMENT_CAP: usize = 4000;

/// One compare-and-set statement's move, with where it was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Transition {
    pub from: String,
    pub to: String,
    /// The `fn`, `def`, `function` or `func` that most recently opened before the statement.
    pub function: Option<String>,
    pub path: String,
    /// 1-based line of the statement's keyword.
    pub line: usize,
}

/// The lifecycle of one (table, column), as the code draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Machine {
    pub table: String,
    pub column: String,
    /// The literal states only, sorted: neither sentinel is listed.
    pub states: Vec<String>,
    pub transitions: Vec<Transition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct States {
    pub machines: Vec<Machine>,
    pub files_read: usize,
}

static UPDATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bUPDATE\s+([A-Za-z_][\w.]*)\s+SET\b").expect("the update pattern")
});

static TEST_TAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"#\[cfg\(test\)\]\s*\n\s*mod\s").expect("the test-module pattern")
});

static EXISTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:NOT\s+)?EXISTS\s*\(").expect("the exists pattern"));

static FUNCTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:fn|def|function)\s+([A-Za-z_]\w*)|\bfunc\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)")
        .expect("the function pattern")
});

static QUOTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"'([^']*)'").expect("the quoted-value pattern"));

/// A literal `\n`, `\r` or `\t` inside a string is whitespace to the SQL it spells. Each escape is
/// swapped for two spaces, so every offset (and every real newline) stays where it was.
fn unescaped(source: &str) -> String {
    source
        .replace("\\n", "  ")
        .replace("\\r", "  ")
        .replace("\\t", "  ")
}

/// A Rust file stops being production code at its test module.
fn production_part<'a>(path: &str, text: &'a str) -> &'a str {
    if !path.ends_with(".rs") {
        return text;
    }
    TEST_TAIL
        .find(text)
        .map_or(text, |found| &text[..found.start()])
}

fn starts_as_comment(text: &str, at: usize) -> bool {
    let line_start = text[..at].rfind('\n').map_or(0, |index| index + 1);
    let line = text[line_start..].trim_start();
    ["//", "/*", "*", "#", "--"]
        .iter()
        .any(|opener| line.starts_with(opener))
}

fn statement_after(text: &str, start: usize) -> &str {
    let rest = &text[start..];
    let stop = rest.find(['"', '`', ';']).unwrap_or(rest.len());
    let mut end = stop.min(STATEMENT_CAP);
    while !rest.is_char_boundary(end) {
        end -= 1;
    }
    &rest[..end]
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The first `word` outside every parenthesis and every `'...'` literal.
fn find_keyword(text: &str, word: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let (mut depth, mut quoted) = (0usize, false);
    for (index, &byte) in bytes.iter().enumerate() {
        match byte {
            b'\'' => quoted = !quoted,
            _ if quoted => {}
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => {
                let end = index + word.len();
                let before = index == 0 || !is_word_byte(bytes[index - 1]);
                let after = end >= bytes.len() || !is_word_byte(bytes[end]);
                if before
                    && after
                    && end <= bytes.len()
                    && bytes[index..end].eq_ignore_ascii_case(word.as_bytes())
                {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn cut_at<'a>(text: &'a str, word: &str) -> &'a str {
    find_keyword(text, word).map_or(text, |index| &text[..index])
}

/// The SET clause and the WHERE clause of one statement (the latter empty when there is none).
fn clauses(statement: &str) -> (&str, &str) {
    let (set, filter) = match find_keyword(statement, "WHERE") {
        Some(index) => (&statement[..index], &statement[index + "WHERE".len()..]),
        None => (statement, ""),
    };
    (cut_at(set, "RETURNING"), cut_at(filter, "RETURNING"))
}

fn split_top_level(text: &str) -> Vec<&str> {
    let (mut depth, mut quoted, mut from) = (0usize, false, 0usize);
    let mut parts = Vec::new();
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'\'' => quoted = !quoted,
            _ if quoted => {}
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(&text[from..index]);
                from = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[from..]);
    parts
}

fn is_status_column(column: &str) -> bool {
    column == "status"
        || column == "state"
        || column.ends_with("_status")
        || column.ends_with("_state")
}

/// `'x'` and nothing else is a literal.
fn literal(value: &str) -> Option<&str> {
    let inner = value.strip_prefix('\'')?.strip_suffix('\'')?;
    (!inner.contains('\'')).then_some(inner)
}

/// (column, new value) for every status-like column the SET clause assigns.
fn assignments(set_clause: &str) -> Vec<(String, String)> {
    split_top_level(set_clause)
        .into_iter()
        .filter_map(|item| {
            let (lhs, rhs) = item.split_once('=')?;
            let column = lhs.trim().rsplit('.').next()?.to_ascii_lowercase();
            let plain = column
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !plain || !is_status_column(&column) {
                return None;
            }
            let rhs = rhs
                .trim_end_matches(|c: char| c.is_whitespace() || c == '\\')
                .trim();
            let to = literal(rhs).unwrap_or(COMPUTED).to_string();
            Some((column, to))
        })
        .collect()
}

/// The index of the parenthesis closing the one at `open`, ignoring `'...'` literals.
fn matching_paren(text: &str, open: usize) -> Option<usize> {
    let (mut depth, mut quoted) = (0usize, false);
    for (index, byte) in text.bytes().enumerate().skip(open) {
        match byte {
            b'\'' => quoted = !quoted,
            _ if quoted => {}
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// The WHERE clause without its `[NOT] EXISTS (...)` groups: what a subquery guards is not what
/// the row moves from.
fn strip_exists(filter: &str) -> String {
    let mut text = filter.to_string();
    while let Some(found) = EXISTS.find(&text) {
        let (start, open) = (found.start(), found.end() - 1);
        let close = matching_paren(&text, open).map_or(text.len(), |index| index + 1);
        text.replace_range(start..close, " ");
    }
    text
}

/// The literal states the WHERE clause guards `column` of `table` with, or `["*"]` for none.
fn guards(filter: &str, table: &str, column: &str) -> Vec<String> {
    let pattern = format!(
        r"(?i)(?:^|[^\w.])(?:(\w+)\.)?{}\s*(?:=\s*'([^']*)'|IN\b\s*\(([^)]*)\))",
        regex::escape(column)
    );
    let Ok(guard) = Regex::new(&pattern) else {
        return vec![ANY.to_string()];
    };
    let own = table.rsplit('.').next().unwrap_or(table);
    let mut from: Vec<String> = Vec::new();
    for caps in guard.captures_iter(&strip_exists(filter)) {
        if caps
            .get(1)
            .is_some_and(|qualifier| !qualifier.as_str().eq_ignore_ascii_case(own))
        {
            continue;
        }
        let values: Vec<String> = match (caps.get(2), caps.get(3)) {
            (Some(one), _) => vec![one.as_str().to_string()],
            (None, Some(list)) => QUOTED
                .captures_iter(list.as_str())
                .map(|value| value[1].to_string())
                .collect(),
            _ => Vec::new(),
        };
        for value in values {
            if !from.contains(&value) {
                from.push(value);
            }
        }
    }
    if from.is_empty() {
        from.push(ANY.to_string());
    }
    from
}

fn enclosing_function(before: &str) -> Option<String> {
    FUNCTION.captures_iter(before).last().and_then(|caps| {
        caps.get(1)
            .or_else(|| caps.get(2))
            .map(|name| name.as_str().to_string())
    })
}

/// Every status transition one file's text spells, as (table, column, transition).
pub fn transitions(path: &str, source: &str) -> Vec<(String, String, Transition)> {
    let normalized = unescaped(source);
    let text = production_part(path, &normalized);
    let mut found = Vec::new();
    for caps in UPDATE.captures_iter(text) {
        let keyword = caps.get(0).expect("the whole match");
        if starts_as_comment(text, keyword.start()) {
            continue;
        }
        let table = caps[1].to_string();
        let (set_clause, filter) = clauses(statement_after(text, keyword.end()));
        let assigned = assignments(set_clause);
        if assigned.is_empty() {
            continue;
        }
        let function = enclosing_function(&text[..keyword.start()]);
        let line = 1 + text[..keyword.start()].matches('\n').count();
        for (column, to) in assigned {
            for from in guards(filter, &table, &column) {
                found.push((
                    table.clone(),
                    column.clone(),
                    Transition {
                        from,
                        to: to.clone(),
                        function: function.clone(),
                        path: path.to_string(),
                        line,
                    },
                ));
            }
        }
    }
    found
}

static ENUM_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?enum[ \t]+(\w+State)[ \t]*\{")
        .expect("the enum pattern")
});

static DOC_EDGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:arrives from|leaves for)\b").expect("the doc-edge pattern")
});

static BACKTICKED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`(\w+)`").expect("the backticked-name pattern"));

static VARIANT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Z]\w*)").expect("the variant pattern"));

/// One variant of an enum as written: its name, its line and its doc comment on one line.
struct EnumVariant {
    name: String,
    line: usize,
    doc: String,
}

/// The variants of the enum whose head sits on line `head_line` (0-based): whatever starts a line
/// at brace depth one. Fields of a struct-like variant sit deeper and are not read.
fn enum_variants(text: &str, head_line: usize) -> Vec<EnumVariant> {
    let mut variants = Vec::new();
    let mut depth = 1usize;
    let mut doc: Vec<String> = Vec::new();
    for (offset, line) in text.lines().enumerate().skip(head_line + 1) {
        let trimmed = line.trim();
        if let Some(doc_line) = trimmed.strip_prefix("///") {
            if !doc_line.starts_with('/') {
                doc.push(doc_line.trim().to_string());
            }
            continue;
        }
        if trimmed.is_empty() {
            doc.clear();
            continue;
        }
        if trimmed.starts_with("//") || trimmed.starts_with("#[") {
            continue;
        }
        if depth == 1
            && let Some(caps) = VARIANT.captures(trimmed)
        {
            variants.push(EnumVariant {
                name: caps[1].to_string(),
                line: offset + 1,
                doc: doc.join(" "),
            });
        }
        doc.clear();
        for character in trimmed.chars() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return variants;
                    }
                }
                _ => {}
            }
        }
    }
    variants
}

/// The Rust enums named `*State` whose variants' docs declare edges: "Arrives from `A`" is the
/// edge A -> this variant, "Leaves for `B`, `C`" the edges this variant -> B and C. Each is
/// (enum name, every variant in declaration order, edges); an edge is anchored at the variant
/// whose doc says it and carries no function. A name in backticks that is not a variant of the
/// same enum is not a state, and an enum that declares no edge is not a machine.
///
/// **Accepted approximation:** a clause runs to the next `;` or sentence end, so a prose detour
/// ("Leaves for `Running` and from there back to `Merging`") reads every variant it names as a
/// target of this one. These are the docs' own words, and the edge's anchor is where to check.
pub fn enum_machines(path: &str, source: &str) -> Vec<(String, Vec<String>, Vec<Transition>)> {
    if !path.ends_with(".rs") {
        return Vec::new();
    }
    let text = production_part(path, source);
    let mut found = Vec::new();
    for caps in ENUM_HEAD.captures_iter(text) {
        let head = caps.get(0).expect("the whole match");
        let head_line = text[..head.start()].matches('\n').count();
        let variants = enum_variants(text, head_line);
        let names: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();

        let mut transitions: Vec<Transition> = Vec::new();
        for variant in &variants {
            let keywords: Vec<_> = DOC_EDGE.find_iter(&variant.doc).collect();
            for (index, keyword) in keywords.iter().enumerate() {
                let rest = &variant.doc[keyword.end()..];
                let next = keywords
                    .get(index + 1)
                    .map_or(rest.len(), |next| next.start() - keyword.end());
                let stop = [rest.find(';'), rest.find(". ")]
                    .into_iter()
                    .flatten()
                    .min()
                    .unwrap_or(rest.len())
                    .min(next);
                let arrives = keyword.as_str().to_ascii_lowercase().starts_with("arrives");
                for named in BACKTICKED.captures_iter(&rest[..stop]) {
                    let other = &named[1];
                    if other == variant.name || !names.iter().any(|name| name == other) {
                        continue;
                    }
                    let (from, to) = if arrives {
                        (other.to_string(), variant.name.clone())
                    } else {
                        (variant.name.clone(), other.to_string())
                    };
                    if transitions.iter().any(|t| t.from == from && t.to == to) {
                        continue;
                    }
                    transitions.push(Transition {
                        from,
                        to,
                        function: None,
                        path: path.to_string(),
                        line: variant.line,
                    });
                }
            }
        }
        if !transitions.is_empty() {
            found.push((caps[1].to_string(), names, transitions));
        }
    }
    found
}

fn is_test_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.starts_with("tests/")
        || path.contains("/tests/")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with("_test.go")
        || name.ends_with("_test.py")
        || name.starts_with("test_")
}

/// The machines a project's files draw, grouped per (table, column) in sorted order.
pub fn machines(root: &Path) -> std::io::Result<States> {
    let mut files = Vec::new();
    super::collect(root, root, &mut files)?;
    files.sort();
    let mut grouped: BTreeMap<(String, String), Vec<Transition>> = BTreeMap::new();
    let mut variants: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    let mut files_read = 0;
    for path in files {
        if !super::scanned(&path) || is_test_path(&path) {
            continue;
        }
        let source = std::fs::read_to_string(root.join(&path)).unwrap_or_default();
        files_read += 1;
        for (table, column, transition) in transitions(&path, &source) {
            grouped.entry((table, column)).or_default().push(transition);
        }
        for (name, names, edges) in enum_machines(&path, &source) {
            let key = (name, "enum".to_string());
            variants.entry(key.clone()).or_default().extend(names);
            grouped.entry(key).or_default().extend(edges);
        }
    }
    let machines = grouped
        .into_iter()
        .map(|((table, column), mut transitions)| {
            transitions.sort_by(|a, b| {
                (&a.path, a.line, &a.from, &a.to).cmp(&(&b.path, b.line, &b.from, &b.to))
            });
            transitions.dedup();
            let mut states: BTreeSet<String> = transitions
                .iter()
                .flat_map(|t| [t.from.clone(), t.to.clone()])
                .filter(|state| state != ANY && state != COMPUTED)
                .collect();
            if let Some(declared) = variants.remove(&(table.clone(), column.clone())) {
                states.extend(declared);
            }
            Machine {
                table,
                column,
                states: states.into_iter().collect(),
                transitions,
            }
        })
        .collect();
    Ok(States {
        machines,
        files_read,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("nucleos-states-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("scratch");
        root
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let full = root.join(rel);
        fs::create_dir_all(full.parent().expect("parent")).expect("dirs");
        fs::write(full, body).expect("write");
    }

    /// (from, to) pairs of what a source gives, for one table and column.
    fn pairs(path: &str, source: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = transitions(path, source)
            .into_iter()
            .map(|(_, _, t)| (t.from, t.to))
            .collect();
        out.sort();
        out
    }

    fn pair(from: &str, to: &str) -> (String, String) {
        (from.to_string(), to.to_string())
    }

    /// AC1
    #[test]
    fn a_compare_and_set_gives_its_from_to_function_and_line() {
        let source = "fn first() {}\n\nfn claim() {\n    q(\"UPDATE jobs\\n SET status = 'b', at = ?1\\n WHERE id = ?2 AND status = 'a'\");\n}\n";

        let found = transitions("core/src/jobs.rs", source);

        assert_eq!(found.len(), 1, "{found:?}");
        let (table, column, transition) = &found[0];
        assert_eq!(table, "jobs");
        assert_eq!(column, "status");
        assert_eq!(transition.from, "a");
        assert_eq!(transition.to, "b");
        assert_eq!(transition.function.as_deref(), Some("claim"));
        assert_eq!(transition.path, "core/src/jobs.rs");
        assert_eq!(transition.line, 4);
    }

    /// AC2
    #[test]
    fn an_in_list_guard_gives_one_transition_per_state_and_no_guard_is_any() {
        let listed =
            "fn drop_it() { q(\"UPDATE t SET status = 'x' WHERE status IN ('a', 'c')\"); }\n";
        let unguarded = "fn wipe() { q(\"UPDATE t SET status = 'x' WHERE id = 1\"); }\n";

        assert_eq!(pairs("a.rs", listed), vec![pair("a", "x"), pair("c", "x")]);
        assert_eq!(pairs("a.rs", unguarded), vec![pair(ANY, "x")]);
    }

    /// AC3
    #[test]
    fn a_bound_target_is_computed_and_an_update_without_the_status_is_nothing() {
        let bound = "fn finish() { q(\"UPDATE t SET status = ?1 WHERE id = ?2 AND status = 'running'\"); }\n";
        let other = "fn touch() { q(\"UPDATE t SET attempts = attempts + 1 WHERE status = 'running'\"); }\n";

        assert_eq!(pairs("a.rs", bound), vec![pair("running", COMPUTED)]);
        assert!(transitions("a.rs", other).is_empty());
    }

    /// AC4
    #[test]
    fn a_guard_inside_exists_or_negated_or_of_another_table_is_not_a_from() {
        let exists = "fn f() { q(\"UPDATE t SET status = 'x' WHERE id = 1 AND NOT EXISTS (SELECT 1 FROM t WHERE status = 'running')\"); }\n";
        let negated_in =
            "fn f() { q(\"UPDATE t SET status = 'x' WHERE status NOT IN ('a', 'b')\"); }\n";
        let not_equal = "fn f() { q(\"UPDATE t SET status = 'x' WHERE status != 'a'\"); }\n";
        let diamond = "fn f() { q(\"UPDATE t SET status = 'x' WHERE status <> 'a'\"); }\n";
        let other_table =
            "fn f() { q(\"UPDATE t SET status = 'x' WHERE runs.status = 'running'\"); }\n";

        for source in [exists, negated_in, not_equal, diamond, other_table] {
            assert_eq!(pairs("a.rs", source), vec![pair(ANY, "x")], "{source}");
        }
    }

    /// AC5
    #[test]
    fn test_modules_test_files_and_comment_lines_are_not_read() {
        let tail = "fn real() { q(\"UPDATE t SET status = 'b' WHERE status = 'a'\"); }\n\n#[cfg(test)]\nmod tests {\n    fn fixture() { q(\"UPDATE t SET status = 'z' WHERE status = 'y'\"); }\n}\n";
        assert_eq!(pairs("a.rs", tail), vec![pair("a", "b")]);

        let comments = "// UPDATE t SET status = 'b' WHERE status = 'a'\n/* UPDATE t SET status = 'b' WHERE status = 'a'\n * UPDATE t SET status = 'b' WHERE status = 'a'\n# UPDATE t SET status = 'b' WHERE status = 'a'\n-- UPDATE t SET status = 'b' WHERE status = 'a'\n";
        assert!(transitions("a.rs", comments).is_empty());

        let root = scratch("test-files");
        let statement = "fn f() { q(\"UPDATE t SET status = 'b' WHERE status = 'a'\"); }\n";
        write(&root, "src/real.rs", statement);
        write(&root, "tests/it.rs", statement);
        write(&root, "src/a.test.ts", statement);
        write(&root, "src/a.test.tsx", statement);
        write(&root, "pkg/a_test.go", statement);
        write(&root, "pkg/a_test.py", statement);
        write(&root, "pkg/test_b.py", statement);

        let found = machines(&root).expect("walk");

        let paths: Vec<&str> = found
            .machines
            .iter()
            .flat_map(|m| m.transitions.iter().map(|t| t.path.as_str()))
            .collect();
        assert_eq!(paths, vec!["src/real.rs"], "{found:?}");
        let _ = fs::remove_dir_all(&root);
    }

    /// AC6
    #[test]
    fn a_go_raw_string_and_a_sql_file_are_read_the_same_way() {
        let go = "package q\n\nfunc (r *Repo) Close(id int) {\n\tr.db.Exec(`UPDATE orders\n\t\tSET status = 'closed'\n\t\tWHERE id = ? AND status = 'open'`, id)\n}\n";
        let sql = "UPDATE orders SET status = 'open' WHERE status = 'draft';\n";

        let found = transitions("repo/orders.go", go);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].2.function.as_deref(), Some("Close"));
        assert_eq!(found[0].2.line, 4);
        assert_eq!(
            (found[0].2.from.as_str(), found[0].2.to.as_str()),
            ("open", "closed")
        );

        assert_eq!(pairs("db/migrate.sql", sql), vec![pair("draft", "open")]);
    }

    /// (from, to) pairs of the one enum machine a source gives.
    fn enum_pairs(source: &str) -> Vec<(String, String)> {
        let found = enum_machines("core/src/job.rs", source);
        assert_eq!(found.len(), 1, "{found:?}");
        let mut out: Vec<(String, String)> = found[0]
            .2
            .iter()
            .map(|t| (t.from.clone(), t.to.clone()))
            .collect();
        out.sort();
        out
    }

    /// An enum named `*State` draws its edges from what its variants' docs say: "Arrives from"
    /// and "Leaves for", read as the edges they name, anchored at the variant that says it.
    #[test]
    fn an_enum_state_draws_the_edges_its_docs_declare() {
        let source = "pub enum ItemState {\n    /// Waiting.\n    Pending,\n    /// Arrives from `Pending`; leaves for `Passed` (green) or `Failed` (red).\n    Running,\n    Passed,\n    /// Terminal.\n    Failed,\n}\n";

        let found = enum_machines("core/src/job.rs", source);

        assert_eq!(found.len(), 1, "{found:?}");
        let (name, variants, transitions) = &found[0];
        assert_eq!(name, "ItemState");
        assert_eq!(variants, &["Pending", "Running", "Passed", "Failed"]);
        assert_eq!(
            enum_pairs(source),
            vec![
                pair("Pending", "Running"),
                pair("Running", "Failed"),
                pair("Running", "Passed"),
            ]
        );
        let arrives = transitions
            .iter()
            .find(|t| t.from == "Pending")
            .expect("the arrival");
        assert_eq!(arrives.function, None);
        assert_eq!(arrives.path, "core/src/job.rs");
        assert_eq!(arrives.line, 5, "the line of `Running`");
    }

    /// Two variants naming one edge from both ends give it once, and a name that is not a variant
    /// of the enum (a function, another type) is not a state.
    #[test]
    fn an_enum_edge_named_from_both_ends_is_one_and_a_non_variant_is_no_state() {
        let source = "enum JobState {\n    /// Leaves for `Done`, and `next_step` picks it up. Also see `Other::Thing`.\n    Open,\n    /// Arrives from `Open`.\n    Done,\n}\n";

        assert_eq!(enum_pairs(source), vec![pair("Open", "Done")]);
    }

    /// An enum with no declared edge, one not named `*State`, and a test-module tail are nothing;
    /// struct-like variants keep their name and not their fields.
    #[test]
    fn an_enum_without_edges_or_without_the_name_or_in_a_test_tail_is_not_a_machine() {
        let quiet = "enum PanelState {\n    Open,\n    Closed,\n}\n";
        let unnamed = "enum Mode {\n    /// Leaves for `B`.\n    A,\n    B,\n}\n";
        let tail = "fn real() {}\n\n#[cfg(test)]\nmod tests {\n    enum FixtureState {\n        /// Leaves for `B`.\n        A,\n        B,\n    }\n}\n";
        let shaped = "enum RunState {\n    /// Leaves for `Stopped`.\n    Live { pid: u32 },\n    Stopped(i32),\n}\n";

        assert!(enum_machines("a.rs", quiet).is_empty());
        assert!(enum_machines("a.rs", unnamed).is_empty());
        assert!(enum_machines("a.rs", tail).is_empty());
        assert!(enum_machines("a.ts", shaped).is_empty(), "Rust only");

        let found = enum_machines("a.rs", shaped);
        assert_eq!(found[0].1, ["Live", "Stopped"]);
    }

    /// The walk lists an enum machine beside the SQL ones: table = the enum, column = `enum`, and
    /// every variant is a state, edges or not.
    #[test]
    fn the_walk_lists_an_enum_machine_with_every_variant_as_a_state() {
        let root = scratch("enum");
        write(
            &root,
            "core/src/job.rs",
            "pub enum JobState {\n    /// Leaves for `Done`.\n    Open,\n    Done,\n    Lonely,\n}\n",
        );

        let found = machines(&root).expect("the scratch folder should be readable");

        let machine = found
            .machines
            .iter()
            .find(|m| m.table == "JobState" && m.column == "enum")
            .unwrap_or_else(|| panic!("{:#?}", found.machines));
        assert_eq!(machine.states, ["Done", "Lonely", "Open"]);
        assert_eq!(machine.transitions.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    /// AC7
    #[test]
    fn the_walk_groups_transitions_by_table_and_column() {
        let root = scratch("group");
        write(
            &root,
            "a.rs",
            "fn one() { q(\"UPDATE zebra SET status = 'b' WHERE status = 'a'\"); }\nfn two() { q(\"UPDATE apple SET phase_state = 'y' WHERE phase_state = 'x'\"); }\nfn three() { q(\"UPDATE apple SET status = 'm' WHERE status = 'l'\"); }\n",
        );
        write(
            &root,
            "b.go",
            "package b\n\nfunc Four() {\n\tq(`UPDATE zebra SET status = 'c' WHERE status = 'b'`)\n}\n",
        );
        write(
            &root,
            "notes.bin",
            "UPDATE zebra SET status = 'q' WHERE status = 'p'",
        );

        let found = machines(&root).expect("walk");

        let keys: Vec<(&str, &str)> = found
            .machines
            .iter()
            .map(|m| (m.table.as_str(), m.column.as_str()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("apple", "phase_state"),
                ("apple", "status"),
                ("zebra", "status")
            ]
        );
        let zebra = &found.machines[2];
        assert_eq!(zebra.states, vec!["a", "b", "c"]);
        assert_eq!(zebra.transitions.len(), 2);
        assert_eq!(zebra.transitions[0].path, "a.rs");
        assert_eq!(zebra.transitions[1].path, "b.go");
        assert_eq!(
            found.files_read, 2,
            "only scanned files are read: {found:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// The checkout this test is *running in*, checked against the one it was *compiled in* (the
    /// same assertion as `map::seam`'s `repository()`).
    fn repository() -> PathBuf {
        let built_in = Path::new(env!("CARGO_MANIFEST_DIR"));
        let running_in = std::env::current_dir().expect("the working directory should be readable");
        assert_eq!(
            built_in,
            running_in.as_path(),
            "this binary was compiled in {} and is running in {} - a shared target directory has handed this checkout a binary built somewhere else. Touch this file to force a rebuild.",
            built_in.display(),
            running_in.display(),
        );
        running_in
            // The package, `core/crates/nucleos-app`, sits three levels below the root.
            .ancestors()
            .nth(3)
            .expect("the package sits inside the repository")
            .to_path_buf()
    }

    /// AC8, against the repository this module ships in. A text rule meets real SQL here, and a
    /// shape it misses shows up as a missing transition.
    #[test]
    fn this_repository_s_vcs_request_machine_is_found_with_its_anchors() {
        let root = repository();

        let found = machines(&root).expect("the repository should be readable");

        assert!(found.files_read > 100, "files_read = {}", found.files_read);
        let vcs = found
            .machines
            .iter()
            .find(|m| m.table == "vcs_requests" && m.column == "status")
            .expect("the vcs_requests/status machine");
        let expected = [
            (
                "queued",
                "running",
                "claim_next",
                "core/src/execution/vcs/queue.rs",
            ),
            (
                "running",
                "interrupted",
                "reconcile_interrupted",
                "core/src/execution/vcs/reconcile.rs",
            ),
            (
                "awaiting_owner",
                "queued",
                "approve_for_owner",
                "core/src/execution/vcs/tickets.rs",
            ),
            (
                "awaiting_owner",
                "rejected",
                "refuse_for_owner",
                "core/src/execution/vcs/tickets.rs",
            ),
            (
                "queued",
                "cancelled",
                "cancel_for_run",
                "core/src/execution/vcs/reconcile.rs",
            ),
            (
                "awaiting_approval",
                "cancelled",
                "cancel_for_run",
                "core/src/execution/vcs/reconcile.rs",
            ),
            (
                "running",
                "queued",
                "requeue_after_moved_target",
                "core/src/execution/vcs/drain.rs",
            ),
            (
                "running",
                COMPUTED,
                "finish",
                "core/src/execution/vcs/queue.rs",
            ),
        ];
        for (from, to, function, path) in expected {
            let hit = vcs.transitions.iter().find(|t| {
                t.from == from
                    && t.to == to
                    && t.function.as_deref() == Some(function)
                    && t.path.ends_with(path)
            });
            let hit = hit.unwrap_or_else(|| {
                panic!(
                    "missing {from} -> {to} in {function} ({path}): {:#?}",
                    vcs.transitions
                )
            });
            let text = fs::read_to_string(root.join(&hit.path)).expect("the anchor's file");
            let line = text
                .lines()
                .nth(hit.line - 1)
                .unwrap_or_else(|| panic!("line {} is past the end of {}", hit.line, hit.path));
            assert!(
                line.contains("UPDATE"),
                "{} line {} does not hold the statement: {line}",
                hit.path,
                hit.line
            );
        }
        // Every literal status UPDATE of vcs.rs sits below its test module.
        assert!(
            vcs.transitions
                .iter()
                .all(|t| t.path != "core/src/execution/vcs.rs"),
            "a transition came from a test module: {:#?}",
            vcs.transitions
        );
        let runs = found
            .machines
            .iter()
            .find(|m| m.table == "runs" && m.column == "status")
            .expect("the runs/status machine");
        assert!(
            runs.transitions
                .iter()
                .any(|t| t.from == "running" && t.to == "failed"),
            "{:#?}",
            runs.transitions
        );
    }

    /// The job queue's own state enum, read from this repository's docs: an item reaches
    /// `Merging` from `Implemented`, and every variant is listed as a state.
    #[test]
    fn this_repository_s_item_state_enum_is_a_machine_with_its_declared_edges() {
        let root = repository();

        let found = machines(&root).expect("the repository should be readable");

        let item = found
            .machines
            .iter()
            .find(|m| m.table == "ItemState" && m.column == "enum")
            .expect("the ItemState enum machine");
        assert!(item.states.len() >= 15, "{:?}", item.states);
        for (from, to) in [
            ("Implemented", "Merging"),
            ("Merging", "Conflicted"),
            ("Reverted", "GateRetriable"),
        ] {
            let hit = item
                .transitions
                .iter()
                .find(|t| t.from == from && t.to == to)
                .unwrap_or_else(|| panic!("missing {from} -> {to}: {:#?}", item.transitions));
            let text = fs::read_to_string(root.join(&hit.path)).expect("the anchor's file");
            let line = text.lines().nth(hit.line - 1).expect("the anchor's line");
            assert!(
                line.trim_start().starts_with(hit.to.as_str())
                    || line.trim_start().starts_with(hit.from.as_str()),
                "{} line {} is not a variant: {line}",
                hit.path,
                hit.line
            );
        }
    }
}
