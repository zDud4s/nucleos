"""`tree.py plan`: split N items into a balanced tree of at most W children per node."""
from __future__ import annotations

from tree_state import NODES_DEFAULTS, TreeError


def _split(items: list[int], width: int, levels: int) -> dict:
    """Build a subtree of exactly `levels` levels over `items` (balanced, <= width per node)."""
    if levels == 1:
        return {"items": items}
    # Capacity of one child subtree is width**(levels-1); use as few children as that allows.
    cap = width ** (levels - 1)
    count = min(width, -(-len(items) // cap))
    base, extra = divmod(len(items), count)
    children, start = [], 0
    for i in range(count):
        size = base + (1 if i < extra else 0)
        children.append(_split_adaptive(items[start:start + size], width, levels - 1))
        start += size
    return {"children": children}


def _split_adaptive(items: list[int], width: int, max_levels: int) -> dict:
    if len(items) <= width:
        return {"items": items}
    levels = 2
    while width ** levels < len(items):
        levels += 1
    return _split(items, width, min(levels, max_levels))


def _depth(node: dict) -> int:
    if "items" in node:
        return 1
    return 1 + max(_depth(c) for c in node["children"])


def build_plan(n: int, w: int, d: int) -> dict:
    if n < 1:
        raise TreeError(f"--items must be at least 1, got {n}")
    if w < 2:
        raise TreeError(f"--max-width must be at least 2, got {w}")
    if d < 1 or n > w ** d:
        raise TreeError(f"{n} items do not fit in {d} level(s) of width {w} (at most {w ** d if d >= 1 else 0})")
    tree = _split_adaptive(list(range(n)), w, d)
    return {"verb": "plan", "items": n, "max_width": w, "max_depth": d,
            "depth": _depth(tree), "tree": tree}


def run(args) -> dict:
    return build_plan(args.items, args.max_width, args.max_depth)


def register(verbs) -> None:
    p = verbs.add_parser("plan", help="split N items into a balanced tree of width <= W")
    p.add_argument("--items", type=int, required=True)
    p.add_argument("--max-width", type=int, required=True)
    p.add_argument("--max-depth", type=int, default=NODES_DEFAULTS["max_depth"])
    p.set_defaults(func=run)
