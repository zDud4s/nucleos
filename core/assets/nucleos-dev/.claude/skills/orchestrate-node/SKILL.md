---
name: orchestrate-node
description: Use when you are a node controller of a node tree — dispatched through the `wf-orchestrator` cockpit with a node packet (`packets/node.md`) as a non-leaf node, or the root controller deciding to decompose a task that will not fit one leaf budget.
---

# Orchestrate node

You control one node. You decompose it into child nodes, dispatch them, track them in the
daemon's node tree, and close with a summary and a handoff file. You never implement. Limits
(`max_depth`, `max_width`, `leaf_budget`, `orchestrator_budget`, `ceiling`, `subtree_verify`)
are the `nodes:` block of the resolved speed table; the daemon refuses what breaks them.
Routing and the "One controller per node" rules live in `dispatch.md`; read it once.

## Procedure

1. **Read the node packet.** Goal, owned paths, budget, parent id, root id. If `node_active`
   shows your node is not `running`, stop.
2. **Decompose breadth-first.** Split by logical part, each with clear `owned_paths`. Look one
   level down, not the whole tree. Keep splitting a child (depth + 1) only until it fits
   `leaf_budget`. Estimate a leaf by reading with exact line ranges: a big file read whole
   does not justify a split, a split that cannot cut the reading does not help.
3. **Respect the limits.** Children sit at your depth + 1, which must stay within `max_depth`;
   no more than `max_width` open siblings. If the task needs more, merge parts or sequence
   them rather than widen.
4. **Open each child** with `node_open` (`parent_id` = your node, `kind` orchestrator or leaf,
   `title`, `goal`, `owned_paths`, `budget_tokens`). Siblings that run concurrently own
   **disjoint** paths; overlapping parts run in sequence or become one child.
5. **Dispatch by pointer.** The child's prompt carries its node id, goal, owned paths and
   pointers to the spec, plan and files it needs. Never paste file contents or sibling output.
   An orchestrator child gets `packets/node.md`; a leaf goes through the normal `dispatch.md`
   routing (`wf-planner`, `wf-executor`, `wf-reviewer`) with its own packet.
6. **Write through.** Call `node_update` at the moment of each decision or state change
   (split chosen, child opened, child closed, a path reassigned, `context_peak` when known).
   The tree must be rebuildable from the daemon alone.
7. **Read summaries only.** A child returns a summary of at most 1500 characters. Do not read
   its handoff file unless it closed `blocked` or `failed`.
8. **Handle `handoff`.** A child that closes `handoff` ran out of budget, not out of ideas.
   Read its tree entry with `node_tree` (`brief`), launch a fresh node on the remainder, and
   record a `rework` row (`mcp__nucleos__record`, `caught_by: budget`, payload
   `kind: decomposition`): the split was too coarse.
9. **Watch your own budget.** At about 80% of `orchestrator_budget` (hard `ceiling` 200k),
   stop opening children: write your handoff file, `node_close` with status `handoff`.
10. **Close.** `node_close` with `status` (`done`, `blocked`, `handoff`, `failed`), `summary`
    (at most 1500 characters: what landed, what is open, decisions) and `handoff_path`. The
    handoff file is `<state_dir>/nodes/<root_id>/<node_id>.md`, shaped by
    `packets/handoff.md`. For the root's verification, list the test and build commands for
    your branch in it; do not run them.

## Boundaries

- Direct children only, and only `wf-orchestrator`, `wf-planner`, `wf-executor`, `wf-reviewer`.
- No build, test or verify run. The one exception: `subtree_verify` true, then a
  branch-scoped `mcp__nucleos__verify` for your own branch. Otherwise the root controller's
  single final verification is the only one.
- No edits to source. A part that needs editing is a leaf for `wf-executor`.

## Red flags

| Rationalization | Reality |
|---|---|
| "I'll just fix this small part myself" | You have no Edit and no Bash; a part that needs editing is a leaf for `wf-executor`, and doing it here burns the orchestrator budget with no review. |
| "I'll paste the child's output into the next prompt" | Pass pointers, never content; pasted output grows every context in the branch. |
| "The file is big, so split it across children" | A big file read whole is not a reason to split; read with exact line ranges, and split only when the reading still exceeds `leaf_budget`. |
| "The two parts touch the same files but run fine in parallel" | Concurrent siblings need disjoint `owned_paths`; the daemon refuses the overlap and a forced one corrupts the tree. Sequence them or merge. |
| "I'll record the decisions in the handoff at the end" | Write through `node_update` as you decide; a node that dies before the end leaves no decisions. |
| "I'll read the child's handoff to be safe" | Read it only if the child closed `blocked` or `failed`; reading every handoff rebuilds the child's context in yours. |
| "I have budget left, one more child" | At about 80% of the budget you write the handoff and close `handoff`; the ceiling is a wall, not a target. |
| "I'll run the tests to check my branch" | Only the root verifies, or you when `subtree_verify` is true; a second test run is unmeasured and slows the tree. |
