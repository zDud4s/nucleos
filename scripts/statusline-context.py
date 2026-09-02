#!/usr/bin/env python3
"""Claude Code status line: how much context this session is carrying.

The number nobody could see. A session's bill is context x turns -- every turn
re-reads the whole window -- so cost grows with the SQUARE of a session's
length, not linearly. Measured on this machine: 17 sessions carried 89% of a
week's tokens, one of them climbing from 46k to 492k across 15,600 turns and
costing 3.76 billion tokens by itself. Nothing on screen said so at any point.

Splitting that one session ten ways would have cost roughly a quarter as much
for the same work. That is the whole argument for putting the number here.

Reads the session's own transcript rather than trusting a field to exist: the
status-line payload's shape is not something this script should depend on, and
the transcript carries the authoritative `usage` block Anthropic returned for
the last turn. Context is input + cache_read + cache_creation -- output is
excluded because it is not re-read next turn, and at 0.4% of the total it would
not move the number anyway.

Only the TAIL of the transcript is read. They reach hundreds of megabytes here,
and this runs on every redraw.

Configure the ceiling with NUCLEOS_CONTEXT_CEILING (default 250000).

Wire it up in ~/.claude/settings.json:

    "statusLine": {"type": "command",
                   "command": "python <this file> 2>/dev/null"}
"""
from __future__ import annotations

import json
import os
import sys

DEFAULT_CEILING = 250_000
TAIL_BYTES = 256 * 1024

# ANSI: dim for the ordinary case, yellow approaching the ceiling, red past it.
DIM = "\033[2m"
YELLOW = "\033[33m"
RED = "\033[31m"
RESET = "\033[0m"


def ceiling() -> int:
    raw = os.environ.get("NUCLEOS_CONTEXT_CEILING")
    if not raw:
        return DEFAULT_CEILING
    try:
        value = int(raw)
    except ValueError:
        return DEFAULT_CEILING
    return value if value > 0 else DEFAULT_CEILING


def last_context(transcript_path: str):
    """Context carried by the most recent assistant turn, or None.

    Scans backwards through the tail. Subagent turns (`isSidechain`) are skipped
    -- they run in their own window, so their context is not what this session
    is carrying, and counting one would make the number jump and then fall back
    for no reason the user could see.
    """
    try:
        size = os.path.getsize(transcript_path)
        with open(transcript_path, "rb") as fh:
            if size > TAIL_BYTES:
                fh.seek(size - TAIL_BYTES)
                fh.readline()  # discard the partial line the seek landed inside
            tail = fh.read()
    except OSError:
        return None

    for raw in reversed(tail.splitlines()):
        if b'"usage"' not in raw:
            continue
        try:
            record = json.loads(raw.decode("utf-8", "replace"))
        except ValueError:
            continue
        if record.get("type") != "assistant" or record.get("isSidechain"):
            continue
        usage = (record.get("message") or {}).get("usage") or {}
        if not usage:
            continue
        return ((usage.get("input_tokens") or 0)
                + (usage.get("cache_read_input_tokens") or 0)
                + (usage.get("cache_creation_input_tokens") or 0))
    return None


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (ValueError, OSError):
        payload = {}

    model = payload.get("model") or {}
    name = model.get("display_name") or model.get("id") or ""
    # `claude-opus-5[1m]` and `Opus 5 (1M context)` both say more than fits.
    name = name.replace("claude-", "").split("[")[0].strip()

    parts = []
    if name:
        parts.append(f"{DIM}{name}{RESET}")

    transcript = payload.get("transcript_path")
    ctx = last_context(transcript) if transcript else None
    cap = ceiling()
    if ctx is not None:
        share = ctx / cap
        colour = RED if share >= 1.0 else YELLOW if share >= 0.8 else DIM
        mark = " — split the session" if share >= 1.0 else ""
        parts.append(f"{colour}{ctx / 1000:.0f}k/{cap / 1000:.0f}k{mark}{RESET}")

    print(f"{DIM} · {RESET}".join(parts) if parts else "")
    return 0


if __name__ == "__main__":
    sys.exit(main())
