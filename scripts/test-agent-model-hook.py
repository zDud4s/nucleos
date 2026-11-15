#!/usr/bin/env python3
"""Cover the agent-model parts of `core/hooks/ask_daemon.py` (SubagentStop forwarding, PreToolUse model choice).

Imports the SOURCE `core/hooks/ask_daemon.py`, like `scripts/test-hook-filter.py`.

Run:  python scripts/test-agent-model-hook.py
"""

import contextlib
import importlib.util
import io
import json
import os
import sys
import threading
import time
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


class JsonServer(Server):
    """Like `Server`, but answers 200 with `answer` as JSON; `delay` stalls the reply."""

    def __init__(self, answer, delay=0.0):
        seen = self.seen = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                length = int(self.headers.get("Content-Length") or 0)
                seen.append((self.path, json.loads(self.rfile.read(length) or b"{}")))
                time.sleep(delay)
                try:
                    data = json.dumps(answer).encode()
                    self.send_response(200)
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except OSError:
                    pass

            def log_message(self, *a):
                pass

        self.httpd = HTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.httpd.server_port}"
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()


def spawn(**extra):
    tool_input = {"description": "map src", "prompt": "list files", "subagent_type": "worker", **extra}
    return {"hook_event_name": "PreToolUse", "tool_name": "Agent", "tool_input": tool_input,
            "session_id": "sess-1", "cwd": ROOT}


def ask(payload, answer, delay=0.0):
    server = JsonServer(answer, delay)
    try:
        with mock.patch.dict(os.environ, {"NUCLEOS_DAEMON_URL": server.url}), \
                mock.patch.object(hook, "read_control_token", return_value=("tok", None)):
            began = time.monotonic()
            decision = hook.agent_model_decision(payload)
            ask.elapsed = time.monotonic() - began  # before close(): shutdown waits out a slow handler
            return decision, server.seen
    finally:
        server.close()


def test_explicit_model_is_left_alone():
    result, seen = ask(spawn(model="opus"), {"model": "haiku"})
    assert result is None and seen == [], (result, seen)


def test_a_model_from_the_daemon_is_written_into_updated_input():
    payload = spawn()
    result, seen = ask(payload, {"model": "haiku", "source": "router", "mode": "apply"})
    assert result == {**payload["tool_input"], "model": "haiku"}, result
    path, body = seen[0]
    assert path == "/hooks/agent-model", path
    assert body["subagent_type"] == "worker" and body["description"] == "map src", body
    assert body["session_id"] == "sess-1", body


def test_a_null_model_answer_changes_nothing():
    result, seen = ask(spawn(), {"model": None, "source": "shadow", "mode": "shadow"})
    assert result is None and len(seen) == 1, (result, seen)


def test_a_non_alias_answer_is_ignored():
    for bad in ("", 7, ["haiku"], "../x"):
        result, _ = ask(spawn(), {"model": bad})
        assert result is None, (bad, result)


def test_a_slow_or_dead_daemon_fails_open_within_the_ceiling():
    result, _ = ask(spawn(), {"model": "haiku"}, delay=4.0)
    assert result is None and ask.elapsed < 2.5, ask.elapsed
    with mock.patch.dict(os.environ, {"NUCLEOS_DAEMON_URL": "http://127.0.0.1:1"}), \
            mock.patch.object(hook, "read_control_token", return_value=("tok", None)):
        assert hook.agent_model_decision(spawn()) is None
    with mock.patch.object(hook, "read_control_token", return_value=(None, "token")):
        assert hook.agent_model_decision(spawn()) is None


def test_other_tools_are_ignored():
    payload = spawn()
    payload["tool_name"] = "Bash"
    result, seen = ask(payload, {"model": "haiku"})
    assert result is None and seen == []


def test_task_alias_is_covered():
    payload = spawn()
    payload["tool_name"] = "Task"
    result, _ = ask(payload, {"model": "haiku"})
    assert result and result["model"] == "haiku", result


def test_the_prompt_sent_is_capped():
    _, seen = ask(spawn(prompt="x" * 20000), {"model": None})
    assert len(seen[0][1]["prompt"]) == 4000, len(seen[0][1]["prompt"])


def test_runs_of_the_daemon_are_out_of_scope():
    with mock.patch.dict(os.environ, {"NUCLEOS_RUN_ID": "r1"}):
        result, seen = ask(spawn(), {"model": "haiku"})
    assert result is None and seen == []


def test_the_rewrite_never_carries_a_permission_decision():
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        try:
            hook.rewrite_input({"description": "d", "model": "haiku"})
        except SystemExit as e:
            assert e.code == 0
    printed = json.loads(out.getvalue())["hookSpecificOutput"]
    assert printed["hookEventName"] == "PreToolUse"
    assert printed["updatedInput"]["model"] == "haiku"
    assert "permissionDecision" not in printed, printed


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
