"""Local queue of node closes that could not reach the daemon.

Entries live in <tree dir>/pending/<seq>-<tool>.json and are replayed later. One entry per
(node, transition): adding the same key again is a no-op.
"""
from __future__ import annotations

import json

import tree_state as ts

MAX_ENTRY_BYTES = 256 * 1024
MAX_ENTRIES = 1000
_RETRIES = 3


def backoff_delays() -> tuple[int, ...]:
    """Seconds to wait before each retry: three retries, doubling."""
    return tuple(2 ** i for i in range(_RETRIES))


def _pending_dir(tree_id: str):
    # Attribute lookup at call time, so tests can patch ts.tree_dir.
    return ts.tree_dir(tree_id) / "pending"


def _read_entry(path):
    try:
        with open(path, "rb") as fh:
            raw = fh.read(MAX_ENTRY_BYTES + 1)
        if len(raw) > MAX_ENTRY_BYTES:
            return None
        data = json.loads(raw.decode("utf-8"))
    except (OSError, ValueError):
        return None
    if not isinstance(data, dict) or not isinstance(data.get("seq"), int):
        return None
    return data


def _entries(tree_id: str) -> list[tuple]:
    base = _pending_dir(tree_id)
    if not base.is_dir():
        return []
    out = []
    for path in base.glob("*.json"):
        data = _read_entry(path)
        if data is not None:
            out.append((data, path))
    out.sort(key=lambda item: item[0]["seq"])
    return out[:MAX_ENTRIES]


def list_entries(tree_id: str) -> list[dict]:
    """The queued entries of a tree, oldest first (the public face of `_entries`)."""
    return [data for data, _path in _entries(tree_id)]


def run_add(args) -> dict:
    tree_id = ts.resolve_tree_id(args)
    ts.check_id(args.tool, "tool")
    ts.check_id(args.node, "node")
    entries = _entries(tree_id)
    for data, _path in entries:
        if data.get("node") == args.node and data.get("transition") == args.transition:
            return {"added": False, "seq": data["seq"], "pending": len(entries)}
    try:
        payload = json.loads(args.payload) if isinstance(args.payload, str) else args.payload
    except ValueError:
        payload = args.payload
    seq = max((d["seq"] for d, _ in entries), default=0) + 1
    base = _pending_dir(tree_id)
    base.mkdir(parents=True, exist_ok=True)
    entry = {"seq": seq, "node": args.node, "transition": args.transition,
             "tool": args.tool, "payload": payload}
    (base / f"{seq}-{args.tool}.json").write_text(json.dumps(entry), encoding="utf-8")
    return {"added": True, "seq": seq, "pending": len(entries) + 1}


def run_list(args) -> dict:
    return {"pending": [d for d, _ in _entries(ts.resolve_tree_id(args))]}


def run_clear(args) -> dict:
    tree_id = ts.resolve_tree_id(args)
    removed = 0
    for data, path in _entries(tree_id):
        if getattr(args, "all", False) or (
            data.get("node") == args.node and data.get("transition") == args.transition
        ):
            try:
                path.unlink()
                removed += 1
            except OSError:
                pass
    return {"removed": removed}


def configure(parser) -> None:
    sub = parser.add_subparsers(dest="pending_action", required=True)
    add = sub.add_parser("add", help="queue a close that could not reach the daemon")
    ts.add_tree_arg(add)
    add.add_argument("--node", required=True)
    add.add_argument("--transition", required=True)
    add.add_argument("--tool", required=True)
    add.add_argument("--payload", required=True, help="JSON payload to replay")
    lst = sub.add_parser("list", help="list queued entries in seq order")
    ts.add_tree_arg(lst)
    clr = sub.add_parser("clear", help="drop one entry by key, or all")
    ts.add_tree_arg(clr)
    clr.add_argument("--node")
    clr.add_argument("--transition")
    clr.add_argument("--all", action="store_true")


def run(args) -> dict:
    action = args.pending_action
    if action == "add":
        return run_add(args)
    if action == "list":
        return run_list(args)
    return run_clear(args)


def register(verbs: dict) -> None:
    verbs["pending"] = (configure, run)
