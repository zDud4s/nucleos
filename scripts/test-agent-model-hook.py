#!/usr/bin/env python3
"""Cover the agent-model parts of `core/hooks/ask_daemon.py` (SubagentStop forwarding).

Imports the SOURCE `core/hooks/ask_daemon.py`, like `scripts/test-hook-filter.py`.

Run:  python scripts/test-agent-model-hook.py
"""

import importlib.util
import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from unittest import mock

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
HOOK = os.path.join(ROOT, "core", "hooks", "ask_daemon.py")

spec = importlib.util.spec_from_file_location("ask_daemon", HOOK)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

PAYLOAD = {
    "hook_event_name": "SubagentStop",
    "session_id": "sess-1",
    "agent_id": "a1",
    "agent_type": "worker",
    "cwd": ROOT,
}


class Server:
    """A loopback server that records bodies and answers with `status` (0 = close the socket)."""

    def __init__(self, status):
        seen = self.seen = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                length = int(self.headers.get("Content-Length") or 0)
                seen.append((self.path, json.loads(self.rfile.read(length) or b"{}")))
                if status == 0:
                    self.connection.close()
                    return
                self.send_response(status)
                self.end_headers()

            def log_message(self, *a):
                pass

        self.httpd = HTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.httpd.server_port}"
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    def close(self):
        self.httpd.shutdown()


def test_outcome_body_shape():
    body = hook.agent_outcome_body(PAYLOAD)
    assert body == {"session_id": "sess-1", "error": False, "agent_id": "a1", "agent_type": "worker"}, body
    body = hook.agent_outcome_body({"session_id": "s", "is_error": True, "transcript_path": "/t.jsonl"})
    assert body == {"session_id": "s", "error": True, "transcript_path": "/t.jsonl"}, body


def test_subagent_stop_forwards_and_never_raises():
    for status in (200, 500, 0):
        server = Server(status)
        try:
            with mock.patch.dict(os.environ, {"NUCLEOS_DAEMON_URL": server.url}), \
                    mock.patch.object(hook, "read_control_token", return_value=("tok", None)):
                hook.report_agent_outcome(PAYLOAD)  # must return, whatever the daemon does
            assert server.seen and server.seen[0][0] == "/hooks/agent-outcome", (status, server.seen)
            assert server.seen[0][1]["session_id"] == "sess-1"
        finally:
            server.close()


def test_no_token_or_dead_daemon_is_silent():
    with mock.patch.object(hook, "read_control_token", return_value=(None, "token")):
        hook.report_agent_outcome(PAYLOAD)
    with mock.patch.dict(os.environ, {"NUCLEOS_DAEMON_URL": "http://127.0.0.1:1"}), \
            mock.patch.object(hook, "read_control_token", return_value=("tok", None)):
        hook.report_agent_outcome(PAYLOAD)
    with mock.patch.object(hook, "read_control_token", side_effect=RuntimeError("boom")):
        hook.report_agent_outcome(PAYLOAD)


if __name__ == "__main__":
    failed = 0
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            try:
                fn()
                print(f"ok   {name}")
            except Exception as e:  # noqa: BLE001
                failed += 1
                print(f"FAIL {name}: {e!r}")
    sys.exit(1 if failed else 0)
