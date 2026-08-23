//! What a workflow's graph is, and what this project changed about it.
//!
//! **A module of its own, and that is the point.** [`crate::workflows`] states, at length, that it
//! never parses anything but a bundle's manifest — that is what let the library, the pin and the
//! whole of drift land without the graph's format being settled. This is where the format IS
//! settled, and keeping it here keeps that property true: install, eject and drift still do not
//! know what a node is, and never will.
//!
//! The plan recorded the decision as open, to be made by the second spec unless something needed it
//! sooner: *"if slice 7 needs to fix it first, the boundary between the specs is in the wrong
//! place"*. Slice 7 did not need it. A canvas is nothing without it, so it is made here — and made
//! as narrowly as a canvas requires. **What is decided:** the file, the four node types, the
//! fields each carries, and that an edge may carry a condition or a verdict. **What is not:** how a
//! node is dispatched, what a condition is evaluated against, what happens when a bundle changes
//! mid-run. §14 keeps all four.
//!
//! # Four types, decided by who executes the node
//!
//! §6.4, and the argument for it is what the reader gets for free. The two questions anybody asks
//! of a workflow they are looking at are *where does this spend money* and *where can this break
//! something*, and in this vocabulary both are answered without reading a word: an **agent** node
//! spends tokens, a **command** node does not, and a command with a verdict edge is where the
//! pipeline stops. A vocabulary organised by packet contract would buy composition checking and
//! pay for it with a graph where everything looks the same.
//!
//! **A gate is not a fifth type.** It is the ROLE a command node plays when something conditions on
//! how it exited — see [`Role`]. Making it a type would mean a bundle could declare a gate that
//! nothing branches on, which is a gate in name only, and would let the same command be two
//! different types depending on how it was written down.
//!
//! # The overlay is painted, not applied
//!
//! §6.2. [`resolve`] does not produce a graph with the project's values substituted in; it produces
//! the origin's graph with each change **stamped** — the effective value beside what the origin
//! said. A node this project switched off keeps its place and its edges, and is reported as
//! `disabled` rather than removed, because hiding it would make the picture lie about what the
//! workflow is.
//!
//! And a row in the overlay naming a node the bundle does not have is neither of those: §12 asks
//! for `switched off here` ≠ `not in the bundle`, so those come back as [`Resolved::orphaned`]
//! instead of being dropped. Silently dropping them is how somebody's override stops applying and
//! nothing ever says so.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// The file inside a bundle that holds the graph.
pub const GRAPH_FILE: &str = "graph.yaml";

/// Who executes a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A model is asked to do something. This is where a workflow spends money.
    Agent,
    /// A program is run. Cheap, and the only kind that can be a gate.
    Command,
    /// A branch on something already known about the run. Executes nothing.
    Decision,
    /// One node becoming many. Carries how many at once, and how they are joined again.
    Fan,
}

/// Which way an edge leaves a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
}

/// One node, with every field any of the four types can carry.
///
/// One struct rather than an enum with four variants, and it is a deliberate trade. YAML with an
/// internally tagged enum gives error messages about "unknown variant" that name neither the node
/// nor the field, which is the wrong answer for a file a person writes by hand. The cost is that
/// `command` on an agent node parses; [`validate`] refuses it by name instead, which is a better
/// message than serde was going to give.
///
/// `deny_unknown_fields`, for the reason `config.rs` gives about `schedules:`: a mistyped key that
/// is silently dropped is a node that quietly loses its model, its timeout or its gate, and the
/// file still looks right. The cost is that a bundle written for a later version of this format
/// fails to load here rather than degrading — which is the safer of the two failures, because the
/// other one runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    /// `type` in the file. Renamed because `type` is a keyword here and nowhere else.
    #[serde(rename = "type")]
    pub kind: Kind,
    /// What a person calls it. Absent means the id is the label, which is right for `plan`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /* agent */
    /// The instructions file, relative to the bundle. The node's actual content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// What passes down the edge out of here. Reserved by §6.4 and drawn by the inspector; nothing
    /// validates against it yet, which is the "not lost" half of that section rather than a gap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<String>,

    /* command */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Free text — `30m`, `2h`. Not parsed here: a duration is the engine's to interpret, and
    /// parsing it in the module that draws pictures would be the format deciding something §14
    /// keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,

    /* decision */
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,

    /* fan */
    /// What the fan-out is over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join: Option<String>,
}

/// One edge. A condition, a verdict, or neither — never both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// The condition under which this edge is taken, as free text for the label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    /// Which way out of a gate. Present here is what MAKES the source node a gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Graph {
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

/// Parse and validate a bundle's graph, in the parser's own words when it will not.
pub fn parse(contents: &str) -> Result<Graph, String> {
    let graph: Graph = match serde_yaml::from_str::<Option<Graph>>(contents) {
        Ok(Some(graph)) => graph,
        Ok(None) => Graph::default(),
        Err(error) => return Err(error.to_string()),
    };
    validate(&graph)?;
    Ok(graph)
}

/// The four rules a graph has to satisfy to be drawable at all.
///
/// Each of them is something that would otherwise produce a picture that is wrong rather than a
/// picture that is missing — an edge to nowhere drawn as a stub, two nodes sharing an id drawn as
/// one, a verdict leaving something that has no exit code to have a verdict about.
pub fn validate(graph: &Graph) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for node in &graph.nodes {
        if node.id.trim().is_empty() {
            return Err("a node has no id".to_string());
        }
        if !ids.insert(node.id.as_str()) {
            return Err(format!("two nodes share the id `{}`", node.id));
        }
        // A command with nothing to run and an agent with no body are both nodes that cannot do
        // the thing their type says they do. Caught here rather than at execution, because the
        // canvas is where somebody is looking at the workflow and able to fix it.
        if node.kind == Kind::Command && node.command.is_none() {
            return Err(format!("`{}` is a command node with no command", node.id));
        }
        if node.kind == Kind::Decision && node.condition.is_none() {
            return Err(format!(
                "`{}` is a decision node with no condition",
                node.id
            ));
        }
    }

    for edge in &graph.edges {
        for end in [&edge.from, &edge.to] {
            if !ids.contains(end.as_str()) {
                return Err(format!("an edge names `{end}`, which is not a node here"));
            }
        }
        if edge.verdict.is_some() {
            let from = graph
                .nodes
                .iter()
                .find(|node| node.id == edge.from)
                .expect("checked above");
            // A verdict is a claim about an exit code, and only a command has one. An agent node
            // with a `pass` edge would be drawn amber — the mark this design reserves for "here is
            // where the pipeline stops" — over something that cannot stop it.
            if from.kind != Kind::Command {
                return Err(format!(
                    "`{}` carries a verdict edge, and only a command node has an exit code to judge",
                    edge.from
                ));
            }
        }
    }
    Ok(())
}

/// What a node is *for*, as distinct from who runs it.
///
/// Two, and there will not be more. `Gate` is a command something branches on, which is a fact
/// about the edges rather than about the node — see the module header for why it is not a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Plain,
    Gate,
}

/// One field of a node as the inspector shows it.
///
/// `origin` is `Some` **only** when this project overrode the field, and that is §6.2's whole
/// requirement in one nullable: absent means inherited, present means changed and here is what it
/// was. A shape that always carried both would make the page compare them to find out which — and
/// two values that happen to be equal would then read as an override.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Field {
    pub name: &'static str,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// One node of the origin's graph, with this project's changes stamped on it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Painted {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    pub role: Role,
    pub label: String,
    /// Switched off in this project. **Still in the graph** — §6.2 — and drawn dotted.
    pub disabled: bool,
    /// Whether this project changed anything at all here, which is the seal §6.2 makes mandatory.
    pub overridden: bool,
    /// Every field this node carries, effective value first.
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Resolved {
    pub nodes: Vec<Painted>,
    pub edges: Vec<Edge>,
    /// Overlay rows naming a node this bundle does not have.
    ///
    /// Not an error and not silence. A bundle that dropped a node leaves somebody's override
    /// applying to nothing, and the only moment they can learn that is here.
    pub orphaned: Vec<String>,
}

/// Paint the project's overlay onto the origin's graph.
///
/// Pure — it takes the graph and the overlay and returns the picture — because this is the function
/// §13 names as one of the three worth testing without a render, and because the alternative is a
/// component that reads a file.
pub fn resolve(
    graph: &Graph,
    overlay: &BTreeMap<String, crate::workflows::NodeOverlay>,
) -> Resolved {
    let mut nodes = Vec::with_capacity(graph.nodes.len());

    for node in &graph.nodes {
        let over = overlay.get(&node.id);
        let mut fields = Vec::new();

        // The three the overlay can reach, each asked the same way: what does this project say,
        // and what did the origin say. `push_field` is where "absent means inherited" lives.
        push_field(
            &mut fields,
            "tool",
            node.tool.as_deref(),
            over.and_then(|o| o.tool.as_deref()),
        );
        push_field(
            &mut fields,
            "model",
            node.model.as_deref(),
            over.and_then(|o| o.model.as_deref()),
        );
        push_field(
            &mut fields,
            "command",
            node.command.as_deref(),
            over.and_then(|o| o.command.as_deref()),
        );

        // And the ones only the origin sets. They are shown because the inspector's job is to say
        // what a node IS, not only what was changed about it.
        for (name, value) in [
            ("body", node.body.as_deref()),
            ("effort", node.effort.as_deref()),
            ("contract", node.contract.as_deref()),
            ("cwd", node.cwd.as_deref()),
            ("timeout", node.timeout.as_deref()),
            ("condition", node.condition.as_deref()),
            ("source", node.source.as_deref()),
            ("join", node.join.as_deref()),
        ] {
            push_field(&mut fields, name, value, None);
        }
        if let Some(concurrency) = node.concurrency {
            fields.push(Field {
                name: "concurrency",
                value: concurrency.to_string(),
                origin: None,
            });
        }

        let disabled = over.and_then(|o| o.disabled).unwrap_or(false);
        nodes.push(Painted {
            role: if is_gate(&graph.edges, &node.id) {
                Role::Gate
            } else {
                Role::Plain
            },
            label: node.label.clone().unwrap_or_else(|| node.id.clone()),
            disabled,
            // Switching a node off is an override like any other. A page that stamped the seal only
            // for value changes would draw the most drastic change a project can make as inherited.
            overridden: disabled || fields.iter().any(|field| field.origin.is_some()),
            fields,
            id: node.id.clone(),
            kind: node.kind,
        });
    }

    let known: BTreeSet<&str> = graph.nodes.iter().map(|node| node.id.as_str()).collect();
    let orphaned = overlay
        .keys()
        .filter(|id| !known.contains(id.as_str()))
        .cloned()
        .collect();

    Resolved {
        nodes,
        edges: graph.edges.clone(),
        orphaned,
    }
}

fn push_field(
    fields: &mut Vec<Field>,
    name: &'static str,
    origin: Option<&str>,
    project: Option<&str>,
) {
    match (project, origin) {
        (Some(project), origin) => fields.push(Field {
            name,
            value: project.to_string(),
            // `Some("")` rather than nothing when the origin did not set the field at all: the
            // project ADDED it, which is an override, and an absent `origin` here would report it
            // as inherited.
            origin: Some(origin.unwrap_or("").to_string()),
        }),
        (None, Some(origin)) => fields.push(Field {
            name,
            value: origin.to_string(),
            origin: None,
        }),
        (None, None) => {}
    }
}

/// Whether anything branches on how this node exited.
pub fn is_gate(edges: &[Edge], node_id: &str) -> bool {
    edges
        .iter()
        .any(|edge| edge.from == node_id && edge.verdict.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::NodeOverlay;

    const HARNESS: &str = r#"
nodes:
  - id: triage
    type: decision
    label: Triage
    condition: size
  - id: plan
    type: agent
    label: Plan
    body: skills/plan/SKILL.md
    tool: claude
    model: opus
    contract: packets/plan.md
  - id: gate
    type: command
    command: python .ai/scripts/select_tests.py --gate --run
    timeout: 30m
  - id: rescue
    type: agent
    body: skills/rescue/SKILL.md
edges:
  - {from: triage, to: plan, when: "size != trivial"}
  - {from: plan, to: gate}
  - {from: gate, to: rescue, verdict: fail}
"#;

    #[test]
    fn the_harness_this_repository_already_has_parses_as_a_graph() {
        let graph = parse(HARNESS).unwrap();
        assert_eq!(graph.nodes.len(), 4);
        assert_eq!(graph.edges.len(), 3);
        assert_eq!(graph.nodes[1].kind, Kind::Agent);
        assert_eq!(graph.nodes[1].model.as_deref(), Some("opus"));
    }

    /// A gate is a role and not a type: the same command node is or is not one depending entirely
    /// on whether anything branches on how it exited.
    #[test]
    fn a_gate_is_a_command_something_branches_on_and_nothing_else() {
        let graph = parse(HARNESS).unwrap();
        assert!(is_gate(&graph.edges, "gate"));
        assert!(!is_gate(&graph.edges, "plan"));

        // Drop the verdict edge and the same node stops being a gate, without the node changing.
        let mut plain = graph.clone();
        plain.edges.retain(|edge| edge.verdict.is_none());
        assert!(!is_gate(&plain.edges, "gate"));
    }

    /// Only a command has an exit code, so only a command can carry a verdict. Amber is reserved
    /// for "here is where the pipeline stops", and an agent cannot stop it this way.
    #[test]
    fn a_verdict_leaving_something_with_no_exit_code_is_refused() {
        let refusal = parse(
            "nodes:\n  - {id: plan, type: agent}\n  - {id: next, type: agent}\nedges:\n  - {from: plan, to: next, verdict: pass}\n",
        )
        .unwrap_err();
        assert!(refusal.contains("exit code"), "{refusal}");
    }

    #[test]
    fn an_edge_to_a_node_that_is_not_here_is_refused_rather_than_drawn_as_a_stub() {
        let refusal =
            parse("nodes:\n  - {id: plan, type: agent}\nedges:\n  - {from: plan, to: nowhere}\n")
                .unwrap_err();
        assert!(refusal.contains("nowhere"), "{refusal}");
    }

    #[test]
    fn two_nodes_with_one_id_are_refused_rather_than_drawn_as_one() {
        let refusal = parse("nodes:\n  - {id: plan, type: agent}\n  - {id: plan, type: agent}\n")
            .unwrap_err();
        assert!(refusal.contains("share the id"), "{refusal}");
    }

    /// The typo `config.rs` was bitten by, in this format. A dropped key is a node that quietly
    /// loses its model or its gate while the file still looks right.
    #[test]
    fn a_mistyped_key_is_an_error_rather_than_a_silently_emptier_node() {
        assert!(parse("nodes:\n  - {id: plan, type: agent, modle: opus}\n").is_err());
    }

    #[test]
    fn a_command_with_nothing_to_run_is_not_a_command() {
        assert!(parse("nodes:\n  - {id: fmt, type: command}\n").is_err());
        assert!(parse("nodes:\n  - {id: risk, type: decision}\n").is_err());
    }

    /// §6.2: a node this project switched off stays in the graph, dotted, with its edges. Hiding it
    /// would make the picture lie about what the workflow is.
    #[test]
    fn a_node_switched_off_here_keeps_its_place_and_its_edges() {
        let graph = parse(HARNESS).unwrap();
        let mut overlay = BTreeMap::new();
        overlay.insert(
            "rescue".to_string(),
            NodeOverlay {
                disabled: Some(true),
                ..NodeOverlay::default()
            },
        );

        let painted = resolve(&graph, &overlay);
        assert_eq!(painted.nodes.len(), 4);
        assert_eq!(painted.edges.len(), 3);
        let rescue = painted.nodes.iter().find(|n| n.id == "rescue").unwrap();
        assert!(rescue.disabled);
        // Switching a node off is the most drastic change a project can make, so it carries the
        // seal too — otherwise the one change worth seeing is the one drawn as inherited.
        assert!(rescue.overridden);
    }

    /// §6.2's other half: the inspector shows what the origin said, beside what this project says.
    #[test]
    fn an_overridden_field_carries_what_the_origin_said_beside_it() {
        let graph = parse(HARNESS).unwrap();
        let mut overlay = BTreeMap::new();
        overlay.insert(
            "plan".to_string(),
            NodeOverlay {
                model: Some("haiku".into()),
                ..NodeOverlay::default()
            },
        );

        let painted = resolve(&graph, &overlay);
        let plan = painted.nodes.iter().find(|n| n.id == "plan").unwrap();
        assert!(plan.overridden);

        let model = plan.fields.iter().find(|f| f.name == "model").unwrap();
        assert_eq!(model.value, "haiku");
        assert_eq!(model.origin.as_deref(), Some("opus"));

        // And a field nobody touched says nothing about an origin, which is what "absent means
        // inherited" has to mean if the seal is to be worth anything.
        let tool = plan.fields.iter().find(|f| f.name == "tool").unwrap();
        assert_eq!(tool.value, "claude");
        assert!(tool.origin.is_none());
    }

    /// A field the origin never set and the project added is an override, not an inheritance.
    #[test]
    fn a_field_this_project_added_is_an_override_and_not_an_inheritance() {
        let graph = parse(HARNESS).unwrap();
        let mut overlay = BTreeMap::new();
        overlay.insert(
            "rescue".to_string(),
            NodeOverlay {
                model: Some("opus".into()),
                ..NodeOverlay::default()
            },
        );
        let painted = resolve(&graph, &overlay);
        let rescue = painted.nodes.iter().find(|n| n.id == "rescue").unwrap();
        let model = rescue.fields.iter().find(|f| f.name == "model").unwrap();
        assert_eq!(model.origin.as_deref(), Some(""));
        assert!(rescue.overridden);
    }

    /// §12: `switched off here` ≠ `not in the bundle`. An override applying to nothing is reported
    /// rather than dropped — it is the only moment somebody can learn it stopped applying.
    #[test]
    fn an_override_for_a_node_the_bundle_dropped_is_reported_not_discarded() {
        let graph = parse(HARNESS).unwrap();
        let mut overlay = BTreeMap::new();
        overlay.insert("council".to_string(), NodeOverlay::default());
        let painted = resolve(&graph, &overlay);
        assert_eq!(painted.orphaned, vec!["council".to_string()]);
        assert_eq!(painted.nodes.len(), 4);
    }

    /// A bundle with no graph in it is not a broken bundle. Skills and scripts with no sequence yet
    /// is an ordinary halfway state, and an empty file has to mean that rather than an error.
    #[test]
    fn an_empty_graph_file_is_a_bundle_with_no_sequence_yet() {
        assert_eq!(parse("").unwrap(), Graph::default());
        assert_eq!(parse("# nothing yet\n").unwrap(), Graph::default());
    }
}
