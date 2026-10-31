//! Shared helpers for the scanner tests. Every test goes through `command_names`.

use super::scanner::command_names;

/// `src` must read as exactly `want`.
pub(super) fn ok(src: &str, want: &[&str]) {
    let want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
    assert_eq!(command_names(src), Ok(want), "source: {src:?}");
}

/// `src` must be refused (fail closed).
pub(super) fn err(src: &str) {
    let got = command_names(src);
    assert!(got.is_err(), "expected Err for {src:?}, got {got:?}");
}
