//! The graph a job runs, overlay painted in (M4 C5, spec motor-de-workflows 5).
//!
//! `graph::resolve` paints the overlay for the canvas (a picture, with provenance per field).
//! This is the same painting as a VALUE the engine runs and snapshots: overrides are written into
//! the nodes themselves, so a job that froze this value keeps its project's choices even if the
//! pins file or the bundle changes mid-run.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::NodeOverlay;
use super::graph::{Graph, Node};

/// The origin's graph with the project's overlay painted in, as a value that survives
/// serialisation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EffectiveGraph {
    pub graph: Graph,
    /// Nodes this project switched off. They stay in `graph` (spec 6.2: painted, not hidden).
    #[serde(default)]
    pub disabled: BTreeSet<String>,
    /// Overlay rows naming no node of the graph: reported, never dropped silently.
    #[serde(default)]
    pub orphaned_overlay: Vec<String>,
}

/// tool/model/command overrides are written INTO the nodes; `disabled` is collected; overlay rows
/// naming no node are reported.
pub fn effective(graph: &Graph, overlay: &BTreeMap<String, NodeOverlay>) -> EffectiveGraph {
    let mut painted = graph.clone();
    let mut disabled = BTreeSet::new();
    for node in &mut painted.nodes {
        let Some(over) = overlay.get(&node.id) else {
            continue;
        };
        if let Some(tool) = &over.tool {
            node.tool = Some(tool.clone());
        }
        if let Some(model) = &over.model {
            node.model = Some(model.clone());
        }
        if let Some(command) = &over.command {
            node.command = Some(command.clone());
        }
        if over.disabled == Some(true) {
            disabled.insert(node.id.clone());
        }
    }
    let known: BTreeSet<&str> = graph.nodes.iter().map(|node| node.id.as_str()).collect();
    let orphaned_overlay = overlay
        .keys()
        .filter(|id| !known.contains(id.as_str()))
        .cloned()
        .collect();
    EffectiveGraph {
        graph: painted,
        disabled,
        orphaned_overlay,
    }
}

impl EffectiveGraph {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.graph.nodes.iter().find(|node| node.id == id)
    }

    pub fn is_disabled(&self, id: &str) -> bool {
        self.disabled.contains(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::graph::{Edge, Kind};

    fn node(id: &str, kind: Kind) -> Node {
        Node {
            id: id.to_string(),
            kind,
            label: None,
            body: None,
            agent: None,
            tool: Some("claude".to_string()),
            model: Some("opus".to_string()),
            effort: None,
            contract: None,
            command: None,
            cwd: None,
            timeout: None,
            condition: None,
            source: None,
            each: Vec::new(),
            concurrency: None,
            join: None,
        }
    }

    fn graph() -> Graph {
        let mut gate = node("gate", Kind::Command);
        gate.tool = None;
        gate.model = None;
        gate.command = Some("project:gate".to_string());
        Graph {
            nodes: vec![node("plan", Kind::Agent), gate],
            edges: vec![Edge {
                from: "plan".to_string(),
                to: "gate".to_string(),
                when: None,
                verdict: None,
            }],
        }
    }

    fn overlay(rows: &[(&str, NodeOverlay)]) -> BTreeMap<String, NodeOverlay> {
        rows.iter()
            .map(|(id, row)| ((*id).to_string(), row.clone()))
            .collect()
    }

    #[test]
    fn overlay_values_replace_node_values() {
        let eff = effective(
            &graph(),
            &overlay(&[
                (
                    "plan",
                    NodeOverlay {
                        model: Some("haiku".into()),
                        tool: Some("codex".into()),
                        ..NodeOverlay::default()
                    },
                ),
                (
                    "gate",
                    NodeOverlay {
                        command: Some("make check".into()),
                        ..NodeOverlay::default()
                    },
                ),
            ]),
        );
        let plan = eff.node("plan").unwrap();
        assert_eq!(plan.model.as_deref(), Some("haiku"));
        assert_eq!(plan.tool.as_deref(), Some("codex"));
        assert_eq!(
            eff.node("gate").unwrap().command.as_deref(),
            Some("make check")
        );
        // An absent field is inherited, not cleared.
        assert_eq!(eff.node("gate").unwrap().tool, None);
    }

    #[test]
    fn disabled_nodes_stay_in_the_graph() {
        let eff = effective(
            &graph(),
            &overlay(&[
                (
                    "gate",
                    NodeOverlay {
                        disabled: Some(true),
                        ..NodeOverlay::default()
                    },
                ),
                (
                    "plan",
                    NodeOverlay {
                        disabled: Some(false),
                        ..NodeOverlay::default()
                    },
                ),
            ]),
        );
        assert!(eff.is_disabled("gate"));
        assert!(!eff.is_disabled("plan"));
        assert!(eff.node("gate").is_some());
        assert_eq!(eff.graph.nodes.len(), 2);
    }

    #[test]
    fn an_overlay_row_for_a_missing_node_is_reported() {
        let eff = effective(
            &graph(),
            &overlay(&[(
                "council",
                NodeOverlay {
                    disabled: Some(true),
                    ..NodeOverlay::default()
                },
            )]),
        );
        assert_eq!(eff.orphaned_overlay, vec!["council".to_string()]);
        assert!(!eff.is_disabled("council"));
        assert_eq!(eff.graph, graph());
    }

    #[test]
    fn serde_round_trip_is_identity() {
        let eff = effective(
            &graph(),
            &overlay(&[
                (
                    "plan",
                    NodeOverlay {
                        model: Some("haiku".into()),
                        disabled: Some(true),
                        ..NodeOverlay::default()
                    },
                ),
                ("ghost", NodeOverlay::default()),
            ]),
        );
        let json = serde_json::to_string(&eff).unwrap();
        let back: EffectiveGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(back, eff);
    }

    #[test]
    fn effective_of_empty_overlay_equals_origin() {
        let eff = effective(&graph(), &BTreeMap::new());
        assert_eq!(eff.graph, graph());
        assert!(eff.disabled.is_empty());
        assert!(eff.orphaned_overlay.is_empty());
    }

    #[test]
    fn effective_paints_the_same_values_the_canvas_shows() {
        let rows = overlay(&[(
            "plan",
            NodeOverlay {
                model: Some("haiku".into()),
                disabled: Some(true),
                ..NodeOverlay::default()
            },
        )]);
        let eff = effective(&graph(), &rows);
        let resolved = crate::workflows::graph::resolve(&graph(), &rows);
        let painted = resolved.nodes.iter().find(|n| n.id == "plan").unwrap();
        let model = painted.fields.iter().find(|f| f.name == "model").unwrap();
        assert_eq!(
            Some(model.value.as_str()),
            eff.node("plan").unwrap().model.as_deref()
        );
        assert_eq!(painted.disabled, eff.is_disabled("plan"));
        assert_eq!(resolved.orphaned, eff.orphaned_overlay);
    }
}
