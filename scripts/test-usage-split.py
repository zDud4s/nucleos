#!/usr/bin/env python3
"""Cover the token accounting in `scripts/usage_split.py`.

That script exists to answer one question — which of two subscriptions is
paying for the work — and the answer drives a real budget decision. Every
number it prints comes from reconciling two vendors that count differently,
and each difference is a silent order-of-magnitude trap:

  cumulative   A Codex rollout writes ``total_token_usage`` once per turn, each
               line superseding the last. Summing them multiplies a session by
               roughly its turn count. Measured on a real rollout: 15 lines
               climbing 17,514 -> 521,278. Summing gives ~3.9M for a 521k session.
  subset       Codex's ``cached_input_tokens`` is INSIDE ``input_tokens``, and
               ``total_tokens`` = input + output. Adding the cached figure
               counts most of the session's input twice.
  disjoint     Claude is the opposite: ``input_tokens`` is fresh input ONLY,
               with cache reads and writes in separate fields. Dropping them
               undercounts by ~97%, because cache read is nearly all of it.

Get any one of these wrong and the tool still runs, still prints a confident
table, and still points at the wrong subscription. Nothing downstream notices.

Also covered: the controller/subagent lane split (an in-process subagent lands
in the controller's own transcript, marked ``isSidechain``, so a naive read
bills its tokens to the controller), the weekly-only rate-limit filter, and
tolerance of an unparseable line.

Hermetic: builds transcript trees in a temp directory via the ``root`` seam.
No network, no real session history, nothing to be flaky about.

Run:  python scripts/test-usage-split.py
"""

import importlib.util
import json
import os
import sys
import tempfile

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
TARGET = os.path.join(ROOT, "scripts", "usage_split.py")

spec = importlib.util.spec_from_file_location("usage_split", TARGET)
usage_split = importlib.util.module_from_spec(spec)
spec.loader.exec_module(usage_split)

TS = "2026-08-10T12:00:00Z"


def write_jsonl(path, lines):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        for line in lines:
            fh.write((line if isinstance(line, str) else json.dumps(line)) + "\n")


def claude_turn(fresh, cread, ccreate, out, sidechain=False, model="claude-opus-5", sid="s1"):
    return {
        "type": "assistant",
        "timestamp": TS,
        "sessionId": sid,
        "isSidechain": sidechain,
        "message": {
            "model": model,
            "usage": {
                "input_tokens": fresh,
                "cache_read_input_tokens": cread,
                "cache_creation_input_tokens": ccreate,
                "output_tokens": out,
            },
        },
    }


def codex_usage(total, cached=0, out=0, window=usage_split.WEEKLY_WINDOW_MINUTES, used=None):
    payload = {
        "type": "token_count",
        "info": {
            "total_token_usage": {
                "input_tokens": total - out,
                "cached_input_tokens": cached,
                "output_tokens": out,
                "total_tokens": total,
            }
        },
    }
    if used is not None:
        payload["rate_limits"] = {"primary": {"window_minutes": window, "used_percent": used}}
    return {"timestamp": TS, "type": "event_msg", "payload": payload}


def total_of(weeks):
    return sum(w["total"] for w in weeks.values())


def cases(tmp):
    """Yield (label, actual, expected) triples."""

    # --- Claude: input_tokens is fresh only; cache fields are additional ------
    root = os.path.join(tmp, "claude-basic")
    write_jsonl(os.path.join(root, "proj", "a.jsonl"),
                [claude_turn(fresh=100, cread=9000, ccreate=500, out=50)])
    got = usage_split.collect_claude(30, None, root=root)
    yield ("claude total = fresh + cache_read + cache_creation + output",
           total_of(got["weeks"]), 9650)
    yield ("claude context per turn excludes output",
           got["contexts"], [9600])

    # --- Claude: the lane split -----------------------------------------------
    root = os.path.join(tmp, "claude-lanes")
    write_jsonl(os.path.join(root, "proj", "a.jsonl"), [
        claude_turn(fresh=10, cread=0, ccreate=0, out=0, sidechain=False),
        claude_turn(fresh=7, cread=0, ccreate=0, out=0, sidechain=True),
    ])
    got = usage_split.collect_claude(30, None, root=root)
    week = next(iter(got["weeks"].values()))
    yield ("controller lane excludes the subagent", week["controller"], 10)
    yield ("subagent lane is billed separately", week["subagent"], 7)
    yield ("a subagent turn is not counted as controller context",
           got["contexts"], [10])

    # --- Claude: project filter ------------------------------------------------
    root = os.path.join(tmp, "claude-filter")
    write_jsonl(os.path.join(root, "keep", "a.jsonl"), [claude_turn(5, 0, 0, 0)])
    write_jsonl(os.path.join(root, "drop", "b.jsonl"), [claude_turn(500, 0, 0, 0)])
    got = usage_split.collect_claude(30, "keep", root=root)
    yield ("project filter excludes other projects", total_of(got["weeks"]), 5)

    # --- Claude: a bad line must not lose the file -----------------------------
    root = os.path.join(tmp, "claude-badline")
    write_jsonl(os.path.join(root, "proj", "a.jsonl"), [
        claude_turn(1, 0, 0, 0),
        "{not json at all",
        claude_turn(2, 0, 0, 0),
    ])
    got = usage_split.collect_claude(30, None, root=root)
    yield ("an unparseable line does not lose the rest of the file",
           total_of(got["weeks"]), 3)

    # --- Codex: total_token_usage is cumulative --------------------------------
    root = os.path.join(tmp, "codex-cumulative")
    write_jsonl(os.path.join(root, "2026", "08", "10", "rollout-x.jsonl"), [
        codex_usage(total=17514),
        codex_usage(total=210000),
        codex_usage(total=521278),
    ])
    got = usage_split.collect_codex(30, root=root)
    yield ("codex cumulative usage counts the LAST line, not the sum",
           total_of(got["weeks"]), 521278)
    yield ("codex counts one session, not one per turn", got["sessions"], 1)

    # --- Codex: cached_input_tokens is a subset --------------------------------
    root = os.path.join(tmp, "codex-subset")
    write_jsonl(os.path.join(root, "2026", "08", "10", "rollout-y.jsonl"),
                [codex_usage(total=529455, cached=481536, out=8177)])
    got = usage_split.collect_codex(30, root=root)
    yield ("codex total_tokens already includes cached input",
           total_of(got["weeks"]), 529455)

    # --- Codex: only the weekly rate-limit window counts -----------------------
    root = os.path.join(tmp, "codex-rate")
    write_jsonl(os.path.join(root, "2026", "08", "10", "rollout-z.jsonl"), [
        codex_usage(total=10, window=300, used=99.0),                  # 5h window
        codex_usage(total=20, used=42.0),                              # weekly
    ])
    got = usage_split.collect_codex(30, root=root)
    yield ("the short rate-limit window is ignored",
           [p for _dt, p in got["rate"]], [42.0])

    # --- Codex: a rollout with no usage at all ---------------------------------
    root = os.path.join(tmp, "codex-empty")
    write_jsonl(os.path.join(root, "2026", "08", "10", "rollout-empty.jsonl"),
                [{"timestamp": TS, "type": "event_msg", "payload": {"type": "agent_message"}}])
    got = usage_split.collect_codex(30, root=root)
    yield ("a rollout with no usage counts as no session", got["sessions"], 0)

    # --- Both: a missing root is not a crash -----------------------------------
    missing = os.path.join(tmp, "does-not-exist")
    yield ("missing claude root yields no weeks",
           total_of(usage_split.collect_claude(30, None, root=missing)["weeks"]), 0)
    yield ("missing codex root yields no weeks",
           total_of(usage_split.collect_codex(30, root=missing)["weeks"]), 0)

    # --- Week bucketing --------------------------------------------------------
    from datetime import datetime, timezone
    yield ("iso_week formats year-Www",
           usage_split.iso_week(datetime(2026, 8, 10, tzinfo=timezone.utc)), "2026-W33")


def main() -> int:
    failures = 0
    total = 0
    with tempfile.TemporaryDirectory() as tmp:
        for label, got, want in cases(tmp):
            total += 1
            if got != want:
                failures += 1
                print(f"FAIL {label}: expected {want!r}, got {got!r}")
    print(f"{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
