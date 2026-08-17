#!/usr/bin/env bash
# Prove a task is scoreable, by scoring the two trees whose verdicts are known.
#
#   scripts/eval/verify-task.sh [--task ID]... [--work-dir PATH] [--tasks-file F]
#
# With no --task, every row in the table.
#
# WHY THIS EXISTS
#
# The task set has now twice contained rows that could not measure anything, and
# both times the defect was invisible to reading:
#
#   * T1's held-out test could not fail. It capped the budget at 0.01 against a
#     0.5 per-run reserve, so the gate shut on the reserve alone and the 5.0 of
#     seeded spend never entered the arithmetic. It passed identically with
#     `email_triage` in the budget filter and with it removed -- the one
#     distinction it existed to draw.
#
#   * T1 and T3 were chosen by reading commit subjects. As diffs they are 1349
#     and 840 lines, so `reference~1` was not the task the row described.
#
# Neither survives being run. So: run them.
#
# THE FOUR CHECKS
#
#   1. The base COMPILES. An agent handed a tree that does not build is doing a
#      different task, and every layer of an ablation fails it identically -- as
#      the 2026-07-30 run showed at a cost of $1.31 for zero information.
#
#   2. The held-out test FAILS on the base (score.sh exit 1). If it passes, the
#      bug is not in the base, or the test cannot see it. Either way the task
#      scores every candidate "solved", including one that changed nothing.
#
#   3. The held-out test PASSES on the reference (score.sh exit 0). If it fails,
#      the test does not describe the fix, and the task scores every candidate
#      "not solved", including a correct one.
#
#   4. The project's own gate is GREEN on the base. Only H3 runs a gate, so this
#      check is younger than the rest and its absence was invisible: the other
#      three layers never ask the base this question. Measured 2026-08-17, T1's
#      base failed `core: fmt` on one `assert!` that bases/T1.patch left across
#      three lines where rustfmt wants one. The 649 tests were green; only the
#      gate saw it. H3 launched on that base opens red and measures the agent
#      tidying somebody else's formatting.
#
# Check 2 is the one that catches a vacuous test, and it is the one nobody runs
# by accident. Checks 2 and 3 together are the same mutation argument used on
# the test itself, applied to the whole task: flip the thing under test, and
# require the verdict to flip with it.
#
# EXIT CODES
#
#   0  every task checked is valid
#   1  at least one task failed a check -- it must not appear in a results table
#   3  harness error
#
# ENVIRONMENT
#
# Same as score.sh: cargo on PATH, and CARGO_TARGET_DIR set to a shared target
# directory. Expect two full crate rebuilds per task -- the graft rewrites a
# source file on purpose, which is what makes cargo rebuild rather than run the
# binary from the other tree.

set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

die() { printf 'verify-task.sh: %s\n' "$*" >&2; exit 3; }

tasks=()
work_dir=""
tasks_file="$script_dir/tasks.tsv"

while [ $# -gt 0 ]; do
  case "$1" in
    --task)       tasks+=("${2:-}"); shift 2 ;;
    --work-dir)   work_dir="${2:-}"; shift 2 ;;
    --tasks-file) tasks_file="${2:-}"; shift 2 ;;
    -h|--help)    awk 'NR > 1 { if (/^#/) { sub(/^# ?/, ""); print; next } exit }' "${BASH_SOURCE[0]}"; exit 0 ;;
    *)            die "unknown argument: $1 (try --help)" ;;
  esac
done

[ -f "$tasks_file" ] || die "no task table at $tasks_file"
command -v cargo >/dev/null 2>&1 || die "cargo is not on PATH"

repo="$(git -C "$script_dir" rev-parse --show-toplevel)" || die "not inside a git repository"

if [ "${#tasks[@]}" -eq 0 ]; then
  while IFS= read -r id; do tasks+=("$id"); done < <(
    awk -F'\t' '{ sub(/\r$/, "") } /^[ \t]*#/ { next } /^[ \t]*$/ { next } { print $1 }' "$tasks_file"
  )
fi
[ "${#tasks[@]}" -gt 0 ] || die "no tasks to verify"

if [ -z "$work_dir" ]; then
  work_dir="$(cd "$repo/.." && pwd)/nucleos-eval-verify"
fi
case "$work_dir" in
  *\ *) die "work dir contains a space ($work_dir) — the worktree tests assert a space-free checkout" ;;
esac
mkdir -p "$work_dir" || die "cannot create $work_dir"

if [ -z "${CARGO_TARGET_DIR:-}" ]; then
  echo "verify-task.sh: warning: CARGO_TARGET_DIR is unset — each tree will build its own ~15 GB target dir" >&2
fi

# Named, not inherited from PATH. On this machine `bash` is C:\Windows\System32\bash.exe — WSL, a
# different operating system with none of this toolchain in it — so a bare `bash scripts/gates.sh`
# would fail for reasons that look like the gate's. Same value `layer.py` gives H3's gate_command.
git_bash="${GIT_BASH:-C:/Program Files/Git/bin/bash.exe}"
[ -x "$git_bash" ] || die "no Git bash at $git_bash (override with GIT_BASH=...); check 4 needs it"

reference_of() {
  awk -F'\t' -v id="$1" '
    { sub(/\r$/, "") } /^[ \t]*#/ { next } /^[ \t]*$/ { next }
    $1 == id { print $2; exit }
  ' "$tasks_file"
}

results=()
overall=0

for task in "${tasks[@]}"; do
  echo
  echo "=============================================================================="
  echo " verifying $task"
  echo "=============================================================================="

  reference="$(reference_of "$task")"
  [ -n "$reference" ] || die "unknown task '$task'"

  base_tree="$work_dir/$task/base"
  ref_tree="$work_dir/$task/reference"
  log_dir="$work_dir/$task"
  mkdir -p "$log_dir"

  # ------------------------------------------------------------------ lay out
  base_line="$(bash "$script_dir/base.sh" --task "$task" --dest "$base_tree" --tasks-file "$tasks_file" --repo "$repo")"
  base_status=$?
  if [ $base_status -ne 0 ]; then
    results+=("$task	BASE-UNBUILDABLE	could not lay out the base tree")
    overall=1
    continue
  fi
  kind="$(printf '%s' "$base_line" | cut -f2)"
  echo "verify-task.sh: base is $kind, from $reference"

  bash "$script_dir/materialize.sh" "$reference" "$ref_tree" --repo "$repo" >/dev/null \
    || { results+=("$task	HARNESS	could not materialize the reference"); overall=1; continue; }

  # ------------------------------------------------------- 1. the base compiles
  echo "verify-task.sh: [1/4] does the base compile?"
  ( cd "$base_tree" && cargo check --all-targets ) > "$log_dir/base-check.log" 2>&1
  if [ $? -ne 0 ]; then
    echo "verify-task.sh: NO. The base does not build; an agent handed it is doing a different task."
    grep -E '^error(\[E[0-9]+\])?:' "$log_dir/base-check.log" | head -n 10
    results+=("$task	BASE-DOES-NOT-COMPILE	$log_dir/base-check.log")
    overall=1
    continue
  fi
  echo "verify-task.sh: yes."

  # ------------------------------------ 2. the held-out test fails on the base
  echo "verify-task.sh: [2/4] does the held-out test fail on the base?"
  bash "$script_dir/score.sh" --task "$task" --tree "$base_tree" \
    --tasks-file "$tasks_file" --ref-repo "$repo" \
    --work-dir "$log_dir/score-base" > "$log_dir/score-base.log" 2>&1
  base_verdict=$?
  case $base_verdict in
    1) echo "verify-task.sh: yes — not-solved, as a base must be." ;;
    0) echo "verify-task.sh: NO. The test PASSES on the base: it cannot see this bug, so every"
       echo "verify-task.sh: candidate scores solved, including one that changed nothing."
       results+=("$task	VACUOUS-TEST	the held-out test passes on the base")
       overall=1; continue ;;
    2) echo "verify-task.sh: NO. The base plus the graft does not compile — usually a helper the"
       echo "verify-task.sh: test needs was deleted along with the test. See $log_dir/score-base.log"
       results+=("$task	GRAFT-DOES-NOT-COMPILE	$log_dir/score-base.log")
       overall=1; continue ;;
    *) results+=("$task	HARNESS	score.sh exit $base_verdict on the base"); overall=1; continue ;;
  esac

  # --------------------------------- 3. the held-out test passes on the reference
  echo "verify-task.sh: [3/4] does the held-out test pass on the reference?"
  bash "$script_dir/score.sh" --task "$task" --tree "$ref_tree" \
    --tasks-file "$tasks_file" --ref-repo "$repo" \
    --work-dir "$log_dir/score-ref" > "$log_dir/score-ref.log" 2>&1
  ref_verdict=$?
  case $ref_verdict in
    0) echo "verify-task.sh: yes — solved." ;;
    1) echo "verify-task.sh: NO. The test FAILS on the reference, so it does not describe the fix"
       echo "verify-task.sh: and would score a correct candidate not-solved."
       results+=("$task	TEST-FAILS-ON-FIX	$log_dir/score-ref.log")
       overall=1; continue ;;
    *) results+=("$task	HARNESS	score.sh exit $ref_verdict on the reference"); overall=1; continue ;;
  esac

  # ------------------------------------ 4. the project's own gate is green on the base
  #
  # Only H3 runs a gate, which is why this check is younger than the other three and why its
  # absence stayed invisible: H0-H2 never ask the base this question. Measured 2026-08-17, T1's
  # base failed `core: fmt` — one `assert!` that `bases/T1.patch` left across three lines where
  # rustfmt wants one. Tests were green; only the gate saw it. An H3 launched on that base would
  # have opened with a red gate and measured the agent tidying somebody else's formatting.
  echo "verify-task.sh: [4/4] is the project's own gate green on the base?"
  ( cd "$base_tree" && "$git_bash" scripts/gates.sh core ) > "$log_dir/base-gate.log" 2>&1
  if [ $? -ne 0 ]; then
    echo "verify-task.sh: NO. H3 would start red, and its verdict would be about the base."
    grep -E '^gates FAILED|^  [a-z]+: ' "$log_dir/base-gate.log" | head -n 10
    results+=("$task	GATE-RED-ON-BASE	$log_dir/base-gate.log")
    overall=1
    continue
  fi
  echo "verify-task.sh: yes."

  results+=("$task	VALID	base=$kind")
done

echo
echo "=============================================================================="
echo " verify-task.sh"
echo "=============================================================================="
printf '%s\n' "${results[@]}" | column -t -s "$(printf '\t')" 2>/dev/null \
  || printf '%s\n' "${results[@]}"
echo
if [ $overall -eq 0 ]; then
  echo "every task verified: base compiles, test fails on base, test passes on reference,"
  echo "and the project's own gate is green on the base."
else
  echo "at least one task is NOT valid. A row that fails here must not appear in a"
  echo "results table — it produces verdicts that mean nothing."
fi
echo "logs: $work_dir"
exit $overall
