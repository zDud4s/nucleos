#!/usr/bin/env python3
"""Initial context of a tree LEAF run, before vs after a cut-over (spec 2026-11-09-arvores-autonomia, criterion 12).

Read-only over the daemon database. For every run of a `work_nodes.kind = 'leaf'` node (via
`work_node_runs`), the initial context is the first `assistant` run event's `message.usage`:
input_tokens + cache_creation_input_tokens + cache_read_input_tokens, i.e. the prompt plus the
inherited files as the model saw them at its first turn. Runs are bucketed by `runs.created_at`
against --cut (before / on-or-after). Wave 4 landed on 2026-11-11 (master 5526e0fb..61b2a812), so
the default cut is 2026-11-11T06:00:00Z and --from (default 2026-10-29) bounds the "before" bucket
(--before-until, default 2026-11-10, excludes the days the wave was being built).

Usage: python scripts/leaf_initial_context.py [--db PATH] [--cut ISO] [--before-until ISO] [--from ISO]
"""
import argparse
import json
import os
import sqlite3
import statistics
import sys

DEFAULT_DB = os.path.join(
    os.environ.get("LOCALAPPDATA", ""), "nucleos", "NucleOS", "data", "nucleos.db"
)


def connect_ro(path):
    return sqlite3.connect("file:" + path.replace("\\", "/") + "?mode=ro", uri=True)


def first_usage_context(payload):
    try:
        u = json.loads(payload)["message"]["usage"]
        return (
            (u.get("input_tokens") or 0)
            + (u.get("cache_creation_input_tokens") or 0)
            + (u.get("cache_read_input_tokens") or 0)
        )
    except (ValueError, KeyError, TypeError):
        return None


def leaf_runs(conn, since):
    return conn.execute(
        "SELECT r.id, r.created_at FROM runs r WHERE r.created_at >= ? AND r.id IN ("
        "SELECT wnr.run_id FROM work_node_runs wnr JOIN work_nodes n ON n.id = wnr.node_id "
        "WHERE n.kind = 'leaf')",
        (since,),
    ).fetchall()


def measure(conn, since, before_until, cut):
    before, after = [], []
    for run_id, created in leaf_runs(conn, since):
        if before_until <= created < cut:
            continue  # the build-up days of the wave belong to neither bucket
        row = conn.execute(
            "SELECT payload FROM run_events WHERE run_id = ? AND kind = 'assistant' "
            "ORDER BY seq LIMIT 1",
            (run_id,),
        ).fetchone()
        ctx = first_usage_context(row[0]) if row else None
        if ctx:
            (after if created >= cut else before).append(ctx)
    return before, after


def stat(xs):
    return {"n": len(xs), "median": statistics.median(xs) if xs else None}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--db", default=DEFAULT_DB)
    ap.add_argument("--from", dest="since", default="2026-10-29")
    ap.add_argument("--before-until", default="2026-11-10")
    ap.add_argument("--cut", default="2026-11-11T06:00:00")
    a = ap.parse_args(argv)
    before, after = measure(connect_ro(a.db), a.since, a.before_until, a.cut)
    b, f = stat(before), stat(after)
    change = (
        round((f["median"] - b["median"]) / b["median"] * 100, 1)
        if b["median"] and f["median"] is not None
        else None
    )
    print(f"before  n={b['n']}  median={b['median']}")
    print(f"after   n={f['n']}  median={f['median']}")
    print(f"change  {change}%")
    print(json.dumps({"before": b, "after": f, "change_pct": change, "cut": a.cut}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
