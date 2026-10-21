//! Fusion (contract §A): several sources sight the same node, several edge sources say who its
//! parent is. This turns that into one tree, deterministically and without I/O.

use std::collections::BTreeMap;

use super::model::{Capabilities, FleetNode, NodeId, NodeKey, NodeState, Timestamp};
use super::source::{Edge, Sighting, Usage};

/// The `AgentSource::name()` of the daemon-run source. Its sightings are special in the merge:
/// they add `run_id`, `stop` and `cost`, and give lines only when nobody else can.
pub const DAEMON_RUNS_SOURCE: &str = "daemon_runs";

/// One source's sightings, with the index of that source in the registry (`FusedNode` points back
/// at it for lines and transcript).
pub struct SourceSightings<'a> {
    pub index: usize,
    pub name: &'a str,
    pub sightings: &'a [Sighting],
}

#[derive(Clone, Debug, PartialEq)]
pub struct FusedNode {
    /// `pinned` is false and `done_omitted` is 0 here; the book decides both.
    pub node: FleetNode,
    pub key: NodeKey,
    pub last_activity_at: Timestamp,
    pub usage: Option<Usage>,
    /// Registry index of the source that reads this node's lines.
    pub lines_source: Option<usize>,
    pub transcript_source: Option<usize>,
    /// Registry index of `daemon_runs` when it sighted this node. For a terminal run that did not
    /// succeed, the hub appends one `Error` line carrying the run status to a node whose lines come
    /// from a native adapter (contract amendment 1): hub appends — `Book::error_jobs` (Task 10) and
    /// `FleetHub::read_errors` (Task 14). A `Sighting` carries no run status, so the hub gets it by
    /// reading the daemon source's own last line.
    pub daemon_source: Option<usize>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Fused {
    /// Sorted by node id.
    pub nodes: Vec<FusedNode>,
    /// (child, parent) pairs of edges dropped because they would close a cycle; the hub logs each
    /// pair once.
    pub ignored_cycles: Vec<(NodeId, NodeId)>,
}

type Member<'a> = (usize, bool, &'a Sighting);

struct Merged {
    key: NodeKey,
    harness: String,
    model: Option<String>,
    role: Option<String>,
    label: String,
    project: Option<String>,
    state: NodeState,
    started_at: Timestamp,
    ended_at: Option<Timestamp>,
    last_activity_at: Timestamp,
    run_id: Option<i64>,
    can_stop: bool,
    usage: Option<Usage>,
    lines_source: Option<usize>,
    transcript_source: Option<usize>,
    daemon_source: Option<usize>,
    native_parent: Option<NodeKey>,
}

/// Alive means working or waiting for the owner; `Done` and `Lost` are over.
pub(crate) fn is_live(state: NodeState) -> bool {
    matches!(state, NodeState::Busy | NodeState::Idle)
}

/// Rule 1 and amendment 1: equal keys merge. `run_id`, `stop`, the cost, and (the run is the
/// authoritative liveness) `state` and `ended_at` come from the daemon sighting when there is one;
/// label, role, model, project and `started_at` come from the first non-daemon sighting, with the
/// daemon's own as the fallback when it is the only one.
fn merge(members: &[Member<'_>]) -> Merged {
    let daemon = members.iter().find(|m| m.1).map(|m| m.2);
    let others: Vec<&Member<'_>> = members.iter().filter(|m| !m.1).collect();
    let base = others
        .first()
        .map(|m| m.2)
        .or(daemon)
        .expect("a group is never empty");
    let source_with = |wanted: fn(&Sighting) -> bool| -> Option<usize> {
        others
            .iter()
            .find(|m| wanted(m.2))
            .map(|m| m.0)
            .or_else(|| members.iter().find(|m| m.1 && wanted(m.2)).map(|m| m.0))
    };
    let native_usage = others.iter().find_map(|m| m.2.usage.clone());
    let usage = match (native_usage, daemon.and_then(|d| d.usage.clone())) {
        (Some(mut native), Some(daemon)) => {
            if daemon.cost_usd.is_some() {
                native.cost_usd = daemon.cost_usd;
            }
            if native.context_tokens.is_none() {
                native.context_tokens = daemon.context_tokens;
            }
            Some(native)
        }
        (native, daemon) => native.or(daemon),
    };
    Merged {
        key: base.key.clone(),
        harness: base.harness_label.clone(),
        model: base.model.clone(),
        role: base.role.clone(),
        label: base.label.clone(),
        project: base.project.clone(),
        state: daemon.map_or(base.state, |d| d.state),
        started_at: base.started_at,
        ended_at: daemon.map_or(base.ended_at, |d| d.ended_at),
        last_activity_at: members
            .iter()
            .map(|m| m.2.last_activity_at)
            .max()
            .expect("a group is never empty"),
        run_id: daemon
            .and_then(|d| d.run_id)
            .or_else(|| others.iter().find_map(|m| m.2.run_id)),
        can_stop: daemon.is_some_and(|d| d.can_stop),
        usage,
        lines_source: source_with(|s| s.has_lines),
        transcript_source: source_with(|s| s.has_transcript),
        daemon_source: members.iter().find(|m| m.1).map(|m| m.0),
        native_parent: others.iter().find_map(|m| m.2.parent.clone()),
    }
}

/// Rules 2 and 3 and amendment 2. Candidates are tried level by level: native parents first, then
/// each edge list in registry order, and inside a list in the order the source gave them (an edge
/// source may emit several candidates per child; the first that survives wins, so the list order
/// is the source's precedence and is NOT sorted). A candidate is dropped when the child already
/// has a parent, when either end is unknown, or when it would close a cycle (reported); a dropped
/// candidate does not use up the child, so a later or lower-precedence edge may still apply.
fn resolve_parents(
    merged: &BTreeMap<NodeKey, Merged>,
    edge_lists: &[Vec<Edge>],
) -> (BTreeMap<NodeKey, NodeKey>, Vec<(NodeId, NodeId)>) {
    let mut levels: Vec<Vec<(NodeKey, NodeKey)>> = vec![
        merged
            .values()
            .filter_map(|m| m.native_parent.clone().map(|p| (m.key.clone(), p)))
            .collect(),
    ];
    for list in edge_lists {
        levels.push(
            list.iter()
                .map(|e| (e.child.clone(), e.parent.clone()))
                .collect(),
        );
    }
    let mut parent_of: BTreeMap<NodeKey, NodeKey> = BTreeMap::new();
    let mut ignored: Vec<(NodeId, NodeId)> = Vec::new();
    for level in levels {
        for (child, parent) in level {
            if parent_of.contains_key(&child)
                || !merged.contains_key(&child)
                || !merged.contains_key(&parent)
            {
                continue;
            }
            if closes_cycle(&parent_of, &child, &parent) {
                let pair = (child.id(), parent.id());
                if !ignored.contains(&pair) {
                    ignored.push(pair);
                }
                continue;
            }
            parent_of.insert(child, parent);
        }
    }
    (parent_of, ignored)
}

/// Whether making `parent` the parent of `child` makes `child` its own ancestor.
fn closes_cycle(parent_of: &BTreeMap<NodeKey, NodeKey>, child: &NodeKey, parent: &NodeKey) -> bool {
    let mut at = parent;
    loop {
        if at == child {
            return true;
        }
        match parent_of.get(at) {
            Some(next) => at = next,
            None => return false,
        }
    }
}

fn build(
    m: &Merged,
    merged: &BTreeMap<NodeKey, Merged>,
    parent_of: &BTreeMap<NodeKey, NodeKey>,
) -> FusedNode {
    let mut root = &m.key;
    let mut depth = 0u32;
    while let Some(parent) = parent_of.get(root) {
        root = parent;
        depth += 1;
    }
    let parent = parent_of.get(&m.key);
    // Rule 5: a child that never wrote the harness's finished marker, under a root that is no
    // longer alive, did not finish cleanly.
    let state = if parent.is_some() && is_live(m.state) && !is_live(merged[root].state) {
        NodeState::Lost
    } else {
        m.state
    };
    FusedNode {
        node: FleetNode {
            id: m.key.id(),
            parent_id: parent.map(NodeKey::id),
            root_id: root.id(),
            depth,
            harness: m.harness.clone(),
            model: m.model.clone(),
            role: m.role.clone(),
            label: m.label.clone(),
            state,
            started_at: m.started_at,
            ended_at: m.ended_at,
            pinned: false,
            done_omitted: 0,
            project: m.project.clone(),
            run_id: m.run_id,
            // Rule 4.
            capabilities: Capabilities {
                stop: m.can_stop && is_live(state),
                open_run: m.run_id.is_some(),
                full_transcript: m.transcript_source.is_some(),
                pin: parent.is_none(),
            },
        },
        key: m.key.clone(),
        last_activity_at: m.last_activity_at,
        usage: m.usage.clone(),
        lines_source: m.lines_source,
        transcript_source: m.transcript_source,
        daemon_source: m.daemon_source,
    }
}

pub fn fuse(sources: &[SourceSightings<'_>], edge_lists: &[Vec<Edge>]) -> Fused {
    let mut groups: BTreeMap<NodeKey, Vec<Member<'_>>> = BTreeMap::new();
    for source in sources {
        let daemon = source.name == DAEMON_RUNS_SOURCE;
        for sighting in source.sightings {
            groups
                .entry(sighting.key.clone())
                .or_default()
                .push((source.index, daemon, sighting));
        }
    }
    let merged: BTreeMap<NodeKey, Merged> = groups
        .iter()
        .map(|(key, members)| (key.clone(), merge(members)))
        .collect();
    let (parent_of, ignored_cycles) = resolve_parents(&merged, edge_lists);
    let mut nodes: Vec<FusedNode> = merged
        .values()
        .map(|m| build(m, &merged, &parent_of))
        .collect();
    nodes.sort_by(|a, b| a.node.id.cmp(&b.node.id));
    Fused {
        nodes,
        ignored_cycles,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::testkit::{at, edge, sighting};

    fn run(sources: Vec<(&str, Vec<Sighting>)>, edges: Vec<Vec<Edge>>) -> Fused {
        let refs: Vec<SourceSightings<'_>> = sources
            .iter()
            .enumerate()
            .map(|(index, (name, sightings))| SourceSightings {
                index,
                name,
                sightings,
            })
            .collect();
        fuse(&refs, &edges)
    }

    fn node<'a>(fused: &'a Fused, id: &str) -> &'a FusedNode {
        fused
            .nodes
            .iter()
            .find(|n| n.node.id == id)
            .unwrap_or_else(|| panic!("no node {id} in {:?}", fused.nodes.iter().map(|n| &n.node.id).collect::<Vec<_>>()))
    }

    #[test]
    fn equal_keys_merge_into_one_node_with_run_id_stop_and_cost_from_the_daemon() {
        let mut native = sighting("claude-code", "s1");
        native.model = Some("opus".into());
        native.usage = Some(Usage { context_tokens: Some(900), cost_usd: None });
        let mut daemon = sighting("claude-code", "s1");
        daemon.run_id = Some(7);
        daemon.can_stop = true;
        daemon.model = Some("ignored".into());
        daemon.usage = Some(Usage { context_tokens: Some(1), cost_usd: Some(0.5) });

        let fused = run(
            vec![("claude_code", vec![native]), (DAEMON_RUNS_SOURCE, vec![daemon])],
            vec![],
        );

        assert_eq!(fused.nodes.len(), 1);
        let n = node(&fused, "claude-code:s1");
        assert_eq!(n.node.model.as_deref(), Some("opus"), "fields come from the native sighting");
        assert_eq!(n.node.run_id, Some(7));
        assert!(n.node.capabilities.stop);
        assert!(n.node.capabilities.open_run);
        assert_eq!(n.lines_source, Some(0), "lines come from the harness adapter, not the daemon");
        assert_eq!(
            n.usage,
            Some(Usage { context_tokens: Some(900), cost_usd: Some(0.5) })
        );
    }

    #[test]
    fn the_daemon_reads_the_lines_when_no_adapter_can() {
        let mut native = sighting("claude-code", "s1");
        native.has_lines = false;
        let mut daemon = sighting("claude-code", "s1");
        daemon.has_lines = true;
        let fused = run(
            vec![("claude_code", vec![native]), (DAEMON_RUNS_SOURCE, vec![daemon])],
            vec![],
        );
        assert_eq!(node(&fused, "claude-code:s1").lines_source, Some(1));
    }

    #[test]
    fn a_daemon_only_node_keeps_its_own_fields() {
        let mut daemon = sighting("daemon", "run-9");
        daemon.run_id = Some(9);
        daemon.can_stop = true;
        daemon.label = "fix the parser".into();
        let fused = run(vec![(DAEMON_RUNS_SOURCE, vec![daemon])], vec![]);
        let n = node(&fused, "daemon:run-9");
        assert_eq!(n.node.label, "fix the parser");
        assert_eq!(n.node.run_id, Some(9));
        assert_eq!(n.daemon_source, Some(0));
    }

    #[test]
    fn a_finished_run_ends_a_node_the_native_adapter_still_calls_busy() {
        let mut native = sighting("claude-code", "s1");
        native.label = "native label".into();
        native.role = Some("planner".into());
        native.started_at = at(5);
        let mut daemon = sighting("claude-code", "s1");
        daemon.state = NodeState::Done;
        daemon.ended_at = Some(at(50));
        daemon.label = "daemon label".into();
        daemon.started_at = at(1);
        daemon.run_id = Some(7);
        daemon.can_stop = true;

        let fused = run(
            vec![("claude_code", vec![native]), (DAEMON_RUNS_SOURCE, vec![daemon])],
            vec![],
        );

        let n = node(&fused, "claude-code:s1");
        assert_eq!(n.node.state, NodeState::Done, "the run is the authoritative liveness");
        assert_eq!(n.node.ended_at, Some(at(50)), "ended_at is the run's too");
        assert!(!n.node.capabilities.stop, "a finished run cannot be stopped");
        assert_eq!(n.node.label, "native label");
        assert_eq!(n.node.role.as_deref(), Some("planner"));
        assert_eq!(n.node.started_at, at(5), "started_at stays native");
        assert_eq!(n.daemon_source, Some(1), "the hub finds the run's status through this");
    }

    #[test]
    fn a_running_run_keeps_a_node_alive_that_the_native_adapter_calls_done() {
        let mut native = sighting("claude-code", "s1");
        native.state = NodeState::Done;
        native.ended_at = Some(at(20));
        let mut daemon = sighting("claude-code", "s1");
        daemon.run_id = Some(7);
        daemon.can_stop = true;

        let fused = run(
            vec![("claude_code", vec![native]), (DAEMON_RUNS_SOURCE, vec![daemon])],
            vec![],
        );

        let n = node(&fused, "claude-code:s1");
        assert_eq!(n.node.state, NodeState::Busy);
        assert_eq!(n.node.ended_at, None, "a live node has no end, whatever the adapter saw");
        assert!(n.node.capabilities.stop);
    }

    #[test]
    fn a_child_is_judged_by_the_root_state_the_daemon_decided() {
        // The root's run finished although its transcript still looks busy: the child is lost.
        let root = sighting("claude-code", "r");
        let mut root_run = sighting("claude-code", "r");
        root_run.state = NodeState::Done;
        root_run.ended_at = Some(at(9));
        root_run.run_id = Some(1);
        let mut child = sighting("claude-code", "c");
        child.parent = Some(root.key.clone());
        let fused = run(
            vec![
                ("claude_code", vec![root, child]),
                (DAEMON_RUNS_SOURCE, vec![root_run]),
            ],
            vec![],
        );
        assert_eq!(node(&fused, "claude-code:r").node.state, NodeState::Done);
        assert_eq!(node(&fused, "claude-code:c").node.state, NodeState::Lost);

        // And the other way round: the transcript says done but the run is still going.
        let mut root = sighting("claude-code", "r");
        root.state = NodeState::Done;
        let mut root_run = sighting("claude-code", "r");
        root_run.run_id = Some(1);
        let mut child = sighting("claude-code", "c");
        child.parent = Some(root.key.clone());
        let fused = run(
            vec![
                ("claude_code", vec![root, child]),
                (DAEMON_RUNS_SOURCE, vec![root_run]),
            ],
            vec![],
        );
        assert_eq!(node(&fused, "claude-code:r").node.state, NodeState::Busy);
        assert_eq!(node(&fused, "claude-code:c").node.state, NodeState::Busy);
    }

    #[test]
    fn a_native_parent_beats_a_work_nodes_edge_which_beats_run_launch() {
        let root_a = sighting("claude-code", "a");
        let root_b = sighting("claude-code", "b");
        let root_c = sighting("claude-code", "c");
        let mut child = sighting("claude-code", "child");
        child.parent = Some(root_a.key.clone());

        let fused = run(
            vec![("claude_code", vec![root_a, root_b, root_c, child])],
            vec![
                vec![edge(("claude-code", "child"), ("claude-code", "b"))], // work_nodes
                vec![edge(("claude-code", "child"), ("claude-code", "c"))], // run_launch
            ],
        );
        assert_eq!(node(&fused, "claude-code:child").node.parent_id.as_deref(), Some("claude-code:a"));

        // Without the native parent, the first edge source wins.
        let child = sighting("codex-cli", "child");
        let fused = run(
            vec![(
                "claude_code",
                vec![sighting("claude-code", "b"), sighting("claude-code", "c")],
            ), ("codex_cli", vec![child])],
            vec![
                vec![edge(("codex-cli", "child"), ("claude-code", "b"))],
                vec![edge(("codex-cli", "child"), ("claude-code", "c"))],
            ],
        );
        let n = node(&fused, "codex-cli:child");
        assert_eq!(n.node.parent_id.as_deref(), Some("claude-code:b"));
        assert_eq!(n.node.root_id, "claude-code:b");
        assert_eq!(n.node.depth, 1);
    }

    #[test]
    fn an_edge_to_a_parent_nobody_sights_is_ignored_and_the_child_is_a_root() {
        let child = sighting("codex-cli", "child");
        let fused = run(
            vec![("codex_cli", vec![child])],
            vec![vec![edge(("codex-cli", "child"), ("claude-code", "ghost"))]],
        );
        let n = node(&fused, "codex-cli:child");
        assert_eq!(n.node.parent_id, None);
        assert_eq!(n.node.root_id, "codex-cli:child");
        assert_eq!(n.node.depth, 0);
        assert!(n.node.capabilities.pin);
    }

    #[test]
    fn an_ignored_edge_lets_the_next_source_apply() {
        let fused = run(
            vec![(
                "all",
                vec![sighting("claude-code", "p"), sighting("codex-cli", "c")],
            )],
            vec![
                vec![edge(("codex-cli", "c"), ("claude-code", "ghost"))],
                vec![edge(("codex-cli", "c"), ("claude-code", "p"))],
            ],
        );
        assert_eq!(node(&fused, "codex-cli:c").node.parent_id.as_deref(), Some("claude-code:p"));
    }

    #[test]
    fn a_native_parent_nobody_sights_does_not_block_a_work_nodes_edge() {
        let mut child = sighting("claude-code", "child");
        child.parent = Some(NodeKey::new("claude-code", "ghost"));
        let fused = run(
            vec![("claude_code", vec![child, sighting("claude-code", "p")])],
            vec![vec![edge(("claude-code", "child"), ("claude-code", "p"))]],
        );
        let n = node(&fused, "claude-code:child");
        assert_eq!(n.node.parent_id.as_deref(), Some("claude-code:p"));
        assert_eq!(n.node.root_id, "claude-code:p");
    }

    #[test]
    fn candidate_edges_in_one_list_keep_the_order_the_source_gave_them() {
        let sightings = || {
            vec![(
                "all",
                vec![
                    sighting("claude-code", "a"),
                    sighting("claude-code", "z"),
                    sighting("codex-cli", "c"),
                ],
            )]
        };
        // The first candidate's parent is unsighted: the second applies.
        let fused = run(
            sightings(),
            vec![vec![
                edge(("codex-cli", "c"), ("claude-code", "ghost")),
                edge(("codex-cli", "c"), ("claude-code", "z")),
            ]],
        );
        assert_eq!(node(&fused, "codex-cli:c").node.parent_id.as_deref(), Some("claude-code:z"));

        // Both are sighted: the first wins, even though "a" sorts before "z".
        let fused = run(
            sightings(),
            vec![vec![
                edge(("codex-cli", "c"), ("claude-code", "z")),
                edge(("codex-cli", "c"), ("claude-code", "a")),
            ]],
        );
        assert_eq!(node(&fused, "codex-cli:c").node.parent_id.as_deref(), Some("claude-code:z"));
    }

    #[test]
    fn an_edge_that_would_close_a_cycle_is_ignored_and_reported() {
        let a = sighting("claude-code", "a");
        let mut b = sighting("claude-code", "b");
        b.parent = Some(a.key.clone()); // native: b under a
        let fused = run(
            vec![("claude_code", vec![a, b])],
            vec![vec![edge(("claude-code", "a"), ("claude-code", "b"))]], // would put a under b
        );
        assert_eq!(node(&fused, "claude-code:a").node.parent_id, None);
        assert_eq!(node(&fused, "claude-code:b").node.parent_id.as_deref(), Some("claude-code:a"));
        assert_eq!(
            fused.ignored_cycles,
            vec![("claude-code:a".to_owned(), "claude-code:b".to_owned())]
        );
    }

    #[test]
    fn a_self_edge_is_a_cycle_too() {
        let fused = run(
            vec![("s", vec![sighting("claude-code", "a")])],
            vec![vec![edge(("claude-code", "a"), ("claude-code", "a"))]],
        );
        assert_eq!(node(&fused, "claude-code:a").node.parent_id, None);
        assert_eq!(fused.ignored_cycles.len(), 1);
    }

    #[test]
    fn depth_and_root_follow_a_cross_harness_chain() {
        let root = sighting("claude-code", "r");
        let mut mid = sighting("claude-code", "m");
        mid.parent = Some(root.key.clone());
        let leaf = sighting("codex-cli", "l");
        let fused = run(
            vec![("claude_code", vec![root, mid]), ("codex_cli", vec![leaf])],
            vec![vec![edge(("codex-cli", "l"), ("claude-code", "m"))]],
        );
        let leaf = node(&fused, "codex-cli:l");
        assert_eq!(leaf.node.depth, 2);
        assert_eq!(leaf.node.root_id, "claude-code:r");
        assert_eq!(node(&fused, "claude-code:m").node.depth, 1);
        assert_eq!(node(&fused, "claude-code:r").node.depth, 0);
    }

    #[test]
    fn capabilities_follow_the_rules() {
        let mut live = sighting("daemon", "run-1");
        live.run_id = Some(1);
        live.can_stop = true;
        // No transcript from any source (`has_transcript` stays false): a daemon run alone has none.
        let mut done = sighting("daemon", "run-2");
        done.run_id = Some(2);
        done.can_stop = true;
        done.state = NodeState::Done;
        let mut idle_child = sighting("claude-code", "c");
        idle_child.state = NodeState::Idle;
        idle_child.parent = Some(NodeKey::new("daemon", "run-1"));

        let fused = run(
            vec![
                (DAEMON_RUNS_SOURCE, vec![live, done]),
                ("claude_code", vec![idle_child]),
            ],
            vec![],
        );
        let live = node(&fused, "daemon:run-1").node.capabilities;
        assert_eq!(live, Capabilities { stop: true, open_run: true, full_transcript: false, pin: true });
        let done = node(&fused, "daemon:run-2").node.capabilities;
        assert!(!done.stop, "a finished run cannot be stopped");
        assert!(done.open_run);
        let child = node(&fused, "claude-code:c").node.capabilities;
        assert_eq!(child, Capabilities { stop: false, open_run: false, full_transcript: false, pin: false });
    }

    #[test]
    fn full_transcript_comes_from_whichever_source_has_one() {
        let mut native = sighting("claude-code", "s");
        native.has_transcript = true;
        let fused = run(vec![("claude_code", vec![native])], vec![]);
        let n = node(&fused, "claude-code:s");
        assert!(n.node.capabilities.full_transcript);
        assert_eq!(n.transcript_source, Some(0));
    }

    #[test]
    fn a_live_child_under_a_dead_root_becomes_lost_and_a_done_child_stays_done() {
        let mut root = sighting("claude-code", "r");
        root.state = NodeState::Done;
        let mut busy = sighting("claude-code", "busy");
        busy.parent = Some(root.key.clone());
        let mut done = sighting("claude-code", "done");
        done.parent = Some(root.key.clone());
        done.state = NodeState::Done;
        let mut idle = sighting("claude-code", "idle");
        idle.parent = Some(root.key.clone());
        idle.state = NodeState::Idle;
        // Only the daemon sighting can say `can_stop`, so the stoppable child is daemon-sighted:
        // without the Lost rule it would be stoppable.
        let mut idle_run = sighting("claude-code", "idle");
        idle_run.state = NodeState::Idle;
        idle_run.run_id = Some(3);
        idle_run.can_stop = true;

        let fused = run(
            vec![
                ("claude_code", vec![root, busy, done, idle]),
                (DAEMON_RUNS_SOURCE, vec![idle_run]),
            ],
            vec![],
        );
        assert_eq!(node(&fused, "claude-code:busy").node.state, NodeState::Lost);
        assert_eq!(node(&fused, "claude-code:idle").node.state, NodeState::Lost);
        assert!(!node(&fused, "claude-code:idle").node.capabilities.stop);
        assert_eq!(node(&fused, "claude-code:done").node.state, NodeState::Done);
        assert_eq!(node(&fused, "claude-code:r").node.state, NodeState::Done);
    }

    #[test]
    fn a_live_child_under_a_live_root_is_untouched() {
        let root = sighting("claude-code", "r");
        let mut child = sighting("claude-code", "c");
        child.parent = Some(root.key.clone());
        let fused = run(vec![("claude_code", vec![root, child])], vec![]);
        assert_eq!(node(&fused, "claude-code:c").node.state, NodeState::Busy);
    }

    #[test]
    fn last_activity_is_the_latest_of_the_sightings_and_the_output_is_sorted() {
        let mut native = sighting("claude-code", "s");
        native.last_activity_at = at(10);
        let mut daemon = sighting("claude-code", "s");
        daemon.last_activity_at = at(30);
        let other = sighting("claude-code", "a");
        let fused = run(
            vec![("claude_code", vec![native, other]), (DAEMON_RUNS_SOURCE, vec![daemon])],
            vec![],
        );
        assert_eq!(node(&fused, "claude-code:s").last_activity_at, at(30));
        let ids: Vec<_> = fused.nodes.iter().map(|n| n.node.id.as_str()).collect();
        assert_eq!(ids, ["claude-code:a", "claude-code:s"]);
    }
}
