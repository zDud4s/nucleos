#!/usr/bin/env python3
"""Toolchain marker in the cache key, the per-command cache outcome in log.jsonl, and the report
(`scripts/heavy_cache.py`, `scripts/heavy_cache_report.py`). Reuses the harness of test_heavy_cache.
"""

from __future__ import annotations

import json
import os
import statistics
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
import heavy_cache as hc  # noqa: E402
import heavy_cache_report as rep  # noqa: E402
import test_heavy_cache as base  # noqa: E402


class ToolchainTests(unittest.TestCase):
    setUp = base.CacheTests.setUp
    run_cmd = base.CacheTests.run_cmd
    calls = base.CacheTests.calls
    rows = base.CacheTests.rows
    ARGV = ["cargo", "test", "-p", "x"]

    def cache_rows(self) -> list[dict]:
        return [r for r in self.rows() if "cache" in r]

    def bump(self, path: Path) -> None:
        """Change size and mtime of a fake tool, as an upgrade would."""
        path.write_text(path.read_text(encoding="utf-8") + "\r\nrem upgraded\r\n", encoding="utf-8")
        os.utime(path, ns=(time.time_ns() + 10**9, time.time_ns() + 10**9))

    def exe(self, name: str) -> Path:
        return self.bin / (name + (".cmd" if os.name == "nt" else ""))

    def test_same_toolchain_hits_and_records_saved_seconds(self) -> None:
        self.assertEqual(self.run_cmd(self.ARGV)[3], 1)
        self.assertEqual(self.run_cmd(self.ARGV)[3], 0)
        rows = self.cache_rows()
        self.assertEqual([r["cache"] for r in rows], ["miss_none", "hit"])
        self.assertEqual(rows[0]["cache_store"], "store")
        self.assertIn("cache_key_ms", rows[0])
        self.assertIn("toolchain_fp_ms", rows[0])
        self.assertIn("cache_saved_s", rows[1])
        self.assertAlmostEqual(rows[1]["cache_saved_s"], rows[0]["run_s"], places=2)

    def test_changed_tool_misses_with_miss_toolchain(self) -> None:
        for name in ("cargo", "go"):
            with self.subTest(tool=name):
                argv = [name, "test", "./..."] if name == "go" else self.ARGV
                self.run_cmd(argv)
                self.assertEqual(self.run_cmd(argv)[3], 0)
                self.bump(self.exe(name))
                self.assertEqual(self.run_cmd(argv)[3], 1, "a new toolchain must really run")
                self.assertEqual(self.cache_rows()[-1]["cache"], "miss_toolchain")

    def test_changed_rustc_on_path_misses(self) -> None:
        rustc = self.exe("rustc")
        rustc.write_text("@echo off\r\n" if os.name == "nt" else "#!/bin/sh\n", encoding="utf-8")
        path = str(self.bin) + os.pathsep + os.environ.get("PATH", "")
        self.run_cmd(self.ARGV, PATH=path)
        self.assertEqual(self.run_cmd(self.ARGV, PATH=path)[3], 0)
        self.bump(rustc)
        self.assertEqual(self.run_cmd(self.ARGV, PATH=path)[3], 1)
        self.assertEqual(self.cache_rows()[-1]["cache"], "miss_toolchain")

    def test_toolchain_file_content_changes_the_fingerprint_and_key(self) -> None:
        # The test repos track a rust-toolchain.toml, so an edit also moves the source tree on a
        # real run; here the fingerprint and the key are checked on their own.
        tcf = self.repo / "rust-toolchain.toml"
        argv = [str(self.exe("cargo")), "test"]
        tcf.write_text('[toolchain]\nchannel = "1.80.0"\n', encoding="utf-8")
        p1, _ = hc.toolchain_parts(argv, str(self.repo))
        k1 = hc._key(str(self.repo), argv, "tree", None, p1)
        tcf.write_text('[toolchain]\nchannel = "1.81.0"\n', encoding="utf-8")
        p2, _ = hc.toolchain_parts(argv, str(self.repo))
        self.assertNotEqual(p1["toolchain_file"], p2["toolchain_file"])
        self.assertNotEqual(k1, hc._key(str(self.repo), argv, "tree", None, p2))
        self.assertEqual(hc._rustup_toolchain_name(None, tcf.read_text(), None), "1.81.0")

    def test_source_change_is_miss_src_and_env_change_is_miss_env(self) -> None:
        self.run_cmd(self.ARGV)
        (self.repo / "new.txt").write_text("x", encoding="utf-8")
        self.run_cmd(self.ARGV)
        self.assertEqual(self.cache_rows()[-1]["cache"], "miss_src")
        self.run_cmd(self.ARGV, RUSTFLAGS="-Dwarnings")
        self.assertEqual(self.cache_rows()[-1]["cache"], "miss_env")

    def test_skip_and_uncacheable_outcomes(self) -> None:
        self.run_cmd(self.ARGV)
        self.run_cmd(self.ARGV, NUCLEOS_HEAVY_FORCE="1")
        self.assertEqual(self.cache_rows()[-1]["cache"], "skip_forced")
        self.run_cmd(self.ARGV, NUCLEOS_HEAVY_FULL="1")
        self.assertEqual(self.cache_rows()[-1]["cache"], "skip_full")
        self.run_cmd(["cargo", "build"])
        self.assertEqual(self.cache_rows()[-1]["cache"], "uncacheable")
        self.run_cmd(["cargo", "test", "-p", "y"], exit_code=1, out=base.CARGO_FAIL)
        last = self.cache_rows()[-1]
        self.assertEqual(last["cache_store"], "store_skip_fail")
        self.run_cmd(["cargo", "test", "-p", "y"])
        self.assertEqual(self.cache_rows()[-1]["cache"], "skip_failfirst")

    def test_stat_failure_gives_unknown_and_does_not_crash(self) -> None:
        parts, notes, _ = hc.toolchain_fp(["cargo-does-not-exist", "test"], str(self.repo),
                                          {"PATH": str(self.bin)})
        self.assertEqual(parts["exe"], hc.UNKNOWN)
        self.assertTrue(notes)
        parts2, _, _ = hc.toolchain_fp(["cargo-does-not-exist", "test"], str(self.repo),
                                       {"PATH": str(self.bin)})
        self.assertEqual(parts, parts2, "the marker is stable")
        self.assertEqual(hc._stat_id(str(self.tmp / "nope"), []), hc.UNKNOWN)
        # and the whole command still runs and is logged
        rc, _, err, ran = self.run_cmd(self.ARGV, RUSTC=str(self.tmp / "no-such-rustc"))
        self.assertEqual((rc, ran), (0, 1))
        self.assertIn("unknown", err)

    def test_fingerprint_cost_is_small(self) -> None:
        argv = [str(self.exe("cargo")), "test"]
        times = []
        for _ in range(50):
            hc._TC_MEMO.clear()
            t = time.perf_counter()
            hc.toolchain_fp(argv, str(self.repo))
            times.append((time.perf_counter() - t) * 1000)
        self.assertLess(statistics.mean(times), 5.0, f"mean {statistics.mean(times):.2f} ms")
        print(f"toolchain_fp mean {statistics.mean(times):.3f} ms", file=sys.stderr)


class ReportTests(unittest.TestCase):
    def test_arithmetic_on_a_synthetic_log(self) -> None:
        d = Path(tempfile.mkdtemp(prefix="heavy-rep-"))
        now = time.strftime("%Y-%m-%dT%H:%M:%S%z")
        old = time.strftime("%Y-%m-%dT%H:%M:%S%z", time.localtime(time.time() - 30 * 86400))

        def row(cache, **kw):
            return {"ts": now, "cache": cache, "argv": ["cargo", "test"], "worktree": "/w/a", **kw}
        rows = [
            row("hit", cache_saved_s=100.0, cache_key_ms=60.0, toolchain_fp_ms=2.0),
            row("hit", cache_saved_s=50.0, cache_key_ms=40.0, toolchain_fp_ms=1.0),
            row("miss_none", cache_key_ms=100.0, toolchain_fp_ms=2.0, cache_store="store"),
            row("miss_toolchain", cache_key_ms=100.0, toolchain_fp_ms=5.0),
            row("skip_forced", cache_key_ms=50.0, toolchain_fp_ms=0.0),
            row("uncacheable"),
            {"ts": now, "exit": 0},  # an old-format row
            {**row("hit", cache_saved_s=999.0), "ts": old},  # outside the window
        ]
        (d / "log.jsonl").write_text("\n".join(json.dumps(r) for r in rows) + "\nnot json\n",
                                     encoding="utf-8")
        s = rep.summarize(rep.load(d / "log.jsonl", 7))
        self.assertEqual(s["counts"]["hit"], 2)
        self.assertEqual(s["cacheable"], 5)
        self.assertAlmostEqual(s["hit_rate"], 0.4)
        self.assertAlmostEqual(s["saved_s"], 150.0)
        self.assertAlmostEqual(s["overhead_s"], 0.36)
        self.assertAlmostEqual(s["net_s"], 150.0 - 0.36)
        self.assertEqual(s["stale_prevented"], 1)
        self.assertEqual(s["top"][0][1], 150.0)
        text = rep.render(s, 7)
        self.assertIn("40.0%", text)
        self.assertEqual(rep.main(["--dir", str(d)]), 0)


if __name__ == "__main__":
    unittest.main()
