# The grafted-test scorer

`score.sh` decides whether a candidate tree solved an eval task. It is the
replacement for the rule `.ai/eval/` shipped with, and it exists because that
rule was wrong in a way that made every planned ablation meaningless.

## The bug it replaces

The old protocol counted a task solved when the project's own gate,
`scripts/gates.sh core`, passed on the tree the agent produced.

Every task in the set is a commit that adds a fix **and** the test that catches
the thing it fixes. So the tree one commit before the reference is green: the
bug is sitting there and nothing in the suite looks for it. Run the old protocol
against `58e4864~1` — a repository whose `worktree.rs` will execute a command
string named by whatever repository the daemon was pointed at — and it reports
success. An agent that did nothing at all scored "solved".

Measured, on this machine, on that exact tree:

| what was run on `58e4864~1` | result |
|---|---|
| `scripts/gates.sh core` (the old protocol) | **all gates green** — scores the task solved |
| `scripts/eval/score.sh --task T2` | **NOT SOLVED** — the reference test runs and fails |

That contrast is the whole reason this directory exists.

## How it decides

1. Look the task up in [`tasks.tsv`](tasks.tsv): `(reference commit, test path, test file)`.
2. Read the test **out of the reference commit** — `git show <reference>:<file>`,
   sliced by [`extract-test.awk`](extract-test.awk). Never out of the candidate.
3. Copy the candidate into a work directory.
4. In the copy, delete any function of that name the candidate already has, and
   splice the reference's version into `mod tests`
   ([`graft-test.awk`](graft-test.awk)).
5. Run exactly that one test:
   `cargo test -p nucleos-core --bin nucleos-core -- --exact <test path>`.

| exit | verdict | means |
|---|---|---|
| 0 | solved | the reference test ran and passed |
| 1 | not solved | the reference test ran and failed |
| 2 | inconclusive | the grafted tree does not compile — a human has to read it |
| 3 | harness error | the scorer could not reach a verdict at all |

### Three properties it is built to hold

**The test always comes from the reference.** A candidate's own copy of the test
is deleted before the reference's is grafted in, so an agent that writes a test
that passes gains nothing. This is the same hole as a gate script the agent can
edit, and it has to be closed the same way — by not reading anything the thing
under measurement wrote.

**Compile failure is inconclusive, not failure.** A candidate that fixed the
problem behind a different signature will not build against the reference's
test. That is a case for a human, and it gets its own exit code and a loud
banner. Silently rounding it down to "not solved" would bias every number the
ablation ever produces, in the direction of understating agents that refactored.

**The candidate is never written to.** The graft happens in a copy. The script
checksums every file in the candidate's crate before and after and aborts with a
harness error if one moved — a scorer that damages what it measures cannot be
run twice, and one that *might* damage it cannot be trusted the first time.

Exit 3 is deliberately separate from exit 2: "the scorer is broken" and "this
candidate needs a human" are different problems and must not share a bucket
either. Cases routed to 3 include a graft that went in but produced no test
result line (`cargo test -- --exact` that matches nothing still exits 0, and
reading that silent zero as a pass is exactly how a scorer starts lying), and a
link step denied access to the output binary because something is holding it.

## Running it

```sh
export RUSTUP_HOME=C:/Projects/rustup CARGO_HOME=C:/Projects/cargo
export PATH="/c/Projects/mingw64/bin:/c/Projects/cargo/bin:$PATH"
export CARGO_TARGET_DIR=C:/Projects/nucleos/target

scripts/eval/score.sh --task T2 --tree /path/to/candidate
```

`CARGO_TARGET_DIR` is not optional in practice — without it cargo builds a fresh
~15 GB target directory per work directory. The script warns when it is unset.

One `cargo test` invocation per call, and nothing else is built. The copy
preserves mtimes, so re-scoring an unchanged candidate does not recompile.

The work directory defaults to `../nucleos-eval-work` relative to the repo —
outside the repository on purpose, because the scorer may not add gitignore
entries and a work tree inside the repo would show up in every `git status` the
thing under measurement runs. Override with `--work-dir`. **It must not contain
a space:** the worktree tests resolve their tempdir from the process's current
directory and assert it is space-free, so a spaced path fails ~25 tests for
reasons that have nothing to do with the task. The script refuses one up front.

### A trap worth knowing about: stale artifacts across candidate trees

Cargo's artifact hash for `nucleos-core` does **not** include the workspace
path. Two candidate trees at two different directories therefore write to the
same `target/debug/deps/nucleos_core-<hash>.exe` and share one fingerprint, and
freshness comes down to source mtimes. A tree laid out by `git archive` carries
the *reference commit's* dates — older than any build that has already
happened — so cargo declares it fresh and runs the binary built from the other
tree.

This is not hypothetical. The first attempt at the gate measurement in the table
above ran `scripts/gates.sh core` on `58e4864~1` and got "all gates green" in
0.87s, having executed
`worktree::tests::create_does_not_run_a_command_the_target_repository_names` —
a test that does not appear anywhere in that tree's source. It was running the
binary a previous `score.sh` call had built from `58e4864`.

`score.sh` is not exposed to this, and the reason is worth stating because it is
easy to delete by accident: the graft rewrites the test file, giving it a
current mtime, and one source newer than the fingerprint rebuilds the whole
crate. There is an explicit `touch` after the graft to keep that true.

**Anything else you run against a materialized tree needs to force it
yourself:**

```sh
find <tree> -type f -exec touch {} +
```

The real fix is a per-tree `CARGO_TARGET_DIR`, which costs ~15 GB per tree and
is why this machine shares one.

### Reproducing a candidate from a commit

```sh
scripts/eval/materialize.sh 58e4864~1 C:/Projects/nucleos-eval-work/candidates/t2-before
```

`git archive`, not `git worktree add`: adding a worktree writes to shared `.git`
state, and the eval may well run while someone is working in the same clone. The
result is what an agent hands over anyway — a directory of files.

## Adding a task

One line in `tasks.tsv`:

```
T3	<commit>	<module>::tests::<test fn>	core/src/<file>.rs
```

The cargo target is derived from the file's path rather than carried in the
table (`core/src/*` → `--bin nucleos-core`, `core/tests/*.rs` → `--test <stem>`),
so nothing else needs editing. A file outside those two shapes makes `score.sh`
stop and ask for a rule instead of guessing one.

A task only belongs here if its reference commit adds the test **together with**
the fix. If the test already existed, the candidate's own suite would catch the
regression and the old protocol would have been fine.

## What the slicing does and does not do

The two awk programs do not parse Rust. They lean on two properties of rustfmt's
output, which this repo gates on (`cargo fmt --all -- --check`):

* a function signature begins its own line;
* the closing brace of a function indented N is a line that is exactly that
  indentation followed by `}`.

The reference side is always a committed blob, so it is always rustfmt-clean.
The candidate side may not be — but the only thing read from the candidate is
where its `mod tests` ends and whether it already has a function by that name.

Where those properties are not enough, the helpers refuse instead of guessing: a
slice whose braces or brackets do not balance is reported as an error, and the
run ends as a harness error rather than grafting broken Rust and calling the
result inconclusive. The balance check counts braces inside string literals too,
so a test containing an unbalanced `{` in a string would be rejected — loudly,
and by a scorer that then needs a fix, which is the right failure direction.

## Known limits

* **Inconclusive does not say whose fault it is.** A grafted tree that fails to
  compile might be a candidate with a different signature, or a candidate that
  was already broken before the graft. The scorer reports the compiler errors
  and stops; distinguishing the two costs a second build of the ungrafted copy,
  which is not worth spending on every run. Build the copy at
  `<work>/<task>/tree` yourself if you need to know.
* **One test per task.** A reference commit that adds several tests is not
  expressible in `tasks.tsv` today. Nothing in the design objects to a list; it
  has not been needed.
* **Grafted indentation is the reference's.** If a candidate nests `mod tests`
  at a different depth the spliced function keeps the reference's indentation.
  Rust does not care and the scorer does not run `cargo fmt`.
