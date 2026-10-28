#!/usr/bin/env python3
"""Tests for tree.py, the orchestration tree engine.

    python test_tree.py -v
"""

from __future__ import annotations

import contextlib
import io
import json
import shutil
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import tree  # noqa: E402
import tree_record  # noqa: E402
import tree_resources  # noqa: E402
import tree_state as ts  # noqa: E402
from test_tree_state import WORKFLOW, TreeCase  # noqa: E402


class CliCase(TreeCase):
    """A temp project carrying the bundle's packets and speed table, driven through tree.main."""

    def setUp(self):
        super().setUp()
        wf = self.root / ".nucleos" / "workflow"
        shutil.copytree(WORKFLOW / "packets", wf / "packets")
        shutil.copy(WORKFLOW / "speed.yaml", wf / "speed.yaml")

    def cli(self, *argv):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = tree.main(list(argv))
        return code, json.loads(out.getvalue())

    def ok(self, *argv):
        code, out = self.cli(*argv)
        self.assertEqual(code, 0, out)
        return out

    def refused(self, *argv):
        code, out = self.cli(*argv)
        self.assertEqual(code, 2, out)
        self.assertIn("error", out)
        return out["error"]

    def init(self, tree_id="root-1"):
        return self.ok("init", tree_id, "--feature-branch", "feat/x", "--goal", "Ship x")

    def branch(self, name, *paths, layer="core", base=None):
        extra = ["--base", base] if base else []
        return self.ok("branch", "add", name, "--paths", *paths, "--layer", layer, *extra)

    def edit(self, fn, tree_id="root-1"):
        manifest = ts.load_manifest(tree_id)
        fn(manifest)
        ts.save_manifest(tree_id, manifest)

    def set_branch(self, name, tree_id="root-1", **fields):
        self.edit(lambda m: m["branches"][name].update(fields), tree_id)


def git(cwd, *args):
    return subprocess.run(
        ["git", "-C", str(cwd), "-c", "core.hooksPath=", "-c", "user.name=t", "-c", "user.email=t@example.com",
         "-c", "commit.gpgsign=false", *args], check=True, capture_output=True, text=True).stdout


class Registry(CliCase):
    def test_both_register_styles_reach_the_parser(self):
        def resources(verbs):  # chunk 5 style: an argparse-like add_parser plus set_defaults(func=)
            p = verbs.add_parser("budget", help="x")
            p.add_argument("--tree", default=None)
            p.set_defaults(func=lambda args: {"verb": "budget", "tree": args.tree})

        def record(verbs):  # chunk 6 style: a dict entry (add_arguments, run)
            verbs["record"] = (lambda p: p.add_argument("node_id"),
                               lambda args: {"verb": "record", "node": args.node_id})

        with mock.patch.object(tree_resources, "register", resources), \
                mock.patch.object(tree_record, "register", record):
            self.assertEqual(self.ok("budget", "--tree", "T1"), {"verb": "budget", "tree": "T1"})
            self.assertEqual(self.ok("record", "n-7"), {"verb": "record", "node": "n-7"})

    def test_verbs_collects_both_styles_without_depending_on_the_real_modules(self):
        def resources(verbs):
            verbs.add_parser("budget", help="x")

        def record(verbs):
            verbs["record"] = (lambda p: None, lambda args: {})

        verbs = tree.Verbs()
        resources(verbs)
        record(verbs)
        self.assertEqual((sorted(verbs), sorted(verbs.parsers)), (["record"], ["budget"]))

    def test_a_refusal_prints_the_error_and_exits_two(self):
        code, out = self.cli("init", "../x", "--feature-branch", "f", "--goal", "g")
        self.assertEqual((code, out["verb"]), (2, "init"))
        self.assertIn("must be", out["error"])


class Init(CliCase):
    def test_init_writes_the_manifest_and_a_contract_skeleton(self):
        out = self.init()
        self.assertEqual(out["tree_id"], "root-1")
        self.assertEqual(out["dirty_owner_files"], [])
        manifest = ts.load_manifest("root-1")
        self.assertEqual((manifest["feature_branch"], manifest["goal"], manifest["state"], manifest["branches"]),
                         ("feat/x", "Ship x", "open", {}))
        contract = ts.contract_path("root-1").read_text(encoding="utf-8")
        self.assertIn("## A. Superfícies partilhadas", contract)
        self.assertIn("## E. Decisões", contract)
        self.assertEqual(ts.contract_ids(contract), set())

    def test_init_lists_the_dirty_tracked_files_of_the_project(self):
        git(self.root, "init", "-q")
        (self.root / "owner.txt").write_text("v1", encoding="utf-8")
        (self.root / "other.txt").write_text("v1", encoding="utf-8")
        git(self.root, "add", "owner.txt", "other.txt")
        git(self.root, "commit", "-q", "-m", "seed")
        (self.root / "owner.txt").write_text("v2", encoding="utf-8")
        (self.root / "untracked.txt").write_text("x", encoding="utf-8")
        out = self.init()
        self.assertEqual(out["dirty_owner_files"], ["owner.txt"])
        self.assertEqual(ts.load_manifest("root-1")["dirty_owner_files"], ["owner.txt"])

    def test_init_refuses_a_tree_that_already_exists(self):
        self.init()
        self.assertIn("already exists", self.refused("init", "root-1", "--feature-branch", "f", "--goal", "g"))
        self.assertIn("must be", self.refused("init", "../x", "--feature-branch", "f", "--goal", "g"))


class BranchAdd(CliCase):
    def setUp(self):
        super().setUp()
        self.init()

    def test_disjoint_branches_are_queued(self):
        out = self.branch("core", "core/src/**")
        self.assertEqual((out["branch"], out["state"], out["base"]), ("core", "queued", None))
        self.branch("shell", "shell/src/**", layer="shell")
        branches = ts.load_manifest("root-1")["branches"]
        self.assertEqual(sorted(branches), ["core", "shell"])
        self.assertEqual((branches["shell"]["layer"], branches["shell"]["paths"], branches["shell"]["state"]),
                         ("shell", ["shell/src/**"], "queued"))

    def test_an_overlapping_branch_is_refused_in_both_directions_and_names_both(self):
        self.branch("core", "core/src/**")
        error = self.refused("branch", "add", "wide", "--paths", "core/**", "--layer", "core")
        self.assertIn("core/**", error)
        self.assertIn("'core'", error)
        error = self.refused("branch", "add", "file", "--paths", "core/src/a.rs", "--layer", "core")
        self.assertIn("core/src/a.rs", error)
        self.assertNotIn("wide", ts.load_manifest("root-1")["branches"])
        self.refused("branch", "add", "back", "--paths", ".\\core\\src\\x.rs", "--layer", "core")
        self.branch("near", "core/srcx/**")  # a different segment: no overlap

    def test_a_branch_may_overlap_its_base_but_not_an_unrelated_one(self):
        self.branch("api", "core/api/**")
        self.branch("ui", "ui/**", layer="shell")
        self.branch("api2", "core/api/**", "core/extra/**", base="api")
        self.branch("api3", "core/api/v3/**", base="api2")  # base of the base
        self.refused("branch", "add", "bad", "--paths", "core/extra/**", "--layer", "core", "--base", "ui")

    def test_merged_and_dropped_branches_release_their_paths(self):
        self.branch("a", "core/a/**")
        self.branch("b", "core/b/**")
        self.set_branch("a", state="merged")
        self.set_branch("b", state="dropped")
        self.branch("c", "core/**")

    def test_refusals(self):
        self.branch("core", "core/**")
        self.assertIn("already", self.refused("branch", "add", "core", "--paths", "x/**", "--layer", "core"))
        self.assertIn("unknown base", self.refused("branch", "add", "n", "--paths", "x/**", "--layer", "l", "--base", "zzz"))
        self.assertIn("must be", self.refused("branch", "add", "a/b", "--paths", "x/**", "--layer", "l"))
        self.assertIn("empty path", self.refused("branch", "add", "n", "--paths", " ", "--layer", "l"))
        self.edit(lambda m: m.update(state="finished"))
        self.assertIn("finished", self.refused("branch", "add", "z", "--paths", "z/**", "--layer", "l",
                                               "--tree", "root-1"))

    def test_no_branch_is_added_while_the_tree_is_parked(self):
        self.edit(lambda m: m.update(state="parked"))
        self.assertIn("parked", self.refused("branch", "add", "z", "--paths", "z/**", "--layer", "l"))
        self.assertNotIn("z", ts.load_manifest("root-1")["branches"])


class Prompt(CliCase):
    NO_CARGO = "No subagent runs `cargo`"

    def setUp(self):
        super().setUp()
        self.init()
        self.branch("core", "core/src/**")

    def prompt(self, node_id="n-5", kind="leaf", *extra):
        return self.ok("prompt", node_id, "--branch", "core", "--kind", kind, "--goal", "Add the wheel",
                       "--paths", "core/src/wheel.rs", "--pointers", "core/src/wheel.rs:222-275 - the loop",
                       *extra)

    def test_every_role_carries_the_no_cargo_rule_and_its_own_rules(self):
        expected = {"leaf": "Write only inside your Owned paths", "orchestrator": "Edit no file",
                    "review": "Read only: change nothing"}
        for kind, own in expected.items():
            text = self.prompt(f"n-{kind}", kind)["prompt"]
            self.assertIn(self.NO_CARGO, text, kind)
            self.assertIn("Make no commit, merge, stash or push", text, kind)
            self.assertIn(own, text, kind)
            others = [rule for k, rule in expected.items() if k != kind]
            for rule in others:
                self.assertNotIn(rule, text, kind)

    def test_the_rules_come_from_the_packet_and_nowhere_else(self):
        packet = self.root / ".nucleos" / "workflow" / "packets" / "node.md"
        packet.write_text(packet.read_text(encoding="utf-8").replace(self.NO_CARGO, "MARKER RULE"), encoding="utf-8")
        text = self.prompt()["prompt"]
        self.assertIn("MARKER RULE", text)
        self.assertNotIn(self.NO_CARGO, text)

    def test_the_report_block_comes_from_the_packet_and_the_status_map_from_tree_state(self):
        packet = self.root / ".nucleos" / "workflow" / "packets" / "node.md"
        text = self.prompt()["prompt"]
        self.assertIn("Header: `Role: leaf`", text)
        self.assertIn("(done and split -> done; contract_request and blocked -> blocked; failed -> failed)", text)
        packet.write_text(packet.read_text(encoding="utf-8").replace("Write it to", "MARKER DELIVERY"), encoding="utf-8")
        self.assertIn("MARKER DELIVERY", self.prompt()["prompt"])
        with mock.patch.dict(ts.OUTCOME_STATUS, {"split": "blocked"}):
            self.assertIn("done -> done; split and contract_request and blocked -> blocked", self.prompt()["prompt"])

    def test_the_packet_fields_are_filled_and_nothing_is_left_over(self):
        out = self.prompt("n-5", "leaf", "--depth", "2", "--parent", "n-3", "--task-id", "T9")
        text = out["prompt"]
        for line in ("Node ID: n-5", "Root ID: root-1", "Parent ID: n-3", "Task ID: T9", "Kind: leaf",
                     "Owned paths: core/src/wheel.rs", "Budget tokens: 120000", "Current depth: 2",
                     "Max depth: 3", "Max width: 6", "Ceiling: 200000", "Subtree verify: false",
                     "Add the wheel", "core/src/wheel.rs:222-275 - the loop", "## Rules", "## Tree",
                     "Branch: core", "Role: leaf", "## Report", "at most 60 lines", "## Constraints"):
            self.assertIn(line, text)
        self.assertNotIn("<!--", text)
        self.assertNotIn("Rules by role", text)
        self.assertNotIn("### all", text)
        self.assertIn(str(ts.contract_path("root-1")).replace("\\", "/"), text)
        self.assertIn(str(ts.handoff_file("root-1", "n-5")).replace("\\", "/"), text)
        self.assertIn("Write it to", text)
        self.assertEqual(Path(out["prompt_file"]).read_text(encoding="utf-8"), text)
        self.assertEqual(Path(out["prompt_file"]).parent, ts.tree_dir("root-1") / "prompts")

    def test_orchestrator_budget_and_review_are_read_only(self):
        self.assertIn("Budget tokens: 150000", self.prompt("n-o", "orchestrator")["prompt"])
        review = self.ok("prompt", "n-r", "--branch", "core", "--kind", "review", "--goal", "Review it",
                         "--sections", "A3", "E2")["prompt"]
        self.assertIn("Kind: leaf", review)
        self.assertIn("Role: review", review)
        self.assertIn("Owned paths: none (read-only)", review)
        self.assertIn("Contract sections to read: A3, E2", review)
        self.assertIn("return the report as your final message", review)
        self.assertNotIn("Write it to", review)

    def test_refusals(self):
        base = ["prompt", "n-9", "--goal", "g"]
        self.assertIn("no branch", self.refused(*base, "--branch", "zzz", "--kind", "leaf", "--paths", "core/src/a.rs"))
        self.assertIn("needs --paths", self.refused(*base, "--branch", "core", "--kind", "leaf"))
        self.assertIn("outside branch", self.refused(*base, "--branch", "core", "--kind", "leaf", "--paths", "shell/x.rs"))
        # containment, not overlap: a leaf wider than its branch is refused, a narrower glob is fine
        self.assertIn("outside branch", self.refused(*base, "--branch", "core", "--kind", "leaf", "--paths", "core/**"))
        self.assertIn("outside branch", self.refused(*base, "--branch", "core", "--kind", "leaf", "--paths", "core/sr"))
        self.ok(*base, "--branch", "core", "--kind", "leaf", "--paths", "core/src/wheel/**", "core/src/b.rs")
        packet = self.root / ".nucleos" / "workflow" / "packets" / "node.md"
        original = packet.read_text(encoding="utf-8")
        packet.write_text(original.split("## Report format")[0], encoding="utf-8")
        self.assertIn("Report format", self.refused(*base, "--branch", "core", "--kind", "review"))
        packet.write_text(original.split("## Rules by role")[0], encoding="utf-8")
        self.assertIn("Rules by role", self.refused(*base, "--branch", "core", "--kind", "review"))
        packet.unlink()
        self.assertIn("node.md", self.refused(*base, "--branch", "core", "--kind", "review"))


class Containment(unittest.TestCase):
    """`tree.contained` and `tree.normalize_paths` against the daemon's `check_containment`."""

    DIR = ".claude/worktrees/slash-cd"

    def test_literal_owned_path_is_a_file_or_a_directory(self):
        owned = [self.DIR]
        for inside in [self.DIR, f"{self.DIR}/core/src/x.rs", f"{self.DIR}/**", f"{self.DIR}/core/*.rs"]:
            self.assertTrue(tree.contained(inside, owned), inside)
        for outside in [f"{self.DIR}-evil", f"{self.DIR}-evil/x.rs", f"{self.DIR}*", ".claude/worktrees",
                        "other/x.rs"]:
            self.assertFalse(tree.contained(outside, owned), outside)

    def test_trailing_slash_and_backslashes(self):
        owned = [f"{self.DIR}/"]
        self.assertTrue(tree.contained(f"{self.DIR}/x.rs", owned))
        self.assertTrue(tree.contained(f"{self.DIR}/**", owned))
        self.assertFalse(tree.contained(f"{self.DIR}-evil/**", owned))
        [child] = tree.normalize_paths([".claude\\worktrees\\slash-cd\\x.rs"])
        self.assertTrue(tree.contained(child, tree.normalize_paths([".claude\\worktrees\\slash-cd"])))

    def test_normalize_paths_refuses_dotdot_and_absolute(self):
        for bad in ["a/../b", "..\\x", "/etc/x", "C:/x"]:
            with self.assertRaises(tree.TreeError, msg=bad):
                tree.normalize_paths([bad])
        self.assertEqual(tree.normalize_paths(["./a/b"]), ["a/b"])


class Status(CliCase):
    NOW = "2026-10-21T12:00:00+00:00"

    def setUp(self):
        super().setUp()
        self.init()

    def nodes_file(self, nodes):
        path = self.tmp / "nodes.json"
        path.write_text(json.dumps({"root_id": "root-1", "nodes": nodes}), encoding="utf-8")
        return str(path)

    def test_status_of_an_unknown_tree_is_refused(self):
        self.assertIn("no tree", self.refused("status", "--tree", "nope"))

    def test_status_of_a_new_tree_says_to_declare_branches(self):
        out = self.ok("status")
        self.assertEqual((out["tree_id"], out["state"], out["branches"], out["suspect_nodes"]),
                         ("root-1", "open", [], []))
        self.assertIn("branch add", out["next"][0])

    def test_next_action_per_branch_state(self):
        self.branch("base", "core/a/**")
        self.branch("top", "core/b/**", base="base")
        out = {b["name"]: b for b in self.ok("status")["branches"]}
        self.assertIn("run `budget`", out["base"]["next"])
        self.assertIn("base 'base' is not merged", out["top"]["next"])
        self.assertFalse(out["top"]["base_ready"])
        self.set_branch("base", state="green", reviews=[{"at": "t", "findings": [{"id": "R1", "state": "open"}]}])
        base = {b["name"]: b for b in self.ok("status")["branches"]}["base"]
        self.assertEqual(base["open_findings"], 1)
        self.assertIn("open finding", base["next"])
        self.set_branch("base", state="green", reviews=[{"at": "t", "findings": []}])
        self.assertIn("tree.py merge", {b["name"]: b for b in self.ok("status")["branches"]}["base"]["next"])
        self.set_branch("base", state="merged")
        out = self.ok("status")
        self.assertEqual({b["name"]: b["next"] for b in out["branches"]}["base"], "")
        self.assertIn("run `budget`", {b["name"]: b["next"] for b in out["branches"]}["top"])

    def test_all_branches_settled_points_at_finish(self):
        self.branch("a", "core/a/**")
        self.set_branch("a", state="merged")
        self.assertEqual(self.ok("status")["next"], ["all branches settled: run `tree.py finish`"])

    def test_status_after_an_interrupted_run_flags_the_dead_node(self):
        self.branch("core", "core/**")
        self.set_branch("core", state="building", node_id="n-core")
        (ts.tree_dir("root-1") / "tree.tmp").write_text("{ half written", encoding="utf-8")  # the crash
        old = "2026-10-21T08:00:00+00:00"
        nodes = [
            {"id": "root-1", "parent_id": None, "kind": "root", "status": "running", "updated_at": old},
            {"id": "n-core", "parent_id": "root-1", "kind": "orchestrator", "status": "running", "updated_at": old},
            {"id": "n-l1", "parent_id": "n-core", "kind": "leaf", "status": "running", "title": "L1",
             "updated_at": "2026-10-21T09:00:00+00:00"},
            {"id": "n-l2", "parent_id": "n-core", "kind": "leaf", "status": "running",
             "updated_at": "2026-10-21T11:50:00+00:00"},
            {"id": "n-l3", "parent_id": "n-core", "kind": "leaf", "status": "done", "updated_at": old},
            {"id": "n-l4", "parent_id": "n-core", "kind": "leaf", "status": "running"}]
        out = self.ok("status", "--nodes", self.nodes_file(nodes), "--now", self.NOW)
        self.assertEqual([(s["id"], s["age_minutes"], s["branch"]) for s in out["suspect_nodes"]],
                         [("n-l1", 180.0, "core")])
        self.assertEqual(out["unverifiable_nodes"], ["n-l4"])
        core = out["branches"][0]
        self.assertEqual(core["suspect_nodes"], ["n-l1"])
        self.assertIn("n-l1", core["next"])
        self.assertIn("handoff", core["next"])
        self.assertTrue(any(line.startswith("core: ") for line in out["next"]))
        # without the nodes file the manifest alone still reads
        self.assertEqual(self.ok("status")["suspect_nodes"], [])

    def test_stale_minutes_comes_from_the_config(self):
        self.branch("core", "core/**")
        self.set_branch("core", state="building", node_id="n-core")
        nodes = [{"id": "n-l2", "parent_id": "n-core", "kind": "leaf", "status": "running",
                  "updated_at": "2026-10-21T11:50:00+00:00"}]
        path = self.nodes_file(nodes)
        self.assertEqual(self.ok("status", "--nodes", path, "--now", self.NOW)["suspect_nodes"], [])
        (self.state / "project.yaml").write_text("tree:\n  stale_minutes: 5\n", encoding="utf-8")
        self.assertEqual([s["id"] for s in self.ok("status", "--nodes", path, "--now", self.NOW)["suspect_nodes"]],
                         ["n-l2"])

    def test_a_bad_nodes_file_is_refused(self):
        bad = self.tmp / "bad.json"
        bad.write_text("[1, 2]", encoding="utf-8")
        self.assertIn("--nodes", self.refused("status", "--nodes", str(bad)))
        self.assertIn("--nodes", self.refused("status", "--nodes", str(self.tmp / "missing.json")))


class BranchLifecycle(CliCase):
    def setUp(self):
        super().setUp()
        self.init()
        self.branch("core", "core/**")

    def branch_state(self, name="core"):
        return ts.load_manifest("root-1")["branches"][name]

    def test_bind_records_the_branch_node_and_starts_building(self):
        self.set_branch("core", state="open")
        self.assertIn("branch bind", self.ok("status")["branches"][0]["next"])
        out = self.ok("branch", "bind", "core", "--node-id", "n-core")
        self.assertEqual((out["state"], out["node_id"]), ("building", "n-core"))
        self.assertEqual((self.branch_state()["node_id"], self.branch_state()["state"]), ("n-core", "building"))

    def test_bind_refusals(self):
        self.assertIn("open", self.refused("branch", "bind", "core", "--node-id", "n-core"))  # still queued
        self.set_branch("core", state="open")
        self.assertIn("--node-id", self.refused("branch", "bind", "core"))
        self.assertIn("must be", self.refused("branch", "bind", "core", "--node-id", "../x"))
        self.ok("branch", "bind", "core", "--node-id", "n-core")
        self.assertIn("already", self.refused("branch", "bind", "core", "--node-id", "n-other"))
        self.branch("ui", "ui/**", layer="shell")
        self.set_branch("ui", state="open")
        self.assertIn("n-core", self.refused("branch", "bind", "ui", "--node-id", "n-core"))

    def test_dropping_a_branch_without_a_node_frees_its_paths(self):
        out = self.ok("branch", "drop", "core", "--reason", "scope cut")
        self.assertEqual((out["state"], out["mcp"]), ("dropped", []))
        self.assertEqual((self.branch_state()["state"], self.branch_state()["drop_reason"]), ("dropped", "scope cut"))
        self.branch("again", "core/**")

    def test_dropping_a_branch_with_a_node_returns_its_failed_close(self):
        self.set_branch("core", state="building", node_id="n-core")
        out = self.ok("branch", "drop", "core", "--reason", "scope cut")
        [close] = out["mcp"]
        self.assertEqual(close["tool"], "node_close")
        self.assertEqual((close["args"]["id"], close["args"]["status"], close["args"]["summary"]),
                         ("n-core", "failed", "dropped: scope cut"))
        self.assertEqual(Path(close["args"]["handoff_path"]), ts.handoff_file("root-1", "n-core"))
        text = Path(close["args"]["handoff_path"]).read_text(encoding="utf-8")
        ts.validate_report(ts.parse_report(text), ts.load_config())
        self.assertEqual(ts.parse_report(text)["outcome"], "failed")

    def test_drop_refusals(self):
        self.assertIn("--reason", self.refused("branch", "drop", "core"))
        for state in ("gating", "green", "reviewing", "merged", "dropped"):
            self.set_branch("core", state=state)
            self.assertIn(state, self.refused("branch", "drop", "core", "--reason", "x"), state)
        for state in ("queued", "open", "building", "red", "fixing", "parked"):
            self.set_branch("core", state=state)
            self.ok("branch", "drop", "core", "--reason", "x")


class Review(CliCase):
    def setUp(self):
        super().setUp()
        self.init()
        self.branch("core", "core/**")
        self.set_branch("core", state="green")

    def write_findings(self, items):
        path = self.tmp / "findings.json"
        path.write_text(json.dumps(items), encoding="utf-8")
        return str(path)

    def submit(self, items, branch="core"):
        return self.ok("review", branch, "--findings", self.write_findings(items))

    def branch_state(self):
        return ts.load_manifest("root-1")["branches"]["core"]["state"]

    def test_findings_merge_by_id_and_the_history_is_kept(self):
        out = self.submit([{"id": "R1", "state": "open", "file": "a.rs", "note": "n1"}, {"id": "R2", "state": "open"}])
        self.assertEqual((out["state"], out["open"]), ("fixing", ["R1", "R2"]))
        out = self.submit([{"id": "R1", "state": "fixed"}, {"id": "R3", "state": "defer", "note": "later"}])
        self.assertEqual(out["open"], ["R2"])
        self.assertEqual(out["counts"], {"fixed": 1, "open": 1, "defer": 1})
        reviews = ts.load_manifest("root-1")["branches"]["core"]["reviews"]
        self.assertEqual(len(reviews), 2)
        self.assertEqual([f["state"] for f in reviews[0]["findings"]], ["open", "open"])
        latest = {f["id"]: f for f in reviews[-1]["findings"]}
        self.assertEqual((latest["R1"]["state"], latest["R1"]["file"], latest["R1"]["note"]), ("fixed", "a.rs", "n1"))
        self.assertEqual((latest["R2"]["state"], latest["R3"]["state"]), ("open", "defer"))

    def test_fixing_every_finding_does_not_make_the_branch_green_again(self):
        self.submit([{"id": "R1", "state": "open"}, {"id": "R2", "state": "open"}])
        out = self.submit([{"id": "R1", "state": "fixed"}, {"id": "R2", "state": "drop"}])
        self.assertEqual((out["open"], self.branch_state()), ([], "fixing"))  # code changed: a new gate must say green

    def test_dropping_or_deferring_every_finding_restores_the_state_before_the_fix(self):
        self.submit([{"id": "R1", "state": "open"}, {"id": "R2", "state": "open"}])
        self.assertEqual(self.branch_state(), "fixing")
        self.assertEqual(self.submit([{"id": "R1", "state": "drop"}])["state"], "fixing")
        out = self.submit([{"id": "R2", "state": "defer"}])
        self.assertEqual((out["open"], out["state"], self.branch_state()), ([], "green", "green"))
        self.assertNotIn("state_before_fix", ts.load_manifest("root-1")["branches"]["core"])
        self.set_branch("core", state="reviewing")
        self.submit([{"id": "R3", "state": "open"}])
        self.assertEqual(self.submit([{"id": "R3", "state": "drop"}])["state"], "reviewing")

    def test_a_fix_made_before_the_last_finding_closes_keeps_the_branch_in_fixing(self):
        self.submit([{"id": "R1", "state": "open"}, {"id": "R2", "state": "open"}])
        self.submit([{"id": "R1", "state": "fixed"}])
        self.assertEqual(self.submit([{"id": "R2", "state": "drop"}])["state"], "fixing")

    def test_a_clean_review_leaves_the_state_alone(self):
        out = self.submit([])
        self.assertEqual((out["open"], self.branch_state()), ([], "green"))

    def test_findings_can_come_from_stdin_as_utf_8(self):
        payload = json.dumps({"findings": [{"id": "R1", "state": "open", "note": "falta o acento: ação"}]},
                             ensure_ascii=False).encode("utf-8")
        stdin = io.TextIOWrapper(io.BytesIO(payload), encoding="cp1252")  # the console codepage must not matter
        with mock.patch("sys.stdin", stdin):
            out = self.ok("review", "core")
        self.assertEqual(out["open"], ["R1"])
        note = ts.load_manifest("root-1")["branches"]["core"]["reviews"][-1]["findings"][0]["note"]
        self.assertEqual(note, "falta o acento: ação")

    def test_an_integration_review_has_no_branch_and_changes_no_state(self):
        out = self.ok("review", "--integration", "--findings", self.write_findings([{"id": "I1", "state": "open"}]))
        self.assertEqual((out["scope"], out["open"]), ("integration", ["I1"]))
        out = self.ok("review", "--integration", "--findings", self.write_findings([{"id": "I1", "state": "fixed"}]))
        manifest = ts.load_manifest("root-1")
        self.assertEqual((out["open"], len(manifest["integration_reviews"]), manifest["branches"]["core"]["state"]),
                         ([], 2, "green"))
        self.assertIn("branch name", self.refused("review", "--findings", self.write_findings([])))

    def test_refusals(self):
        for bad, text in [([{"id": "R1", "state": "keep"}], "state must be"),
                          ([{"state": "open"}], "needs an id"),
                          ([{"id": "R1", "state": "open"}, {"id": "R1", "state": "fixed"}], "duplicate"),
                          ({"id": "R1"}, "must be a JSON list")]:
            path = self.tmp / "bad.json"
            path.write_text(json.dumps(bad), encoding="utf-8")
            self.assertIn(text, self.refused("review", "core", "--findings", str(path)))
        self.set_branch("core", state="queued")
        self.assertIn("gate", self.refused("review", "core", "--findings", str(self.tmp / "bad.json")))
        self.set_branch("core", state="merged")
        self.assertIn("merged", self.refused("review", "core", "--findings", str(self.tmp / "bad.json")))


class Merge(CliCase):
    def setUp(self):
        super().setUp()
        self.init()
        self.branch("api", "core/api/**")
        self.branch("ui", "ui/**", layer="shell", base="api")

    def review(self, branch, items):
        path = self.tmp / "findings.json"
        path.write_text(json.dumps(items), encoding="utf-8")
        return self.ok("review", branch, "--findings", str(path))

    def branch_state(self, name):
        return ts.load_manifest("root-1")["branches"][name]["state"]

    def test_a_branch_merges_only_after_its_base(self):
        self.set_branch("ui", state="green")
        error = self.refused("merge", "ui")
        self.assertIn("'api'", error)
        self.assertIn("not merged", error)
        self.set_branch("api", state="green")
        self.ok("merge", "api")
        out = self.ok("merge", "ui", "--sha", "abc123")
        self.assertEqual((out["state"], self.branch_state("ui")), ("merged", "merged"))
        self.assertEqual(ts.load_manifest("root-1")["branches"]["ui"]["merge_sha"], "abc123")

    def test_an_open_finding_blocks_the_merge_until_it_is_resolved_and_the_gate_is_green(self):
        self.set_branch("api", state="green")
        self.review("api", [{"id": "R1", "state": "open"}, {"id": "R2", "state": "defer"}])
        self.assertIn("R1", self.refused("merge", "api"))
        self.review("api", [{"id": "R1", "state": "fixed"}])
        self.assertIn("green", self.refused("merge", "api"))  # fixing: the gate has not said green again
        self.set_branch("api", state="green")
        self.assertEqual(self.ok("merge", "api")["state"], "merged")

    def test_merge_unblocks_the_branches_that_wait_for_it(self):
        self.set_branch("api", state="green")
        self.assertEqual(self.ok("merge", "api")["unblocked"], ["ui"])

    def test_merging_a_branch_with_a_node_returns_the_node_close(self):
        self.set_branch("api", state="green", node_id="n-api")
        out = self.ok("merge", "api")
        handoff = ts.handoff_file("root-1", "n-api")
        self.assertEqual(out["mcp"], [{"tool": "node_close", "args": {
            "id": "n-api", "status": "done", "summary": "Branch api merged into feat/x",
            "handoff_path": str(handoff)}}])
        ts.validate_report(ts.parse_report(handoff.read_text(encoding="utf-8")), ts.load_config())

    def test_a_handoff_the_branch_node_wrote_itself_is_kept_on_merge(self):
        mine = ts.handoff_file("root-1", "n-api")
        mine.parent.mkdir(parents=True, exist_ok=True)
        mine.write_text("my own report", encoding="utf-8")
        self.set_branch("api", state="green", node_id="n-api")
        self.assertEqual(self.ok("merge", "api")["mcp"][0]["args"]["handoff_path"], str(mine))
        self.assertEqual(mine.read_text(encoding="utf-8"), "my own report")

    def test_a_branch_without_a_node_merges_with_no_mcp(self):
        self.set_branch("api", state="green")
        self.assertEqual(self.ok("merge", "api")["mcp"], [])

    def test_merging_an_unreviewed_branch_warns_but_is_allowed(self):
        self.set_branch("api", state="green")
        self.assertEqual(self.ok("merge", "api")["warnings"], ["unreviewed"])
        self.set_branch("ui", state="green")
        self.review("ui", [])
        self.assertEqual(self.ok("merge", "ui")["warnings"], [])

    def test_only_a_green_unmerged_branch_merges(self):
        for state in ("queued", "building", "gating", "red"):
            self.set_branch("api", state=state)
            self.assertIn("green", self.refused("merge", "api"), state)
        for state in ("parked", "dropped"):
            self.set_branch("api", state=state)
            self.assertIn(state, self.refused("merge", "api"))
        self.set_branch("api", state="merged")
        self.assertIn("already merged", self.refused("merge", "api"))
        self.assertIn("no branch", self.refused("merge", "zzz"))


class Park(CliCase):
    def setUp(self):
        super().setUp()
        self.init()
        self.branch("core", "core/**")
        self.set_branch("core", state="building", node_id="n-core")

    def assert_valid_handoff(self, path):
        ts.validate_report(ts.parse_report(Path(path).read_text(encoding="utf-8")), ts.load_config())

    def manifest(self):
        return ts.load_manifest("root-1")

    def test_parking_a_node_returns_update_and_close_with_a_valid_handoff(self):
        out = self.ok("park", "node", "n-7", "--needs", "owner must  choose X")
        update, close = out["mcp"]
        self.assertEqual(update, {"tool": "node_update", "args": {"id": "n-7", "decision": "owner must choose X"}})
        self.assertEqual((close["tool"], close["args"]["id"], close["args"]["status"]), ("node_close", "n-7", "blocked"))
        self.assertTrue(close["args"]["summary"].startswith("Parked:"))
        self.assertTrue(Path(close["args"]["handoff_path"]).is_file())
        self.assert_valid_handoff(close["args"]["handoff_path"])
        self.assertEqual(Path(close["args"]["handoff_path"]), ts.handoff_file("root-1", "n-7"))

    def test_a_handoff_the_node_wrote_itself_is_kept(self):
        mine = ts.handoff_file("root-1", "n-8")
        mine.parent.mkdir(parents=True, exist_ok=True)
        mine.write_text("my own report", encoding="utf-8")
        out = self.ok("park", "node", "n-8", "--needs", "x", "--role", "orchestrator")
        self.assertEqual(mine.read_text(encoding="utf-8"), "my own report")
        self.assertEqual(out["mcp"][1]["args"]["handoff_path"], str(mine))

    def test_parking_a_branch_takes_two_steps(self):
        one = self.ok("park", "branch", "core", "--needs", "pick a database")
        self.assertEqual(one["step"], 1)
        self.assertEqual(one["mcp"], [{"tool": "node_open", "args": {
            "parent_id": "n-core", "kind": "leaf", "title": "park: core", "goal": "pick a database",
            "owned_paths": []}}])
        branch = self.manifest()["branches"]["core"]
        self.assertEqual((branch["state"], branch["park"]["needs"], branch["park"]["marker_node"],
                          branch["park"]["prev_state"]), ("parked", "pick a database", None, "building"))
        self.assertIn("max_width", one["next"])
        self.assertEqual(self.ok("park", "branch", "core")["mcp"], one["mcp"])  # a cut between the steps resumes
        self.assertIn("--marker-id", self.ok("status")["branches"][0]["next"])
        two = self.ok("park", "branch", "core", "--marker-id", "m-1")
        self.assertEqual(two["step"], 2)
        update, close = two["mcp"]
        self.assertEqual(update["args"], {"id": "m-1", "decision": "pick a database"})
        self.assertEqual((close["tool"], close["args"]["status"], close["args"]["id"]), ("node_close", "blocked", "m-1"))
        self.assert_valid_handoff(close["args"]["handoff_path"])
        self.assertEqual(self.manifest()["branches"]["core"]["park"]["marker_node"], "m-1")
        self.assertIn("already parked", self.refused("park", "branch", "core"))
        self.assertIn("already recorded", self.refused("park", "branch", "core", "--marker-id", "m-2"))
        self.assertIn("resume", self.ok("status")["branches"][0]["next"])

    def test_resume_restores_the_state_before_the_park(self):
        self.ok("park", "branch", "core", "--needs", "x")
        out = self.ok("park", "branch", "core", "--resume")
        self.assertEqual(out["state"], "building")
        branch = self.manifest()["branches"]["core"]
        self.assertEqual((branch["state"], branch["park"]), ("building", None))
        self.assertIn("nothing parked", self.refused("park", "branch", "core", "--resume"))

    def test_a_branch_without_a_node_parks_under_the_root(self):
        self.branch("other", "other/**")
        out = self.ok("park", "branch", "other", "--needs", "x")
        self.assertEqual(out["mcp"][0]["args"]["parent_id"], "root-1")

    def test_parking_the_whole_tree(self):
        one = self.ok("park", "tree", "--needs", "owner is away")
        self.assertEqual(one["mcp"][0]["args"]["parent_id"], "root-1")
        manifest = self.manifest()
        self.assertEqual((manifest["state"], manifest["park"]["needs"]), ("parked", "owner is away"))
        self.assertIn("tree parked", self.ok("status")["next"][0])
        two = self.ok("park", "tree", "--marker-id", "m-9")
        self.assertEqual(two["mcp"][1]["args"]["status"], "blocked")
        self.assertEqual(self.manifest()["park"]["marker_node"], "m-9")
        self.ok("park", "tree", "--resume")
        self.assertEqual((self.manifest()["state"], self.manifest()["park"]), ("open", None))

    def test_refusals(self):
        self.assertIn("--needs", self.refused("park", "branch", "core"))
        self.assertIn("--needs", self.refused("park", "node", "n-7"))
        self.assertIn("needs a node id", self.refused("park", "node", "--needs", "x"))
        self.assertIn("park first", self.refused("park", "branch", "core", "--marker-id", "m-1"))
        self.assertIn("no branch", self.refused("park", "branch", "zzz", "--needs", "x"))
        self.assertIn("needs a branch name", self.refused("park", "branch", "--needs", "x"))
        self.set_branch("core", state="merged")
        self.assertIn("merged", self.refused("park", "branch", "core", "--needs", "x"))


class Finish(CliCase):
    def setUp(self):
        super().setUp()
        self.init()
        self.branch("core", "core/**")
        self.branch("ui", "ui/**", layer="shell")

    def assert_valid_handoff(self, path):
        ts.validate_report(ts.parse_report(Path(path).read_text(encoding="utf-8")), ts.load_config())

    def test_it_refuses_while_a_branch_is_not_settled(self):
        self.set_branch("core", state="merged")
        error = self.refused("finish")
        self.assertIn("ui (queued)", error)
        self.assertNotIn("core (", error)
        self.assertEqual(ts.load_manifest("root-1")["state"], "open")

    def test_it_refuses_without_branches_and_while_the_tree_is_parked(self):
        self.edit(lambda m: m["branches"].clear())
        self.assertIn("no branches", self.refused("finish"))
        self.branch("a", "a/**")
        self.set_branch("a", state="merged")
        self.ok("park", "tree", "--needs", "owner away")
        self.assertIn("parked", self.refused("finish"))

    def test_merged_and_dropped_branches_close_the_root_done(self):
        self.set_branch("core", state="merged")
        self.set_branch("ui", state="dropped")
        out = self.ok("finish")
        self.assertEqual(out["status"], "done")
        self.assertEqual([c["tool"] for c in out["mcp"]], ["node_close"])
        close = out["mcp"][0]["args"]
        self.assertEqual((close["id"], close["status"]), ("root-1", "done"))
        self.assertIn("1 merged", close["summary"])
        self.assertIn("1 dropped", close["summary"])
        self.assert_valid_handoff(close["handoff_path"])
        self.assertEqual(Path(close["handoff_path"]), ts.handoff_file("root-1", "root-1"))
        manifest = ts.load_manifest("root-1")
        self.assertEqual(manifest["state"], "finished")
        self.assertEqual(self.ok("finish", "--tree", "root-1")["mcp"], out["mcp"])  # repeatable if the close failed

    def test_a_parked_branch_closes_the_root_blocked_with_its_decision(self):
        self.set_branch("core", state="merged")
        self.ok("park", "branch", "ui", "--needs", "pick the palette")
        out = self.ok("finish", "--summary", "Core shipped; UI waits.")
        self.assertEqual(out["status"], "blocked")
        update, close = out["mcp"]
        self.assertEqual(update["tool"], "node_update")
        self.assertEqual(update["args"]["id"], "root-1")
        self.assertIn("ui", update["args"]["decision"])
        self.assertIn("pick the palette", update["args"]["decision"])
        self.assertEqual((close["args"]["status"], close["args"]["summary"]), ("blocked", "Core shipped; UI waits."))
        self.assert_valid_handoff(close["args"]["handoff_path"])
        parsed = ts.parse_report(Path(close["args"]["handoff_path"]).read_text(encoding="utf-8"))
        self.assertEqual(parsed["outcome"], "blocked")
        self.assertIn("pick the palette", parsed["blocked_reason"])
        # the root is closed blocked, but the tree stays open: the parked branch can still be resumed
        manifest = ts.load_manifest("root-1")
        self.assertEqual((manifest["state"], manifest["root_closed"], manifest["finish"]["parked"]),
                         ("open", True, ["ui"]))

    def test_finish_again_with_the_same_parked_branches_replays_the_close(self):
        self.set_branch("core", state="merged")
        self.ok("park", "branch", "ui", "--needs", "pick the palette")
        first = self.ok("finish")
        again = self.ok("finish")
        self.assertEqual((again["again"], again["mcp"]), (True, first["mcp"]))

    def test_a_resumed_branch_that_merges_does_not_close_the_root_again(self):
        self.set_branch("core", state="merged")
        self.ok("park", "branch", "ui", "--needs", "pick the palette")
        self.ok("finish")
        self.ok("park", "branch", "ui", "--resume")  # allowed: the tree is still open
        self.set_branch("ui", state="green")
        self.ok("merge", "ui")
        out = self.ok("finish")
        self.assertEqual((out["mcp"], out["root_already_closed"]), ([], True))
        self.assertEqual(ts.load_manifest("root-1")["state"], "finished")


if __name__ == "__main__":
    unittest.main()
