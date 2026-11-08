//! Nested branches: an orchestrator branch is the parent of the branches its node starts. Pure
//! reads over the manifest, plus `ancestor_chain`, the one read of the node table that the
//! structural guard of `branch_start` needs; nothing here touches git.

use super::model::{Branch, Manifest};
use super::partition::Kind;
use super::root::ROOT_BRANCH;
use crate::conductor::depth::{AncestorView, DepthRefusal};
use crate::work_nodes::{self, NodeError};

/// The kind a start ends up with, and why it is not the one asked for when it differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub kind: Kind,
    /// Set when an orchestrator was asked for and a leaf was opened instead.
    pub degraded: Option<String>,
}

/// The words of a structural refusal; the same text `POST /nodes` answers with.
pub fn refusal_text(refusal: &DepthRefusal) -> &'static str {
    match refusal {
        DepthRefusal::SameScopeAsParent => "child has the same scope as its parent",
        DepthRefusal::SingleChildChain => {
            "child has the same scope as its parent and would only extend the chain"
        }
        DepthRefusal::RepeatedGoal => "child repeats the goal of an ancestor (repeated goal)",
        DepthRefusal::TreeNodeCeiling => "tree has reached its node ceiling",
    }
}

/// An orchestrator becomes a leaf, with the reason, when `check_child` refused it, when its
/// estimate fits `leaf_budget` (splitting it would only add a level), or when `child_depth` is the
/// last level allowed (its own children could not exist). No estimate keeps the orchestrator. A
/// leaf is always a leaf.
pub fn decide_kind(
    requested: Kind,
    estimate: Option<i64>,
    leaf_budget: i64,
    child_depth: i64,
    max_depth: Option<i64>,
    refusal: Option<DepthRefusal>,
) -> Decision {
    if requested == Kind::Leaf {
        return Decision {
            kind: Kind::Leaf,
            degraded: None,
        };
    }
    let reason = if let Some(refusal) = refusal {
        Some(format!(
            "{}: opened as a leaf, over budget if need be",
            refusal_text(&refusal)
        ))
    } else if let Some(estimate) = estimate.filter(|estimate| *estimate <= leaf_budget) {
        Some(format!(
            "estimate of {estimate} tokens fits leaf_budget {leaf_budget}: opened as a leaf"
        ))
    } else {
        max_depth
            .filter(|max| child_depth >= *max)
            .map(|max| {
                format!(
                    "depth {child_depth} is the limit of {max}, its children could not exist: opened as a leaf"
                )
            })
    };
    match reason {
        Some(reason) => Decision {
            kind: Kind::Leaf,
            degraded: Some(reason),
        },
        None => Decision {
            kind: Kind::Orchestrator,
            degraded: None,
        },
    }
}

/// The ids of the work nodes a run is bound to (`work_nodes.run_id`, kept as text).
pub async fn run_bound_nodes(
    pool: &sqlx::SqlitePool,
    run_id: i64,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM work_nodes WHERE run_id = ?")
        .bind(run_id.to_string())
        .fetch_all(pool)
        .await
}

/// How many nodes the tree rooted at `root_id` holds, the number `check_child` weighs against the
/// node ceiling.
pub async fn tree_node_count(pool: &sqlx::SqlitePool, root_id: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM work_nodes WHERE root_id = ?")
        .bind(root_id)
        .fetch_one(pool)
        .await
}

/// The root-first ancestor chain of `node` (the node last) that `check_child` reads. A copy of the
/// private one in `http/nodes.rs`: the bin cannot be reached from the lib.
pub async fn ancestor_chain(
    pool: &sqlx::SqlitePool,
    mut node: work_nodes::Node,
) -> Result<Vec<AncestorView>, NodeError> {
    let mut chain = Vec::new();
    loop {
        let open_children: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM work_nodes \
             WHERE parent_id = ? AND status IN ('planned', 'running')",
        )
        .bind(&node.id)
        .fetch_one(pool)
        .await?;
        chain.push(AncestorView {
            owned_paths: node.owned_paths.clone(),
            goal: node.goal.clone(),
            open_children,
        });
        let Some(parent_id) = node.parent_id else {
            break;
        };
        node = work_nodes::get(pool, &parent_id).await?;
    }
    chain.reverse();
    Ok(chain)
}

/// The branch bound to `caller_node`, or `None` when that is the stem branch (`root`) or no branch
/// is bound, so a stem-level start stays unparented.
pub fn parent_branch_of(manifest: &Manifest, caller_node: &str) -> Option<String> {
    manifest
        .branches
        .values()
        .find(|branch| branch.node_id.as_deref() == Some(caller_node))
        .map(|branch| branch.name.clone())
        .filter(|name| name != ROOT_BRANCH)
}

/// The parent, the grandparent and so on, cut where a name repeats or names no branch.
pub fn ancestors(manifest: &Manifest, branch: &str) -> Vec<String> {
    let mut seen = vec![branch.to_owned()];
    let mut out = Vec::new();
    let mut next = manifest
        .branches
        .get(branch)
        .and_then(|b| b.parent.as_deref());
    while let Some(name) = next {
        if seen.iter().any(|s| s == name) {
            break;
        }
        let Some(found) = manifest.branches.get(name) else {
            break;
        };
        seen.push(name.to_owned());
        out.push(name.to_owned());
        next = found.parent.as_deref();
    }
    out
}

/// The branches whose `parent` is `parent`, by name.
pub fn children<'a>(manifest: &'a Manifest, parent: &str) -> Vec<&'a Branch> {
    manifest
        .branches
        .values()
        .filter(|branch| branch.parent.as_deref() == Some(parent))
        .collect()
}

/// True when every child is `merged` or `dropped`. A `parked` child is not settled: the owner is
/// asked about it.
pub fn children_settled(manifest: &Manifest, parent: &str) -> bool {
    children(manifest, parent)
        .iter()
        .all(|child| matches!(child.state.as_str(), "merged" | "dropped"))
}

/// True for an orchestrator branch whose children all ended `dropped`: it has no diff of its own.
pub fn all_children_dropped(manifest: &Manifest, parent: &str) -> bool {
    let kids = children(manifest, parent);
    !kids.is_empty() && kids.iter().all(|child| child.state == "dropped")
}

/// The git ref a branch's worktree starts from: its parent's git branch when `branch.parent` is set,
/// else `feature` (the feature branch as `open_branch` computes it). `Err` names a parent that is
/// not live or has no git branch yet.
pub fn start_ref(manifest: &Manifest, branch: &Branch, feature: &str) -> Result<String, String> {
    match branch.parent.as_deref() {
        None => Ok(feature.to_owned()),
        Some(parent) => live_parent_ref(manifest, parent),
    }
}

/// The git branch `branch` merges into: its parent's for a child, the feature branch otherwise. `Err`
/// names a parent that is `dropped`, `parked` or has no git branch.
pub fn merge_target(manifest: &Manifest, branch: &str) -> Result<String, String> {
    match manifest
        .branches
        .get(branch)
        .and_then(|held| held.parent.as_deref())
    {
        None => Ok(manifest.feature_branch.clone()),
        Some(parent) => live_parent_ref(manifest, parent),
    }
}

fn live_parent_ref(manifest: &Manifest, parent: &str) -> Result<String, String> {
    let Some(found) = manifest.branches.get(parent) else {
        return Err(format!("parent branch {parent} is not in this tree"));
    };
    if matches!(found.state.as_str(), "dropped" | "parked") {
        return Err(format!(
            "parent branch {parent} is {}: its children merge nowhere",
            found.state
        ));
    }
    found
        .git_branch
        .clone()
        .ok_or_else(|| format!("parent branch {parent} has no git branch yet"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(branches: &[(&str, &str, Option<&str>, Option<&str>)]) -> Manifest {
        let mut manifest = Manifest::new("t", "feat/t", "goal", "C:/p", &[]);
        for (name, state, parent, node) in branches {
            let mut branch = Branch::new(name, &[], "backend", None);
            branch.state = (*state).to_owned();
            branch.parent = parent.map(str::to_owned);
            branch.node_id = node.map(str::to_owned);
            manifest.branches.insert((*name).to_owned(), branch);
        }
        manifest
    }

    const BUDGET: i64 = 120_000;

    fn decide(estimate: Option<i64>, depth: i64, refusal: Option<DepthRefusal>) -> Decision {
        decide_kind(Kind::Orchestrator, estimate, BUDGET, depth, Some(6), refusal)
    }

    #[test]
    fn an_orchestrator_above_the_budget_stays_an_orchestrator() {
        let decision = decide(Some(180_000), 2, None);
        assert_eq!(decision.kind, Kind::Orchestrator);
        assert_eq!(decision.degraded, None);
    }

    #[test]
    fn an_orchestrator_at_or_below_the_budget_becomes_a_leaf_with_a_reason() {
        for estimate in [BUDGET, 40_000] {
            let decision = decide(Some(estimate), 2, None);
            assert_eq!(decision.kind, Kind::Leaf, "{estimate}");
            let reason = decision.degraded.expect("a reason");
            assert!(reason.contains(&estimate.to_string()), "{reason}");
            assert!(reason.contains("leaf_budget 120000"), "{reason}");
        }
    }

    #[test]
    fn an_orchestrator_without_an_estimate_is_kept() {
        let decision = decide(None, 2, None);
        assert_eq!(decision.kind, Kind::Orchestrator);
        assert_eq!(decision.degraded, None);
    }

    #[test]
    fn an_orchestrator_at_max_depth_becomes_a_leaf() {
        let decision = decide(Some(500_000), 6, None);
        assert_eq!(decision.kind, Kind::Leaf);
        assert!(decision.degraded.unwrap().contains("depth 6"));
        assert_eq!(decide(Some(500_000), 5, None).kind, Kind::Orchestrator);
        let unlimited = decide_kind(Kind::Orchestrator, None, BUDGET, 40, None, None);
        assert_eq!(unlimited.kind, Kind::Orchestrator);
    }

    #[test]
    fn a_check_child_refusal_degrades_an_orchestrator_and_names_it() {
        let decision = decide(Some(500_000), 2, Some(DepthRefusal::RepeatedGoal));
        assert_eq!(decision.kind, Kind::Leaf);
        assert!(decision.degraded.unwrap().contains("repeated goal"));
    }

    #[test]
    fn a_leaf_request_is_never_changed() {
        let decision = decide_kind(
            Kind::Leaf,
            Some(900_000),
            BUDGET,
            6,
            Some(6),
            Some(DepthRefusal::RepeatedGoal),
        );
        assert_eq!(decision.kind, Kind::Leaf);
        assert_eq!(decision.degraded, None);
    }

    #[test]
    fn the_stem_branch_has_no_parent() {
        let m = manifest(&[("root", "building", None, Some("n-root"))]);
        assert_eq!(parent_branch_of(&m, "n-root"), None);
        assert_eq!(parent_branch_of(&m, "unbound"), None);
    }

    #[test]
    fn a_child_of_an_orchestrator_branch_finds_that_branch() {
        let m = manifest(&[
            ("root", "building", None, Some("n-root")),
            ("api", "building", None, Some("n-api")),
        ]);
        assert_eq!(parent_branch_of(&m, "n-api"), Some("api".to_owned()));
    }

    #[test]
    fn ancestors_walk_up_and_stop_on_a_cycle() {
        let m = manifest(&[
            ("a", "building", None, None),
            ("b", "building", Some("a"), None),
            ("c", "building", Some("b"), None),
        ]);
        assert_eq!(ancestors(&m, "c"), ["b", "a"]);
        let cyclic = manifest(&[
            ("a", "building", Some("b"), None),
            ("b", "building", Some("a"), None),
        ]);
        assert_eq!(ancestors(&cyclic, "a"), ["b"]);
    }

    #[test]
    fn a_child_starts_from_and_merges_into_a_live_parent_only() {
        let mut m = manifest(&[
            ("p", "building", None, None),
            ("c", "green", Some("p"), None),
            ("s", "green", None, None),
        ]);
        m.branches.get_mut("p").unwrap().git_branch = Some("tree/t/p".to_owned());
        let child = m.branches["c"].clone();
        assert_eq!(start_ref(&m, &child, "feat/t").unwrap(), "tree/t/p");
        assert_eq!(merge_target(&m, "c").unwrap(), "tree/t/p");
        assert_eq!(merge_target(&m, "s").unwrap(), "feat/t");
        for state in ["dropped", "parked"] {
            m.branches.get_mut("p").unwrap().state = state.to_owned();
            let error = merge_target(&m, "c").unwrap_err();
            assert!(error.contains("parent branch p"), "{error}");
            assert!(start_ref(&m, &child, "feat/t").is_err());
        }
    }

    #[test]
    fn all_children_dropped_needs_children_and_no_survivor() {
        let m = manifest(&[
            ("p", "building", None, None),
            ("a", "dropped", Some("p"), None),
            ("b", "merged", Some("p"), None),
        ]);
        assert!(!all_children_dropped(&m, "p"));
        assert!(!all_children_dropped(&m, "a"));
        let all = manifest(&[
            ("p", "building", None, None),
            ("a", "dropped", Some("p"), None),
        ]);
        assert!(all_children_dropped(&all, "p"));
    }

    #[test]
    fn children_settled_only_when_every_child_is_merged_or_dropped() {
        let done = manifest(&[
            ("p", "building", None, None),
            ("m", "merged", Some("p"), None),
            ("d", "dropped", Some("p"), None),
        ]);
        assert!(children_settled(&done, "p"));
        assert_eq!(children(&done, "p").len(), 2);
        for state in ["parked", "building", "green"] {
            let m = manifest(&[
                ("p", "building", None, None),
                ("m", "merged", Some("p"), None),
                ("x", state, Some("p"), None),
            ]);
            assert!(!children_settled(&m, "p"), "{state}");
        }
    }
}
