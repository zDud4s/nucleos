#!/usr/bin/env python3
"""Drive `auto-approve.py` against a stub daemon.

Wired into `scripts/gates.sh` as `eval: approver`, in the hooks leg. (Corrected 2026-09-09: this
said the opposite — that wiring it in would put Python in the definition of green for everyone —
and it has been in the gate for long enough that the sentence sent whoever read it looking for a
decision nobody still holds. The hooks leg runs six Python suites.) Also runs standalone:
`python scripts/eval/test-auto-approve.py`.

It is here rather than thrown away because it earned it. The approver looked correct and was not:
a proposal answered with 409 stays pending, so the loop re-answered it every poll and spent the
entire `--max` ceiling on the one row that could never move. Reading the code did not show that;
running it against a stub that keeps answering did.

A stub rather than the real daemon on purpose. The live one has no pending proposals, and
fabricating one means writing into the owner's database — for a test of a script whose whole
contract is "touch only the runs you were named".
"""

import json
import os
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))

# The approver is handed seconds of work and finishes in about one. This ceiling is not for slow
# machines; it is for the approver never returning at all. Sandbox spike 0a hit exactly that: with
# the stub's listener outside the permitted port range the connect was refused, nothing in the
# approver treated that as an error, and the run sat silent until the runner's own 15-minute
# ceiling killed it. The other 87 ephemeral-port sites in this repo fail with an exit code somebody
# can read; this one printed nothing at all. A test that hangs instead of failing is a defect on
# its own terms, whatever is decided about the port range. Raise it with the environment variable
# when a machine genuinely needs longer.
TIMEOUT_S = float(os.environ.get("NUCLEOS_APPROVE_TEST_TIMEOUT", "60"))

# One of each thing the filter has to tell apart: the run we asked about, another run entirely, a
# different KIND sharing the same table, one already decided, and one that cannot be decided at all.
PROPOSALS = [
    {"id": 1, "kind": "action-approval", "status": "pending", "run_id": 111,
     "tool_name": "Bash", "reasoning": "wants to run cargo", "tool_input": "{}", "created_at": "t"},
    {"id": 2, "kind": "action-approval", "status": "pending", "run_id": 222,
     "tool_name": "Bash", "reasoning": "another run entirely", "tool_input": "{}", "created_at": "t"},
    {"id": 3, "kind": "contact-merge", "status": "pending", "run_id": 111,
     "tool_name": None, "reasoning": "not an action approval", "tool_input": None, "created_at": "t"},
    {"id": 4, "kind": "action-approval", "status": "decided", "run_id": 111,
     "tool_name": "Bash", "reasoning": "already answered", "tool_input": "{}", "created_at": "t"},
    {"id": 5, "kind": "action-approval", "status": "pending", "run_id": 111,
     "tool_name": "Write", "reasoning": "wants to write a file", "tool_input": "{}", "created_at": "t"},
]

approved = []
unparseable = []


class Stub(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def _send(self, code, body):
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        # Deliberately unchanging: a real daemon flips a proposal to `decided`, and a stub that did
        # the same would hide the retry bug this test exists for.
        self._send(200, json.dumps(PROPOSALS).encode())

    def do_POST(self):
        proposal_id = int(self.path.split("/")[2])
        approved.append(proposal_id)
        # As the daemon does: a JSON content type over a body that does not parse is a 400, and an
        # empty body is exactly that. A stub that ignored the body let the approver send one, and
        # the first real cell to need an approval sat in `awaiting_approval` because of it.
        raw = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        try:
            json.loads(raw)
        except ValueError:
            unparseable.append(proposal_id)
            self._send(400, b"Failed to parse the request body as JSON")
            return
        # 5 answers 409 — the normal race, and the one that stays pending for ever.
        if proposal_id == 5:
            self._send(409, b'{"error":"not pending"}')
        else:
            self._send(200, b'{"ok":true}')


def main() -> int:
    # Port 0: the OS picks a free one. A fixed port makes a gate fail for the one reason that has
    # nothing to do with the code — somebody else already listening.
    server = HTTPServer(("127.0.0.1", 0), Stub)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()

    log = os.path.join(tempfile.mkdtemp(prefix="nucleos-approve-"), "approvals.jsonl")
    try:
        result = subprocess.run(
            [sys.executable, os.path.join(ROOT, "scripts/eval/auto-approve.py"),
             "--run", "111", "--daemon-url", f"http://127.0.0.1:{port}",
             "--until-idle", "1", "--interval", "0.1", "--log", log],
            capture_output=True, text=True, cwd=ROOT,
            # The stub never looks at the Authorization header, but the approver reads a token
            # before it does anything at all, and reads it out of a built daemon. Handing it one
            # here is what keeps this test hermetic — see control_token in auto-approve.py.
            env={**os.environ, "NUCLEOS_DAEMON_TOKEN": "stub"},
            timeout=TIMEOUT_S,
        )
    except subprocess.TimeoutExpired as expired:
        server.shutdown()
        # Print enough to tell the two causes apart. An approver stuck in its own poll loop and an
        # approver that cannot reach the stub at all look identical from out here, and which one it
        # is decides where to look next: whether any POST arrived is the discriminator.
        print(f"FAIL the approver did not return within {TIMEOUT_S:g}s and was killed")
        print(f"     stub was listening on 127.0.0.1:{port}; "
              f"proposals it saw answered: {approved or 'none'}")
        for name in ("stdout", "stderr"):
            captured = getattr(expired, name) or ""
            if isinstance(captured, bytes):
                captured = captured.decode("utf-8", errors="replace")
            if captured.strip():
                print(f"     {name} tail: {captured.strip()[-400:]}")
        return 1
    server.shutdown()

    print(result.stdout.strip())
    if result.stderr.strip():
        print("STDERR:", result.stderr.strip()[:400])

    # Said here rather than left to `open(log)`: when the approver exits before writing a line,
    # the traceback names this file and a missing temp path, which describes neither the failure
    # nor the place to look for it.
    if not os.path.exists(log):
        print(f"FAIL the approver wrote no log (exit {result.returncode}); nothing to check")
        return 1

    records = [json.loads(line) for line in open(log, encoding="utf-8")]
    failures = 0

    def check(label, condition):
        nonlocal failures
        print(("ok   " if condition else "FAIL ") + label)
        failures += 0 if condition else 1

    check("only the named run's pending action-approvals were called", approved == [1, 5])
    check("another run was never touched", 2 not in approved)
    check("a contact-merge was not answered", 3 not in approved)
    check("an already-decided proposal was not re-answered", 4 not in approved)
    check("an un-answerable proposal was tried once, not every poll", approved.count(5) == 1)
    check("both answers were recorded", len(records) == 2)
    check("the classifier's reasoning survives into the record",
          records[0]["reasoning"] == "wants to run cargo")
    check("a 409 is recorded as failed rather than swallowed",
          "failed" in records[1] and "409" in records[1]["failed"])
    check("the exit was clean", result.returncode == 0)
    check("every approval carries a body the daemon can parse", unparseable == [])

    total = 10
    print(f"\n{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
