//! Source guard for the single spawn door (spec D3): an agent or gate process is started by
//! `sandbox.rs` and by nothing else.
//!
//! A text scan, because there is no type that stops a module from calling `Command::new`. It looks
//! for the primitives that start a tracked child (`spawn_in_own_group`, `TreeKiller::new`) and for
//! a `Command::new` naming an engine CLI. The sites that have not moved behind the door yet sit in
//! [`PENDING_MIGRATION`]; the branch that wires them (sbx1-wire) empties that list, and the stale
//! check below fails the day an entry stops spawning, so the list can only shrink.

use crate::source_scan;

/// Agent and gate spawn sites still outside the door. Each one is deleted by sbx1-wire.
const PENDING_MIGRATION: &[(&str, &str)] = &[
    ("execution/runner/claude.rs", "ClaudeCliRunner"),
    ("execution/runner/codex.rs", "CodexCliRunner"),
    // Not named in the spec, found by this scan: the third CLI runner spawns the same way.
    ("execution/runner/agy.rs", "agy runner"),
    ("gate.rs", "gate::run_gate"),
];

/// Users of the same primitives that start neither an agent nor a gate: git and `gh` transport, and
/// a kill-only sweep. They stay outside the door on purpose.
const NOT_AGENT_OR_GATE: &[(&str, &str)] = &[
    ("execution/git_exec/transport.rs", "git transport"),
    ("integrations/github/exec.rs", "gh transport"),
    ("conductor/sweep.rs", "kill-only sweep of a stale pid"),
];

/// Files that may spawn freely: the door itself and the home of the primitives.
fn is_door_or_primitive(module: &str) -> bool {
    module == "sandbox.rs" || module.starts_with("sandbox/") || module == "process_tree.rs"
}

/// Whether a code line starts a tracked child or an engine CLI.
fn spawns_in_line(line: &str) -> bool {
    let line = line.trim_start();
    if line.starts_with("//") {
        return false;
    }
    line.contains("spawn_in_own_group(")
        || line.contains("TreeKiller::new(")
        || ["claude", "codex", "agy"]
            .iter()
            .any(|cli| line.contains(&format!("Command::new(\"{cli}\"")))
}

/// Whether `source` (production code only) holds a spawn site.
fn spawns_in(source: &str) -> bool {
    source.lines().any(spawns_in_line)
}

/// Every module that spawns, as its `module_path`, outside the door.
fn spawners() -> Vec<String> {
    let mut found = Vec::new();
    for path in source_scan::rust_files() {
        let module = source_scan::module_path(&path);
        if is_door_or_primitive(&module) || source_scan::is_test_only_module(&path) {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("a source file should be readable");
        if spawns_in(&source_scan::production_code(&source)) {
            found.push(module);
        }
    }
    found.sort();
    found
}

#[test]
fn the_detector_sees_a_spawn_and_ignores_a_comment() {
    assert!(spawns_in("spawn_in_own_group(&mut cmd);"));
    assert!(spawns_in("    let k = TreeKiller::new(pid);"));
    assert!(spawns_in("let c = Command::new(\"codex\");"));
    assert!(!spawns_in("// spawn_in_own_group(&mut cmd) is the door's job"));
    assert!(!spawns_in("let c = Command::new(\"git\");"));
    assert!(!spawns_in("fn spawn_in_own_group_tests() {}"));
}

#[test]
fn no_agent_or_gate_spawn_exists_outside_the_sandbox_door() {
    let allowed: Vec<&str> = PENDING_MIGRATION
        .iter()
        .chain(NOT_AGENT_OR_GATE)
        .map(|(file, _)| *file)
        .collect();
    let offenders: Vec<String> = spawners()
        .into_iter()
        .filter(|module| !allowed.contains(&module.as_str()))
        .collect();
    assert!(
        offenders.is_empty(),
        "these modules start a tracked child outside sandbox.rs: {offenders:?}\n\
         Go through `sandbox::spawn` (spec 2026-09-14, D3). A git/gh transport or a kill-only \
         site belongs in NOT_AGENT_OR_GATE in sandbox/guard.rs, with the reason."
    );
}

/// D7: the switch is read from the daemon's config directory and no route can change it, so no
/// module outside the sandbox may name the file or the functions that read it.
#[test]
fn nothing_outside_the_sandbox_names_the_switch() {
    let mut offenders = Vec::new();
    for path in source_scan::rust_files() {
        let module = source_scan::module_path(&path);
        if is_door_or_primitive(&module) {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("a source file should be readable");
        if ["\"sandbox.conf\"", "switch_dir(", "switch_is_off("]
            .iter()
            .any(|needle| source.contains(needle))
        {
            offenders.push(module);
        }
    }
    assert!(offenders.is_empty(), "these modules touch the sandbox switch: {offenders:?}");
}

#[test]
fn every_allow_list_entry_still_spawns() {
    let current = spawners();
    let stale: Vec<&str> = PENDING_MIGRATION
        .iter()
        .chain(NOT_AGENT_OR_GATE)
        .map(|(file, _)| *file)
        .filter(|file| !current.iter().any(|module| module == file))
        .collect();
    assert!(
        stale.is_empty(),
        "these allow-list entries no longer spawn, so delete them: {stale:?}"
    );
}
