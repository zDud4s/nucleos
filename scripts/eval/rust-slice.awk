# Shared slicing helpers for the eval scorer's two awk programs.
#
# Both callers read a Rust source file into the global array `line[1..NR]` with
# carriage returns already stripped, then ask for one function by name. Nothing
# here parses Rust. It leans on two properties of rustfmt's output, which this
# repo gates on (`cargo fmt --all -- --check`, scripts/gates.sh):
#
#   * a function signature begins its own line;
#   * the closing brace of a function indented N is a line that is exactly that
#     indentation followed by `}`.
#
# Where those are not enough the helpers refuse instead of guessing: a slice
# whose braces or brackets do not balance is reported as an error and never
# returned. Silently mis-slicing would graft broken Rust and be scored
# "inconclusive", which is the one verdict that must stay trustworthy.

function rs_braces(from, to,   i, s, n) {
  n = 0
  for (i = from; i <= to; i++) {
    s = line[i]; n += gsub(/\{/, "{", s)
    s = line[i]; n -= gsub(/\}/, "}", s)
  }
  return n
}

function rs_brackets(from, to,   i, s, n) {
  n = 0
  for (i = from; i <= to; i++) {
    s = line[i]; n += gsub(/\[/, "[", s)
    s = line[i]; n -= gsub(/\]/, "]", s)
  }
  return n
}

# Locate the function `name`, together with the attributes and comments directly
# above it. Fills out["start"] and out["stop"] (inclusive line numbers).
#
# Returns 0 on success, or:
#   3  no function by that name
#   4  no closing brace at the signature's own indentation
#   5  the slice does not balance
function rs_find_fn(name, out,   i, j, s, sig, target, indent, closing) {
  sig = "^[ \t]*(pub([ \t]*\\([^)]*\\))?[ \t]+)?(const[ \t]+)?(async[ \t]+)?(unsafe[ \t]+)?fn[ \t]+" name "[ \t]*[(<]"
  target = 0
  for (i = 1; i <= NR; i++) {
    if (line[i] ~ sig) { target = i; break }
  }
  if (!target) return 3

  match(line[target], /^[ \t]*/)
  indent = substr(line[target], 1, RLENGTH)

  # Walk back over the contiguous doc-comment / attribute block. The doc comment
  # explaining WHY a security test exists is the most useful thing in it, so the
  # graft carries it rather than the bare function.
  out["start"] = target
  while (out["start"] > 1) {
    s = line[out["start"] - 1]
    sub(/^[ \t]+/, "", s)
    if (s ~ /^(\/\/|#\[|#!\[)/) { out["start"]--; continue }
    break
  }
  # An attribute spread over several lines leaves that prefix bracket-negative
  # (its `]` was collected, its `#[` was not). Keep walking until it balances.
  while (out["start"] > 1 && rs_brackets(out["start"], target - 1) < 0) out["start"]--

  closing = indent "}"
  out["stop"] = 0
  for (j = target + 1; j <= NR; j++) {
    if (line[j] == closing) { out["stop"] = j; break }
  }
  if (!out["stop"]) return 4

  if (rs_braces(out["start"], out["stop"]) != 0) return 5
  if (rs_brackets(out["start"], out["stop"]) != 0) return 5
  return 0
}

function rs_explain(code, name) {
  if (code == 3) return "no function named " name
  if (code == 4) return "no closing brace at the indentation of " name " -- is the file rustfmt-clean?"
  if (code == 5) return "the slice for " name " does not balance -- refusing to guess where it ends"
  return "unknown slicing error " code " for " name
}
