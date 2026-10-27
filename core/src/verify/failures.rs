//! Per-test failure ids, read from a runner's output (task-claims spec 2026-10-22, section 3 Verify).
//!
//! `verify_runs` keeps a 4000-character tail, which cannot say *which* tests failed. The executor
//! therefore runs [`extract`] on the full output (up to 1 MiB, `gate::GATE_OUTPUT_CAP`) before that
//! tail is cut, and stores the result in `verify_runs.failed_tests`.
//!
//! Covered: libtest (pretty format), pytest, vitest, `go test`. Anything else, and any output whose
//! own failure count disagrees with the lines seen or that was truncated, yields `None`: the caller
//! then treats the unit as "no ids" and it falls to the group-level rules.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// What `gate::TailBuffer::render` prepends when the output exceeded the cap.
pub const TRUNCATION_MARK: &str = "…[output truncated; showing tail]";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failures {
    /// The runner's own id of every failed test, verbatim.
    pub tests: BTreeSet<String>,
    /// Best-effort repo-relative files the failures point at, `/`-separated; feeds the incident's
    /// `guard_paths` (the prober keeps only those that exist in its tree).
    pub files: BTreeSet<String>,
}

impl Failures {
    pub fn merge(&mut self, other: &Failures) {
        self.tests.extend(other.tests.iter().cloned());
        self.files.extend(other.files.iter().cloned());
    }

    /// Keeps `raw` as a file only if it is a plain relative path: absolute paths (a registry crate,
    /// another machine's checkout) and `..` say nothing about this repository.
    fn add_file(&mut self, raw: &str) {
        let path = raw.trim().replace('\\', "/");
        let path = path.trim_start_matches("./");
        let plain = !path.is_empty()
            && !path.starts_with('/')
            && !path.contains(':')
            && path.split('/').all(|segment| segment != "..");
        if plain {
            self.files.insert(path.to_owned());
        }
    }
}

enum Reading {
    /// The output has nothing of this runner's.
    Absent,
    /// The runner's markers are there but the list cannot be trusted to be whole.
    Incomplete,
    Ids(Failures),
}

type Reader = fn(&str) -> Reading;

/// Every runner found must be complete; the union of their ids, or `None`.
pub fn extract(output: &str) -> Option<Failures> {
    if output.trim_start().starts_with(TRUNCATION_MARK) {
        return None;
    }
    let mut found = Failures::default();
    for reader in [libtest as Reader, pytest, vitest, go] {
        match reader(output) {
            Reading::Absent => {}
            Reading::Incomplete => return None,
            Reading::Ids(ids) => found.merge(&ids),
        }
    }
    (!found.tests.is_empty()).then_some(found)
}

/// libtest: `test <path> ... FAILED` lines, confirmed by the sum of `N failed` over the
/// `test result:` lines. Files from `thread '…' panicked at <file>:<line>:<col>:`.
fn libtest(output: &str) -> Reading {
    let mut found = Failures::default();
    let mut seen = 0usize;
    let mut reported = 0usize;
    for line in output.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("test ") {
            if let Some(name) = rest.strip_suffix(" ... FAILED") {
                found.tests.insert(name.to_owned());
                seen += 1;
            } else if let Some(result) = rest.strip_prefix("result: ") {
                reported += libtest_failed_count(result);
            }
        } else if let Some((_, location)) = line.split_once(" panicked at ")
            && let Some(file) = panic_file(location)
        {
            found.add_file(file);
        }
    }
    // Compared with the lines seen, not with the set: two test binaries may fail the same name.
    match (seen, reported) {
        (0, 0) => Reading::Absent,
        (seen, reported) if seen == reported => Reading::Ids(found),
        _ => Reading::Incomplete,
    }
}

/// `FAILED. 1 passed; 2 failed; 0 ignored; …` -> 2.
fn libtest_failed_count(result: &str) -> usize {
    result
        .split(';')
        .find_map(|part| {
            let count = part.trim().strip_suffix(" failed")?;
            count.rsplit(' ').next()?.parse::<usize>().ok()
        })
        .unwrap_or(0)
}

/// `core\src\verify.rs:10:5:` -> `core\src\verify.rs`. Windows drive letters survive: only the last
/// two colon-separated fields are cut.
fn panic_file(location: &str) -> Option<&str> {
    let location = location.trim().trim_end_matches(':');
    let mut parts = location.rsplitn(3, ':');
    let (column, line, file) = (parts.next()?, parts.next()?, parts.next()?);
    let numeric = |field: &str| !field.is_empty() && field.chars().all(|c| c.is_ascii_digit());
    (numeric(column) && numeric(line)).then_some(file)
}

/// pytest: `FAILED <nodeid>[ - msg]` / `ERROR <nodeid>[ - msg]` summary lines (a node id names a
/// `.py` file, which keeps an application's own `ERROR …` log line out), confirmed by the footer
/// `N failed, N error(s), … in <time>`. File = the node id up to the first `::`.
fn pytest(output: &str) -> Reading {
    let mut found = Failures::default();
    let mut seen = 0usize;
    let mut footer: Option<usize> = None;
    for line in output.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("FAILED ").or_else(|| line.strip_prefix("ERROR ")) {
            let id = rest.split_once(" - ").map_or(rest, |(id, _)| id).trim();
            if id.contains(".py") {
                found.tests.insert(id.to_owned());
                found.add_file(id.split("::").next().unwrap_or(id));
                seen += 1;
            }
        } else if let Some(count) = pytest_footer(line) {
            footer = Some(count);
        }
    }
    match (seen, footer) {
        (0, None) | (0, Some(0)) => Reading::Absent,
        (seen, Some(count)) if seen == count => Reading::Ids(found),
        _ => Reading::Incomplete,
    }
}

/// `=== 1 failed, 1 error, 4 passed in 0.31s ===` (or the `-q` form without the `=`) -> 2.
/// `None` for any line that is not wholly made of `<n> <word>` parts followed by ` in <time>`.
fn pytest_footer(line: &str) -> Option<usize> {
    let body = line.trim().trim_matches('=').trim();
    let (counts, _) = body.rsplit_once(" in ")?;
    let mut failing = 0usize;
    for part in counts.split(',') {
        let (count, word) = part.trim().split_once(' ')?;
        let count: usize = count.parse().ok()?;
        if matches!(word.trim(), "failed" | "error" | "errors") {
            failing += count;
        }
    }
    Some(failing)
}

/// vitest: ` FAIL  <file> > <suite> > <test>` lines (the progress and the summary both print them; the
/// set dedupes), confirmed by `Tests  N failed | …`. File = text before the first ` > `. A file-level
/// `FAIL <file> [ … ]` has no ` > ` and is not a test.
fn vitest(output: &str) -> Reading {
    let mut found = Failures::default();
    let mut expected: Option<usize> = None;
    for line in output.lines() {
        let line = line.trim();
        if let Some(id) = line.strip_prefix("FAIL ").map(str::trim) {
            if let Some((file, _)) = id.split_once(" > ") {
                found.tests.insert(id.to_owned());
                found.add_file(file);
            }
        } else if let Some(summary) = line.strip_prefix("Tests ") {
            expected = Some(vitest_failed_count(summary));
        }
    }
    match (found.tests.len(), expected) {
        (0, None) | (0, Some(0)) => Reading::Absent,
        (seen, Some(count)) if seen == count => Reading::Ids(found),
        _ => Reading::Incomplete,
    }
}

/// `1 failed | 3 passed (4)` -> 1.
fn vitest_failed_count(summary: &str) -> usize {
    summary
        .split('|')
        .find_map(|part| {
            let (count, word) = part.trim().split_once(' ')?;
            word.starts_with("failed").then(|| count.parse::<usize>().ok()).flatten()
        })
        .unwrap_or(0)
}

/// go: `--- FAIL: <name>` lines (indented for subtests) are held until the `FAIL\t<pkg>\t…` line that
/// closes their package, then prefixed with it. `go test` prints no count line, so a name that never
/// meets a package line is the only way to see incompleteness. Files from `<x>_test.go:<line>:`
/// (relative to the package; the prober drops those that do not exist at the repository root).
fn go(output: &str) -> Reading {
    let mut found = Failures::default();
    let mut pending: Vec<String> = Vec::new();
    for line in output.lines() {
        let line = line.trim_end();
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("--- FAIL: ") {
            if let Some(name) = rest.split_whitespace().next() {
                pending.push(name.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("FAIL\t") {
            let package = rest.split_whitespace().next().unwrap_or_default();
            if !package.is_empty() {
                for name in pending.drain(..) {
                    found.tests.insert(format!("{package}.{name}"));
                }
            }
        } else if let Some(file) = go_file(trimmed) {
            found.add_file(&file);
        }
    }
    if !pending.is_empty() {
        Reading::Incomplete
    } else if found.tests.is_empty() {
        Reading::Absent
    } else {
        Reading::Ids(found)
    }
}

/// `x_test.go:12: boom` -> `x_test.go`.
fn go_file(line: &str) -> Option<String> {
    let token = line.split_whitespace().next()?;
    let (stem, after) = token.split_once(".go:")?;
    after.chars().next().filter(char::is_ascii_digit)?;
    (!stem.is_empty()).then(|| format!("{stem}.go"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(failures: &Failures) -> Vec<&str> {
        failures.tests.iter().map(String::as_str).collect()
    }

    #[test]
    fn libtest_failures_need_the_runners_own_count_to_agree() {
        let out = "running 2 tests\n\
                   test a::ok ... ok\n\
                   test verify::tests::boom ... FAILED\n\
                   test verify::tests::bang ... FAILED\n\n\
                   failures:\n\n\
                   ---- verify::tests::boom stdout ----\n\
                   thread 'verify::tests::boom' panicked at core\\src\\verify.rs:10:5:\n\
                   assertion failed\n\n\
                   test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let failures = extract(out).expect("ids");
        assert_eq!(ids(&failures), ["verify::tests::bang", "verify::tests::boom"]);
        assert!(failures.files.contains("core/src/verify.rs"), "{:?}", failures.files);

        // The runner says 3 failed but only 2 lines were seen: the list is incomplete.
        let short = out.replace("2 failed", "3 failed");
        assert_eq!(extract(&short), None);
    }

    #[test]
    fn libtest_keeps_relative_panic_files_only() {
        let out = "test a::b ... FAILED\n\
                   thread 'a::b' panicked at C:\\Users\\x\\.cargo\\registry\\dep.rs:1:2:\n\
                   thread 'a::b' panicked at ./core/src/a.rs:7:9:\n\
                   test result: FAILED. 0 passed; 1 failed; 0 ignored\n";
        let failures = extract(out).expect("ids");
        assert_eq!(failures.files.iter().map(String::as_str).collect::<Vec<_>>(), ["core/src/a.rs"]);
    }

    #[test]
    fn pytest_and_vitest_and_go_are_read() {
        let py = "=== short test summary info ===\n\
                  FAILED tests/test_a.py::test_b - AssertionError: no\n\
                  ERROR tests/test_c.py - ImportError\n\
                  === 1 failed, 1 error, 4 passed in 0.31s ===\n";
        let failures = extract(py).expect("pytest ids");
        assert_eq!(ids(&failures), ["tests/test_a.py::test_b", "tests/test_c.py"]);
        assert!(failures.files.contains("tests/test_a.py"));

        let vi = " FAIL  src/a.test.ts > suite > does x\n\
                 \n Test Files  1 failed (1)\n      Tests  1 failed | 3 passed (4)\n";
        let failures = extract(vi).expect("vitest ids");
        assert_eq!(ids(&failures), ["src/a.test.ts > suite > does x"]);
        assert!(failures.files.contains("src/a.test.ts"));

        let go = "--- FAIL: TestX (0.00s)\n    x_test.go:12: boom\nFAIL\nFAIL\texample.com/m/pkg\t0.01s\n";
        let failures = extract(go).expect("go ids");
        assert_eq!(ids(&failures), ["example.com/m/pkg.TestX"]);
        assert!(failures.files.contains("x_test.go"));
    }

    #[test]
    fn go_subtests_and_two_packages_get_their_own_package_prefix() {
        let go = "--- FAIL: TestA (0.00s)\n    --- FAIL: TestA/sub (0.00s)\nFAIL\nFAIL\texample.com/m/a\t0.01s\n\
                  --- FAIL: TestB (0.00s)\nFAIL\nFAIL\texample.com/m/b\t0.01s\n";
        let failures = extract(go).expect("go ids");
        assert_eq!(
            ids(&failures),
            ["example.com/m/a.TestA", "example.com/m/a.TestA/sub", "example.com/m/b.TestB"]
        );
    }

    #[test]
    fn a_go_failure_with_no_package_line_is_incomplete() {
        assert_eq!(extract("--- FAIL: TestX (0.00s)\n"), None);
    }

    #[test]
    fn nothing_usable_is_none() {
        assert_eq!(extract(""), None);
        assert_eq!(extract("error: could not compile `x`\n"), None);
        // Jest prints `FAIL <file>` and `Tests:`; neither is read.
        assert_eq!(extract("FAIL src/a.test.js\nTests:       1 failed, 3 passed, 4 total\n"), None);
        // A truncated tail may hide failures: never a partial list.
        let truncated = format!("{TRUNCATION_MARK}\ntest a ... FAILED\ntest result: FAILED. 0 passed; 1 failed;\n");
        assert_eq!(extract(&truncated), None);
        // Everything passed: nothing to report.
        assert_eq!(extract("test a ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; finished in 0.00s\n"), None);
    }

    #[test]
    fn a_mixed_gate_output_needs_every_detected_runner_complete() {
        let both = "test a::b ... FAILED\ntest result: FAILED. 0 passed; 1 failed;\n\
                    FAILED tests/t.py::x - boom\n=== 1 failed in 1s ===\n";
        let failures = extract(both).expect("union");
        assert_eq!(ids(&failures), ["a::b", "tests/t.py::x"]);
        let broken = both.replace("=== 1 failed in 1s ===", "=== 2 failed in 1s ===");
        assert_eq!(extract(&broken), None);
    }

    #[test]
    fn an_application_log_line_starting_with_error_is_not_a_pytest_failure() {
        let out = "ERROR something happened\ntest a ... FAILED\ntest result: FAILED. 0 passed; 1 failed;\n";
        assert_eq!(ids(&extract(out).expect("libtest only")), ["a"]);
    }
}
