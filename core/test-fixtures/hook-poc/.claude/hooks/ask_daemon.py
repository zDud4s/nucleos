#!/usr/bin/env python3
import json
import os
import sys
import urllib.request


def deny(reason: str):
    # Fail closed: print a block decision and exit 0, matching the same "block" contract the
    # daemon itself uses (Step 1's `pretooluse_decision`) — this is not a script crash, it's the
    # deliberate default when the thing that's supposed to make the real decision can't be reached.
    print(json.dumps({"decision": "block", "reason": reason}))
    sys.exit(0)


def main():
    try:
        payload = json.load(sys.stdin)
    except Exception as e:
        deny(f"failed to parse hook payload from stdin ({e}) — failing closed")
    if not isinstance(payload, dict):
        deny("hook payload from stdin is not a JSON object — failing closed")

    daemon_url = os.environ.get("NUCLEOS_DAEMON_URL")
    token = os.environ.get("NUCLEOS_DAEMON_TOKEN")
    if not daemon_url or not token:
        # Local-dev escape hatch: NUCLEOS_HOOK_ALLOW_UNCONFIGURED=1 explicitly opts into fail-open
        # behavior when no daemon is running at all (e.g. poking at this hook fixture by hand,
        # outside a nucleos-spawned run). This must be an explicit opt-in — never the silent
        # default — or every "daemon isn't up" case silently becomes "allow everything".
        if os.environ.get("NUCLEOS_HOOK_ALLOW_UNCONFIGURED") == "1":
            sys.exit(0)
        deny(
            "NUCLEOS_DAEMON_URL/NUCLEOS_DAEMON_TOKEN not set — failing closed "
            "(set NUCLEOS_HOOK_ALLOW_UNCONFIGURED=1 to opt into fail-open for local dev without a daemon)"
        )

    try:
        # run_id == runs.id (spec §3.3); the core validates it against in-flight runs before acting.
        run_id = int(os.environ.get("NUCLEOS_RUN_ID", "0"))
    except ValueError:
        deny("NUCLEOS_RUN_ID is not an integer — failing closed")

    body = json.dumps({
        "run_id": run_id,
        "tool_name": payload.get("tool_name", ""),
        "tool_input": payload.get("tool_input", {}),
    }).encode()
    req = urllib.request.Request(
        f"{daemon_url}/hooks/pretooluse-decision",
        data=body,
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            # urlopen raises urllib.error.HTTPError for any non-2xx response, which lands in the
            # except clause below — reaching this line means the daemon answered with a 2xx.
            decision = json.loads(resp.read())
    except Exception as e:
        # Connection refused, timeout, non-2xx status, malformed JSON response — all of it fails
        # closed here, not just the "daemon offline" case above.
        deny(f"daemon unreachable or errored ({e}) — failing closed")

    if not isinstance(decision, dict):
        # A 2xx body that parses as valid JSON but isn't an object (array/string/number) would make
        # `decision.get(...)` raise below — an uncaught exception here fails *open*, exactly the hole
        # this script exists to close. Guard it explicitly.
        deny("daemon returned a non-object decision body — failing closed")

    verdict = decision.get("decision")
    if verdict in ("deny", "pending_approval"):
        # Both are "block" to the CLI (spec §3.3): the core has already acted on the difference — a
        # "pending_approval" also terminated this run into awaiting_approval on the daemon side.
        print(json.dumps({"decision": "block", "reason": decision.get("reason", "")}))
        sys.exit(0)
    if verdict != "allow":
        # A 2xx response whose body isn't an explicit "allow" (or the deny/pending_approval pair
        # above) is treated like daemon-unreachable: fail closed rather than defaulting to "allow" on
        # anything unrecognized (a future daemon response-shape change should never silently become
        # fail-open).
        deny(f"daemon returned an unrecognized decision {verdict!r} — failing closed")
    sys.exit(0)

if __name__ == "__main__":
    main()
