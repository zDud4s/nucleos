#!/usr/bin/env python3
"""RED unit tests for the light lane's cost-model half (plan 2026-10-21-broker-light-lane):
build_light / load_light / rebuild_light / light_entry / light_fits / light_enabled /
light_max / light_margin_mb and the LIGHT_* constants of `scripts/heavy_cost.py`.

Pure functions only; the broker itself is covered by test_heavy_light_broker.py. Until the new
symbols exist every test here fails on an AttributeError (or on an assertion).

    python scripts/heavy_tests/test_heavy_light.py -v
"""

from __future__ import annotations

import json
import os
import shutil
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

SCRIPTS = Path(__file__).resolve().parents[1]
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))

import heavy_cost as hc  # noqa: E402

CLS = "x|y|-|a|c"


def ts_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%S%z")


def row(cls: str = CLS, mem=100.0, run=10.0, cores=1.0, **extra) -> dict:
    r = {"v": 1, "ts": ts_now(), "cls": cls, "peak_mem_mb": mem, "run_s": run,
         "cores_avg": cores, "cores_p90": 99}
    r.update(extra)
    return r


def clean_env(**extra: str) -> dict:
    env = {k: v for k, v in os.environ.items() if not k.startswith("NUCLEOS_HEAVY")}
    env.update(extra)
    return env


class Build(unittest.TestCase):
    def test_build_light_takes_last_twenty_all_durations_complete_rows_only(self) -> None:
        # Last twenty rows of the class, in log order: the five heavy ones at the head are out.
        rows = [row(mem=5000.0, run=10.0, cores=1.0) for _ in range(5)]
        rows += [row(mem=10.0 * i, run=float(i), cores=i / 10.0) for i in range(1, 21)]
        out = hc.build_light(rows)
        self.assertEqual(set(out), {"v", "built", "classes"})
        self.assertEqual(out["v"], 1)
        e = out["classes"][CLS]
        self.assertEqual(e["n"], 20)
        # p90 of 1..20 is the 18th value; cores come from cores_avg, never cores_p90 (99 here).
        self.assertAlmostEqual(e["mem_mb"], 180.0)
        self.assertAlmostEqual(e["run_s"], 18.0)
        self.assertAlmostEqual(e["cores"], 1.8)
        self.assertIs(e["light"], True)

        # Every duration counts: build_costs would drop these (< 20 s), the light model keeps them.
        out = hc.build_light([row(run=1.0) for _ in range(5)])
        self.assertEqual(out["classes"][CLS]["n"], 5)
        self.assertIs(out["classes"][CLS]["light"], True)

        # Rows missing a number (or carrying a non-number) are skipped, not counted as zero.
        good = [row() for _ in range(5)]
        bad = [row(mem=None), row(cores=None), row(run=None), row(mem="1"), row(cores=True),
               {"v": 1, "ts": ts_now(), "cls": CLS, "run_s": 5.0}]
        out = hc.build_light(good[:2] + bad + good[2:])
        self.assertEqual(out["classes"][CLS]["n"], 5)
        self.assertIs(out["classes"][CLS]["light"], True)

        # One class's rows never leak into another's.
        out = hc.build_light([row(cls="a|b|-|a|c") for _ in range(5)] + [row() for _ in range(5)])
        self.assertEqual(out["classes"]["a|b|-|a|c"]["n"], 5)
        self.assertEqual(out["classes"][CLS]["n"], 5)

    def test_light_thresholds_and_min_n(self) -> None:
        self.assertEqual(hc.LIGHT_MEM_MB, 1536)
        self.assertEqual(hc.LIGHT_RUN_S, 180)
        self.assertEqual(hc.LIGHT_CORES, 3)
        self.assertEqual(hc.LIGHT_MIN_N, 5)

        def flag(n: int, **kw) -> bool:
            classes = hc.build_light([row(**kw) for _ in range(n)])["classes"]
            return classes.get(CLS, {}).get("light") is True

        cases = [
            ("mem at the limit", dict(mem=1536.0), True),
            ("mem over the limit", dict(mem=1537.0), False),
            ("run at the limit", dict(run=180.0), True),
            ("run over the limit", dict(run=181.0), False),
            ("cores at the limit", dict(cores=3.0), True),
            ("cores over the limit", dict(cores=3.01), False),
        ]
        for name, kw, want in cases:
            with self.subTest(name):
                self.assertIs(flag(5, **kw), want)
        with self.subTest("n=4"):
            self.assertIs(flag(4), False)
        with self.subTest("n=5"):
            self.assertIs(flag(5), True)
        with self.subTest("p90 not max: one slow outlier in twenty is tolerated"):
            rows = [row() for _ in range(19)] + [row(run=999.0)]
            self.assertIs(hc.build_light(rows)["classes"][CLS]["light"], True)

    def test_costs_json_format_unchanged(self) -> None:
        rows = [row(run=30.0, cores_p90=3.0) for _ in range(3)]
        # The light model carries a flag and run_s the cost model does not ...
        self.assertIn("light", hc.build_light(rows * 2)["classes"][CLS])
        # ... and build_costs keeps exactly its old shape.
        costs = hc.build_costs(rows)
        self.assertEqual(set(costs), {"v", "built", "classes"})
        self.assertEqual(set(costs["classes"][CLS]), {"cores", "mem_mb", "n"})
        self.assertEqual(costs["classes"][CLS]["n"], 3)


class Entry(unittest.TestCase):
    @staticmethod
    def light_of(**stats) -> dict:
        e = {"n": 10, "mem_mb": 500.0, "run_s": 20.0, "cores": 1.0, "light": True}
        e.update(stats)
        return {"v": 1, "classes": {CLS: e}}

    def test_light_entry_rederives_and_does_not_trust_the_flag(self) -> None:
        got = hc.light_entry(self.light_of(), CLS)
        self.assertIsInstance(got, dict)
        self.assertEqual(got["mem_mb"], 500.0)
        for name, stats in (
            ("mem over", dict(mem_mb=4000.0)),
            ("run over", dict(run_s=600.0)),
            ("cores over", dict(cores=8.0)),
            ("n too small", dict(n=3)),
        ):
            with self.subTest(name):
                self.assertIsNone(hc.light_entry(self.light_of(**stats), CLS))
        with self.subTest("unknown class"):
            self.assertIsNone(hc.light_entry(self.light_of(), "other|-|-|a|c"))
        for junk in ({}, None, {"classes": []}, {"classes": {CLS: "light"}},
                     {"classes": {CLS: {"light": True}}}):
            with self.subTest(junk=junk):
                self.assertIsNone(hc.light_entry(junk, CLS))

    def test_light_fits_margin_busy_and_env(self) -> None:
        entry = {"n": 10, "mem_mb": 500.0, "run_s": 20.0, "cores": 1.0, "light": True}

        def probe(avail=8000.0, busy=2.0, logical=12) -> dict:
            return {"logical": logical, "total_mb": 16000, "avail_mb": avail, "busy_cores": busy}

        with mock.patch.dict(os.environ, clean_env(), clear=True):
            self.assertEqual(hc.light_margin_mb(), 1024)
            with self.subTest("memory: default margin, boundary"):
                self.assertIs(hc.light_fits(entry, probe(avail=1524.0))[0], True)
                ok, detail = hc.light_fits(entry, probe(avail=1523.0))
                self.assertIs(ok, False)
                self.assertTrue(detail)
            with self.subTest("memory: explicit margin"):
                self.assertIs(hc.light_fits(entry, probe(avail=1100.0), margin=512)[0], True)
                self.assertIs(hc.light_fits(entry, probe(avail=1000.0), margin=512)[0], False)
            with self.subTest("busy unknown is ignored"):
                self.assertIs(hc.light_fits(entry, probe(busy=None, logical=2))[0], True)
            with self.subTest("busy + cores <= logical - 1"):
                self.assertIs(hc.light_fits(entry, probe(busy=10.0, logical=12))[0], True)
                self.assertIs(hc.light_fits(entry, probe(busy=10.5, logical=12))[0], False)
        with self.subTest("env margin override"):
            with mock.patch.dict(os.environ, clean_env(NUCLEOS_HEAVY_LIGHT_MARGIN_MB="2048"),
                                 clear=True):
                self.assertEqual(hc.light_margin_mb(), 2048)
                self.assertIs(hc.light_fits(entry, probe(avail=2548.0))[0], True)
                self.assertIs(hc.light_fits(entry, probe(avail=2547.0))[0], False)

    def test_light_env_switches(self) -> None:
        for raw, want in ((None, True), ("1", True), ("", True), ("0", False)):
            with self.subTest(light=raw):
                extra = {} if raw is None else {"NUCLEOS_HEAVY_LIGHT": raw}
                with mock.patch.dict(os.environ, clean_env(**extra), clear=True):
                    self.assertIs(hc.light_enabled(), want)
        for raw, want in ((None, 2), ("3", 3), ("abc", 2), ("", 2), ("0", 0), ("-1", 0)):
            with self.subTest(light_max=raw):
                extra = {} if raw is None else {"NUCLEOS_HEAVY_LIGHT_MAX": raw}
                with mock.patch.dict(os.environ, clean_env(**extra), clear=True):
                    self.assertEqual(hc.light_max(), want)
        for raw, want in ((None, 1024), ("2048", 2048), ("junk", 1024)):
            with self.subTest(margin=raw):
                extra = {} if raw is None else {"NUCLEOS_HEAVY_LIGHT_MARGIN_MB": raw}
                with mock.patch.dict(os.environ, clean_env(**extra), clear=True):
                    self.assertEqual(hc.light_margin_mb(), want)


class Rebuild(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="heavy-light-")
        self.d = Path(self.tmp)
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def write_log(self, rows: list[dict]) -> None:
        (self.d / "log.jsonl").write_text(
            "".join(json.dumps(r) + "\n" for r in rows), encoding="utf-8")

    def test_rebuild_light_age_gate_and_fail_safe(self) -> None:
        self.write_log([row() for _ in range(5)])
        hc.rebuild_light(self.d)
        light = json.loads((self.d / "light.json").read_text(encoding="utf-8"))
        self.assertIs(light["classes"][CLS]["light"], True)
        self.assertEqual(light["classes"][CLS]["n"], 5)
        self.assertFalse((self.d / "costs.json").exists())
        self.assertEqual(hc.load_light(self.d)["classes"][CLS]["n"], 5)

        # A fresh file is not rebuilt inside max_age_s ...
        sentinel = {"v": 1, "built": "x", "classes": {"sentinel": {}}}
        (self.d / "light.json").write_text(json.dumps(sentinel), encoding="utf-8")
        hc.rebuild_light(self.d)
        self.assertEqual(json.loads((self.d / "light.json").read_text(encoding="utf-8")), sentinel)
        # ... and a stale one is.
        old = time.time() - 3600
        os.utime(self.d / "light.json", (old, old))
        hc.rebuild_light(self.d)
        self.assertIn(CLS, json.loads((self.d / "light.json").read_text(encoding="utf-8"))["classes"])
        self.assertFalse((self.d / "light.json.tmp").exists())
        self.assertFalse((self.d / "costs.json").exists())

        # Missing or garbage input never raises, and writes nothing it cannot stand behind.
        empty = self.d / "empty"
        empty.mkdir()
        hc.rebuild_light(empty)
        self.assertFalse((empty / "costs.json").exists())
        hc.rebuild_light(self.d / "no-such-dir")
        junk = self.d / "junk"
        junk.mkdir()
        (junk / "log.jsonl").write_bytes(b"\xff\xfe not json\n{\"cls\": 1}\n[1, 2]\n\x00\x00\n")
        hc.rebuild_light(junk)
        self.assertEqual(hc.load_light(self.d / "nothing-here"), {})
        (junk / "light.json").write_text("not json{", encoding="utf-8")
        self.assertEqual(hc.load_light(junk), {})


if __name__ == "__main__":
    unittest.main()
