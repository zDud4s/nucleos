"""cmd_warm must never spawn DETACHED_PROCESS (it makes a console-less child that flashes
windows for every cargo it runs); it must use CREATE_NO_WINDOW. Stdlib only."""
import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HEAVY = Path(__file__).resolve().parents[2] / "scripts" / "heavy.py"
DETACHED_PROCESS = 0x00000008
CREATE_NO_WINDOW = 0x08000000


class _Dummy:
    pid = os.getpid()


class WarmFlagsTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "nt", "creationflags are only set on Windows")
    def test_warm_spawn_never_detached_and_uses_no_window(self) -> None:
        spec = importlib.util.spec_from_file_location("heavy_under_test", HEAVY)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        tmp = Path(tempfile.mkdtemp(prefix="heavy-flags-"))
        repo = tmp / "wt"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        calls: list[int] = []

        def fake_popen(cmd, **kw):
            if "--agent" in cmd and "warm" in cmd:  # the broker spawn, not helper git calls
                calls.append(kw.get("creationflags", 0))
                return _Dummy()
            return real_popen(cmd, **kw)

        saved = {k: os.environ.get(k) for k in ("NUCLEOS_HEAVY", "NUCLEOS_HEAVY_DIR")}
        os.environ.pop("NUCLEOS_HEAVY", None)
        os.environ["NUCLEOS_HEAVY_DIR"] = str(tmp / "state")
        cwd = os.getcwd()
        real_popen = mod.subprocess.Popen
        mod.subprocess.Popen = fake_popen
        try:
            rc = mod.cmd_warm(["--cwd", str(repo), "--", sys.executable, "-c", "pass"])
        finally:
            mod.subprocess.Popen = real_popen
            os.chdir(cwd)
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
        self.assertEqual(rc, 0)
        self.assertTrue(calls, "cmd_warm never spawned")
        for flags in calls:
            self.assertEqual(flags & DETACHED_PROCESS, 0, hex(flags))
            self.assertNotEqual(flags & CREATE_NO_WINDOW, 0, hex(flags))


if __name__ == "__main__":
    unittest.main()
