//! The HTTP routes the owner's shell (and the channel adapters) share must stay registered.
//!
//! Phase 4 deletes the v1 Telegram sidecar and its daemon client. None of the routes that client
//! called is v1-only, so none may disappear with it. This reads the route registrations as text
//! (no compile dependency on the daemon's internals) and fails when one is dropped.

use std::fs;
use std::path::{Path, PathBuf};

/// Every `core/src/http.rs` and `core/src/http/**/*.rs` file, concatenated.
fn http_sources() -> String {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<PathBuf> = vec![src.join("http.rs")];
    let mut dirs = vec![src.join("http")];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).expect("read core/src/http") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
        .iter()
        .map(|f| fs::read_to_string(f).unwrap_or_else(|e| panic!("read {}: {e}", f.display())))
        .collect::<Vec<_>>()
        .join("\n")
}

/// True when `.route(` is followed (whitespace aside) by the string literal `"<path>"`.
fn registers(text: &str, path: &str) -> bool {
    let needle = format!("\"{path}\"");
    text.match_indices(".route(").any(|(at, m)| {
        text[at + m.len()..]
            .trim_start()
            .starts_with(needle.as_str())
    })
}

#[test]
fn shared_routes_stay_registered() {
    let text = http_sources();
    let routes = [
        "/assistant/message",
        "/assistant/{turn_id}",
        "/owner-notes",
        "/capture-requests",
        "/capture-requests/{job_id}/answer",
        "/proposals",
        "/proposals/refused-actions",
        "/proposals/{id}/approve",
        "/proposals/{id}/reject",
        "/notifications/policy",
        "/autopilot/kill",
        "/autopilot/budget",
        "/email/triage",
        "/email/queue",
        "/runs/{id}/cancel",
        "/feed",
        "/projects",
    ];
    let missing: Vec<&str> = routes
        .iter()
        .copied()
        .filter(|r| !registers(&text, r))
        .collect();
    assert!(
        missing.is_empty(),
        "routes no longer registered in core/src/http*: {missing:?}"
    );
}
