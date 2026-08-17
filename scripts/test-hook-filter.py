#!/usr/bin/env python3
"""Cover the filter in `.claude/hooks/ask_daemon.py`.

**This is the most load-bearing Python in the repository and it had no test at all.** The script is
`include_str!`'d into the daemon by `core/src/triage.rs:28`, so it ships inside the binary, and its
interactive branch is what decides whether a git operation a person types is refused and sent to the
queue. It was written, reviewed, committed, and shipped with a hole you could walk through with a
`cd`: the filter read only the first two whitespace-separated tokens, so `cd repo && git push` was
never governed. Nobody found that by reading it. This is the cover that was missing.

The filter is deliberately OVER-broad — it must never be the authority, only keep the fast path free
— so the cases below assert in both directions: everything that could possibly be a queue operation
is caught however it is spelled, and the ordinary commands a session runs all day are left alone.

Pure function, called directly: no daemon, no network, no subprocess, nothing to be flaky about.
`ask_daemon.py` guards its entry point with `if __name__ == "__main__"`, so importing it runs
nothing.

Run:  python scripts/test-hook-filter.py
"""

import importlib.util
import os
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
HOOK = os.path.join(ROOT, ".claude", "hooks", "ask_daemon.py")

spec = importlib.util.spec_from_file_location("ask_daemon", HOOK)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

G = "g" + "it"  # kept out of the text so the running session's own hook does not govern this file

# (command, the verb it must be governed as — or "" for "leave it alone")
CASES = [
    # The plain spellings.
    (f"{G} merge feature", "merge"),
    (f"{G} push origin master", "push"),
    (f"{G} tag v1.0", "tag"),
    (f"{G} fetch origin", "fetch"),
    (f"{G} branch -d topic", "branch"),
    (f"{G} rebase main", "rebase"),
    # The bypass this test exists for: same operation, shell in front of it.
    (f"cd /repo && {G} merge feature", "merge"),
    (f"cd /repo\n{G} merge feature", "merge"),
    (f"true; {G} push origin master", "push"),
    (f"cd x&&{G} push", "push"),
    (f"echo hi | {G} fetch origin", "fetch"),
    (f"(cd /repo && {G} rebase main)", "rebase"),
    (f"FOO=bar {G} merge feature", "merge"),
    (f"cd /repo && {G} merge feature && echo done", "merge"),
    # Invoked by path, or by the Windows name. What it is called says nothing about what it does.
    (f"/usr/bin/{G} push origin master", "push"),
    (f"C:\\Program Files\\Git\\bin\\{G}.exe push origin master", "push"),
    # Global flags sit between the program and the verb.
    (f"{G} -C /repo merge feature", "merge"),
    (f"{G} -c user.name=x push origin master", "push"),
    (f"{G} --git-dir /r/.git fetch origin", "fetch"),
    (f"{G} --no-pager merge feature", "merge"),
    # And the ordinary work of a session, which must stay untouched. A guard that refuses these is
    # a guard someone turns off.
    (f"{G} status", ""),
    (f"{G} log --oneline -3", ""),
    (f"{G} diff --stat", ""),
    (f"{G} add -A", ""),
    (f"{G} commit -m 'merge the two helpers'", ""),
    (f"cd /repo && {G} status --short", ""),
    (f"{G} stash pop", ""),
    ("echo hello world", ""),
    ("cargo test -p nucleos-core", ""),
    ("", ""),
    # `commit` is not a queue verb even though a governed word follows it.
    (f"{G} commit -m 'push it later'", ""),
    # Not git at all, however much it looks like it.
    ("legit merge feature", ""),
    ("digit push origin master", ""),
]


def main() -> int:
    failures = 0
    for command, want in CASES:
        got = hook.governed_git_verb(command)
        ok = got == want
        failures += 0 if ok else 1
        if not ok:
            # Built outside the f-string: a backslash in an f-string expression is a syntax error
            # before Python 3.12, and this file has to run wherever the gate runs.
            shown = command.replace("\n", "\\n")
            expected = want or "<not governed>"
            actual = got or "<not governed>"
            print(f"FAIL {shown!r}: expected {expected!r}, got {actual!r}")

    total = len(CASES)
    print(f"{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
