# Execution Packet

<!-- READ-ONLY TEMPLATE. See packets/README.md for the contract. -->

Task ID: <!-- must match the planning packet -->
Objective: <!-- one sentence -->
Size: <!-- trivial | small | medium | large -->

## Steps
<!-- ordered atomic steps, max 7 -->

## File Context
<!-- minimum snippets needed, formatted as file:line - snippet -->

## Boundaries
Allowed files: <!-- only these files may be modified -->
Do-not-touch files/dirs: <!-- these must not be changed under any circumstance -->

## Tests
To add: <!-- same as planning packet `Tests to add` after normalization (trim/lowercase; none/-/empty equivalent) — not byte-identical -->

## Validation
Commands: <!-- exact commands to run after implementation.
  Executors run NO tests (workflow.md Rule 3, batched mode): list the targeted tests you
  wrote or changed and mark each `deferred: batched`; the controller runs them once for
  every packet together and the single final verify decides ship/no-ship. Non-test checks
  (a `git` query, a file-existence check) may be run and evidenced normally.
  Never plan a project-wide or full-suite command into a packet. -->
Expected result: <!-- what success looks like -->

## Handoff

<!-- executor fills this section after completing the work.
  ORDER IS LOAD-BEARING: every field below must stay INSIDE `## Handoff`.
  `<bundle_dir>/scripts/evidence_gate.py` reads the Handoff section as the lines from this
  heading up to the NEXT `## ` heading, so a section opening between the heading and
  `Validation evidence:` puts the evidence outside it and the gate exits 2 `malformed input:
  missing Handoff Validation evidence:` before it judges a single command. -->
Files changed:
Tests added:
<!-- mirror Tests.To add; planned tests are added or skipped with reason -->
Validation evidence:
<!-- one block per command: `$ <command>`, `exit: <code>`, `tail: <last 5 lines of output, or full output if shorter>` -->
Deviations from plan: <!-- none, or list each deviation with reason -->
New risks discovered: <!-- none, or list each -->
Memory updates: <!-- durable facts learned, or `none` -->
Pending deletions: <!-- required deletions not executed, or `none` -->

## Follow-ups
<!-- optional bullets feeding the TODO scanner; one per line -->
