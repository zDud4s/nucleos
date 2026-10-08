#!/usr/bin/env python3
"""RED tests for the heavy broker's FIXED POOL of cargo target dirs (`scripts/heavy.py`).

2026-10-04: one `.cargo-target-<branch slug>` per branch grew without bound and filled the
disk. The broker now leases `<td_root>/.cargo-target-pool-<k>`, k = 1..N with
N = NUCLEOS_HEAVY_TARGET_SLOTS (default 3), to every eligible cargo child:

- exclusive: no two running broker children share a pool dir; with one slot, a second
  eligible build waits for the first to finish;
- affine: a worktree gets the slot it used last when that slot is free, and other worktrees
  prefer their own / an unused slot over it;
- owner change: before a slot last used by worktree A runs worktree B, the broker runs
  `cargo clean -p <member>` on it for the workspace members; if that clean fails, the slot dir
  is removed instead;
- a lease whose holder process is gone does not block;
- `hold-worktree` leases NO slot and exports no target dir; nested broker calls lease their own,
  and the first such lease stays pinned to the session until it ends.

Eligibility itself is pinned in test_heavy_targetdir*.py. Fixtures (fake cargo, throwaway
repos, hermetic state) come from test_heavy_targetdir.py. Nothing real is compiled.
"""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import time
import unittest
from pathlib import Path

HERE = Path(__file__).parent
_spec = importlib.util.spec_from_file_location("test_heavy_targetdir", HERE / "test_heavy_targetdir.py")
base = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(base)
HEAVY = base.HEAVY
real = base.real

POLL_S = 0.1
LONG = 120.0  # generous: the machine running these is loaded

# A python child for `hold-worktree`: records the CARGO_TARGET_DIR it was given, then runs the
# broker for two nested cargo builds (bounded by --wait-max so a self-deadlock times out): a plain
# one, and one aimed at a src-tauri manifest (which owns its own target dir).
HELD = r'''
import json, os, pathlib, subprocess, sys

heavy, cargo, out, tauri = sys.argv[1:5]
pathlib.Path(out).write_text(json.dumps({"td": os.environ.get("CARGO_TARGET_DIR"),
                                         "btd": os.environ.get("CARGO_BUILD_TARGET_DIR")}))
rc = subprocess.run([sys.executable, heavy, "--wait-max", "45", "--", cargo, "test", "-p",
                     "nucleos-core"]).returncode
rc2 = subprocess.run([sys.executable, heavy, "--wait-max", "45", "--", cargo, "test",
                      "--manifest-path", tauri]).returncode
sys.exit(rc or rc2)
'''

# A hold-worktree child for the session pin: one nested build, then it waits on the gate dir
# (the slot leased by that build must stay pinned to the session meanwhile), then a second one.
PINNED = r'''
import pathlib, subprocess, sys, time

heavy, cargo, gate = sys.argv[1:4]
def build():
    return subprocess.run([sys.executable, heavy, "--wait-max", "45", "--", cargo, "test", "-p",
                           "nucleos-core"]).returncode
rc = build()
pathlib.Path(gate, "between").write_text("")
deadline = time.monotonic() + 120
while not pathlib.Path(gate, "resume").exists() and time.monotonic() < deadline:
    time.sleep(0.1)
sys.exit(rc or build())
'''


class PoolTests(base.TargetDirTests):
    """Reuses the parent's hermetic setUp/helpers; its own tests are blanked below."""

    def setUp(self) -> None:
        super().setUp()
        self.calls_dir = self.tmp / "calls"
        self.calls_dir.mkdir()
        self.gate = self.tmp / "gate"
        self.gate.mkdir()
        self.repo_b = base.make_repo(self.tmp / "wt-b", branch="feat/other")
        (self.tmp / "held.py").write_text(HELD, encoding="utf-8")
        (self.tmp / "pinned.py").write_text(PINNED, encoding="utf-8")
        self.procs: list[subprocess.Popen] = []
        # Registered after the parent's rmtree, so it runs BEFORE it (cleanups are LIFO).
        self.addCleanup(self._stop_all)

    def _stop_all(self) -> None:
        try:
            (self.gate / "release-all").write_text("")
        except OSError:
            pass
        for p in self.procs:
            try:
                p.wait(timeout=60)
            except subprocess.TimeoutExpired:
                p.kill()
                p.wait(timeout=30)

    # ---- helpers -------------------------------------------------------------------
    def pool_env(self, tag: str, slots: int, gated: bool, **extra) -> dict:
        env = self.env(FAKE_LOG=str(self.calls_dir), FAKE_TAG=tag,
                       NUCLEOS_HEAVY_TARGET_SLOTS=str(slots), **extra)
        env.pop("FAKE_OUT", None)
        if gated:
            env["FAKE_GATE"] = str(self.gate)
        return env

    def spawn(self, tag: str, repo: Path, slots: int, gated: bool = True,
              wait_max: int = 150, **extra) -> subprocess.Popen:
        err = open(self.tmp / f"stderr-{tag}.txt", "w", encoding="utf-8")
        self.addCleanup(err.close)
        p = subprocess.Popen(
            [sys.executable, str(HEAVY), "--wait-max", str(wait_max), "--", str(self.cargo),
             "test", "-p", "nucleos-core"],
            env=self.pool_env(tag, slots, gated, **extra), cwd=str(repo),
            stdout=subprocess.DEVNULL, stderr=err)
        self.procs.append(p)
        return p

    def stderr_of(self, tag: str) -> str:
        p = self.tmp / f"stderr-{tag}.txt"
        return p.read_text(encoding="utf-8", errors="replace") if p.exists() else ""

    def run_seq(self, tag: str, repo: Path, slots: int, **extra) -> str:
        """One ungated eligible build to completion; returns the td its build saw."""
        p = self.spawn(tag, repo, slots, gated=False, **extra)
        rc = p.wait(timeout=LONG + 60)
        self.assertEqual(rc, 0, f"{tag}: broker failed: {self.stderr_of(tag)!r}")
        builds = [c for c in self.calls() if c["tag"] == tag and self.is_build(c)]
        self.assertTrue(builds, f"{tag}: the build never ran")
        return builds[-1]["td"]

    def wait_for(self, pred, what: str, timeout: float = LONG):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            got = pred()
            if got:
                return got
            time.sleep(POLL_S)
        self.fail(f"timed out after {timeout:.0f}s waiting for {what}")

    def started(self, tag: str):
        f = self.gate / f"started-{tag}"
        if not f.exists():
            return None
        try:
            return json.loads(f.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return None  # caught mid-write; poll again

    def release(self, tag: str) -> None:
        (self.gate / f"release-{tag}").write_text("")

    def calls(self) -> list[dict]:
        out = []
        for f in self.calls_dir.glob("*.json"):
            try:
                out.append(json.loads(f.read_text(encoding="utf-8")))
            except (OSError, ValueError):
                pass
        return sorted(out, key=lambda c: c["t"])

    @staticmethod
    def is_build(c: dict) -> bool:
        return bool(c["argv"]) and c["argv"][0] not in ("metadata", "clean")

    @staticmethod
    def is_member_clean(c: dict) -> bool:
        return bool(c["argv"]) and c["argv"][0] == "clean" and "-p" in c["argv"]

    def slot(self, k: int) -> Path:
        return self.td_root / f".cargo-target-pool-{k}"

    # 3 --------------------------------------------------------------------------------
    def test_concurrent_children_get_distinct_slots(self) -> None:
        a = self.spawn("A", self.repo, slots=3)
        b = self.spawn("B", self.repo_b, slots=3)
        sa = self.wait_for(lambda: self.started("A"), "A's build to start")
        sb = self.wait_for(lambda: self.started("B"), "B's build to start (both running at once)")
        # Both are running right now: neither has been released.
        self.assertFalse((self.gate / "ended-A").exists() or (self.gate / "ended-B").exists())
        ka = self.pool_index(sa["td"])
        kb = self.pool_index(sb["td"])
        self.assertNotEqual(ka, kb, f"two running children share a pool dir: {sa} {sb}")
        self.release("A")
        self.release("B")
        self.assertEqual(a.wait(timeout=LONG), 0, self.stderr_of("A"))
        self.assertEqual(b.wait(timeout=LONG), 0, self.stderr_of("B"))

    def test_single_slot_makes_a_second_worktree_wait(self) -> None:
        a = self.spawn("A", self.repo, slots=1)
        sa = self.wait_for(lambda: self.started("A"), "A's build to start")
        self.assertEqual(self.pool_index(sa["td"], slots=1), 1, sa)
        b = self.spawn("B", self.repo_b, slots=1)
        # Capacity (4) admits both cargo builds (weight 2 each) and they are different
        # worktrees, so only the single pool slot can hold B back. Watch for a bounded window:
        # B must not start while A still holds the slot.
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline and b.poll() is None:
            self.assertIsNone(self.started("B"),
                              "B started while A held the only pool slot")
            time.sleep(POLL_S)
        self.assertIsNone(b.poll(), f"B gave up instead of waiting: {self.stderr_of('B')!r}")
        self.release("A")
        sb = self.wait_for(lambda: self.started("B"), "B's build to start once A released")
        self.assertTrue((self.gate / "ended-A").exists(),
                        "B started before A's build finished")
        self.assertEqual(self.pool_index(sb["td"], slots=1), 1, sb)
        self.release("B")
        self.assertEqual(a.wait(timeout=LONG), 0, self.stderr_of("A"))
        self.assertEqual(b.wait(timeout=LONG), 0, self.stderr_of("B"))

    # 4 --------------------------------------------------------------------------------
    def test_a_worktree_keeps_its_slot_and_others_avoid_it(self) -> None:
        ka = self.pool_index(self.run_seq("A1", self.repo, slots=3))
        self.assertEqual(self.pool_index(self.run_seq("A2", self.repo, slots=3)), ka,
                         "the same worktree must get its last slot back")
        kb = self.pool_index(self.run_seq("B1", self.repo_b, slots=3))
        self.assertNotEqual(kb, ka, "another worktree took A's slot while an unused one was free")
        self.assertEqual(self.pool_index(self.run_seq("B2", self.repo_b, slots=3)), kb)
        self.assertEqual(self.pool_index(self.run_seq("A3", self.repo, slots=3)), ka,
                         "A must get its slot back after B ran elsewhere")
        a_slot = real(self.slot(ka))
        cleans = [c for c in self.calls() if c["argv"] and c["argv"][0] == "clean"
                  and c["td"] and real(c["td"]) == a_slot]
        self.assertEqual(cleans, [], "a worktree reusing its own slot must not clean it")

    # 5 --------------------------------------------------------------------------------
    def test_owner_change_cleans_workspace_members_first(self) -> None:
        td_a = self.run_seq("A", self.repo, slots=1)
        self.assertEqual(self.pool_index(td_a, slots=1), 1)
        self.assertEqual([c for c in self.calls() if c["argv"][:1] == ["clean"]], [],
                         "the first owner of an unused slot has nothing to clean")
        td_b = self.run_seq("B", self.repo_b, slots=1)
        self.assertEqual(real(td_b), real(self.slot(1)))
        calls = self.calls()
        b_build = next(c for c in calls if c["tag"] == "B" and self.is_build(c))
        cleans = [c for c in calls if self.is_member_clean(c)]
        self.assertTrue(cleans, f"no `cargo clean -p` before B used A's slot: {calls}")
        before = [c for c in cleans if c["t"] < b_build["t"]
                  and c["td"] and real(c["td"]) == real(self.slot(1))]
        self.assertTrue(before, f"the clean must target the slot and precede B's build: {cleans}")
        self.assertIn("nucleos-core", before[-1]["argv"], before[-1])
        # B again on its own slot: no further clean.
        n = len([c for c in self.calls() if c["argv"][:1] == ["clean"]])
        self.run_seq("B2", self.repo_b, slots=1)
        self.assertEqual(len([c for c in self.calls() if c["argv"][:1] == ["clean"]]), n,
                         "B reusing the slot it now owns must not clean it")

    def test_failed_clean_removes_the_slot_before_the_new_owner_builds(self) -> None:
        td_a = self.run_seq("A", self.repo, slots=1)
        self.assertEqual(self.pool_index(td_a, slots=1), 1)
        slot = self.slot(1)
        slot.mkdir(parents=True, exist_ok=True)
        marker = slot / "stale-marker"
        marker.write_text("from A")
        td_b = self.run_seq("B", self.repo_b, slots=1, FAKE_CLEAN_EXIT="3",
                            FAKE_PROBE=str(marker))
        self.assertEqual(real(td_b), real(slot))
        calls = self.calls()
        self.assertTrue([c for c in calls if self.is_member_clean(c)],
                        "the broker must try `cargo clean -p` before removing the slot")
        b_build = next(c for c in calls if c["tag"] == "B" and self.is_build(c))
        self.assertFalse(b_build.get("probe"),
                         "B's build saw A's leftovers: a failed clean must remove the slot dir")

    # 6 --------------------------------------------------------------------------------
    def test_dead_holder_lease_is_reclaimed(self) -> None:
        a = self.spawn("A", self.repo, slots=1)
        sa = self.wait_for(lambda: self.started("A"), "A's build to start")
        self.assertEqual(self.pool_index(sa["td"], slots=1), 1, sa)
        a.kill()  # the lease holder dies without releasing
        a.wait(timeout=60)
        self.release("A")  # let an orphaned fake child (POSIX) exit too
        p = self.spawn("B", self.repo_b, slots=1, gated=False, wait_max=60)
        rc = p.wait(timeout=LONG + 60)
        self.assertEqual(rc, 0, f"B was blocked by a dead holder's lease: {self.stderr_of('B')!r}")
        build = [c for c in self.calls() if c["tag"] == "B" and self.is_build(c)]
        self.assertTrue(build, "B's build never ran")
        self.assertEqual(real(build[-1]["td"]), real(self.slot(1)), build[-1])

    # 7 --------------------------------------------------------------------------------
    def test_hold_worktree_leases_no_slot_and_nested_calls_lease_their_own(self) -> None:
        held_out = self.tmp / "held.json"
        tauri = self.tmp / "wt-a" / "shell" / "src-tauri" / "Cargo.toml"
        env = self.pool_env("H", slots=1, gated=False)
        cp = subprocess.run(
            [sys.executable, str(HEAVY), "hold-worktree", "--", sys.executable,
             str(self.tmp / "held.py"), str(HEAVY), str(self.cargo), str(held_out), str(tauri)],
            env=env, cwd=str(self.repo), capture_output=True, text=True, timeout=LONG + 60)
        self.assertTrue(held_out.exists(), f"the held child never ran: {cp.stderr!r}")
        child = json.loads(held_out.read_text(encoding="utf-8"))
        self.assertIsNone(child.get("td"), f"hold-worktree must not export a target dir: {child}")
        self.assertIsNone(child.get("btd"), child)
        self.assertEqual(cp.returncode, 0, cp.stderr)
        builds = [c for c in self.calls() if self.is_build(c)]
        plain = [c for c in builds if "--manifest-path" not in c["argv"]]
        owned = [c for c in builds if "--manifest-path" in c["argv"]]
        self.assertTrue(plain, "the nested plain cargo never ran")
        self.assertEqual(self.pool_index(plain[-1]["td"], slots=1), 1, plain[-1])
        self.assertTrue(owned, "the nested src-tauri cargo never ran")
        self.assertIsNone(owned[-1]["td"],
                          f"a crate with its own target dir must get no CARGO_TARGET_DIR: {owned[-1]}")

    # 8 --------------------------------------------------------------------------------
    def test_owner_change_clean_uses_the_build_profile(self) -> None:
        self.run_seq("A", self.repo, slots=1)
        err = open(self.tmp / "stderr-B.txt", "w", encoding="utf-8")
        self.addCleanup(err.close)
        cp = subprocess.run(
            [sys.executable, str(HEAVY), "--wait-max", "60", "--", str(self.cargo), "build",
             "--release", "-p", "nucleos-core"],
            env=self.pool_env("B", 1, False), cwd=str(self.repo_b), stdout=subprocess.DEVNULL,
            stderr=err, timeout=LONG + 60)
        self.assertEqual(cp.returncode, 0)
        cleans = [c for c in self.calls() if self.is_member_clean(c) and c["tag"] == "B"]
        self.assertTrue(cleans, "no member clean before B used A's slot")
        self.assertIn("--release", cleans[-1]["argv"],
                      f"the clean must target the build's profile: {cleans[-1]}")

    # 9 --------------------------------------------------------------------------------
    def test_a_hold_session_keeps_its_slot_between_nested_calls(self) -> None:
        # 2026-10-08: a gate's steps are separate broker runs, and in the gap between clippy
        # and test another worktree took the slot, so the gate's next step paid a clean rebuild.
        env = self.pool_env("H", slots=1, gated=False)
        hold = subprocess.Popen(
            [sys.executable, str(HEAVY), "hold-worktree", "--", sys.executable,
             str(self.tmp / "pinned.py"), str(HEAVY), str(self.cargo), str(self.gate)],
            env=env, cwd=str(self.repo), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
            text=True)
        self.procs.append(hold)
        self.wait_for(lambda: (self.gate / "between").exists(), "the session's first build")
        # The slot is free of any build now, but pinned: another worktree must not get it.
        other = self.spawn("B", self.repo_b, slots=1, gated=False, wait_max=4)
        self.assertEqual(other.wait(timeout=LONG), 75,
                         f"B took a slot pinned to a live session: {self.stderr_of('B')!r}")
        (self.gate / "resume").write_text("")
        _, err = hold.communicate(timeout=LONG + 60)
        self.assertEqual(hold.returncode, 0, err)
        builds = [c for c in self.calls() if c["tag"] == "H" and self.is_build(c)]
        self.assertEqual(len(builds), 2, builds)
        self.assertEqual({real(b["td"]) for b in builds}, {real(self.slot(1))}, builds)
        self.assertFalse([c for c in self.calls() if self.is_member_clean(c)],
                         "nobody else built in the slot, so nothing may have cleaned it")
        # The session is over: the pin goes with it.
        self.assertEqual(real(self.run_seq("C", self.repo_b, slots=1)), real(self.slot(1)))


# The parent's own tests must not run again under this module.
for _n in [n for n in dir(base.TargetDirTests) if n.startswith("test_")]:
    setattr(PoolTests, _n, None)

if __name__ == "__main__":
    unittest.main()
