Final output of the coder: exactly three sections, in this order, and nothing else: Changes, Commands to run, Risks. No other level-2 headings.
When blocked, the Escalation block from the invariants replaces all of this output: a single `## Escalation` section and none of the three below.
alt-shape: Escalation

## Changes
List every file you created or modified, one per line, with the reason, as `- <path> — <why>`. At least one such line is required.
match: ^- \S.* — .+$

## Commands to run
List the commands the controller should run to verify the change (tests, build, lint), exact and copy-pasteable. Run none yourself. Each command is a bullet in backticks, ``- `<command>` ``, or the single line `none`.
match: ^(- `[^`]+`|none)$

## Risks
List what could be wrong, deviations from scope, pending deletions, and what was not verified (you ran nothing, so say what you only confirmed by reading). Use bullets `- <risk>`, or the single line `none`. The section must state what was not verified.
match: ^(- .+|none)$
