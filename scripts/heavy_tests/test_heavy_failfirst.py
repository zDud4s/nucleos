#!/usr/bin/env python3
"""Failed-first re-runs (`scripts/heavy_failfirst.py` and its hook in the broker).

A brokered test command that fails records the failing test ids, keyed by (worktree, command);
the next identical run narrows to them, a pass clears the record, and the daemon's postgate/integration gate trees (NUCLEOS_HEAVY_FULL=1,
exported by scripts/gates.sh) always runs in full.

Hermetic: temp NUCLEOS_HEAVY_DIR, a throwaway git repo, and fake `cargo`/`go`/`npm`/`npx`
launchers around one python script that logs its argv, prints a canned output and exits as told.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
import heavy_failfirst as ff  # noqa: E402
import test_heavy_fingerprint as _fp  # noqa: E402

HEAVY = HERE.parents[1] / "scripts" / "heavy.py"

FAKE = r'''
import json, os, sys
with open(os.environ["FAKE_ARGV_LOG"], "a", encoding="utf-8") as f:
    f.write(json.dumps(sys.argv[1:]) + "\n")
src = os.environ.get("FAKE_OUT_FILE")
if src and os.path.exists(src):
    sys.stdout.write(open(src, encoding="utf-8").read())
    sys.stdout.flush()
sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
'''

NEXTEST_OUT = """\
    Starting 3 tests across 1 binary
        PASS [   0.010s] (1/3) nx::bin ok_one
        FAIL [   0.011s] (2/3) nx::bin tests::bad_one
        FAIL [   0.012s] (3/3) nx::bin tests::bad_two
------------
     Summary [   0.020s] 3 tests run: 1 passed, 2 failed, 0 skipped
        FAIL [   0.011s] ( 2/3) nx::bin tests::bad_one
"""
CARGO_OUT = """\
running 3 tests
test a::ok_one ... ok
test a::bad_one ... FAILED
test b::bad_two ... FAILED

test result: FAILED. 1 passed; 2 failed; 0 ignored
"""
GO_OUT = """\
=== RUN   TestBad
--- FAIL: TestBad (0.00s)
=== RUN   TestSub
    --- FAIL: TestSub/case_a (0.00s)
--- FAIL: TestSub (0.00s)
FAIL
FAIL\texample.com/x\t0.010s
"""
JS_OUT = """\
 FAIL  src/a.test.ts > suite > does a thing
 FAIL  src/b.spec.tsx > other
 Test Files  2 failed | 1 passed (3)
"""


class UnitTests(unittest.TestCase):
    def test_parse_each_runner(self) -> None:
        self.assertEqual(ff.parse_failures("nextest", NEXTEST_OUT.splitlines()),
                         ["tests::bad_one", "tests::bad_two"])
        self.assertEqual(ff.parse_failures("cargo-test", CARGO_OUT.splitlines()),
                         ["a::bad_one", "b::bad_two"])
        self.assertEqual(ff.parse_failures("go", GO_OUT.splitlines()), ["TestBad", "TestSub"])
        self.assertEqual(ff.parse_failures("js", JS_OUT.splitlines()),
                         ["src/a.test.ts", "src/b.spec.tsx"])

    def test_unparseable_output_gives_none(self) -> None:
        for runner in ("nextest", "cargo-test", "go", "js"):
            self.assertIsNone(ff.parse_failures(runner, ["Segmentation fault\n", "error: boom\n"]))

    def test_nextest_retry_lines_are_not_failures(self) -> None:
        lines = ["        TRY 1 FAIL [   0.1s] (1/1) nx::bin flaky\n",
                 "        PASS [   0.1s] (1/1) nx::bin flaky\n"]
        self.assertIsNone(ff.parse_failures("nextest", lines))

    def test_detect(self) -> None:
        d = ff.detect
        self.assertEqual(d(["cargo", "nextest", "run", "--workspace"]), "nextest")
        self.assertEqual(d(["C:/x/cargo.exe", "test", "-p", "a"]), "cargo-test")
        self.assertIsNone(d(["cargo", "test", "--no-run"]))
        self.assertIsNone(d(["cargo", "build"]))
        self.assertEqual(d(["go", "test", "./..."]), "go")
        self.assertIsNone(d(["go", "vet", "./..."]))
        self.assertEqual(d(["npm", "--prefix", "shell", "test"]), "js")
        self.assertEqual(d(["npm", "run", "test"]), "js")
        self.assertEqual(d(["npx", "vitest", "run"]), "js")
        self.assertIsNone(d(["npm", "run", "build"]))

    def test_narrow_nextest(self) -> None:
        argv, why = ff.narrow("nextest", ["cargo", "nextest", "run", "-p", "x", "--", "--nocapture"],
                              ["t::a", "t::b"])
        self.assertEqual(why, "")
        self.assertEqual(argv, ["cargo", "nextest", "run", "-p", "x", "-E",
                                "test(=t::a) | test(=t::b)", "--", "--nocapture"])

    def test_narrow_cargo_test(self) -> None:
        argv, _ = ff.narrow("cargo-test", ["cargo", "test", "-p", "x"], ["a::b", "c::d"])
        self.assertEqual(argv, ["cargo", "test", "-p", "x", "--", "a::b", "c::d", "--exact"])
        argv, _ = ff.narrow("cargo-test", ["cargo", "test", "--", "--test-threads", "1"], ["a::b"])
        self.assertEqual(argv, ["cargo", "test", "--", "--test-threads", "1", "a::b", "--exact"])

    def test_narrow_go_and_js(self) -> None:
        argv, _ = ff.narrow("go", ["go", "test", "-count=1", "./..."], ["TestA", "TestB"])
        self.assertEqual(argv, ["go", "test", "-run", "^(TestA|TestB)$", "-count=1", "./..."])
        argv, _ = ff.narrow("js", ["npm", "test"], ["src/a.test.ts"])
        self.assertEqual(argv, ["npm", "test", "--", "src/a.test.ts"])
        argv, _ = ff.narrow("js", ["npx", "vitest", "run"], ["src/a.test.ts"], ["new.test.ts"])
        self.assertEqual(argv, ["npx", "vitest", "run", "src/a.test.ts", "new.test.ts"])

    def test_cannot_narrow_safely_says_why(self) -> None:
        cases = [
            ("nextest", ["cargo", "nextest", "run", "-E", "kind(lib)"]),
            ("nextest", ["cargo", "nextest", "run", "some_filter"]),
            ("cargo-test", ["cargo", "test", "some_filter"]),
            ("cargo-test", ["cargo", "test", "--", "some_filter"]),
            ("go", ["go", "test", "-run", "TestX", "./..."]),
            ("js", ["npx", "vitest"]),
        ]
        for runner, argv in cases:
            narrowed, why = ff.narrow(runner, argv, ["a::b"])
            self.assertIsNone(narrowed, argv)
            self.assertTrue(why, argv)

    def test_key_ignores_program_path_and_extension(self) -> None:
        a = ff._key("/w", ["C:/bin/cargo.exe", "test"])
        self.assertEqual(a, ff._key("/w", ["cargo", "test"]))
        self.assertNotEqual(a, ff._key("/other", ["cargo", "test"]))
        self.assertNotEqual(a, ff._key("/w", ["cargo", "test", "-p", "x"]))


class BrokerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-ff-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.state = self.tmp / "state"
        self.repo = _fp.make_repo(self.tmp / "wt-a")
        self.repo2 = _fp.make_repo(self.tmp / "wt-b")
        self.log = self.tmp / "argv.log"
        self.out = self.tmp / "out.txt"
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        (self.tmp / "fake.py").write_text(FAKE, encoding="utf-8")
        for name in ("cargo", "go", "npm", "npx"):
            if os.name == "nt":
                p = self.bin / f"{name}.cmd"
                p.write_text(f'@echo off\r\n"{sys.executable}" "{self.tmp / "fake.py"}" %*\r\n'
                             "exit /b %ERRORLEVEL%\r\n", encoding="utf-8")
            else:
                p = self.bin / name
                p.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{self.tmp / "fake.py"}" "$@"\n',
                             encoding="utf-8")
                p.chmod(0o755)

    def run_cmd(self, argv: list[str], out: str = "", exit_code: int = 0, cwd: Path | None = None,
                **env_kw: str) -> tuple[int, str, list[str]]:
        """Run `argv` (argv[0] a fake launcher name) through the broker.
        Returns (exit, stderr, the argv the fake program received)."""
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
        calls = self.calls()
        self.assertEqual(len(calls), before + 1, f"the program must run once: {cp.stderr!r}")
        got = calls[-1]
        while "--target-dir" in got:  # the broker's own pooled target dir, not under test
            i = got.index("--target-dir")
            got = got[:i] + got[i + 2:]
        return cp.returncode, cp.stderr, got

    def calls(self) -> list[list[str]]:
        if not self.log.exists():
            return []
        return [json.loads(x) for x in self.log.read_text(encoding="utf-8").splitlines()]

    def records(self) -> list[dict]:
        d = self.state / "failfirst"
        return [json.loads(p.read_text(encoding="utf-8")) for p in d.glob("*.json")] if d.exists() else []

    # -- cases

    def test_failure_is_recorded(self) -> None:
        rc, err, args = self.run_cmd(["cargo", "test", "-p", "x"], CARGO_OUT, 101)
        self.assertEqual(rc, 101)
        self.assertEqual(args, ["test", "-p", "x"])  # the first run is always full
        recs = self.records()
        self.assertEqual(len(recs), 1)
        self.assertEqual(recs[0]["ids"], ["a::bad_one", "b::bad_two"])
        self.assertEqual(recs[0]["runner"], "cargo-test")
        self.assertNotIn("ONLY", err)

    def test_next_run_is_narrowed_for_each_runner(self) -> None:
        cases = [
            (["cargo", "nextest", "run", "-p", "x"], NEXTEST_OUT,
             ["nextest", "run", "-p", "x", "-E", "test(=tests::bad_one) | test(=tests::bad_two)"]),
            (["cargo", "test", "-p", "x"], CARGO_OUT,
             ["test", "-p", "x", "--", "a::bad_one", "b::bad_two", "--exact"]),
            # one name, and no `^` on Windows: cmd.exe eats `|` and `^` in the fake's .cmd shim
            # (the real go.exe is no shim); the multi-name pattern is in test_narrow_go_and_js
            (["go", "test", "./..."], "--- FAIL: TestBad (0.00s)\nFAIL\n",
             ["test", "-run", ("" if os.name == "nt" else "^") + "(TestBad)$", "./..."]),
            (["npm", "test"], JS_OUT, ["test", "--", "src/a.test.ts", "src/b.spec.tsx"]),
            (["npx", "vitest", "run"], JS_OUT, ["vitest", "run", "src/a.test.ts", "src/b.spec.tsx"]),
        ]
        for argv, out, expected in cases:
            with self.subTest(argv=argv):
                self.run_cmd(argv, out, 1)
                rc, err, args = self.run_cmd(argv, "", 1)
                self.assertEqual(args, expected)
                self.assertIn("ONLY", err)
                self.assertIn("did NOT run", err)
                self.assertIn("NUCLEOS_HEAVY_FULL=1", err)

    def test_pass_clears_the_record_and_next_run_is_full(self) -> None:
        argv = ["cargo", "test", "-p", "x"]
        self.run_cmd(argv, CARGO_OUT, 101)
        rc, err, args = self.run_cmd(argv, "test result: ok\n", 0)
        self.assertEqual(rc, 0)
        self.assertIn("ONLY", err)
        self.assertIn("not the whole suite", err)  # a narrowed pass is never reported as green
        self.assertEqual(self.records(), [])
        rc, err, args = self.run_cmd(argv, "test result: ok\n", 0)
        self.assertEqual(args, ["test", "-p", "x"])
        self.assertNotIn("ONLY", err)

    def test_still_failing_narrowed_run_replaces_the_record(self) -> None:
        argv = ["cargo", "test"]
        self.run_cmd(argv, CARGO_OUT, 101)
        self.run_cmd(argv, "test a::bad_one ... FAILED\n", 101)
        self.assertEqual(self.records()[0]["ids"], ["a::bad_one"])

    def test_opt_out(self) -> None:
        argv = ["cargo", "test"]
        self.run_cmd(argv, CARGO_OUT, 101, NUCLEOS_HEAVY_FAILFIRST="0")
        self.assertEqual(self.records(), [], "opted out: nothing is recorded")
        self.run_cmd(argv, CARGO_OUT, 101)  # record one
        rc, err, args = self.run_cmd(argv, "", 101, NUCLEOS_HEAVY_FAILFIRST="0")
        self.assertEqual(args, ["test"])
        self.assertNotIn("ONLY", err)

    def test_gate_path_stays_full(self) -> None:
        argv = ["cargo", "test"]
        self.run_cmd(argv, CARGO_OUT, 101)
        rc, err, args = self.run_cmd(argv, CARGO_OUT, 101, NUCLEOS_HEAVY_FULL="1")
        self.assertEqual(args, ["test"])
        self.assertNotIn("re-running ONLY", err)
        self.assertIn("full run was requested", err)

    def test_gates_script_exports_the_full_signal(self) -> None:
        text = (HEAVY.parent / "gates.sh").read_text(encoding="utf-8")
        i = text.index("export NUCLEOS_HEAVY_FULL=1")
        self.assertGreater(i, text.index('if [ "${BASH_SOURCE[0]}" != "$0" ]'))
        self.assertLess(i, text.index('target="${1:-all}"'))

    def gates_full(self, parts: tuple[str, ...], caller: str | None = None) -> str:
        """What gates.sh's FULL block leaves NUCLEOS_HEAVY_FULL as, run in a git tree at `parts`."""
        text = (HEAVY.parent / "gates.sh").read_text(encoding="utf-8")
        a = text.index('if [ -z "${NUCLEOS_HEAVY_FULL+x}" ]')
        block = text[a:text.index("\nfi\n", a) + 4]
        bash = None
        for cand in ("C:/Program Files/Git/bin/bash.exe", "/bin/bash"):
            if os.path.exists(cand):
                bash = cand
                break
        bash = bash or shutil.which("bash")
        if not bash:
            self.skipTest("no bash")
        with tempfile.TemporaryDirectory() as d:
            tree = Path(d).joinpath(*parts)
            tree.mkdir(parents=True)
            subprocess.run(["git", "init", "-q", str(tree)], check=True)
            env = {k: v for k, v in os.environ.items() if k != "NUCLEOS_HEAVY_FULL"}
            if caller is not None:
                env["NUCLEOS_HEAVY_FULL"] = caller
            out = subprocess.run(
                [bash, "-c", block + '\nprintf "%s" "${NUCLEOS_HEAVY_FULL-unset}"'],
                cwd=tree, env=env, capture_output=True, text=True, check=True,
            )
            return out.stdout

    def test_gates_forces_full_in_postgate_and_integration_trees(self) -> None:
        self.assertEqual(self.gates_full((".nucleos", "worktrees", "postgate-demo")), "1")
        self.assertEqual(self.gates_full((".nucleos", "worktrees", "integration-7")), "1")

    def test_gates_leaves_an_agent_worktree_narrowable(self) -> None:
        self.assertEqual(self.gates_full((".claude", "worktrees", "feature")), "unset")
        self.assertEqual(self.gates_full(("postgate-demo",)), "unset")

    def test_gates_respects_a_caller_set_full(self) -> None:
        self.assertEqual(self.gates_full((".claude", "worktrees", "feature"), caller="1"), "1")
        self.assertEqual(
            self.gates_full((".nucleos", "worktrees", "postgate-demo"), caller="0"), "0")

    def test_unparseable_output_runs_full(self) -> None:
        argv = ["cargo", "test"]
        self.run_cmd(argv, CARGO_OUT, 101)
        self.run_cmd(argv, "Segmentation fault\n", 139)  # narrowed run died without ids
        self.assertEqual(self.records(), [])
        rc, err, args = self.run_cmd(argv, "", 0)
        self.assertEqual(args, ["test"])

    def test_unparseable_first_failure_records_nothing_and_says_so(self) -> None:
        rc, err, args = self.run_cmd(["go", "test", "./..."], "panic: boom\n", 2)
        self.assertEqual(self.records(), [])
        self.assertIn("could not read failing test ids", err)

    def test_unsafe_to_narrow_runs_full_and_says_why(self) -> None:
        self.run_cmd(["go", "test", "-run", "TestBad", "./..."], GO_OUT, 1)
        rc, err, args = self.run_cmd(["go", "test", "-run", "TestBad", "./..."], "", 1)
        self.assertEqual(args, ["test", "-run", "TestBad", "./..."])
        self.assertIn("not narrowed", err)
        self.assertIn("-run", err)

    def test_other_worktree_is_not_affected(self) -> None:
        argv = ["cargo", "test"]
        self.run_cmd(argv, CARGO_OUT, 101)
        rc, err, args = self.run_cmd(argv, "", 0, cwd=self.repo2)
        self.assertEqual(args, ["test"])
        self.assertNotIn("ONLY", err)

    def test_compile_error_keeps_the_record(self) -> None:
        argv = ["cargo", "test"]
        self.run_cmd(argv, CARGO_OUT, 101)
        self.run_cmd(argv, "error: could not compile `x`\n", 101)
        self.assertEqual(self.records()[0]["ids"], ["a::bad_one", "b::bad_two"])

    def test_changed_test_file_joins_the_vitest_run(self) -> None:
        argv = ["npm", "test"]
        self.run_cmd(argv, JS_OUT, 1)
        (self.repo / "shell" / "src").mkdir(parents=True, exist_ok=True)
        (self.repo / "shell" / "src" / "new.test.ts").write_text("x\n", encoding="utf-8")
        rc, err, args = self.run_cmd(argv, "", 1)
        self.assertEqual(args, ["test", "--", "src/a.test.ts", "src/b.spec.tsx", "new.test.ts"])


if __name__ == "__main__":
    unittest.main()
