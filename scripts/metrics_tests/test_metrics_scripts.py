import json
import os
import sqlite3
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
import leaf_initial_context as lic  # noqa: E402
import tree_autonomy_rate as tar  # noqa: E402


def fixture():
    c = sqlite3.connect(":memory:")
    c.executescript(
        """
        CREATE TABLE tree_jobs (id TEXT, state TEXT, updated_at TEXT);
        CREATE TABLE decisions (id TEXT, tree_id TEXT, answer TEXT, origin TEXT);
        CREATE TABLE work_nodes (id TEXT, kind TEXT, status TEXT, tree_job_id TEXT);
        CREATE TABLE work_node_runs (node_id TEXT, run_id INTEGER);
        CREATE TABLE runs (id INTEGER, created_at TEXT);
        CREATE TABLE run_events (run_id INTEGER, seq INTEGER, kind TEXT, payload TEXT);
        """
    )
    return c


class AutonomyRate(unittest.TestCase):
    def test_counts_jobs_without_owner_answer(self):
        c = fixture()
        c.executemany(
            "INSERT INTO tree_jobs VALUES (?,?,?)",
            [("a", "finished", "2026-11-10"), ("b", "finished", "2026-11-10"),
             ("c", "open", "2026-11-10"), ("old", "finished", "2026-01-01")],
        )
        c.executemany(
            "INSERT INTO decisions VALUES (?,?,?,?)",
            [("1", "a", "yes", "shell"), ("2", "b", "ok", "auto_default"), ("3", "b", None, "agent")],
        )
        c.executemany(
            "INSERT INTO work_nodes VALUES (?,?,?,?)",
            [("n1", "leaf", "done", "a"), ("n2", "leaf", "failed", "b"), ("n3", "leaf", "blocked", "b")],
        )
        r = tar.measure(c, "2026-11-01")
        self.assertEqual((r["terminated_jobs"], r["without_intervention"]), (2, 1))
        self.assertEqual(r["rate"], 0.5)
        self.assertEqual((r["nodes_total"], r["nodes_blocked"], r["nodes_failed"]), (3, 1, 1))

    def test_empty_window(self):
        self.assertIsNone(tar.measure(fixture(), "2026-11-01")["rate"])


class LeafContext(unittest.TestCase):
    def test_before_after_medians(self):
        c = fixture()
        c.execute("INSERT INTO work_nodes VALUES ('n','leaf','done','j')")
        usage = lambda i, w, r: json.dumps(
            {"message": {"usage": {"input_tokens": i, "cache_creation_input_tokens": w,
                                   "cache_read_input_tokens": r}}})
        runs = [(1, "2026-11-05", 100), (2, "2026-11-06", 300), (3, "2026-11-12", 50)]
        for rid, created, tokens in runs:
            c.execute("INSERT INTO runs VALUES (?,?)", (rid, created))
            c.execute("INSERT INTO work_node_runs VALUES ('n',?)", (rid,))
            c.execute("INSERT INTO run_events VALUES (?,1,'assistant',?)", (rid, usage(1, tokens - 1, 0)))
        before, after = lic.measure(c, "2026-10-29", "2026-11-10", "2026-11-11T06:00:00")
        self.assertEqual((before, after), ([100, 300], [50]))


if __name__ == "__main__":
    unittest.main()
