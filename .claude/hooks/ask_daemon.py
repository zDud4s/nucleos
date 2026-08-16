#!/usr/bin/env python3
"""Production PreToolUse hook for NucleOS sessions — launched by the daemon or opened by hand.

A run (NUCLEOS_RUN_ID present) gets the full safety gate: allows emit the recognized
hookSpecificOutput approval contract, while denials and pending approvals emit the
empirically proven legacy block contract.

An interactive session gets one narrow question instead: is this a git operation the
VCS queue performs? If so it is queued and this call is refused; otherwise this hook
says nothing at all. **It can refuse but it can never approve** — that asymmetry is what
makes it safe to speak here, where the old version had to stay silent. Registered
repo-wide, an allow would auto-approve a person's own tools; a refusal grants nothing.

That silence was a real hole rather than a conservative default. The queue exists to
order git operations between sessions, and the sessions doing most of the work are the
ones a person opens in an editor. Measured: an editor session asked to `git merge master`
merged, with no hook, no proposal and no row.

All malformed-input, configuration, and daemon errors still fail closed.
"""

import json
import os
import subprocess
import sys
import urllib.request

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


def control_token(cwd: str) -> str:
    """The daemon's own token, read the way the desktop app reads it.

    An editor session inherits no NucleOS environment, so the token has to be fetched
    rather than found. It lives in Credential Manager under the person's own account,
    which is exactly who is sitting here — this grants nothing the session did not
    already have, it only stops the session having to be told how.

    The binary is located from the repository rather than from PATH: a linked worktree
    has no `target/` of its own, but `--git-common-dir` names the main checkout from
    inside any of them.
    """
    from_env = os.environ.get("NUCLEOS_DAEMON_TOKEN")
    if from_env:
        return from_env

    common = subprocess.run(
        ["git", "-C", cwd, "rev-parse", "--path-format=absolute", "--git-common-dir"],
        capture_output=True, text=True, timeout=10,
    )
    if common.returncode != 0:
        deny("could not locate the repository to find the daemon binary - failing closed")
    main_root = os.path.dirname(common.stdout.strip())

    for build in ("debug", "release"):
        binary = os.path.join(main_root, "target", build, "nucleos-core.exe")
        if os.path.exists(binary):
            printed = subprocess.run(
                [binary, "--print-token"], capture_output=True, text=True, timeout=20
            )
            if printed.returncode == 0 and printed.stdout.strip():
                return printed.stdout.strip()
    deny(
        "this is a git operation the queue performs, and the daemon token could not be "
        "read to queue it - failing closed. Run it from a terminal if you meant to act "
        "as yourself rather than through an agent."
    )


def read_payload(silent_on_error: bool) -> dict:
    try:
        payload = json.load(sys.stdin)
    except Exception as exc:
        if silent_on_error:
            no_opinion()
        deny(f"failed to parse hook payload from stdin ({exc}) - failing closed")
    if not isinstance(payload, dict):
        if silent_on_error:
            no_opinion()
        deny("hook payload from stdin is not a JSON object - failing closed")
    return payload


def interactive_session(payload: dict) -> None:
    """A session a person opened. Refuses queue operations; says nothing about anything else."""
    if payload.get("tool_name") not in ("Bash", "PowerShell"):
        no_opinion()
    command = (payload.get("tool_input") or {}).get("command")
    if not isinstance(command, str):
        no_opinion()

    if not governed_git_verb(command):
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
    request = urllib.request.Request(
        f"{daemon_url}/hooks/session-git-decision",
        data=body,
        headers={
            "Authorization": f"Bearer {control_token(cwd)}",
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
    if decision.get("decision") == "allow":
        # Never re-emitted as an approval. The daemon means "not mine to govern", and the
        # session's ordinary permissions decide from here.
        no_opinion()
    deny(decision.get("reason", "this git operation belongs to the queue"))


def main() -> None:
    run_id_raw = os.environ.get("NUCLEOS_RUN_ID")
    if run_id_raw is None:
        # Parsed inside, and unparseable input is silence there rather than a refusal. The two
        # branches fail closed in different directions on purpose: for a run, an unreadable
        # payload means the gate cannot see what it is governing and must stop everything. For a
        # person's session it means only that we cannot tell whether this was git — and refusing
        # every tool call in someone's editor, over a payload the editor itself wrote, would take
        # the session down to protect a guarantee that was never at risk.
        interactive_session(read_payload(silent_on_error=True))

    try:
        payload = json.load(sys.stdin)
    except Exception as exc:
        deny(f"failed to parse hook payload from stdin ({exc}) - failing closed")
    if not isinstance(payload, dict):
        deny("hook payload from stdin is not a JSON object - failing closed")

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
    if verdict in ("deny", "pending_approval"):
        deny(decision.get("reason", ""))
    if verdict != "allow":
        deny(f"daemon returned an unrecognized decision {verdict!r} - failing closed")
    approve(decision.get("reason", "autopilot: allowed"))


if __name__ == "__main__":
    main()
