#!/usr/bin/env python3
"""Rate of tree jobs closed without owner intervention (spec 2026-11-09-arvores-autonomia, criterion 31).

Read-only over the daemon database. A tree job is "terminated" when `tree_jobs.state` is not `open`
and `updated_at` falls in the window; it is "without intervention" when none of its decisions
(`decisions.tree_id = tree_jobs.id`) carries an owner answer. An owner answer is a decision with a
non-null `answer` whose `origin` is not in --machine-origins (default: auto_default, agent).
`driver` and `shell` count as the owner: both are an owner-side session answering.

Also reports blocked/failed nodes (`work_nodes.tree_job_id`) of the terminated jobs against their total.

Usage: python scripts/tree_autonomy_rate.py [--db PATH] [--days N | --since ISO] [--machine-origins a,b]
"""
import argparse
import json
import os
import sqlite3
import sys
from datetime import datetime, timedelta, timezone

DEFAULT_DB = os.path.join(
    os.environ.get("LOCALAPPDATA", ""), "nucleos", "NucleOS", "data", "nucleos.db"
)
MACHINE_ORIGINS = ("auto_default", "agent")


def connect_ro(path):
    uri = "file:" + path.replace("\\", "/") + "?mode=ro"
    return sqlite3.connect(uri, uri=True)


def measure(conn, since_iso, machine_origins=MACHINE_ORIGINS):
    jobs = conn.execute(
        "SELECT id FROM tree_jobs WHERE state <> 'open' AND updated_at >= ?", (since_iso,)
    ).fetchall()
    ids = [r[0] for r in jobs]
    marks = ",".join("?" * len(ids)) or "NULL"
    intervened = set()
    if ids:
        orig = ",".join("?" * len(machine_origins)) or "NULL"
        rows = conn.execute(
            f"SELECT DISTINCT tree_id FROM decisions WHERE tree_id IN ({marks}) "
            f"AND answer IS NOT NULL AND COALESCE(origin, '') NOT IN ({orig})",
            (*ids, *machine_origins),
        ).fetchall()
        intervened = {r[0] for r in rows}
    nodes = {"total": 0, "blocked": 0, "failed": 0}
    if ids:
        for status, n in conn.execute(
            f"SELECT status, COUNT(*) FROM work_nodes WHERE tree_job_id IN ({marks}) GROUP BY status",
            ids,
        ):
            nodes["total"] += n
            if status in ("blocked", "failed"):
                nodes[status] = n
    terminated = len(ids)
    autonomous = terminated - len(intervened)
    return {
        "since": since_iso,
        "terminated_jobs": terminated,
        "without_intervention": autonomous,
        "rate": round(autonomous / terminated, 4) if terminated else None,
        "nodes_total": nodes["total"],
        "nodes_blocked": nodes["blocked"],
        "nodes_failed": nodes["failed"],
    }


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--db", default=DEFAULT_DB)
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--days", type=float, default=7)
    g.add_argument("--since", help="ISO timestamp; overrides --days")
    ap.add_argument("--machine-origins", default=",".join(MACHINE_ORIGINS))
    a = ap.parse_args(argv)
    since = a.since or (datetime.now(timezone.utc) - timedelta(days=a.days)).isoformat()
    origins = tuple(o for o in a.machine_origins.split(",") if o)
    res = measure(connect_ro(a.db), since, origins)
    print(f"window since {res['since']}")
    print(f"terminated jobs       {res['terminated_jobs']}")
    print(f"without intervention  {res['without_intervention']}  (rate {res['rate']})")
    print(
        f"nodes blocked+failed  {res['nodes_blocked']}+{res['nodes_failed']} of {res['nodes_total']}"
    )
    print(json.dumps(res))
    return 0


if __name__ == "__main__":
    sys.exit(main())
