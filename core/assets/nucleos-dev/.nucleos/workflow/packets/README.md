# Packet schemas — the contract

Four of the original five schemas here end their header with "See packets/README.md for the contract." This is that file. It states only what the templates assume and nothing new.

## What these files are

Read-only forms. A phase READS a schema and EMITS a filled copy; it never edits the schema itself. Editing one during a task is a workflow violation, not a shortcut — `workflow.md`'s layer model calls this out because a phase that "fixes" a form has changed the contract every later phase is judged against, silently and for everyone.

| Schema | Filled by | Holds |
|---|---|---|
| `intent.md` | controller, before planning | the confirmed intent; required when Size is `medium`/`large` or Risk is `elevated` |
| `plan.md` | `plan` | triage verdict, the smallest safe plan, one or more inline execution packets, the `Memory tags` line |
| `execute.md` | `execute` | the steps, the boundaries, the validation commands, and the Handoff with its evidence |
| `review.md` | `review` | the hard gates, the checklist, the verdict |
| `rescue.md` | `rescue` | what was attempted, what failed, which assumption to test next |
| `node.md` | the parent node (or the root controller) | one work-tree node's identity, goal, owned paths, budget, inherited limits and plan pointers; read by a sub-orchestrator or leaf |
| `handoff.md` | the node itself, on close | the full node handoff at `<state_dir>/nodes/<root_id>/<node_id>.md`; its Summary is the text `node_close` passes up |

## Where filled copies go

Through stdin or a temp file, per `.nucleos/workflow/dispatch.md` — never by writing into this directory. Medium and large tasks MAY additionally persist a plan as a new `<state_dir>/plans/<YYYY-MM-DD>-<slug>.md`; new file only, never an overwrite. `<state_dir>` is read from `.nucleos/workflow/project.json` and lives outside the working tree, so none of it can enter git.

## What judges a filled packet

Two scripts, from `<bundle_dir>/scripts/`, and a model whose verdict is advisory.

**`evidence_gate.py`** reads a filled `execute.md` and refuses a Handoff whose validation evidence is incomplete. It can refuse and it can never approve. Every command listed under `Validation.Commands` needs a block under `Validation evidence:` carrying both an `exit:` line and a `tail:`. It does **not** require the exit to be zero — an honest red leg passes, a hidden one is what the gate catches. See `examples/execute-filled.md`, which ends on a red leg for exactly that reason.

**ORDER IS LOAD-BEARING in `execute.md`.** Every Handoff field must stay INSIDE the `## Handoff` section. The gate reads that section as the lines from its heading up to the next `## ` heading, so a section opening between the heading and `Validation evidence:` puts the evidence outside it and the gate exits 2 — `malformed input: missing Handoff Validation evidence:` — before judging a single command. That is why `## Follow-ups` comes last.

**`review_gate.py`** reads a filled `review.md` and refuses a packet that contradicts itself. Same promise as its sibling: it can refuse and it can never approve, so exit 0 means nothing was deterministically wrong and NOT that the review was good — the checklist above the verdict stays a model's judgement, per `workflow.md` Rule 6. Three contradictions:

- **An approval standing on its own findings.** `Verdict: approve` beside an unchecked hard gate, a named `Potential regressions:` or a non-`none` `Missing tests:` is not an approval. The same findings under `request-changes` or `escalate` pass untouched — the packet is then doing its job, and a gate that refused those would teach a reviewer to delete findings instead of changing the verdict.
- **A review that never says what it could not see.** `Limitations:` is required and may be `none`, but it has to be answered. A review that ran out of context or never opened half the diff ends in the same word as a thorough one, and this is the only place that shows.
- **A plan that moved after approval.** `Plan sha256:` pins the raw bytes of the persisted `<state_dir>/plans/<date>-<slug>.md`; the gate re-derives them. Both it and `Plan path:` may be `none`, which is the ordinary small task with no file to pin, and then the gate says nothing. Distinct from the `prompt_sha256` in a metric row, which pins a DISPATCHED PROMPT after the fact: that is an audit of what was sent; this is a precondition of what is being judged.

Both gates tell an unwritten packet from a damaged one: a field still carrying the schema's own HTML comment exits 1 as "not yet authored", never 2. Neither ever exits 0 on it — an unwritten packet has proven nothing.

**The test commands** inside a packet are only the targeted tests the executor writes or changes, and the executor does not run them. Never plan a project-wide selection or a full suite into a packet: the controller runs `mcp__nucleos__verify` exactly once per task, over the whole branch diff. `workflow.md` Rule 3 is the authority.

## The worked example

`examples/execute-filled.md` is a filled `execute.md` kept as the quality anchor — the schema says which fields exist, the example says what an acceptable value in each one reads like. Its evidence blocks are pasted output, on purpose: a reference packet carrying invented output teaches an executor to invent output, which is the one thing `evidence_gate.py` exists to prevent (see the note in that file about its one illustrative block).
