//! Builders and fakes for the fleet tests. `#[cfg(test)]` only.
// Later chunks (book, hub, the adapters) use the builders this chunk does not.
#![allow(dead_code)]

use chrono::DateTime;

use super::fuse::FusedNode;
use super::model::{Capabilities, FleetNode, NodeKey, NodeState, Timestamp};
use super::source::{Edge, Sighting};

/// A fixed instant plus `secs`: every test reasons in offsets from one origin.
pub(crate) fn at(secs: i64) -> Timestamp {
    DateTime::from_timestamp(1_760_000_000 + secs, 0).expect("a valid timestamp")
}

/// A busy, line-bearing sighting with nothing else set; tests mutate the fields they care about.
pub(crate) fn sighting(harness: &str, native_id: &str) -> Sighting {
    Sighting {
        key: NodeKey::new(harness, native_id),
        parent: None,
        harness_label: harness.to_owned(),
        model: None,
        role: None,
        label: native_id.to_owned(),
        project: None,
        state: NodeState::Busy,
        started_at: at(0),
        ended_at: None,
        last_activity_at: at(0),
        run_id: None,
        can_stop: false,
        has_lines: true,
        has_transcript: false,
        usage: None,
    }
}

pub(crate) fn edge(child: (&str, &str), parent: (&str, &str)) -> Edge {
    Edge {
        child: NodeKey::new(child.0, child.1),
        parent: NodeKey::new(parent.0, parent.1),
    }
}

/// A fused node. `parent` is `None` for a root; `root` is the key of the tree's root.
pub(crate) fn fused(
    key: (&str, &str),
    parent: Option<(&str, &str)>,
    root: (&str, &str),
    depth: u32,
    state: NodeState,
) -> FusedNode {
    let key = NodeKey::new(key.0, key.1);
    FusedNode {
        node: FleetNode {
            id: key.id(),
            parent_id: parent.map(|p| NodeKey::new(p.0, p.1).id()),
            root_id: NodeKey::new(root.0, root.1).id(),
            depth,
            harness: key.harness.clone(),
            model: None,
            role: None,
            label: key.native_id.clone(),
            state,
            started_at: at(0),
            ended_at: None,
            pinned: false,
            done_omitted: 0,
            project: None,
            run_id: None,
            capabilities: Capabilities {
                stop: false,
                open_run: false,
                full_transcript: false,
                pin: parent.is_none(),
            },
        },
        key,
        last_activity_at: at(0),
        usage: None,
        lines_source: Some(0),
        transcript_source: None,
        daemon_source: None,
    }
}

pub(crate) fn root_node(key: (&str, &str), state: NodeState) -> FusedNode {
    fused(key, None, key, 0, state)
}

pub(crate) fn child_node(key: (&str, &str), root: (&str, &str), state: NodeState) -> FusedNode {
    fused(key, Some(root), root, 1, state)
}
