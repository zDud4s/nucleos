//! The test map a project keeps at its root, `nucleos.tests.yaml`: which groups of tests it has,
//! which changed paths activate each group, and which tools only the daemon may run.
//!
//! Reading and checking only. Nothing here runs a command — `gate.rs` does today and the
//! executor will — and nothing writes the file: `detect::propose_tests_map` proposes one, and the
//! owner commits it like any other change (spec `.ai/specs/2026-10-05-selecao-de-testes-design.md`
//! §3.1, §3.3).
// The matcher and the map's queries are called by `test_select`, whose caller is the F2a
// executor; until it lands only the tests reach them, and core is a binary crate.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Where the map lives, relative to the project root. At the root and not under `.nucleos/`,
/// because the daemon puts `/.nucleos/` in every managed repository's `info/exclude`, and a map
/// there would never be versioned.
pub const MAP_FILE: &str = "nucleos.tests.yaml";

/// The one format version this daemon reads.
pub const VERSION: u32 = 1;

/// A map is a hand-written list of groups; anything this size is not one.
const MAX_MAP: u64 = 256 * 1024;

/// The markers a `select` template may carry, each a whole argv word (spec §3.2). The
/// language-aware ones (`{modules}`, `{packages}`, `{crates}`) are F4 and refused until then.
pub const FILES: &str = "{files}";
pub const DIRS: &str = "{dirs}";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestsMap {
    pub version: u32,
    pub tests: Tests,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tests {
    #[serde(default)]
    pub groups: BTreeMap<String, Group>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_entrypoints: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, Tool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub warm: BTreeMap<String, Warm>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wrappers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub full_sweep: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub no_test: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reads: Option<Vec<String>>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub cache: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// Also hash ignored files matching `reads` into the fingerprint. Needs `reads`: without it
    /// the whole ignored tree, build output included, would be hashed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_ignored: bool,
}

/// A verification tool only the daemon may run. `allow` lists the subcommands that stay open to
/// sessions — managing dependencies, formatting, reading metadata — so refusing `cargo` does not
/// refuse `cargo fmt`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
}

/// Build state the daemon keeps per worktree, under the variable that points a tool at it.
/// `dir` is relative to the worktree, or `shared:<dir>` for one directory every worktree of the
/// project shares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Warm {
    pub dir: String,
    #[serde(default, skip_serializing_if = "Seed::is_none")]
    pub seed: Seed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Seed {
    #[default]
    None,
    Copy,
}

impl Seed {
    fn is_none(&self) -> bool {
        *self == Seed::None
    }
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Whether `path` (repository-relative, forward slashes) is matched by `pattern`.
///
/// Two kinds of pattern, because maps are mostly written as the first:
/// - **no wildcard:** a prefix. `core/` matches everything under it; `Cargo.toml` matches that
///   file and, were it a directory, everything under it.
/// - **a glob:** `**` crosses directories (and `**/` may match none), `*` and `?` stay within
///   one. `[` is literal — no map needs a character class, and an unsupported one that silently
///   matched nothing would read as a working map.
///
/// Case-sensitive, as git paths are.
pub fn matches(pattern: &str, path: &str) -> bool {
    if !has_wildcard(pattern) {
        return if pattern.ends_with('/') {
            path.starts_with(pattern)
        } else {
            path == pattern
                || (path.starts_with(pattern) && path.as_bytes().get(pattern.len()) == Some(&b'/'))
        };
    }
    glob(pattern.as_bytes(), path.as_bytes())
}

fn has_wildcard(pattern: &str) -> bool {
    pattern.contains(['*', '?'])
}

fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match pattern {
        [] => text.is_empty(),
        [b'*', b'*', b'/', rest @ ..] => {
            glob(rest, text)
                || (0..text.len()).any(|i| text[i] == b'/' && glob(rest, &text[i + 1..]))
        }
        [b'*', b'*', rest @ ..] => (0..=text.len()).any(|i| glob(rest, &text[i..])),
        [b'*', rest @ ..] => (0..=text.len())
            .take_while(|&i| i == 0 || text[i - 1] != b'/')
            .any(|i| glob(rest, &text[i..])),
        [b'?', rest @ ..] => !text.is_empty() && text[0] != b'/' && glob(rest, &text[1..]),
        [c, rest @ ..] => text.first() == Some(c) && glob(rest, &text[1..]),
    }
}

impl Group {
    pub fn claims(&self, path: &str) -> bool {
        self.paths.iter().any(|pattern| matches(pattern, path))
    }
}

impl TestsMap {
    /// Whether touching `path` runs every group. The map itself always does, whatever its own
    /// `full_sweep` says (spec §3.4, defence 3): a map that dropped itself from the list would
    /// otherwise let the change that shrinks it run fewer tests.
    pub fn sweeps(&self, path: &str) -> bool {
        path == MAP_FILE
            || self
                .tests
                .full_sweep
                .iter()
                .any(|pattern| matches(pattern, path))
    }

    pub fn untested(&self, path: &str) -> bool {
        self.tests
            .no_test
            .iter()
            .any(|pattern| matches(pattern, path))
    }
}

/// What a project's root holds, as far as the test map goes.
#[derive(Debug, Clone, PartialEq)]
pub enum MapState {
    /// No `nucleos.tests.yaml`: the project keeps today's behaviour (spec D10).
    Absent,
    /// A file that is not a map this daemon can use, with every reason at once — somebody fixing
    /// a map should not have to fix it one error per round trip.
    Invalid(Vec<String>),
    Valid(TestsMap),
}

/// The map at `root`, read from disk. Reading from a SHA, for bisection, is F3.
pub fn load(root: &Path) -> MapState {
    let path = root.join(MAP_FILE);
    match std::fs::metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return MapState::Absent,
        Err(error) => return MapState::Invalid(vec![format!("cannot read {MAP_FILE}: {error}")]),
        Ok(meta) if meta.len() > MAX_MAP => {
            return MapState::Invalid(vec![format!(
                "{MAP_FILE} is {} bytes; a map is at most {MAX_MAP}",
                meta.len()
            )]);
        }
        Ok(_) => {}
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => match parse(&text) {
            Ok(map) => MapState::Valid(map),
            Err(errors) => MapState::Invalid(errors),
        },
        Err(error) => MapState::Invalid(vec![format!("cannot read {MAP_FILE}: {error}")]),
    }
}

/// Parse and check a map. Every obligation of spec §3.2 that can be checked without running
/// anything is checked here, so a map that loads is one the selector and the executor can use
/// as written.
pub fn parse(text: &str) -> Result<TestsMap, Vec<String>> {
    let map: TestsMap =
        serde_yaml::from_str(text).map_err(|error| vec![format!("not a test map: {error}")])?;
    let mut errors = Vec::new();
    if map.version != VERSION {
        errors.push(format!(
            "version {} is not one this daemon reads (it reads {VERSION})",
            map.version
        ));
    }
    for (name, group) in &map.tests.groups {
        check_group(name, group, &mut errors);
    }
    for (key, patterns) in [
        ("full_sweep", &map.tests.full_sweep),
        ("no_test", &map.tests.no_test),
        ("gate_entrypoints", &map.tests.gate_entrypoints),
    ] {
        for pattern in patterns {
            check_pattern(key, pattern, &mut errors);
        }
    }
    for name in map.tests.tools.keys().chain(map.tests.wrappers.iter()) {
        if name.is_empty() || name.contains(['/', '\\', ' ']) {
            errors.push(format!("tools/wrappers: `{name}` is not a program name"));
        }
    }
    for (variable, warm) in &map.tests.warm {
        if !is_env_name(variable) {
            errors.push(format!(
                "warm: `{variable}` is not an environment variable name"
            ));
        }
        let dir = warm.dir.strip_prefix("shared:").unwrap_or(&warm.dir);
        if has_wildcard(dir) || !is_relative_inside(dir) {
            errors.push(format!(
                "warm.{variable}.dir: `{}` must be a relative directory without `..` or wildcards",
                warm.dir
            ));
        }
    }
    if errors.is_empty() {
        Ok(map)
    } else {
        Err(errors)
    }
}

fn check_group(name: &str, group: &Group, errors: &mut Vec<String>) {
    let at = format!("groups.{name}");
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        errors.push(format!(
            "{at}: a group name is letters, digits, `-` and `_`"
        ));
    }
    if group.paths.is_empty() {
        errors.push(format!(
            "{at}.paths: a group nothing activates would never run"
        ));
    }
    for pattern in &group.paths {
        check_pattern(&format!("{at}.paths"), pattern, errors);
    }
    for (key, command) in [
        ("command", Some(&group.command)),
        ("check", group.check.as_ref()),
    ] {
        let Some(command) = command else { continue };
        match crate::gate::split_command(command) {
            Ok(words) if words.is_empty() => errors.push(format!("{at}.{key}: empty")),
            Ok(words)
                if words
                    .iter()
                    .any(|word| word.contains('{') && word.contains('}')) =>
            {
                errors.push(format!("{at}.{key}: markers belong in `select`, not here"))
            }
            Ok(_) => {}
            Err(error) => errors.push(format!("{at}.{key}: {error}")),
        }
    }
    if let Some(select) = &group.select {
        check_select(&at, select, errors);
    }
    if let Some(reads) = &group.reads {
        for pattern in reads {
            check_pattern(&format!("{at}.reads"), pattern, errors);
        }
        // Spec §3.2: a group may not be activated by a file it does not read.
        for path in &group.paths {
            if !covered_by(path, reads) {
                errors.push(format!(
                    "{at}: `{path}` activates the group but is not inside its `reads`"
                ));
            }
        }
    }
    if group.include_ignored && group.reads.is_none() {
        errors.push(format!(
            "{at}.include_ignored: needs `reads`; without it the whole ignored tree (build output included) would be hashed"
        ));
    }
    for variable in &group.env {
        if !is_env_name(variable) {
            errors.push(format!(
                "{at}.env: `{variable}` is not an environment variable name"
            ));
        }
    }
}

/// A `select` template: splits like a gate command, carries at least one marker, every marker is
/// a whole word and a known one, and when the template declares `--` every marker comes after it
/// (spec §3.2, rule 3) — so a path can never be read as a flag.
fn check_select(at: &str, select: &str, errors: &mut Vec<String>) {
    let words = match crate::gate::split_command(select) {
        Ok(words) => words,
        Err(error) => {
            errors.push(format!("{at}.select: {error}"));
            return;
        }
    };
    let separator = words.iter().position(|word| word == "--");
    let mut markers = 0;
    for (index, word) in words.iter().enumerate() {
        if word == FILES || word == DIRS {
            markers += 1;
            if separator.is_some_and(|separator| index < separator) {
                errors.push(format!("{at}.select: `{word}` comes before `--`"));
            }
        } else if word.contains('{') && word.contains('}') {
            errors.push(format!(
                "{at}.select: `{word}` is not a marker this daemon fills (it fills {FILES} and {DIRS}, each as a whole word)"
            ));
        }
    }
    if markers == 0 {
        errors.push(format!(
            "{at}.select: carries no marker, so it would run the same tests as `command`"
        ));
    }
}

fn check_pattern(at: &str, pattern: &str, errors: &mut Vec<String>) {
    if !is_relative_inside(pattern.trim_end_matches('/')) {
        errors.push(format!(
            "{at}: `{pattern}` must be relative to the repository root, with forward slashes and no `..`"
        ));
    }
}

/// Relative, forward slashes, no `..` segment, no drive, not empty.
fn is_relative_inside(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && path.split('/').all(|segment| segment != "..")
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether every path `pattern` can match is matched by one of `reads`. Conservative: it proves
/// coverage for the shapes maps are written in — the same pattern, a literal directory above the
/// pattern's literal start, or `**` — and says no otherwise, so the owner widens `reads` rather
/// than the daemon assuming.
fn covered_by(pattern: &str, reads: &[String]) -> bool {
    let start = &pattern[..pattern.find(['*', '?']).unwrap_or(pattern.len())];
    reads.iter().any(|read| {
        read == pattern
            || read == "**"
            || (!has_wildcard(read)
                && if read.ends_with('/') {
                    start.starts_with(read.as_str())
                } else {
                    start == read
                        || (start.starts_with(read.as_str())
                            && start.as_bytes().get(read.len()) == Some(&b'/'))
                })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_without_a_wildcard_is_a_prefix_on_whole_segments() {
        assert!(matches("core/", "core/src/main.rs"));
        assert!(matches("core", "core/src/main.rs"));
        assert!(matches("Cargo.toml", "Cargo.toml"));
        // `core` must not claim `core-extra/`, which only shares its first letters.
        assert!(!matches("core", "core-extra/x.rs"));
        assert!(!matches("core/", "shell/core/x.rs"));
    }

    #[test]
    fn a_single_star_stays_inside_one_directory() {
        assert!(matches("scripts/*.py", "scripts/heavy.py"));
        assert!(!matches("scripts/*.py", "scripts/eval/run.py"));
        assert!(matches("scripts/?.sh", "scripts/a.sh"));
    }

    #[test]
    fn a_double_star_crosses_directories_and_may_match_none() {
        assert!(matches("**/*.go", "main.go"));
        assert!(matches("**/*.go", "sidecars/echo/main.go"));
        assert!(matches("core/**/x.rs", "core/x.rs"));
        assert!(matches("core/**/x.rs", "core/a/b/x.rs"));
        assert!(matches("core/**", "core/a/b"));
    }

    #[test]
    fn paths_are_matched_case_sensitively() {
        assert!(!matches("Core/", "core/main.rs"));
        assert!(!matches("*.RS", "main.rs"));
    }

    const MINIMAL: &str = "version: 1\ntests:\n  groups:\n    core:\n      paths: [core/]\n      command: cargo test -p nucleos-core\n";

    fn errors(text: &str) -> Vec<String> {
        parse(text).expect_err("the map should be refused")
    }

    #[test]
    fn a_minimal_map_parses_with_the_defaults_filled_in() {
        let map = parse(MINIMAL).unwrap();
        let core = &map.tests.groups["core"];
        assert!(core.cache, "cache defaults to true");
        assert!(!core.include_ignored);
        assert!(map.tests.full_sweep.is_empty());
    }

    #[test]
    fn a_misspelt_key_is_refused_rather_than_ignored() {
        // `comand` in a hand-written map would otherwise leave a group with no command at all.
        let text = MINIMAL.replace("command:", "comand:");
        assert!(errors(&text)[0].contains("not a test map"));
    }

    #[test]
    fn a_newer_version_is_refused() {
        let text = MINIMAL.replace("version: 1", "version: 2");
        assert!(errors(&text).iter().any(|e| e.contains("version 2")));
    }

    #[test]
    fn a_select_template_is_held_to_the_marker_rules() {
        let with = |select: &str| format!("{MINIMAL}      select: {select}\n");
        assert!(parse(&with("pytest {dirs}")).is_ok());
        assert!(parse(&with("cargo test -- {files}")).is_ok());
        assert!(
            errors(&with("pytest -q"))
                .iter()
                .any(|e| e.contains("no marker"))
        );
        assert!(
            errors(&with("pytest --x={files}"))
                .iter()
                .any(|e| e.contains("not a marker"))
        );
        assert!(
            errors(&with("pytest {modules}"))
                .iter()
                .any(|e| e.contains("not a marker"))
        );
        assert!(
            errors(&with("run {files} -- x"))
                .iter()
                .any(|e| e.contains("before `--`"))
        );
    }

    #[test]
    fn markers_are_refused_in_command_and_check() {
        let text = MINIMAL.replace("cargo test -p nucleos-core", "pytest {files}");
        assert!(
            errors(&text)
                .iter()
                .any(|e| e.contains("belong in `select`"))
        );
    }

    #[test]
    fn an_unterminated_quote_is_reported_where_it_is() {
        let text = MINIMAL.replace("cargo test -p nucleos-core", "cargo test \"x");
        assert!(
            errors(&text)
                .iter()
                .any(|e| e.starts_with("groups.core.command"))
        );
    }

    #[test]
    fn a_path_outside_its_reads_is_refused() {
        let ok = format!("{MINIMAL}      reads: [core/, Cargo.lock]\n");
        assert!(parse(&ok).is_ok());
        let narrowed = format!("{MINIMAL}      reads: [core/src/]\n");
        assert!(
            errors(&narrowed)
                .iter()
                .any(|e| e.contains("not inside its `reads`"))
        );
        let glob = MINIMAL.replace("[core/]", "[core/src/*.rs]") + "      reads: [core/]\n";
        assert!(
            parse(&glob).is_ok(),
            "a literal directory covers a glob that starts under it"
        );
    }

    #[test]
    fn patterns_must_stay_inside_the_repository() {
        for bad in ["../x", "/etc", "C:/x", "core\\\\src"] {
            let text = MINIMAL.replace("[core/]", &format!("['{bad}']"));
            assert!(!errors(&text).is_empty(), "{bad} should be refused");
        }
    }

    #[test]
    fn warm_state_is_a_relative_directory_or_a_shared_one() {
        let base = format!("{MINIMAL}  warm:\n");
        assert!(parse(&format!("{base}    CARGO_TARGET_DIR: {{ dir: target }}\n")).is_ok());
        assert!(
            parse(&format!(
                "{base}    GOCACHE: {{ dir: 'shared:gocache', seed: copy }}\n"
            ))
            .is_ok()
        );
        assert!(!errors(&format!("{base}    X: {{ dir: ../out }}\n")).is_empty());
        assert!(!errors(&format!("{base}    1X: {{ dir: out }}\n")).is_empty());
    }

    #[test]
    fn every_error_is_reported_at_once() {
        let text =
            "version: 3\ntests:\n  groups:\n    'bad name':\n      paths: []\n      command: ''\n";
        assert!(errors(text).len() >= 3, "{:?}", errors(text));
    }

    #[test]
    fn the_map_itself_always_sweeps_whatever_it_says() {
        let map = parse(MINIMAL).unwrap();
        assert!(map.sweeps(MAP_FILE));
        assert!(!map.sweeps("core/src/main.rs"));
    }

    #[test]
    fn load_tells_absent_from_invalid_from_valid() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(load(temp.path()), MapState::Absent);
        std::fs::write(temp.path().join(MAP_FILE), "version: 1\ntests: [").unwrap();
        assert!(matches!(load(temp.path()), MapState::Invalid(_)));
        std::fs::write(temp.path().join(MAP_FILE), MINIMAL).unwrap();
        assert!(matches!(load(temp.path()), MapState::Valid(_)));
    }

    #[test]
    fn include_ignored_without_reads_is_refused() {
        let text = format!("{MINIMAL}      include_ignored: true\n");
        let found = errors(&text);
        assert!(
            found
                .iter()
                .any(|e| e.contains("include_ignored") && e.contains("reads")),
            "{found:?}"
        );
    }

    #[test]
    fn include_ignored_with_reads_is_accepted() {
        let text = format!("{MINIMAL}      reads: [core/]\n      include_ignored: true\n");
        let map = parse(&text).unwrap();
        assert!(map.tests.groups["core"].include_ignored);
    }
}
