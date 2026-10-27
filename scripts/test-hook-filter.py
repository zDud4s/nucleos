#!/usr/bin/env python3
"""Cover the filter and the outcome reporter in `core/hooks/ask_daemon.py`.

**This is the most load-bearing Python in the repository and it had no test at all.** The script is
`include_str!`'d into the daemon by `core/src/governance/autopilot.rs`'s `HOOK_SOURCE` — `include_str!("../hooks/
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
    # `pull` fetches and merges into the checked-out branch: the daemon refuses a direct one, so
    # the hook has to ask it.
    (f"{G} pull", "pull"),
    (f"cd /repo && {G} pull origin master", "pull"),
    # The word as the shell sees it, not as it is typed: a bash backslash escape, a PowerShell
    # backtick escape, and quotes around the program or the verb all run the same git.
    (f"{G} pu\\sh origin master", "push"),
    (f"{G} pu`sh origin master", "push"),
    (f"{G} m`erge feature", "merge"),
    (f'"{G}" push origin master', "push"),
    (f'{G} "push" origin master', "push"),
    (f"'{G}' 'merge' feature", "merge"),
    (f'{G} p"u"sh origin master', "push"),
    (f"{G} p''ull", "pull"),
    # And the escapes leave the harmless alone.
    (f"{G} st`atus", ""),
    (f"{G} st\\atus", ""),
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


def _fake_response(body):
    """A stand-in for what `urllib.request.urlopen` yields: a context manager with `.read()`."""
    response = mock.MagicMock()
    response.read.return_value = body if isinstance(body, bytes) else json.dumps(body).encode()
    response.__enter__.return_value = response
    response.__exit__.return_value = False
    return response


_ADMIT_PAYLOAD = {
    "hook_event_name": "PreToolUse",
    "tool_name": "Bash",
    "tool_use_id": "toolu_1",
    "tool_input": {"command": "cargo test"},
}


def admit_cases():
    """`admit_heavy`: the one fail-open leg on the PreToolUse path.

    Every case is judged on the same two things: it stayed silent (it never prints a decision) and
    it did not raise, plus the specific behaviour the label names. Returns `(label, failure)` pairs.
    """
    cases = []

    def admit(payload, urlopen):
        return _silent_and_did_not_raise(
            lambda: hook.admit_heavy("http://127.0.0.1:1", "secret", 7, payload)
        )

    # A light or unidentified call never asks: not a shell tool, or no usable tool_use_id.
    for label, payload in (
        ("a non-shell tool", dict(_ADMIT_PAYLOAD, tool_name="Read")),
        ("a missing tool_use_id", {k: v for k, v in _ADMIT_PAYLOAD.items() if k != "tool_use_id"}),
        ("an empty tool_use_id", dict(_ADMIT_PAYLOAD, tool_use_id="")),
        ("a non-string tool_use_id", dict(_ADMIT_PAYLOAD, tool_use_id=5)),
    ):
        with mock.patch("urllib.request.urlopen", return_value=_fake_response({"admission": "granted"})) as opened:
            failure = admit(payload, opened)
            if not failure and opened.call_count != 0:
                failure = f"asked the daemon {opened.call_count} time(s)"
        cases.append((f"admit_heavy, a_light_or_unidentified_call_never_asks ({label})", failure))

    # An unreachable daemon lets the command run, silently, after exactly one attempt.
    with mock.patch("urllib.request.urlopen", side_effect=OSError("connection refused")) as opened:
        failure = admit(_ADMIT_PAYLOAD, opened)
        if not failure and opened.call_count != 1:
            failure = f"expected one attempt, got {opened.call_count}"
    cases.append(("admit_heavy, an_unreachable_daemon_lets_the_command_run_silently", failure))

    # waiting, waiting, granted: polled until granted, then stopped.
    answers = [
        _fake_response({"admission": "waiting"}),
        _fake_response({"admission": "waiting"}),
        _fake_response({"admission": "granted"}),
    ]
    with mock.patch("urllib.request.urlopen", side_effect=answers) as opened:
        failure = admit(_ADMIT_PAYLOAD, opened)
        if not failure and opened.call_count != 3:
            failure = f"expected 3 polls, got {opened.call_count}"
        if not failure:
            request = opened.call_args_list[0][0][0]
            if not request.full_url.endswith("/hooks/admit"):
                failure = f"polled {request.full_url!r}"
            else:
                sent = json.loads(request.data)
                want = {
                    "run_id": 7,
                    "tool_use_id": "toolu_1",
                    "tool_name": "Bash",
                    "tool_input": {"command": "cargo test"},
                }
                if sent != want:
                    failure = f"sent {sent!r}"
    cases.append(("admit_heavy, waiting_then_granted_polls_until_granted", failure))

    # gave_up (and any answer that is not "waiting") lets the command run after one call.
    for answer in ({"admission": "gave_up"}, {"admission": "something-new"}, {}, ["waiting"], b"not json"):
        with mock.patch("urllib.request.urlopen", return_value=_fake_response(answer)) as opened:
            failure = admit(_ADMIT_PAYLOAD, opened)
            if not failure and opened.call_count != 1:
                failure = f"expected one call, got {opened.call_count}"
        cases.append((f"admit_heavy, gave_up_lets_the_command_run ({answer!r})", failure))

    # The hook budget ends the wait: a daemon that always says waiting is not polled forever.
    ticks = iter(range(0, 10_000_000, 1000))
    limit = 60
    state = {"calls": 0}

    def always_waiting(*_args, **_kwargs):
        state["calls"] += 1
        if state["calls"] > limit:
            return _fake_response({"admission": "granted"})
        return _fake_response({"admission": "waiting"})

    with mock.patch("urllib.request.urlopen", side_effect=always_waiting), mock.patch.object(
        hook.time, "monotonic", side_effect=lambda: next(ticks)
    ):
        failure = admit(_ADMIT_PAYLOAD, None)
    if not failure and state["calls"] >= limit:
        failure = f"kept polling past the budget ({state['calls']} calls)"
    if not failure and hook.ADMIT_HOOK_BUDGET_SECONDS >= hook.ADMIT_POLL_TIMEOUT_SECONDS * 100:
        failure = "budget constant is implausible"
    cases.append(("admit_heavy, the_hook_budget_ends_the_wait", failure))

    # report_outcome carries the tool_use_id to the daemon.
    sent_bodies = []

    def capture(request, timeout=None):
        sent_bodies.append(json.loads(request.data))
        return _fake_response({})

    with mock.patch.dict(
        os.environ,
        {
            "NUCLEOS_RUN_ID": "1",
            "NUCLEOS_DAEMON_URL": "http://127.0.0.1:1",
            "NUCLEOS_DAEMON_TOKEN": "secret",
        },
        clear=True,
    ), mock.patch("urllib.request.urlopen", side_effect=capture):
        failure = _silent_and_did_not_raise(
            lambda: hook.report_outcome(dict(_OUTCOME_PAYLOAD, tool_use_id="toolu_9"))
        )
    if not failure and (not sent_bodies or sent_bodies[0].get("tool_use_id") != "toolu_9"):
        failure = f"body was {sent_bodies!r}"
    cases.append(("report_outcome_sends_the_tool_use_id", failure))

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


def _token_found_at(wanted, env=None):
    """`control_token` with exactly one daemon binary on disk, at `wanted`: `""` if it read `tok`.

    Nothing real runs. `cargo_target_dirs`, `os.path.exists` and `subprocess.run` are answered
    here, the ambient `NUCLEOS_DAEMON_TOKEN` is removed, and `deny()` exits, so its `SystemExit`
    is caught and reported with what it printed. `env` is laid over the environment.
    """
    target = [os.path.join("T", "target")]

    def run(args, **_kwargs):
        if args[0] == G:
            return subprocess.CompletedProcess(args, 0, stdout="/repo/.git\n", stderr="")
        if list(args) == [wanted, "--print-token"]:
            return subprocess.CompletedProcess(args, 0, stdout="tok\n", stderr="")
        return subprocess.CompletedProcess(args, 1, stdout="", stderr="unexpected call")

    captured = io.StringIO()
    with mock.patch.dict(os.environ, env or {}, clear=False):
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
    ] + [
        (
            # The daemon started by scripts/run-daemon.ps1 runs from this copy, and a machine that
            # only starts it that way has nothing under any target dir: the guard refused --land.
            f"control_token reads the token from the staged run copy, .nucleos-run/{name}",
            _token_found_at(os.path.join(os.path.dirname("/repo"), ".nucleos-run", name)),
        )
        for name in ("nucleos-core", "nucleos-core.exe")
    ] + [
        (
            # Since 2026-10-19 the script stages it in the dev home, out of the projects dir.
            f"control_token reads the token from the dev home's run copy, run/{name}",
            _token_found_at(os.path.join("/dev-home", "run", name),
                            env={"NUCLEOS_DEV_HOME": "/dev-home"}),
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
    with open(os.path.join(ROOT, "core", "src", "execution", "vcs.rs"), encoding="utf-8") as handle:
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


def _run_main_gate(replies):
    """Runs `main()` as a governed run (NUCLEOS_RUN_ID=7); returns `(exit_code, printed)`.

    `replies` are the daemon's JSON bodies, served in order to each urlopen call: the first is
    the pretooluse-decision answer, the second (if any) the ask-wait answer.
    """
    payload = {
        "hook_event_name": "PreToolUse",
        "session_id": "rewrite",
        "tool_name": "Bash",
        "tool_input": {"command": "cargo test"},
        "cwd": "/work/repo",
    }
    queue = list(replies)

    def urlopen(_request, timeout=None):
        return _Reply(queue.pop(0))

    env = {
        "NUCLEOS_RUN_ID": "7",
        "NUCLEOS_DAEMON_URL": "http://127.0.0.1:1",
        "NUCLEOS_DAEMON_TOKEN": "tok",
    }
    captured = io.StringIO()
    code = "no exit"
    with mock.patch.dict(os.environ, env):
        with mock.patch("sys.stdin", io.StringIO(json.dumps(payload))):
            with mock.patch.object(hook.urllib.request, "urlopen", side_effect=urlopen):
                with mock.patch("sys.stdout", captured):
                    try:
                        hook.main()
                    except SystemExit as exit_:
                        code = exit_.code
    return code, captured.getvalue()


def _special_of(printed):
    try:
        return json.loads(printed)["hookSpecificOutput"]
    except Exception:  # noqa: BLE001 - the caller reports what was printed
        return None


def run_rewrite_cases():
    """A run's allow forwards the daemon's `updated_input` as `updatedInput`; nothing else does."""
    cases = []

    code, printed = _run_main_gate(
        [{"decision": "allow", "reason": "ok", "updated_input": {"command": "X"}}]
    )
    special = _special_of(printed)
    good = (
        code in (0, None)
        and special is not None
        and special.get("permissionDecision") == "allow"
        and special.get("updatedInput") == {"command": "X"}
    )
    cases.append((
        "an allow with updated_input prints updatedInput",
        _verdict(good, f"{code!r} {printed!r}"),
    ))

    code, printed = _run_main_gate(
        [{"decision": "allow", "reason": "ok", "updated_input": "junk"}]
    )
    special = _special_of(printed)
    good = (
        code in (0, None)
        and special is not None
        and special.get("permissionDecision") == "allow"
        and "updatedInput" not in special
    )
    cases.append((
        "an allow with a malformed updated_input prints no updatedInput",
        _verdict(good, f"{code!r} {printed!r}"),
    ))

    code, printed = _run_main_gate(
        [{"decision": "deny", "reason": "no", "updated_input": {"command": "X"}}]
    )
    good = '"block"' in printed and "updatedInput" not in printed
    cases.append((
        "a deny with updated_input prints the block contract and no updatedInput",
        _verdict(good, f"{code!r} {printed!r}"),
    ))

    code, printed = _run_main_gate([
        {"decision": "asking", "updated_input": {"command": "X"}},
        {"decision": "allow", "reason": "approved"},
    ])
    special = _special_of(printed)
    good = (
        code in (0, None)
        and special is not None
        and special.get("permissionDecision") == "allow"
        and special.get("updatedInput") == {"command": "X"}
    )
    cases.append((
        "asking then allow keeps the first answer's updatedInput",
        _verdict(good, f"{code!r} {printed!r}"),
    ))
    return cases


HINT_TEXT = (
    "this project verifies through the daemon: call mcp__nucleos__verify to run this "
    "worktree's build, test and gate commands in its queue and cache; it answers within "
    "seconds with the verdict or a ticket, which you wait on in the background with "
    "`nucleos-core --verify-ticket <id>` (Monitor or Bash run_in_background); never poll "
    "mcp__nucleos__verify_status. Nothing was refused; this command runs as written."
)
HINTS_ENV = "NUCLEOS_VERIFY_HINTS_FILE"


def _hint_entry(expires_at):
    return {
        "project": "alpha",
        "expires_at": expires_at,
        "tools": {"cargo": ["fmt"], "tsc": []},
        "entrypoints": ["scripts/gates.sh"],
    }


def _write_hints(path, worktrees):
    with open(path, "w", encoding="utf-8") as handle:
        json.dump({"version": 1, "worktrees": worktrees}, handle)


def _bash(command, cwd="/work/repo", session="hint"):
    return {
        "session_id": session,
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "cwd": cwd,
    }


def _run_hinted(tmp, hints_path, payload, urlopen=None):
    """`_run_interactive` with the hints file pointed at `hints_path`."""
    if urlopen is None:
        def urlopen(_request, timeout=None):
            raise AssertionError("no daemon call is expected here")
    with mock.patch.dict(os.environ, {HINTS_ENV: hints_path}):
        return _run_interactive(tmp, payload, urlopen)


def _context_of(printed):
    """The additionalContext printed, or None; fails loudly on anything but context alone."""
    if not printed.strip():
        return None
    out = json.loads(printed)
    special = out["hookSpecificOutput"]
    if set(out) != {"hookSpecificOutput"} or "permissionDecision" in special:
        raise AssertionError(f"not a context-only answer: {printed!r}")
    return special["additionalContext"]


def _guarded(label, thunk):
    """One case: `thunk` returns a failure string; an exception is a failure, not a crash."""
    try:
        return (label, thunk())
    except Exception as exc:  # noqa: BLE001 - the message names what broke
        return (label, f"{type(exc).__name__}: {exc}")


def verify_hint_cases():
    """The interactive hook's local verify hint (F2c-3): only ever adds context, OFF is untouched."""
    import time as clock

    cases = []

    def identical():
        failures = []
        with tempfile.TemporaryDirectory() as tmp:
            missing = os.path.join(tmp, "absent.json")
            read = {"session_id": "hint", "tool_name": "Read", "tool_input": {}, "cwd": "/work/repo"}
            payloads = [
                (_bash("cargo test"), False),
                (dict(_bash("npm test"), tool_name="PowerShell"), False),
                (read, False),
                (_bash("ls"), False),
                (read, True),  # with a settled pending id to tell
            ]
            for payload, settled in payloads:
                outputs = []
                for patched in (False, True):
                    urlopen = None
                    if settled:
                        _seed(tmp, "hint", [11])
                        urlopen, _calls = _tickets(
                            {11: {"id": 11, "status": "failed", "failure_reason": "boom"}}
                        )
                    if patched:
                        # Reference output: the pre-change path, the hint switched off outright.
                        with mock.patch.object(hook, "verify_hint", return_value=""):
                            outputs.append(_run_hinted(tmp, missing, payload, urlopen))
                    else:
                        outputs.append(_run_hinted(tmp, missing, payload, urlopen))
                if outputs[0] != outputs[1]:
                    failures.append(f"{payload['tool_name']} settled={settled}: {outputs!r}")
        return "; ".join(failures)

    cases.append(_guarded("with no live hint the interactive output is byte-identical", identical))

    def hint_never_invites_polling():
        text = hook.VERIFY_HINT
        failures = []
        if text != HINT_TEXT:
            failures.append(f"hint text drifted from the pinned one: {text!r}")
        if "nucleos-core --verify-ticket" not in text:
            failures.append("the hint does not name the background wait command")
        if "never poll mcp__nucleos__verify_status" not in text:
            failures.append("the hint does not say never to poll verify_status")
        if "wait on it with" in text or text.count("verify_status") != 1:
            failures.append("the hint still invites waiting on verify_status")
        return "; ".join(failures)

    cases.append(_guarded("the verify hint says push or background wait, never poll", hint_never_invites_polling))

    def adds_nothing():
        now = int(clock.time())
        failures = []
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "hints.json")
            payload = _bash("cargo test")
            # The same payload gets its hint from a live entry, so silence below means something.
            _write_hints(path, {"/work/repo": _hint_entry(now + 60)})
            _code, printed = _run_hinted(tmp, path, payload)
            if _context_of(printed) != HINT_TEXT:
                failures.append(f"control got {printed!r}")
            variants = {
                "expired": {"/work/repo": _hint_entry(now - 5)},
                "expiring now": {"/work/repo": _hint_entry(now)},
                "foreign worktree": {"/work/other": _hint_entry(now + 60)},
                "sibling prefix": {"/work/rep": _hint_entry(now + 60)},
                "entry not an object": {"/work/repo": "x"},
            }
            for label, worktrees in variants.items():
                _write_hints(path, worktrees)
                _code, printed = _run_hinted(tmp, path, payload)
                if printed.strip():
                    failures.append(f"{label}: {printed!r}")
            bodies = {
                "malformed": "{not json",
                "list": "[]",
                "worktrees list": '{"version":1,"worktrees":[]}',
                "empty": "",
            }
            for label, body in bodies.items():
                with open(path, "w", encoding="utf-8") as handle:
                    handle.write(body)
                code, printed = _run_hinted(tmp, path, payload)
                if printed.strip() or code not in (0, None):
                    failures.append(f"{label}: {code!r} {printed!r}")
        return "; ".join(failures)

    cases.append(_guarded("an expired, foreign or malformed hint adds nothing", adds_nothing))

    def adds_context():
        now = int(clock.time())
        failures = []
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "hints.json")
            _write_hints(path, {"/work/repo": _hint_entry(now + 60)})
            hits = [
                _bash("cargo test"),
                _bash("cargo test", cwd="/work/repo/core"),
                _bash("cargo +nightly test --workspace"),
                _bash("cargo --manifest-path core/Cargo.toml test"),
                _bash("cargo.exe test"),
                _bash("/usr/bin/cargo build"),
                _bash("tsc -b"),
                _bash("bash scripts/gates.sh core"),
                _bash("./scripts/gates.sh core"),
                _bash("RUST_LOG=debug cargo test"),
                _bash("env RUST_LOG=debug cargo test"),
                _bash("python scripts/heavy.py cargo test -p nucleos-core"),
                _bash("cd core && cargo test"),
                dict(_bash("cargo test"), tool_name="PowerShell"),
            ]
            for payload in hits:
                code, printed = _run_hinted(tmp, path, payload)
                text = _context_of(printed)
                if code not in (0, None) or text != HINT_TEXT:
                    failures.append(f"{payload['tool_input']['command']!r}: {code!r} {printed!r}")
            # A Windows cwd (backslashes) matches a forward-slash key, from a subdirectory too.
            _write_hints(path, {"C:/work/repo": _hint_entry(now + 60)})
            win = _bash("cargo test", cwd="C:\\work\\repo\\core")
            _code, printed = _run_hinted(tmp, path, win)
            if _context_of(printed) != HINT_TEXT:
                failures.append(f"windows cwd: {printed!r}")
            _write_hints(path, {"/work/repo": _hint_entry(now + 60)})
            read_it = {
                "session_id": "hint",
                "tool_name": "Read",
                "tool_input": {"command": "cargo test"},
                "cwd": "/work/repo",
            }
            not_text = {
                "session_id": "hint",
                "tool_name": "Bash",
                "tool_input": {"command": 5},
                "cwd": "/work/repo",
            }
            misses = [
                _bash("cargo fmt"),  # an allowed subcommand
                _bash("cargo --version"),  # no subcommand
                _bash("cargo"),
                _bash("ls -la"),
                _bash("git status"),
                _bash("echo cargo test"),
                _bash("grep -n cargo Cargo.toml"),
                _bash('git commit -m "run cargo test"'),
                _bash("cat scripts/gates.sh"),
                _bash("cargo test", cwd="/elsewhere"),
                read_it,
                not_text,
            ]
            for payload in misses:
                code, printed = _run_hinted(tmp, path, payload)
                if printed.strip() or code not in (0, None):
                    failures.append(f"miss {payload['tool_input']!r}: {code!r} {printed!r}")
        return "; ".join(failures)

    cases.append(_guarded("a live hint adds context and never a decision", adds_context))

    def after_notice():
        now = int(clock.time())
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "hints.json")
            _write_hints(path, {"/work/repo": _hint_entry(now + 60)})
            _seed(tmp, "hint", [11])
            urlopen, _calls = _tickets({11: {"id": 11, "status": "failed", "failure_reason": "boom"}})
            code, printed = _run_hinted(tmp, path, _bash("cargo test"), urlopen)
            text = _context_of(printed)
            joined = "\n\n" + HINT_TEXT
            good = (
                code in (0, None)
                and text is not None
                and "#11" in text
                and text.endswith(joined)
                and text.index("#11") < text.rindex(joined)
            )
            return _verdict(good, f"{code!r} {printed!r}")

    cases.append(_guarded("the hint follows the settled notice", after_notice))

    def same_file_name():
        with open(os.path.join(ROOT, "core", "src", "verify", "ide_box.rs"), encoding="utf-8") as handle:
            source = handle.read()
        found = re.search(r'HINTS_FILE:\s*&str\s*=\s*"(.*?)"', source)
        rust = found.group(1) if found else None
        return _verdict(rust == hook.HINTS_FILE, f"{rust!r} vs {hook.HINTS_FILE!r}")

    cases.append(_guarded("HINTS_FILE equals verify/ide_box.rs HINTS_FILE", same_file_name))
    return cases


def _run_bundle(payload, urlopen_side_effect):
    """Runs `main()` on a payload with a mocked stdin/urlopen; returns `(exit_code, printed)`."""
    captured = io.StringIO()
    code = "no exit"
    with mock.patch.dict(os.environ, {}):
        os.environ.pop("NUCLEOS_RUN_ID", None)
        with mock.patch.object(hook, "read_control_token", return_value=("tok", None)):
            with mock.patch.object(
                hook.urllib.request, "urlopen", side_effect=urlopen_side_effect
            ):
                with mock.patch("sys.stdin", io.StringIO(json.dumps(payload))):
                    with mock.patch("sys.stdout", captured):
                        try:
                            hook.main()
                        except SystemExit as exit_:
                            code = exit_.code
    return code, captured.getvalue()


def session_start_cases():
    """SessionStart is forwarded to the daemon's bundle hooks, verbatim, and fails open."""
    cases = []
    cases.append((
        "SessionStart is a forwarded session event",
        "" if "SessionStart" in hook.SESSION_EVENTS else f"missing from {hook.SESSION_EVENTS!r}",
    ))
    context = json.dumps({
        "hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "tree"}
    })
    reply = _Reply({"results": [{"exit_code": 0, "stdout": context, "stderr": ""}]})
    payload = {"hook_event_name": "SessionStart", "cwd": "/work/repo"}

    code, printed = _run_bundle(payload, [reply])
    cases.append((
        "main routes SessionStart to bundle_hooks and prints the JSON verbatim",
        "" if code == 0 and printed.strip() == context else f"exit {code!r}, printed {printed!r}",
    ))

    code, printed = _run_bundle(payload, urllib.error.URLError("down"))
    cases.append((
        "SessionStart fails open when the daemon is unreachable",
        "" if code == 0 and printed == "" else f"exit {code!r}, printed {printed!r}",
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
        + verify_hint_cases()
        + run_rewrite_cases()
        + admit_cases()
        + one_source_cases()
        + session_start_cases()
    ):
        total += 1
        if failure:
            failures += 1
            print(f"FAIL {label}: {failure}")

    print(f"{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
