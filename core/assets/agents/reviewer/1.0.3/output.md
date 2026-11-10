# Output format

Your entire reply is the two sections below, in this order: no preamble, no trailing text.

## Findings

- Heading `## Findings`, first.
- Every finding is one line: `- [high|medium|low] <file>:<line> — <text>`.
  - `<file>` is a repo-relative path, or `task` / `input` for input-level findings. It contains no spaces; use `task` or `input` if the path cannot be written without them.
  - `<line>` is a positive integer, or 0 when no line applies (document-level).
  - Then ` — ` (space, em dash, space), then one sentence stating the defect and its consequence.
  - One finding per line: no wrapping, no sub-bullets, no blank lines between.
- No findings: write exactly one line `- none`. This is the single permitted exception to the pattern, valid only together with Verdict `approved`.
- Order: high, then medium, then low.
match: ^- (\[(high|medium|low)\] \S+:\d+ — .+|none)$

## Verdict

- Heading `## Verdict`, second.
- Exactly one non-blank line under it, from the closed set `approved` | `changes`, nothing else on the line.
- Consistency rule: at least one high or medium finding means `changes`. Only low findings, or `- none`, means `approved`. `- none` with `changes`, or a high/medium finding with `approved`, is invalid.
- Missing input case: the single finding `- [high] input:0 — ...` and `changes`.
one-of: approved | changes

Example (illustrative; the fenced blocks are not part of the output):

With findings:

```
## Findings
- [high] core/src/team/launch.rs:323 — the launch path unwraps the spawn result, so a failed spawn panics the daemon instead of parking the run.
- [low] core/src/team/launch.rs:340 — the new helper has no test for the empty roster case, so a regression there would go unnoticed.

## Verdict
changes
```

No findings:

```
## Findings
- none

## Verdict
approved
```
