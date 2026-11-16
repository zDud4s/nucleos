#!/usr/bin/env python3
"""Cover `scripts/gate_seed.py`: seeding a cold gate target dir from a warm sibling.

Fake donor trees in a temp dir only; nothing is built.

Run:  python scripts/test-gate-seed.py
"""
import os
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gate_seed  # noqa: E402

OLD = 3600.0  # an hour: past the busy window, inside the staleness one


def touch(path, age=OLD, text="x"):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    t = time.time() - age
    os.utime(path, (t, t))


def make_donor(warm, name, age=OLD):
    target = warm / name / "target"
    d = target / "debug"
    touch(d / ".fingerprint" / "serde-aaaa" / "lib-serde", age)
    touch(d / ".fingerprint" / "nucleos-core-bbbb" / "lib-nucleos-core", age)
    touch(d / "build" / "serde-aaaa" / "output", age, f"cargo:rustc-link-search=native={target}\\debug\\build\\x\\out\n")
    touch(d / "build" / "serde-aaaa" / "root-output", age, f"{str(target).replace(chr(92), '/')}/debug/build/x/out")
    touch(d / "build" / "nucleos-daemon-cccc" / "output", age)
    touch(d / "deps" / "libserde-aaaa.rlib", age, "rlib")
    touch(d / "deps" / "libnucleos_core-dddd.rlib", age)
    touch(d / "deps" / "nucleos_app-eeee.exe", age)
    touch(d / "deps" / "nucleos-core-ffff.exe", age)
    return target


class SeedTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.warm = Path(self._tmp.name) / "warm"
        self.target = self.warm / "me" / "target"

    def tearDown(self):
        self._tmp.cleanup()

    def run_main(self, env=None):
        return gate_seed.main(["gate_seed.py", str(self.target)], env=env if env is not None else {})

    def test_workspace_crates_excluded_registry_copied(self):
        make_donor(self.warm, "donor")
        self.assertEqual(self.run_main(), 0)
        d = self.target / "debug"
        self.assertTrue((d / ".fingerprint" / "serde-aaaa" / "lib-serde").is_file())
        self.assertTrue((d / "build" / "serde-aaaa" / "output").is_file())
        self.assertTrue((d / "deps" / "libserde-aaaa.rlib").is_file())
        self.assertFalse((d / ".fingerprint" / "nucleos-core-bbbb").exists())
        self.assertFalse((d / "build" / "nucleos-daemon-cccc").exists())
        left = sorted(p.name for p in (d / "deps").iterdir())
        self.assertEqual(left, ["libserde-aaaa.rlib"])
        self.assertFalse((self.target / "debug.seeding").exists())
        self.assertFalse((self.warm / "me" / ".target.seed.lock").exists())

    def test_busy_donor_skipped(self):
        make_donor(self.warm, "donor", age=30)
        self.run_main()
        self.assertFalse((self.target / "debug").exists())

    def test_stale_donor_skipped(self):
        make_donor(self.warm, "donor", age=4 * 86400)
        self.run_main()
        self.assertFalse((self.target / "debug").exists())

    def test_warm_target_untouched(self):
        make_donor(self.warm, "donor")
        marker = self.target / "debug" / "mine"
        touch(marker)
        self.run_main()
        self.assertEqual(sorted(p.name for p in (self.target / "debug").iterdir()), ["mine"])

    def test_disable_flag(self):
        make_donor(self.warm, "donor")
        self.run_main({"NUCLEOS_GATE_SEED": "0"})
        self.assertFalse((self.target / "debug").exists())

    def test_paths_rewritten(self):
        donor = make_donor(self.warm, "donor")
        self.run_main()
        out = (self.target / "debug" / "build" / "serde-aaaa" / "output").read_text()
        self.assertNotIn(str(donor), out)
        self.assertIn(str(self.target), out)
        root = (self.target / "debug" / "build" / "serde-aaaa" / "root-output").read_text()
        self.assertNotIn("donor", root)
        # The donor itself is never modified, even through a hardlink.
        self.assertIn(str(donor), (donor / "debug" / "build" / "serde-aaaa" / "output").read_text())

    def test_most_recent_donor_wins(self):
        make_donor(self.warm, "older", age=7200)
        make_donor(self.warm, "newer", age=3600)
        self.assertEqual(gate_seed.find_donor(str(self.target), time.time()), str(self.warm / "newer" / "target"))

    def test_held_lock_skips(self):
        make_donor(self.warm, "donor")
        (self.warm / "me").mkdir(parents=True)
        (self.warm / "me" / ".target.seed.lock").mkdir()
        self.run_main()
        self.assertFalse((self.target / "debug").exists())


class NativePathTests(unittest.TestCase):
    def test_drive_path_converted(self):
        self.assertEqual(gate_seed.native_path("/c/Users/x", windows=True), "C:/Users/x")

    def test_bare_drive_converted(self):
        self.assertEqual(gate_seed.native_path("/c", windows=True), "C:/")

    def test_native_path_unchanged(self):
        self.assertEqual(gate_seed.native_path("C:/a", windows=True), "C:/a")

    def test_multi_letter_segment_unchanged(self):
        self.assertEqual(gate_seed.native_path("/usr/lib", windows=True), "/usr/lib")

    def test_not_windows_unchanged(self):
        self.assertEqual(gate_seed.native_path("/c/Users/x", windows=False), "/c/Users/x")


if __name__ == "__main__":
    unittest.main()
