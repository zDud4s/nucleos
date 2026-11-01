//! Structural guards on opening a child node, in place of a fixed depth cap.
//!
//! Depth follows the task's complexity, so what stops a runaway tree is its shape:
//! a child must divide its parent's scope, must not restate an ancestor's goal, and
//! must fit under the tree's node ceiling. [`check_child`] is pure: the caller reads
//! the chain and the total, and acts on the refusal.
//!
//! Two checks stay in `work_nodes` and are not repeated here: containment of the
//! child's paths in the parent's (`check_containment`) and the optional configured
//! `max_depth`.

use std::collections::BTreeSet;

/// What the guards need to know about one ancestor of the node being opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AncestorView {
    pub owned_paths: Vec<String>,
    pub goal: String,
    pub open_children: i64,
}

/// The node being opened, as requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildArgs {
    pub owned_paths: Vec<String>,
    pub goal: String,
}

/// Per-tree limits. Deliberately has no depth field: see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeLimits {
    pub max_nodes: i64,
}

impl Default for TreeLimits {
    fn default() -> Self {
        Self { max_nodes: 300 }
    }
}

/// Why a child was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepthRefusal {
    /// The parent already has a child, and this one owns the parent's whole scope.
    SameScopeAsParent,
    /// It would be the parent's only child, on the parent's whole scope: a handoff,
    /// not a split.
    SingleChildChain,
    /// Its goal normalizes to the same text as an ancestor's.
    RepeatedGoal,
    /// The tree is already at its node ceiling.
    TreeNodeCeiling,
}

/// Normalizes a goal for comparison: lowercase, whitespace runs collapsed to one
/// space, trimmed.
pub fn goal_fingerprint(goal: &str) -> String {
    goal.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Decides whether a child may open. `chain` is root first, so the direct parent is
/// its last element; an empty chain means a root. The first refusal wins: ceiling,
/// then scope, then goal.
pub fn check_child(
    chain: &[AncestorView],
    args: &ChildArgs,
    tree_total: i64,
    limits: &TreeLimits,
) -> Result<(), DepthRefusal> {
    if tree_total >= limits.max_nodes {
        return Err(DepthRefusal::TreeNodeCeiling);
    }

    // A child with no paths owns nothing, so it cannot fail to divide its parent.
    let same_scope_parent = chain.last().filter(|parent| {
        !args.owned_paths.is_empty() && path_set(&args.owned_paths) == path_set(&parent.owned_paths)
    });
    if let Some(parent) = same_scope_parent {
        return Err(if parent.open_children >= 1 {
            DepthRefusal::SameScopeAsParent
        } else {
            DepthRefusal::SingleChildChain
        });
    }

    let fingerprint = goal_fingerprint(&args.goal);
    if chain
        .iter()
        .any(|ancestor| goal_fingerprint(&ancestor.goal) == fingerprint)
    {
        return Err(DepthRefusal::RepeatedGoal);
    }

    Ok(())
}

/// A path list as a set: order and duplicates do not change a scope.
fn path_set(paths: &[String]) -> BTreeSet<&str> {
    paths.iter().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    fn ancestor(owned: &[&str], goal: &str, open_children: i64) -> AncestorView {
        AncestorView {
            owned_paths: paths(owned),
            goal: goal.to_string(),
            open_children,
        }
    }

    fn child(owned: &[&str], goal: &str) -> ChildArgs {
        ChildArgs {
            owned_paths: paths(owned),
            goal: goal.to_string(),
        }
    }

    /// `size` distinct paths; a smaller size is a strict subset of a larger one.
    fn scope_of(size: usize) -> Vec<String> {
        (0..size).map(|index| format!("src/f{index}.rs")).collect()
    }

    /// Root first, every level a strict subset of the one above and with its own
    /// goal. The last level still owns two paths, so a smaller child fits under it.
    fn shrinking_chain(levels: usize) -> Vec<AncestorView> {
        (0..levels)
            .map(|level| AncestorView {
                owned_paths: scope_of(levels + 1 - level),
                goal: format!("goal of level {level}"),
                open_children: 0,
            })
            .collect()
    }

    #[test]
    fn child_with_same_owned_paths_as_parent_is_refused() {
        // G1: the parent already has a child, and this one divides nothing.
        for open_children in [1, 2] {
            let chain = [ancestor(
                &["src/a.rs", "src/b.rs"],
                "split the module",
                open_children,
            )];
            let args = child(&["src/a.rs", "src/b.rs"], "write the parser");

            assert_eq!(
                check_child(&chain, &args, 2, &TreeLimits::default()),
                Err(DepthRefusal::SameScopeAsParent),
                "open_children = {open_children}"
            );
        }
    }

    #[test]
    fn single_child_with_same_scope_is_refused_as_chain() {
        // G2: it would be the parent's only child, on the parent's whole scope:
        // that is a handoff, not a split.
        let chain = [ancestor(&["src/a.rs", "src/b.rs"], "split the module", 0)];
        let args = child(&["src/a.rs", "src/b.rs"], "write the parser");

        assert_eq!(
            check_child(&chain, &args, 1, &TreeLimits::default()),
            Err(DepthRefusal::SingleChildChain)
        );
    }

    #[test]
    fn goal_fingerprint_equal_to_an_ancestor_is_refused() {
        // The match is on the root, not the direct parent, and the scope still
        // shrinks: only the goal can refuse this child.
        let chain = [
            ancestor(
                &["src/a.rs", "src/b.rs", "src/c.rs"],
                "Split the billing module",
                1,
            ),
            ancestor(&["src/a.rs", "src/b.rs"], "Extract the invoice parser", 1),
        ];
        let args = child(&["src/a.rs"], "  split the   BILLING module\n");

        assert_eq!(
            check_child(&chain, &args, 3, &TreeLimits::default()),
            Err(DepthRefusal::RepeatedGoal)
        );
    }

    #[test]
    fn goal_fingerprint_ignores_case_and_whitespace() {
        let expected = goal_fingerprint("fix the bug");

        assert_eq!(goal_fingerprint("FIX The bUg"), expected, "case");
        assert_eq!(goal_fingerprint("  fix the bug \n"), expected, "trim");
        assert_eq!(goal_fingerprint("fix   the\t\r\n bug"), expected, "runs");
        assert_ne!(goal_fingerprint("fix the build"), expected, "other goal");
    }

    #[test]
    fn total_node_ceiling_refuses_at_300() {
        let limits = TreeLimits::default();
        assert_eq!(limits.max_nodes, 300);

        // Scope and goal are both acceptable: only the total can refuse.
        let chain = [ancestor(&["src/a.rs", "src/b.rs"], "split the module", 1)];
        let args = child(&["src/a.rs"], "write the parser");

        assert_eq!(check_child(&chain, &args, 299, &limits), Ok(()));
        assert_eq!(
            check_child(&chain, &args, 300, &limits),
            Err(DepthRefusal::TreeNodeCeiling)
        );
        assert_eq!(
            check_child(&chain, &args, 301, &limits),
            Err(DepthRefusal::TreeNodeCeiling)
        );
        // The ceiling also holds for a root, which has no chain to check.
        assert_eq!(
            check_child(&[], &args, 300, &limits),
            Err(DepthRefusal::TreeNodeCeiling)
        );
        // The limit is the configured one, not a constant.
        assert_eq!(
            check_child(&chain, &args, 5, &TreeLimits { max_nodes: 5 }),
            Err(DepthRefusal::TreeNodeCeiling)
        );
    }

    #[test]
    fn no_depth_cap_by_default() {
        // Exhaustive destructuring: a depth field added to the limits breaks this
        // test, since the configured `max_depth` stays in `work_nodes`.
        let TreeLimits { max_nodes } = TreeLimits::default();
        let limits = TreeLimits { max_nodes };

        let chain = shrinking_chain(50);
        assert_eq!(chain.len(), 50);
        let args = ChildArgs {
            owned_paths: scope_of(1),
            goal: "goal of the new child".to_string(),
        };

        assert_eq!(check_child(&chain, &args, 51, &limits), Ok(()));
    }

    #[test]
    fn nested_split_ten_levels_with_shrinking_scope_is_allowed() {
        // Opens the tree from the root down: each level is checked as a child of
        // the levels above it.
        let levels = shrinking_chain(10);

        for (depth, level) in levels.iter().enumerate() {
            let args = ChildArgs {
                owned_paths: level.owned_paths.clone(),
                goal: level.goal.clone(),
            };

            assert_eq!(
                check_child(
                    &levels[..depth],
                    &args,
                    depth as i64,
                    &TreeLimits::default()
                ),
                Ok(()),
                "depth = {depth}"
            );
        }
    }

    #[test]
    fn same_scope_ignores_order_and_duplicates() {
        let limits = TreeLimits::default();
        let chain = [ancestor(&["src/a.rs", "src/b.rs"], "split the module", 1)];

        assert_eq!(
            check_child(
                &chain,
                &child(&["src/b.rs", "src/a.rs"], "write the parser"),
                2,
                &limits
            ),
            Err(DepthRefusal::SameScopeAsParent),
            "order"
        );
        assert_eq!(
            check_child(
                &chain,
                &child(&["src/b.rs", "src/a.rs", "src/b.rs"], "write the parser"),
                2,
                &limits
            ),
            Err(DepthRefusal::SameScopeAsParent),
            "duplicates in the child"
        );

        let duplicated_parent = [ancestor(
            &["src/a.rs", "src/a.rs", "src/b.rs"],
            "split the module",
            0,
        )];
        assert_eq!(
            check_child(
                &duplicated_parent,
                &child(&["src/b.rs", "src/a.rs"], "write the parser"),
                1,
                &limits
            ),
            Err(DepthRefusal::SingleChildChain),
            "duplicates in the parent"
        );

        // As long as the parent's list, but a smaller set: it does shrink.
        assert_eq!(
            check_child(
                &chain,
                &child(&["src/a.rs", "src/a.rs"], "write the parser"),
                2,
                &limits
            ),
            Ok(()),
            "same length, smaller set"
        );
    }

    #[test]
    fn child_without_paths_is_not_refused_on_scope() {
        // A read-only child divides nothing but owns nothing either, even when
        // its parent's set is just as empty.
        let limits = TreeLimits::default();
        let args = child(&[], "review the parser");

        for open_children in [0, 1] {
            let empty_parent = [ancestor(&[], "split the module", open_children)];
            assert_eq!(
                check_child(&empty_parent, &args, 2, &limits),
                Ok(()),
                "empty parent, open_children = {open_children}"
            );

            let parent = [ancestor(&["src/a.rs"], "split the module", open_children)];
            assert_eq!(
                check_child(&parent, &args, 2, &limits),
                Ok(()),
                "parent with paths, open_children = {open_children}"
            );
        }
    }

    #[test]
    fn empty_chain_is_allowed_below_the_ceiling() {
        let limits = TreeLimits::default();

        assert_eq!(
            check_child(&[], &child(&["src/a.rs"], "split the module"), 0, &limits),
            Ok(())
        );
        assert_eq!(
            check_child(&[], &child(&[], "survey the module"), 0, &limits),
            Ok(())
        );
        assert_eq!(
            check_child(&[], &child(&["src/a.rs"], "split the module"), 299, &limits),
            Ok(())
        );
    }

    #[test]
    fn smaller_scope_with_new_goal_is_allowed() {
        let args = child(&["src/a.rs"], "write the parser");

        for open_children in [0, 1] {
            let chain = [ancestor(
                &["src/a.rs", "src/b.rs"],
                "split the module",
                open_children,
            )];

            assert_eq!(
                check_child(&chain, &args, 2, &TreeLimits::default()),
                Ok(()),
                "open_children = {open_children}"
            );
        }
    }
}
