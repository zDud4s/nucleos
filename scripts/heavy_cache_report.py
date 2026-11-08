#!/usr/bin/env python3
"""Does the heavy broker's result cache pay off? Reads log.jsonl and prints a short summary.

    python scripts/heavy_cache_report.py [--days N] [--dir STATE_DIR]

State dir: NUCLEOS_HEAVY_DIR, else ~/.nucleos/heavy (the same rule as heavy.py's state_dir()).
Rows written before the cache fields existed are skipped.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import sys
import time
from pathlib import Path

CACHEABLE = ("hit", "miss_none", "miss_src", "miss_toolchain", "miss_env",
             "skip_failfirst", "skip_forced", "skip_full")


def state_dir() -> Path:
    override = os.environ.get("NUCLEOS_HEAVY_DIR")
    return Path(override) if override else Path.home() / ".nucleos" / "heavy"


def _ts(row: dict) -> float | None:
    try:
        return time.mktime(time.strptime(row["ts"][:19], "%Y-%m-%dT%H:%M:%S"))
    except (KeyError, TypeError, ValueError):
        return None


def _num(v) -> float:
    try:
        return float(v)
    except (TypeError, ValueError):
        return 0.0


def _label(row: dict) -> str:
    argv = row.get("argv")
    text = " ".join(str(a) for a in argv) if isinstance(argv, list) else str(argv or row.get("argv0"))
    return f"{text} [{os.path.basename(str(row.get('worktree') or ''))}]"


def load(path: Path, days: float, now: float | None = None) -> list[dict]:
    cutoff = (now if now is not None else time.time()) - days * 86400
    rows = []
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return rows
    for line in lines:
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if not isinstance(row, dict) or "cache" not in row:
            continue
        t = _ts(row)
        if t is not None and t < cutoff:
            continue
        rows.append(row)
    return rows


def summarize(rows: list[dict]) -> dict:
    counts = collections.Counter(r["cache"] for r in rows)
    stores = collections.Counter(r["cache_store"] for r in rows if r.get("cache_store"))
    cacheable = sum(counts[k] for k in CACHEABLE)
    saved = sum(_num(r.get("cache_saved_s")) for r in rows)
    overhead = sum(_num(r.get("cache_key_ms")) + _num(r.get("toolchain_fp_ms")) for r in rows) / 1000
    per_cmd: dict = collections.defaultdict(float)
    for r in rows:
        if r.get("cache_saved_s"):
            per_cmd[_label(r)] += _num(r["cache_saved_s"])
    return {
        "counts": counts, "stores": stores, "cacheable": cacheable,
        "hit_rate": counts["hit"] / cacheable if cacheable else 0.0,
        "saved_s": saved, "overhead_s": overhead, "net_s": saved - overhead,
        "stale_prevented": counts["miss_toolchain"],
        "top": sorted(per_cmd.items(), key=lambda kv: -kv[1])[:5],
    }


def render(s: dict, days: float) -> str:
    out = [f"heavy cache, last {days:g} day(s)"]
    for k, n in sorted(s["counts"].items()):
        out.append(f"  {k:<16}{n}")
    for k, n in sorted(s["stores"].items()):
        out.append(f"  {k:<16}{n}  (after the run)")
    out.append(f"hit rate (cacheable commands): {s['hit_rate'] * 100:.1f}% of {s['cacheable']}")
    out.append(f"time saved:     {s['saved_s']:.1f} s")
    out.append(f"overhead:       {s['overhead_s']:.1f} s (cache_key_ms + toolchain_fp_ms)")
    out.append(f"net benefit:    {s['net_s']:.1f} s")
    out.append(f"stale passes prevented by the toolchain marker: {s['stale_prevented']}")
    if s["top"]:
        out.append("top commands by time saved:")
        for label, secs in s["top"]:
            out.append(f"  {secs:8.1f} s  {label[:100]}")
    return "\n".join(out)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--days", type=float, default=7)
    ap.add_argument("--dir", default=None)
    args = ap.parse_args(argv)
    d = Path(args.dir) if args.dir else state_dir()
    print(render(summarize(load(d / "log.jsonl", args.days)), args.days))
    return 0


if __name__ == "__main__":
    sys.exit(main())
