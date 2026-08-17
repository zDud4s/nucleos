#!/usr/bin/env python3
"""Drive `auto-approve.py` against a stub daemon.

Not wired into `scripts/gates.sh`: that would put Python in the definition of green for everyone,
which is a decision about the gate rather than about this script. Run it by hand after touching
the approver — `python scripts/eval/test-auto-approve.py`.

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
    result = subprocess.run(
        [sys.executable, os.path.join(ROOT, "scripts/eval/auto-approve.py"),
         "--run", "111", "--daemon-url", f"http://127.0.0.1:{port}",
         "--until-idle", "1", "--interval", "0.1", "--log", log],
        capture_output=True, text=True, cwd=ROOT,
    )
    server.shutdown()

    print(result.stdout.strip())
    if result.stderr.strip():
        print("STDERR:", result.stderr.strip()[:400])

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

    total = 9
    print(f"\n{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
