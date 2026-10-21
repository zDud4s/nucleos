//! The legacy bridge: a [`JobView`] read as the walker's input, and the walker's answer read back
//! as a [`Next`].
//!
//! It exists for the differential test (spec 12.1): the old tables keep deciding, and the walker
//! has to agree with every one of them. Until the engine builds `WalkState` from `job_nodes` rows
//! nothing in production but the shadow differential calls it.
// `walk_state_of` is only called by tests since C7 made the engine build its own `WalkState`;
// `step_as_next` is live (the shadow differential).
#![allow(dead_code)]

use super::*;
use crate::workflows::facts::Facts;
use crate::workflows::graph::Graph as WorkflowGraph;
use crate::workflows::walk::{Disabled, NodeState, Step, WalkState};

/// The walker's input for a legacy `JobView`: node states for the singleton nodes, the work fan's
/// rows, and the one engine fact the autopilot's round decision reads.
///
/// `graph` is accepted so the mapping can grow with the graph; today the autopilot's node ids
/// (`spec`, `plan`, `review`, fan `work`) are the contract.
pub(crate) fn walk_state_of(_graph: &WorkflowGraph, view: &JobView) -> (WalkState, Facts) {
    let mut nodes = std::collections::BTreeMap::new();
    let mut disabled = std::collections::BTreeMap::new();

    if view.specced {
        nodes.insert("spec".to_string(), NodeState::Done);
    } else if view.speccing {
        nodes.insert("spec".to_string(), NodeState::Running);
    }
    if view.planned {
        nodes.insert("plan".to_string(), NodeState::Done);
    } else if view.planning {
        nodes.insert("plan".to_string(), NodeState::Running);
    }

    // The old decision consults these two BEFORE the review state, even with a review running.
    if view.rounds.replanned == Replan::Done || view.rounds.round_added_nothing {
        nodes.insert("review".to_string(), NodeState::Skipped);
    } else {
        match view.review {
            ReviewState::Pending => {}
            ReviewState::Running => {
                nodes.insert("review".to_string(), NodeState::Running);
            }
            ReviewState::Done => {
                nodes.insert("review".to_string(), NodeState::Done);
            }
            ReviewState::Skipped => {
                nodes.insert("review".to_string(), NodeState::Skipped);
            }
            ReviewState::Retry => {
                nodes.insert("review".to_string(), NodeState::RetryOwed);
            }
            // A job with review off is the shadow-mode case of spec 6.
            ReviewState::NotWanted => {
                disabled.insert("review".to_string(), Disabled::Shadow);
            }
        }
    }

    let mut fans = std::collections::BTreeMap::new();
    fans.insert("work".to_string(), view.items.clone());

    let state = WalkState {
        round: view.rounds.round,
        max_rounds: view.rounds.max_rounds,
        dry_rounds: view.rounds.dry_rounds,
        replanning: view.rounds.replanning,
        a_broken_item_may_go_round_again: view.rounds.a_broken_item_may_go_round_again,
        has_team: view.has_team,
        max_parallel: view.max_parallel,
        nodes,
        fans,
        disabled,
    };

    // What the engine injects once the replan has landed (M1 S2-A).
    let mut facts = Facts::new();
    let done = view.rounds.replanned == Replan::Done;
    facts.insert("plan.done", if done { "true" } else { "false" });
    (state, facts)
}

/// The walker's answer as the old decision would have phrased it, or `None` where the old
/// vocabulary has no word for it (`Stuck`, or a node the legacy job never had).
pub(crate) fn step_as_next(step: &Step, _graph: &WorkflowGraph) -> Option<Next> {
    Some(match step {
        Step::RunNode {
            node_id,
            item: None,
        } => match node_id.as_str() {
            "spec" => Next::SpawnSpec,
            "plan" => Next::SpawnPlan,
            "review" => Next::SpawnReview,
            _ => return None,
        },
        Step::RunNode {
            item: Some(ordinal),
            ..
        } => Next::RunGate { ordinal: *ordinal },
        Step::FanOut { items, .. } => Next::SpawnImplement(items.clone()),
        Step::MergeItem { item, .. } => Next::MergeItem { ordinal: *item },
        Step::SettleRevert {
            item: Some(ordinal),
            ..
        } => Next::SettleRevert { ordinal: *ordinal },
        Step::SettleRevert { item: None, .. } => return None,
        Step::Orphan { item, .. } => Next::Orphan { ordinal: *item },
        Step::Replan { .. } => Next::SpawnReplan,
        Step::Wait => Next::Wait,
        Step::GateErrored { .. } => Next::Finish(Outcome::GateErrored),
        Step::Finish(outcome) => Next::Finish(*outcome),
        Step::Stuck(_) => return None,
    })
}
