# Print one named Rust function -- with its attributes and doc comment -- from
# stdin or the named file.
#
#   awk -f rust-slice.awk -f extract-test.awk -v name=<fn> <file>
#
# Only ever pointed at `git show <reference>:<file>`, never at a candidate tree.
# Output is LF-terminated regardless of what came in.
#
# Exits 2 on bad usage, and 3/4/5 for the slicing failures rust-slice.awk names.

{ sub(/\r$/, ""); line[NR] = $0 }

END {
  if (name == "") {
    print "extract-test.awk: -v name=<fn> is required" > "/dev/stderr"
    exit 2
  }
  if (NR == 0) {
    print "extract-test.awk: empty input" > "/dev/stderr"
    exit 2
  }

  code = rs_find_fn(name, at)
  if (code != 0) {
    print "extract-test.awk: " rs_explain(code, name) > "/dev/stderr"
    exit code
  }

  for (i = at["start"]; i <= at["stop"]; i++) print line[i]
}
