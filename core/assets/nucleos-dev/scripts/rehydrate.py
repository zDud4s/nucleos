#!/usr/bin/env python3
"""SessionStart hook: re-inject the daemon's work tree after a native compact.

Reads the hook payload (`session_id`) from stdin, asks the daemon which work tree is active for
that session (`GET /nodes/active?session_id=`), fetches its brief (`GET /nodes/tree/{root}?view=brief`)
and prints one `hookSpecificOutput` JSON object carrying the brief's `text` as additional context.

Fails open, every way: no session id, no token, daemon down, 404, bad JSON, empty text, timeout
-> exit 0 with nothing printed. Stdlib only.

Daemon discovery mirrors `core/hooks/ask_daemon.py`: base URL from `NUCLEOS_DAEMON_URL` (default
http://127.0.0.1:8791), bearer token from `NUCLEOS_DAEMON_TOKEN` or, failing that, the project's
own `.claude/hooks/ask_daemon.py::read_control_token`.
"""

from __future__ import annotations

import importlib.util
import json
import os
import sys
import urllib.parse
import urllib.request
from pathlib import Path

DEFAULT_DAEMON_URL = "http://127.0.0.1:8791"
TIMEOUT_SECS = 3


def find_token(cwd: str) -> str | None:
    env = os.environ.get("NUCLEOS_DAEMON_TOKEN")
    if env:
        return env
    for base in (cwd, os.environ.get("CLAUDE_PROJECT_DIR") or "", os.getcwd()):
        if not base:
            continue
        helper = Path(base) / ".claude" / "hooks" / "ask_daemon.py"
        if not helper.is_file():
            continue
        spec = importlib.util.spec_from_file_location("_nucleos_ask_daemon", helper)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        token, _why = module.read_control_token(base, 2, 3)
        if token:
            return token
    return None


def get_json(url: str, token: str) -> dict:
    request = urllib.request.Request(url, headers={"Authorization": f"Bearer {token}"})
    with urllib.request.urlopen(request, timeout=TIMEOUT_SECS) as response:
        data = json.loads(response.read())
    if not isinstance(data, dict):
        raise ValueError("not an object")
    return data


def rehydrate(payload: dict) -> str | None:
    session_id = payload.get("session_id")
    if not isinstance(session_id, str) or not session_id:
        return None
    token = find_token(str(payload.get("cwd") or os.getcwd()))
    if not token:
        return None
    base = (os.environ.get("NUCLEOS_DAEMON_URL") or DEFAULT_DAEMON_URL).rstrip("/")
    active = get_json(f"{base}/nodes/active?session_id={urllib.parse.quote(session_id, safe='')}",
                      token)
    root_id = active.get("root_id")
    if root_id is None or root_id == "":
        return None
    tree = get_json(f"{base}/nodes/tree/{urllib.parse.quote(str(root_id), safe='')}?view=brief",
                    token)
    text = tree.get("text")
    return text if isinstance(text, str) and text.strip() else None


def main() -> int:
    try:
        payload = json.loads(sys.stdin.read())
        if not isinstance(payload, dict):
            return 0
        text = rehydrate(payload)
        if text:
            print(json.dumps({"hookSpecificOutput": {
                "hookEventName": "SessionStart", "additionalContext": text}}))
    except Exception:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
