//! §spec motor-de-workflows 3.2 / 3.3 / 3.5 / 4.5 - the pure graph walker.
//!
//! A graph, the state of its nodes and the published facts go in; the next [`Step`] comes out.
//! Nothing here touches a database, a worktree, a subprocess or the clock, so every question the job
//! driver used to answer in one hand-written function is a table test.
//!
//! # The queue rules are not written twice
//!
//! The per-item rules (which item halts the queue, which items may start together, what a failed
//! queue ends as) live in `execution::job` and are reused verbatim: [`queue_halt`], [`batch_over`],
//! [`failed_ending_of`], [`never_lands`]. This module decides only WHERE in the graph the job is.
//!
//! # Return edges
//!
//! An edge that closes a cycle (found by a depth-first walk from the entry, in declaration order) is
//! a RETURN edge. Only a decision may follow one, and it follows it as a new round: the walker never
//! loops on its own, it hands back [`Step::Replan`] and the driver bumps the round.
use std::collections::{BTreeMap, BTreeSet};

use super::condition::{self, Eval};
use super::facts::Facts;
use super::graph::{Edge, Graph, Kind, Node, Verdict, is_gate};
use crate::execution::job::{
    DRY_ROUNDS_TO_STOP, Halt, ItemState, ItemView, Outcome, batch_over, failed_ending_of,
    never_lands, queue_halt,
};

/// Why a node is switched off. Followed identically; kept apart because the UI and the inspector
/// must say which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disabled {
    /// The mode's set; changes with the mode.
    Shadow,
    /// `NodeOverlay.disabled`.
    Project,
}

/// Where a SINGLETON node (agent/command/decision not inside a fan's `each`) is, in the CURRENT
/// round. Absent from the map means `NotEntered`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    NotEntered,
    Running,
    /// The agent landed. A review that FAILED is `Done`: a review that failed is still a review that
    /// happened.
    Done,
    Failed,
    Cancelled,
    /// Never owed this round (loader-derived, e.g. the round queued nothing). Followed like `Done`,
    /// no run.
    Skipped,
    /// Owed again. Same answer as `NotEntered`.
    RetryOwed,
    /// Singleton gate (a command with verdict edges) passed.
    GatePassed,
    /// Singleton gate red.
    GateRed {
        retries_left: bool,
    },
    GateErrored,
    /// Singleton gate red, and the branch reset is still owed.
    RevertOwed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkState {
    pub round: i64,
    pub max_rounds: i64,
    pub dry_rounds: i64,
    /// The return-edge target is in flight right now (the replan node).
    pub replanning: bool,
    pub a_broken_item_may_go_round_again: bool,
    pub has_team: bool,
    /// `teams.max_parallel` (>= 1). Meets `Node.concurrency` in [`ceiling`].
    pub max_parallel: usize,
    pub nodes: BTreeMap<String, NodeState>,
    /// Every row of each fan's FIRST `each` node, all rounds, ordinal order (position == ordinal),
    /// keyed by FAN id.
    pub fans: BTreeMap<String, Vec<ItemView>>,
    pub disabled: BTreeMap<String, Disabled>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphStuck {
    pub node: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Start a singleton node (`item: None`) or the GATE-side node of one fan item
    /// (`item: Some(ordinal)`, `node_id` = the segment's gate node).
    RunNode {
        node_id: String,
        item: Option<usize>,
    },
    /// Start the segment's first node for these items (ordinals, at most the ceiling). `node_id` is
    /// the FAN id. Retry and conflict resolution are the driver's reading of the item state.
    FanOut { node_id: String, items: Vec<usize> },
    /// Engine op at the fan boundary (not a node): bring the item's branch into the job's. Team jobs
    /// only.
    MergeItem { node_id: String, item: usize },
    /// Finish a reset the branch is owed. `item: None` is a singleton gate.
    SettleRevert {
        node_id: String,
        item: Option<usize>,
    },
    /// Fan-boundary op: put a Pending item down because a dependency never lands.
    Orphan { node_id: String, item: usize },
    /// A return edge was taken: re-enter `node_id` as a new round.
    Replan { node_id: String },
    /// A node is in flight, or a branch is waiting on one.
    Wait,
    /// A gate would not run: silence, never a verdict (3.2).
    GateErrored {
        node_id: String,
        item: Option<usize>,
    },
    /// 4.5: a decision's key nobody published (or a cycle with no decision in it).
    Stuck(GraphStuck),
    /// The job is over. `Finish(Outcome::Completed)` is the contract's `Complete`; the other
    /// outcomes ride the same variant because the old `Next::Finish(Outcome)` carried them.
    Finish(Outcome),
}

/// What one visited node says about where to go next.
enum Flow<'a> {
    To(&'a str),
    End,
    Out(Step),
}

fn stuck(node: &str, key: &str) -> Step {
    Step::Stuck(GraphStuck {
        node: node.to_string(),
        key: key.to_string(),
    })
}

fn in_each(graph: &Graph, id: &str) -> bool {
    graph
        .nodes
        .iter()
        .filter(|node| node.kind == Kind::Fan)
        .any(|fan| fan.each.iter().any(|member| member == id))
}

/// The entry is the first declared node that no fan repeats.
fn entry(graph: &Graph) -> Option<&Node> {
    graph.nodes.iter().find(|node| !in_each(graph, &node.id))
}

/// An edge that belongs to a fan's segment (`gate -> work`, or between two members): never followed
/// as a graph edge.
fn is_segment_edge(graph: &Graph, edge: &Edge) -> bool {
    graph
        .nodes
        .iter()
        .filter(|node| node.kind == Kind::Fan)
        .any(|fan| {
            fan.each.contains(&edge.from) && (edge.to == fan.id || fan.each.contains(&edge.to))
        })
}

/// Indices of the edges that close a cycle, by DFS from `from` over non-segment edges.
fn return_edges(graph: &Graph, from: &str) -> BTreeSet<usize> {
    fn visit<'a>(
        graph: &'a Graph,
        at: &'a str,
        stack: &mut Vec<&'a str>,
        seen: &mut BTreeSet<&'a str>,
        out: &mut BTreeSet<usize>,
    ) {
        stack.push(at);
        seen.insert(at);
        for (index, edge) in graph.edges.iter().enumerate() {
            if edge.from != at || is_segment_edge(graph, edge) {
                continue;
            }
            if stack.contains(&edge.to.as_str()) {
                out.insert(index);
            } else if !seen.contains(edge.to.as_str()) {
                visit(graph, &edge.to, stack, seen, out);
            }
        }
        stack.pop();
    }
    let mut out = BTreeSet::new();
    visit(graph, from, &mut Vec::new(), &mut BTreeSet::new(), &mut out);
    out
}

/// The gate node of a fan's segment: the first member after the work node that is a gate by role,
/// else the first command member. `None` when there is none or it is switched off.
fn segment_gate<'a>(graph: &'a Graph, state: &WalkState, fan: &'a Node) -> Option<&'a str> {
    let rest = fan.each.get(1..).unwrap_or(&[]);
    let by_role = rest.iter().find(|id| is_gate(&graph.edges, id));
    let by_kind = || {
        rest.iter().find(|id| {
            graph
                .nodes
                .iter()
                .any(|node| node.id == **id && node.kind == Kind::Command)
        })
    };
    let gate = by_role.or_else(by_kind)?;
    (!state.disabled.contains_key(gate.as_str())).then_some(gate.as_str())
}

/// How many items of a fan may be in flight at once: a team job runs `min(max_parallel, declared)`,
/// anything else runs one at a time whatever the graph declares.
fn ceiling(fan: &Node, state: &WalkState) -> usize {
    if !state.has_team {
        return 1;
    }
    let declared = fan.concurrency.map_or(usize::MAX, |n| n as usize);
    state.max_parallel.min(declared).max(1)
}

struct Walk<'a> {
    graph: &'a Graph,
    state: &'a WalkState,
    facts: &'a Facts,
    returns: BTreeSet<usize>,
}

impl<'a> Walk<'a> {
    fn node(&self, id: &str) -> Option<&'a Node> {
        self.graph.nodes.iter().find(|node| node.id == id)
    }

    fn fans(&self) -> impl Iterator<Item = &'a Node> + '_ {
        self.graph
            .nodes
            .iter()
            .filter(|node| node.kind == Kind::Fan)
    }

    fn items_of(&self, fan: &Node) -> &'a [ItemView] {
        self.state
            .fans
            .get(&fan.id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn all_items(&self) -> Vec<ItemView> {
        self.fans()
            .flat_map(|fan| self.items_of(fan).iter().cloned())
            .collect()
    }

    fn ending(&self) -> Outcome {
        failed_ending_of(&self.all_items()).unwrap_or(Outcome::Completed)
    }

    fn node_state(&self, id: &str) -> NodeState {
        self.state
            .nodes
            .get(id)
            .copied()
            .unwrap_or(NodeState::NotEntered)
    }

    /// Every non-segment edge leaving `id`, return edges included, with its index.
    fn edges_from(&self, id: &'a str) -> impl Iterator<Item = (usize, &'a Edge)> + '_ {
        self.graph
            .edges
            .iter()
            .enumerate()
            .filter(move |(_, edge)| edge.from == id && !is_segment_edge(self.graph, edge))
    }

    /// Singleton nodes: not a fan, not repeated by one.
    fn singletons(&self) -> impl Iterator<Item = &'a Node> + '_ {
        self.graph.nodes.iter().filter(|node| {
            matches!(node.kind, Kind::Agent | Kind::Command)
                && !in_each(self.graph, &node.id)
                && !self.state.disabled.contains_key(&node.id)
        })
    }

    /// Phase A: whatever stops the queue or owes the branch a reset outranks everything else, even
    /// nodes not yet entered.
    fn halt(&self) -> Option<Step> {
        for fan in self.fans() {
            if let Some(item) = self
                .items_of(fan)
                .iter()
                .position(|item| item.state == ItemState::Reverted)
            {
                return Some(Step::SettleRevert {
                    node_id: fan.id.clone(),
                    item: Some(item),
                });
            }
        }
        for node in self.singletons() {
            if self.node_state(&node.id) == NodeState::RevertOwed {
                return Some(Step::SettleRevert {
                    node_id: node.id.clone(),
                    item: None,
                });
            }
        }
        for fan in self.fans() {
            let halted = queue_halt(
                self.items_of(fan),
                self.state.has_team,
                self.state.a_broken_item_may_go_round_again,
            );
            match halted {
                Some((_, Halt::Finish(outcome))) => return Some(Step::Finish(outcome)),
                Some((item, Halt::GateErrored)) => {
                    return Some(Step::GateErrored {
                        node_id: segment_gate(self.graph, self.state, fan)
                            .unwrap_or(&fan.id)
                            .to_string(),
                        item: Some(item),
                    });
                }
                None => {}
            }
        }
        for node in self.singletons() {
            match self.node_state(&node.id) {
                NodeState::Cancelled => return Some(Step::Finish(Outcome::Cancelled)),
                NodeState::Failed => return Some(Step::Finish(Outcome::Failed)),
                NodeState::GateErrored => {
                    return Some(Step::GateErrored {
                        node_id: node.id.clone(),
                        item: None,
                    });
                }
                _ => {}
            }
        }
        None
    }

    fn walk(&self, entry: &'a Node) -> Step {
        let mut visited: BTreeSet<&str> = BTreeSet::new();
        let mut cursor = entry;
        loop {
            if !visited.insert(cursor.id.as_str()) {
                return stuck(&cursor.id, "cycle");
            }
            match self.visit(cursor) {
                Flow::To(next) => match self.node(next) {
                    Some(node) => cursor = node,
                    None => return stuck(next, "missing"),
                },
                Flow::End => return Step::Finish(self.ending()),
                Flow::Out(step) => return step,
            }
        }
    }

    fn visit(&self, node: &'a Node) -> Flow<'a> {
        let state = self.node_state(&node.id);
        if self.state.disabled.contains_key(&node.id) || state == NodeState::Skipped {
            return if node.kind == Kind::Decision {
                self.first_plain_edge(node)
            } else {
                self.advance(node, Verdict::Pass)
            };
        }
        match node.kind {
            Kind::Agent | Kind::Command => self.singleton(node, state),
            Kind::Decision => self.decision(node),
            Kind::Fan => self.fan(node),
        }
    }

    /// The first out-edge that is not a return edge (a disabled decision, or rounds switched off).
    fn first_plain_edge(&self, node: &'a Node) -> Flow<'a> {
        match self
            .edges_from(&node.id)
            .find(|(index, _)| !self.returns.contains(index))
        {
            Some((_, edge)) => Flow::To(&edge.to),
            None => Flow::End,
        }
    }

    /// Leave a node that passed: the edge matching the verdict for a gate, the first unconditioned
    /// edge for anything else (`when` on those is a drawing label, 4.4).
    fn advance(&self, node: &'a Node, verdict: Verdict) -> Flow<'a> {
        let gate = is_gate(&self.graph.edges, &node.id);
        match self
            .edges_from(&node.id)
            .map(|(_, edge)| edge)
            .find(|edge| {
                if gate {
                    edge.verdict == Some(verdict)
                } else {
                    edge.verdict.is_none()
                }
            }) {
            Some(edge) => Flow::To(&edge.to),
            None => Flow::End,
        }
    }

    fn singleton(&self, node: &'a Node, state: NodeState) -> Flow<'a> {
        let run = |id: &str| {
            Flow::Out(Step::RunNode {
                node_id: id.to_string(),
                item: None,
            })
        };
        match state {
            NodeState::NotEntered | NodeState::RetryOwed => run(&node.id),
            NodeState::Running => Flow::Out(Step::Wait),
            NodeState::Done | NodeState::GatePassed | NodeState::Skipped => {
                self.advance(node, Verdict::Pass)
            }
            NodeState::GateRed { retries_left: true } => match self.producer_of(&node.id) {
                Some(producer) => run(producer),
                None => Flow::Out(stuck(&node.id, "producer")),
            },
            NodeState::GateRed {
                retries_left: false,
            } => {
                let producer = self.producer_of(&node.id);
                let fail = self
                    .edges_from(&node.id)
                    .map(|(_, edge)| edge)
                    .find(|edge| edge.verdict == Some(Verdict::Fail));
                match fail {
                    Some(edge) if Some(edge.to.as_str()) != producer => Flow::To(&edge.to),
                    _ => Flow::Out(Step::Finish(Outcome::GateFailed)),
                }
            }
            NodeState::Failed => Flow::Out(Step::Finish(Outcome::Failed)),
            NodeState::Cancelled => Flow::Out(Step::Finish(Outcome::Cancelled)),
            NodeState::GateErrored => Flow::Out(Step::GateErrored {
                node_id: node.id.clone(),
                item: None,
            }),
            NodeState::RevertOwed => Flow::Out(Step::SettleRevert {
                node_id: node.id.clone(),
                item: None,
            }),
        }
    }

    /// Where a gate's input comes from: the `from` of the first edge into it.
    fn producer_of(&self, gate: &str) -> Option<&'a str> {
        self.graph
            .edges
            .iter()
            .find(|edge| edge.to == gate && !is_segment_edge(self.graph, edge))
            .map(|edge| edge.from.as_str())
    }

    /// The decision's condition as a truth value, or the step that says why it has none.
    fn truth(&self, node: &Node) -> Result<bool, Step> {
        let source = node.condition.as_deref().unwrap_or("");
        let parsed = condition::parse(source).map_err(|_| stuck(&node.id, source))?;
        match condition::evaluate(&parsed, self.facts) {
            Eval::True => Ok(true),
            Eval::False => Ok(false),
            Eval::Unpublished { key } => Err(stuck(&node.id, &key)),
        }
    }

    /// The out-edge (return edges included) a value is written on.
    fn catching(&self, node: &'a Node, value: bool) -> Option<&'a Edge> {
        let wanted = if value { "true" } else { "false" };
        self.edges_from(&node.id)
            .map(|(_, edge)| edge)
            .find(|edge| edge.when.as_deref().map(str::trim) == Some(wanted))
    }

    fn decision(&self, node: &'a Node) -> Flow<'a> {
        let state = self.state;
        let Some(ret) = self
            .edges_from(&node.id)
            .find(|(index, _)| self.returns.contains(index))
            .map(|(_, edge)| edge)
        else {
            return match self.truth(node) {
                Err(step) => Flow::Out(step),
                Ok(value) => match self.catching(node, value) {
                    Some(edge) => Flow::To(&edge.to),
                    // 4.4 wants both values covered; the loop's exit needs one uncovered. The graph
                    // ends where nothing catches the value.
                    None => Flow::End,
                },
            };
        };

        // The round decision, in the order `close_the_round` always asked its questions.
        if state.max_rounds <= 1 {
            return self.first_plain_edge(node);
        }
        if let Some(outcome) = failed_ending_of(&self.all_items()) {
            let go_round_again = matches!(outcome, Outcome::GateFailed | Outcome::Failed)
                && state.a_broken_item_may_go_round_again
                && state.round + 1 < state.max_rounds
                && state.dry_rounds < DRY_ROUNDS_TO_STOP;
            if !go_round_again {
                return Flow::Out(Step::Finish(outcome));
            }
            return Flow::Out(self.replan_or_wait(ret));
        }
        if state.replanning {
            return Flow::Out(Step::Wait);
        }
        match self.truth(node) {
            Err(step) => Flow::Out(step),
            Ok(value) => match self.catching(node, value) {
                Some(edge) if std::ptr::eq(edge, ret) => {
                    Flow::Out(if state.dry_rounds >= DRY_ROUNDS_TO_STOP {
                        Step::Finish(Outcome::Completed)
                    } else if state.round + 1 >= state.max_rounds {
                        Step::Finish(Outcome::Stopped)
                    } else {
                        Step::Replan {
                            node_id: ret.to.clone(),
                        }
                    })
                }
                Some(edge) => Flow::To(&edge.to),
                None => Flow::End,
            },
        }
    }

    fn replan_or_wait(&self, ret: &Edge) -> Step {
        if self.state.replanning {
            Step::Wait
        } else {
            Step::Replan {
                node_id: ret.to.clone(),
            }
        }
    }

    fn fan(&self, fan: &'a Node) -> Flow<'a> {
        let items = self.items_of(fan);
        let state = self.state;
        let gate = segment_gate(self.graph, state, fan);
        let gate_step = |item: usize| Step::RunNode {
            node_id: gate.unwrap_or(&fan.id).to_string(),
            item: Some(item),
        };

        // An empty queue is a finished job, in any round (kept from the old decision, D3).
        if items.is_empty() {
            return Flow::Out(Step::Finish(Outcome::Completed));
        }
        if !state.has_team && items.iter().any(|item| item.state == ItemState::Running) {
            return Flow::Out(Step::Wait);
        }
        if let Some(item) = items
            .iter()
            .position(|item| item.state == ItemState::Merging)
        {
            return Flow::Out(gate_step(item));
        }
        if let Some(item) = items
            .iter()
            .position(|item| item.state == ItemState::Implemented)
        {
            if state.has_team {
                return Flow::Out(Step::MergeItem {
                    node_id: fan.id.clone(),
                    item,
                });
            }
            // A segment with no gate has nothing to measure `Implemented` with, so it counts as
            // `Passed` and the queue carries on.
            if gate.is_some() {
                return Flow::Out(gate_step(item));
            }
        }
        if let Some(item) = items.iter().position(|item| {
            item.state == ItemState::Pending
                && item.depends_on.iter().any(|dependency| {
                    items
                        .get(*dependency as usize)
                        .is_some_and(|dependency| never_lands(dependency.state))
                })
        }) {
            return Flow::Out(Step::Orphan {
                node_id: fan.id.clone(),
                item,
            });
        }
        let batch = batch_over(items, ceiling(fan, state));
        if !batch.is_empty() {
            return Flow::Out(Step::FanOut {
                node_id: fan.id.clone(),
                items: batch,
            });
        }
        if items.iter().any(|item| item.state == ItemState::Running) {
            return Flow::Out(Step::Wait);
        }
        if items.iter().any(|item| item.state.claimable_as().is_some()) {
            return Flow::Out(Step::Finish(Outcome::Stopped));
        }
        // `join: all` is the only rule: every item has settled.
        self.advance(fan, Verdict::Pass)
    }
}

/// The next move for a job, given its graph, where each node is and what has been published.
pub fn next_step(graph: &Graph, state: &WalkState, facts: &Facts) -> Step {
    let Some(entry) = entry(graph) else {
        return Step::Finish(Outcome::Completed);
    };
    let walk = Walk {
        graph,
        state,
        facts,
        returns: return_edges(graph, &entry.id),
    };
    if let Some(step) = walk.halt() {
        return step;
    }
    walk.walk(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::NodeOverlay;
    use crate::workflows::graph;

    /// Fixtures skip `graph::validate` on purpose: several of them are shapes the walker must cope
    /// with whether or not the installer would let them in (a decision value nothing catches).
    fn raw(yaml: &str) -> Graph {
        serde_yaml::from_str(yaml).expect("the fixture is valid yaml")
    }

    fn autopilot() -> Graph {
        raw(include_str!("../../assets/autopilot/1.0.0/graph.yaml"))
    }

    fn line() -> Graph {
        raw(r#"
nodes:
  - {id: a, type: agent}
  - {id: b, type: agent}
  - {id: c, type: agent}
edges:
  - {from: a, to: b}
  - {from: b, to: c}
"#)
    }

    fn gated() -> Graph {
        raw(r#"
nodes:
  - {id: build, type: agent}
  - {id: check, type: command, command: "true"}
  - {id: ship, type: agent}
  - {id: repair, type: agent}
edges:
  - {from: build, to: check}
  - {from: check, to: ship, verdict: pass}
  - {from: check, to: repair, verdict: fail}
"#)
    }

    fn gated_without_a_fail_edge() -> Graph {
        raw(r#"
nodes:
  - {id: build, type: agent}
  - {id: check, type: command, command: "true"}
  - {id: ship, type: agent}
edges:
  - {from: build, to: check}
  - {from: check, to: ship, verdict: pass}
"#)
    }

    /// The autopilot minus its `spec` node, so a test starts at `plan`.
    fn round_loop() -> Graph {
        raw(r#"
nodes:
  - {id: plan, type: agent}
  - {id: implement, type: agent}
  - {id: gate, type: command, command: "project:gate"}
  - {id: work, type: fan, source: plan, each: [implement, gate], concurrency: 8, join: all}
  - {id: review, type: agent}
  - {id: more, type: decision, condition: "plan.done != true"}
edges:
  - {from: plan, to: work}
  - {from: gate, to: work, verdict: pass}
  - {from: gate, to: work, verdict: fail}
  - {from: work, to: review}
  - {from: review, to: more}
  - {from: more, to: plan, when: "true"}
"#)
    }

    fn harness() -> Graph {
        raw(r#"
nodes:
  - {id: triage, type: decision, condition: "triage.risk == high"}
  - {id: deep, type: agent}
  - {id: quick, type: agent}
edges:
  - {from: triage, to: deep, when: "true"}
  - {from: triage, to: quick, when: "false"}
"#)
    }

    fn state() -> WalkState {
        WalkState {
            round: 0,
            max_rounds: 1,
            dry_rounds: 0,
            replanning: false,
            a_broken_item_may_go_round_again: false,
            has_team: false,
            max_parallel: 1,
            nodes: BTreeMap::new(),
            fans: BTreeMap::new(),
            disabled: BTreeMap::new(),
        }
    }

    fn facts(pairs: &[(&str, &str)]) -> Facts {
        let mut facts = Facts::new();
        for (key, value) in pairs {
            facts.insert(*key, *value);
        }
        facts
    }

    fn item(state: ItemState) -> ItemView {
        ItemView {
            state,
            depends_on: Vec::new(),
            files: Vec::new(),
        }
    }

    fn item_with(state: ItemState, depends_on: &[i64], files: &[&str]) -> ItemView {
        ItemView {
            state,
            depends_on: depends_on.to_vec(),
            files: files.iter().map(|f| f.to_string()).collect(),
        }
    }

    /// Nodes in the given states.
    fn with(mut s: WalkState, nodes: &[(&str, NodeState)]) -> WalkState {
        for (id, node_state) in nodes {
            s.nodes.insert(id.to_string(), *node_state);
        }
        s
    }

    /// The `work` fan holding these items.
    fn queue(mut s: WalkState, items: Vec<ItemView>) -> WalkState {
        s.fans.insert("work".to_string(), items);
        s
    }

    fn queue_of(mut s: WalkState, states: &[ItemState]) -> WalkState {
        s.fans.insert(
            "work".to_string(),
            states.iter().copied().map(item).collect(),
        );
        s
    }

    /// `round_loop` with `plan` landed: the walker is at the fan.
    fn planned() -> WalkState {
        with(state(), &[("plan", NodeState::Done)])
    }

    /// `round_loop` with the queue settled and the review done: the walker is at the decision.
    fn closing(states: &[ItemState]) -> WalkState {
        queue_of(
            with(
                state(),
                &[("plan", NodeState::Done), ("review", NodeState::Done)],
            ),
            states,
        )
    }

    fn run(id: &str) -> Step {
        Step::RunNode {
            node_id: id.to_string(),
            item: None,
        }
    }

    fn run_item(id: &str, item: usize) -> Step {
        Step::RunNode {
            node_id: id.to_string(),
            item: Some(item),
        }
    }

    fn replan(id: &str) -> Step {
        Step::Replan {
            node_id: id.to_string(),
        }
    }

    #[test]
    fn a_line_runs_the_first_unentered_node() {
        let g = line();
        let none = facts(&[]);
        assert_eq!(next_step(&g, &state(), &none), run("a"));
        let s = with(state(), &[("a", NodeState::Done)]);
        assert_eq!(next_step(&g, &s, &none), run("b"));
        let s = with(
            state(),
            &[
                ("a", NodeState::Done),
                ("b", NodeState::Done),
                ("c", NodeState::Done),
            ],
        );
        assert_eq!(next_step(&g, &s, &none), Step::Finish(Outcome::Completed));
    }

    #[test]
    fn a_node_in_flight_waits() {
        let s = with(
            state(),
            &[("a", NodeState::Done), ("b", NodeState::Running)],
        );
        assert_eq!(next_step(&line(), &s, &facts(&[])), Step::Wait);
    }

    #[test]
    fn entry_is_the_first_declared_node_outside_any_fan() {
        let none = facts(&[]);
        assert_eq!(next_step(&autopilot(), &state(), &none), run("spec"));
        assert_eq!(next_step(&round_loop(), &state(), &none), run("plan"));
        assert_eq!(entry(&autopilot()).unwrap().id, "spec");
    }

    #[test]
    fn a_disabled_node_is_followed_as_if_it_passed() {
        let none = facts(&[]);
        for kind in [Disabled::Shadow, Disabled::Project] {
            // An agent.
            let mut off = state();
            off.disabled.insert("a".to_string(), kind);
            let done = with(state(), &[("a", NodeState::Done)]);
            assert_eq!(
                next_step(&line(), &off, &none),
                next_step(&line(), &done, &none),
                "{kind:?} agent"
            );

            // A gate with a pass edge.
            let mut off = with(state(), &[("build", NodeState::Done)]);
            off.disabled.insert("check".to_string(), kind);
            let passed = with(
                state(),
                &[("build", NodeState::Done), ("check", NodeState::GatePassed)],
            );
            assert_eq!(
                next_step(&gated(), &off, &none),
                next_step(&gated(), &passed, &none),
                "{kind:?} gate"
            );
            assert_eq!(next_step(&gated(), &off, &none), run("ship"));

            // A fan.
            let mut off = planned();
            off.disabled.insert("work".to_string(), kind);
            let settled = queue_of(planned(), &[ItemState::Passed]);
            assert_eq!(
                next_step(&round_loop(), &off, &none),
                next_step(&round_loop(), &settled, &none),
                "{kind:?} fan"
            );
            assert_eq!(next_step(&round_loop(), &off, &none), run("review"));
        }
    }

    #[test]
    fn shadow_and_project_disabled_walk_identically_but_stay_distinguishable() {
        let none = facts(&[]);
        let mut shadow = state();
        shadow.disabled.insert("b".to_string(), Disabled::Shadow);
        let mut project = state();
        project.disabled.insert("b".to_string(), Disabled::Project);
        let both = [("a", NodeState::Done)];
        let (shadow, project) = (with(shadow, &both), with(project, &both));
        assert_eq!(
            next_step(&line(), &shadow, &none),
            next_step(&line(), &project, &none)
        );
        assert_eq!(shadow.disabled["b"], Disabled::Shadow);
        assert_eq!(project.disabled["b"], Disabled::Project);
        assert_ne!(shadow, project);

        // Only the project's switch is in the overlay, so only it is painted off.
        let mut overlay = BTreeMap::new();
        overlay.insert(
            "b".to_string(),
            NodeOverlay {
                disabled: Some(true),
                ..NodeOverlay::default()
            },
        );
        let resolved = graph::resolve(&line(), &overlay);
        let painted = |id: &str| resolved.nodes.iter().find(|n| n.id == id).unwrap().disabled;
        assert!(painted("b"));
        assert!(
            !painted("a"),
            "a shadow-disabled node is not in the overlay"
        );
    }

    #[test]
    fn a_skipped_node_is_followed_without_a_run() {
        let none = facts(&[]);
        let s = with(state(), &[("a", NodeState::Skipped)]);
        assert_eq!(next_step(&line(), &s, &none), run("b"));

        // A skipped review goes straight to the round decision (one round: nothing to read).
        let s = queue_of(
            with(
                state(),
                &[("plan", NodeState::Done), ("review", NodeState::Skipped)],
            ),
            &[ItemState::Passed],
        );
        assert_eq!(
            next_step(&round_loop(), &s, &none),
            Step::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn a_decision_follows_the_edge_its_value_catches() {
        let s = state();
        assert_eq!(
            next_step(&harness(), &s, &facts(&[("triage.risk", "high")])),
            run("deep")
        );
        assert_eq!(
            next_step(&harness(), &s, &facts(&[("triage.risk", "low")])),
            run("quick")
        );
    }

    #[test]
    fn an_unpublished_key_is_graph_stuck_naming_node_and_key() {
        assert_eq!(
            next_step(&harness(), &state(), &facts(&[])),
            Step::Stuck(GraphStuck {
                node: "triage".to_string(),
                key: "triage.risk".to_string(),
            })
        );
    }

    #[test]
    fn a_decision_value_no_edge_catches_ends_the_graph() {
        let one_armed = raw(r#"
nodes:
  - {id: triage, type: decision, condition: "triage.risk == high"}
  - {id: deep, type: agent}
edges:
  - {from: triage, to: deep, when: "true"}
"#);
        assert_eq!(
            next_step(&one_armed, &state(), &facts(&[("triage.risk", "low")])),
            Step::Finish(Outcome::Completed)
        );
        assert_eq!(
            next_step(&one_armed, &state(), &facts(&[("triage.risk", "high")])),
            run("deep")
        );
    }

    #[test]
    fn a_one_round_job_never_reads_the_round_condition() {
        let s = closing(&[ItemState::Passed]);
        assert_eq!(
            next_step(&round_loop(), &s, &facts(&[])),
            Step::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn the_return_edge_replans_while_rounds_remain() {
        let mut s = closing(&[ItemState::Passed]);
        s.max_rounds = 3;
        let more = facts(&[("plan.done", "false")]);
        assert_eq!(next_step(&round_loop(), &s, &more), replan("plan"));
        s.replanning = true;
        assert_eq!(next_step(&round_loop(), &s, &more), Step::Wait);
    }

    #[test]
    fn the_return_edge_stops_at_the_ceiling_and_at_two_dry_rounds() {
        let more = facts(&[("plan.done", "false")]);
        let mut s = closing(&[ItemState::Passed]);
        s.max_rounds = 3;
        s.round = 2;
        assert_eq!(
            next_step(&round_loop(), &s, &more),
            Step::Finish(Outcome::Stopped)
        );
        s.dry_rounds = DRY_ROUNDS_TO_STOP;
        assert_eq!(
            next_step(&round_loop(), &s, &more),
            Step::Finish(Outcome::Completed),
            "the dry check beats the ceiling"
        );
        s.round = 0;
        assert_eq!(
            next_step(&round_loop(), &s, &more),
            Step::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn a_done_replan_completes_even_with_rounds_left() {
        let mut s = closing(&[ItemState::Passed]);
        s.max_rounds = 3;
        assert_eq!(
            next_step(&round_loop(), &s, &facts(&[("plan.done", "true")])),
            Step::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn a_broken_item_goes_round_again_only_when_licensed() {
        let none = facts(&[("plan.done", "false")]);
        for broken in [ItemState::GateFailed, ItemState::Failed] {
            let mut s = closing(&[broken, ItemState::Passed]);
            s.max_rounds = 3;
            let ending = if broken == ItemState::Failed {
                Outcome::Failed
            } else {
                Outcome::GateFailed
            };
            assert_eq!(
                next_step(&round_loop(), &s, &none),
                Step::Finish(ending),
                "{broken:?} not licensed"
            );
            s.a_broken_item_may_go_round_again = true;
            assert_eq!(
                next_step(&round_loop(), &s, &none),
                replan("plan"),
                "{broken:?} licensed"
            );
            s.replanning = true;
            assert_eq!(next_step(&round_loop(), &s, &none), Step::Wait);
            s.replanning = false;
            s.round = 2;
            assert_eq!(
                next_step(&round_loop(), &s, &none),
                Step::Finish(ending),
                "{broken:?} licensed but out of rounds"
            );
        }

        // A gate that would not run is never taken round again.
        let mut s = closing(&[ItemState::GateErrored]);
        s.max_rounds = 3;
        s.a_broken_item_may_go_round_again = true;
        s.has_team = true;
        s.max_parallel = 2;
        assert_eq!(
            next_step(&round_loop(), &s, &none),
            Step::Finish(Outcome::GateErrored)
        );
    }

    #[test]
    fn a_fan_starts_its_batch_up_to_the_ceiling() {
        let mut s = queue(
            planned(),
            vec![
                item_with(ItemState::Pending, &[], &["a.rs"]),
                item_with(ItemState::Pending, &[], &["a.rs"]),
                item_with(ItemState::Pending, &[], &["b.rs"]),
            ],
        );
        let none = facts(&[]);
        // No team: one at a time whatever the graph declares.
        assert_eq!(
            next_step(&round_loop(), &s, &none),
            Step::FanOut {
                node_id: "work".to_string(),
                items: vec![0],
            }
        );
        s.has_team = true;
        s.max_parallel = 4;
        assert_eq!(
            next_step(&round_loop(), &s, &none),
            Step::FanOut {
                node_id: "work".to_string(),
                items: vec![0, 2],
            }
        );
        // The team's allowance is cut by what the graph declares.
        let mut tight = round_loop();
        tight
            .nodes
            .iter_mut()
            .find(|n| n.id == "work")
            .unwrap()
            .concurrency = Some(1);
        assert_eq!(
            next_step(&tight, &s, &none),
            Step::FanOut {
                node_id: "work".to_string(),
                items: vec![0],
            }
        );
    }

    #[test]
    fn an_item_in_flight_stops_a_sequential_fan_but_not_a_team_fan() {
        let none = facts(&[]);
        let mut s = queue_of(planned(), &[ItemState::Running, ItemState::Pending]);
        assert_eq!(next_step(&round_loop(), &s, &none), Step::Wait);
        s.has_team = true;
        s.max_parallel = 4;
        assert_eq!(
            next_step(&round_loop(), &s, &none),
            Step::FanOut {
                node_id: "work".to_string(),
                items: vec![1],
            }
        );
        // Everything in flight and nothing to start: wait.
        let s = queue_of(planned(), &[ItemState::Running]);
        let mut team = s.clone();
        team.has_team = true;
        team.max_parallel = 4;
        assert_eq!(next_step(&round_loop(), &team, &none), Step::Wait);
    }

    #[test]
    fn a_merging_or_implemented_item_outranks_starting_new_work() {
        let none = facts(&[]);
        let mut s = queue_of(planned(), &[ItemState::Pending, ItemState::Implemented]);
        assert_eq!(
            next_step(&round_loop(), &s, &none),
            run_item("gate", 1),
            "a sequential job gates it"
        );
        s.has_team = true;
        s.max_parallel = 4;
        assert_eq!(
            next_step(&round_loop(), &s, &none),
            Step::MergeItem {
                node_id: "work".to_string(),
                item: 1,
            }
        );
        let s = queue_of(
            {
                let mut t = planned();
                t.has_team = true;
                t.max_parallel = 4;
                t
            },
            &[ItemState::Pending, ItemState::Merging],
        );
        assert_eq!(next_step(&round_loop(), &s, &none), run_item("gate", 1));
    }

    #[test]
    fn an_orphan_is_set_aside_before_selection() {
        let s = queue(
            planned(),
            vec![
                item(ItemState::GateFailed),
                item_with(ItemState::Pending, &[0], &[]),
            ],
        );
        assert_eq!(
            next_step(&round_loop(), &s, &facts(&[])),
            Step::Orphan {
                node_id: "work".to_string(),
                item: 1,
            }
        );
    }

    #[test]
    fn a_reverted_item_is_settled_before_anything() {
        // Even with `spec` not yet entered.
        let s = queue_of(state(), &[ItemState::Passed, ItemState::Reverted]);
        assert_eq!(
            next_step(&autopilot(), &s, &facts(&[])),
            Step::SettleRevert {
                node_id: "work".to_string(),
                item: Some(1),
            }
        );
    }

    #[test]
    fn an_empty_fan_finishes_the_job() {
        assert_eq!(
            next_step(&round_loop(), &planned(), &facts(&[])),
            Step::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn a_fan_with_no_gate_member_treats_implemented_as_passed() {
        let bare = raw(r#"
nodes:
  - {id: plan, type: agent}
  - {id: implement, type: agent}
  - {id: work, type: fan, source: plan, each: [implement]}
edges:
  - {from: plan, to: work}
"#);
        let s = queue_of(planned(), &[ItemState::Implemented]);
        assert_eq!(
            next_step(&bare, &s, &facts(&[])),
            Step::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn a_disabled_gate_member_means_no_gate() {
        let none = facts(&[]);
        let settled = [("plan", NodeState::Done), ("review", NodeState::Done)];
        let on = queue_of(with(state(), &settled), &[ItemState::Implemented]);
        assert_eq!(next_step(&round_loop(), &on, &none), run_item("gate", 0));
        let mut off = on.clone();
        off.disabled.insert("gate".to_string(), Disabled::Project);
        assert_eq!(
            next_step(&round_loop(), &off, &none),
            Step::Finish(Outcome::Completed)
        );
    }

    #[test]
    fn a_gate_that_would_not_run_halts() {
        let none = facts(&[]);
        let mut s = queue_of(planned(), &[ItemState::GateErrored]);
        assert_eq!(
            next_step(&round_loop(), &s, &none),
            Step::GateErrored {
                node_id: "gate".to_string(),
                item: Some(0),
            }
        );
        // A team carries on with the rest, and the END says what happened.
        s.has_team = true;
        s.max_parallel = 2;
        s.nodes.insert("review".to_string(), NodeState::Done);
        assert_eq!(
            next_step(&round_loop(), &s, &none),
            Step::Finish(Outcome::GateErrored)
        );
        // A singleton gate.
        let s = with(
            state(),
            &[
                ("build", NodeState::Done),
                ("check", NodeState::GateErrored),
            ],
        );
        assert_eq!(
            next_step(&gated(), &s, &none),
            Step::GateErrored {
                node_id: "check".to_string(),
                item: None,
            }
        );
    }

    #[test]
    fn a_green_gate_follows_pass() {
        let s = with(
            state(),
            &[("build", NodeState::Done), ("check", NodeState::GatePassed)],
        );
        assert_eq!(next_step(&gated(), &s, &facts(&[])), run("ship"));
        let unrun = with(state(), &[("build", NodeState::Done)]);
        assert_eq!(next_step(&gated(), &unrun, &facts(&[])), run("check"));
    }

    #[test]
    fn a_red_gate_with_retries_reruns_its_producer() {
        let s = with(
            state(),
            &[
                ("build", NodeState::Done),
                ("check", NodeState::GateRed { retries_left: true }),
            ],
        );
        assert_eq!(next_step(&gated(), &s, &facts(&[])), run("build"));
    }

    #[test]
    fn a_red_gate_out_of_retries_follows_fail_or_finishes_gate_failed() {
        let s = with(
            state(),
            &[
                ("build", NodeState::Done),
                (
                    "check",
                    NodeState::GateRed {
                        retries_left: false,
                    },
                ),
            ],
        );
        assert_eq!(next_step(&gated(), &s, &facts(&[])), run("repair"));
        assert_eq!(
            next_step(&gated_without_a_fail_edge(), &s, &facts(&[])),
            Step::Finish(Outcome::GateFailed)
        );
    }

    #[test]
    fn a_red_gate_with_a_revert_owed_settles_first() {
        let s = with(
            state(),
            &[("build", NodeState::Done), ("check", NodeState::RevertOwed)],
        );
        assert_eq!(
            next_step(&gated(), &s, &facts(&[])),
            Step::SettleRevert {
                node_id: "check".to_string(),
                item: None,
            }
        );
    }

    #[test]
    fn a_cycle_with_no_decision_is_stuck_not_a_hang() {
        let cycle = raw(r#"
nodes:
  - {id: a, type: agent}
  - {id: b, type: agent}
edges:
  - {from: a, to: b}
  - {from: b, to: a}
"#);
        let s = with(state(), &[("a", NodeState::Done), ("b", NodeState::Done)]);
        assert_eq!(
            next_step(&cycle, &s, &facts(&[])),
            Step::Stuck(GraphStuck {
                node: "a".to_string(),
                key: "cycle".to_string(),
            })
        );
    }

    #[test]
    fn the_walker_reads_no_facts_it_is_not_asked_about() {
        let s = with(state(), &[("a", NodeState::Done)]);
        assert_eq!(next_step(&line(), &s, &Facts::new()), run("b"));
    }

    #[test]
    fn the_autopilot_asset_parses_and_validates_for_the_walker() {
        let g = autopilot();
        graph::validate(&g).expect("the seeded autopilot installs");
        assert_eq!(entry(&g).unwrap().id, "spec");

        let returns = return_edges(&g, "spec");
        assert_eq!(returns.len(), 1);
        let edge = &g.edges[*returns.iter().next().unwrap()];
        assert_eq!((edge.from.as_str(), edge.to.as_str()), ("more", "plan"));

        let fan = g.nodes.iter().find(|n| n.id == "work").unwrap();
        assert_eq!(fan.each[0], "implement");
        assert_eq!(segment_gate(&g, &state(), fan), Some("gate"));
    }
}
