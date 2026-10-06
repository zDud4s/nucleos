//! Which groups of a project's test map a set of changed paths activates — spec
//! `.ai/specs/2026-10-05-selecao-de-testes-design.md` §3.2.
//!
//! [`select`] is pure: the map and the paths in, the units out. The one question that needs the
//! filesystem — which paths are safe to hand a test runner as arguments — is [`vet`], which the
//! caller asks first and passes in.
// Nothing outside the tests calls this until the F2a executor does; core is a binary crate, so
// clippy's `dead_code` would otherwise fail the gate.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;

use crate::tests_map::{DIRS, FILES, TestsMap};

/// One command to run, and why it was chosen.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Unit {
    pub group: String,
    pub argv: Vec<String>,
    pub reason: Reason,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    /// A path in `full_sweep` (or the map itself) was touched: every group runs.
    FullSweep { path: String },
    /// A path no group claims and `no_test` does not excuse was touched: every group runs.
    Unclaimed,
    /// These paths, all claimed by the group.
    Paths { paths: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Selection {
    pub units: Vec<Unit>,
    /// Paths nothing in the map accounts for. Not an error — the map is a conservative superset
    /// (spec D6) — but a sign it is incomplete, which the caller reports.
    pub unclaimed: Vec<String>,
    pub full: bool,
}

/// The units `changed` needs, under the rules of spec §3.2, in this order:
/// 1. a path in `full_sweep` runs every group's `command`;
/// 2. a path a group claims activates that group — before `no_test` is consulted;
/// 3. a path in `no_test` is ignored;
/// 4. any other path is unclaimed and runs every group's `command`.
///
/// An active group with a `select` template runs it filled with its paths that are in
/// `fillable` (see [`vet`]); with none of them fillable — every one deleted, say — it runs
/// `command`, which tests at least as much.
pub fn select(map: &TestsMap, changed: &[String], fillable: &BTreeSet<String>) -> Selection {
    let changed: BTreeSet<String> = changed.iter().map(|path| normalize(path)).collect();
    let sweep = changed.iter().find(|path| map.sweeps(path)).cloned();
    let unclaimed: Vec<String> = changed
        .iter()
        .filter(|path| {
            !map.sweeps(path)
                && !map.tests.groups.values().any(|group| group.claims(path))
                && !map.untested(path)
        })
        .cloned()
        .collect();
    let full = sweep.is_some() || !unclaimed.is_empty();

    let mut units = Vec::new();
    for (name, group) in &map.tests.groups {
        let command = || crate::gate::split_command(&group.command).unwrap_or_default();
        if full {
            let reason = match &sweep {
                Some(path) => Reason::FullSweep { path: path.clone() },
                None => Reason::Unclaimed,
            };
            units.push(Unit {
                group: name.clone(),
                argv: command(),
                reason,
            });
            continue;
        }
        let claimed: Vec<String> = changed
            .iter()
            .filter(|path| group.claims(path))
            .cloned()
            .collect();
        if claimed.is_empty() {
            continue;
        }
        let files: Vec<&String> = claimed
            .iter()
            .filter(|path| fillable.contains(*path))
            .collect();
        let argv = match &group.select {
            Some(template) if !files.is_empty() => fill(template, &files).unwrap_or_else(command),
            _ => command(),
        };
        units.push(Unit {
            group: name.clone(),
            argv,
            reason: Reason::Paths { paths: claimed },
        });
    }
    Selection {
        units,
        unclaimed,
        full,
    }
}

/// A template's words with each marker replaced by its paths, every one prefixed `./` so none
/// can begin with `-` (spec §3.2, rule 2). `{dirs}` is each path's directory once, `.` for the
/// root.
fn fill(template: &str, files: &[&String]) -> Option<Vec<String>> {
    let dirs: BTreeSet<String> = files
        .iter()
        .map(|path| match path.rsplit_once('/') {
            Some((dir, _)) => format!("./{dir}"),
            None => ".".to_string(),
        })
        .collect();
    let mut argv = Vec::new();
    for word in crate::gate::split_command(template).ok()? {
        match word.as_str() {
            FILES => argv.extend(files.iter().map(|path| format!("./{path}"))),
            DIRS => argv.extend(dirs.iter().cloned()),
            _ => argv.push(word),
        }
    }
    Some(argv)
}

fn normalize(path: &str) -> String {
    let path = path.replace('\\', "/");
    path.strip_prefix("./").unwrap_or(&path).to_string()
}

/// The paths among `paths` that may be handed to a test runner as arguments: relative, without
/// `..`, existing in `worktree`, and still inside it once symlinks are resolved (spec §3.2,
/// rule 1). They come from agents, so anything else is refused here rather than trusted. A deleted
/// path is not refused as an error — it still activates its group in [`select`] — it just cannot
/// be an argument.
pub fn vet(worktree: &Path, paths: &[String]) -> BTreeSet<String> {
    let Ok(root) = worktree.canonicalize() else {
        return BTreeSet::new();
    };
    paths
        .iter()
        .map(|path| normalize(path))
        .filter(|path| {
            !path.is_empty()
                && !path.starts_with('/')
                && !path.contains(':')
                && path
                    .split('/')
                    .all(|segment| segment != ".." && !segment.is_empty())
        })
        .filter(|path| {
            worktree
                .join(path)
                .canonicalize()
                .is_ok_and(|resolved| resolved.starts_with(&root))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_map::parse;

    const MAP: &str = "version: 1
tests:
  groups:
    core:
      paths: [core/]
      command: cargo test -p nucleos-core
    py:
      paths: ['**/*.py']
      command: pytest
      select: pytest -q -- {files}
    go:
      paths: [sidecars/]
      command: go test ./...
      select: go test {dirs}
  full_sweep: [scripts/gates.sh]
  no_test: [docs/, core/README.md]
";

    fn map() -> TestsMap {
        parse(MAP).unwrap()
    }

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|p| p.to_string()).collect()
    }

    fn all_fillable(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn a_path_in_one_group_runs_only_that_group() {
        let changed = paths(&["core/src/main.rs"]);
        let selection = select(&map(), &changed, &all_fillable(&["core/src/main.rs"]));
        assert!(!selection.full);
        assert_eq!(selection.units.len(), 1);
        assert_eq!(selection.units[0].group, "core");
        assert_eq!(
            selection.units[0].argv,
            ["cargo", "test", "-p", "nucleos-core"]
        );
    }

    #[test]
    fn a_select_template_gets_its_files_prefixed_so_none_reads_as_a_flag() {
        let changed = paths(&["tools/-rf.py", "a.py"]);
        let selection = select(&map(), &changed, &all_fillable(&["tools/-rf.py", "a.py"]));
        assert_eq!(
            selection.units[0].argv,
            ["pytest", "-q", "--", "./a.py", "./tools/-rf.py"]
        );
    }

    #[test]
    fn dirs_are_listed_once_each() {
        let changed = paths(&[
            "sidecars/echo/a.go",
            "sidecars/echo/b.go",
            "sidecars/web/c.go",
        ]);
        let selection = select(
            &map(),
            &changed,
            &all_fillable(&[
                "sidecars/echo/a.go",
                "sidecars/echo/b.go",
                "sidecars/web/c.go",
            ]),
        );
        assert_eq!(
            selection.units[0].argv,
            ["go", "test", "./sidecars/echo", "./sidecars/web"]
        );
    }

    #[test]
    fn a_deleted_path_still_activates_its_group_and_falls_back_to_the_whole_command() {
        let changed = paths(&["sidecars/echo/gone.go"]);
        let selection = select(&map(), &changed, &BTreeSet::new());
        assert_eq!(selection.units[0].argv, ["go", "test", "./..."]);
    }

    #[test]
    fn a_group_claims_a_path_before_no_test_can_excuse_it() {
        let selection = select(&map(), &paths(&["core/README.md"]), &BTreeSet::new());
        assert_eq!(selection.units.len(), 1);
        assert_eq!(selection.units[0].group, "core");
    }

    #[test]
    fn a_path_in_no_test_runs_nothing() {
        let selection = select(&map(), &paths(&["docs/x.md"]), &BTreeSet::new());
        assert!(selection.units.is_empty() && !selection.full);
    }

    #[test]
    fn an_unclaimed_path_runs_every_group_whole_and_is_reported() {
        let selection = select(&map(), &paths(&["README.md"]), &BTreeSet::new());
        assert!(selection.full);
        assert_eq!(selection.unclaimed, ["README.md"]);
        assert_eq!(selection.units.len(), 3);
        assert!(
            selection
                .units
                .iter()
                .all(|unit| unit.reason == Reason::Unclaimed)
        );
        assert!(selection.units.iter().all(|unit| {
            !unit
                .argv
                .iter()
                .any(|w| w.starts_with("./") && w != "./...")
        }));
    }

    #[test]
    fn touching_the_map_or_a_full_sweep_path_runs_every_group() {
        for path in [crate::tests_map::MAP_FILE, "scripts/gates.sh"] {
            let selection = select(&map(), &paths(&[path]), &BTreeSet::new());
            assert!(selection.full, "{path}");
            assert_eq!(selection.units.len(), 3);
        }
    }

    #[test]
    fn vet_keeps_only_existing_paths_inside_the_worktree() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("core")).unwrap();
        std::fs::write(temp.path().join("core/a.rs"), "").unwrap();
        let vetted = vet(
            temp.path(),
            &paths(&[
                "core/a.rs",
                "core/gone.rs",
                "../x",
                "/etc/passwd",
                "core//a.rs",
            ]),
        );
        assert_eq!(vetted.into_iter().collect::<Vec<_>>(), ["core/a.rs"]);
    }

    /// This repository's own map, read from the checkout the tests were compiled in. It must parse,
    /// and every tracked file must be accounted for: an unclaimed path is legal (it runs everything)
    /// but in this repository it means the map fell behind the tree.
    #[test]
    fn this_repository_map_claims_every_tracked_file() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let map = match crate::tests_map::load(root) {
            crate::tests_map::MapState::Valid(map) => map,
            other => panic!("{}: {other:?}", crate::tests_map::MAP_FILE),
        };
        let output = std::process::Command::new("git")
            .args(["ls-files", "-z"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success());
        let tracked: Vec<String> = String::from_utf8(output.stdout)
            .unwrap()
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        let unclaimed: Vec<&String> = tracked
            .iter()
            .filter(|path| {
                !map.sweeps(path)
                    && !map.tests.groups.values().any(|group| group.claims(path))
                    && !map.untested(path)
            })
            .collect();
        assert!(
            unclaimed.is_empty(),
            "add these to a group or to no_test in {}: {unclaimed:?}",
            crate::tests_map::MAP_FILE
        );
    }
}
