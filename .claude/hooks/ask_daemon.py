#!/usr/bin/env python3
"""Production PreToolUse hook for NucleOS-managed autopilot runs.

Interactive sessions (no NUCLEOS_RUN_ID) emit no output, expressing no opinion.
Autopilot allows emit the recognized hookSpecificOutput approval contract, while
autopilot denials and pending approvals emit the empirically proven legacy block
contract. All malformed-input, configuration, and daemon errors still fail closed.
"""

import json
import os
import sys
import urllib.request


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


def main() -> None:
    run_id_raw = os.environ.get("NUCLEOS_RUN_ID")
    if run_id_raw is None:
        # The spike proved legacy allow was only a non-blocking fall-through. Once
        # registered repo-wide, emitting allow could auto-approve a human's tools;
        # silence is the only safe "no opinion" for interactive sessions.
        no_opinion()

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
