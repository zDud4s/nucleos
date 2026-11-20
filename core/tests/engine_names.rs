//! The engine-name guard (spec 2026-11-18 motor-por-contrato, §6.9): outside `engines/adapters/`
//! the core never names an engine. A name branch is any of
//!
//! - an engine/runner enum or id constant: `Engine::Claude`, `Engine::Codex` (so `ChatEngine::…`
//!   too), `RunnerKind::Claude|Codex`, `EngineId::CLAUDE|CODEX`, `engines::CLAUDE|CODEX`;
//! - the literal `"claude"` / `"codex"` used as a comparison (`== "x"`, `!= "x"`, a match arm
//!   `"x" =>`, an or-pattern `"x" |`);
//! - an environment variable whose name starts with `CLAUDE_` or `CODEX_`.
//!
//! Test code is not scanned: a file named `tests.rs` / `tests_*.rs` / `*_tests.rs`, anything under a
//! `tests/` directory, and everything from the first `#[cfg(test)]` that is directly followed by an
//! inline `mod … {` to the end of the file.
//!
//! Every file that still matches today is in [`EXCEPTIONS`], with the reason. THE LIST MAY ONLY
//! SHRINK: never add a row to make a new match pass — route the new code through the descriptor
//! (`agents::capabilities`, `engines_descriptor`, the probes) instead. A row whose file no longer
//! matches fails the guard until the row is deleted, so the list cannot rot. The two permanent
//! rows are the named ones (`pinned_to_codex`, `codex_refusal`); the rest migrate in phases 2/3 of
//! the spec and each phase removes its rows.

use std::fs;
use std::path::{Path, PathBuf};

/// (path under `core/src`, why it still names an engine). May only shrink — see the module doc.
const EXCEPTIONS: &[(&str, &str)] = &[
    // Permanent until the 2026-11-03 gate moves them behind a capability.
    (
        "execution/runs/create.rs",
        "permanent: `pinned_to_codex` (the classifier governs tools unless the node is pinned to Codex)",
    ),
    (
        "models/route_advice.rs",
        "permanent: `codex_refusal` (Codex refuses a governed run shape) and the two RunnerKind rows",
    ),
    // The engine descriptor and its readers: they own the names until phase 2 moves them into the
    // adapters' contract.
    (
        "engines_descriptor.rs",
        "migrates in phase 2: the built-in descriptor rows (id, config dir variable)",
    ),
    (
        "agents/capabilities.rs",
        "migrates in phase 2: the `Engine` constants and the per-engine translate arms",
    ),
    (
        "agents/launch.rs",
        "migrates in phase 2: the subagent engine is Claude's translate table",
    ),
    (
        "agents/prefix.rs",
        "migrates in phase 2: the prefix translates its tools for Codex",
    ),
    (
        "agents/harness.rs",
        "migrates in phase 3: reads CLAUDE_CONFIG_DIR for the harness home",
    ),
    (
        "accounts/token.rs",
        "migrates in phase 3: the CLAUDE_CODE_OAUTH_TOKEN variable of the account token",
    ),
    (
        "assistant/slash/capabilities.rs",
        "migrates in phase 2: the `ChatEngine` constants",
    ),
    (
        "assistant/turn.rs",
        "migrates in phase 3: a Codex thread-id branch and CODEX_HOME",
    ),
    (
        "assistants.rs",
        "migrates in phase 2: the per-engine model and cli lookup",
    ),
    (
        "config/models.rs",
        "migrates in phase 2: the active-runner Codex branch",
    ),
    (
        "execution/runner/auth_watch.rs",
        "migrates in phase 3: the default engine of the auth watch",
    ),
    (
        "execution/runner/stream.rs",
        "migrates in phase 3: Claude Code env knobs (auto-compact window, background tasks)",
    ),
    (
        "fleet/daemon_runs.rs",
        "migrates in phase 2: the default engine of a daemon run",
    ),
    (
        "governance/autopilot.rs",
        "migrates in phase 3: the CLAUDE_CODE_AUTO_COMPACT_WINDOW settings key",
    ),
    (
        "models/model_catalog.rs",
        "migrates in phase 2: model id to engine, and CODEX_HOME for the catalogue",
    ),
    (
        "models/route_advice/engines.rs",
        "migrates in phase 2: the Codex sandbox arm and the Claude default primary",
    ),
    (
        "models/route_advice/probes.rs",
        "migrates in phase 2: the per-engine default model of the probes",
    ),
    (
        "team/launch.rs",
        "migrates in phase 2: Engine to RunnerKind and the Codex seat checks",
    ),
    (
        "tree_job/relaunch.rs",
        "migrates in phase 2: the default engine and the Claude-only resume",
    ),
];

const ENGINES: [&str; 2] = ["claude", "codex"];

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

/// `core/`. This file compiles into the `nucleos-daemon` binary's unit tests (see `src/main.rs`),
/// whose package is `core/crates/nucleos-daemon`, two levels below it.
fn core_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// A path relative to `core/src`, with forward slashes.
fn relative(src: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(src).unwrap_or(file);
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

fn is_exempt_path(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    rel.starts_with("engines/adapters/")
        || rel.split('/').any(|part| part == "tests")
        || name == "tests.rs"
        || name.starts_with("tests_")
        || name.ends_with("_tests.rs")
}

/// Is `text` followed by a character that ends an identifier (or by nothing)?
fn ends_word(after: &str) -> bool {
    !after
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// The line without a trailing `// …` comment (only when the `//` is outside a string literal).
fn code_of(line: &str) -> &str {
    let mut quotes = 0;
    let bytes = line.as_bytes();
    for i in 0..bytes.len() {
        match bytes[i] {
            b'"' if i == 0 || bytes[i - 1] != b'\\' => quotes += 1,
            b'/' if quotes % 2 == 0 && bytes.get(i + 1) == Some(&b'/') => return &line[..i],
            _ => {}
        }
    }
    line
}

/// The engine-name patterns on one line of code.
fn line_hit(code: &str) -> Option<String> {
    let names = [
        ("Engine::", ["Claude", "Codex"]),
        ("RunnerKind::", ["Claude", "Codex"]),
        ("EngineId::", ["CLAUDE", "CODEX"]),
        ("engines::", ["CLAUDE", "CODEX"]),
    ];
    for (prefix, variants) in names {
        for variant in variants {
            let needle = format!("{prefix}{variant}");
            let mut from = 0;
            while let Some(at) = code[from..].find(&needle) {
                let end = from + at + needle.len();
                if ends_word(&code[end..]) {
                    return Some(needle);
                }
                from = end;
            }
        }
    }
    for engine in ENGINES {
        let literal = format!("\"{engine}\"");
        let mut from = 0;
        while let Some(at) = code[from..].find(&literal) {
            let start = from + at;
            let end = start + literal.len();
            let before = code[..start].trim_end();
            let after = code[end..].trim_start();
            if before.ends_with("==")
                || before.ends_with("!=")
                || after.starts_with("=>")
                || (after.starts_with('|') && !after.starts_with("||"))
            {
                return Some(format!("comparison with {literal}"));
            }
            from = end;
        }
        let var = format!("\"{}_", engine.to_uppercase());
        if code.contains(&var) {
            return Some(format!("env var {}_…", engine.to_uppercase()));
        }
    }
    None
}

/// Is line `at` a `#[cfg(test)]` directly followed by an inline `mod … {`?
fn starts_test_mod(lines: &[&str], at: usize) -> bool {
    if lines[at].trim() != "#[cfg(test)]" {
        return false;
    }
    let Some(next) = lines[at + 1..]
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
    else {
        return false;
    };
    let item = next
        .strip_prefix("pub(crate) ")
        .or_else(|| next.strip_prefix("pub(super) "))
        .or_else(|| next.strip_prefix("pub "))
        .unwrap_or(next);
    item.starts_with("mod ") && item.ends_with('{')
}

/// Every engine-name hit in production code of one file, as (1-based line, what matched).
fn scan(text: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if starts_test_mod(&lines, i) {
            break;
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        if let Some(what) = line_hit(code_of(line)) {
            found.push((i + 1, what));
        }
    }
    found
}

/// Files of `core/src` with at least one hit: (relative path, hits).
fn offenders() -> Vec<(String, Vec<(usize, String)>)> {
    let src = core_root().join("src");
    let mut files = Vec::new();
    collect(&src, &mut files);
    files.sort();
    let mut out = Vec::new();
    for file in files {
        let rel = relative(&src, &file);
        if is_exempt_path(&rel) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        let hits = scan(&text);
        if !hits.is_empty() {
            out.push((rel, hits));
        }
    }
    out
}

fn excepted(rel: &str) -> bool {
    EXCEPTIONS.iter().any(|(path, _)| *path == rel)
}

#[test]
fn no_engine_name_outside_the_adapters_and_the_exceptions() {
    let mut report = Vec::new();
    for (rel, hits) in offenders() {
        if excepted(&rel) {
            continue;
        }
        for (line, what) in hits {
            report.push(format!("{rel}:{line}: {what}"));
        }
    }
    assert!(
        report.is_empty(),
        "an engine is named outside engines/adapters/ — ask the descriptor or a capability instead \
         (do NOT add an exception, the list only shrinks):\n{}",
        report.join("\n")
    );
}

#[test]
fn every_exception_still_names_an_engine() {
    let live: Vec<String> = offenders().into_iter().map(|(rel, _)| rel).collect();
    let stale: Vec<&str> = EXCEPTIONS
        .iter()
        .map(|(path, _)| *path)
        .filter(|path| !live.iter().any(|rel| rel == path))
        .collect();
    assert!(
        stale.is_empty(),
        "these files no longer name an engine — delete their rows from EXCEPTIONS (it only \
         shrinks): {stale:?}"
    );
    let mut seen = Vec::new();
    for (path, reason) in EXCEPTIONS {
        assert!(!reason.is_empty(), "{path} needs a reason");
        assert!(!seen.contains(path), "{path} is listed twice");
        seen.push(path);
    }
}

#[test]
fn the_scanner_flags_a_violating_snippet_and_ignores_the_allowed_ones() {
    let engine = ["Eng", "ine::Cl", "aude"].concat();
    let runner = ["Runner", "Kind::Co", "dex"].concat();
    let id = ["crate::eng", "ines::CO", "DEX"].concat();
    let eq = ["if name == \"cl", "aude\" {}"].concat();
    let arm = ["\"co", "dex\" => 1,"].concat();
    let env = ["std::env::var(\"CLA", "UDE_HOME\")"].concat();
    for snippet in [engine, runner, id, eq, arm, env] {
        let found = scan(&format!("fn f() {{\n    {snippet}\n}}\n"));
        assert_eq!(found.len(), 1, "{snippet}: {found:?}");
        assert_eq!(found[0].0, 2, "{snippet}");
    }

    let comment = ["// Eng", "ine::Cl", "aude is mentioned here"].concat();
    let trailing = ["let a = 1; // name == \"cl", "aude\""].concat();
    let argument = ["let bin = \"cl", "aude\".to_string();"].concat();
    let longer = ["Eng", "ine::Claude", "Code"].concat();
    let test_only = [
        "fn f() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let _ = Eng",
        "ine::Cl",
        "aude; }\n}\n",
    ]
    .concat();
    for snippet in [comment, trailing, argument, longer] {
        assert!(scan(&snippet).is_empty(), "{snippet}");
    }
    assert!(scan(&test_only).is_empty());
}
