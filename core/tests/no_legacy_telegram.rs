//! Phase-4 guards: the v1 Telegram plumbing is gone from the core. Each pattern is built with
//! `concat!` so this file never matches itself (it lives outside `src` anyway). A test is
//! `#[ignore]`d until the chunk that removes its pattern lands; that chunk drops the attribute.

use std::fs;
use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn sources() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect(&root.join("src"), &mut files);
    collect(&root.join("crates").join("nucleos-base").join("src"), &mut files);
    files
}

fn hits(pattern: &str) -> Vec<String> {
    let mut found = Vec::new();
    for file in sources() {
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            if line.contains(pattern) {
                found.push(format!("{}:{}", file.display(), n + 1));
            }
        }
    }
    found
}

fn assert_gone(pattern: &str) {
    let found = hits(pattern);
    assert!(
        found.is_empty(),
        "`{pattern}` must be gone, still at: {found:#?}"
    );
}

#[test]
fn no_origin_telegram_remains() {
    assert_gone(concat!("Origin", "::", "Telegram"));
}

#[test]
fn no_telegram_doctrine_remains() {
    assert_gone(concat!("telegram", "_doctrine"));
}

#[test]
fn no_telegram_env_remains() {
    assert_gone(concat!("telegram", "_env"));
}

#[test]
fn no_telegram_config_remains() {
    assert_gone(concat!("Telegram", "Config"));
    assert_gone(concat!("load_", "telegram", "_config"));
}

#[test]
fn no_telegram_origin_refusal_remains() {
    assert_gone(concat!("Telegram", "Origin"));
}

#[test]
fn no_telegram_sidecar_probe_remains() {
    assert_gone(concat!("telegram", "_sidecar_probe"));
}

/// Protects against deleting too much: the telegram binary name and the v2 manifest stay.
#[test]
fn the_telegram_binary_and_v2_manifest_stay() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let sidecar = fs::read_to_string(root.join("sidecar.rs")).expect("sidecar.rs is readable");
    assert!(
        sidecar.contains("pub const TELEGRAM: &str = \"telegram\";"),
        "sidecar::TELEGRAM must stay"
    );
    let registry =
        fs::read_to_string(root.join("channels").join("registry.rs")).expect("registry.rs");
    assert!(registry.contains("pub const TELEGRAM: Manifest"));
    assert!(registry.contains("health_name: \"telegram_sidecar\""));
}
