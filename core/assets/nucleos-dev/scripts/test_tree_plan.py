"""RED tests for `tree.py plan --items N --max-width W` (module tree_plan).

Contract the GREEN must implement:
  tree_plan.register(verbs)            -> verbs.add_parser("plan"), args --items, --max-width,
                                          optional --max-depth, set_defaults(func=run)
  tree_plan.run(args) -> dict          -> {"verb": "plan", "items": N, "max_width": W,
                                          "max_depth": D, "depth": d, "tree": node}
  tree_plan.build_plan(n, w, d) -> dict (same dict, pure function)
A node is either a leaf group  {"items": [int, ...]}  (item ids 0..N-1, len <= W)
or an orchestrator            {"children": [node, ...]} (1 <= len <= W).
The root is always a node; N <= W gives a single leaf-group root (depth 1).
Invalid input (N < 1, W < 2, N > W**max_depth) raises tree_state.TreeError.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))

import tree  # noqa: E402
import tree_plan  # noqa: E402
import tree_state as ts  # noqa: E402


def run_plan(items, width, depth=None):
    """Drive the verb through the same registration path tree.py uses."""
    verbs = tree.Verbs()
    tree_plan.register(verbs)
    argv = ["plan", "--items", str(items), "--max-width", str(width)]
    if depth is not None:
        argv += ["--max-depth", str(depth)]
    args = tree.build_parser(verbs).parse_args(argv)
    return args.func(args)


def leaf_groups(node):
    if "items" in node:
        return [node["items"]]
    return [g for child in node["children"] for g in leaf_groups(child)]


def depth_of(node):
    if "items" in node:
        return 1
    return 1 + max(depth_of(c) for c in node["children"])


def all_nodes(node):
    yield node
    for child in node.get("children", []):
        yield from all_nodes(child)


def count_items(node):
    return sum(len(g) for g in leaf_groups(node))


def test_defaults_come_from_tree_state():
    out = run_plan(3, 6)
    assert out["verb"] == "plan"
    assert out["max_depth"] == ts.NODES_DEFAULTS["max_depth"]


def test_n_not_above_width_is_single_level():
    out = run_plan(4, 6)
    assert out["depth"] == 1
    assert out["tree"] == {"items": [0, 1, 2, 3]}


def test_n_equal_width_is_single_level():
    out = run_plan(6, 6)
    assert out["depth"] == 1
    assert out["tree"]["items"] == list(range(6))


def test_n_above_width_groups_balanced():
    out = run_plan(14, 4)
    root = out["tree"]
    assert "children" in root
    assert len(root["children"]) <= 4
    sizes = [count_items(c) for c in root["children"]]
    assert max(sizes) - min(sizes) <= 1
    for group in leaf_groups(root):
        assert 1 <= len(group) <= 4


@pytest.mark.parametrize("n,w", [(7, 6), (13, 3), (36, 6), (37, 6), (100, 5), (27, 3)])
def test_every_item_exactly_once_and_widths_respected(n, w):
    out = run_plan(n, w)
    seen = [i for g in leaf_groups(out["tree"]) for i in g]
    assert sorted(seen) == list(range(n))
    for node in all_nodes(out["tree"]):
        if "children" in node:
            assert 1 <= len(node["children"]) <= w
        else:
            assert 1 <= len(node["items"]) <= w


@pytest.mark.parametrize("n,w", [(13, 3), (36, 6), (100, 5), (27, 3)])
def test_siblings_balanced_at_every_level(n, w):
    out = run_plan(n, w)
    for node in all_nodes(out["tree"]):
        if "children" in node:
            sizes = [count_items(c) for c in node["children"]]
            assert max(sizes) - min(sizes) <= 1


@pytest.mark.parametrize("n,w,d", [(7, 2, 3), (8, 2, 3), (27, 3, 3), (36, 6, 2), (6, 6, 1)])
def test_depth_never_exceeds_max_depth(n, w, d):
    out = run_plan(n, w, d)
    assert out["depth"] == depth_of(out["tree"])
    assert out["depth"] <= d
    assert out["max_depth"] == d


def test_too_many_items_for_depth_errors():
    # 2 levels of width 3 hold at most 9 items.
    with pytest.raises(ts.TreeError):
        run_plan(10, 3, 2)


def test_zero_items_errors():
    with pytest.raises(ts.TreeError):
        run_plan(0, 6)


@pytest.mark.parametrize("n,w", [(-1, 6), (5, 0), (5, 1), (5, -2)])
def test_invalid_args_error(n, w):
    with pytest.raises(ts.TreeError):
        run_plan(n, w)


def test_cli_prints_json_and_error_exit(capsys):
    assert tree.main(["plan", "--items", "5", "--max-width", "6"]) == 0
    assert json.loads(capsys.readouterr().out)["tree"] == {"items": [0, 1, 2, 3, 4]}
    assert tree.main(["plan", "--items", "0", "--max-width", "6"]) == 2
    assert "error" in json.loads(capsys.readouterr().out)
