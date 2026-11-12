"""RED tests for tree_pending: the local queue of closes that could not reach the daemon.

Run: python -m pytest core/assets/nucleos-dev/scripts/test_tree_pending.py
"""
import argparse
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import tree_pending  # noqa: E402
import tree_state  # noqa: E402


class PendingTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.base = Path(self._tmp.name)
        # tree_pending must reach the tree directory through `tree_state.tree_dir` (attribute lookup
        # at call time), so patching it redirects every read and write to the temp state dir.
        patcher = mock.patch.object(
            tree_state, "tree_dir", lambda tree_id, root=None: self.base / "trees" / tree_id
        )
        patcher.start()
        self.addCleanup(patcher.stop)

    def pending_dir(self, tree="t1"):
        return self.base / "trees" / tree / "pending"

    def add(self, node="n1", transition="close", tool="node_close", payload=None, tree="t1"):
        return tree_pending.run_add(
            argparse.Namespace(
                tree=tree,
                node=node,
                transition=transition,
                tool=tool,
                payload=json.dumps(payload if payload is not None else {"node_id": node}),
            )
        )

    def listing(self, tree="t1"):
        return tree_pending.run_list(argparse.Namespace(tree=tree))

    def clear(self, tree="t1", node=None, transition=None, all_=False):
        return tree_pending.run_clear(
            argparse.Namespace(tree=tree, node=node, transition=transition, all=all_)
        )

    def test_backoff_delays_are_three_retries_doubling(self):
        self.assertEqual(tuple(tree_pending.backoff_delays()), (1, 2, 4))

    def test_add_writes_seq_tool_json_under_the_tree_pending_dir(self):
        self.add(node="n1", tool="node_close", payload={"node_id": "n1", "status": "done"})
        files = sorted(p.name for p in self.pending_dir().iterdir())
        self.assertEqual(len(files), 1)
        name = files[0]
        self.assertTrue(name.endswith("-node_close.json"), name)
        self.assertTrue(name.split("-", 1)[0].isdigit(), name)
        stored = json.loads((self.pending_dir() / name).read_text(encoding="utf-8"))
        self.assertEqual(stored["payload"], {"node_id": "n1", "status": "done"})
        self.assertEqual(stored["node"], "n1")
        self.assertEqual(stored["transition"], "close")
        self.assertEqual(stored["tool"], "node_close")

    def test_add_is_idempotent_per_node_and_transition(self):
        self.add(node="n1", transition="close", payload={"v": 1})
        self.add(node="n1", transition="close", payload={"v": 2})
        self.assertEqual(len(list(self.pending_dir().iterdir())), 1)
        self.assertEqual(len(self.listing()["pending"]), 1)

    def test_same_node_other_transition_is_a_different_key(self):
        self.add(node="n1", transition="close")
        self.add(node="n1", transition="fail", tool="node_fail")
        self.assertEqual(len(self.listing()["pending"]), 2)

    def test_list_returns_pending_in_seq_order(self):
        self.add(node="a", transition="close")
        self.add(node="b", transition="close")
        self.add(node="c", transition="close")
        entries = self.listing()["pending"]
        self.assertEqual([e["node"] for e in entries], ["a", "b", "c"])
        seqs = [e["seq"] for e in entries]
        self.assertEqual(seqs, sorted(seqs))
        self.assertEqual(len(set(seqs)), 3)

    def test_seq_sorts_numerically_not_lexically(self):
        for i in range(11):
            self.add(node=f"n{i}")
        nodes = [e["node"] for e in self.listing()["pending"]]
        self.assertEqual(nodes, [f"n{i}" for i in range(11)])

    def test_list_of_a_tree_without_pending_is_empty(self):
        self.assertEqual(self.listing(tree="none")["pending"], [])

    def test_clear_removes_one_entry_by_key(self):
        self.add(node="a")
        self.add(node="b")
        self.clear(node="a", transition="close")
        self.assertEqual([e["node"] for e in self.listing()["pending"]], ["b"])

    def test_clear_all_removes_everything(self):
        self.add(node="a")
        self.add(node="b")
        self.clear(all_=True)
        self.assertEqual(self.listing()["pending"], [])

    def test_clear_of_a_missing_entry_is_not_an_error(self):
        self.clear(node="ghost", transition="close")
        self.assertEqual(self.listing()["pending"], [])

    def test_trees_are_isolated(self):
        self.add(node="a", tree="t1")
        self.assertEqual(self.listing(tree="t2")["pending"], [])

    def test_register_adds_a_pending_verb_with_add_list_clear(self):
        verbs = {}
        tree_pending.register(verbs)
        self.assertIn("pending", verbs)
        configure, run = verbs["pending"]
        parser = argparse.ArgumentParser()
        configure(parser)
        args = parser.parse_args(["list", "--tree", "t1"])
        self.assertEqual(args.pending_action, "list")
        self.add(node="a")
        self.assertEqual(len(run(args)["pending"]), 1)

    def test_register_cli_add_then_clear_all(self):
        verbs = {}
        tree_pending.register(verbs)
        configure, run = verbs["pending"]
        parser = argparse.ArgumentParser()
        configure(parser)
        run(parser.parse_args([
            "add", "--tree", "t1", "--node", "n9", "--transition", "close",
            "--tool", "node_close", "--payload", "{}",
        ]))
        self.assertEqual(len(self.listing()["pending"]), 1)
        run(parser.parse_args(["clear", "--tree", "t1", "--all"]))
        self.assertEqual(self.listing()["pending"], [])


if __name__ == "__main__":
    unittest.main()
