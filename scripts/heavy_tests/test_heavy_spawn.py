#!/usr/bin/env python3
"""Regression: the broker must spawn a command that only exists as a Windows `.cmd` shim.

`CreateProcess` does not resolve `shim` to `shim.cmd` by itself; `run_child` resolves argv[0]
with `shutil.which` first. Hermetic: the shim lives in a temp dir put first on PATH and the
broker state lives in a temp NUCLEOS_HEAVY_DIR.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HEAVY = ROOT / "scripts" / "heavy.py"


@unittest.skipUnless(os.name == "nt", "the .cmd shim lookup is a Windows concern")
class SpawnTests(unittest.TestCase):
    def test_cmd_shim_is_resolved_and_run(self) -> None:
        tmp = tempfile.mkdtemp(prefix="heavy-spawn-")
        try:
            shim_dir = Path(tmp) / "bin"
            shim_dir.mkdir()
            (shim_dir / "shim.cmd").write_text("@echo shim-ok %*\r\n", encoding="utf-8")
            env = dict(os.environ)
            for k in ("NUCLEOS_HEAVY", "NUCLEOS_HEAVY_TOKEN", "NUCLEOS_HEAVY_HELD",
                      "NUCLEOS_HEAVY_PRIO", "NUCLEOS_HEAVY_MAIN"):
                env.pop(k, None)
            env["NUCLEOS_HEAVY_DIR"] = str(Path(tmp) / "state")
            env["PATH"] = str(shim_dir) + os.pathsep + env.get("PATH", "")
            proc = subprocess.run(
                [sys.executable, str(HEAVY), "--prio", "1", "--agent", "main", "--", "shim", "a", "b"],
                env=env, capture_output=True, text=True, timeout=60, cwd=str(ROOT),
            )
            self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
            self.assertIn("shim-ok a b", proc.stdout)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
