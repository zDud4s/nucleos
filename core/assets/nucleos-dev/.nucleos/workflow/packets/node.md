# Node Packet

<!-- READ-ONLY TEMPLATE. See packets/README.md for the contract.
  Filled by the parent (or the root controller) and handed to a sub-orchestrator or leaf.
  Pointers only: never paste the parent's plan or spec into this packet. -->

Node ID: <!-- the id returned by mcp__nucleos__node_open -->
Root ID: <!-- the work tree's root node id -->
Parent ID: <!-- the parent node id, or `none` for a root -->
Task ID: <!-- the ROOT's task_id; ledger rows for this node use it with node_id in the payload -->
Kind: <!-- orchestrator | leaf -->
Title: <!-- a few words -->

## Goal
<!-- 1-3 verifiable sentences: what must be true when this node closes `done` -->

## Ownership
Owned paths: <!-- globs; the daemon refuses overlap with a running sibling. Touch nothing outside them. -->
Budget tokens: <!-- planned context budget for this node -->

## Inherited limits
Current depth: <!-- this node's depth in the tree -->
Max depth: <!-- from the speed profile; a child that would exceed it is refused by node_open -->
Max width: <!-- max open siblings per parent -->
Leaf budget: <!-- default budget_tokens for a leaf child -->
Orchestrator budget: <!-- default budget_tokens for an orchestrator child -->
Ceiling: <!-- `nodes.ceiling` from the speed profile (200000); absolute context ceiling; at ~80% of Budget tokens stop, write the handoff, close `handoff` -->
Subtree verify: <!-- `nodes.subtree_verify` (true | false), as resolved for this task -->

## Pointers
<!-- paths with line ranges into the parent plan/spec, one per line: `<path>:<start>-<end> - why it matters`.
  Never content. Read only these ranges. -->

## Constraints
- Run no build or test tool (compilers, package managers, test runners): list the exact commands in the handoff for the root to run.
- Write state through `mcp__nucleos__node_update` on every status change, decision and context peak.
- Dispatch only your own direct children (kind `orchestrator` or `leaf`); never another node's phases.
- Close through `mcp__nucleos__node_close` with a summary of at most 1500 characters and the path of the full handoff, written first from `packets/handoff.md`.
- An orchestrator never implements and never rotates its own context: at the ceiling it closes `handoff` and the parent relaunches a fresh node from the daemon tree.

## Rules by role

<!-- Read by `scripts/tree.py prompt`: `### all` plus the subsection of the node's role are copied into the generated prompt under `## Rules`. This is the only copy of these rules; edit them here, never in a skill or in a hand-written prompt. -->

### all
- No subagent runs `cargo` (build, test, check, clippy or fmt). Put the exact command in your report under `Commands for the root`; the root runs it.
- Run no other build or test tool either (compilers, package managers, test runners).
- Make no commit, merge, stash or push.
- The contract file is read-only. A gap in it means: stop and return `Outcome: contract_request` with the options. Never decide it yourself.
- At ~80% of your context budget, or near 200k tokens, stop and write your handoff.

### leaf
- Write only inside your Owned paths. A file outside them is reverted by the root and fails your node.
- You open no child nodes.

### orchestrator
- Edit no file: you have no Edit tool, and an orchestrator that changes code is refused.
- Open children with `node_open`; their `owned_paths` must be disjoint and contained in yours.
- Split only into independent pieces. A piece that needs a new interface between siblings, or a path outside yours, is a `contract_request`, not a split.
- Close yourself `done` when every child is closed.

### review
- Read only: change nothing, not even a comment.
- Return the findings in `Open items / next step`, one per line as `R<n> <file>: <note>`, most serious first.

## Report format

<!-- Read by `scripts/tree.py prompt`: `### all` plus `### file` (leaf, orchestrator) or `### message` (review) are copied into the generated prompt under `## Report`. Placeholders filled by tree.py: {max_lines} (tree.report_max_lines), {role}, {status_map} (from tree_state.OUTCOME_STATUS) and {handoff} (this node's handoff path). The only copy of this text; edit it here. -->

### all
- Finish with a report in the format of `.nucleos/workflow/packets/handoff.md`: at most {max_lines} lines, not counting HTML comments.
- Header: `Role: {role}`; `Outcome` is one of done, split, contract_request, blocked, failed; `Status` must match it ({status_map}).

### file
- Write it to `{handoff}` before you call node_close.

### message
- You have no Write tool: return the report as your final message; the root records it.
