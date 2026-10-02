#!/usr/bin/env python3
"""Production hook for NucleOS sessions — launched by the daemon or opened by hand.

Registered under three events (`autopilot.rs`'s `wire_classifier_hook`), told apart by the
payload's own `hook_event_name` rather than by anything the invocation says, because the same
command is wired to all three:

A run (NUCLEOS_RUN_ID present) gets the full safety gate on `PreToolUse`: allows emit the
recognized hookSpecificOutput approval contract, while denials and pending approvals emit the
empirically proven legacy block contract.

An interactive session gets one narrow question instead: is this a git operation the
VCS queue performs? If so it is queued and this call is refused; otherwise this hook
says nothing at all. **It can refuse but it can never approve** — that asymmetry is what
makes it safe to speak here, where the old version had to stay silent. Registered
repo-wide, an allow would auto-approve a person's own tools; a refusal grants nothing.
This branch may also add context (`additionalContext`) telling the agent that a vcs request
it was refused for has settled, once per request; that carries no permission decision either,
and it is silent on every error.

That silence was a real hole rather than a conservative default. The queue exists to
order git operations between sessions, and the sessions doing most of the work are the
ones a person opens in an editor. Measured: an editor session asked to `git merge master`
merged, with no hook, no proposal and no row.

`PostToolUse`/`PostToolUseFailure` report the OUTCOME of a call this hook already judged, and
never block anything: by the time either fires the tool has already run (or already failed), so
a refusal here would be a barrier with nothing left to stop — an expensive warning, not a
protection. `report_outcome` is therefore silent on every failure of its own: an unreachable
daemon, a missing token, a payload it cannot use. Never `deny()`, never `approve()`.

All malformed-input, configuration, and daemon errors still fail closed ON THE PreToolUse PATH —
the one path where failing open would matter.
"""

import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

# Statuses after which a vcs request will not change again.
SETTLED_STATUSES = (
    "succeeded", "failed", "blocked", "rejected", "cancelled", "interrupted", "escalated",
)
PENDING_DIR_ENV = "NUCLEOS_VCS_PENDING_DIR"
# An id still unsettled this long is dropped, and the file is not re-checked more often than
# the second figure, so a burst of tool calls costs one check rather than one per call.
PENDING_TTL_SECONDS = 30 * 60
PENDING_RECHECK_SECONDS = 15
SESSION_ID_SHAPE = re.compile(r"[A-Za-z0-9_-]{1,128}")

# A deliberately over-broad local filter, and it must never be the authority. Its only job is
# to keep the fast path free: this hook runs in front of every tool call in every editor
# session, and asking the daemon each time would spend git subprocesses on keystrokes. What
# passes here is asked properly; what does not was never a queue operation under any spelling.
#
# `branch` is in the list for `branch -d` and lets `branch --list` through to the daemon, which
# declines it. That is the right way round: over-including costs one HTTP call, under-including
# costs the guarantee.
QUEUE_SUBCOMMANDS = ("merge", "push", "tag", "fetch", "branch", "rebase")

# Where one command word can end and another begin, in sh or in PowerShell. Normalized to spaces
# before the scan below, so `cd x&&git push` splits with no spaces in it anywhere.
SEGMENT_SEPARATORS = "\n\r;|&()`{}"

# Global flags that swallow the token after them. Everything else starting with `-` is either a
# valueless flag or a `--flag=value`, and skipping one token is right for both.
GIT_FLAGS_WITH_VALUES = ("-C", "-c", "--git-dir", "--work-tree", "--namespace", "--exec-path")

DEFAULT_DAEMON_URL = "http://127.0.0.1:8791"


def governed_git_verb(command: str) -> str:
    """The queue verb this command would run, or `""`.

    Scans the WHOLE command, not its first two words. The first version of this filter read
    `tokens[0]` and `tokens[1]` and so governed a command only when it literally began `git
    <verb>` — which `cd repo\\ngit push`, `true && git push`, and any leading `echo` walked
    straight past into silence. That is the one direction this filter must not fail in, and it
    failed in it: the file's own comment above says under-including costs the guarantee, while
    the code under it under-included.

    Deliberately over-broad, as that comment asks. `echo "run git push later"` matches and is
    refused, and a quoted mention is the price of not needing a shell parser to be sure. The cost
    of a false positive is one refusal a person can reword; the cost of a false negative is the
    queue's whole promise, silently.
    """
    normalized = command
    for separator in SEGMENT_SEPARATORS:
        normalized = normalized.replace(separator, " ")
    tokens = [token.strip("\"'") for token in normalized.split()]

    for index, token in enumerate(tokens):
        # `git`, `/usr/bin/git`, `C:\Program Files\Git\bin\git.exe` — the name it was invoked by
        # says nothing about what it does.
        name = token.replace("\\", "/").rsplit("/", 1)[-1].lower()
        if name not in ("git", "git.exe"):
            continue
        rest = index + 1
        while rest < len(tokens):
            candidate = tokens[rest]
            if candidate in GIT_FLAGS_WITH_VALUES:
                rest += 2
                continue
            if candidate.startswith("-"):
                rest += 1
                continue
            if candidate.lower() in QUEUE_SUBCOMMANDS:
                return candidate.lower()
            # A git call that is not a queue operation. Keep looking: a command may hold more
            # than one, and the second is as governed as the first.
            break
    return ""


def approve(reason: str = "autopilot: allowed") -> None:
    print(
        json.dumps(
            {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "permissionDecisionReason": reason,
                }
            }
        )
    )
    sys.exit(0)


def no_opinion() -> None:
    sys.exit(0)


def deny(reason: str) -> None:
    # A block is a deliberate hook decision, not a script crash, so exit zero.
    print(json.dumps({"decision": "block", "reason": reason}))
    sys.exit(0)


def configured_target_dir(path):
    """`build.target-dir` out of one cargo config file, or None if it says nothing.

    Parsed rather than grepped. The config this exists for explains the setting in a
    comment that itself contains `CARGO_TARGET_DIR = "..."`, so a regex over the file
    finds the documentation before it finds the setting. Every failure -- no file, no
    `tomllib`, malformed TOML -- is None: this only ever ADDS places to look, so a
    reader that gives up leaves the old behaviour exactly as it was.
    """
    try:
        import tomllib

        with open(path, "rb") as handle:
            configured = tomllib.load(handle).get("build", {}).get("target-dir")
        return configured if isinstance(configured, str) and configured else None
    except Exception:
        return None


def cargo_target_dirs(main_root, environ=None, configs=None):
    """Every directory cargo might have built into, in cargo's own precedence order.

    `CARGO_TARGET_DIR` beats `build.target-dir` in a config file, which beats
    `<root>/target`. A LIST and not one resolved answer, because this is looking for a
    file that exists rather than reproducing cargo's decision: a binary left in an older
    location still prints the token, and this must not refuse because it went looking in
    only the newest place.
    """
    environ = os.environ if environ is None else environ
    if configs is None:
        cargo_home = environ.get("CARGO_HOME") or os.path.expanduser("~/.cargo")
        configs = [
            os.path.join(cargo_home, "config.toml"),
            os.path.join(main_root, ".cargo", "config.toml"),
        ]

    roots = []
    from_env = environ.get("CARGO_TARGET_DIR")
    if from_env:
        roots.append(from_env)
    for config in configs:
        configured = configured_target_dir(config)
        if configured:
            roots.append(configured)
    roots.append(os.path.join(main_root, "target"))
    return roots


def daemon_binary_candidates(roots, os_name=None):
    """Every path the daemon binary might have, in the order `control_token` tries them.

    cargo names the binary `nucleos-core.exe` on Windows and `nucleos-core` everywhere else.
    Looking only for the `.exe` failed CLOSED off Windows: no candidate ever existed, so every
    queue operation an agent asked for was refused for a token that could not be read.

    The platform's own name comes first. On Windows that keeps the old list, in the old order, as
    the exact head of this one, so a machine that found its binary before finds the same one now,
    and the extension-less names are tried only after every `.exe` has missed. Off Windows the
    `.exe` names come last; where no such file exists they cost one `os.path.exists` each.
    """
    os_name = os.name if os_name is None else os_name
    if os_name == "nt":
        names = ("nucleos-core.exe", "nucleos-core")
    else:
        names = ("nucleos-core", "nucleos-core.exe")
    return [
        os.path.join(root, build, name)
        for name in names
        for root in roots
        for build in ("debug", "release")
    ]


def control_token(cwd: str) -> str:
    """The daemon's own token, read the way the desktop app reads it.

    An editor session inherits no NucleOS environment, so the token has to be fetched
    rather than found. It lives in the system credential store under the person's own account,
    which is exactly who is sitting here — this grants nothing the session did not
    already have, it only stops the session having to be told how.

    The binary is located from the repository rather than from PATH: a linked worktree
    has no `target/` of its own, but `--git-common-dir` names the main checkout from
    inside any of them.

    Where cargo PUT it is a second question, and assuming `<main_root>/target` is how this
    guard spent a day inert. On a machine whose worktrees share one build tree outside the
    repository there is no `target/` at all, so every merge, push, tag and fetch from every
    agent session was refused for a missing token -- a guard failing closed on its own
    configuration, saying nothing about why. `cargo_target_dirs` mirrors cargo's own
    precedence instead of guessing at one location.
    """
    token, why = read_control_token(cwd)
    if token:
        return token
    if why == "repo":
        deny("could not locate the repository to find the daemon binary - failing closed")
    deny(
        "this is a git operation the queue performs, and the daemon token could not be "
        "read to queue it - failing closed. Run it from a terminal if you meant to act "
        "as yourself rather than through an agent."
    )


def read_control_token(cwd: str, git_timeout=10, print_timeout=20):
    """`(token, why)`: the token or None, and why none (`"repo"` or `"token"`).

    Raises nothing and never denies, so a caller that must stay silent (the follow-up that
    reports a settled request) can use it; `control_token` turns the failure into its denials.
    """
    from_env = os.environ.get("NUCLEOS_DAEMON_TOKEN")
    if from_env:
        return from_env, None

    try:
        common = subprocess.run(
            ["git", "-C", cwd, "rev-parse", "--path-format=absolute", "--git-common-dir"],
            capture_output=True, text=True, timeout=git_timeout,
        )
        if common.returncode != 0:
            return None, "repo"
        main_root = os.path.dirname(common.stdout.strip())

        for binary in daemon_binary_candidates(cargo_target_dirs(main_root)):
            if os.path.exists(binary):
                printed = subprocess.run(
                    [binary, "--print-token"], capture_output=True, text=True,
                    timeout=print_timeout,
                )
                if printed.returncode == 0 and printed.stdout.strip():
                    return printed.stdout.strip(), None
    except Exception:
        return None, "token"
    return None, "token"


def pending_path(session_id):
    """Where this session's unsettled vcs request ids are kept, or None for an unsafe id."""
    try:
        if not isinstance(session_id, str) or not SESSION_ID_SHAPE.fullmatch(session_id):
            return None
        base = os.environ.get(PENDING_DIR_ENV) or os.path.join(
            tempfile.gettempdir(), "nucleos-vcs-pending"
        )
        return os.path.join(base, f"{session_id}.json")
    except Exception:
        return None


def _load_pending(path):
    """Reads the pending file as `(entries, checked)`.

    `entries` is a list of `{"id": N, "since": epoch}`; `checked` is when the file was last
    checked against the daemon (0 when never). The first version of the file was a plain list
    of ints, and a session may still hold one: those ids count as remembered when the file was
    last written.
    """
    with open(path, "r", encoding="utf-8") as handle:
        loaded = json.load(handle)
    try:
        fallback = os.path.getmtime(path)
    except Exception:
        fallback = time.time()
    checked = 0
    items = loaded
    if isinstance(loaded, dict):
        raw_checked = loaded.get("checked")
        if isinstance(raw_checked, (int, float)) and not isinstance(raw_checked, bool):
            checked = raw_checked
        items = loaded.get("ids")
    if not isinstance(items, list):
        return [], checked
    entries = []
    for item in items:
        if isinstance(item, bool):
            continue
        if isinstance(item, int):
            entries.append({"id": item, "since": fallback})
        elif isinstance(item, dict):
            rid, since = item.get("id"), item.get("since")
            if isinstance(rid, int) and not isinstance(rid, bool):
                if not isinstance(since, (int, float)) or isinstance(since, bool):
                    since = fallback
                entries.append({"id": rid, "since": since})
    return entries, checked


def _store_pending(path, entries, checked=0):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = f"{path}.{os.getpid()}.tmp"
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump({"checked": checked, "ids": entries}, handle)
    os.replace(tmp, path)


def remember_pending(session_id, request_id):
    """Adds one request id to the session's pending list. Silent on every failure."""
    try:
        path = pending_path(session_id)
        if path is None or not isinstance(request_id, int) or isinstance(request_id, bool):
            return
        try:
            entries, checked = _load_pending(path)
        except Exception:
            entries, checked = [], 0
        if request_id not in [e["id"] for e in entries]:
            entries.append({"id": request_id, "since": time.time()})
        _store_pending(path, entries, checked)
    except Exception:
        pass


def notice_for(ticket):
    """One sentence for a settled ticket, mirroring the daemon's own wording."""
    try:
        rid = ticket.get("id")
        status = ticket.get("status")
        sha = ticket.get("result_sha") or "an unrecorded commit"
        reason = ticket.get("failure_reason") or "no reason recorded"
        head = f"vcs request #{rid} has settled: "
        if status == "succeeded":
            return (
                head + f"it succeeded and landed at {sha}. Re-read git status and git log "
                "before going on."
            )
        if status == "escalated":
            return (
                head + "the conflict went to a person. Do not resolve it by hand."
            )
        if status == "blocked":
            return (
                head + f"it was refused before it started ({reason}). Nothing changed. "
                "Clear the cause and run the command again."
            )
        if status in ("failed", "interrupted"):
            return (
                head + f"it {status} ({reason}). Check the repository state before "
                "going on."
            )
        if status in ("cancelled", "rejected"):
            return head + f"it was {status}. Nothing was performed."
        return head + f"{status}."
    except Exception:
        return ""


def settled_notice(payload, token=None):
    """Text telling the agent which pending vcs requests have settled, or `""`.

    Silent on every error and makes no network call when nothing is pending. A file checked
    less than `PENDING_RECHECK_SECONDS` ago is left alone, so a burst of tool calls costs one
    check; an id still unsettled after `PENDING_TTL_SECONDS` is dropped with one last notice.
    `token` is an already-read daemon token, so the governed path does not read it twice.
    """
    try:
        path = pending_path(payload.get("session_id"))
        if path is None or not os.path.exists(path):
            return ""
        entries, checked = _load_pending(path)
        if not entries:
            return ""
        now = time.time()
        if 0 <= now - checked < PENDING_RECHECK_SECONDS:
            return ""
        if not token:
            cwd = payload.get("cwd") or os.getcwd()
            token, _why = read_control_token(cwd, 2, 3)
        daemon_url = os.environ.get("NUCLEOS_DAEMON_URL") or DEFAULT_DAEMON_URL
        notices = []
        remaining = []
        for entry in entries:
            rid = entry["id"]
            keep = True
            if token:
                try:
                    request = urllib.request.Request(
                        f"{daemon_url}/vcs/requests/{rid}",
                        headers={"Authorization": f"Bearer {token}"},
                        method="GET",
                    )
                    with urllib.request.urlopen(request, timeout=2) as response:
                        ticket = json.loads(response.read())
                    if isinstance(ticket, dict) and ticket.get("status") in SETTLED_STATUSES:
                        text = notice_for(ticket)
                        if text:
                            notices.append(text)
                            keep = False
                except urllib.error.HTTPError as exc:
                    if exc.code == 404:
                        keep = False
                except Exception:
                    pass
            if keep and now - entry["since"] > PENDING_TTL_SECONDS:
                notices.append(
                    f"vcs request #{rid} has not settled after "
                    f"{PENDING_TTL_SECONDS // 60} minutes; it is still the queue's - do not "
                    "run it again."
                )
                keep = False
            if keep:
                remaining.append(entry)
        if remaining:
            _store_pending(path, remaining, now)
        else:
            os.remove(path)
        return "\n".join(notices)
    except Exception:
        return ""


def tell(text: str) -> None:
    """Adds context to the next turn. Carries no permission decision, so it grants nothing."""
    print(
        json.dumps(
            {"hookSpecificOutput": {"hookEventName": "PreToolUse", "additionalContext": text}}
        )
    )
    sys.exit(0)


def read_stdin_once():
    """Reads and parses stdin exactly ONCE, returning `(payload, reason)`.

    stdin can be read only once, and `main` now has three branches that each need it — the
    outcome report, the interactive filter, and the governed-run gate — with three DIFFERENT
    reactions to a parse failure: always silent, silent only for a person's session, and `deny()`
    for a governed run. Reading here once and handing every branch the same pair lets each apply
    its OWN reaction instead of the reader picking one for all three.

    `reason` is `None` on success. On failure it is the exact message the pre-restructuring code
    used to `deny()` with, preserved so the run branch's `deny(reason)` reads byte-identical to
    what it printed before this function existed.
    """
    try:
        payload = json.load(sys.stdin)
    except Exception as exc:
        return None, f"failed to parse hook payload from stdin ({exc}) - failing closed"
    if not isinstance(payload, dict):
        return None, "hook payload from stdin is not a JSON object - failing closed"
    return payload, None


def report_outcome(payload: dict) -> None:
    """Reports what a tool call did, and never blocks anything.

    By the time `PostToolUse`/`PostToolUseFailure` fires the call has already run (or already
    failed) — there is nothing left here to approve or deny, only something to record. So this
    function is silent on EVERY failure of its own: an unreachable daemon, a missing token, a
    payload it cannot use. It never raises past its own boundary and never calls `deny()` or
    `approve()` — `main` returns straight after calling this, which leaves the process to exit 0
    with nothing printed, exactly like `no_opinion()` without the `sys.exit` neither of them needs
    here.

    `payload` is already a parsed dict — the caller determined the event from it, so by
    construction it always has `hook_event_name`. NUCLEOS_RUN_ID/NUCLEOS_DAEMON_URL/
    NUCLEOS_DAEMON_TOKEN are read fresh rather than trusted from the payload, exactly as the
    `PreToolUse` gate below reads them: the CLI's own payload never carries a run id at all, only
    the environment the daemon spawned this process with does.
    """
    try:
        run_id = int(os.environ.get("NUCLEOS_RUN_ID", ""))
        daemon_url = os.environ.get("NUCLEOS_DAEMON_URL")
        token = os.environ.get("NUCLEOS_DAEMON_TOKEN")
        if not daemon_url or not token:
            return

        body = json.dumps(
            {
                "run_id": run_id,
                "tool_name": payload.get("tool_name", ""),
                "tool_input": payload.get("tool_input", {}),
                "tool_response": payload.get("tool_response", {}),
                "event": payload.get("hook_event_name", ""),
            }
        ).encode()
        request = urllib.request.Request(
            f"{daemon_url}/hooks/posttooluse",
            data=body,
            headers={
                "Authorization": f"Bearer {token}",
                "Content-Type": "application/json",
            },
            method="POST",
        )
        # Three seconds, not `PreToolUse`'s five: a slow outcome report must not delay the NEXT
        # turn, and unlike the gate above there is no verdict on the other end worth waiting for.
        with urllib.request.urlopen(request, timeout=3):
            pass
    except Exception:
        # Every failure is the same non-event: the ledger missed one entry. Nothing here is worth
        # acting on from a hook that cannot block anyway.
        pass


def interactive_session(payload: dict) -> None:
    """A session a person opened. Refuses queue operations; may add context, never approves."""
    command = (payload.get("tool_input") or {}).get("command")
    if (
        payload.get("tool_name") not in ("Bash", "PowerShell")
        or not isinstance(command, str)
        or not governed_git_verb(command)
    ):
        notice = settled_notice(payload)
        if notice:
            tell(notice)
        no_opinion()

    cwd = payload.get("cwd") or os.getcwd()
    daemon_url = os.environ.get("NUCLEOS_DAEMON_URL") or DEFAULT_DAEMON_URL
    body = json.dumps(
        {
            "tool_name": payload.get("tool_name", ""),
            "tool_input": payload.get("tool_input", {}),
            "cwd": cwd,
        }
    ).encode()
    token = control_token(cwd)
    request = urllib.request.Request(
        f"{daemon_url}/hooks/session-git-decision",
        data=body,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            decision = json.loads(response.read())
    except Exception as exc:
        # Fails closed, and the wording matters: with the daemon down there is no queue to
        # order anything, so letting the command through is the one outcome that breaks the
        # guarantee silently.
        deny(f"daemon unreachable or errored ({exc}) - failing closed")

    if not isinstance(decision, dict):
        deny("daemon returned a non-object decision body - failing closed")
    request_id = decision.get("request_id")
    if (
        decision.get("settled") is False
        and isinstance(request_id, int)
        and not isinstance(request_id, bool)
    ):
        remember_pending(payload.get("session_id"), request_id)
    notice = settled_notice(payload, token)
    if decision.get("decision") == "allow":
        # Never re-emitted as an approval. The daemon means "not mine to govern", and the
        # session's ordinary permissions decide from here.
        if notice:
            tell(notice)
        no_opinion()
    reason = decision.get("reason", "this git operation belongs to the queue")
    deny(reason + ("\n\n" + notice if notice else ""))


def main() -> None:
    # Read exactly ONCE — stdin cannot be read twice — and keep the parse error rather than
    # acting on it: which of the three branches below owns this call is decided next, and each
    # reacts to a parse failure in its OWN way.
    payload, parse_error = read_stdin_once()

    # `hook_event_name` is the CLI's own field, not something either the interactive filter or
    # the `PreToolUse` gate below has ever needed to read — until now. This branch runs whatever
    # `NUCLEOS_RUN_ID` says, because a `PostToolUse` outcome fires for the SAME conversation a
    # run's `PreToolUse` calls fire in, and it never blocks: never `deny()`, never `approve()`.
    #
    # An outer JSON parse failure (`payload is None`) leaves `event` unreadable, so a call that
    # was genuinely `PostToolUse`/`PostToolUseFailure` but arrived unparseable falls through to
    # the branches below instead of landing here — see `read_stdin_once`'s docstring. In practice
    # this is unreachable: the CLI's own hook payloads are always well-formed JSON, and this
    # ambiguity only exists for input that should never occur at all.
    event = payload.get("hook_event_name") if payload is not None else None
    if event in ("PostToolUse", "PostToolUseFailure"):
        report_outcome(payload)
        return

    run_id_raw = os.environ.get("NUCLEOS_RUN_ID")
    if run_id_raw is None:
        # Silent on a parse failure, exactly as `read_payload(silent_on_error=True)` used to be:
        # for a person's session, an unreadable payload means only that we cannot tell whether
        # this was git — and refusing every tool call in someone's editor, over a payload the
        # editor itself wrote, would take the session down to protect a guarantee never at risk.
        if payload is None:
            no_opinion()
        interactive_session(payload)

    # For a run, an unreadable payload means the gate cannot see what it is governing and must
    # stop everything — `deny()`, exactly as the un-restructured code did.
    if payload is None:
        deny(parse_error)

    daemon_url = os.environ.get("NUCLEOS_DAEMON_URL")
    token = os.environ.get("NUCLEOS_DAEMON_TOKEN")
    if not daemon_url or not token:
        deny(
            "NUCLEOS_DAEMON_URL/NUCLEOS_DAEMON_TOKEN not set for autopilot run "
            "- failing closed"
        )

    try:
        # run_id == runs.id; the core validates it against in-flight runs before acting.
        run_id = int(run_id_raw)
    except (TypeError, ValueError):
        deny("NUCLEOS_RUN_ID is not an integer - failing closed")

    body = json.dumps(
        {
            "run_id": run_id,
            "tool_name": payload.get("tool_name", ""),
            "tool_input": payload.get("tool_input", {}),
        }
    ).encode()
    request = urllib.request.Request(
        f"{daemon_url}/hooks/pretooluse-decision",
        data=body,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            decision = json.loads(response.read())
    except Exception as exc:
        deny(f"daemon unreachable or errored ({exc}) - failing closed")

    if not isinstance(decision, dict):
        deny("daemon returned a non-object decision body - failing closed")

    verdict = decision.get("decision")
    if verdict == "asking":
        decision = wait_for_answer(daemon_url, token, run_id)
        verdict = decision.get("decision")
    if verdict in ("deny", "pending_approval"):
        deny(decision.get("reason", ""))
    if verdict != "allow":
        deny(f"daemon returned an unrecognized decision {verdict!r} - failing closed")
    approve(decision.get("reason", "autopilot: allowed"))


def wait_for_answer(daemon_url, token, run_id):
    """Waits for the person to answer for a tool call the daemon is holding.

    The gate above answers in five seconds because that is all the time a tool call
    can spare. A call somebody has to say yes to cannot be answered in five seconds,
    so the daemon says `asking` and the waiting happens here instead -- which keeps
    the ordinary call as fast as it was and confines the long timeout to the one
    case that earned it. A daemon that is simply down still fails at the first call,
    in five seconds, exactly as before.

    Fifty seconds against the daemon's own forty-five, so the daemon is what decides
    a question nobody answered rather than this timing out first and saying something
    less useful. Every failure here is a refusal: a tool call that cannot be allowed
    must never be allowed by default.
    """
    body = json.dumps({"run_id": run_id}).encode()
    request = urllib.request.Request(
        f"{daemon_url}/hooks/ask-wait",
        data=body,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=50) as response:
            answered = json.loads(response.read())
    except Exception as exc:
        deny(f"nobody could be asked about this ({exc}) - failing closed")
    if not isinstance(answered, dict):
        deny("daemon returned a non-object answer body - failing closed")
    return answered


if __name__ == "__main__":
    main()
