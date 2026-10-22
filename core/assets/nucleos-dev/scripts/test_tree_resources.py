#!/usr/bin/env python3
"""Tests for tree_resources: budget, open, close, gate (spec 2026-10-21, sections 6-8)."""
import argparse
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import tree_resources as tr  # noqa: E402
from tree_state import TreeError  # noqa: E402


def make_branch(name, layer="core", state="queued", base=None, target=None, worktree=None):
    return {
        "name": name, "paths": [f"{name}/**"], "layer": layer, "base": base, "state": state,
        "git_branch": None, "worktree": worktree, "target_dir": target, "node_id": None,
        "gates": [], "reviews": [], "park": None,
    }


def make_manifest(root, branches, costs=None):
    return {
        "version": 1, "tree_id": "T1", "feature_branch": "feat/x", "goal": "g",
        "created_at": "2026-10-21T00:00:00Z", "state": "open", "project_root": root,
        "dirty_owner_files": [], "branches": {b["name"]: b for b in branches},
        "records": [], "attempts": {}, "costs_gb": dict(costs or {}),
        "disk_samples": [], "park": None,
    }


class Store:
    def __init__(self):
        self.m = None
        self.saves = 0


class FakeRun:
    """Records every command; `fail` maps a substring of the joined command to (rc, output)."""

    def __init__(self, fail=None):
        self.calls = []
        self.fail = fail or {}

    def __call__(self, cmd, cwd=None):
        self.calls.append(list(cmd))
        line = " ".join(str(c) for c in cmd)
        for needle, result in self.fail.items():
            if needle in line:
                return result
        return 0, ""


class Env(unittest.TestCase):
    """Temp project root and dev home, with every measurement seam replaced."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.root = os.path.join(self.tmp, "proj")
        self.home = os.path.join(self.tmp, "dev")
        os.makedirs(self.root)
        os.makedirs(os.path.join(self.home, "cargo-target"))
        self.free_dev = 100.0
        self.free_root = 100.0
        self.sizes = {}
        self.heavy = None
        self.store = Store()
        self.cfg = {
            "floor_gb": 15, "max_live_branches": 4, "seed_gb_per_layer": 8,
            "worktrees_dir": ".claude/worktrees", "links": ["shell/node_modules"], "heavy": None,
        }
        self.fake = FakeRun()

        def free(path):
            return self.free_root if str(path) == self.root else self.free_dev

        def load_manifest(tid):
            return self.store.m

        def save_manifest(tid, m):
            self.store.m = m
            self.store.saves += 1

        for name, value in {
            "disk_free_gb": free,
            "dir_size_gb": lambda p: self.sizes.get(str(p), 0.0),
            "heavy_status": lambda cfg, root: self.heavy,
            "load_config": lambda: self.cfg,
            "dev_home": lambda: self.home,
            "load_manifest": load_manifest,
            "save_manifest": save_manifest,
            "run": self.fake,
            "find_core": lambda: None,
            "IS_WINDOWS": True,
        }.items():
            patcher = mock.patch.object(tr, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)

    def put(self, branches, costs=None):
        self.store.m = make_manifest(self.root, branches, costs)

    def args(self, **kw):
        base = {"tree": "T1"}
        base.update(kw)
        return argparse.Namespace(**base)


class BudgetTest(Env):
    def budget(self):
        return tr.compute_budget(self.cfg, self.store.m, self.root, tr._free(self.cfg, self.root))

    def test_slack_discounts_only_the_growth_still_to_come(self):
        # open branch x already holds 3 GB of its 8 GB seed: 5 GB still to grow.
        self.put([
            make_branch("x", state="building", target="X"),
            make_branch("a"), make_branch("b"), make_branch("c"),
        ])
        self.sizes["X"] = 3.0
        self.free_dev = self.free_root = 40.0
        res = self.budget()
        self.assertEqual(res["slack_gb"], 20.0)  # 40 - 15 - 5
        self.assertEqual(res["can_open"], ["a", "b"])  # 8 + 8 fit in 20, a third does not
        self.assertTrue(any("c" in r and "disco" in r for r in res["reasons"]))

    def test_uses_the_smaller_of_the_two_volumes(self):
        self.put([make_branch("a")])
        self.free_dev, self.free_root = 50.0, 20.0
        self.assertEqual(self.budget()["free_gb"], 20.0)

    def test_below_floor_opens_nothing_and_suggests_merged_target_dirs(self):
        self.put([
            make_branch("done", state="merged", target="D"),
            make_branch("a"),
        ])
        self.free_dev = 10.0
        res = self.budget()
        self.assertEqual(res["can_open"], [])
        self.assertEqual(res["gates"], 0)
        self.assertTrue(any("disco" in r for r in res["reasons"]))
        self.assertEqual(res["cleanup"], ["D"])

    def test_measured_cost_replaces_the_seed(self):
        self.put([make_branch("a"), make_branch("b"), make_branch("c")], costs={"core": 4.0})
        self.free_dev = self.free_root = 28.0  # slack 13: 3 branches at 4 GB fit
        self.assertEqual(self.budget()["can_open"], ["a", "b", "c"])

    def test_live_branch_cap(self):
        self.cfg["max_live_branches"] = 2
        self.put([make_branch("x", state="open"), make_branch("a"), make_branch("b")])
        res = self.budget()
        self.assertEqual(res["can_open"], ["a"])
        self.assertTrue(any("max_live_branches" in r for r in res["reasons"]))

    def test_branch_waits_for_its_base_to_be_merged(self):
        self.put([make_branch("a"), make_branch("b", base="a")])
        self.assertEqual(self.budget()["can_open"], ["a"])
        self.store.m["branches"]["a"]["state"] = "merged"
        self.assertEqual(self.budget()["can_open"], ["b"])

    def test_busy_heavy_slot_lowers_gates(self):
        self.put([make_branch("a")])
        self.heavy = {"capacity": 4, "held_weight": 2, "queue": 0, "locks": []}
        self.assertEqual(self.budget()["gates"], 1)
        self.heavy = {"capacity": 4, "held_weight": 4, "queue": 1, "locks": []}
        self.assertEqual(self.budget()["gates"], 0)

    def test_without_heavy_one_gate_and_no_known_lock(self):
        self.put([make_branch("a")])
        res = self.budget()
        self.assertEqual(res["gates"], 1)
        self.assertFalse(res["land_waiting"])

    def test_foreign_lock_on_the_project_root_marks_land_waiting(self):
        self.put([make_branch("a")])
        shouty = self.root.upper().replace("\\", "/") + "/"
        self.heavy = {"capacity": 4, "held_weight": 2, "queue": 0,
                      "locks": [{"hash": "h", "worktree": shouty, "pid": 999999, "alive": True}]}
        res = self.budget()
        self.assertTrue(res["land_waiting"])
        self.assertTrue(any("land" in r for r in res["reasons"]))

    def test_dead_own_or_other_tree_locks_do_not_block_the_land(self):
        self.put([make_branch("a")])
        for lock in (
            {"worktree": self.root, "pid": 999999, "alive": False},
            {"worktree": self.root, "pid": os.getpid(), "alive": True},
            {"worktree": os.path.join(self.root, ".claude", "worktrees", "T1-a"),
             "pid": 999999, "alive": True},
        ):
            self.heavy = {"capacity": 4, "held_weight": 0, "queue": 0, "locks": [lock]}
            self.assertFalse(self.budget()["land_waiting"], lock)

    def test_run_budget_records_a_disk_sample(self):
        self.put([make_branch("a")])
        self.free_dev, self.free_root = 50.0, 30.0
        res = tr.run_budget(self.args())
        self.assertEqual(res["free_gb"], 30.0)
        sample = self.store.m["disk_samples"][-1]
        self.assertEqual((sample["verb"], sample["free_gb"]), ("budget", 30.0))
        self.assertEqual(self.store.saves, 1)

    def test_land_wait_over_the_limit_tells_the_controller_to_switch_branch(self):
        self.put([make_branch("a")])
        self.cfg["lock_wait_minutes"] = 10
        self.heavy = {"capacity": 4, "held_weight": 2, "queue": 0,
                      "locks": [{"worktree": self.root, "pid": 999999, "alive": True}]}
        clock = {"now": "2026-10-21T10:00:00+00:00"}
        with mock.patch.object(tr, "now_iso", lambda: clock["now"]):
            first = tr.run_budget(self.args())
            self.assertEqual(first["land_wait_minutes"], 0)
            self.assertEqual(self.store.m["land_waiting_since"], clock["now"])
            clock["now"] = "2026-10-21T10:11:00+00:00"
            later = tr.run_budget(self.args())
            self.assertEqual(later["land_wait_minutes"], 11)
            self.assertTrue(any("passar a outro ramo" in r for r in later["reasons"]))
            self.assertEqual(self.store.m["land_waiting_since"], "2026-10-21T10:00:00+00:00")
            self.heavy["locks"] = []  # the lock is gone: the wait ends
            clock["now"] = "2026-10-21T10:12:00+00:00"
            done = tr.run_budget(self.args())
        self.assertEqual(done["land_wait_minutes"], 0)
        self.assertNotIn("land_waiting_since", self.store.m)


class GateTest(Env):
    def log(self, lines=100):
        path = os.path.join(self.tmp, "gate.log")
        Path(path).write_text("\n".join(f"line {i}" for i in range(lines)), encoding="utf-8")
        return path

    def gate(self, name, code, log=None, sha="abc"):
        return tr.run_gate(self.args(branch=name, exit_code=code, log=log or self.log(), sha=sha))

    def test_green_gate_records_tail_sample_and_state(self):
        self.put([make_branch("a", state="building", target="TA")])
        self.free_dev, self.free_root = 60.0, 45.5
        res = self.gate("a", 0)
        b = self.store.m["branches"]["a"]
        rec = b["gates"][0]
        self.assertEqual(b["state"], "green")
        self.assertEqual((rec["exit"], rec["sha"], rec["free_gb"]), (0, "abc", 45.5))
        lines = rec["log_tail"].splitlines()
        self.assertEqual((len(lines), lines[0], lines[-1]), (40, "line 60", "line 99"))
        sample = self.store.m["disk_samples"][-1]
        self.assertEqual((sample["verb"], sample["free_gb"]), ("gate", 45.5))
        self.assertEqual(res["state"], "green")

    def test_red_gate_sets_red_and_does_not_measure_cost(self):
        self.put([make_branch("a", state="gating", target="TA")])
        self.sizes["TA"] = 5.0
        self.gate("a", 101)
        self.assertEqual(self.store.m["branches"]["a"]["state"], "red")
        self.assertNotIn("core", self.store.m["costs_gb"])

    def test_first_green_gate_measures_the_layer_cost_once(self):
        self.put([make_branch("a", state="gating", target="TA"),
                  make_branch("b", state="gating", target="TB")])
        self.sizes.update({"TA": 6.5, "TB": 9.0})
        res = self.gate("a", 0)
        self.assertEqual(res["cost_measured"], 6.5)
        self.gate("b", 0)
        self.assertEqual(self.store.m["costs_gb"]["core"], 6.5)

    def test_empty_target_dir_is_not_recorded_as_a_cost(self):
        self.put([make_branch("a", state="gating", target="TA")])
        self.gate("a", 0)  # size 0.0: directory missing
        self.assertNotIn("core", self.store.m["costs_gb"])

    def test_gate_below_floor_warns(self):
        self.put([make_branch("a", state="gating", target="TA")])
        self.free_dev = 9.0
        res = self.gate("a", 0)
        self.assertTrue(any("disco" in r for r in res["reasons"]))

    def test_unknown_branch_and_missing_log_are_errors(self):
        self.put([make_branch("a", state="gating")])
        with self.assertRaises(TreeError):
            self.gate("nope", 0)
        with self.assertRaises(TreeError):
            self.gate("a", 0, log=os.path.join(self.tmp, "missing.log"))


class OpenTest(Env):
    def setUp(self):
        super().setUp()
        os.makedirs(os.path.join(self.root, "shell", "node_modules"))
        self.put([make_branch("a", layer="core")])
        self.wt = os.path.join(self.root, ".claude", "worktrees", "T1-a")

    def open(self):
        return tr.run_open(self.args(branch="a"))

    def test_open_creates_worktree_link_and_target_dir_in_order(self):
        res = self.open()
        self.assertEqual(Path(res["worktree"]), Path(self.wt))
        self.assertEqual(res["git_branch"], "tree/T1/a")
        self.assertEqual(
            Path(res["env"]["CARGO_TARGET_DIR"]), Path(self.home) / "cargo-target" / "T1-a"
        )
        self.assertEqual(res["orchestrator_piece"]["paths"], ["a/**"])
        calls = self.fake.calls
        self.assertEqual(calls[0][:4], ["git", "rev-parse", "--verify", "--quiet"])
        self.assertEqual(
            calls[1], ["git", "worktree", "add", "-b", "tree/T1/a", self.wt, "feat/x"]
        )
        self.assertEqual(calls[2], [
            "cmd", "/c", "mklink", "/J",
            str(Path(self.wt) / "shell" / "node_modules"),
            str(Path(self.root) / "shell" / "node_modules"),
        ])
        b = self.store.m["branches"]["a"]
        self.assertEqual(b["state"], "open")
        self.assertEqual((b["git_branch"], b["worktree"]), ("tree/T1/a", str(Path(self.wt))))
        self.assertEqual(self.store.m["disk_samples"][-1]["verb"], "budget")

    def test_open_refuses_when_the_budget_does_not_allow_it(self):
        self.free_dev = 10.0
        with self.assertRaises(TreeError) as ctx:
            self.open()
        self.assertIn("disco", str(ctx.exception))
        self.assertEqual(self.fake.calls, [])
        self.assertEqual(self.store.m["branches"]["a"]["state"], "queued")
        self.assertEqual(self.store.saves, 1)  # the disk sample is kept

    def test_open_refuses_a_branch_that_is_not_queued(self):
        self.store.m["branches"]["a"]["state"] = "open"
        with self.assertRaises(TreeError):
            self.open()

    def test_missing_feature_branch_is_created_from_master(self):
        self.fake.fail = {"rev-parse": (1, "")}
        self.open()
        self.assertEqual(self.fake.calls[1], ["git", "branch", "feat/x", "master"])
        self.assertEqual(self.fake.calls[2][:3], ["git", "worktree", "add"])

    def test_link_failure_rolls_the_worktree_back(self):
        self.fake.fail = {"mklink": (1, "denied")}
        with self.assertRaises(TreeError):
            self.open()
        self.assertEqual(self.fake.calls[-2], ["git", "worktree", "remove", "--force", self.wt])
        self.assertEqual(self.fake.calls[-1], ["git", "branch", "-D", "tree/T1/a"])
        b = self.store.m["branches"]["a"]
        self.assertEqual((b["state"], b["worktree"], b["git_branch"]), ("queued", None, None))
        self.assertEqual(self.store.m["disk_samples"][-1]["verb"], "budget")
        self.assertEqual(self.store.saves, 1)

    def test_workflow_sync_runs_when_the_core_binary_is_found(self):
        with mock.patch.object(tr, "find_core", lambda: "/x/nucleos-core"):
            res = self.open()
        self.assertEqual(self.fake.calls[-1], ["/x/nucleos-core", "--workflow-sync", self.wt])
        self.assertEqual(res["workflow_sync"], "ok")

    def test_workflow_sync_failure_does_not_fail_the_open(self):
        self.fake.fail = {"--workflow-sync": (1, "daemon down")}
        with mock.patch.object(tr, "find_core", lambda: "/x/nucleos-core"):
            res = self.open()
        self.assertTrue(res["workflow_sync"].startswith("failed"))
        self.assertEqual(self.store.m["branches"]["a"]["state"], "open")

    def test_missing_link_source_is_skipped(self):
        shutil.rmtree(os.path.join(self.root, "shell"))
        res = self.open()
        self.assertEqual(res["skipped_links"], ["shell/node_modules"])
        self.assertFalse(any("mklink" in " ".join(c) for c in self.fake.calls))


class CloseTest(Env):
    def setUp(self):
        super().setUp()
        self.wt = os.path.join(self.root, ".claude", "worktrees", "T1-a")
        self.link = os.path.join(self.wt, "shell", "node_modules")
        os.makedirs(self.link)
        self.target = os.path.join(self.home, "cargo-target", "T1-a")
        os.makedirs(self.target)
        self.put([make_branch("a", state="merged", target=self.target, worktree=self.wt)])
        self.store.m["branches"]["a"]["git_branch"] = "tree/T1/a"
        # the target dir removal is recorded in the same list as the commands, to show the order
        patcher = mock.patch.object(
            tr, "remove_tree", lambda p: self.fake.calls.append(["rmtree", str(p)])
        )
        patcher.start()
        self.addCleanup(patcher.stop)

    def close(self):
        return tr.run_close(self.args(branch="a"))

    def test_teardown_order_links_worktree_target_branch(self):
        res = self.close()
        self.assertEqual(self.fake.calls, [
            ["cmd", "/c", "rmdir", self.link],
            ["git", "worktree", "remove", self.wt],
            ["rmtree", self.target],
            ["git", "merge-base", "--is-ancestor", "tree/T1/a", "feat/x"],
            ["git", "branch", "-D", "tree/T1/a"],
        ])
        self.assertTrue(res["branch_deleted"])
        b = self.store.m["branches"]["a"]
        self.assertEqual((b["worktree"], b["target_dir"]), (None, None))

    def test_link_removal_failure_aborts_everything_else(self):
        self.fake.fail = {"rmdir": (1, "in use")}
        with self.assertRaises(TreeError):
            self.close()
        self.assertEqual(self.fake.calls, [["cmd", "/c", "rmdir", self.link]])
        b = self.store.m["branches"]["a"]
        self.assertEqual((b["worktree"], b["target_dir"]), (self.wt, self.target))
        self.assertEqual(self.store.saves, 0)

    def test_worktree_remove_failure_keeps_target_dir_and_branch(self):
        self.fake.fail = {"worktree remove": (1, "dirty")}
        with self.assertRaises(TreeError):
            self.close()
        self.assertEqual(len(self.fake.calls), 2)
        self.assertEqual(self.store.saves, 0)

    def test_dropped_branch_is_kept_without_asking_git(self):
        self.store.m["branches"]["a"]["state"] = "dropped"
        res = self.close()
        self.assertFalse(res["branch_deleted"])
        self.assertFalse(any(c[:2] == ["git", "branch"] or "merge-base" in c for c in self.fake.calls))

    def test_merged_state_but_not_an_ancestor_of_the_feature_branch_is_kept(self):
        self.fake.fail = {"merge-base": (1, "")}
        res = self.close()
        self.assertFalse(res["branch_deleted"])
        self.assertFalse(any(c[:3] == ["git", "branch", "-D"] for c in self.fake.calls))

    def test_refuses_a_target_dir_outside_the_dev_home_cargo_target(self):
        self.store.m["branches"]["a"]["target_dir"] = os.path.join(self.tmp, "elsewhere")
        with self.assertRaises(TreeError):
            self.close()
        self.assertEqual(self.fake.calls, [])

    def test_refuses_a_branch_without_a_worktree(self):
        self.store.m["branches"]["a"]["worktree"] = None
        with self.assertRaises(TreeError):
            self.close()

    def test_branch_delete_failure_is_reported_not_raised(self):
        self.fake.fail = {"branch -D": (1, "checked out elsewhere")}
        res = self.close()
        self.assertFalse(res["branch_deleted"])


class RegisterTest(unittest.TestCase):
    def parser(self):
        parser = argparse.ArgumentParser()
        tr.register(parser.add_subparsers(dest="verb"))
        return parser

    def test_verbs_are_wired_to_their_runners(self):
        p = self.parser()
        self.assertIs(p.parse_args(["budget", "--tree", "T1"]).func, tr.run_budget)
        self.assertIs(p.parse_args(["open", "a"]).func, tr.run_open)
        self.assertIs(p.parse_args(["close", "a"]).func, tr.run_close)
        ns = p.parse_args(["gate", "a", "--exit", "101", "--log", "f.log", "--sha", "s1"])
        self.assertIs(ns.func, tr.run_gate)
        self.assertEqual((ns.branch, ns.exit_code, ns.log, ns.sha), ("a", 101, "f.log", "s1"))

    def test_gate_requires_exit_log_and_sha(self):
        with self.assertRaises(SystemExit), mock.patch("sys.stderr"):
            self.parser().parse_args(["gate", "a", "--exit", "0"])


if __name__ == "__main__":
    unittest.main()
