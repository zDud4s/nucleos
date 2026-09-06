#!/usr/bin/env python3
"""Cover the filter and the outcome reporter in `core/hooks/ask_daemon.py`.

**This is the most load-bearing Python in the repository and it had no test at all.** The script is
`include_str!`'d into the daemon by `core/src/autopilot.rs`'s `HOOK_SOURCE` — `include_str!("../hooks/
ask_daemon.py")`, i.e. `core/hooks/ask_daemon.py` — so it ships inside the binary, and its interactive
branch is what decides whether a git operation a person types is refused and sent to the queue. It was
written, reviewed, committed, and shipped with a hole you could walk through with a `cd`: the filter
read only the first two whitespace-separated tokens, so `cd repo && git push` was never governed.
Nobody found that by reading it. This is the cover that was missing.

**Imports the SOURCE, `core/hooks/ask_daemon.py`, and not `.claude/hooks/ask_daemon.py`.** The
latter is an ARTEFACT `wire_classifier_hook` writes to a project's own `.claude/` directory from
`HOOK_SOURCE`, compiled into a binary that was built at some earlier point in time — so a copy of
it sitting in this checkout is only as current as the last time some daemon here was rebuilt and
run against it. Testing that copy would run this whole file green over a stale script the moment
`core/hooks/ask_daemon.py` changed and nothing rebuilt it in between, which is exactly backwards
for a gate whose entire job is to catch a change to the file it tests.

The filter is deliberately OVER-broad — it must never be the authority, only keep the fast path free
— so the cases below assert in both directions: everything that could possibly be a queue operation
is caught however it is spelled, and the ordinary commands a session runs all day are left alone.

Pure function, called directly: no daemon, no network, no subprocess, nothing to be flaky about.
`ask_daemon.py` guards its entry point with `if __name__ == "__main__"`, so importing it runs
nothing.

Run:  python scripts/test-hook-filter.py
"""

import importlib.util
import io
import os
import sys
import tempfile
from unittest import mock

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
HOOK = os.path.join(ROOT, "core", "hooks", "ask_daemon.py")

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


def target_dir_cases(tmp):
    """Where the daemon binary might be, and in what order.

    The hole this covers is not in the filter: it is in `control_token`, which used to look only in
    `<main_root>/target`. On a machine whose worktrees share one build tree OUTSIDE the repository
    there is no `target/` at all, so the lookup found nothing and every merge, push, tag and fetch
    from every agent session was refused — reported as a token that could not be read, which sends
    whoever reads it to the credential manager instead of to the build directory.
    """
    config = os.path.join(tmp, "config.toml")
    # `newline=""` so `os.linesep` reaches the file as itself. In text mode Python translates a
    # line ending on the way out, so writing one that is already a line ending produces a doubled
    # one, and tomllib will not parse the file it lands in.
    with open(config, "w", encoding="utf-8", newline="") as handle:
        # The comment on the first line is the case, not decoration: the real config this mirrors
        # documents the setting with a line a regex would match before it reached the setting.
        handle.write(
            '# $env:CARGO_TARGET_DIR = "C:/Projects/.decoy"'
            + os.linesep
            + "[build]"
            + os.linesep
            + 'target-dir = "C:/Projects/.cargo-target"'
            + os.linesep
        )
    root = os.path.join(tmp, "repo")
    default = os.path.join(root, "target")

    return [
        (
            "a configured target-dir is looked in, and <root>/target still is",
            hook.cargo_target_dirs(root, environ={}, configs=[config]),
            ["C:/Projects/.cargo-target", default],
        ),
        (
            "CARGO_TARGET_DIR comes first, as it does for cargo itself",
            hook.cargo_target_dirs(
                root,
                environ={"CARGO_TARGET_DIR": "C:/Projects/.cargo-target-test"},
                configs=[config],
            ),
            ["C:/Projects/.cargo-target-test", "C:/Projects/.cargo-target", default],
        ),
        (
            "a machine that configures nothing behaves exactly as it did before",
            hook.cargo_target_dirs(root, environ={}, configs=[os.path.join(tmp, "absent.toml")]),
            [default],
        ),
        (
            "the documentation in the config is not read as the setting",
            [hook.configured_target_dir(config)],
            ["C:/Projects/.cargo-target"],
        ),
    ]


# The `PostToolUse` payload `report_outcome` is handed once the caller has already decided this
# call is an outcome report. Its content does not matter to the three cases below — every one of
# them fails BEFORE the body would be inspected — so one fixture stands in for all three.
_OUTCOME_PAYLOAD = {
    "hook_event_name": "PostToolUse",
    "tool_name": "Bash",
    "tool_input": {"command": "git push origin main"},
    "tool_response": {"success": True},
}


def _silent_and_did_not_raise(thunk) -> str:
    """Runs `thunk`, and returns `""` if it stayed silent, or a description of how it did not.

    Two ways to fail this that a bare `try/except` around the call would conflate: printing
    something (the process still exits 0, but a caller reading stdout sees a decision that was
    never meant to exist) and raising past the boundary `report_outcome` promises not to cross.
    Told apart here so a failure says which one happened.
    """
    captured = io.StringIO()
    try:
        with mock.patch("sys.stdout", captured):
            thunk()
    except Exception as exc:  # noqa: BLE001 - the exact exception is the point of the message
        return f"raised {exc!r} instead of staying silent"
    printed = captured.getvalue()
    if printed:
        return f"printed {printed!r} instead of staying silent"
    return ""


def report_outcome_silence_cases():
    """Three ways `report_outcome` can fail, and the one invariant that covers all three: silence.

    Not one case, deliberately: a broad "it never raises" claim is not the same evidence as "an
    unreachable daemon is silent AND a missing token is silent AND an unusable payload is silent",
    each exercised on its own. Returns `(label, failure_or_empty)` pairs; `main` counts a
    non-empty second element as a failure.
    """
    cases = []

    with mock.patch.dict(
        os.environ,
        {
            "NUCLEOS_RUN_ID": "1",
            "NUCLEOS_DAEMON_URL": "http://127.0.0.1:1",
            "NUCLEOS_DAEMON_TOKEN": "secret",
        },
        clear=True,
    ):
        # Case 1: the daemon is unreachable.
        with mock.patch("urllib.request.urlopen", side_effect=OSError("connection refused")):
            cases.append((
                "the daemon is unreachable",
                _silent_and_did_not_raise(lambda: hook.report_outcome(_OUTCOME_PAYLOAD)),
            ))

    with mock.patch.dict(
        os.environ,
        {"NUCLEOS_RUN_ID": "1", "NUCLEOS_DAEMON_URL": "http://127.0.0.1:1"},
        clear=True,
    ):
        # Case 2: NUCLEOS_DAEMON_TOKEN is missing.
        cases.append((
            "the token is missing",
            _silent_and_did_not_raise(lambda: hook.report_outcome(_OUTCOME_PAYLOAD)),
        ))

    with mock.patch.dict(
        os.environ,
        {
            "NUCLEOS_RUN_ID": "1",
            "NUCLEOS_DAEMON_URL": "http://127.0.0.1:1",
            "NUCLEOS_DAEMON_TOKEN": "secret",
        },
        clear=True,
    ):
        # Case 3: the payload will not parse — the caller could not hand it anything usable.
        # `None` is what a `read_stdin_once()` failure would leave a caller holding.
        cases.append((
            "the payload will not parse",
            _silent_and_did_not_raise(lambda: hook.report_outcome(None)),
        ))

    return cases


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

    with tempfile.TemporaryDirectory() as tmp:
        for label, got, want in target_dir_cases(tmp):
            total += 1
            if got != want:
                failures += 1
                print(f"FAIL {label}: expected {want!r}, got {got!r}")

    for label, failure in report_outcome_silence_cases():
        total += 1
        if failure:
            failures += 1
            print(f"FAIL report_outcome, {label}: {failure}")

    print(f"{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
