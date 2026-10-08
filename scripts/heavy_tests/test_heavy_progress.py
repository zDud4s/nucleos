#!/usr/bin/env python3
"""Tests for the broker's progress reading (`scripts/heavy.py`): segment splitting on `\\r`,
cargo's bar and the test-runner lines, and that a caller that is not a terminal gets the
child's output without the bar the broker turned on for heavy_watch."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import os
import sys
import tempfile
import threading
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("heavy", ROOT / "scripts" / "heavy.py")
heavy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(heavy)


class Chunks(io.RawIOBase):
    """A pipe that hands its data over in the given pieces."""

    def __init__(self, parts: list[bytes]):
        self.parts = list(parts)

    def read1(self, size: int = -1) -> bytes:
        return self.parts.pop(0) if self.parts else b""


class SegmentsTest(unittest.TestCase):
    def test_a_lone_cr_ends_a_segment_and_crlf_stays_one(self):
        segs, rest = heavy._segments(b"a\r\nb\rc\nd", eof=False)
        self.assertEqual(segs, [b"a\r\n", b"b\r", b"c\n"])
        self.assertEqual(rest, b"d")

    def test_a_trailing_cr_waits_for_the_next_chunk(self):
        segs, rest = heavy._segments(b"x\r", eof=False)
        self.assertEqual((segs, rest), ([], b"x\r"))
        self.assertEqual(heavy._segments(b"x\r", eof=True), ([b"x\r"], b""))


class NoteProgressTest(unittest.TestCase):
    def test_cargo_bar_gives_units_and_is_marked_as_bar(self):
        prog = {}
        bar = b"\x1b[1m    Building\x1b[0m [=====>     ] 120/310: nucleos_core(test), foo   \r"
        self.assertTrue(heavy._note_progress(bar, prog))
        self.assertEqual((prog["units_done"], prog["units_total"]), (120, 310))
        self.assertEqual(prog["building"], "nucleos_core(test), foo")

    def test_test_binary_and_tests_are_counted(self):
        prog = {}
        lines = [
            rb"     Running unittests src\main.rs (C:\t\debug\deps\nucleos_core-0123456789abcdef.exe)" + b"\n",
            b"running 3 tests\n", b"test a ... ok\n", b"test b ... FAILED\n",
            b"   Doc-tests probe\n",
        ]
        for line in lines[:4]:
            self.assertFalse(heavy._note_progress(line, prog))
        self.assertEqual((prog["binary"], prog["binaries"]), ("nucleos_core", 1))
        self.assertEqual((prog["tests_done"], prog["tests_total"]), (2, 3))
        heavy._note_progress(lines[4], prog)
        self.assertEqual((prog["binary"], prog["binaries"], prog["tests_done"]), ("probe", 2, 0))


class ChildRunTest(unittest.TestCase):
    def test_a_child_test_process_does_not_reset_the_binary_total(self):
        # scheduler.rs re-runs its own test binary for one test; the child prints into the
        # same output, with its own `running 1 test`.
        prog = {}
        for line in [rb"     Running unittests src\lib.rs (C:\t\deps\nucleos_core-0123456789abcdef.exe)" + b"\n",
                     b"running 4000 tests\n", b"test a ... ok\n",
                     b"running 1 test\n", b"test b ... ok\n", b"test c ... ok\n"]:
            heavy._note_progress(line, prog)
        self.assertEqual((prog["tests_total"], prog["tests_done"]), (4000, 3))

    def test_the_next_binary_takes_its_own_total(self):
        prog = {}
        for line in [b"     Running unittests src\\lib.rs (C:\\t\\deps\\a-0123456789abcdef.exe)\n",
                     b"running 5 tests\n",
                     b"     Running tests\\x.rs (C:\\t\\deps\\x-0123456789abcdef.exe)\n",
                     b"running 2 tests\n"]:
            heavy._note_progress(line, prog)
        self.assertEqual((prog["binary"], prog["tests_total"]), ("x", 2))


class LibtestDetailTest(unittest.TestCase):
    def test_failures_and_the_last_finished_test_are_kept(self):
        prog = {}
        for line in [b"running 2 tests\n", b"test m::a ... ok\n", b"test m::b ... FAILED\n"]:
            heavy._note_progress(line, prog)
        self.assertEqual((prog["failed"], prog["last_test"]), (1, "m::b"))

    def test_finished_completes_the_units_cargo_never_draws(self):
        prog = {}
        heavy._note_progress(b"    Building [====> ] 314/315: nucleos_core(test)\r", prog)
        heavy._note_progress(b"    Finished `test` profile [unoptimized] target(s) in 9s\n", prog)
        self.assertEqual(prog["units_done"], 315)


class NextestTest(unittest.TestCase):
    # Captured from cargo-nextest 0.9.146 writing into a pipe.
    OUTPUT = [
        b"    Finished `test` profile [unoptimized + debuginfo] target(s) in 16.32s\n",
        b" Nextest run ID 00c7b8ee-b89f-4660-bb12-e016d79c70ab with nextest profile: default\n",
        b"    Starting 3 tests across 2 binaries (1 test skipped)\n",
        b"        PASS [   0.332s] (1/3) nx::x it\n",
        b"        PASS [   0.394s] (2/3) nx a\n",
        b"        FAIL [   0.385s] (3/3) nx b\n",
        b"    running 1 test\n",
        b"    test b ... FAILED\n",
        b"    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 2 filtered out\n",
        b"     Summary [   0.509s] 3 tests run: 2 passed, 1 failed, 1 skipped\n",
        b"        FAIL [   0.385s] (3/3) nx b\n",
    ]

    def run_lines(self, lines):
        prog = {}
        for line in lines:
            self.assertFalse(heavy._note_progress(line, prog))
        return prog

    def test_counts_come_from_the_runner_line(self):
        prog = self.run_lines(self.OUTPUT[:5])
        self.assertEqual((prog["runner"], prog["binaries"]), ("nextest", 2))
        self.assertEqual((prog["tests_done"], prog["tests_total"], prog["last_test"]),
                         (2, 3, "nx a"))

    def test_a_failure_counts_once_despite_its_echoes(self):
        prog = self.run_lines(self.OUTPUT)
        self.assertEqual((prog["tests_done"], prog["failed"], prog["last_test"]), (3, 1, "nx b"))

    def test_slow_and_retried_attempts_are_not_failures(self):
        prog = self.run_lines([
            b"    Starting 2 tests across 1 binary\n",
            b"        SLOW [> 60.000s] (1/2) nx slow\n",
            b"   TRY 1 FAIL [   0.100s] (1/2) nx flaky\n",
            b"  TRY 2 PASS [   0.100s] (1/2) nx flaky\n",
        ])
        self.assertEqual((prog["tests_done"], prog.get("failed")), (1, None))


class PrepareStepTest(unittest.TestCase):
    def test_cargo_waiting_is_a_step_until_the_bar_starts(self):
        prog = {}
        heavy._note_progress(b"    Blocking waiting for file lock on build directory\n", prog)
        self.assertEqual(prog["step"], "Blocking waiting for file lock on build directory")
        heavy._note_progress(b"    Building [> ] 1/315: a\r", prog)
        self.assertNotIn("step", prog)
        heavy._note_progress(b"    Updating something a test printed\n", prog)
        self.assertNotIn("step", prog)


class CpuTest(unittest.TestCase):
    BUSY = [sys.executable, "-c", "import time\nt = time.process_time()\n"
            "while time.process_time() - t < 0.6: pass"]

    def test_the_run_reports_the_cpu_its_tree_used(self):
        job = heavy.make_kill_on_close_job()
        with tempfile.TemporaryDirectory() as d, \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            code, stats = heavy.run_child(self.BUSY, dict(os.environ), job,
                                          progress=Path(d) / "progress" / "1")
        self.assertEqual(code, 0)
        self.assertGreaterEqual(stats["cpu_s"], 0.5)

    def test_prepare_cost_is_taken_at_the_first_sign_of_building(self):
        stats, stop = heavy._new_stats(), threading.Event()
        stats["progress"]["units_total"] = 10
        stop.set()
        with tempfile.TemporaryDirectory() as d:
            heavy._write_progress(Path(d) / "p", stats, stop, cpu=lambda: 4.25)
        self.assertEqual(stats["prep_cpu_s"], 4.2)


class PumpTest(unittest.TestCase):
    def test_bar_is_dropped_for_a_pipe_and_everything_else_is_byte_exact(self):
        parts = [b"   Compiling x\n    Building [=> ] 1/2: x  ", b"\r", b"          \r",
                 b"    Finished\r\n"]
        dst, stats = io.BytesIO(), heavy._new_stats()
        heavy._pump(Chunks(parts), dst, stats, show_bar=False)
        self.assertEqual(dst.getvalue(), b"   Compiling x\n    Finished\r\n")
        self.assertTrue(stats["compiled"])
        self.assertEqual(stats["progress"]["units_done"], 1)

    def test_with_show_bar_the_stream_is_unchanged(self):
        parts = [b"a\r", b"\nb\rc", b""]
        dst = io.BytesIO()
        heavy._pump(Chunks(parts), dst, heavy._new_stats(), show_bar=True)
        self.assertEqual(dst.getvalue(), b"a\r\nb\rc")


if __name__ == "__main__":
    unittest.main()
