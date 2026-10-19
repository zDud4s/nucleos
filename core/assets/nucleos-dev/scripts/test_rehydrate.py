#!/usr/bin/env python3
"""Tests for rehydrate: the SessionStart hook that re-injects the daemon's work tree.

    python test_rehydrate.py -v
"""

from __future__ import annotations

import http.server
import json
import os
import subprocess
import sys
import threading
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "rehydrate.py"


class Stub(http.server.BaseHTTPRequestHandler):
    log: list = []
    active_status = 200

    def log_message(self, *args):
        pass

    def _send(self, status, body):
        raw = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        Stub.log.append((self.path, self.headers.get("Authorization")))
        if self.path.startswith("/nodes/active"):
            if Stub.active_status == 404:
                return self._send(404, {"error": "none"})
            return self._send(200, {"root_id": "r1"})
        if self.path.startswith("/nodes/tree/r1"):
            return self._send(200, {"root_id": "r1", "text": "GOAL: ship it"})
        self._send(404, {})


class Rehydrate(unittest.TestCase):
    def setUp(self):
        Stub.log = []
        Stub.active_status = 200
        self.server = http.server.HTTPServer(("127.0.0.1", 0), Stub)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.url = f"http://127.0.0.1:{self.server.server_port}"

    def run_hook(self, stdin, url=None):
        env = dict(os.environ, NUCLEOS_DAEMON_URL=url or self.url, NUCLEOS_DAEMON_TOKEN="tok")
        proc = subprocess.run([sys.executable, str(SCRIPT)], input=stdin, capture_output=True,
                              text=True, env=env, timeout=30)
        self.assertEqual(proc.returncode, 0)
        return proc.stdout

    def test_happy_path(self):
        out = json.loads(self.run_hook(json.dumps({"session_id": "s 1"})))
        self.assertEqual(out, {"hookSpecificOutput": {
            "hookEventName": "SessionStart", "additionalContext": "GOAL: ship it"}})
        self.assertEqual([p for p, _ in Stub.log],
                         ["/nodes/active?session_id=s%201", "/nodes/tree/r1?view=brief"])

    def test_auth_header_sent(self):
        self.run_hook(json.dumps({"session_id": "s1"}))
        self.assertTrue(Stub.log and all(a == "Bearer tok" for _, a in Stub.log))

    def test_no_active_tree_prints_nothing(self):
        Stub.active_status = 404
        self.assertEqual(self.run_hook(json.dumps({"session_id": "s1"})), "")

    def test_daemon_down_prints_nothing(self):
        self.assertEqual(self.run_hook(json.dumps({"session_id": "s1"}),
                                       url="http://127.0.0.1:9"), "")

    def test_missing_session_id_prints_nothing(self):
        self.assertEqual(self.run_hook("{}"), "")
        self.assertEqual(Stub.log, [])

    def test_malformed_stdin_prints_nothing(self):
        self.assertEqual(self.run_hook("not json"), "")
        self.assertEqual(self.run_hook(""), "")


if __name__ == "__main__":
    unittest.main()
