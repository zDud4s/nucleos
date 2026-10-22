#!/usr/bin/env python3
"""Tests for tree_state.py and for the bundle files the tree engine reads.

    python test_tree_state.py -v
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import yaml

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
BUNDLE = HERE.parent
WORKFLOW = BUNDLE / ".nucleos" / "workflow"

import tree_state as ts  # noqa: E402


class BundleFiles(unittest.TestCase):
    def test_the_handoff_packet_carries_the_report_fields(self):
        text = (WORKFLOW / "packets" / "handoff.md").read_text(encoding="utf-8")
        for needle in ("Role:", "Outcome:", "Contract used:", "## Contract request"):
            self.assertIn(needle, text)
        self.assertLess(text.index("Status:"), text.index("Role:"))
        self.assertLess(text.index("Contract used:"), text.index("## Summary"))
        self.assertLess(text.index("## Contract request"), text.index("## Blocked reason"))

    def test_the_node_packet_holds_the_rules_by_role(self):
        text = (WORKFLOW / "packets" / "node.md").read_text(encoding="utf-8")
        self.assertIn("## Rules by role", text)
        rules = text[text.index("## Rules by role"):]
        for sub in ("### all", "### leaf", "### orchestrator", "### review"):
            self.assertIn(sub, rules)
        self.assertIn("No subagent runs `cargo`", rules[rules.index("### all"):rules.index("### leaf")])
        report = text[text.index("## Report format"):]
        for sub in ("### all", "### file", "### message"):
            self.assertIn(sub, report)
        for placeholder in ("{max_lines}", "{role}", "{status_map}", "{handoff}"):
            self.assertIn(placeholder, report)

    def test_speed_yaml_has_a_nested_tree_block_and_no_top_level_leak(self):
        table = yaml.safe_load((WORKFLOW / "speed.yaml").read_text(encoding="utf-8"))
        self.assertEqual(table["tree"], {
            "floor_gb": 15, "max_live_branches": 4, "seed_gb_per_layer": 8,
            "lock_wait_minutes": 10, "stale_minutes": 30, "report_max_lines": 60})
        for leaked in ("floor_gb", "max_depth", "stale_minutes"):
            self.assertNotIn(leaked, table)


def make_project(tmp: Path) -> tuple[Path, Path]:
    """A project root with its own project.json, and the state dir it points at."""
    root, state = tmp / "project", tmp / "state"
    wf = root / ".nucleos" / "workflow"
    wf.mkdir(parents=True)
    state.mkdir()
    (wf / "project.json").write_text(
        json.dumps({"id": "t", "state_dir": str(state), "bundle_dir": str(BUNDLE)}), encoding="utf-8")
    return root, state


class TreeCase(unittest.TestCase):
    """A temp project plus NUCLEOS_PROJECT_ROOT / NUCLEOS_DEV_HOME pointed at it."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        self.root, self.state = make_project(self.tmp)
        patch = mock.patch.dict(os.environ, {
            "NUCLEOS_PROJECT_ROOT": str(self.root), "NUCLEOS_DEV_HOME": str(self.tmp / "dev")})
        patch.start()
        self.addCleanup(patch.stop)
        os.environ.pop("NUCLEOS_TREE_ID", None)


class Config(TreeCase):
    def test_defaults_when_nothing_is_configured(self):
        cfg = ts.load_config()
        self.assertEqual(
            (cfg["floor_gb"], cfg["max_live_branches"], cfg["seed_gb_per_layer"],
             cfg["lock_wait_minutes"], cfg["stale_minutes"], cfg["report_max_lines"]),
            (15, 4, 8, 10, 30, 60))
        self.assertEqual((cfg["worktrees_dir"], cfg["links"], cfg["heavy"], cfg["base_branch"]),
                         (".nucleos/worktrees", [], None, "master"))

    def test_speed_yaml_and_project_yaml_layer_over_the_defaults(self):
        (self.root / ".nucleos" / "workflow" / "speed.yaml").write_text(
            "tree:\n  floor_gb: 20\n  stale_minutes: 45\n", encoding="utf-8")
        (self.state / "project.yaml").write_text(
            "project_name: x\ntree:\n  worktrees_dir: .claude/worktrees\n"
            "  links: [shell/node_modules]\n  heavy: scripts/heavy.py\n  max_live_branches: 2\n",
            encoding="utf-8")
        cfg = ts.load_config()
        self.assertEqual((cfg["floor_gb"], cfg["stale_minutes"], cfg["max_live_branches"]), (20, 45, 2))
        self.assertEqual((cfg["worktrees_dir"], cfg["links"], cfg["heavy"]),
                         (".claude/worktrees", ["shell/node_modules"], "scripts/heavy.py"))

    def test_a_project_key_in_speed_yaml_or_an_unknown_key_is_refused(self):
        speed = self.root / ".nucleos" / "workflow" / "speed.yaml"
        speed.write_text("tree:\n  worktrees_dir: x\n", encoding="utf-8")
        with self.assertRaisesRegex(ts.TreeError, "unknown tree keys"):
            ts.load_config()
        speed.write_text("tree:\n  floor_gb: 15\n", encoding="utf-8")
        (self.state / "project.yaml").write_text("tree:\n  nope: 1\n", encoding="utf-8")
        with self.assertRaisesRegex(ts.TreeError, "unknown tree keys"):
            ts.load_config()

    def test_a_negative_or_boolean_number_is_refused(self):
        (self.state / "project.yaml").write_text("tree:\n  floor_gb: -1\n", encoding="utf-8")
        with self.assertRaisesRegex(ts.TreeError, "floor_gb"):
            ts.load_config()
        (self.state / "project.yaml").write_text("tree:\n  floor_gb: true\n", encoding="utf-8")
        with self.assertRaisesRegex(ts.TreeError, "floor_gb"):
            ts.load_config()

    def test_nodes_limits_come_from_the_nodes_block(self):
        self.assertEqual(ts.load_nodes_limits()["max_depth"], 3)
        (self.root / ".nucleos" / "workflow" / "speed.yaml").write_text(
            "nodes:\n  max_depth: 2\n  subtree_verify: true\n", encoding="utf-8")
        limits = ts.load_nodes_limits()
        self.assertEqual((limits["max_depth"], limits["ceiling"], limits["subtree_verify"]), (2, 200000, True))

    def test_dev_home_follows_the_environment(self):
        self.assertEqual(ts.dev_home(), self.tmp / "dev")
        with mock.patch.dict(os.environ):
            os.environ.pop("NUCLEOS_DEV_HOME")
            self.assertEqual(ts.dev_home().name, "nucleos-dev")


class Manifest(TreeCase):
    def test_round_trip_leaves_no_temporary_file(self):
        m = ts.new_manifest("n-1", "feat/x", "goal", self.root, ["a.txt"])
        ts.save_manifest("n-1", m)
        self.assertEqual(ts.load_manifest("n-1"), m)
        self.assertEqual([p.name for p in ts.tree_dir("n-1").iterdir()], ["tree.json"])
        self.assertEqual(ts.tree_dir("n-1").parents[1], self.state)
        self.assertEqual(m["dirty_owner_files"], ["a.txt"])
        self.assertEqual((m["costs_gb"], m["records"], m["attempts"], m["park"]), ({}, [], {}, None))

    def test_missing_wrong_version_and_unsafe_ids_are_refused(self):
        with self.assertRaisesRegex(ts.TreeError, "no tree"):
            ts.load_manifest("n-9")
        for bad in ("../x", "a/b", "", ".hidden"):
            with self.assertRaises(ts.TreeError):
                ts.tree_dir(bad)
        m = ts.new_manifest("n-1", "f", "g", self.root, [])
        m["version"] = 2
        ts.save_manifest("n-1", m)
        with self.assertRaisesRegex(ts.TreeError, "version"):
            ts.load_manifest("n-1")

    def test_save_refuses_a_manifest_of_another_tree(self):
        with self.assertRaises(ts.TreeError):
            ts.save_manifest("n-2", ts.new_manifest("n-1", "f", "g", self.root, []))

    def test_resolve_tree_id_prefers_flag_then_env_then_the_only_open_tree(self):
        class Args:
            tree = None
        with self.assertRaisesRegex(ts.TreeError, "no open tree"):
            ts.resolve_tree_id(Args())
        ts.save_manifest("n-1", ts.new_manifest("n-1", "f", "g", self.root, []))
        self.assertEqual(ts.resolve_tree_id(Args()), "n-1")
        done = ts.new_manifest("n-0", "f", "g", self.root, [])
        done["state"] = "finished"
        ts.save_manifest("n-0", done)
        self.assertEqual(ts.resolve_tree_id(Args()), "n-1")
        ts.save_manifest("n-2", ts.new_manifest("n-2", "f", "g", self.root, []))
        with self.assertRaisesRegex(ts.TreeError, "pass --tree"):
            ts.resolve_tree_id(Args())
        os.environ["NUCLEOS_TREE_ID"] = "n-2"
        self.assertEqual(ts.resolve_tree_id(Args()), "n-2")
        Args.tree = "n-1"
        self.assertEqual(ts.resolve_tree_id(Args()), "n-1")

    def test_open_findings_reads_the_latest_review(self):
        b = ts.new_branch("core", ["core/**"], "core", None)
        self.assertEqual(ts.open_findings(b), [])
        b["reviews"] = [{"at": "t", "findings": [{"id": "R1", "state": "open"}]},
                        {"at": "t", "findings": [{"id": "R1", "state": "fixed"}, {"id": "R2", "state": "open"}]}]
        self.assertEqual([f["id"] for f in ts.open_findings(b)], ["R2"])


class Overlap(unittest.TestCase):
    def test_same_cases_as_the_daemon_globs_overlap_test(self):
        self.assertTrue(ts.globs_overlap("src/**", "src/http/x.rs"))
        self.assertTrue(ts.globs_overlap("src/http/x.rs", "src/**"))
        self.assertFalse(ts.globs_overlap("core/a/**", "core/b/**"))
        self.assertTrue(ts.globs_overlap("**", "core/b/x.rs"))
        self.assertTrue(ts.globs_overlap("./src/**", "src\\lib.rs"))
        self.assertFalse(ts.globs_overlap("src/a.rs", "src/b.rs"))
        self.assertEqual(ts.sets_overlap(["x/**", "y/**"], ["z/**", "y/q.rs"]), ("y/**", "y/q.rs"))
        self.assertIsNone(ts.sets_overlap(["x/**"], []))

    def test_a_wildcard_inside_a_segment_cuts_the_prefix_there(self):
        self.assertFalse(ts.globs_overlap("core/a/**", "core/ab/**"))
        self.assertTrue(ts.globs_overlap("core/a*/x", "core/ab/y"))


class PathMatches(unittest.TestCase):
    def test_globs_and_literals(self):
        self.assertTrue(ts.path_matches("core/a/x.rs", "core/a/**"))
        self.assertTrue(ts.path_matches("core/a/b/x.rs", "core/a/**"))
        self.assertFalse(ts.path_matches("core/ab/x.rs", "core/a/**"))
        self.assertTrue(ts.path_matches("core/x.rs", "core/**/x.rs"))
        self.assertTrue(ts.path_matches("core/q/r/x.rs", "core/**/x.rs"))
        self.assertTrue(ts.path_matches("core/a/x.rs", "core/a/*.rs"))
        self.assertFalse(ts.path_matches("core/a/b/x.rs", "core/a/*.rs"))
        self.assertTrue(ts.path_matches("a/x.go", "a/*.{go,rs}"))
        self.assertFalse(ts.path_matches("a/x.py", "a/*.{go,rs}"))

    def test_a_literal_file_or_directory_and_backslashes(self):
        self.assertTrue(ts.path_matches("core/a.rs", "core/a.rs"))
        self.assertFalse(ts.path_matches("core/a.rs.bak", "core/a.rs"))
        self.assertTrue(ts.path_matches("core/dir/x.rs", "core/dir/"))
        self.assertTrue(ts.path_matches("core\\dir\\x.rs", "./core/dir"))
        self.assertFalse(ts.path_matches("core/directory/x.rs", "core/dir"))


def report(**over):
    fields = dict(node_id="n-2", root_id="n-1", status="done", role="leaf", outcome="done",
                  summary="Did it.", done=["edited a.rs"], files=["core/a.rs"],
                  commands=["cargo test -p x a"], contract_used=["A3"])
    fields.update(over)
    return ts.render_handoff(**fields)


class ContractIds(unittest.TestCase):
    def test_headings_and_table_first_cells(self):
        text = (
            "# Contract\n\n## A. Superfícies partilhadas\n### A3. Command `foo`\n"
            "| id | what |\n|---|---|\n| A4 | route |\n| `E2` | event |\n| **B1** | x |\n"
            "| note | A9 in a later cell |\n## E. Decisões\n### E1 — choice\n#### **E3**: bold\n"
            "```\n### A7 inside a fence\n```\n### A3x not an id\n### a5 lowercase\n")
        self.assertEqual(ts.contract_ids(text), {"A3", "A4", "E2", "B1", "E1", "E3"})

    def test_the_init_skeleton_has_no_ids(self):
        self.assertEqual(ts.contract_ids("## A. Superfícies partilhadas\n\n## E. Decisões\n"), set())


class Report(TreeCase):
    def setUp(self):
        super().setUp()
        self.cfg = ts.load_config()

    def test_a_rendered_handoff_parses_and_validates(self):
        parsed = ts.parse_report(report())
        self.assertEqual(
            (parsed["node_id"], parsed["root_id"], parsed["status"], parsed["role"], parsed["outcome"]),
            ("n-2", "n-1", "done", "leaf", "done"))
        self.assertEqual((parsed["contract_used"], parsed["files"], parsed["commands"], parsed["done"]),
                         (["A3"], ["core/a.rs"], ["cargo test -p x a"], ["edited a.rs"]))
        self.assertEqual((parsed["summary"], parsed["contract_request"], parsed["blocked_reason"]),
                         ("Did it.", "", ""))
        ts.validate_report(parsed, self.cfg)
        self.assertEqual(ts.report_problems(parsed, self.cfg), [])

    def test_the_unfilled_packet_does_not_validate(self):
        parsed = ts.parse_report((WORKFLOW / "packets" / "handoff.md").read_text(encoding="utf-8"))
        problems = ts.report_problems(parsed, self.cfg)
        self.assertTrue(any("node_id" in p for p in problems))
        self.assertTrue(any("Role" in p for p in problems))
        with self.assertRaises(ts.TreeError):
            ts.validate_report(parsed, self.cfg)

    def test_status_must_match_the_outcome(self):
        for status, outcome, ok in [("done", "done", True), ("done", "split", True),
                                    ("blocked", "contract_request", True), ("blocked", "blocked", True),
                                    ("failed", "failed", True), ("done", "failed", False),
                                    ("blocked", "done", False), ("handoff", "done", False)]:
            fields = dict(status=status, outcome=outcome, role="orchestrator", files=[], commands=[],
                          contract_request="what is missing" if outcome == "contract_request" else "none",
                          blocked_reason="stuck" if outcome in ("blocked", "failed") else "none")
            problems = ts.report_problems(ts.parse_report(report(**fields)), self.cfg)
            self.assertEqual(problems == [], ok, (status, outcome, problems))

    def test_role_rules(self):
        leaf_split = ts.report_problems(ts.parse_report(report(outcome="split")), self.cfg)
        self.assertTrue(any("only an orchestrator" in p for p in leaf_split))
        edits = ts.report_problems(ts.parse_report(report(role="orchestrator")), self.cfg)
        self.assertTrue(any("edits no file" in p for p in edits))
        review = ts.report_problems(ts.parse_report(report(role="review", files=[])), self.cfg)
        self.assertEqual(review, [])
        bad_role = ts.report_problems(ts.parse_report(report(role="boss")), self.cfg)
        self.assertTrue(any("Role" in p for p in bad_role))

    def test_summary_is_required_and_capped(self):
        self.assertTrue(any("summary" in p for p in ts.report_problems(ts.parse_report(report(summary="")), self.cfg)))
        long = ts.report_problems(ts.parse_report(report(summary="x" * 1501)), self.cfg)
        self.assertTrue(any("1500" in p for p in long))
        self.assertEqual(ts.report_problems(ts.parse_report(report(summary="x" * 1500)), self.cfg), [])

    def test_contract_request_and_blocked_reason_fields(self):
        no_text = ts.report_problems(ts.parse_report(report(
            status="blocked", outcome="contract_request", files=[])), self.cfg)
        self.assertTrue(any("Contract request" in p for p in no_text))
        stray = ts.report_problems(ts.parse_report(report(contract_request="a gap")), self.cfg)
        self.assertTrue(any("Contract request" in p for p in stray))
        no_reason = ts.report_problems(ts.parse_report(report(status="failed", outcome="failed")), self.cfg)
        self.assertTrue(any("Blocked reason" in p for p in no_reason))

    def test_contract_used_must_be_ids_known_to_the_contract(self):
        self.assertEqual(ts.parse_report(report(contract_used=[]))["contract_used"], [])
        self.assertEqual(ts.report_problems(ts.parse_report(report(contract_used=[])), self.cfg), [])
        parsed = ts.parse_report(report(contract_used=["A3", "E2"]))
        self.assertEqual(ts.report_problems(parsed, self.cfg, known_ids={"A3", "E2"}), [])
        missing = ts.report_problems(parsed, self.cfg, known_ids={"A3"})
        self.assertTrue(any("E2" in p and "contract.md" in p for p in missing))
        garbled = ts.report_problems(ts.parse_report(report(contract_used=["a3"])), self.cfg)
        self.assertTrue(any("a3" in p for p in garbled))
        raw = report().replace("Contract used: A3\n", "Contract used:\n")
        self.assertTrue(any("Contract used" in p for p in ts.report_problems(ts.parse_report(raw), self.cfg)))

    def test_the_line_cap_ignores_html_comments(self):
        long = report(done=[f"child {i}" for i in range(70)])
        self.assertTrue(any("lines" in p for p in ts.report_problems(ts.parse_report(long), self.cfg)))
        padded = report() + "<!-- x -->\n" * 100 + "<!--\n" + "many\n" * 100 + "-->\n"
        parsed = ts.parse_report(padded)
        self.assertLess(parsed["line_count"], 60)
        self.assertEqual(ts.report_problems(parsed, self.cfg), [])
        tight = dict(self.cfg, report_max_lines=10)
        self.assertTrue(any("lines" in p for p in ts.report_problems(ts.parse_report(report()), tight)))

    def test_render_handoff_is_a_valid_park_report(self):
        text = ts.render_handoff(node_id="m-1", root_id="n-1", status="blocked", role="leaf",
                                 outcome="blocked", summary="Parked.", decisions=["needs the owner"],
                                 blocked_reason="needs the owner")
        parsed = ts.parse_report(text)
        ts.validate_report(parsed, self.cfg)
        self.assertEqual(parsed["decisions"], ["needs the owner"])
        self.assertEqual(parsed["blocked_reason"], "needs the owner")


if __name__ == "__main__":
    unittest.main()
