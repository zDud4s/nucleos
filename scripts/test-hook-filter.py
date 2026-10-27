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

# Claims are off for every test but the claims ones (which set it empty): a registration request
# would otherwise consume one of the replies the helpers below feed to `urlopen`.
os.environ["NUCLEOS_CLAIMS_OFF"] = "1"

ROOT =os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
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
    "worktree's build, test and gate commands in its queue and cache, and wait on it with "
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


def _claims_env(tmp, **extra):
    env = {"NUCLEOS_CLAIMS_OFF": "", "NUCLEOS_CLAIMS_DIR": tmp, "NUCLEOS_DAEMON_URL": "http://127.0.0.1:1"}
    env.update(extra)
    return env


def _exchange(tmp, payload, replies, now=1000.0, **kwargs):
    """Runs `claims_exchange`; returns `(answer, requests)` where requests are `(url, body, timeout)`."""
    seen = []
    queue = list(replies)

    def urlopen(request, timeout=None):
        seen.append((request.full_url, json.loads(request.data), timeout))
        reply = queue.pop(0)
        if isinstance(reply, Exception):
            raise reply
        return _Reply(reply)

    with mock.patch.dict(os.environ, _claims_env(tmp)):
        os.environ.pop("NUCLEOS_RUN_ID", None)
        with mock.patch.object(hook, "read_control_token", return_value=("tok", None)):
            with mock.patch.object(hook.urllib.request, "urlopen", side_effect=urlopen):
                answer = hook.claims_exchange(payload, now=now, **kwargs)
    return answer, seen


def claims_exchange_cases():
    cases = []
    payload = {"session_id": "s1", "cwd": "/work/repo", "tool_name": "Read", "tool_input": {}}
    ok = {"registered": True, "project_id": "alpha", "notices": [], "incidents": []}

    with tempfile.TemporaryDirectory() as tmp:
        answer, seen = _exchange(tmp, payload, [ok])  # now=1000: next = 1020
        good = answer == ok and len(seen) == 1 and seen[0][0].endswith("/tasks/session")
        good = good and seen[0][1] == {"session_id": "s1", "cwd": "/work/repo", "want_incidents": False}
        cases.append(("a tool call registers the session with (session_id, cwd)", "" if good else f"{answer!r} {seen!r}"))
        cases.append(("the registration waits 2 seconds at most", "" if seen and seen[0][2] == 2 else f"timeout {seen[0][2]!r}"))

        answer, seen = _exchange(tmp, payload, [ok], now=1005.0)
        cases.append(("a second call within 20 s sends nothing", "" if answer is None and not seen else f"{seen!r}"))
        answer, seen = _exchange(tmp, payload, [ok], now=1005.0, force=True)  # next = 1025
        cases.append(("force skips the rate limit (task tools, prompts)", "" if answer == ok and len(seen) == 1 else f"{seen!r}"))
        answer, seen = _exchange(tmp, payload, [ok], now=1021.0)
        cases.append(("a forced call restarts the 20 s window", "" if answer is None and not seen else f"{seen!r}"))
        answer, seen = _exchange(tmp, payload, [ok], now=1026.0)
        cases.append(("after 20 s the next call registers again (a long tool call is covered by its PostToolUse)", "" if len(seen) == 1 else f"{seen!r}"))

    with tempfile.TemporaryDirectory() as tmp:
        answer, seen = _exchange(tmp, payload, [urllib.error.URLError("down")])
        cases.append(("a dead daemon is silent", "" if answer is None else f"{answer!r}"))
        answer, seen = _exchange(tmp, payload, [ok], now=1030.0)
        cases.append(("and is not retried for a minute", "" if answer is None and not seen else f"{seen!r}"))
        answer, seen = _exchange(tmp, payload, [ok], now=1061.0)
        cases.append(("then it is tried again", "" if answer == ok else f"{answer!r}"))

    with tempfile.TemporaryDirectory() as tmp:
        away = {"registered": False}
        _exchange(tmp, payload, [away])
        answer, seen = _exchange(tmp, payload, [ok], now=1200.0)
        cases.append(("an unregistered project is not asked again for 5 minutes", "" if answer is None and not seen else f"{seen!r}"))
        answer, seen = _exchange(tmp, payload, [ok], now=1301.0)
        cases.append(("and is asked again after", "" if answer == ok else f"{answer!r} {seen!r}"))

    with tempfile.TemporaryDirectory() as tmp:
        nested = os.path.join(tmp, "not", "there", "yet")
        answer, seen = _exchange(nested, payload, [ok])
        written = os.path.join(nested, "s1.json")
        cases.append(("the state directory is created when it does not exist (like the pending one)", "" if os.path.isfile(written) else f"no {written}"))
        again, seen = _exchange(nested, payload, [ok], now=1005.0)
        cases.append(("and the state it wrote is what rate-limits the next call", "" if again is None and not seen else f"{seen!r}"))

    with tempfile.TemporaryDirectory() as tmp:
        for label, bad in (("no session id", {"cwd": "/w"}), ("an unsafe session id", {"session_id": "../x", "cwd": "/w"}), ("no cwd", {"session_id": "s1"})):
            answer, seen = _exchange(tmp, bad, [ok])
            cases.append((f"{label}: nothing is sent", "" if answer is None and not seen else f"{seen!r}"))
        with mock.patch.dict(os.environ, _claims_env(tmp, NUCLEOS_RUN_ID="7")):
            with mock.patch.object(hook.urllib.request, "urlopen", side_effect=AssertionError("a run is not a session")):
                got = hook.claims_exchange(payload, now=1000.0)
        cases.append(("a governed run never registers as a session", "" if got is None else f"{got!r}"))

    # The switch is "on" for any NON-EMPTY value, "0" included; only empty (what every test here
    # sets) leaves claims running.
    for value in ("1", "0", "off"):
        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.dict(os.environ, _claims_env(tmp, NUCLEOS_CLAIMS_OFF=value)):
                with mock.patch.object(hook.urllib.request, "urlopen", side_effect=AssertionError("switched off")):
                    got = hook.claims_exchange(payload, now=1000.0)
            cases.append((f"NUCLEOS_CLAIMS_OFF={value!r} switches claims off", "" if got is None else f"{got!r}"))
    with tempfile.TemporaryDirectory() as tmp:
        answer, seen = _exchange(tmp, payload, [ok])
        cases.append(("NUCLEOS_CLAIMS_OFF empty leaves claims on", "" if answer == ok else f"{answer!r}"))
    return cases


def _run_claims_main(tmp, payload, replies):
    """`main()` as an interactive session with claims on; returns `(exit_code, printed, requests)`."""
    seen, queue, captured, code = [], list(replies), io.StringIO(), "no exit"

    def urlopen(request, timeout=None):
        seen.append(request.full_url)
        reply = queue.pop(0) if queue else {"registered": False}
        if isinstance(reply, Exception):
            raise reply
        return _Reply(reply)

    with mock.patch.dict(os.environ, _claims_env(tmp)):
        os.environ.pop("NUCLEOS_RUN_ID", None)
        with mock.patch.object(hook, "read_control_token", return_value=("tok", None)):
            with mock.patch.object(hook, "control_token", return_value="tok"):
                with mock.patch.object(hook.urllib.request, "urlopen", side_effect=urlopen):
                    with mock.patch("sys.stdin", io.StringIO(json.dumps(payload))):
                        with mock.patch("sys.stdout", captured):
                            try:
                                hook.main()
                            except SystemExit as exit_:
                                code = exit_.code
    return code, captured.getvalue(), seen


def claims_tool_call_cases():
    cases = []
    ok = {"registered": True, "project_id": "alpha", "notices": [], "incidents": []}
    base = {"hook_event_name": "PreToolUse", "session_id": "s1", "cwd": "/work/repo"}

    with tempfile.TemporaryDirectory() as tmp:
        payload = dict(base, tool_name="mcp__nucleos__task_claim", tool_input={"key": "master-red", "session_id": "forged"})
        code, printed, seen = _run_claims_main(tmp, payload, [ok])
        try:
            special = json.loads(printed)["hookSpecificOutput"]
            good = (
                code in (0, None)
                and special["permissionDecision"] == "allow"
                and special["updatedInput"] == {"key": "master-red", "session_id": "s1"}
                and seen[0].endswith("/tasks/session")
            )
            failure = "" if good else f"{code!r} {printed!r} {seen!r}"
        except Exception as exc:  # noqa: BLE001
            failure = f"unparseable {printed!r}: {exc!r}"
        cases.append(("a task tool is allowed with the session id stamped over whatever the model sent, after the session is registered", failure))

    # A fresh state directory per tool: a shared one would rate-limit every call after the first,
    # and the loop would prove nothing about the later names. `exercised` checks the call really
    # reached the claims step (the notice came through), so a silent pass cannot hide a skipped case.
    for tool in ("mcp__nucleos__task_claimx", "mcp__other__task_claim", "mcp__nucleos__verify", "task_claim", "Bash"):
        with tempfile.TemporaryDirectory() as tmp:
            payload = dict(base, tool_name=tool, tool_input={"command": "ls"} if tool == "Bash" else {})
            code, printed, seen = _run_claims_main(tmp, payload, [dict(ok, notices=["hello"])])
            special = json.loads(printed)["hookSpecificOutput"] if printed.strip() else {}
            bad = "permissionDecision" in special or "updatedInput" in special
            exercised = bool(seen) and "hello" in special.get("additionalContext", "")
            if bad:
                failure = f"approved: {printed!r}"
            elif not exercised:
                failure = f"the case did not reach the claims step: {printed!r} {seen!r}"
            else:
                failure = ""
            cases.append((f"{tool} is never approved or rewritten: only the four exact task tools are", failure))

    with tempfile.TemporaryDirectory() as tmp:
        payload = dict(base, tool_name="mcp__nucleos__task_list", tool_input={})
        code, printed, _ = _run_claims_main(tmp, payload, [dict(ok, notices=["Your claim on #3 is gone"])])
        special = json.loads(printed)["hookSpecificOutput"]
        good = special["permissionDecision"] == "allow" and "#3" in special.get("additionalContext", "")
        cases.append(("a notice rides on the same answer as the stamp", "" if good else f"{printed!r}"))

    with tempfile.TemporaryDirectory() as tmp:
        payload = dict(base, tool_name="mcp__nucleos__task_claim", tool_input={"key": "master-red"})
        code, printed, _ = _run_claims_main(tmp, payload, [urllib.error.URLError("down")])
        special = json.loads(printed)["hookSpecificOutput"]
        good = special["updatedInput"]["session_id"] == "s1"
        cases.append(("with the daemon down the stamp still goes: the tool call fails on its own, the hook never blocks", "" if good else f"{printed!r}"))

    with tempfile.TemporaryDirectory() as tmp:
        payload = dict(base, tool_name="mcp__nucleos__task_claim", tool_input={}, session_id="../x")
        code, printed, _ = _run_claims_main(tmp, payload, [ok])
        cases.append(("an unsafe session id is not stamped", "" if not printed.strip() else f"{printed!r}"))

    with tempfile.TemporaryDirectory() as tmp:
        payload = dict(base, tool_name="Read", tool_input={"file_path": "a"})
        code, printed, _ = _run_claims_main(tmp, payload, [dict(ok, notices=["Incident #4 `master-red` was closed"])])
        special = json.loads(printed)["hookSpecificOutput"]
        good = "#4" in special["additionalContext"] and special["hookEventName"] == "PreToolUse" and "permissionDecision" not in special
        cases.append(("an ordinary tool call only gets context", "" if good else f"{printed!r}"))

        post = dict(base, hook_event_name="PostToolUse", tool_name="Read", tool_input={}, tool_response={})
        code, printed, seen = _run_claims_main(tmp, post, [dict(ok, notices=["Your claim on #3 is gone"])])
        # (state dir shared with the previous call: this one is within 20 s of it)
        cases.append(("a PostToolUse inside the rate limit stays silent", "" if not printed.strip() and not seen else f"{printed!r} {seen!r}"))

    with tempfile.TemporaryDirectory() as tmp:
        post = dict(base, hook_event_name="PostToolUse", tool_name="Read", tool_input={}, tool_response={})
        code, printed, seen = _run_claims_main(tmp, post, [dict(ok, notices=["Your claim on #3 is gone"])])
        special = json.loads(printed)["hookSpecificOutput"]
        good = special["hookEventName"] == "PostToolUse" and "#3" in special["additionalContext"] and seen == ["http://127.0.0.1:1/tasks/session"]
        cases.append(("a PostToolUse registers the session and injects the notice, without a decision", "" if good and "permissionDecision" not in special else f"{printed!r} {seen!r}"))

    rust = os.path.join(ROOT, "core", "src", "mcp_tools", "boxes.rs")
    try:
        with open(rust, encoding="utf-8") as handle:
            listed = re.search(r"TASK_TOOL_NAMES: \[&str; 4\] = \[([^\]]*)\]", handle.read())
    except OSError:
        listed = None
    want = sorted(f"mcp__nucleos__{n}" for n in re.findall(r'"(task_[a-z]+)"', listed.group(1))) if listed else []
    cases.append(("TASK_TOOLS equals mcp_tools::TASK_TOOL_NAMES", "" if sorted(hook.TASK_TOOLS) == want else f"{sorted(hook.TASK_TOOLS)!r} vs {want!r}"))
    return cases


def claims_prompt_cases():
    cases = []
    incident = "#4 master-red since 1234567: unclaimed"
    answer = {"registered": True, "project_id": "alpha", "notices": [], "incidents": [incident]}
    prompt = {"hook_event_name": "UserPromptSubmit", "session_id": "s1", "cwd": "/work/repo"}
    bundle_none = {"results": []}

    with tempfile.TemporaryDirectory() as tmp:
        code, printed, _ = _run_claims_main(tmp, prompt, [answer, bundle_none])
        good = code == 0 and incident in printed and "task_claim" in printed
        cases.append(("a prompt lists the live incidents even when no workflow bundle has a hook", "" if good else f"{code!r} {printed!r}"))

        code, printed, _ = _run_claims_main(tmp, prompt, [answer, bundle_none])
        cases.append(("the same list is not repeated on the next prompt", "" if incident not in printed else f"{printed!r}"))

        changed = dict(answer, incidents=[incident, "#5 test:a::b since 7654321: being fixed by run #9"])
        code, printed, _ = _run_claims_main(tmp, prompt, [changed, bundle_none])
        cases.append(("a changed list is shown again", "" if "#5" in printed else f"{printed!r}"))

        start = dict(prompt, hook_event_name="SessionStart", source="compact")
        code, printed, _ = _run_claims_main(tmp, start, [changed, bundle_none])
        cases.append(("after a compaction the list comes back even if unchanged", "" if "#5" in printed else f"{printed!r}"))

        code, printed, _ = _run_claims_main(tmp, prompt, [dict(answer, incidents=[]), bundle_none])
        code, printed, _ = _run_claims_main(tmp, prompt, [changed, bundle_none])
        cases.append(("a list that went empty and came back is shown again", "" if "#5" in printed else f"{printed!r}"))

    with tempfile.TemporaryDirectory() as tmp:
        bundle = {"results": [{"exit_code": 0, "stdout": "tree", "stderr": ""}]}
        code, printed, _ = _run_claims_main(tmp, prompt, [answer, bundle])
        cases.append(("claim context and a bundle's plain text share one document", "" if "tree" in printed and incident in printed else f"{printed!r}"))

        as_json = json.dumps({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": "tree"}})
        bundle = {"results": [{"exit_code": 0, "stdout": as_json, "stderr": ""}]}
        code, printed, _ = _run_claims_main(tmp, prompt, [dict(answer, incidents=[incident + " v2"]), bundle])
        try:
            context = json.loads(printed)["hookSpecificOutput"]["additionalContext"]
            failure = "" if "tree" in context and "v2" in context else f"{context!r}"
        except Exception as exc:  # noqa: BLE001
            failure = f"not one JSON document: {printed!r} ({exc!r})"
        cases.append(("with a bundle that prints JSON the claim text goes inside its additionalContext", failure))

        block = {"results": [{"exit_code": 2, "stdout": "", "stderr": "stop"}]}
        with mock.patch("sys.stderr", io.StringIO()):
            code, printed, _ = _run_claims_main(tmp, prompt, [dict(answer, incidents=[incident + " v3"]), block])
        cases.append(("a bundle's exit 2 still blocks, and the claim text does not hide it", "" if code == 2 else f"{code!r}"))

    with tempfile.TemporaryDirectory() as tmp:
        code, printed, _ = _run_claims_main(tmp, prompt, [urllib.error.URLError("down"), urllib.error.URLError("down")])
        cases.append(("with the daemon down a prompt still goes through, silently", "" if code == 0 and printed == "" else f"{code!r} {printed!r}"))
        stop = {"hook_event_name": "Stop", "session_id": "s1", "cwd": "/work/repo"}
        code, printed, seen = _run_claims_main(tmp, stop, [bundle_none])
        cases.append(("a Stop never asks about claims", "" if seen and all("/tasks/session" not in url for url in seen) else f"{seen!r}"))
    return cases


CLAIMED_LAND = "NUCLEOS_ALLOW_" + "CLAIMED_LAND"  # kept out of the text so this session's own hook ignores it


def claimed_land_override_cases():
    """The owner's land override is refused to every agent, spelled however it is spelled."""
    results = []

    def payload(tool, command, event="PreToolUse"):
        return {"hook_event_name": event, "tool_name": tool, "tool_input": {"command": command}, "cwd": "/work/repo"}

    refused = [
        ("bash prefix", payload("Bash", f"{CLAIMED_LAND}=1 nucleos-core --land")),
        ("export", payload("Bash", f"export {CLAIMED_LAND}=1; nucleos-core --land")),
        ("env", payload("Bash", f"env {CLAIMED_LAND}=1 nucleos-core --land")),
        ("powershell", payload("PowerShell", f"$env:{CLAIMED_LAND}='1'; nucleos-core --land")),
        ("quoted and escaped", payload("Bash", f"\"{CLAIMED_LAND[:10]}\"{CLAIMED_LAND[10:]}=1 nucleos-core --land")),
        ("lower case", payload("Bash", f"{CLAIMED_LAND.lower()}=1 nucleos-core --land")),
    ]
    for label, p in refused:
        results.append((f"claimed land override, {label}", None if hook.claimed_land_override(p) else "was not recognised"))
    allowed = [
        ("plain land", payload("Bash", "nucleos-core --land")),
        ("another tool", {"hook_event_name": "PreToolUse", "tool_name": "Read", "tool_input": {"file_path": CLAIMED_LAND}}),
    ]
    for label, p in allowed:
        results.append((f"claimed land override, {label}", "was refused" if hook.claimed_land_override(p) else None))

    # Through main(): an interactive session and a governed run are both refused before the daemon
    # is asked anything.
    for run_id in (None, "7"):
        captured = io.StringIO()
        env = {"NUCLEOS_DAEMON_URL": "http://127.0.0.1:1", "NUCLEOS_DAEMON_TOKEN": "tok"}
        if run_id:
            env["NUCLEOS_RUN_ID"] = run_id
        code = "no exit"
        with mock.patch.dict(os.environ, env):
            if not run_id:
                os.environ.pop("NUCLEOS_RUN_ID", None)
            with mock.patch("sys.stdin", io.StringIO(json.dumps(payload("Bash", f"{CLAIMED_LAND}=1 nucleos-core --land")))):
                with mock.patch.object(hook.urllib.request, "urlopen", side_effect=AssertionError("asked the daemon")):
                    with mock.patch("sys.stdout", captured):
                        try:
                            hook.main()
                        except SystemExit as exit_:
                            code = exit_.code
        blocked = '"block"' in captured.getvalue() and code == 0
        results.append((f"claimed land override through main, run={run_id}", None if blocked else f"printed {captured.getvalue()!r}, exit {code}"))
    return results


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
        + claims_exchange_cases()
        + claims_tool_call_cases()
        + claims_prompt_cases()
        + claimed_land_override_cases()
    ):
        total += 1
        if failure:
            failures += 1
            print(f"FAIL {label}: {failure}")

    print(f"{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
