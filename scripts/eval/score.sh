#!/usr/bin/env bash
# Decide whether a candidate tree solved an eval task, by grafting the reference
# commit's test onto the candidate and running only that test.
#
# WHY THIS EXISTS
#
# `.ai/eval/`'s protocol counted a task solved when the project's own gate
# (`scripts/gates.sh core`) passed on the tree the agent produced. Every task in
# the set is a commit that adds a fix AND the test that catches it -- so the tree
# one commit earlier is already green: the bug is present and nothing looks for
# it. Under that protocol an agent that changed nothing scored "solved", and
# every ablation number built on it would have measured noise. Hence: run the
# reference's test, not the candidate's suite.
#
# WHAT IT GUARANTEES
#
#   1. The test comes from the reference commit, always. It is read with
#      `git show <reference>:<file>` out of a repository that has that commit,
#      and any function of the same name already in the candidate is deleted
#      before the reference version is spliced in. An agent that writes its own
#      passing test gains nothing -- the same hole as a gate script the agent
#      can edit.
#
#   2. A grafted tree that does not compile is INCONCLUSIVE (exit 2), never
#      "not solved". A candidate that fixed the bug behind a different signature
#      will not build against the reference's test; that is a case for a human
#      to read. Rounding it down to a failure would bias every number the
#      ablation ever produces, in the direction of understating agents that
#      refactored.
#
#   3. The candidate tree is never written to. It is copied into a work
#      directory and the graft happens there; the script checksums every file
#      under the candidate's crate before and after and aborts if one moved. A
#      scorer that damages what it measures cannot be run twice.
#
# USAGE
#
#   scripts/eval/score.sh --task T2 --tree <path> [options]
#
#     --task ID          a row in scripts/eval/tasks.tsv
#     --tree PATH        the candidate: a repo-root-shaped directory to judge
#     --work-dir PATH    where the graft is built (default: alongside the repo)
#     --tasks-file PATH  override the task table
#     --ref-repo PATH    a repo containing the reference commit
#                        (default: the repo this script lives in)
#     -h, --help
#
# It spends exactly one `cargo test` invocation per call, and builds nothing
# else. Two calls on the same unchanged candidate cost one compile, because the
# copy preserves mtimes.
#
# EXIT CODES
#
#   0  solved        -- the reference test ran and passed
#   1  not solved    -- the reference test ran and failed
#   2  inconclusive  -- the grafted tree does not compile; read it yourself
#   3  harness error -- the scorer could not reach a verdict at all
#
# 3 is deliberately distinct from 2: "the scorer is broken" and "the candidate
# needs a human" are different problems and must not share a bucket either.
#
# ENVIRONMENT
#
# `cargo` must be on PATH. On this machine that means, before calling:
#
#   export RUSTUP_HOME=C:/Projects/rustup CARGO_HOME=C:/Projects/cargo
#   export PATH="/c/Projects/mingw64/bin:/c/Projects/cargo/bin:$PATH"
#   export CARGO_TARGET_DIR=C:/Projects/nucleos/target
#
# CARGO_TARGET_DIR is not optional in practice: without it cargo builds a fresh
# ~15 GB target directory per work directory. The script warns if it is unset.

set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

task=""
tree=""
work_dir=""
tasks_file="$script_dir/tasks.tsv"
ref_repo=""

# 3 == harness error. Everything that is not a verdict leaves through here.
die() { printf 'score.sh: %s\n' "$*" >&2; exit 3; }

usage() {
  awk 'NR > 1 { if (/^#/) { sub(/^# ?/, ""); print; next } exit }' "${BASH_SOURCE[0]}"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --task)       task="${2:-}"; shift 2 ;;
    --tree)       tree="${2:-}"; shift 2 ;;
    --work-dir)   work_dir="${2:-}"; shift 2 ;;
    --tasks-file) tasks_file="${2:-}"; shift 2 ;;
    --ref-repo)   ref_repo="${2:-}"; shift 2 ;;
    -h|--help)    usage; exit 0 ;;
    *)            die "unknown argument: $1 (try --help)" ;;
  esac
done

[ -n "$task" ] || die "--task is required"
[ -n "$tree" ] || die "--tree is required"
[ -d "$tree" ] || die "--tree is not a directory: $tree"
[ -f "$tasks_file" ] || die "no task table at $tasks_file"

tree="$(cd "$tree" && pwd)"

if [ -z "$ref_repo" ]; then
  ref_repo="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null)" \
    || die "this script is not inside a git repository; pass --ref-repo"
fi
[ -d "$ref_repo" ] || die "--ref-repo is not a directory: $ref_repo"

command -v git >/dev/null 2>&1 || die "git is not on PATH"
command -v cargo >/dev/null 2>&1 || die "cargo is not on PATH (see ENVIRONMENT in --help)"
command -v awk >/dev/null 2>&1 || die "awk is not on PATH"

if [ -z "${CARGO_TARGET_DIR:-}" ]; then
  echo "score.sh: warning: CARGO_TARGET_DIR is unset — cargo will build a target dir inside the work dir (~15 GB)" >&2
fi

# ---------------------------------------------------------------- task lookup

# Tab-separated, `#` comments, tolerant of a stray CR if the table was checked
# out with CRLF despite scripts/eval/.gitattributes.
row="$(awk -F'\t' -v id="$task" '
  { sub(/\r$/, "") }
  /^[ \t]*#/ { next }
  /^[ \t]*$/ { next }
  $1 == id { print; found = 1; exit }
  END { if (!found) exit 1 }
' "$tasks_file")" || die "unknown task '$task' — known ids: $(awk -F'\t' '!/^[ \t]*#/ && NF { printf "%s ", $1 }' "$tasks_file")"

reference="$(printf '%s' "$row" | cut -f2)"
test_path="$(printf '%s' "$row" | cut -f3)"
test_file="$(printf '%s' "$row" | cut -f4)"
[ -n "$reference" ] && [ -n "$test_path" ] && [ -n "$test_file" ] \
  || die "task '$task' row is malformed (expected 4 tab-separated fields): $row"

# `worktree::tests::foo` -> `foo`
fn_name="${test_path##*::}"

# The cargo target is derived from the file's location rather than carried in
# the table, so adding a task stays a one-line change. Unit tests in the daemon
# binary's own source are the only shape the set has today.
case "$test_file" in
  core/src/*)   package="nucleos-core"; cargo_target=(--bin nucleos-core) ;;
  core/tests/*) package="nucleos-core"
                base="${test_file##*/}"; cargo_target=(--test "${base%.rs}") ;;
  *)            die "no cargo target rule for '$test_file' — add one to score.sh" ;;
esac

reference_sha="$(git -C "$ref_repo" rev-parse --verify --quiet "${reference}^{commit}")" \
  || die "reference commit '$reference' is not in $ref_repo"

# ------------------------------------------------------------------ work area

if [ -z "$work_dir" ]; then
  # Outside the repository on purpose: the scorer may not add gitignore entries,
  # and a work tree inside the repo would show up in every `git status` the
  # thing under measurement runs.
  work_dir="$(cd "$ref_repo/.." && pwd)/nucleos-eval-work"
fi
run_dir="$work_dir/$task"
graft_tree="$run_dir/tree"

case "$run_dir" in
  *\ *) die "work dir contains a space ($run_dir) — the worktree tests assert a space-free checkout and would all fail; pass --work-dir" ;;
esac

mkdir -p "$run_dir" || die "cannot create $run_dir"
fragment="$run_dir/fragment.rs"
test_log="$run_dir/test.log"

echo "score.sh: task=$task reference=${reference_sha:0:7} test=$test_path"
echo "score.sh: candidate=$tree"
echo "score.sh: work=$run_dir"

# ------------------------------------------- extract the test, from the source

git -C "$ref_repo" show "$reference_sha:$test_file" \
  | awk -f "$script_dir/rust-slice.awk" -f "$script_dir/extract-test.awk" -v name="$fn_name" \
  > "$fragment"
extract_status=("${PIPESTATUS[@]}")
[ "${extract_status[0]}" -eq 0 ] || die "git show $reference_sha:$test_file failed"
[ "${extract_status[1]}" -eq 0 ] || die "could not extract $fn_name from $reference_sha:$test_file"
[ -s "$fragment" ] || die "extracted an empty fragment for $fn_name"

grep -qE "fn[[:space:]]+${fn_name}[[:space:]]*\(" "$fragment" \
  || die "extracted fragment does not define $fn_name — refusing to graft it"

echo "score.sh: extracted $fn_name ($(wc -l < "$fragment" | tr -d ' ') lines) from the reference"

# ------------------------------------------------- copy the candidate, untouched

candidate_file="$tree/$test_file"
[ -f "$candidate_file" ] || die "candidate has no $test_file"

# The tripwire for rule 3, over the whole crate rather than just the file the
# graft touches: the claim being made is that scoring leaves the candidate
# alone, and a check narrow enough to only cover the expected damage would not
# catch the unexpected kind. Build outputs are pruned — a candidate that carries
# its own target/ would otherwise make this the slowest step in the script.
crate_dir="$tree/${test_file%%/*}"
checksum_crate() {
  find "$crate_dir" \( -name target -o -name node_modules \) -prune -o -type f -print0 \
    | sort -z | xargs -0 sha256sum 2>/dev/null
}
before="$(checksum_crate)"

rm -rf "$graft_tree" || die "cannot clear $graft_tree"
mkdir -p "$graft_tree" || die "cannot create $graft_tree"
# tar rather than cp -r: it preserves mtimes, so cargo treats an unchanged
# candidate as fresh on a second run instead of rebuilding the crate.
( cd "$tree" && tar -cf - \
    --exclude=.git --exclude=target --exclude=node_modules --exclude=.ai . ) \
  | ( cd "$graft_tree" && tar -xf - )
copy_status=("${PIPESTATUS[@]}")
[ "${copy_status[0]}" -eq 0 ] && [ "${copy_status[1]}" -eq 0 ] \
  || die "copying the candidate into $graft_tree failed"
[ -f "$graft_tree/$test_file" ] || die "the copy is missing $test_file"

# ------------------------------------------------------------------- the graft

# Match the candidate's line endings rather than imposing LF: the blobs in this
# repo are LF but a checkout with core.autocrlf=true is CRLF on disk, and a file
# that is half one and half the other is a nuisance to read in a diff.
cr="$(tr -cd '\r' < "$candidate_file" | wc -c)"
lf="$(tr -cd '\n' < "$candidate_file" | wc -c)"

awk -f "$script_dir/rust-slice.awk" -f "$script_dir/graft-test.awk" \
    -v name="$fn_name" -v fragment="$fragment" "$graft_tree/$test_file" \
  > "$run_dir/grafted.tmp" \
  || die "grafting $fn_name into $test_file failed"

if [ "$cr" -gt 0 ] && [ $((cr * 2)) -gt "$lf" ]; then
  sed 's/$/\r/' "$run_dir/grafted.tmp" > "$graft_tree/$test_file"
else
  cp "$run_dir/grafted.tmp" "$graft_tree/$test_file"
fi
rm -f "$run_dir/grafted.tmp"

# LOAD-BEARING, and the reason the copy above may preserve mtimes at all.
#
# CARGO_TARGET_DIR is shared, and cargo's artifact hash for this package does
# not include the workspace path -- two candidate trees at two paths write to
# the same `nucleos_core-<hash>.exe` and share one fingerprint. Freshness then
# comes down to mtimes, and a tree laid out by `git archive` carries the
# reference commit's dates, which are older than any build. Cargo calls such a
# tree fresh and runs the binary built from the OTHER one. (Measured: a
# `scripts/gates.sh core` on 58e4864~1 ran a binary built from 58e4864 and
# reported green, having executed a test that tree does not contain.)
#
# One source file newer than the fingerprint forces the whole crate to rebuild,
# and the graft has just rewritten exactly that file. Keep it that way.
touch "$graft_tree/$test_file"

defs="$(grep -cE "fn[[:space:]]+${fn_name}[[:space:]]*\(" "$graft_tree/$test_file")"
[ "$defs" -eq 1 ] || die "grafted file defines $fn_name $defs times — expected exactly 1"

after="$(checksum_crate)"
if [ "$before" != "$after" ]; then
  die "the candidate tree changed during scoring — this is a scorer bug, not a result"
fi
echo "score.sh: grafted $fn_name; candidate tree verified unmodified"

# ----------------------------------------------------------------- run the test

# One cargo invocation, not a `--no-run` build followed by a run: the verdict
# line for THIS test is what separates "compiled and ran" from "did not
# compile", so a second invocation would buy nothing and double the budget a
# caller has to spend.
echo "score.sh: running $test_path"
( cd "$graft_tree" && cargo test -p "$package" "${cargo_target[@]}" -- --exact "$test_path" ) > "$test_log" 2>&1
cargo_status=$?
cargo_runs=1

# Order matters. A result line for this test proves the tree compiled AND that
# the graft is what ran, so it outranks every other reading of the output.
if grep -qF "test $test_path ... ok" "$test_log"; then
  verdict="solved"
elif grep -qF "test $test_path ... FAILED" "$test_log"; then
  verdict="not-solved"
elif grep -qiE 'os error 5|Acesso negado|Permission denied' "$test_log"; then
  # Something holding the output binary is a fault of this machine, not of the
  # candidate. Putting it in the "inconclusive" bucket would quietly mix
  # environment noise into the one verdict a human is asked to read.
  echo
  echo "score.sh: the link step was denied access to the output binary."
  echo "score.sh: something is holding it. Rename it, do not kill it:"
  echo "score.sh:   mv \"\${CARGO_TARGET_DIR}/debug/nucleos-core.exe\" \"\${CARGO_TARGET_DIR}/debug/nucleos-core.exe.inuse\""
  echo "score.sh: log: $test_log"
  exit 3
elif grep -qE '^error(\[E[0-9]+\])?:|could not compile' "$test_log"; then
  echo
  echo "=============================================================================="
  echo " $task: INCONCLUSIVE — the grafted tree does not compile."
  echo
  echo " This is NOT 'not solved'. The reference test never got to run, so this"
  echo " candidate has not been scored. The usual cause is a candidate that fixed"
  echo " the problem behind a different signature, which a human has to judge."
  echo "=============================================================================="
  echo
  echo "--- compiler errors ---"
  grep -E '^error(\[E[0-9]+\])?:' "$test_log" | head -n 20
  echo "--- full log: $test_log ---"
  echo
  echo "score.sh: verdict=inconclusive task=$task cargo_runs=$cargo_runs"
  exit 2
else
  # `cargo test -- --exact <name>` that matches nothing still exits 0. Reading a
  # silent zero as a pass is exactly how a scorer starts lying, so a run with no
  # result line for THIS test is a harness error, never a verdict.
  echo
  echo "score.sh: no result line for '$test_path' and no compiler error (cargo exit $cargo_status)."
  echo "score.sh: the graft went in but cargo did not run it. Read $test_log."
  echo
  tail -n 30 "$test_log"
  exit 3
fi

echo
echo "--- what cargo reported ---"
grep -E "^(running |test $test_path |test result:)" "$test_log" | sed 's/^/    /'

echo
echo "=============================================================================="
if [ "$verdict" = solved ]; then
  echo " $task: SOLVED — the reference test passes on this candidate."
else
  echo " $task: NOT SOLVED — the reference test runs and fails on this candidate."
  echo
  echo "--- why it failed ---"
  sed -n '/^failures:$/,$p' "$test_log" | head -n 20
fi
echo "=============================================================================="
echo
echo "score.sh: verdict=$verdict task=$task cargo_runs=$cargo_runs log=$test_log"

[ "$verdict" = solved ] && exit 0
exit 1
