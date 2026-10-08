#!/usr/bin/env python3
"""RED tests: `_own_target_dir_config` must walk from the cwd up to (not including) the repo
root. It compared a str root with Path objects, so the walk never ran and a cwd below a crate
that sets its own target-dir was not recognised."""

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).parent
_spec = importlib.util.spec_from_file_location("test_heavy_targetdir", HERE / "test_heavy_targetdir.py")
base = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(base)

CFG = '[build]\ntarget-dir = "target"\n'


def load_heavy():
    s = importlib.util.spec_from_file_location("heavy_mod", base.HEAVY)
    m = importlib.util.module_from_spec(s)
    s.loader.exec_module(m)
    return m


class AncestorTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        tmp = Path(self._tmp.name)
        self.repo = base.make_repo(tmp / "repo", branch="feat/x")
        self.td_root = tmp / "tdroot"
        self.td_root.mkdir()
        saved = dict(os.environ)
        self.addCleanup(lambda: (os.environ.clear(), os.environ.update(saved)))
        for k in ("NUCLEOS_HEAVY_TARGET", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"):
            os.environ.pop(k, None)
        os.environ["NUCLEOS_HEAVY_TD_ROOT"] = str(self.td_root)
        self.heavy = load_heavy()

    def write_cfg(self, d: Path):
        (d / ".cargo").mkdir(parents=True, exist_ok=True)
        (d / ".cargo" / "config.toml").write_text(CFG)

    def test_cwd_below_a_crate_with_own_target_dir_is_recognised(self):
        crate = self.repo / "mycrate"
        self.write_cfg(crate)
        cwd = crate / "src"
        cwd.mkdir()
        argv = ["cargo", "build"]
        self.assertTrue(self.heavy._own_target_dir_config(argv, str(self.repo), str(cwd)))
        self.assertIsNone(self.heavy.injected_target_dir(argv, str(self.repo), str(cwd)))

    def test_config_at_the_repo_root_is_not_honoured(self):
        self.write_cfg(self.repo)
        cwd = self.repo / "somecrate"
        cwd.mkdir()
        self.assertFalse(self.heavy._own_target_dir_config(["cargo", "build"], str(self.repo), str(cwd)))


if __name__ == "__main__":
    unittest.main()
