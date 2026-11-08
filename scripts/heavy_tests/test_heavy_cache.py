#!/usr/bin/env python3
"""Result cache (`scripts/heavy_cache.py` and its hooks in the broker).

Hermetic like test_heavy_failfirst.py: temp NUCLEOS_HEAVY_DIR, throwaway git repos, fake
`cargo`/`go`/`npm`/`npx` launchers around one python script that logs its argv, prints a canned
output, optionally edits a file of the worktree mid-run, and exits as told.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
import heavy_cache as hc  # noqa: E402
import test_heavy_fingerprint as _fp  # noqa: E402

HEAVY = HERE.parents[1] / "scripts" / "heavy.py"

FAKE = r'''
import json, os, sys
with open(os.environ["FAKE_ARGV_LOG"], "a", encoding="utf-8") as f:
    f.write(json.dumps(sys.argv[1:]) + "\n")
touch = os.environ.get("FAKE_TOUCH")
if touch:
    with open(touch, "a", encoding="utf-8") as f:
        f.write("changed mid-run\n")
src = os.environ.get("FAKE_OUT_FILE")
if src and os.path.exists(src):
    sys.stdout.write(open(src, encoding="utf-8").read())
    sys.stdout.flush()
sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
'''

CARGO_FAIL = "running 2 tests\ntest a::bad_one ... FAILED\n\ntest result: FAILED. 1 passed; 1 failed\n"
OK_OUT = "running 2 tests\ntest a::one ... ok\ntest a::two ... ok\n\ntest result: ok. 2 passed\n"


class CacheTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-cache-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.state = self.tmp / "state"
        self.repo = _fp.make_repo(self.tmp / "wt-a")
        self.repo2 = _fp.make_repo(self.tmp / "wt-b")
        self.log = self.tmp / "argv.log"
        self.out = self.tmp / "out.txt"
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        (self.tmp / "fake.py").write_text(FAKE, encoding="utf-8")
        for name in ("cargo", "go", "npm", "npx", "bash"):
            if os.name == "nt":
                p = self.bin / f"{name}.cmd"
                p.write_text(f'@echo off\r\n"{sys.executable}" "{self.tmp / "fake.py"}" %*\r\n'
                             "exit /b %ERRORLEVEL%\r\n", encoding="utf-8")
            else:
                p = self.bin / name
                p.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{self.tmp / "fake.py"}" "$@"\n',
                             encoding="utf-8")
                p.chmod(0o755)

    def run_cmd(self, argv: list[str], out: str = OK_OUT, exit_code: int = 0,
                cwd: Path | None = None, **env_kw: str):
        """Returns (exit, stdout, stderr, number of times the fake program ran for this call)."""
        self.out.write_text(out, encoding="utf-8")
        env = dict(os.environ)
        for k in list(env):
            if k.startswith(("NUCLEOS_HEAVY", "FAKE_")) or k in ("CARGO_TARGET_DIR",):
                env.pop(k)
        env.update(NUCLEOS_HEAVY_DIR=str(self.state), NUCLEOS_HEAVY_LOCAL="1",
                   NUCLEOS_HEAVY_POLL_S="0.1", NUCLEOS_HEAVY_CAPACITY="4",
                   CARGO_TARGET_DIR=str(self.tmp / "td"), FAKE_ARGV_LOG=str(self.log),
                   FAKE_OUT_FILE=str(self.out), FAKE_EXIT=str(exit_code))
        env.update(env_kw)
        before = len(self.calls())
        exe = str(self.bin / (argv[0] + (".cmd" if os.name == "nt" else "")))
        cp = subprocess.run([sys.executable, str(HEAVY), "--wait-max", "60", "--", exe, *argv[1:]],
                            env=env, cwd=str(cwd or self.repo), capture_output=True, text=True,
                            timeout=120)
        return cp.returncode, cp.stdout, cp.stderr, len(self.calls()) - before

    def calls(self) -> list[list[str]]:
        if not self.log.exists():
            return []
        return [json.loads(x) for x in self.log.read_text(encoding="utf-8").splitlines()]

    def rows(self) -> list[dict]:
        p = self.state / "log.jsonl"
        return [json.loads(x) for x in p.read_text(encoding="utf-8").splitlines()] if p.exists() else []

    def entries(self) -> list[Path]:
        d = self.state / "rescache"
        return sorted(d.glob("*.json")) if d.exists() else []

    def sandbox(self) -> set[str]:
        """Queue, token, lock and slot files the broker leaves under its state dir."""
        out = set()
        for p in self.state.rglob("*"):
            rel = p.relative_to(self.state).as_posix()
            if p.is_file() and not rel.startswith(("rescache", "failfirst", "log.jsonl", "costs",
                                                   "light")):
                out.add(rel)
        return out

    ARGV = ["cargo", "test", "-p", "x"]

    # -- cases

    def test_identical_state_hits_and_skips_the_run(self) -> None:
        rc, _, _, ran = self.run_cmd(self.ARGV)
        self.assertEqual((rc, ran), (0, 1))
        rc, out, err, ran = self.run_cmd(self.ARGV, "should not be shown\n", 7)
        self.assertEqual(rc, 0)
        self.assertEqual(ran, 0, "the child must not run on a hit")
        self.assertIn("CACHED", err)
        self.assertIn("identical source state", err)
        self.assertIn("NUCLEOS_HEAVY_FORCE=1", err)
        self.assertIn("test result: ok. 2 passed", out)  # the recorded tail, not the new output
        self.assertNotIn("should not be shown", out)

    def test_each_runner_hits(self) -> None:
        for argv in (["go", "vet", "./..."], ["go", "test", "-race", "./..."], ["npm", "test"],
                     ["npx", "vitest", "run"], ["cargo", "clippy", "-p", "x"]):
            with self.subTest(argv=argv):
                self.assertEqual(self.run_cmd(argv)[3], 1)
                self.assertEqual(self.run_cmd(argv)[3], 0)

    def test_gates_script_is_never_cached(self) -> None:
        # gates.sh picks its targets from a merge-base diff and exports NUCLEOS_HEAVY_FULL only
        # after the outer lookup: the same tree can need different gates. Its inner commands cache.
        argv = ["bash", "scripts/gates.sh", "rust"]
        self.assertFalse(hc.cacheable(argv))
        self.assertEqual(self.run_cmd(argv)[3], 1)
        self.assertEqual(self.run_cmd(argv)[3], 1)
        self.assertEqual(self.entries(), [])

    def test_final_gate_trees_never_use_the_cache(self) -> None:
        for name in ("postgate-demo", "integration-7"):
            with self.subTest(tree=name):
                tree = _fp.make_repo(self.tmp / "gt" / ".nucleos" / "worktrees" / name)
                self.assertEqual(self.run_cmd(self.ARGV, cwd=tree)[3], 1)
                self.assertEqual(self.run_cmd(self.ARGV, cwd=tree)[3], 1)
                self.assertEqual(self.entries(), [], "and nothing is stored")

    def test_non_verification_commands_are_not_cached(self) -> None:
        for argv in (["cargo", "build"], ["npm", "install"], ["go", "build", "./..."]):
            with self.subTest(argv=argv):
                self.run_cmd(argv)
                self.assertEqual(self.run_cmd(argv)[3], 1)

    def test_tracked_change_misses(self) -> None:
        self.run_cmd(self.ARGV)
        tracked = next(p for p in self.repo.rglob("*") if p.is_file() and ".git" not in p.parts)
        tracked.write_text(tracked.read_text(encoding="utf-8") + "\n// edit\n", encoding="utf-8")
        self.assertEqual(self.run_cmd(self.ARGV)[3], 1)
        self.assertEqual(self.run_cmd(self.ARGV)[3], 0, "the new state is cached in turn")

    def test_untracked_new_file_misses(self) -> None:
        self.run_cmd(self.ARGV)
        (self.repo / "brand_new.rs").write_text("fn x() {}\n", encoding="utf-8")
        self.assertEqual(self.run_cmd(self.ARGV)[3], 1)

    def test_ignored_file_does_not_miss(self) -> None:
        (self.repo / ".gitignore").write_text("junk/\n", encoding="utf-8")
        self.run_cmd(self.ARGV)
        (self.repo / "junk").mkdir()
        (self.repo / "junk" / "a.txt").write_text("x", encoding="utf-8")
        self.assertEqual(self.run_cmd(self.ARGV)[3], 0)

    def test_commit_of_the_same_content_still_hits(self) -> None:
        (self.repo / "new.rs").write_text("fn y() {}\n", encoding="utf-8")
        self.run_cmd(self.ARGV)
        _fp.git(self.repo, "add", "-A")
        _fp.git(self.repo, "commit", "-q", "-m", "x")
        self.assertEqual(self.run_cmd(self.ARGV)[3], 0)

    def test_failure_is_not_cached(self) -> None:
        argv = ["go", "vet", "./..."]
        rc, _, _, ran = self.run_cmd(argv, "boom\n", 1)
        self.assertEqual((rc, ran), (1, 1))
        self.assertEqual(self.entries(), [])
        rc, _, _, ran = self.run_cmd(argv, "boom\n", 1)
        self.assertEqual((rc, ran), (1, 1))

    def test_a_failure_drops_an_earlier_success_of_the_key(self) -> None:
        argv = ["go", "vet", "./..."]
        self.run_cmd(argv)
        self.assertEqual(len(self.entries()), 1)
        self.run_cmd(argv, "boom\n", 1, NUCLEOS_HEAVY_FORCE="1")
        self.assertEqual(self.entries(), [], "a flaky command must not keep a stale green")

    def test_narrowed_success_is_not_cached(self) -> None:
        self.run_cmd(self.ARGV, CARGO_FAIL, 101)
        rc, _, err, ran = self.run_cmd(self.ARGV)  # failed-first: narrowed, passes
        self.assertEqual((rc, ran), (0, 1))
        self.assertIn("ONLY", err)
        self.assertEqual(self.entries(), [])
        rc, out, err, ran = self.run_cmd(self.ARGV)  # now a full run, not a hit
        self.assertEqual(ran, 1)
        self.assertNotIn("ONLY", err)
        self.assertEqual(len(self.entries()), 1)

    def test_failed_first_record_blocks_a_hit(self) -> None:
        self.run_cmd(self.ARGV)  # cached green
        self.run_cmd(self.ARGV, CARGO_FAIL, 101, NUCLEOS_HEAVY_FORCE="1")  # then red
        rc, _, err, ran = self.run_cmd(self.ARGV)
        self.assertEqual(ran, 1, "the record wins over any cache entry")
        self.assertIn("ONLY", err)

    def test_full_env_bypasses_the_cache(self) -> None:
        self.run_cmd(self.ARGV)
        self.assertEqual(self.run_cmd(self.ARGV, NUCLEOS_HEAVY_FULL="1")[3], 1)
        shutil.rmtree(self.state / "rescache")
        self.run_cmd(self.ARGV, NUCLEOS_HEAVY_FULL="1")
        self.assertEqual(self.entries(), [], "a FULL run stores nothing either")

    def test_opt_out(self) -> None:
        self.run_cmd(self.ARGV, NUCLEOS_HEAVY_CACHE="0")
        self.assertEqual(self.entries(), [])
        self.run_cmd(self.ARGV)
        self.assertEqual(self.run_cmd(self.ARGV, NUCLEOS_HEAVY_CACHE="0")[3], 1)

    def test_force_reruns_once_and_refreshes(self) -> None:
        self.run_cmd(self.ARGV)
        self.assertEqual(self.run_cmd(self.ARGV, NUCLEOS_HEAVY_FORCE="1")[3], 1)
        self.assertEqual(self.run_cmd(self.ARGV)[3], 0)

    def test_ttl(self) -> None:
        self.run_cmd(self.ARGV)
        (p,) = self.entries()
        rec = json.loads(p.read_text(encoding="utf-8"))
        rec["ts"] = time.time() - hc.TTL_S + 600
        p.write_text(json.dumps(rec), encoding="utf-8")
        self.assertEqual(self.run_cmd(self.ARGV)[3], 0, "inside the TTL")
        rec["ts"] = time.time() - hc.TTL_S - 60
        p.write_text(json.dumps(rec), encoding="utf-8")
        self.assertEqual(self.run_cmd(self.ARGV)[3], 1, "past the TTL")

    def test_other_worktree_misses(self) -> None:
        self.run_cmd(self.ARGV)
        self.assertEqual(self.run_cmd(self.ARGV, cwd=self.repo2)[3], 1)

    def test_other_argv_and_env_miss(self) -> None:
        self.run_cmd(self.ARGV)
        self.assertEqual(self.run_cmd(["cargo", "test", "-p", "y"])[3], 1)
        self.assertEqual(self.run_cmd(self.ARGV, CARGO_TARGET_DIR=str(self.tmp / "td2"))[3], 1)
        self.assertEqual(self.run_cmd(self.ARGV, RUSTFLAGS="-Dwarnings")[3], 1)
        self.assertEqual(self.run_cmd(self.ARGV)[3], 0)

    def test_hit_takes_no_queue_token_or_lock(self) -> None:
        self.run_cmd(self.ARGV)
        before = self.sandbox()
        rc, _, err, ran = self.run_cmd(self.ARGV)
        self.assertEqual((rc, ran), (0, 0))
        self.assertEqual(self.sandbox(), before)
        hit = self.rows()[-1]
        self.assertTrue(hit["cache_hit"])
        self.assertEqual((hit["weight"], hit["token"], hit["wait_lock_s"], hit["wait_token_s"]),
                         (0, False, 0.0, 0.0))

    def test_hit_does_not_wait_for_a_busy_worktree_lock(self) -> None:
        self.run_cmd(self.ARGV)
        slow = self.tmp / "slow.py"
        slow.write_text("import time; time.sleep(25)\n", encoding="utf-8")
        env = dict(os.environ, NUCLEOS_HEAVY_DIR=str(self.state), NUCLEOS_HEAVY_LOCAL="1",
                   NUCLEOS_HEAVY_CAPACITY="4", CARGO_TARGET_DIR=str(self.tmp / "td"))
        holder = subprocess.Popen([sys.executable, str(HEAVY), "--kind", "cargo", "--",
                                   sys.executable, str(slow)], env=env, cwd=str(self.repo),
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.addCleanup(holder.kill)
        time.sleep(2)
        t = time.time()
        rc, _, _, ran = self.run_cmd(self.ARGV)
        self.assertEqual((rc, ran), (0, 0))
        self.assertLess(time.time() - t, 15)

    def test_source_change_mid_run_is_not_cached(self) -> None:
        touch = self.repo / "touched.txt"
        rc, _, err, ran = self.run_cmd(self.ARGV, FAKE_TOUCH=str(touch))
        self.assertEqual((rc, ran), (0, 1))
        self.assertIn("source changed", err)
        self.assertEqual(self.entries(), [])

    def test_log_row_has_the_source_state(self) -> None:
        self.run_cmd(self.ARGV)
        self.run_cmd(["cargo", "build"])  # not cacheable, still stamped
        self.run_cmd(self.ARGV)  # hit
        tree = subprocess.run(["git", "rev-parse", "HEAD^{tree}"], cwd=self.repo, check=True,
                              capture_output=True, text=True).stdout.strip()
        rows = self.rows()
        self.assertGreaterEqual(len(rows), 3)
        for r in rows:
            self.assertEqual(r.get("src_state"), tree, r)

    def test_state_cost_is_small(self) -> None:
        t = time.time()
        st = hc.source_state(str(self.repo))
        self.assertIsNotNone(st)
        self.assertLess(time.time() - t, 5)

    def test_store_is_bounded(self) -> None:
        old = hc.MAX_ENTRIES
        hc.MAX_ENTRIES = 5
        self.addCleanup(setattr, hc, "MAX_ENTRIES", old)
        for i in range(9):
            hc._store(self.state, f"k{i:02d}", {"exit": 0, "ts": time.time()})
            time.sleep(0.02)
        self.assertEqual(len(self.entries()), 5)


if __name__ == "__main__":
    unittest.main()
