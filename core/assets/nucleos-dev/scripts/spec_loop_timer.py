#!/usr/bin/env python3
"""Wakes a spec session that is parked on background work just before its prompt cache expires, and
once a session the Stop hook let go with items still open.

`spec_loop.py --stop-hook` lets such a session stop, because a push while it waits buys nothing.
But the Stop hook only runs at the end of a turn, so nothing would look at the session again if
the work outlasts the cache, if its completion never wakes the session, or if the hook gave up on
it. This runs in the background from every Stop (`core/hooks/stop_timer.py`, `asyncRewake`) and
exits 2 when the session has been idle for WINDOW_SECS: a read of the cache costs about 0.1 of the
context, and writing it back after a miss about 2.0 at the 1 h TTL.

A later turn in the session means a later Stop and a new timer, so this one bows out as soon as it
sees one. Every other exit is 0 and silent: exit 2 is the only thing the session ever notices.

    python <bundle_dir>/scripts/test_spec_loop.py -v
"""

from __future__ import annotations

import json
import sys
import time
from datetime import datetime

import spec_loop

# Idle time before the wake, by the kind of cache write the last request made. The margin covers a
# long response, whose record is written after the cache was read.
WINDOW_SECS = {"ephemeral_1h_input_tokens": 55 * 60, "ephemeral_5m_input_tokens": 4 * 60}
DEFAULT_WINDOW_SECS = 55 * 60
POLL_SECS = 30
# How long one unchanged set of tasks keeps being woken, from its first wake. Past this a hung task
# stops costing a turn per window. Time and not a count: the count that fitted the 1 h cache gave
# the 5 min one only 24 minutes. The longest real wait measured is 4.4 h.
MAX_WAKE_SECS = 5.5 * 3600
# How long a timer with no tasks waits to learn whether its own Stop let the session go. The two
# start together, and the daemon's bundle hooks answer within 50 s.
LET_GO_GRACE_SECS = 60


def _window(usage: dict) -> int | None:
    writes = usage.get("cache_creation") or {}
    written = {kind: writes.get(kind) or 0 for kind in WINDOW_SECS}
    kind = max(written, key=written.get)
    return WINDOW_SECS[kind] if written[kind] else None


def last_request(transcript: str) -> tuple[float, int] | None:
    """When the session's last request answered, and the idle window its cache allows."""
    try:
        with open(transcript, "rb") as f:
            f.seek(0, 2)
            f.seek(max(0, f.tell() - spec_loop._TAIL_BYTES))
            lines = f.read().decode("utf-8", "replace").splitlines()
    except OSError:
        return None
    when, window = None, None
    for line in reversed(lines):
        if '"assistant"' not in line:
            continue
        try:
            rec = json.loads(line)
        except ValueError:
            continue
        if not isinstance(rec, dict) or rec.get("type") != "assistant" or rec.get("isSidechain"):
            continue
        if when is None:
            try:
                when = datetime.fromisoformat(rec["timestamp"].replace("Z", "+00:00")).timestamp()
            except (KeyError, AttributeError, TypeError, ValueError):
                return None
        # A request that only read the cache says nothing about its TTL; an earlier one wrote it.
        window = _window((rec.get("message") or {}).get("usage") or {})
        if window:
            break
    return (when, window or DEFAULT_WINDOW_SECS) if when is not None else None


def wake_message(payload: dict, now=time.time, sleep=time.sleep) -> str | None:
    """The text to wake the session with once the window is up, or None to let it be."""
    session, transcript = payload.get("session_id"), payload.get("transcript_path")
    loops = spec_loop.loops_dir(payload.get("cwd"))
    tasks = spec_loop.running_tasks(payload)
    if not (session and transcript and loops):
        return None
    path = spec_loop.loop_path(loops, session)

    def still_parked() -> bool:
        try:
            loop = spec_loop.parse(path.read_text(encoding="utf-8"))
        except OSError:
            return False
        return bool(loop["open"]) and loop["status"] == "active"

    state_path = path.with_suffix(".state.json")
    wakes = spec_loop._load_state(state_path).get("wakes") or {}
    since = wakes.get("since") if wakes.get("tasks") == tasks else None
    start = last_request(transcript)
    if not start or (since and now() - since >= MAX_WAKE_SECS) or not still_parked():
        return None

    began = now()
    deadline = start[0] + start[1]
    while now() < deadline:
        sleep(min(POLL_SECS, max(0, deadline - now())))
        latest = last_request(transcript)
        if not latest or latest[0] != start[0]:
            return None
        if (not tasks and now() - began >= LET_GO_GRACE_SECS
                and not spec_loop._load_state(state_path).get("let_go")):
            return None
    if not still_parked():
        return None

    state = spec_loop._load_state(state_path)
    if not tasks:
        # Read after the sleep: this timer starts alongside the very Stop that may let the session go.
        n_open = state.get("let_go")
        if not n_open or state.get("woken_for") == n_open:
            return None
        state.update(woken=True, woken_for=n_open)
        state_path.write_text(json.dumps(state), encoding="utf-8")
        return (f"Spec loop timer: this session stopped with {n_open} open item(s) in "
                f"{path.as_posix()} after the pushes closed nothing, and nothing else will wake it. "
                "Carry on with the next item. If it waits on something outside this session, say "
                "what in one line; if only the owner can decide it, mark it `- [!]` and set "
                "`status: blocked`.")
    state["wakes"] = {"tasks": tasks, "since": since or now()}
    state_path.write_text(json.dumps(state), encoding="utf-8")
    return ("Spec loop timer: background work is still running and the prompt cache is about to "
            f"expire ({', '.join(tasks)}). This wake-up exists to keep the cache: if the work is "
            "still going, answer in one short line and end the turn, with no report. If a task has "
            "finished without telling you, or looks hung, deal with that instead.")


def main() -> int:
    try:
        message = wake_message(spec_loop._payload())
    except Exception as exc:  # fail-open: only a real wake may exit 2
        print(f"spec loop timer: {exc}", file=sys.stderr)
        return 0
    if not message:
        return 0
    print(message, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
