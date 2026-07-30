# Splice a function slice into a Rust file's `mod tests`, replacing whatever
# function of the same name is already there.
#
#   awk -f rust-slice.awk -f graft-test.awk -v name=<fn> -v fragment=<file> <candidate.rs>
#
# The delete-then-insert is the load-bearing half. A candidate tree that already
# contains a test by this name -- because the agent wrote its own, or because it
# is the reference tree itself -- must still be judged by the reference's
# version, so the candidate's copy is removed before the fragment goes in.
#
# The fragment is appended at the end of the block rather than put back where
# the old one was: position has no effect on what runs, and "last thing before
# the closing brace" is one rule instead of two.
#
# Output is LF-terminated; score.sh converts it to the candidate file's own line
# endings afterwards.
#
# Exits 2 on bad usage, 3 if there is no `mod tests`, 4 if its closing brace is
# missing, and 5 if a same-named function is present but cannot be sliced.

{ sub(/\r$/, ""); line[NR] = $0 }

END {
  if (name == "" || fragment == "") {
    print "graft-test.awk: -v name=<fn> and -v fragment=<file> are required" > "/dev/stderr"
    exit 2
  }

  # The LAST `mod tests {`: a file can carry other `#[cfg(test)]` items above it
  # (worktree.rs does), and the test module is conventionally the file's tail.
  modline = 0
  for (i = 1; i <= NR; i++) {
    if (line[i] ~ /^[ \t]*(pub([ \t]*\([^)]*\))?[ \t]+)?mod[ \t]+tests[ \t]*\{[ \t]*$/) modline = i
  }
  if (!modline) {
    print "graft-test.awk: no `mod tests {` in the candidate file" > "/dev/stderr"
    exit 3
  }
  match(line[modline], /^[ \t]*/)
  modindent = substr(line[modline], 1, RLENGTH)

  modclose = 0
  for (i = modline + 1; i <= NR; i++) {
    if (line[i] == modindent "}") { modclose = i; break }
  }
  if (!modclose) {
    print "graft-test.awk: `mod tests` has no closing brace at its own indentation" > "/dev/stderr"
    exit 4
  }

  # A same-named function in the candidate, if any, is dropped.
  code = rs_find_fn(name, at)
  if (code == 0) {
    drop_start = at["start"]; drop_stop = at["stop"]
  } else if (code == 3) {
    drop_start = 0; drop_stop = -1
  } else {
    print "graft-test.awk: " rs_explain(code, name) > "/dev/stderr"
    exit code
  }

  last = "x"
  for (i = 1; i <= NR; i++) {
    if (i >= drop_start && i <= drop_stop) continue
    if (i == modclose) {
      if (last != "") print ""
      while ((getline f < fragment) > 0) { sub(/\r$/, "", f); print f }
      close(fragment)
    }
    print line[i]
    last = line[i]
  }
}
