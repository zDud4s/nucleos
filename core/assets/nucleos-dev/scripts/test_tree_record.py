"""Tests for tree_record.py: the record verb and file ownership (spec 8.2)."""
import argparse
import io
import json
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import tree_record  # noqa: E402
import tree_state  # noqa: E402
from tree_state import TreeError  # noqa: E402

def git(cwd, *args):
    return subprocess.run(
        ["git", "-C", str(cwd), *args], check=True, capture_output=True, text=True
    ).stdout

def write(root, rel, text):
    path = Path(root) / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")

def read(root, rel):
    return (Path(root) / rel).read_text(encoding="utf-8")

def make_repo(root):
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    git(root, "init", "-q")
    git(root, "config", "user.email", "t@example.com")
    git(root, "config", "user.name", "t")
    git(root, "config", "commit.gpgsign", "false")
    git(root, "config", "core.autocrlf", "false")
    write(root, "a/x.txt", "x0\n")
    write(root, "b/y.txt", "y0\n")
    write(root, "c/z.txt", "z0\n")
    git(root, "add", "-A")
    git(root, "commit", "-q", "-m", "base")
    return root

class GitLayerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.repo = make_repo(self.tmp / "repo")

    def test_diff_entries_lists_modified_and_untracked_files_not_directories(self):
        write(self.repo, "a/x.txt", "x1\n")
        write(self.repo, "a/sub/deep.txt", "d\n")
        write(self.repo, "a/sub/more/deeper.txt", "d\n")
        entries = tree_record.diff_entries(self.repo)
        self.assertEqual(
            sorted(entries),
            sorted(
                [
                    (" M", "a/x.txt"),
                    ("??", "a/sub/deep.txt"),
                    ("??", "a/sub/more/deeper.txt"),
                ]
            ),
        )

    def test_diff_entries_reports_a_rename_as_an_add_and_a_delete(self):
        git(self.repo, "mv", "a/x.txt", "a/r.txt")
        entries = tree_record.diff_entries(self.repo)
        self.assertIn(("A ", "a/r.txt"), entries)
        self.assertIn(("D ", "a/x.txt"), entries)
        self.assertEqual(len(entries), 2)

    def test_revert_restores_tracked_deletes_untracked_and_unstages_adds(self):
        write(self.repo, "a/x.txt", "x1\n")
        write(self.repo, "a/new.txt", "n\n")
        write(self.repo, "a/staged.txt", "s\n")
        git(self.repo, "add", "a/staged.txt")
        for code, path in tree_record.diff_entries(self.repo):
            tree_record.revert(self.repo, code, path)
        self.assertEqual(read(self.repo, "a/x.txt"), "x0\n")
        self.assertFalse((self.repo / "a/new.txt").exists())
        self.assertFalse((self.repo / "a/staged.txt").exists())
        self.assertEqual(tree_record.diff_entries(self.repo), [])

class LoadNodesTests(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def dump(self, data):
        path = self.tmp / "nodes.json"
        path.write_text(json.dumps(data), encoding="utf-8")
        return path

    def test_accepts_the_node_tree_object(self):
        root, nodes = tree_record.load_nodes(
            self.dump(
                {
                    "root_id": "R",
                    "goal": "g",
                    "nodes": [
                        {"id": "R", "parent_id": None, "depth": 0, "kind": "root", "status": "running"},
                        {"id": "L", "parent_id": "R", "depth": 1, "kind": "leaf", "status": "running"},
                    ],
                }
            )
        )
        self.assertEqual(root, "R")
        self.assertEqual([n["id"] for n in nodes], ["R", "L"])

def fake_parse(text):
    try:
        data = json.loads(text)
    except ValueError as exc:
        raise TreeError(f"report is not parseable: {exc}")
    if not isinstance(data, dict):
        raise TreeError("report is not an object")
    return data

def fake_validate(report, cfg, known_ids=None):
    if not report.get("summary"):
        raise TreeError("invalid report: report has no summary")
    missing = [
        c for c in report.get("contract_used") or [] if known_ids is not None and c not in known_ids
    ]
    if missing:
        raise TreeError("invalid report: " + "; ".join(f"Contract used {c} is not in contract.md" for c in missing))

def fake_contract_ids(text):
    return set(re.findall(r"^##\s+([A-Z]\d+)", text, re.M))

class RecordTests(unittest.TestCase):
    """Each test drives run_record on a real temp git repo; the chunk 4 helpers are faked."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.repo = make_repo(self.tmp / "repo")
        self.tree_dir = self.tmp / "state" / "trees" / "R"
        self.tree_dir.mkdir(parents=True)
        (self.tree_dir / "contract.md").write_text(
            "# Contract\n\n## A3 shape\nx\n\n## E2 decision\ny\n", encoding="utf-8"
        )
        self.manifest = {
            "tree_id": "R",
            "branches": {"core": {"name": "core", "node_id": "B", "worktree": str(self.repo)}},
            "records": [],
            "attempts": {},
        }
        self.nodes = [
            self.node("R", None, 0, "root", []),
            self.node("B", "R", 1, "orchestrator", ["a/**", "b/**", "c/**"]),
            self.node("L1", "B", 2, "leaf", ["a/**"]),
            self.node("L2", "B", 2, "leaf", ["b/**"]),
            self.node("O", "B", 2, "orchestrator", ["c/**"]),
        ]
        for target, value in (
            ("contract_path", lambda tid, root=None: self.tree_dir / "contract.md"),
            (
                "handoff_file",
                lambda tid, nid, root=None: self.tmp / "state" / "nodes" / tid / f"{nid}.md",
            ),
            ("load_manifest", lambda tid: self.manifest),
            ("save_manifest", lambda tid, m: setattr(self, "manifest", m)),
            ("load_config", lambda root=None: {"report_max_lines": 60}),
            ("contract_ids", fake_contract_ids),
            ("parse_report", fake_parse),
            ("validate_report", fake_validate),
        ):
            patcher = mock.patch.object(tree_state, target, value)
            patcher.start()
            self.addCleanup(patcher.stop)

    @staticmethod
    def node(node_id, parent, depth, kind, owned, status="running"):
        return {
            "id": node_id,
            "parent_id": parent,
            "depth": depth,
            "kind": kind,
            "status": status,
            "owned_paths": owned,
        }

    def report(self, node_id, files, used=(), status="done"):
        return json.dumps(
            {
                "node_id": node_id,
                "root_id": "R",
                "status": status,
                "role": "leaf",
                "outcome": "done",
                "contract_used": list(used),
                "summary": f"{node_id} did the work",
                "files": list(files),
                "commands": [],
            }
        )

    def rec(self, node_id, text, owned=None, tree="R"):
        nodes_file = self.tmp / "nodes.json"
        nodes_file.write_text(json.dumps({"root_id": "R", "nodes": self.nodes}), encoding="utf-8")
        report_file = self.tmp / "report.txt"
        report_file.write_text(text, encoding="utf-8")
        args = argparse.Namespace(
            node_id=node_id,
            branch="core",
            nodes=str(nodes_file),
            report_file=str(report_file),
            owned=owned,
            tree=tree,
        )
        return tree_record.run_record(args)

    def report_path(self, node_id):
        return self.tmp / "state" / "nodes" / "R" / f"{node_id}.md"

    # -- the 8.2 table, row by row ------------------------------------------------------------

    def test_row1_owned_and_declared_is_accepted(self):
        write(self.repo, "a/x.txt", "x1\n")
        out = self.rec("L1", self.report("L1", ["a/x.txt"]))
        self.assertEqual(out["verdict"], "accepted")
        self.assertEqual(out["accepted"], ["a/x.txt"])
        self.assertEqual(out["warnings"], [])
        self.assertEqual(out["refused"], [])
        self.assertEqual(out["reverted"], [])
        self.assertEqual(out["failed_nodes"], [])
        self.assertEqual(read(self.repo, "a/x.txt"), "x1\n")
        self.assertEqual(
            out["wip_commit"],
            {"message": "wip(core): record L1", "paths": ["a/x.txt"]},
        )

    def test_row2_owned_but_undeclared_is_accepted_with_a_warning(self):
        write(self.repo, "a/x.txt", "x1\n")
        write(self.repo, "a/new.txt", "n\n")
        out = self.rec("L1", self.report("L1", ["a/x.txt"]))
        self.assertEqual(out["verdict"], "accepted")
        self.assertEqual(out["accepted"], ["a/new.txt", "a/x.txt"])
        self.assertEqual(len(out["warnings"]), 1)
        self.assertIn("a/new.txt", out["warnings"][0])
        self.assertEqual(out["reverted"], [])
        self.assertTrue((self.repo / "a/new.txt").exists())

    def test_row3_declared_but_not_owned_and_unowned_is_refused_and_reverted(self):
        write(self.repo, "a/x.txt", "x1\n")
        write(self.repo, "c/z.txt", "z1\n")
        out = self.rec("L1", self.report("L1", ["a/x.txt", "c/z.txt"]))
        self.assertEqual(out["verdict"], "failed")
        self.assertEqual(out["refused"], ["c/z.txt"])
        self.assertEqual(out["reverted"], ["c/z.txt"])
        self.assertEqual(out["failed_nodes"], ["L1"])
        self.assertEqual(read(self.repo, "c/z.txt"), "z0\n")
        self.assertEqual(out["accepted"], ["a/x.txt"])
        close = out["mcp"][0]
        self.assertEqual(close["tool"], "node_close")
        self.assertEqual(close["args"]["id"], "L1")
        self.assertEqual(close["args"]["status"], "failed")

    def test_row3_declared_in_a_live_sibling_file_is_refused_but_not_reverted(self):
        write(self.repo, "b/y.txt", "y1\n")  # L2's work, still uncommitted
        out = self.rec("L1", self.report("L1", ["b/y.txt"]))
        self.assertEqual(out["verdict"], "failed")
        self.assertEqual(out["refused"], ["b/y.txt"])
        self.assertEqual(out["reverted"], [])
        self.assertEqual(out["failed_nodes"], ["L1"])
        self.assertEqual(read(self.repo, "b/y.txt"), "y1\n")

    def test_row3_declared_in_an_unrecorded_closed_siblings_file_is_refused_not_reverted(self):
        self.nodes[3]["status"] = "done"  # L2 closed itself, its record is still to come
        write(self.repo, "b/y.txt", "y1\n")
        out = self.rec("L1", self.report("L1", ["b/y.txt"]))
        self.assertEqual(out["verdict"], "failed")
        self.assertEqual(out["reverted"], [])
        self.assertEqual(read(self.repo, "b/y.txt"), "y1\n")

    def test_an_unrecorded_done_siblings_diff_is_left_for_its_own_record(self):
        self.nodes[3]["status"] = "done"  # direct launch: L2 closed itself before being recorded
        write(self.repo, "a/x.txt", "x1\n")
        write(self.repo, "b/y.txt", "y1\n")
        out = self.rec("L1", self.report("L1", ["a/x.txt"]))
        self.assertEqual(out["verdict"], "accepted")
        self.assertEqual(out["reverted"], [])
        self.assertEqual(out["foreign"], [])
        self.assertEqual(read(self.repo, "b/y.txt"), "y1\n")

    def test_a_recorded_leaf_no_longer_owns_its_paths(self):
        # L2 was accepted (and committed by the controller); a later stray write is foreign.
        self.manifest["records"].append({"node_id": "L2", "verdict": "accepted"})
        write(self.repo, "b/y.txt", "y1\n")
        out = self.rec("L1", self.report("L1", []))
        self.assertEqual(out["reverted"], ["b/y.txt"])
        self.assertEqual(out["foreign"], [{"file": "b/y.txt", "charged": "B"}])

    def test_a_retry_record_does_not_release_a_leafs_paths(self):
        self.manifest["records"].append({"node_id": "L2", "verdict": "retry"})
        write(self.repo, "b/y.txt", "y1\n")
        out = self.rec("L1", self.report("L1", []))
        self.assertEqual(out["reverted"], [])
        self.assertEqual(read(self.repo, "b/y.txt"), "y1\n")

    def test_a_handoff_predecessors_files_are_owned_by_its_successor(self):
        self.nodes.append(self.node("L1p", "B", 2, "leaf", ["a/**"], status="handoff"))
        write(self.repo, "a/pred.txt", "p\n")  # written by the predecessor before the relaunch
        write(self.repo, "a/x.txt", "x1\n")
        out = self.rec("L1", self.report("L1", ["a/x.txt"]))  # the successor
        self.assertEqual(out["verdict"], "accepted")
        self.assertEqual(out["accepted"], ["a/pred.txt", "a/x.txt"])
        self.assertEqual(out["reverted"], [])

    def test_row4_a_live_siblings_file_is_left_alone(self):
        write(self.repo, "a/x.txt", "x1\n")
        write(self.repo, "b/y.txt", "y1\n")
        out = self.rec("L1", self.report("L1", ["a/x.txt"]))
        self.assertEqual(out["verdict"], "accepted")
        self.assertEqual(out["accepted"], ["a/x.txt"])
        self.assertEqual(out["reverted"], [])
        self.assertEqual(out["foreign"], [])
        self.assertEqual(read(self.repo, "b/y.txt"), "y1\n")
        self.assertEqual(out["wip_commit"]["paths"], ["a/x.txt"])

    def test_row5_foreign_write_is_reverted_and_charged_to_the_deepest_orchestrator(self):
        write(self.repo, "a/x.txt", "x1\n")
        write(self.repo, "c/z.txt", "z1\n")  # inside O (depth 2) and B (depth 1), no live leaf
        write(self.repo, "d/q.txt", "q\n")  # inside no orchestrator: charged to the branch node
        out = self.rec("L1", self.report("L1", ["a/x.txt"]))
        self.assertEqual(out["verdict"], "accepted")
        self.assertEqual(out["reverted"], ["c/z.txt", "d/q.txt"])
        self.assertEqual(
            out["foreign"],
            [{"file": "c/z.txt", "charged": "O"}, {"file": "d/q.txt", "charged": "B"}],
        )
        self.assertEqual(out["failed_nodes"], ["B", "O"])
        self.assertEqual(read(self.repo, "c/z.txt"), "z0\n")
        self.assertFalse((self.repo / "d/q.txt").exists())
        self.assertEqual(out["accepted"], ["a/x.txt"])

    def test_an_orchestrator_that_edits_is_charged_to_itself(self):
        write(self.repo, "c/z.txt", "z1\n")
        text = json.dumps(
            {
                "node_id": "O", "root_id": "R", "status": "done", "role": "orchestrator",
                "outcome": "split", "contract_used": [], "summary": "split it", "files": [],
                "commands": [],
            }
        )
        out = self.rec("O", text)
        self.assertEqual(out["verdict"], "failed")
        self.assertEqual(out["failed_nodes"], ["O"])
        self.assertEqual(out["reverted"], ["c/z.txt"])
        self.assertEqual(out["mcp"][0]["args"]["status"], "failed")

    # -- reports ------------------------------------------------------------------------------

    def test_the_report_is_written_where_node_close_expects_it(self):
        write(self.repo, "a/x.txt", "x1\n")
        text = self.report("L1", ["a/x.txt"])
        out = self.rec("L1", text)
        path = self.report_path("L1")
        self.assertEqual(path.read_text(encoding="utf-8"), text)
        close = out["mcp"][0]
        self.assertEqual(close["tool"], "node_close")
        self.assertEqual(
            close["args"],
            {
                "id": "L1",
                "status": "done",
                "summary": "L1 did the work",
                "handoff_path": str(path),
            },
        )

    def test_an_already_closed_leaf_yields_no_node_close(self):
        self.nodes[2]["status"] = "done"  # L1 closed itself (direct launch)
        write(self.repo, "a/x.txt", "x1\n")
        out = self.rec("L1", self.report("L1", ["a/x.txt"]))
        self.assertEqual(out["verdict"], "accepted")
        self.assertEqual(out["mcp"], [])
        self.assertTrue(self.report_path("L1").is_file())  # the record still files the report
        bad = self.rec("L1", json.dumps({"node_id": "L1", "summary": ""}))  # attempt 1: retry
        self.assertEqual(bad["mcp"], [])
        again = self.rec("L1", json.dumps({"node_id": "L1", "summary": ""}))  # attempt 2: failed
        self.assertEqual((again["verdict"], again["mcp"]), ("failed", []))

    def test_declared_files_may_carry_backticks_and_an_annotation(self):
        write(self.repo, "a/x.txt", "x1\n")
        out = self.rec("L1", self.report("L1", ["`a/x.txt` (modified)"]))
        self.assertEqual(out["accepted"], ["a/x.txt"])
        self.assertEqual(out["warnings"], [])

    def test_the_tree_must_match_the_nodes_root(self):
        write(self.repo, "a/x.txt", "x1\n")
        with self.assertRaises(TreeError):
            self.rec("L1", self.report("L1", ["a/x.txt"]), tree="OTHER")
        self.assertEqual(read(self.repo, "a/x.txt"), "x1\n")

    def test_invalid_report_is_a_retry_then_failed_and_never_reverts(self):
        write(self.repo, "a/x.txt", "x1\n")
        write(self.repo, "c/z.txt", "z1\n")  # would be foreign if the diff were checked
        first = self.rec("L1", "not json at all")
        self.assertEqual(first["verdict"], "retry")
        self.assertEqual(first["accepted"], [])
        self.assertEqual(first["reverted"], [])
        self.assertEqual(first["mcp"], [])
        self.assertIn("not parseable", first["error"])
        self.assertEqual(self.manifest["attempts"]["L1"], 1)
        self.assertEqual(read(self.repo, "a/x.txt"), "x1\n")
        self.assertEqual(read(self.repo, "c/z.txt"), "z1\n")
        self.assertFalse(self.report_path("L1").exists())

        second = self.rec("L1", json.dumps({"node_id": "L1", "summary": ""}))
        self.assertEqual(second["verdict"], "failed")
        self.assertEqual(second["failed_nodes"], ["L1"])
        self.assertEqual(self.manifest["attempts"]["L1"], 2)
        self.assertEqual(read(self.repo, "a/x.txt"), "x1\n")  # still nothing reverted
        self.assertEqual(read(self.repo, "c/z.txt"), "z1\n")
        close = second["mcp"][0]
        self.assertEqual(close["args"]["status"], "failed")
        self.assertTrue(Path(close["args"]["handoff_path"]).is_file())
        self.assertEqual([r["verdict"] for r in self.manifest["records"]], ["retry", "failed"])

    def test_relaunching_does_not_revert_when_the_leaf_returned_no_report(self):
        write(self.repo, "a/x.txt", "x1\n")
        write(self.repo, "a/new.txt", "n\n")
        out = self.rec("L1", "")  # the leaf came back with no report at all
        self.assertEqual(out["verdict"], "retry")
        self.assertEqual(out["reverted"], [])
        self.assertEqual(out["accepted"], [])
        self.assertEqual(out["mcp"], [])
        self.assertEqual(self.manifest["attempts"]["L1"], 1)
        self.assertEqual(read(self.repo, "a/x.txt"), "x1\n")  # the relaunch keeps the diff
        self.assertEqual(read(self.repo, "a/new.txt"), "n\n")
        self.assertEqual(
            sorted(tree_record.diff_entries(self.repo)),
            sorted([(" M", "a/x.txt"), ("??", "a/new.txt")]),
        )

    def test_contract_used_must_exist_in_the_contract(self):
        write(self.repo, "a/x.txt", "x1\n")
        out = self.rec("L1", self.report("L1", ["a/x.txt"], used=["A3", "E9"]))
        self.assertEqual(out["verdict"], "retry")
        self.assertIn("E9", out["error"])
        self.assertNotIn("A3", out["error"])
        self.assertEqual(out["reverted"], [])
        ok = self.rec("L1", self.report("L1", ["a/x.txt"], used=["A3", "E2"]))
        self.assertEqual(ok["verdict"], "accepted")

    def test_contract_used_without_a_contract_file_is_an_invalid_report(self):
        (self.tree_dir / "contract.md").unlink()
        write(self.repo, "a/x.txt", "x1\n")
        out = self.rec("L1", self.report("L1", ["a/x.txt"], used=["A3"]))
        self.assertEqual(out["verdict"], "retry")
        self.assertIn("no contract.md", out["error"])
        ok = self.rec("L1", self.report("L1", ["a/x.txt"]))
        self.assertEqual(ok["verdict"], "accepted")

    # -- files and the manifest ---------------------------------------------------------------

    def test_the_record_is_appended_to_the_manifest(self):
        write(self.repo, "a/x.txt", "x1\n")
        self.rec("L1", self.report("L1", ["a/x.txt"]))
        record = self.manifest["records"][-1]
        self.assertEqual(
            (record["node_id"], record["branch"], record["verdict"], record["accepted"]),
            ("L1", "core", "accepted", ["a/x.txt"]),
        )
        self.assertIn("at", record)

    def test_the_report_is_read_from_stdin_without_report_file(self):
        write(self.repo, "a/x.txt", "x1\n")
        nodes_file = self.tmp / "nodes.json"
        nodes_file.write_text(json.dumps({"root_id": "R", "nodes": self.nodes}), encoding="utf-8")
        args = argparse.Namespace(
            node_id="L1", branch="core", nodes=str(nodes_file), report_file=None, owned=None,
            tree="R",
        )
        with mock.patch.object(sys, "stdin", io.TextIOWrapper(io.BytesIO(self.report("L1", ["a/x.txt"]).encode("utf-8")), encoding="utf-8")):
            self.assertEqual(tree_record.run_record(args)["verdict"], "accepted")

    # -- inputs the record cannot work without ------------------------------------------------

    def test_a_live_leaf_without_owned_paths_stops_before_reverting_anything(self):
        for node in self.nodes:
            if node["id"] == "L2":
                node.pop("owned_paths")
        write(self.repo, "c/z.txt", "z1\n")
        with self.assertRaises(TreeError) as ctx:
            self.rec("L1", self.report("L1", []))
        self.assertIn("L2", str(ctx.exception))
        self.assertEqual(read(self.repo, "c/z.txt"), "z1\n")
        owned = self.tmp / "owned.json"
        owned.write_text(json.dumps({"L2": ["b/**"]}), encoding="utf-8")
        out = self.rec("L1", self.report("L1", []), owned=str(owned))
        self.assertEqual(out["reverted"], ["c/z.txt"])


class RegisterTests(unittest.TestCase):
    def test_register_adds_the_record_verb_with_its_options(self):
        verbs = {}
        tree_record.register(verbs)
        add_arguments, run = verbs["record"]
        self.assertIs(run, tree_record.run_record)
        parser = argparse.ArgumentParser()
        add_arguments(parser)
        args = parser.parse_args(
            ["L1", "--branch", "core", "--nodes", "n.json", "--report-file", "r.md", "--owned", "o.json"]
        )
        self.assertEqual(
            (args.node_id, args.branch, args.nodes, args.report_file, args.owned),
            ("L1", "core", "n.json", "r.md", "o.json"),
        )
        self.assertIsNone(args.tree)
        self.assertEqual(parser.parse_args(["L1", "--branch", "b", "--nodes", "n", "--tree", "T"]).tree, "T")


if __name__ == "__main__":
    unittest.main()
