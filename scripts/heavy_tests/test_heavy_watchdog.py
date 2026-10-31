#!/usr/bin/env python3
"""RED tests for the broker's stall watchdog (`scripts/heavy.py`).

A Rust test binary sat deadlocked for 23+ minutes at 0 CPU with no children and nobody noticed.
While the child runs, the broker must watch the CPU time of the child's whole process tree; when
it does not advance for NUCLEOS_HEAVY_STALL_SECS (default 600, 0 = off), the broker says so on
stderr, optionally dumps the threads with gdb, kills the tree and exits with EXIT_STALLED (76).

Every test runs the real broker on the command line with small Python children and a short
threshold. NUCLEOS_HEAVY_DIR points at a private temp dir, which gives the run its own empty
queue (admitted at once, no other session's token, lock or slot in sight) and also keeps it from
forwarding to the machine's staged broker. A Python child is not cargo, so it takes no worktree
lock, no target-dir slot and no CARGO_TARGET_DIR.

Seams these tests assume, none of which exists yet:
    EXIT_STALLED = 76
    stall_secs() -> float          NUCLEOS_HEAVY_STALL_SECS; unset or garbage 600.0, "0" 0.0
    NUCLEOS_HEAVY_GDB              gdb to use ahead of the fixed path and PATH; "0" = no dump
    NUCLEOS_HEAVY_GDB_TIMEOUT_SECS how long the dump may take

    python scripts/heavy_tests/test_heavy_watchdog.py -v
"""

from __future__ import annotations

import importlib.util
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
HEAVY = ROOT / "scripts" / "heavy.py"
# The real interpreter, not a venv launcher: the pid a child writes must be the broker's child.
PY = getattr(sys, "_base_executable", None) or sys.executable
# Long enough for a stalled child to outlive any assertion, short enough that the run before the
# watchdog exists fails on the exit code rather than on a timeout. Its own exit is CHILD_EXIT.
SLEEP_S = 25
CHILD_EXIT = 5


def load_heavy():
    spec = importlib.util.spec_from_file_location("heavy_under_test", HEAVY)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


heavy = load_heavy()


def burn(seconds: float) -> str:
    return f"t = time.monotonic()\nwhile time.monotonic() - t < {seconds}: pass\n"


class WatchdogCase(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-wd-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def env(self, stall: str, **extra: str) -> dict:
        env = {k: v for k, v in os.environ.items() if not k.startswith("NUCLEOS_HEAVY")}
        tmp = str(self.tmp)
        env.update(
            NUCLEOS_HEAVY_DIR=str(self.tmp / "state"),
            NUCLEOS_HEAVY_STALL_SECS=stall,
            # This machine has a real gdb: a test that is not about the dump must not pay for it.
            NUCLEOS_HEAVY_GDB="0",
            # Whatever a dump writes stays in this test's directory.
            NUCLEOS_DEV_HOME=tmp, TEMP=tmp, TMP=tmp, TMPDIR=tmp,
        )
        env.update(extra)
        return env

    def broker(self, script: Path, env: dict) -> tuple[subprocess.CompletedProcess, float]:
        t0 = time.monotonic()
        cp = subprocess.run(
            [sys.executable, str(HEAVY), "--", PY, str(script)],
            env=env, capture_output=True, text=True, encoding="utf-8", errors="replace",
            timeout=SLEEP_S + 35, cwd=str(ROOT),
        )
        return cp, time.monotonic() - t0

    def script(self, name: str, body: str) -> Path:
        path = self.tmp / name
        path.write_text("import os, pathlib, subprocess, sys, time\n" + body, encoding="utf-8")
        return path

    def pid_line(self, name: str) -> str:
        return f"pathlib.Path({str(self.tmp / name)!r}).write_text(str(os.getpid()))\n"

    def read_pid(self, name: str) -> int:
        return int((self.tmp / name).read_text())

    def sleeper(self, before: str = "") -> Path:
        """A child that blocks at 0 CPU past any assertion, then exits with CHILD_EXIT."""
        return self.script(
            "child.py",
            before + self.pid_line("child.pid")
            + f"time.sleep({SLEEP_S})\nraise SystemExit({CHILD_EXIT})\n")

    def assert_dead(self, pid: int, who: str) -> None:
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline and heavy.is_alive(pid):
            time.sleep(0.1)
        self.assertFalse(heavy.is_alive(pid), f"the {who} (pid {pid}) outlived the broker's kill")

    def assert_not_flagged(self, cp: subprocess.CompletedProcess) -> None:
        self.assertNotRegex(cp.stderr.lower(), r"\bstall|\bstuck")

    def stall_line(self, stderr: str, pid: int) -> str | None:
        """The stderr line that says the command is stuck, for how long, and which pid."""
        for line in stderr.splitlines():
            if (re.search(r"\bstall|\bstuck", line, re.I) and re.search(rf"\b{pid}\b", line)
                    and re.search(r"\b\d+(?:\.\d+)?\s*s(?:ec(?:ond)?s?)?\b", line)):
                return line
        return None


class StalledCommandTests(WatchdogCase):
    def test_a_stalled_command_says_so_kills_its_tree_and_exits_76(self) -> None:
        grand = self.script(
            "grand.py", self.pid_line("grand.pid") + f"time.sleep({SLEEP_S})\n")
        spawn = f"grand = subprocess.Popen([{PY!r}, {str(grand)!r}])\n"
        cp, _ = self.broker(self.sleeper(before=spawn), self.env("2"))
        self.assertEqual(cp.returncode, 76, cp.stderr)
        child = self.read_pid("child.pid")
        self.assertIsNotNone(self.stall_line(cp.stderr, child),
                             f"no line says stuck + duration + pid {child}:\n{cp.stderr}")
        self.assert_dead(child, "child")
        self.assert_dead(self.read_pid("grand.pid"), "grandchild")

    def test_the_run_is_logged_with_the_stalled_exit(self) -> None:
        cp, _ = self.broker(self.sleeper(), self.env("2"))
        self.assertEqual(cp.returncode, 76, cp.stderr)
        log = self.tmp / "state" / "log.jsonl"
        rows = [json.loads(r) for r in log.read_text(encoding="utf-8").splitlines() if r.strip()]
        self.assertEqual([r["exit"] for r in rows], [76])

    def test_the_stall_clock_counts_from_the_last_cpu_not_from_the_start(self) -> None:
        # Works past the threshold, then hangs: the shape of a test binary that deadlocks late.
        script = self.sleeper(before=burn(4))
        cp, elapsed = self.broker(script, self.env("2"))
        self.assertEqual(cp.returncode, 76, cp.stderr)
        self.assertGreaterEqual(elapsed, 5.0, "killed before 2 s had passed without CPU")
        self.assert_dead(self.read_pid("child.pid"), "child")


class DumpFailureTests(WatchdogCase):
    """A dump that fails or hangs never stands between a stalled command and its death."""

    def fake_gdb(self, then_windows: str, then_posix: str) -> Path:
        """A `gdb` that leaves a marker (so the dump is known to have been tried), then acts."""
        marker = self.tmp / "gdb-called"
        if os.name == "nt":
            path = self.tmp / "fake-gdb.cmd"
            path.write_text(f'@echo called> "{marker}"\n{then_windows}\n', encoding="utf-8")
        else:
            path = self.tmp / "fake-gdb"
            path.write_text(f'#!/bin/sh\necho called > "{marker}"\n{then_posix}\n',
                            encoding="utf-8")
            path.chmod(path.stat().st_mode | stat.S_IXUSR)
        return path

    def assert_killed_after_the_dump(self, cp: subprocess.CompletedProcess) -> None:
        self.assertEqual(cp.returncode, 76, cp.stderr)
        self.assertTrue((self.tmp / "gdb-called").exists(), "the dump was never attempted")
        self.assert_dead(self.read_pid("child.pid"), "child")

    def test_a_failing_dump_does_not_stop_the_kill(self) -> None:
        gdb = self.fake_gdb("@exit /b 1", "exit 1")
        cp, _ = self.broker(self.sleeper(), self.env("2", NUCLEOS_HEAVY_GDB=str(gdb)))
        self.assert_killed_after_the_dump(cp)

    def test_a_hanging_dump_expires_and_does_not_stop_the_kill(self) -> None:
        hang = f'"{PY}" -c "import time; time.sleep({SLEEP_S + 5})"'
        gdb = self.fake_gdb("@" + hang, hang)
        env = self.env("2", NUCLEOS_HEAVY_GDB=str(gdb), NUCLEOS_HEAVY_GDB_TIMEOUT_SECS="1")
        cp, elapsed = self.broker(self.sleeper(), env)
        self.assert_killed_after_the_dump(cp)
        self.assertLess(elapsed, SLEEP_S, "the broker waited for the hung gdb")


class HealthyCommandTests(WatchdogCase):
    """Guards: they pass before the watchdog exists, and must still pass once it does."""

    def test_a_silent_cpu_burner_is_not_killed(self) -> None:
        script = self.script("child.py", burn(6) + "raise SystemExit(7)\n")
        cp, _ = self.broker(script, self.env("3"))
        self.assertEqual(cp.returncode, 7, cp.stderr)
        self.assert_not_flagged(cp)

    def test_a_sleeping_child_with_a_busy_grandchild_is_not_killed(self) -> None:
        grand = self.script("grand.py", burn(6))
        script = self.script(
            "child.py",
            f"raise SystemExit(subprocess.Popen([{PY!r}, {str(grand)!r}]).wait() or 9)\n")
        cp, _ = self.broker(script, self.env("3"))
        self.assertEqual(cp.returncode, 9, cp.stderr)
        self.assert_not_flagged(cp)

    def test_a_command_that_ends_normally_keeps_its_exit_and_output(self) -> None:
        script = self.script(
            "child.py", "print('child-out', flush=True)\ntime.sleep(1)\nraise SystemExit(3)\n")
        cp, _ = self.broker(script, self.env("3"))
        self.assertEqual(cp.returncode, 3, cp.stderr)
        self.assertIn("child-out", cp.stdout)
        self.assert_not_flagged(cp)

    def test_zero_turns_the_watchdog_off(self) -> None:
        # A threshold of 0 read literally would kill at the first sample.
        script = self.script("child.py", "time.sleep(2.5)\nraise SystemExit(6)\n")
        cp, _ = self.broker(script, self.env("0"))
        self.assertEqual(cp.returncode, 6, cp.stderr)
        self.assert_not_flagged(cp)


class EveryPathTests(WatchdogCase):
    """`NUCLEOS_HEAVY=0` runs the child with no job, the same way the fail-open path does: there
    the CPU comes from summing the live process tree."""

    def test_the_bypass_is_watched_too(self) -> None:
        grand = self.script(
            "grand.py", self.pid_line("grand.pid") + f"time.sleep({SLEEP_S})\n")
        spawn = f"grand = subprocess.Popen([{PY!r}, {str(grand)!r}])\n"
        cp, _ = self.broker(self.sleeper(before=spawn), self.env("2", NUCLEOS_HEAVY="0"))
        self.assertEqual(cp.returncode, 76, cp.stderr)
        self.assert_dead(self.read_pid("child.pid"), "child")
        self.assert_dead(self.read_pid("grand.pid"), "grandchild")

    def test_a_busy_grandchild_is_seen_without_a_job(self) -> None:
        grand = self.script("grand.py", burn(6))
        script = self.script(
            "child.py",
            f"raise SystemExit(subprocess.Popen([{PY!r}, {str(grand)!r}]).wait() or 9)\n")
        cp, _ = self.broker(script, self.env("3", NUCLEOS_HEAVY="0"))
        self.assertEqual(cp.returncode, 9, cp.stderr)
        self.assert_not_flagged(cp)


class CargoLockTests(WatchdogCase):
    """Cargo queueing for a lock or compiling may show no CPU in its tree: the first waits its turn
    and the second's rustc runs in the sccache server. The broker reads the child's output whatever
    the child is, so a Python child that prints cargo's lines stands in for cargo."""

    LOCK_LINE = "    Blocking waiting for file lock on build directory"
    # Ended by `\n`, not cargo's `\r`: a `\r` at the end of the pipe stays unread until more output.
    BAR_LINE = "    Building [=>        ] 1/10: crate_a(test)"
    RUNNING_LINE = "     Running unittests src/lib.rs (target/debug/deps/x-0123)"

    def test_a_command_waiting_for_a_file_lock_is_not_killed(self) -> None:
        script = self.script(
            "child.py",
            f"print({self.LOCK_LINE!r}, file=sys.stderr, flush=True)\n"
            "time.sleep(6)\nraise SystemExit(8)\n")
        cp, _ = self.broker(script, self.env("2"))
        self.assertEqual(cp.returncode, 8, cp.stderr)
        self.assert_not_flagged(cp)

    def test_a_test_binary_that_hangs_after_the_lock_is_killed(self) -> None:
        # Nothing to rebuild: no bar ever clears `step`, and the binary then deadlocks.
        print_lines = (
            f"print({self.LOCK_LINE!r}, file=sys.stderr, flush=True)\n"
            "print('     Running unittests src/lib.rs (target/debug/deps/x-0123)',"
            " file=sys.stderr, flush=True)\n")
        cp, _ = self.broker(self.sleeper(before=print_lines), self.env("2"))
        self.assertEqual(cp.returncode, 76, cp.stderr)

    def test_a_compile_with_no_cpu_is_not_killed(self) -> None:
        script = self.script(
            "child.py",
            f"print({self.BAR_LINE!r}, file=sys.stderr, flush=True)\n"
            "time.sleep(6)\nraise SystemExit(8)\n")
        cp, _ = self.broker(script, self.env("2"))
        self.assertEqual(cp.returncode, 8, cp.stderr)
        self.assert_not_flagged(cp)

    def test_a_test_binary_with_no_cpu_after_a_compile_is_killed(self) -> None:
        print_lines = (
            f"print({self.BAR_LINE!r}, file=sys.stderr, flush=True)\n"
            f"print({self.RUNNING_LINE!r}, file=sys.stderr, flush=True)\n")
        cp, _ = self.broker(self.sleeper(before=print_lines), self.env("2"))
        self.assertEqual(cp.returncode, 76, cp.stderr)

    def test_only_a_build_phase_with_no_later_progress_counts(self) -> None:
        step = "Blocking waiting for file lock on build directory"
        for prog, want in (
                ({"step": step}, True),
                ({"step": step, "binary": "src/lib.rs"}, False),
                ({"step": step, "units_total": 10}, True),
                ({"step": step, "runner": "nextest"}, False),
                ({"units_done": 3, "units_total": 10}, True),
                ({"units_done": 3, "units_total": 10, "binary": "src/lib.rs"}, False),
                ({"units_done": 3, "units_total": 10, "runner": "nextest"}, False),
                # `Finished` fills the bar: what runs after it (`cargo run`) sets no `binary`.
                ({"units_done": 10, "units_total": 10}, False),
                ({"step": "Updating crates.io index"}, False),
                ({}, False)):
            with self.subTest(prog=prog):
                self.assertIs(heavy._cargo_still_building(prog), want)


class HistoryTests(WatchdogCase):
    """A stalled run says nothing about what its command costs or how long it takes."""

    def stalled_row(self) -> dict:
        cp, _ = self.broker(self.sleeper(), self.env("2"))
        self.assertEqual(cp.returncode, 76, cp.stderr)
        log = self.tmp / "state" / "log.jsonl"
        row = json.loads(log.read_text(encoding="utf-8").splitlines()[0])
        self.assertTrue(row["stalled"])
        return row

    def test_it_stays_out_of_the_cost_model_and_the_broker_estimate(self) -> None:
        row = dict(self.stalled_row(), run_s=600.0, peak_mem_mb=10.0, cores_avg=0.1)
        spec = importlib.util.spec_from_file_location(
            "heavy_cost_under_test", ROOT / "scripts" / "heavy_cost.py")
        cost = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cost)
        self.assertEqual(cost.build_costs([row] * 5)["classes"], {})
        self.assertEqual(cost.build_light([row] * 5)["classes"], {})

        state = self.tmp / "state"
        holder = {"argv": row["argv"], "acquired": time.time()}
        for code, estimated in ((76, False), (0, True)):
            with self.subTest(exit=code):
                (state / "log.jsonl").write_text(
                    json.dumps(dict(row, exit=code)) + "\n", encoding="utf-8")
                self.assertEqual(heavy._holder_estimate(state, holder) is not None, estimated)

    def test_it_stays_out_of_the_watch_view_medians(self) -> None:
        with mock.patch.dict(os.environ, {"NUCLEOS_HEAVY_DIR": str(self.tmp / "state")}):
            spec = importlib.util.spec_from_file_location(
                "heavy_watch_under_test", ROOT / "scripts" / "heavy_watch.py")
            watch = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(watch)
        argv = watch.heavy._log_argv(["cargo", "test"])
        rows = [{"argv": argv, "exit": 76, "run_s": 600.0, "cpu_s": 1.0, "worktree": "w"}] * 3
        rows += [{"argv": argv, "exit": 0, "run_s": 30.0, "cpu_s": 20.0, "worktree": "w"}]
        with mock.patch.object(watch, "history", return_value=rows):
            runs, _ = watch._past_runs({"argv": ["cargo", "test"], "worktree": "w"}, "run_s")
        self.assertEqual([r["exit"] for r in runs], [0])


class TreeWalkTests(unittest.TestCase):
    """The parts of the tree sum that run only on POSIX, or only when a job is missing."""

    def test_the_tree_is_the_root_and_its_descendants_parents_first(self) -> None:
        edges = [(2, 1), (3, 2), (4, 99), (1, 3), (5, 1)]  # 1-2-3-1 is a cycle from a reused pid
        self.assertEqual(heavy._tree_pids(1, edges), [1, 2, 5, 3])

    def test_ps_time_columns(self) -> None:
        for raw, want in (("00:10", 10.0), ("0:00.25", 0.25), ("01:02:03", 3723.0),
                          ("1-00:00:01", 86401.0)):
            with self.subTest(raw=raw):
                self.assertEqual(heavy._ps_seconds(raw), want)


class StallSettingTests(unittest.TestCase):
    def test_exit_stalled_is_76(self) -> None:
        self.assertEqual(heavy.EXIT_STALLED, 76)
        self.assertNotEqual(heavy.EXIT_STALLED, heavy.EXIT_QUEUE_TIMEOUT)

    def test_stall_secs_reads_the_env(self) -> None:
        # "0" must not fall back to the default, which is what `_env_float` does with it.
        for raw, want in (("0", 0.0), ("2", 2.0), ("not-a-number", 600.0)):
            with self.subTest(raw=raw), mock.patch.dict(
                    os.environ, {"NUCLEOS_HEAVY_STALL_SECS": raw}):
                self.assertEqual(heavy.stall_secs(), want)

    def test_stall_secs_defaults_to_ten_minutes(self) -> None:
        env = {k: v for k, v in os.environ.items() if k != "NUCLEOS_HEAVY_STALL_SECS"}
        with mock.patch.dict(os.environ, env, clear=True):
            self.assertEqual(heavy.stall_secs(), 600.0)


if __name__ == "__main__":
    unittest.main()
