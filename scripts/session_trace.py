#!/usr/bin/env python3
"""Detect bad agent *paths* in Claude Code session transcripts.

``usage_audit.py`` answers "how much did the agent do"; this answers "did it do
it sensibly". It replays the file-level trace of a session — every Read, Edit,
Grep and shell command, in order — and flags four process smells:

  thrash    the same file read over and over with no edit in between
  unverified  code edited after the last passing test run, never re-verified
  late-scope  files first edited in the tail of the session (scope creep proxy)
  flailing  runs of searches that never land on a file

Nothing here judges the diff. It judges the route taken to produce it, which is
the part no code review ever sees.

Read-only over ``~/.claude/projects/*/*.jsonl``. Anything token-shaped is
redacted, so the report is safe to paste anywhere.

Usage:
    python scripts/session_trace.py                    # this repo, last 30 days
    python scripts/session_trace.py --days 90 --top 15
    python scripts/session_trace.py --session 4ada39e  # one session, full trace
    python scripts/session_trace.py --project nucleos --json trace.json
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import re
import subprocess
import sys
import time
from collections import Counter, defaultdict
from datetime import datetime

RE_TOKEN = re.compile(r"eyJ[\w.-]{20,}")
# Test runners as they actually appear here: buried mid-line after a PATH
# prefix, so this is searched against the whole command, never line one.
RE_TEST = re.compile(
    r"(cargo\s+(test|nextest)|pytest|go\s+test|select_tests\.py|gates\.sh"
    r"|vitest|jest|(npm|pnpm|yarn)\s+(run\s+)?test)",
    re.I,
)
RE_CHECK = re.compile(
    r"(cargo\s+(clippy|check|build)|tsc\b|(npm|pnpm|yarn)\s+run\s+build|mypy|ruff)", re.I
)
EDIT_TOOLS = {"Edit", "Write", "NotebookEdit"}
SEARCH_TOOLS = {"Grep", "Glob"}
SHELL_TOOLS = {"Bash", "PowerShell"}
# Agent scratch space is not the repo. Also catches the big false positive of
# the first run: re-reading a background job's .log/.output is *polling*, which
# is correct behaviour, and scored as thrash until it was excluded here.
RE_EXTERNAL = re.compile(
    r"[\\/](temp|tmp)[\\/]claude[\\/]|scratchpad|[\\/]tasks[\\/]|\.(log|output)$", re.I
)
# Only code can be broken by an edit that lands after the tests passed; a
# trailing docs/memory edit is not an unverified-code risk.
CODE_EXT = {".rs", ".go", ".ts", ".tsx", ".js", ".jsx", ".py", ".sql", ".toml",
            ".sh", ".ps1", ".yaml", ".yml", ".json", ".css", ".html"}

# Flag thresholds. Deliberately loose — the point of the first run is to see
# whether they separate anything at all, not to be right on attempt one.
T_THRASH = 3   # consecutive same-offset reads of one file, no edit between
T_LATE = 3     # files first edited in the last 30% of the trace
T_FLAIL = 4    # consecutive searches with no read/edit landing
T_WINDOW = 40  # tool calls; repeats further apart are a revisit, not a loop


def redact(text: str) -> str:
    return RE_TOKEN.sub("<REDACTED-TOKEN>", text)


def parse_ts(ts: str | None) -> float | None:
    if not ts:
        return None
    try:
        return datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


def repo_name() -> str | None:
    """Basename of the main repo, even when run from inside a worktree."""
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
            capture_output=True, text=True, timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0:
        return None
    common = out.stdout.strip()
    if not common:
        return None
    return os.path.basename(os.path.dirname(common)) or None


def norm_path(raw: str | None) -> str | None:
    if not raw or not isinstance(raw, str):
        return None
    return raw.replace("\\", "/").lower()


def is_repo_code(path: str | None) -> bool:
    if not path or RE_EXTERNAL.search(path):
        return False
    return os.path.splitext(path)[1] in CODE_EXT


def short(path: str, width: int = 46) -> str:
    return path if len(path) <= width else "…" + path[-(width - 1):]


def classify_shell(cmd: str) -> str | None:
    if RE_TEST.search(cmd):
        return "test"
    if RE_CHECK.search(cmd):
        return "check"
    return None


def build_events(path: str) -> dict:
    """Flatten one transcript into an ordered event list plus session meta."""
    events: list[dict] = []
    results: dict[str, bool] = {}   # tool_use_id -> is_error
    first_ts = last_ts = None
    user_msgs = 0
    tokens_out = 0

    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                d = json.loads(line)
            except json.JSONDecodeError:
                continue
            ts = parse_ts(d.get("timestamp"))
            if ts:
                first_ts = min(first_ts or ts, ts)
                last_ts = max(last_ts or ts, ts)
            msg = d.get("message") or {}
            content = msg.get("content")
            kind = d.get("type")

            if kind == "user":
                if isinstance(content, str) and content.strip() and not d.get("isMeta"):
                    if not content.startswith("<"):
                        user_msgs += 1
                if isinstance(content, list):
                    for c in content:
                        if isinstance(c, dict) and c.get("type") == "tool_result":
                            results[c.get("tool_use_id")] = bool(c.get("is_error"))
                continue

            if kind != "assistant":
                continue
            tokens_out += (msg.get("usage") or {}).get("output_tokens") or 0
            for c in content or []:
                if not (isinstance(c, dict) and c.get("type") == "tool_use"):
                    continue
                name = c.get("name", "?")
                inp = c.get("input") or {}
                ev = {"id": c.get("id"), "tool": name, "ts": ts, "file": None,
                      "offset": None, "kind": "other", "verify": None}
                if name == "Read":
                    ev["kind"] = "read"
                    ev["file"] = norm_path(inp.get("file_path"))
                    ev["offset"] = inp.get("offset")
                elif name in EDIT_TOOLS:
                    ev["kind"] = "edit"
                    ev["file"] = norm_path(inp.get("file_path"))
                elif name in SEARCH_TOOLS:
                    ev["kind"] = "search"
                elif name in SHELL_TOOLS:
                    ev["kind"] = "shell"
                    ev["verify"] = classify_shell(inp.get("command") or "")
                events.append(ev)

    return {
        "file": os.path.basename(path),
        "sid": os.path.basename(path).replace(".jsonl", ""),
        "project": os.path.basename(os.path.dirname(path)),
        "events": events,
        "results": results,
        "first_ts": first_ts,
        "last_ts": last_ts,
        "user_msgs": user_msgs,
        "tokens_out": tokens_out,
    }


def detect(sess: dict) -> dict:
    """Apply the four heuristics to one flattened session."""
    events = sess["events"]
    results = sess["results"]
    n = len(events)

    # --- H1 thrash: consecutive reads of one file, same offset, no edit between
    per_file: dict[str, list[tuple[int, dict]]] = defaultdict(list)
    for i, ev in enumerate(events):
        if ev["file"] and ev["kind"] in ("read", "edit") and not RE_EXTERNAL.search(ev["file"]):
            per_file[ev["file"]].append((i, ev))
    thrash: list[tuple[str, int]] = []
    for fpath, seq in per_file.items():
        run, best, seen_offsets, prev = 0, 0, set(), None
        for i, ev in seq:
            if ev["kind"] == "edit":
                run, seen_offsets, prev = 0, set(), None
                continue
            if prev is not None and i - prev > T_WINDOW:
                run, seen_offsets = 0, set()   # coming back later ≠ looping now
            # A different offset is paging through a big file, not thrash.
            if ev["offset"] in seen_offsets:
                run += 1
                best = max(best, run + 1)
            else:
                seen_offsets.add(ev["offset"])
                run = max(run, 1)
            prev = i
        if best >= T_THRASH:
            thrash.append((fpath, best))
    thrash.sort(key=lambda kv: -kv[1])

    # --- H2 unverified tail: edits after the last passing verification
    last_pass = -1
    verifications = 0
    for i, ev in enumerate(events):
        if ev["kind"] == "shell" and ev["verify"]:
            verifications += 1
            if not results.get(ev["id"], False):
                last_pass = i
    repo_edits = [
        i for i, ev in enumerate(events)
        if ev["kind"] == "edit" and is_repo_code(ev["file"])
    ]
    if verifications == 0:
        unverified = len(repo_edits)          # edited, never verified at all
        never_verified = bool(repo_edits)
    else:
        unverified = len([i for i in repo_edits if i > last_pass])
        never_verified = False

    # --- H3 late scope: repo files whose FIRST edit lands in the last 30%
    cutoff = n * 0.7
    first_edit: dict[str, int] = {}
    for i, ev in enumerate(events):
        if ev["kind"] == "edit" and ev["file"] and not RE_EXTERNAL.search(ev["file"]):
            first_edit.setdefault(ev["file"], i)
    late = sorted(f for f, i in first_edit.items() if i >= cutoff)

    # --- H4 flailing: consecutive searches that never land on a read/edit
    run, flail = 0, 0
    for ev in events:
        if ev["kind"] == "search":
            run += 1
            flail = max(flail, run)
        elif ev["kind"] in ("read", "edit"):
            run = 0

    flags = {
        "thrash": len(thrash),
        "unverified": unverified if (unverified and (verifications or repo_edits)) else 0,
        "late_scope": len(late) if len(late) >= T_LATE else 0,
        "flailing": flail if flail >= T_FLAIL else 0,
    }
    dur = (sess["last_ts"] - sess["first_ts"]) / 60 if sess["first_ts"] and sess["last_ts"] else 0
    return {
        **{k: sess[k] for k in ("sid", "project", "user_msgs", "tokens_out", "first_ts")},
        "tools": n,
        "minutes": round(dur, 1),
        "files_edited": len(first_edit),
        "verifications": verifications,
        "never_verified": never_verified,
        "thrash_files": thrash[:5],
        "late_files": late[:8],
        "flags": flags,
        "score": sum(1 for v in flags.values() if v),
    }


def collect(days: int, project_filter: str | None, session: str | None) -> list[dict]:
    root = os.path.join(os.path.expanduser("~"), ".claude", "projects")
    if not os.path.isdir(root):
        return []
    cutoff = time.time() - days * 86400
    out = []
    for proj in sorted(os.listdir(root)):
        if project_filter and project_filter.lower() not in proj.lower():
            continue
        for path in glob.glob(os.path.join(root, proj, "*.jsonl")):
            if session:
                if not os.path.basename(path).startswith(session):
                    continue
            else:
                try:
                    if os.path.getmtime(path) < cutoff:
                        continue
                except OSError:
                    continue
            try:
                out.append(detect(build_events(path)))
            except OSError:
                continue
    return out


def render(rows: list[dict], top: int, days: int, scope: str) -> str:
    if not rows:
        return f"No sessions found for {scope} in the last {days} days."
    total = len(rows)
    # A session with no tool calls at all is a conversation, not a path. Left in
    # the base rate it scores "clean" for free and flatters every heuristic.
    active = [r for r in rows if r["tools"] > 0]
    base = len(active)
    hit = Counter()
    for r in active:
        for k, v in r["flags"].items():
            if v:
                hit[k] += 1

    out = [f"# Session path audit — {scope}, last {days} days", ""]
    out.append(f"Sessions analysed: {total} ({total - base} had no tool calls, "
               f"excluded from rates below)")
    out.append("")
    if base > 1:   # a base rate over one session says nothing
        out.append("## Do the heuristics discriminate?")
        out.append(f"| smell | sessions flagged | share of {base} |")
        out.append("|---|---:|---:|")
        for key in ("thrash", "unverified", "late_scope", "flailing"):
            c = hit[key]
            out.append(f"| {key} | {c} | {100 * c / base:.0f}% |")
        clean = sum(1 for r in active if r["score"] == 0)
        out.append(f"| _clean (no flags)_ | {clean} | {100 * clean / base:.0f}% |")
        out.append("")
        out.append("A smell firing on ~everything or ~nothing is a bad detector, not a finding.")
        out.append("")

    out.append(f"## Most suspicious sessions (top {top})")
    out.append("| session | when | min | tools | files | thrash | unverif | late | flail |")
    out.append("|---|---|---:|---:|---:|---:|---:|---:|---:|")
    worst = sorted(rows, key=lambda r: (-r["score"], -r["tools"]))[:top]
    for r in worst:
        when = datetime.fromtimestamp(r["first_ts"]).strftime("%m-%d %H:%M") if r["first_ts"] else "?"
        f = r["flags"]
        out.append(
            f"| `{r['sid'][:8]}` | {when} | {r['minutes']:.0f} | {r['tools']} | {r['files_edited']} "
            f"| {f['thrash'] or ''} | {f['unverified'] or ''} | {f['late_scope'] or ''} "
            f"| {f['flailing'] or ''} |"
        )
    out.append("")

    out.append("## Detail — worst 5")
    for r in worst[:5]:
        out.append("")
        out.append(f"### `{r['sid'][:8]}` — score {r['score']}/4, {r['tools']} tool calls, "
                   f"{r['files_edited']} files edited, {r['verifications']} verification runs")
        if r["thrash_files"]:
            out.append("- **thrash**: " + ", ".join(
                f"`{short(f)}` ×{c}" for f, c in r["thrash_files"]))
        if r["never_verified"]:
            out.append(f"- **unverified**: {r['flags']['unverified']} repo edits and "
                       f"*no test run at all* in the session")
        elif r["flags"]["unverified"]:
            out.append(f"- **unverified**: {r['flags']['unverified']} edits after the last "
                       f"passing verification, never re-verified")
        if r["flags"]["late_scope"]:
            out.append("- **late scope**: first touched in the last 30% — " + ", ".join(
                f"`{short(f, 34)}`" for f in r["late_files"]))
        if r["flags"]["flailing"]:
            out.append(f"- **flailing**: {r['flags']['flailing']} searches in a row with no "
                       f"file read or edited")
        if r["score"] == 0:
            out.append("- clean")
    out.append("")
    return redact("\n".join(out))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--days", type=int, default=30, help="lookback window (default 30)")
    ap.add_argument("--project", help="project dir substring (default: this repo)")
    ap.add_argument("--session", help="analyse one session by id prefix (ignores --days)")
    ap.add_argument("--top", type=int, default=12, help="rows in the ranking (default 12)")
    ap.add_argument("--json", dest="json_path", help="also dump raw rows to this JSON file")
    args = ap.parse_args()

    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")

    proj = args.project if args.project is not None else repo_name()
    rows = collect(args.days, proj, args.session)
    if args.json_path:
        with open(args.json_path, "w", encoding="utf-8") as fh:
            json.dump(rows, fh, indent=1, ensure_ascii=False)
    print(render(rows, args.top, args.days, proj or "all projects"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
