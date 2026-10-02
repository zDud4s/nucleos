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
import json
import os
import re
import subprocess
import sys
import tempfile
import urllib.error
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
    # Quoted values with spaces, and the `=` spelling.
    (f'{G} -C "C:/My Repo" push origin master', "push"),
    (f'{G} -C "C:\\My Repo" push origin master', "push"),
    (f"{G} -C 'C:/My Repo' -c user.name=x merge feature", "merge"),
    (f'{G} --git-dir="C:/My Repo/.git" push', "push"),
    (f'{G} -c "user.name=A B" fetch origin', "fetch"),
    (f'sh -c "{G} push origin master"', "push"),
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


def binary_candidate_cases():
    """Where `control_token` looks for the daemon binary, and in what order, per platform.

    `os_name` is spelled out, so both orders are checked on whatever machine runs this file.
    Windows must keep the list it always had, in the same order, as the head of the new one.
    """
    roots = [os.path.join("A", "target"), os.path.join("B", "target")]
    exe = [os.path.join(r, b, "nucleos-core.exe") for r in roots for b in ("debug", "release")]
    bare = [os.path.join(r, b, "nucleos-core") for r in roots for b in ("debug", "release")]
    labels = (
        "on Windows the old .exe list comes first, unchanged, then the bare names",
        "off Windows the bare names come first, then the .exe names",
    )
    try:
        windows = hook.daemon_binary_candidates(roots, os_name="nt")
        other = hook.daemon_binary_candidates(roots, os_name="posix")
    except Exception as exc:  # noqa: BLE001 - reported, not raised, so every other case still runs
        return [(label, f"raised {exc!r}") for label in labels]
    return [
        (labels[0], "" if windows == exe + bare else f"got {windows!r}"),
        (labels[1], "" if other == bare + exe else f"got {other!r}"),
    ]


def _token_found_at(wanted):
    """`control_token` with exactly one daemon binary on disk, at `wanted`: `""` if it read `tok`.

    Nothing real runs. `cargo_target_dirs`, `os.path.exists` and `subprocess.run` are answered
    here, the ambient `NUCLEOS_DAEMON_TOKEN` is removed, and `deny()` exits, so its `SystemExit`
    is caught and reported with what it printed.
    """
    target = [os.path.join("T", "target")]

    def run(args, **_kwargs):
        if args[0] == G:
            return subprocess.CompletedProcess(args, 0, stdout="/repo/.git\n", stderr="")
        if list(args) == [wanted, "--print-token"]:
            return subprocess.CompletedProcess(args, 0, stdout="tok\n", stderr="")
        return subprocess.CompletedProcess(args, 1, stdout="", stderr="unexpected call")

    captured = io.StringIO()
    with mock.patch.dict(os.environ, {}, clear=False):
        os.environ.pop("NUCLEOS_DAEMON_TOKEN", None)
        with mock.patch.object(hook, "cargo_target_dirs", lambda _root: target):
            with mock.patch.object(hook.os.path, "exists", lambda path: path == wanted):
                with mock.patch.object(hook.subprocess, "run", side_effect=run):
                    with mock.patch("sys.stdout", captured):
                        try:
                            token = hook.control_token("/repo")
                        except SystemExit:
                            return f"refused: {captured.getvalue().strip()!r}"
    return "" if token == "tok" else f"returned {token!r}"


def control_token_cases():
    """The token is read from the daemon binary under either of its names.

    The bare name is the fix: off Windows cargo writes no `.exe`, and a lookup that knew only the
    `.exe` refused every queue operation an agent asked for. The `.exe` case guards Windows.
    """
    return [
        (
            f"control_token reads the token from debug/{name}",
            _token_found_at(os.path.join("T", "target", "debug", name)),
        )
        for name in ("nucleos-core", "nucleos-core.exe")
    ]


def one_source_cases():
    """One hook source compiled into the daemon, and the tracked `.claude/` copy equal to it.

    Owner decision, 2026-09-14: `core/hooks/ask_daemon.py` is the source, every `include_str!` of
    the hook in `core/src` embeds it, and `.claude/hooks/ask_daemon.py` (the hook this repository's
    own sessions run) is a byte copy. Compared with line endings normalised: a Windows checkout
    with `core.autocrlf` holds CRLF working copies and CI holds LF, and either way it is one script.
    """
    source = os.path.join(ROOT, "core", "hooks", "ask_daemon.py")
    copy = os.path.join(ROOT, ".claude", "hooks", "ask_daemon.py")
    with open(source, "rb") as handle:
        want = handle.read().replace(b"\r\n", b"\n")
    with open(copy, "rb") as handle:
        got = handle.read().replace(b"\r\n", b"\n")
    cases = [(
        ".claude/hooks/ask_daemon.py is a copy of core/hooks/ask_daemon.py",
        "" if got == want else "they differ; copy core/hooks/ask_daemon.py over the .claude/ one",
    )]

    embed = re.compile(r'include_str!\(\s*"([^"]*ask_daemon\.py)"\s*\)')
    found = []
    for folder, _dirs, files in os.walk(os.path.join(ROOT, "core", "src")):
        for name in sorted(files):
            if not name.endswith(".rs"):
                continue
            with open(os.path.join(folder, name), encoding="utf-8", errors="replace") as handle:
                for relative in embed.findall(handle.read()):
                    found.append((name, os.path.normpath(os.path.join(folder, relative))))
    strays = [(name, target) for name, target in found if target != os.path.normpath(source)]
    if len(found) < 2:
        failure = f"expected the embeddings in autopilot.rs and triage.rs, found {found!r}"
    elif strays:
        failure = f"embeds something other than core/hooks/ask_daemon.py: {strays!r}"
    else:
        failure = ""
    cases.append(("every include_str! of ask_daemon.py in core/src embeds core/hooks/", failure))
    return cases


class _Reply:
    """A urlopen result: a context manager whose `.read()` returns the JSON bytes."""

    def __init__(self, body):
        self._body = json.dumps(body).encode()

    def __enter__(self):
        return self

    def __exit__(self, *_exc):
        return False

    def read(self):
        return self._body


def _http_404():
    return urllib.error.HTTPError("http://x", 404, "not found", None, None)


def _pending_file(tmp, session):
    return os.path.join(tmp, f"{session}.json")


def _seed(tmp, session, ids):
    with open(_pending_file(tmp, session), "w", encoding="utf-8") as handle:
        json.dump(ids, handle)


def _tickets(table):
    """A urlopen stand-in: `table` maps a request id to a body or an exception to raise.

    Returns `(side_effect, calls)`; `calls` records every URL asked for.
    """
    calls = []

    def urlopen(request, timeout=None):
        url = request.full_url if hasattr(request, "full_url") else request
        calls.append(url)
        answer = table[int(url.rsplit("/", 1)[-1])]
        if isinstance(answer, Exception):
            raise answer
        return _Reply(answer)

    return urlopen, calls


def _notice_with(tmp, session, table, token="tok"):
    urlopen, calls = _tickets(table)
    with mock.patch.dict(os.environ, {hook.PENDING_DIR_ENV: tmp}):
        with mock.patch.object(hook, "read_control_token", return_value=(token, None)):
            with mock.patch.object(hook.urllib.request, "urlopen", side_effect=urlopen):
                text = hook.settled_notice({"session_id": session, "cwd": "/repo"})
    return text, calls


def _read_ids(tmp, session):
    path = _pending_file(tmp, session)
    if not os.path.exists(path):
        return None
    with open(path, encoding="utf-8") as handle:
        loaded = json.load(handle)
    if isinstance(loaded, dict):
        return [entry["id"] for entry in loaded["ids"]]
    return loaded


def _file_body(tmp, session):
    with open(_pending_file(tmp, session), encoding="utf-8") as handle:
        return json.load(handle)


def _notice_at(tmp, session, table, now, token="tok"):
    """Like `_notice_with`, with the hook's clock fixed at `now`."""
    with mock.patch.object(hook.time, "time", return_value=now):
        return _notice_with(tmp, session, table, token)


def _verdict(good, detail):
    return "" if good else detail


def pending_follow_up_cases():
    """`settled_notice`: told once, silent on every error, no network when nothing is pending."""
    cases = []
    ok = {"id": 7, "status": "succeeded", "result_sha": "abc123", "failure_reason": None}
    with tempfile.TemporaryDirectory() as tmp:
        text, calls = _notice_with(tmp, "s1", {})
        cases.append((
            "settled_notice: nothing pending makes no urlopen call",
            _verdict(text == "" and not calls, f"{text!r} {calls!r}"),
        ))

        _seed(tmp, "s2", [7])
        text, calls = _notice_with(tmp, "s2", {7: ok})
        good = (
            "vcs request #7 has settled" in text
            and "abc123" in text
            and _read_ids(tmp, "s2") is None
        )
        cases.append((
            "settled_notice: a settled request is told and the file cleared",
            _verdict(good, f"{text!r} {_read_ids(tmp, 's2')!r}"),
        ))
        text, calls = _notice_with(tmp, "s2", {7: ok})
        cases.append((
            "settled_notice: told once, the second time is silent with no call",
            _verdict(text == "" and not calls, f"{text!r} {calls!r}"),
        ))

        _seed(tmp, "s3", [8, 9])
        text, _calls = _notice_with(
            tmp, "s3", {8: {"id": 8, "status": "running"}, 9: {"id": 9, "status": "escalated"}}
        )
        good = "#9" in text and "#8" not in text and _read_ids(tmp, "s3") == [8]
        cases.append((
            "settled_notice: an unsettled request is kept, a settled one dropped",
            _verdict(good, f"{text!r} {_read_ids(tmp, 's3')!r}"),
        ))

        _seed(tmp, "s4", [3])
        text, _calls = _notice_with(tmp, "s4", {3: _http_404()})
        cases.append((
            "settled_notice: a 404 drops the id silently",
            _verdict(text == "" and _read_ids(tmp, "s4") is None, f"{text!r}"),
        ))

        _seed(tmp, "s5", [4])
        text, _calls = _notice_with(tmp, "s5", {4: OSError("down")})
        cases.append((
            "settled_notice: an unreachable daemon is silent and keeps the id",
            _verdict(text == "" and _read_ids(tmp, "s5") == [4], f"{text!r}"),
        ))

        _seed(tmp, "s6", [5])
        text, calls = _notice_with(tmp, "s6", {5: ok}, token=None)
        cases.append((
            "settled_notice: no token is silent with no call",
            _verdict(
                text == "" and not calls and _read_ids(tmp, "s6") == [5], f"{text!r} {calls!r}"
            ),
        ))
    cases += pending_expiry_cases()
    return cases


def pending_expiry_cases():
    """Old file format, TTL, throttle, and the settled-statuses tie to the Rust list."""
    cases = []
    running = {"id": 5, "status": "running"}
    ttl, gap = hook.PENDING_TTL_SECONDS, hook.PENDING_RECHECK_SECONDS
    with tempfile.TemporaryDirectory() as tmp:
        # The old plain-int file is still read, and rewritten in the new format.
        _seed(tmp, "old", [5])
        text, calls = _notice_with(tmp, "old", {5: running})
        body = _file_body(tmp, "old")
        good = (
            text == ""
            and len(calls) == 1
            and isinstance(body, dict)
            and [e["id"] for e in body["ids"]] == [5]
            and "since" in body["ids"][0]
        )
        cases.append((
            "settled_notice: an old plain-int file is read and kept",
            _verdict(good, f"{text!r} {body!r}"),
        ))

        _seed(tmp, "old2", [7])
        text, _calls = _notice_with(
            tmp, "old2", {7: {"id": 7, "status": "succeeded", "result_sha": "abc"}}
        )
        cases.append((
            "settled_notice: an old plain-int file's settled id is told",
            _verdict("#7" in text and _read_ids(tmp, "old2") is None, f"{text!r}"),
        ))

        # Throttle: a second call inside the window makes no request; after it, one.
        _seed(tmp, "thr", [{"id": 5, "since": 1000}])
        _text, calls1 = _notice_at(tmp, "thr", {5: running}, 1010)
        _text, calls2 = _notice_at(tmp, "thr", {5: running}, 1010 + gap - 1)
        _text, calls3 = _notice_at(tmp, "thr", {5: running}, 1010 + gap + 1)
        cases.append((
            "settled_notice: a burst inside the recheck window costs one check",
            _verdict(
                (len(calls1), len(calls2), len(calls3)) == (1, 0, 1),
                f"{calls1} {calls2} {calls3}",
            ),
        ))

        # TTL: an unsettled id past the TTL is dropped with a one-time notice.
        _seed(tmp, "ttl", [{"id": 5, "since": 1000}])
        text, _calls = _notice_at(tmp, "ttl", {5: running}, 1000 + ttl + 1)
        good = "#5" in text and "not settled" in text and _read_ids(tmp, "ttl") is None
        cases.append((
            "settled_notice: an id past the TTL is dropped with a notice",
            _verdict(good, f"{text!r}"),
        ))
        text, calls = _notice_at(tmp, "ttl", {5: running}, 1000 + ttl + 100)
        cases.append((
            "settled_notice: the expired id is not told twice",
            _verdict(text == "" and not calls, f"{text!r} {calls!r}"),
        ))

        _seed(tmp, "young", [{"id": 5, "since": 1000}])
        text, _calls = _notice_at(tmp, "young", {5: running}, 1000 + ttl - 1)
        cases.append((
            "settled_notice: an id inside the TTL is kept",
            _verdict(text == "" and _read_ids(tmp, "young") == [5], f"{text!r}"),
        ))

        # The TTL holds with the daemon down and with no token, the cases that repeat forever.
        _seed(tmp, "down", [{"id": 5, "since": 1000}])
        text, _calls = _notice_at(tmp, "down", {5: OSError("down")}, 1000 + ttl + 1)
        cases.append((
            "settled_notice: an id past the TTL is dropped with the daemon down",
            _verdict("#5" in text and _read_ids(tmp, "down") is None, f"{text!r}"),
        ))
        _seed(tmp, "notok", [{"id": 5, "since": 1000}])
        text, calls = _notice_at(tmp, "notok", {5: running}, 1000 + ttl + 1, token=None)
        cases.append((
            "settled_notice: an id past the TTL is dropped with no token, no call",
            _verdict(
                "#5" in text and not calls and _read_ids(tmp, "notok") is None,
                f"{text!r} {calls!r}",
            ),
        ))

        # A token passed in is used and read_control_token is never reached.
        _seed(tmp, "given", [5])
        urlopen, calls = _tickets({5: running})
        with mock.patch.dict(os.environ, {hook.PENDING_DIR_ENV: tmp}):
            with mock.patch.object(hook, "read_control_token", side_effect=AssertionError("read")):
                with mock.patch.object(hook.urllib.request, "urlopen", side_effect=urlopen):
                    try:
                        hook.settled_notice({"session_id": "given", "cwd": "/repo"}, "given-token")
                        failure = _verdict(len(calls) == 1, f"{calls!r}")
                    except AssertionError:
                        failure = "settled_notice read the token despite being given one"
        cases.append(("settled_notice: a token passed in is used, not re-read", failure))

    # The Python list is the Rust one.
    with open(os.path.join(ROOT, "core", "src", "vcs.rs"), encoding="utf-8") as handle:
        source = handle.read()
    found = re.search(r"TERMINAL_STATUSES:\s*\[&str;\s*\d+\]\s*=\s*\[(.*?)\]", source, re.S)
    rust = set(re.findall(r'"([a-z_]+)"', found.group(1))) if found else None
    cases.append((
        "SETTLED_STATUSES equals vcs.rs TERMINAL_STATUSES",
        _verdict(rust == set(hook.SETTLED_STATUSES), f"{rust!r} vs {hook.SETTLED_STATUSES!r}"),
    ))
    return cases


def _run_interactive(tmp, payload, urlopen_side_effect, token="tok"):
    """Runs `interactive_session`; returns `(exit_code, printed)`."""
    captured = io.StringIO()
    code = "no exit"
    with mock.patch.dict(os.environ, {hook.PENDING_DIR_ENV: tmp}):
        os.environ.pop("NUCLEOS_RUN_ID", None)
        with mock.patch.object(hook, "read_control_token", return_value=(token, None)):
            with mock.patch.object(hook, "control_token", return_value=token):
                with mock.patch.object(
                    hook.urllib.request, "urlopen", side_effect=urlopen_side_effect
                ):
                    with mock.patch("sys.stdout", captured):
                        try:
                            hook.interactive_session(payload)
                        except SystemExit as exit_:
                            code = exit_.code
    return code, captured.getvalue()


def interactive_follow_up_cases():
    """The interactive branch: context only, never a permission decision; remembers unsettled ids."""
    cases = []
    with tempfile.TemporaryDirectory() as tmp:
        _seed(tmp, "sess", [11])
        urlopen, _calls = _tickets({11: {"id": 11, "status": "failed", "failure_reason": "boom"}})
        payload = {"session_id": "sess", "tool_name": "Read", "tool_input": {}, "cwd": "/repo"}
        code, printed = _run_interactive(tmp, payload, urlopen)
        try:
            out = json.loads(printed)
            special = out["hookSpecificOutput"]
            good = (
                code in (0, None)
                and set(out) == {"hookSpecificOutput"}
                and "additionalContext" in special
                and "permissionDecision" not in special
                and "#11" in special["additionalContext"]
            )
            failure = _verdict(good, f"{code!r} {printed!r}")
        except Exception as exc:  # noqa: BLE001 - the message names what was printed
            failure = f"unparseable {printed!r}: {exc!r}"
        cases.append(("a non-git call with a settled pending id only adds context", failure))

        deny_body = {"decision": "deny", "reason": "queued", "request_id": 21, "settled": False}

        def post(_request, timeout=None):
            return _Reply(deny_body)

        payload = {
            "session_id": "sess2",
            "tool_name": "Bash",
            "tool_input": {"command": "git push"},
            "cwd": "/repo",
        }
        code, printed = _run_interactive(tmp, payload, post)
        good = code in (0, None) and '"block"' in printed and _read_ids(tmp, "sess2") == [21]
        cases.append((
            "a daemon deny with settled:false remembers its request id",
            _verdict(good, f"{code!r} {printed!r} {_read_ids(tmp, 'sess2')!r}"),
        ))

        # A daemon deny with a settled notice appended: reason, blank line, notice.
        _seed(tmp, "sess3", [11])
        running_21 = {"id": 21, "status": "running"}
        failed_11 = {"id": 11, "status": "failed", "failure_reason": "boom"}

        def by_url(request, timeout=None):
            url = request.full_url
            if url.endswith("/hooks/session-git-decision"):
                return _Reply(deny_body)
            return _Reply(failed_11 if url.endswith("/11") else running_21)

        payload3 = {
            "session_id": "sess3",
            "tool_name": "Bash",
            "tool_input": {"command": "git push"},
            "cwd": "/repo",
        }
        code, printed = _run_interactive(tmp, payload3, by_url)
        try:
            reason = json.loads(printed)["reason"]
        except Exception:
            reason = None
        good = reason is not None and reason.startswith("queued\n\nvcs request #11 has settled")
        cases.append((
            "a deny carries the settled notice after the reason",
            _verdict(good, f"{printed!r}"),
        ))

        # A daemon deny that is already settled records no id.
        settled_body = {"decision": "deny", "reason": "done", "request_id": 31, "settled": True}
        payload4 = dict(payload3, session_id="sess4")
        code, printed = _run_interactive(
            tmp, payload4, lambda r, timeout=None: _Reply(settled_body)
        )
        cases.append((
            "a daemon deny with settled:true records no id",
            _verdict('"block"' in printed and _read_ids(tmp, "sess4") is None, f"{printed!r}"),
        ))

        # The refusal path reads the token once.
        _seed(tmp, "sess5", [11])
        reads = []

        def counted(*_args, **_kwargs):
            reads.append(1)
            return "t", None

        payload5 = dict(payload3, session_id="sess5")
        captured = io.StringIO()
        with mock.patch.dict(os.environ, {hook.PENDING_DIR_ENV: tmp}):
            with mock.patch.object(hook, "read_control_token", side_effect=counted):
                with mock.patch.object(hook.urllib.request, "urlopen", side_effect=by_url):
                    with mock.patch("sys.stdout", captured):
                        try:
                            hook.interactive_session(payload5)
                        except SystemExit:
                            pass
        cases.append((
            "the refusal path reads the token once",
            _verdict(len(reads) == 1, f"{len(reads)} reads"),
        ))

        payload["session_id"] = "../evil"
        before = sorted(os.listdir(tmp))
        _run_interactive(tmp, payload, post)
        after = sorted(os.listdir(tmp))
        stray = os.path.exists(os.path.join(tmp, "..", "evil.json"))
        cases.append((
            "an unsafe session_id writes nothing",
            _verdict(before == after and not stray, f"{before!r} {after!r}"),
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

    for label, failure in (
        binary_candidate_cases()
        + control_token_cases()
        + pending_follow_up_cases()
        + interactive_follow_up_cases()
        + one_source_cases()
    ):
        total += 1
        if failure:
            failures += 1
            print(f"FAIL {label}: {failure}")

    print(f"{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
