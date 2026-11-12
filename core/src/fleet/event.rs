//! The events of `GET /fleet/live`, one JSON object per line, discriminated by `t`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::model::{FleetNode, NodeId, NodeMetrics, TermLine};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum FleetEvent {
    /// First event of every connection, and the answer to a client that fell behind.
    Snapshot {
        nodes: Vec<FleetNode>,
        metrics: Vec<NodeMetrics>,
        lines: BTreeMap<NodeId, Vec<TermLine>>,
    },
    /// A whole node: new, or its structure or state changed.
    Node {
        node: FleetNode,
    },
    /// Metrics that changed, batched; never forces layout.
    Metrics {
        metrics: Vec<NodeMetrics>,
    },
    Lines {
        node_id: NodeId,
        lines: Vec<TermLine>,
    },
    /// The whole tree under this root is gone.
    Removed {
        node_id: NodeId,
    },
    Heartbeat,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::model::TermKind;

    #[test]
    fn the_events_have_the_wire_shape_the_contract_names() {
        let heartbeat = serde_json::to_value(FleetEvent::Heartbeat).unwrap();
        assert_eq!(heartbeat, serde_json::json!({"t": "heartbeat"}));

        let lines = FleetEvent::Lines {
            node_id: "a:1".into(),
            lines: vec![TermLine::new(TermKind::Text, "hi")],
        };
        assert_eq!(
            serde_json::to_value(lines).unwrap(),
            serde_json::json!({"t": "lines", "node_id": "a:1",
                               "lines": [{"kind": "text", "text": "hi"}]})
        );

        let removed = FleetEvent::Removed {
            node_id: "a:1".into(),
        };
        assert_eq!(
            serde_json::to_value(removed).unwrap(),
            serde_json::json!({"t": "removed", "node_id": "a:1"})
        );

        let snapshot = FleetEvent::Snapshot {
            nodes: vec![],
            metrics: vec![],
            lines: BTreeMap::new(),
        };
        assert_eq!(
            serde_json::to_value(snapshot).unwrap(),
            serde_json::json!({"t": "snapshot", "nodes": [], "metrics": [], "lines": {}})
        );
    }
}
