How to write the `## Commands to run` section: read this before handing back test, build, lint or format commands for the controller to run.

# Stating commands

You never run tests, builds or checks. The controller runs what you list, so each bullet must be runnable as written, with no interpretation.

## Finding the right command
Look in the project's own instructions before guessing: CLAUDE.md, AGENTS.md, CONTRIBUTING, README, `package.json` scripts, Makefile/justfile, CI config (`.github/workflows`). Prefer what they name over anything you would infer from the language. Copy it verbatim; do not paraphrase, reorder flags or "simplify".

## Order
1. The narrowest command that covers your change: a single test, test file, package, crate or module filter.
2. Then the project's broader gate, only if the instructions name one.

## Each bullet
- One command per bullet, in backticks. No chained one-liners (`a && b; c`): they fail in other shells and hide which step broke.
- Include the working directory if it is not the repo root, and any env or PATH setup the instructions mention, as part of the note after the command.
- Say what the command covers: which new or changed tests it exercises. If you added a test, name it.
- If the project forbids something in a given shell (for example "run only from PowerShell"), carry that into the note.
- Never claim a result. Do not write "tests pass", "should pass" or "verified". You did not run it.

## Nothing to run
If the change is docs or comments only, write `none`.

## No command found
Say so under `## Risks`, then give the most likely command and label it explicitly as a guess.

## Examples

Good:
- `cargo test -p billing parse_invoice_rounds_half_up` (repo root; covers the new test `parse_invoice_rounds_half_up` in `billing/src/parse.rs`)
- `npm test -- src/pages/Cart.test.tsx` (run in `web/`; covers the two changed cases in Cart.test.tsx)
- `make check` (broader gate named in the project's CONTRIBUTING; run from the repo root)

Bad:
- `cargo test` (too broad as a first step, no scope, no note on coverage)
- `cd shell && npm install && npm test && npm run lint` (chained, installs dependencies, three steps in one bullet)
- `pytest tests/` - all tests pass with my change (claims a result you never obtained)

Bad, guess not labelled: a bullet `make check` when no Makefile exists. Correct form: under Risks, "No test command found in the repo docs; guess: `pytest tests/test_cart.py`".
