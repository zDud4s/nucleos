"""One broker per machine: every scripts/heavy.py forwards to master's version, staged under
`<dev home>/run/broker/<stamp>/` from the git objects."""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HEAVY = Path(__file__).resolve().parents[1] / "heavy.py"

# Master's "broker": records how it was run and returns a code the test picks.
FAKE = """import json, os, sys
from pathlib import Path
def main(argv):
    Path(os.environ["FAKE_OUT"]).write_text(json.dumps({"argv": argv, "file": __file__}))
    return int(os.environ.get("FAKE_CODE", "7"))
"""


def git(cwd: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(cwd), *args], capture_output=True, text=True,
                          check=True).stdout


class SingleBrokerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.repo = self.tmp / "repo"
        scripts = self.repo / "scripts"
        scripts.mkdir(parents=True)
        git(self.repo, "init", "-q", "-b", "master")
        git(self.repo, "config", "user.email", "t@t")
        git(self.repo, "config", "user.name", "t")
        self.commit_master("7")
        # The checkout's own heavy.py is the code under test, uncommitted on top of master.
        shutil.copy(HEAVY, scripts / "heavy.py")
        self.dev = self.tmp / "dev"
        self.out = self.tmp / "out.json"

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp, ignore_errors=True)

    def commit_master(self, tag: str) -> None:
        scripts = self.repo / "scripts"
        (scripts / "heavy.py").write_text(FAKE + f"# {tag}\n")
        (scripts / "heavy_cost.py").write_text(f"# cost {tag}\n")
        (scripts / "heavy_classify.py").write_text(f"# classify {tag}\n")
        git(self.repo, "add", "scripts")
        git(self.repo, "commit", "-q", "-m", tag)

    def run_heavy(self, *argv: str, **extra: str) -> subprocess.CompletedProcess:
        env = {k: v for k, v in os.environ.items() if not k.startswith("NUCLEOS_HEAVY")}
        env.update(NUCLEOS_DEV_HOME=str(self.dev), FAKE_OUT=str(self.out),
                   HOME=str(self.tmp), USERPROFILE=str(self.tmp))
        env.update(extra)
        return subprocess.run([sys.executable, str(self.repo / "scripts" / "heavy.py"), *argv],
                              cwd=self.repo, env=env, capture_output=True, text=True,
                              timeout=60)

    def versions(self) -> list[Path]:
        root = self.dev / "run" / "broker"
        return sorted(d for d in root.iterdir() if not d.name.startswith(".")) \
            if root.is_dir() else []

    def test_forwards_to_master_staged_copy(self) -> None:
        r = self.run_heavy("status", "--json")
        self.assertEqual(r.returncode, 7, r.stderr)
        rec = json.loads(self.out.read_text())
        self.assertEqual(rec["argv"], ["status", "--json"])
        [version] = self.versions()
        self.assertEqual(Path(rec["file"]).resolve(), (version / "heavy.py").resolve())
        for name in ("heavy.py", "heavy_cost.py", "heavy_classify.py"):
            self.assertTrue((version / name).is_file(), name)

    def test_land_on_master_refreshes_the_copy(self) -> None:
        self.assertEqual(self.run_heavy("status").returncode, 7)
        self.commit_master("8")
        shutil.copy(HEAVY, self.repo / "scripts" / "heavy.py")
        r = self.run_heavy("status", FAKE_CODE="9")
        self.assertEqual(r.returncode, 9, r.stderr)
        self.assertEqual(len(self.versions()), 2)
        ran = Path(json.loads(self.out.read_text())["file"])
        self.assertIn("# 8", ran.read_text())

    def test_same_master_reuses_the_copy(self) -> None:
        self.run_heavy("status")
        self.run_heavy("status")
        self.assertEqual(len(self.versions()), 1)

    def test_real_master_broker_is_staged_whole_and_runs(self) -> None:
        scripts = self.repo / "scripts"
        names = sorted(p.name for p in HEAVY.parent.glob("heavy*.py"))
        for name in names:
            shutil.copy(HEAVY.parent / name, scripts / name)
        git(self.repo, "add", "scripts")
        git(self.repo, "commit", "-q", "-m", "real")
        r = self.run_heavy("status", "--json")
        self.assertEqual(r.returncode, 0, r.stderr)
        json.loads(r.stdout)
        [version] = self.versions()
        self.assertEqual(sorted(p.name for p in version.glob("*.py")), names)

    def test_broken_master_falls_back_to_this_file(self) -> None:
        (self.repo / "scripts" / "heavy.py").write_text("raise ImportError('boom')\n")
        git(self.repo, "commit", "-q", "-am", "broken")
        shutil.copy(HEAVY, self.repo / "scripts" / "heavy.py")
        r = self.run_heavy("status", "--json")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("failed to load", r.stderr)
        json.loads(r.stdout)

    def test_local_switch_runs_this_file(self) -> None:
        r = self.run_heavy("status", NUCLEOS_HEAVY_LOCAL="1")
        self.assertNotEqual(r.returncode, 7)
        self.assertFalse(self.out.exists())
        self.assertEqual(self.versions(), [])

    def test_isolated_state_dir_runs_this_file(self) -> None:
        r = self.run_heavy("status", NUCLEOS_HEAVY_DIR=str(self.tmp / "state"))
        self.assertNotEqual(r.returncode, 7)
        self.assertFalse(self.out.exists())

    def test_unknown_master_runs_this_file(self) -> None:
        r = self.run_heavy("status", NUCLEOS_HEAVY_MASTER_REF="no-such-ref")
        self.assertNotEqual(r.returncode, 7)
        self.assertFalse(self.out.exists())


if __name__ == "__main__":
    unittest.main()
