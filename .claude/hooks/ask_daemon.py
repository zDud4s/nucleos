#!/usr/bin/env python3
"""Production PreToolUse hook for NucleOS-managed autopilot runs.

Interactive/human sessions are identified by the absence of NUCLEOS_RUN_ID and
are always allowed without contacting the daemon. Autopilot runs are identified
by NUCLEOS_RUN_ID and must also provide NUCLEOS_DAEMON_URL and
NUCLEOS_DAEMON_TOKEN; their tool calls are daemon-gated and fail closed.

This intentionally differs from the proof-of-concept fixture: unconfigured here
means an ordinary interactive session, not an autopilot run that may be gated.
"""

import json
import os
import sys
import urllib.request


def allow() -> None:
    print(json.dumps({"decision": "allow"}))
    sys.exit(0)


def deny(reason: str) -> None:
    # A block is a deliberate hook decision, not a script crash, so exit zero.
    print(json.dumps({"decision": "block", "reason": reason}))
    sys.exit(0)


def main() -> None:
    run_id_raw = os.environ.get("NUCLEOS_RUN_ID")
    if run_id_raw is None:
        # Autopilot never gates ordinary interactive/human Claude Code sessions.
        allow()

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
        print(json.dumps({"decision": "block", "reason": decision.get("reason", "")}))
        sys.exit(0)
    if verdict != "allow":
        deny(f"daemon returned an unrecognized decision {verdict!r} - failing closed")
    allow()


if __name__ == "__main__":
    main()
