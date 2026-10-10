//! How much production code one core file may hold.
//!
//! A file past [`LIMIT`] lines of code is usually doing more than one thing, and the map can
//! only draw a module as one box. The count is code alone — no blank lines, no comments, nothing
//! from the file's test module on — because this crate writes long comments on purpose and keeps
//! its tests beside the code, and either would make the number say nothing about the code.
//!
//! The files already past the limit are listed with a ceiling in `file-size-baseline.txt`. They
//! may shrink and never grow; everything else stays under the limit. When this fails, split the
//! file by what it does — not into `x_a.rs` and `x_b.rs` to get under the number.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const LIMIT: usize = 1000;

/// Lines of production code in a Rust source: everything before the file's test module, without
/// blank lines, comment lines (`//`, `///`, `//!`, and `/* ... */` that start a line) or module
/// declarations (`pub mod batch;`) — a parent naming its children is the folder's index, not code,
/// and counting it would make moving a file into a folder grow the folder's parent.
///
/// The test module is the `#[cfg(test)]` that opens a `mod` with a body (`mod tests {`, `pub mod
/// tests {`), not one declaring a test-only file (`mod fixture;`). A
/// `#[cfg(test)]` on a single helper among the production code does not end it, so the helper
/// is counted. A comment after code on the same line leaves that line code.
fn code_lines(source: &str) -> usize {
    let lines: Vec<&str> = source.lines().collect();
    let mut count = 0;
    let mut in_block = false;
    for (at, line) in lines.iter().enumerate() {
        let line = line.trim_end_matches('\r');
        let opens_tests = lines
            .get(at + 1)
            .is_some_and(|next| opens_module(next) && !declares_module(next.trim()));
        if line == "#[cfg(test)]" && opens_tests {
            break;
        }
        let text = line.trim();
        if in_block {
            in_block = !text.contains("*/");
            continue;
        }
        // A declaration and the attribute right above it (`#[cfg(test)]` over `mod fixture;`).
        let gates_a_declaration = text.starts_with("#[")
            && lines
                .get(at + 1)
                .is_some_and(|next| declares_module(next.trim()));
        if text.is_empty() || text.starts_with("//") || declares_module(text) || gates_a_declaration
        {
            continue;
        }
        if text.starts_with("/*") {
            in_block = !text.contains("*/");
            continue;
        }
        count += 1;
    }
    count
}

/// Whether a line only declares a module that lives in its own file: `pub mod batch;`.
fn declares_module(text: &str) -> bool {
    opens_module(text) && text.ends_with(';') && !text.contains('{')
}

/// Whether a line opens a module: `mod x`, `pub mod x`, `pub(crate) mod x`.
fn opens_module(line: &str) -> bool {
    let mut rest = line.trim_start();
    if let Some(after) = rest.strip_prefix("pub") {
        rest = match after.strip_prefix('(') {
            Some(inner) => inner.split_once(") ").map_or("", |(_, tail)| tail),
            None => after.trim_start(),
        };
    }
    rest.starts_with("mod ")
}

/// The baseline: each listed file's path from the package root, and its ceiling.
fn baseline(text: &str) -> BTreeMap<String, usize> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (path, ceiling) = line
                .rsplit_once(' ')
                .unwrap_or_else(|| panic!("baseline line without a ceiling: {line}"));
            let ceiling = ceiling
                .parse()
                .unwrap_or_else(|_| panic!("baseline ceiling is not a number: {line}"));
            (path.trim().to_owned(), ceiling)
        })
        .collect()
}

/// What is wrong, given every file's count and the baseline. Empty when nothing is.
fn problems(counts: &BTreeMap<String, usize>, ceilings: &BTreeMap<String, usize>) -> Vec<String> {
    let mut found = Vec::new();
    for (path, &lines) in counts {
        match ceilings.get(path) {
            Some(&ceiling) if lines > ceiling => found.push(format!(
                "{path}: {lines} lines of code, past its ceiling of {ceiling}"
            )),
            None if lines > LIMIT => found.push(format!(
                "{path}: {lines} lines of code, past the limit of {LIMIT}"
            )),
            _ => {}
        }
    }
    for (path, &ceiling) in ceilings {
        match counts.get(path) {
            None => found.push(format!(
                "{path} is in file-size-baseline.txt and is not on disk: remove its line (a \
                 moved file takes its ceiling with it, under its new path)"
            )),
            Some(&lines) if lines <= LIMIT => found.push(format!(
                "{path} is down to {lines} lines: remove its line from file-size-baseline.txt"
            )),
            Some(_) if ceiling <= LIMIT => found.push(format!(
                "{path} has a ceiling of {ceiling}, at or under the limit: remove its line"
            )),
            _ => {}
        }
    }
    found
}

#[test]
fn no_core_file_holds_more_code_than_its_ceiling() {
    let built_in = Path::new(env!("CARGO_MANIFEST_DIR"));
    let running_in = std::env::current_dir().expect("the working directory should be readable");
    assert_eq!(
        built_in,
        running_in.as_path(),
        "this test binary was compiled in {} and is running in {} — a shared target directory has \
         handed this checkout a binary built somewhere else. Touch this file to force a rebuild.",
        built_in.display(),
        running_in.display(),
    );

    let counts = crate::source_scan::rust_files()
        .into_iter()
        .map(|path| {
            let name = path
                .strip_prefix(built_in)
                .expect("every source root is inside the package")
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let source = fs::read_to_string(&path).expect("a source file should be readable");
            (name, code_lines(&source))
        })
        .collect::<BTreeMap<_, _>>();
    assert!(
        counts.len() > 100,
        "the scan found only {} files",
        counts.len()
    );

    let ceilings = baseline(
        &fs::read_to_string(built_in.join("file-size-baseline.txt"))
            .expect("file-size-baseline.txt should be readable"),
    );

    let found = problems(&counts, &ceilings);
    assert!(
        found.is_empty(),
        "{}\n\nSplit a file by what it does, not into numbered parts to get under the number.",
        found.join("\n")
    );
}

#[test]
fn only_code_is_counted() {
    let source = "\
//! module doc
use std::fs;

/// item doc
pub mod child;
#[cfg(test)]
mod fixture;

fn a() {
    // a comment
    /* one line */
    /*
       a block
    */
    *value = 1; // a trailing comment stays code
}

#[cfg(test)]
fn helper() {}

#[cfg(test)]
pub(crate) mod tests {
    fn not_counted() {}
}
";
    // `use`, `fn a() {`, `*value = 1;`, `}`, `#[cfg(test)]`, `fn helper() {}`. The two module
    // declarations, and the attribute over one of them, are not code.
    assert_eq!(code_lines(source), 6);
}

#[test]
fn a_file_may_shrink_under_its_ceiling_but_not_grow_past_it() {
    let counts = |lines| BTreeMap::from([("src/big.rs".to_owned(), lines)]);
    let ceilings = BTreeMap::from([("src/big.rs".to_owned(), 1500)]);

    assert!(problems(&counts(1400), &ceilings).is_empty());
    assert_eq!(problems(&counts(1501), &ceilings).len(), 1);
    assert_eq!(
        problems(&counts(900), &ceilings).len(),
        1,
        "it leaves the list"
    );
}

#[test]
fn a_new_file_past_the_limit_fails_and_a_stale_line_fails() {
    let counts = BTreeMap::from([("src/new.rs".to_owned(), LIMIT + 1)]);
    let ceilings = BTreeMap::from([("src/gone.rs".to_owned(), 2000)]);

    let found = problems(&counts, &ceilings);
    assert_eq!(found.len(), 2, "{found:?}");
}
