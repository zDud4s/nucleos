//! The autopilot's own bundle, written to the library so it can be read back like any other.
//!
//! # Why the built-in workflow lives on disk and not in the code
//!
//! It could have stayed a graph compiled into the binary, and that would have been safer in exactly
//! one way and worse in every other. Two sources of graph is the two-engines smell one level down:
//! the canvas would either not show the workflow that runs most often in this app, or show it
//! through a second path that could disagree with the first. Seeding it means **one code path** —
//! [`crate::workflows`] lists it, [`crate::workflow_graph`] parses it, the canvas draws it, and a
//! person can open it, read it, fork it or eject it like anything else in the library.
//!
//! # Why nothing is lost when it is overwritten
//!
//! The obvious objection is that seeding destroys somebody's edits. It cannot, and the reason is
//! the rule that makes the library work at all: **a version is immutable and is defined by its
//! hash**. `autopilot/1.0.0` either hashes to what this binary carries or it is not that version
//! any more. Re-seeding a directory whose hash has moved is not undoing an edit; it is restoring
//! the thing the name refers to.
//!
//! Somebody who wants their own autopilot publishes `1.0.1` or ejects it into their project — the
//! two exits §6.3 already lays side by side so that ejecting is a deliberate choice rather than the
//! default one.
//!
//! # What it does NOT do
//!
//! Change what a job does. `job.rs` is still the authority on what runs next and still builds its
//! own prompts; this bundle is the same sequence written down where it can be looked at. The engine
//! learns to read it later, and until then the file and the code say the same thing because they
//! were written from each other.

use std::collections::BTreeMap;
use std::path::Path;

use crate::workflows;

pub const NAME: &str = "autopilot";
pub const VERSION: &str = "1.0.0";

/// The bundle, by relative path. Embedded rather than read, so a daemon with no source tree beside
/// it still has one — and so the hash below is a property of the binary rather than of a checkout.
const FILES: &[(&str, &str)] = &[
    (
        "bundle.yaml",
        include_str!("../assets/autopilot/1.0.0/bundle.yaml"),
    ),
    (
        "graph.yaml",
        include_str!("../assets/autopilot/1.0.0/graph.yaml"),
    ),
];

/// What the seeding did, so the caller can say it out loud exactly when it is worth saying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seeded {
    /// Already there, hash for hash. The ordinary case, and silent.
    Unchanged,
    /// Not there at all — a first start, or a library somebody moved.
    Written,
    /// There, and different. Worth a line: it is the only case where a person's disk changed
    /// under a name that is supposed to mean one thing.
    Restored,
}

/// The hash the embedded bundle would have on disk.
///
/// Built through [`workflows::hash_of`] and folded by [`workflows::digest_of`] — the same two
/// functions a real directory goes through — because a second implementation of "what is this
/// bundle's hash" is a second answer to whether it has drifted.
pub fn digest() -> String {
    let files: BTreeMap<String, String> = FILES
        .iter()
        .map(|(path, contents)| ((*path).to_string(), workflows::hash_of(contents.as_bytes())))
        .collect();
    workflows::digest_of(&files)
}

/// Put the autopilot in the library if it is not there, or not itself.
pub fn seed(library_root: &Path) -> std::io::Result<Seeded> {
    let dir = library_root.join(NAME).join(VERSION);

    // `file_hashes` on a directory that is not there is an error, not an empty map, so absence is
    // asked about first and answered as its own case. The two are worth telling apart: one is a
    // first start and one is a bundle that stopped being itself.
    let existing = if dir.is_dir() {
        Some(workflows::digest_of(&workflows::file_hashes(&dir)?))
    } else {
        None
    };
    if existing.as_deref() == Some(digest().as_str()) {
        return Ok(Seeded::Unchanged);
    }

    // **Emptied and rewritten, not written over.** The hash is over the whole directory, so a file
    // somebody added beside the bundle is drift exactly as much as a file they edited — and writing
    // only the files this binary knows about would leave that one there, leave the hash wrong, and
    // report `Restored` on every start for ever without ever converging.
    //
    // Scoped to this one version directory, which the daemon is the author of, and reached only
    // when its hash has already been read and found not to be this bundle's.
    if existing.is_some() {
        std::fs::remove_dir_all(&dir)?;
    }
    for (path, contents) in FILES {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, contents)?;
    }

    Ok(if existing.is_some() {
        Seeded::Restored
    } else {
        Seeded::Written
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bundle this binary carries has to be a bundle. It is embedded rather than read, so
    /// nothing else in the build would ever notice that it stopped parsing.
    #[test]
    fn the_bundle_this_binary_carries_parses_as_a_graph() {
        let graph = FILES
            .iter()
            .find(|(path, _)| *path == crate::workflow_graph::GRAPH_FILE)
            .map(|(_, contents)| *contents)
            .expect("the seeded bundle has a graph");
        let parsed = crate::workflow_graph::parse(graph).expect("the seeded graph is valid");

        // The sequence `job.rs` runs, by the names `runs.stage` already uses.
        let ids: Vec<&str> = parsed.nodes.iter().map(|node| node.id.as_str()).collect();
        assert_eq!(ids, ["plan", "implement", "gate", "work", "more", "review"]);

        // The replan round is a DECISION and not a condition written on the edge out of the fan.
        // §4.4 gives the condition to the decision node and leaves `when` on any other edge as a
        // label nothing evaluates — so the second spelling would be a branch nothing ever takes.
        let more = parsed.nodes.iter().find(|n| n.id == "more").unwrap();
        assert_eq!(more.kind, crate::workflow_graph::Kind::Decision);
        assert!(more.condition.is_some());

        // And the two things about it that no prose ever managed to state: the gate is inside the
        // fan, per item, and the gate is the project's own command rather than one this file made up.
        let fan = parsed.nodes.iter().find(|n| n.id == "work").unwrap();
        assert_eq!(fan.each, ["implement", "gate"]);
        let gate = parsed.nodes.iter().find(|n| n.id == "gate").unwrap();
        assert_eq!(
            crate::workflow_graph::command_source(gate.command.as_deref().unwrap()),
            crate::workflow_graph::CommandSource::Project("gate")
        );
    }

    /// A gate is what stops a pipeline, so the picture has to draw it as one.
    #[test]
    fn the_gate_of_the_seeded_bundle_is_a_gate_by_role() {
        let graph = crate::workflow_graph::parse(FILES[1].1).unwrap();
        assert!(crate::workflow_graph::is_gate(&graph.edges, "gate"));
        assert!(!crate::workflow_graph::is_gate(&graph.edges, "plan"));
    }

    #[test]
    fn a_library_without_it_gets_it_and_a_library_with_it_is_left_alone() {
        let library = tempfile::tempdir().unwrap();
        assert_eq!(seed(library.path()).unwrap(), Seeded::Written);
        assert_eq!(seed(library.path()).unwrap(), Seeded::Unchanged);

        let listed = workflows::library(library.path()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, NAME);
        assert_eq!(listed[0].version, VERSION);
        assert_eq!(listed[0].hash, digest());
    }

    /// **A version is defined by its hash**, so a `1.0.0` that hashes differently is not that
    /// version any more and restoring it undoes nothing anybody is entitled to keep. Reported apart
    /// from a first write, because this is the only case worth telling a person about.
    #[test]
    fn a_bundle_that_stopped_being_itself_is_restored_and_says_so() {
        let library = tempfile::tempdir().unwrap();
        seed(library.path()).unwrap();

        let graph = library
            .path()
            .join(NAME)
            .join(VERSION)
            .join(crate::workflow_graph::GRAPH_FILE);
        std::fs::write(&graph, "nodes: []\n").unwrap();

        assert_eq!(seed(library.path()).unwrap(), Seeded::Restored);
        assert!(
            std::fs::read_to_string(&graph)
                .unwrap()
                .contains("implement")
        );
        assert_eq!(seed(library.path()).unwrap(), Seeded::Unchanged);
    }

    /// A file somebody added is drift too: the hash is over the whole directory, and a bundle with
    /// an extra file in it is not the bundle this binary carries.
    ///
    /// **And it has to converge.** Writing only the files this binary knows about would leave the
    /// extra one there and report `Restored` on every start for ever — a daemon that says it fixed
    /// something every morning and never did.
    #[test]
    fn a_file_added_beside_it_counts_as_drift_and_the_restore_converges() {
        let library = tempfile::tempdir().unwrap();
        seed(library.path()).unwrap();
        let extra = library.path().join(NAME).join(VERSION).join("notes.md");
        std::fs::write(&extra, "mine\n").unwrap();

        assert_eq!(seed(library.path()).unwrap(), Seeded::Restored);
        assert!(!extra.exists());
        assert_eq!(seed(library.path()).unwrap(), Seeded::Unchanged);
    }
}
