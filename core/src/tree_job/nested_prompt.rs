//! The text an orchestrator branch's agent is given on top of its task: what it is, how it splits,
//! and where it stops. The daemon enforces the same rules (`nesting`, `check_child`, `max_depth`);
//! this only tells the agent so it does not learn them by being refused.

use super::partition::floor;

/// The default size, in tokens, an orchestrator's own context is meant to stay under.
pub const DEFAULT_ORCHESTRATOR_BUDGET: i64 = 150_000;

/// The preamble prepended to the prompt of a branch opened as `kind: orchestrator`.
pub fn orchestrator_preamble(
    depth: i64,
    max_depth: Option<i64>,
    leaf_budget: i64,
    orchestrator_budget: i64,
    own_node: &str,
    tree_job: &str,
) -> String {
    let limit = match max_depth {
        Some(max) => format!(
            "The deepest level allowed is {max}; a piece you start at depth {max} can only be a leaf."
        ),
        None => "There is no fixed deepest level, but the daemon refuses a split that does not divide its parent."
            .to_owned(),
    };
    let small = floor(leaf_budget);
    format!(
        "You are an orchestrator at depth {depth} of this tree. You edit no files: you split your \
         task into pieces and start each with the `tree_branch_start` tool, passing `id` = {tree_job} \
         (the tree job id, not a node id) and `node_id` = {own_node} (your own node id; a node opens \
         only its own children). Keep your own context under {orchestrator_budget} tokens.\n\
         For every piece pass `estimate_tokens`, your estimate of what building it takes. A piece \
         whose estimate is at most {leaf_budget} tokens is a leaf: start it with `kind: leaf` (or no \
         kind). Start a piece as `kind: orchestrator` only when its estimate exceeds {leaf_budget} \
         tokens; it will split itself again. Fold pieces smaller than {small} tokens into a \
         neighbour instead of starting a run for each. {limit} At the limit, make the best leaf you \
         can and say in your summary that it exceeded the budget. A child merges into your branch; \
         when every child has ended, your branch is gated and merged for you."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preamble_names_depth_ids_budget_and_the_floor() {
        let text = orchestrator_preamble(3, Some(6), 120_000, 150_000, "node-own", "tree-job-1");
        for needle in [
            "depth 3",
            "node-own",
            "tree-job-1",
            "tree_branch_start",
            "estimate_tokens",
            "120000",
            "150000",
            "30000",
            "deepest level allowed is 6",
        ] {
            assert!(text.contains(needle), "{needle}: {text}");
        }
    }

    #[test]
    fn the_preamble_without_a_limit_says_so() {
        let text = orchestrator_preamble(2, None, 120_000, 150_000, "n", "t");
        assert!(text.contains("no fixed deepest level"), "{text}");
    }
}
