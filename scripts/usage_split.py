#!/usr/bin/env python3
"""Where the weekly budget actually goes, across Claude Code and Codex.

``usage_audit.py`` answers "how am I using Claude Code"; this answers "which of
my two subscriptions is paying for it, and what is driving the bill". It reads
both tools' local session records and reports the split.

Three things it measures that a per-tool view cannot:

  split       Claude vs Codex tokens per ISO week, so a tool silently falling
              out of rotation is visible. (It happened here: dispatched phases
              went 97/97 to Claude for five weeks before anyone noticed.)
  lane        inside Claude, controller vs dispatched subagent. Subagents run
              in-process and land in the same transcript, marked isSidechain.
  drivers     cost is context x turns, not output. Ranking sessions by output
              tokens points at the wrong ones -- output is well under 1% of the
              total. This ranks by context actually processed.

Accounting differs between the two vendors and is normalized here:

  Claude   ``input_tokens`` is FRESH input only; cache reads and cache writes
           are separate fields. Total = input + cache_read + cache_creation + output.
  Codex    ``cached_input_tokens`` is a SUBSET of ``input_tokens``, and
           ``total_tokens`` = input + output. Adding the subset double-counts.

Cache reads can be weighted below fresh input to model billed usage. Cache
writes remain unweighted: they bill above fresh input, not below it.

``total_token_usage`` in a Codex rollout is CUMULATIVE per session -- one line
per turn, each superseding the last. Only the final one counts; summing them
overstates a session by roughly its turn count.

Codex additionally records its own weekly rate-limit percentage, which is a
real quota reading rather than a token proxy. Claude Code's transcripts carry
no equivalent, so that column is Codex-only.

Read-only over ``~/.claude/projects/`` and ``~/.codex/sessions/``.

Usage:
    python scripts/usage_split.py                    # last 30 days
    python scripts/usage_split.py --days 7
    python scripts/usage_split.py --cache-weight 0.1 # bill cache reads at 10%
    python scripts/usage_split.py --ceiling 250000   # session context ceiling
    python scripts/usage_split.py --json split.json
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import statistics
import sys
import time
from collections import Counter, defaultdict
from datetime import datetime

# The weekly rate-limit window Codex reports, in minutes. Codex also reports a
# shorter rolling window; only the weekly one answers "am I about to run out".
WEEKLY_WINDOW_MINUTES = 10080


def parse_ts(ts):
    if not ts:
        return None
    try:
        return datetime.fromisoformat(ts.replace("Z", "+00:00"))
    except (ValueError, AttributeError):
        return None


def iso_week(dt: datetime) -> str:
    year, week, _ = dt.isocalendar()
    return f"{year}-W{week:02d}"


def iter_jsonl(path: str):
    """Yield parsed objects, skipping anything unparseable.

    A live session's transcript can be mid-write, and older files predate
    schema changes; one bad line must not lose the rest of the file.
    """
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                line = line.strip()
                if not line or line[0] != "{":
                    continue
                try:
                    yield json.loads(line)
                except ValueError:
                    continue
    except OSError:
        return


def recent_files(root: str, pattern: str, cutoff: float):
    if not os.path.isdir(root):
        return []
    out = []
    for path in glob.glob(os.path.join(root, pattern), recursive=True):
        try:
            if os.path.getmtime(path) >= cutoff:
                out.append(path)
        except OSError:
            continue
    return out


def collect_claude(days: int, project_filter, root=None, cache_weight=1.0):
    # `root` is the seam the test uses: it builds a transcript tree in a temp
    # directory rather than reading the machine's real session history, which
    # would make the test neither hermetic nor repeatable.
    root = root or os.path.join(os.path.expanduser("~"), ".claude", "projects")
    cutoff = time.time() - days * 86400
    weeks = defaultdict(Counter)      # week -> lane totals
    models = defaultdict(Counter)     # (model, lane) -> totals
    contexts = []                     # controller context per turn
    sessions = defaultdict(lambda: {"tokens": 0, "turns": 0, "ctx_max": 0,
                                    "ctx_first": None, "model": "?", "project": "?"})
    raw_total = 0

    for path in recent_files(root, os.path.join("**", "*.jsonl"), cutoff):
        project = os.path.basename(os.path.dirname(path))
        if project_filter and project_filter.lower() not in project.lower():
            continue
        for d in iter_jsonl(path):
            if d.get("type") != "assistant":
                continue
            msg = d.get("message") or {}
            usage = msg.get("usage") or {}
            if not usage:
                continue
            dt = parse_ts(d.get("timestamp"))
            if dt is None:
                continue
            fresh = usage.get("input_tokens") or 0
            cread = usage.get("cache_read_input_tokens") or 0
            ccreate = usage.get("cache_creation_input_tokens") or 0
            out = usage.get("output_tokens") or 0
            # Context is everything the model had to read this turn. It is the
            # cost driver: it is re-read every turn, so a session's bill grows
            # with the square of its length, not linearly.
            ctx = fresh + cread + ccreate
            # Cache reads may bill below fresh input; cache writes do not. Keep
            # context raw: a discounted read still occupies the model window.
            weighted_cread = cread if cache_weight == 1.0 else cread * cache_weight
            raw_usage = ctx + out
            total = fresh + weighted_cread + ccreate + out
            raw_total += raw_usage
            lane = "subagent" if d.get("isSidechain") else "controller"
            model = msg.get("model") or "?"

            wk = weeks[iso_week(dt)]
            wk[lane] += total
            wk["total"] += total
            m = models[(model, lane)]
            m["total"] += total
            m["cache_read"] += cread
            m["output"] += out
            m["turns"] += 1

            if lane == "controller":
                contexts.append(ctx)
                sid = d.get("sessionId") or path
                s = sessions[sid]
                # The ceiling section describes window size, so its session
                # figures stay raw even when the report totals are weighted.
                s["tokens"] += raw_usage
                s["turns"] += 1
                s["ctx_max"] = max(s["ctx_max"], ctx)
                if s["ctx_first"] is None:
                    s["ctx_first"] = ctx
                s["model"] = model
                s["project"] = project

    return {"weeks": weeks, "models": models, "contexts": contexts, "sessions": sessions,
            "raw_total": raw_total}


def collect_codex(days: int, root=None, cache_weight=1.0):
    root = root or os.path.join(os.path.expanduser("~"), ".codex", "sessions")
    cutoff = time.time() - days * 86400
    weeks = defaultdict(Counter)
    rate = []
    sessions = 0

    for path in recent_files(root, os.path.join("**", "rollout-*.jsonl"), cutoff):
        last_usage = None
        last_dt = None
        for d in iter_jsonl(path):
            payload = d.get("payload") or {}
            info = payload.get("info") or {}
            usage = info.get("total_token_usage")
            if usage:
                # Cumulative: each line supersedes the previous one.
                last_usage = usage
                last_dt = parse_ts(d.get("timestamp")) or last_dt
            limits = payload.get("rate_limits") or {}
            primary = limits.get("primary") or {}
            if primary.get("window_minutes") == WEEKLY_WINDOW_MINUTES:
                used = primary.get("used_percent")
                dt = parse_ts(d.get("timestamp"))
                if used is not None and dt is not None:
                    rate.append((dt, float(used)))
        if not last_usage or last_dt is None:
            continue
        sessions += 1
        wk = weeks[iso_week(last_dt)]
        # total_tokens already equals input + output, and cached_input_tokens is
        # a subset of input. Weight only that subset; adding it would double-count.
        total_tokens = last_usage.get("total_tokens") or 0
        cached = last_usage.get("cached_input_tokens") or 0
        weighted_total = (total_tokens if cache_weight == 1.0 else
                          total_tokens - cached + cached * cache_weight)
        wk["total"] += weighted_total
        wk["cached"] += cached
        wk["output"] += last_usage.get("output_tokens") or 0

    rate.sort()
    return {"weeks": weeks, "rate": rate, "sessions": sessions}


def pct(part, whole) -> str:
    return f"{100 * part / whole:.1f}%" if whole else "--"


def render(claude, codex, days: int, ceiling: int, top: int, cache_weight=1.0) -> str:
    out = [f"# Usage split - Claude vs Codex, last {days} days", ""]
    if cache_weight != 1.0:
        out += [f"> Totals use a cache-read weight of **{cache_weight:g}**. Context and "
                "context-ceiling figures remain raw window size.", ""]

    all_weeks = sorted(set(claude["weeks"]) | set(codex["weeks"]))
    out += ["## Weekly split", "",
            "| week | Claude | Codex | Claude share |",
            "|---|---:|---:|---:|"]
    for wk in all_weeks:
        c = claude["weeks"][wk]["total"]
        x = codex["weeks"][wk]["total"]
        out.append(f"| {wk} | {c/1e6:,.0f}M | {x/1e6:,.0f}M | {pct(c, c + x)} |")
    ct = sum(w["total"] for w in claude["weeks"].values())
    raw_ct = claude.get("raw_total", ct)
    xt = sum(w["total"] for w in codex["weeks"].values())
    out += [f"| **total** | **{ct/1e6:,.0f}M** | **{xt/1e6:,.0f}M** | **{pct(ct, ct + xt)}** |", ""]
    if xt == 0 and ct > 0:
        out += ["> Codex recorded no usage in this window. If that is not "
                "deliberate, it has fallen out of rotation.", ""]

    out += ["## Claude: controller vs dispatched subagent", "",
            "| week | controller | subagent | controller share |",
            "|---|---:|---:|---:|"]
    for wk in sorted(claude["weeks"]):
        w = claude["weeks"][wk]
        out.append(f"| {wk} | {w['controller']/1e6:,.0f}M | {w['subagent']/1e6:,.0f}M "
                   f"| {pct(w['controller'], w['total'])} |")
    out.append("")

    out += ["## Claude: by model", "",
            "| model | lane | tokens | share | cache read | output |",
            "|---|---|---:|---:|---:|---:|"]
    rows = sorted(claude["models"].items(), key=lambda kv: -kv[1]["total"])[:top]
    for (model, lane), m in rows:
        out.append(f"| {model} | {lane} | {m['total']/1e6:,.0f}M | {pct(m['total'], ct)} "
                   f"| {pct(m['cache_read'], m['total'])} | {pct(m['output'], m['total'])} |")
    out.append("")

    ctxs = sorted(claude["contexts"])
    if ctxs:
        n = len(ctxs)
        out += ["## Cost drivers", "",
                f"- Controller turns: **{n:,}**",
                f"- Context per turn: median **{statistics.median(ctxs)/1000:,.0f}k**, "
                f"p90 **{ctxs[int(n * 0.9)]/1000:,.0f}k**, max **{ctxs[-1]/1000:,.0f}k**",
                ""]

    over = [(sid, s) for sid, s in claude["sessions"].items() if s["ctx_max"] >= ceiling]
    over.sort(key=lambda kv: -kv[1]["tokens"])
    out += [f"## Sessions past the {ceiling/1000:,.0f}k context ceiling", ""]
    if not over:
        out += ["None. Every session stayed under the ceiling.", ""]
    else:
        share = sum(s["tokens"] for _, s in over)
        out += [f"{len(over)} of {len(claude['sessions'])} sessions, "
                f"{pct(share, raw_ct)} of all Claude tokens.", "",
                "| session | project | model | turns | context in -> peak | tokens |",
                "|---|---|---|---:|---|---:|"]
        for sid, s in over[:top]:
            out.append(f"| {str(sid)[:8]} | {s['project'][-24:]} | {s['model']} | {s['turns']:,} "
                       f"| {(s['ctx_first'] or 0)/1000:,.0f}k -> {s['ctx_max']/1000:,.0f}k "
                       f"| {s['tokens']/1e6:,.0f}M |")
        out.append("")

    out += ["## Codex", ""]
    if codex["sessions"]:
        out.append(f"- Sessions with recorded usage: **{codex['sessions']:,}**")
    if codex["rate"]:
        dt, used = codex["rate"][-1]
        out.append(f"- Weekly rate limit last seen at **{used:.0f}%** used "
                   f"({dt.strftime('%Y-%m-%d %H:%M')}Z)")
        peak_dt, peak = max(codex["rate"], key=lambda kv: kv[1])
        out.append(f"- Peak in window: **{peak:.0f}%** ({peak_dt.strftime('%Y-%m-%d')})")
    else:
        out.append("- No weekly rate-limit reading in this window.")
    out += ["",
            "> Claude Code's transcripts record no quota percentage, so there is no "
            "matching row for it - the Claude columns above are token counts, not "
            "limit consumption. Neither vendor's subscription weighting is public: "
            "cache reads bill well below fresh input, so raw tokens overstate "
            "Claude's true share of its own limit.", ""]
    return "\n".join(out)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--days", type=int, default=30, help="lookback window (default 30)")
    ap.add_argument("--ceiling", type=int, default=250_000,
                    help="session context ceiling in tokens (default 250000)")
    ap.add_argument("--project", help="only Claude projects whose directory name contains this")
    ap.add_argument("--json", dest="json_path", help="also dump raw aggregates to this JSON file")
    ap.add_argument("--top", type=int, default=15, help="rows per section (default 15)")
    ap.add_argument("--cache-weight", type=float, default=1.0,
                    help="bill cache reads at this weight (default 1.0)")
    args = ap.parse_args()

    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")

    claude = collect_claude(args.days, args.project, cache_weight=args.cache_weight)
    codex = collect_codex(args.days, cache_weight=args.cache_weight)

    if args.json_path:
        payload = {
            "days": args.days,
            "ceiling": args.ceiling,
            "claude_weeks": {k: dict(v) for k, v in claude["weeks"].items()},
            "claude_models": {f"{m}|{lane}": dict(v)
                              for (m, lane), v in claude["models"].items()},
            "claude_sessions": {str(k): v for k, v in claude["sessions"].items()},
            "codex_weeks": {k: dict(v) for k, v in codex["weeks"].items()},
            "codex_rate": [(dt.isoformat(), used) for dt, used in codex["rate"]],
        }
        with open(args.json_path, "w", encoding="utf-8") as fh:
            json.dump(payload, fh, indent=1, ensure_ascii=False)

    print(render(claude, codex, args.days, args.ceiling, args.top, args.cache_weight))
    return 0


if __name__ == "__main__":
    sys.exit(main())
