#!/usr/bin/env python3
"""RED tests for two heavy broker follow-ups (`scripts/heavy.py`).

F4: a fingerprint HIT must still take (wait for) the worktree lock; a bare `cargo test` at
the worktree root whose fingerprint is registered used to run with neither token nor lock.
F7: `heavy.py status` must report the CURRENT lock holder's age (when it acquired the lock),
not the lock file's mtime.

Hermetic, same fixture style as test_heavy_fingerprint.py: temp NUCLEOS_HEAVY_DIR, a throwaway
git repo shaped like this project, a fake `cargo`. Nothing real is compiled.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_heavy_fingerprint as _fp  # noqa: E402

wait_for = _fp.wait_for

HEAVY = Path(__file__).resolve().parents[2] / "scripts" / "heavy.py"


class FollowupTests(_fp.FingerprintTests):
    # Inherit only the fixture (setUp/helpers); the inherited tests must not rerun here.
    def _only_fixture(self) -> None:  # pragma: no cover
        pass

    def broker_cargo(self, cargo_args: list[str], cwd: Path, **env_kw) -> subprocess.Popen:
        p = subprocess.Popen(
            [sys.executable, str(HEAVY), "--kind", "cargo", "--wait-max", "60", "--",
             str(self.cargo), *cargo_args],
            env=self.env(**env_kw), cwd=str(cwd),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.procs.append(p)
        return p

    def test_hit_waits_for_worktree_lock(self) -> None:
        bare = ["test"]
        # Register the bare fingerprint, then prove the next identical run is a hit.
        rc, _ = self.run_broker(bare)
        self.assertEqual(rc, 0)
        rc, row = self.run_broker(bare)
        self.assertEqual(rc, 0)
        self.assertTrue(self.is_hit(row), f"precondition: bare repeat must be a hit: {row}")

        started, stop = self.marks / "holder.start", self.marks / "holder.stop"
        holder = self.broker_cargo(["check", "-p", "nucleos-core"], self.repo,
                                   target_dir=self.tmp / "target-holder",
                                   FAKE_START_FILE=str(started), FAKE_STOP_FILE=str(stop))
        self.assertTrue(wait_for(started.exists), "lock holder never started")
        hit_start = self.marks / "hit.start"
        waiter = self.popen(bare, self.repo, FAKE_START_FILE=str(hit_start))
        # Give the waiter ample time to either start (the bug) or queue behind the lock.
        wait_for(hit_start.exists, timeout=12.0)
        self.assertFalse(hit_start.exists(),
                         "a fingerprint hit must not start its child while the worktree "
                         "lock is held by another cargo command")
        stop.write_text("x")
        self.assertEqual(holder.wait(timeout=30), 0)
        self.assertEqual(waiter.wait(timeout=30), 0)
        self.assertTrue(hit_start.exists(), "the hit must run once the lock is released")

    def test_status_age_is_current_holders(self) -> None:
        started, stop = self.marks / "h.start", self.marks / "h.stop"
        holder = self.broker_cargo(["check", "-p", "nucleos-core"], self.repo,
                                   FAKE_START_FILE=str(started), FAKE_STOP_FILE=str(stop))
        self.assertTrue(wait_for(started.exists), "lock holder never started")
        locks = list((self.state / "wt").glob("*.lock"))
        self.assertEqual(len(locks), 1, locks)
        old = time.time() - 3600
        os.utime(locks[0], (old, old))  # the previous holder's mtime; ours acquired just now
        cp = subprocess.run([sys.executable, str(HEAVY), "status"], env=self.env(),
                            capture_output=True, text=True, timeout=60)
        stop.write_text("x")
        holder.wait(timeout=30)
        line = next((l for l in cp.stdout.splitlines() if "lock=" in l), None)
        self.assertIsNotNone(line, cp.stdout)
        m = re.search(r"age=(\d+)s", line)
        self.assertIsNotNone(m, line)
        self.assertLess(int(m.group(1)), 600,
                        f"status shows the file's mtime age, not the holder's: {line}")

    def test_status_lock_line_separates_age_and_waiters(self) -> None:
        started, stop = self.marks / "s.start", self.marks / "s.stop"
        holder = self.broker_cargo(["check", "-p", "nucleos-core"], self.repo,
                                   FAKE_START_FILE=str(started), FAKE_STOP_FILE=str(stop))
        self.assertTrue(wait_for(started.exists), "lock holder never started")
        cp = subprocess.run([sys.executable, str(HEAVY), "status"], env=self.env(),
                            capture_output=True, text=True, timeout=60)
        stop.write_text("x")
        holder.wait(timeout=30)
        line = next((l for l in cp.stdout.splitlines() if "lock=" in l), None)
        self.assertIsNotNone(line, cp.stdout)
        self.assertRegex(line, r"age=\d+s waiters=\d+")


# Drop the inherited tests so only those above run.
for _n in [n for n in dir(_fp.FingerprintTests) if n.startswith("test_")]:
    setattr(FollowupTests, _n, None)

if __name__ == "__main__":
    unittest.main()
