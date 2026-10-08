#!/usr/bin/env python3
"""Tests for the heavy broker's injected cargo target dir (`scripts/heavy.py`).

Measured 2026-10-03: agents ran filtered tests with CARGO_TARGET_DIR pointing at a per-branch
directory while `scripts/gates.sh` fell back to the machine-wide shared one, so the final gate
recompiled 225 crates that were already built. Every cargo run of one worktree must land in a
broker-chosen target dir, so the broker injects CARGO_TARGET_DIR when the caller named none.

Revised 2026-10-04: one `.cargo-target-<branch slug>` per branch grew without bound and filled
the disk, so the broker now leases a dir from a FIXED POOL, `<root>/.cargo-target-pool-<k>`
(k = 1..NUCLEOS_HEAVY_TARGET_SLOTS, default 3). Eligibility is unchanged; only the name of the
dir an eligible run gets is. The lease itself (exclusivity, affinity, owner-change clean) is
pinned in test_heavy_target_pool.py.

Hermetic: state lives in a temp NUCLEOS_HEAVY_DIR, the root of the injected dirs is a temp
NUCLEOS_HEAVY_TD_ROOT, each "worktree" is a throwaway git repo shaped like this project, and
`cargo` is a fake launcher around a python script that records its CARGO_TARGET_DIR and argv.
Nothing real is compiled.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HEAVY = ROOT / "scripts" / "heavy.py"

# Fake cargo body: records what the broker handed the child.
#   FAKE_OUT   - the last BUILD invocation (anything but `metadata`/`clean`) is written here.
#   FAKE_LOG   - a directory; every invocation drops one json record there (argv, td, time).
#   FAKE_TAG   - a label copied into each record.
#   FAKE_PROBE - a path; the record says whether it existed when cargo ran.
#   FAKE_CLEAN_EXIT - exit code of `cargo clean` (default 0).
#   FAKE_GATE  - a directory; a build writes started-<tag>, waits (bounded) for release-<tag>
#                or release-all, then writes ended-<tag> just before it exits.
# `cargo metadata` answers a minimal workspace with one member, nucleos-core.
FAKE = r'''
import json, os, pathlib, sys, time

argv = sys.argv[1:]
sub = argv[0] if argv else ""
td = os.environ.get("CARGO_TARGET_DIR")
tag = os.environ.get("FAKE_TAG", "")
log = os.environ.get("FAKE_LOG")
if log:
    rec = {"t": time.time_ns(), "pid": os.getpid(), "tag": tag, "argv": argv, "td": td,
           "cwd": os.getcwd()}
    probe = os.environ.get("FAKE_PROBE")
    if probe:
        rec["probe"] = os.path.exists(probe)
    pathlib.Path(log, f"{rec['t']}-{os.getpid()}.json").write_text(json.dumps(rec))
if sub == "metadata":
    pid = "path+file:///ws/core#nucleos-core@0.1.0"
    print(json.dumps({
        "packages": [{"name": "nucleos-core", "version": "0.1.0", "id": pid,
                      "manifest_path": os.path.join(os.getcwd(), "core", "Cargo.toml")}],
        "workspace_members": [pid], "workspace_default_members": [pid],
        "target_directory": td or "", "workspace_root": os.getcwd(), "version": 1,
    }))
    sys.exit(0)
if sub == "clean":
    sys.exit(int(os.environ.get("FAKE_CLEAN_EXIT") or 0))
out = os.environ.get("FAKE_OUT")
if out:
    pathlib.Path(out).write_text(json.dumps({
        "td": td,
        "btd": os.environ.get("CARGO_BUILD_TARGET_DIR"),
        "argv": argv,
    }))
gate = os.environ.get("FAKE_GATE")
if gate:
    g = pathlib.Path(gate)
    (g / f"started-{tag}").write_text(json.dumps({"td": td}))
    deadline = time.time() + 180
    while (time.time() < deadline and not (g / f"release-{tag}").exists()
           and not (g / "release-all").exists()):
        time.sleep(0.05)
    (g / f"ended-{tag}").write_text("")
sys.exit(0)
'''

# A python child for `hold-worktree`: runs the broker for a cargo command, nested.
NESTED = r'''
import os, subprocess, sys

heavy, cargo, out = sys.argv[1:4]
env = dict(os.environ, FAKE_OUT=out)
sys.exit(subprocess.run([sys.executable, heavy, "--", cargo, "test", "-p", "nucleos-core"],
                        env=env).returncode)
'''

FILES = {
    "Cargo.toml": '[workspace]\nmembers = ["core"]\n',
    "Cargo.lock": "# lock v1\n",
    "rust-toolchain.toml": '[toolchain]\nchannel = "stable"\n',
    "core/Cargo.toml": '[package]\nname = "nucleos-core"\n',
    "core/src/main.rs": "fn main() {}\n",
    "scripts/gates.sh": "#!/bin/sh\necho gates\n",
}

POOL_RE = re.compile(r"^\.cargo-target-pool-(\d+)$")


def git(path: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(path), *args], check=True, capture_output=True)


def make_repo(path: Path, branch: str | None = "feat/x-y", detach: bool = False) -> Path:
    path.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "init", "-q", str(path)], check=True, capture_output=True)
    git(path, "config", "user.email", "t@example.com")
    git(path, "config", "user.name", "t")
    git(path, "config", "commit.gpgsign", "false")
    for rel, text in FILES.items():
        f = path / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_text(text, encoding="utf-8")
    git(path, "add", "-A")
    git(path, "commit", "-q", "-m", "init")
    if detach:
        git(path, "checkout", "-q", "--detach")
    elif branch:
        git(path, "checkout", "-q", "-B", branch)
    return path


def real(p) -> str:
    return os.path.normcase(os.path.realpath(str(p)))


class TargetDirTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-td-"))
        self.state = self.tmp / "state"
        self.td_root = self.tmp / "tdroot"
        self.td_root.mkdir()
        self.cargo_home = self.tmp / "cargo-home"
        self.cargo_home.mkdir()
        self.out = self.tmp / "out.json"
        self.repo = make_repo(self.tmp / "wt-a")
        (self.tmp / "fake.py").write_text(FAKE, encoding="utf-8")
        (self.tmp / "nested.py").write_text(NESTED, encoding="utf-8")
        if os.name == "nt":
            self.cargo = self.tmp / "cargo.cmd"
            self.cargo.write_text(
                f'@echo off\r\n"{sys.executable}" "{self.tmp / "fake.py"}" %*\r\n'
                "exit /b %ERRORLEVEL%\r\n",
                encoding="utf-8",
            )
        else:
            self.cargo = self.tmp / "cargo"
            self.cargo.write_text(
                f'#!/bin/sh\nexec "{sys.executable}" "{self.tmp / "fake.py"}" "$@"\n',
                encoding="utf-8",
            )
            self.cargo.chmod(0o755)
        self.addCleanup(lambda: shutil.rmtree(self.tmp, True))

    # ---- helpers -------------------------------------------------------------------
    def env(self, **extra: str) -> dict:
        env = dict(os.environ)
        for k in list(env):
            if (k in ("NUCLEOS_HEAVY", "NUCLEOS_HEAVY_TOKEN", "NUCLEOS_HEAVY_HELD",
                      "NUCLEOS_HEAVY_PRIO", "NUCLEOS_HEAVY_WAIT_MAX", "NUCLEOS_HEAVY_MAIN",
                      "NUCLEOS_HEAVY_TARGET", "NUCLEOS_HEAVY_TARGET_SLOTS", "RUSTFLAGS",
                      "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR")
                    or k.startswith(("CARGO_PROFILE_", "CARGO_FEATURES", "FAKE_"))):
                env.pop(k)
        env.update(
            NUCLEOS_HEAVY_DIR=str(self.state),
            NUCLEOS_HEAVY_CAPACITY="4",
            NUCLEOS_HEAVY_POLL_S="0.1",
            NUCLEOS_HEAVY_TD_ROOT=str(self.td_root),
            CARGO_HOME=str(self.cargo_home),
            FAKE_OUT=str(self.out),
        )
        env.update(extra)
        return env

    def log_rows(self) -> list[dict]:
        p = self.state / "log.jsonl"
        if not p.exists():
            return []
        return [json.loads(x) for x in p.read_text(encoding="utf-8").splitlines() if x.strip()]

    def run_broker(self, cargo_args=None, cwd: Path | None = None, kind: str | None = None,
                   **env_kw) -> tuple[int, dict, dict]:
        """Run the broker once; return (exit code, what the child saw, the log row)."""
        if self.out.exists():
            self.out.unlink()
        before = len(self.log_rows())
        args = [sys.executable, str(HEAVY), "--wait-max", "60"]
        if kind:
            args += ["--kind", kind]
        args += ["--", str(self.cargo), *(cargo_args or ["test", "-p", "nucleos-core"])]
        cp = subprocess.run(args, env=self.env(**env_kw), cwd=str(cwd or self.repo),
                            capture_output=True, text=True, timeout=120)
        rows = self.log_rows()
        self.assertEqual(len(rows), before + 1, f"expected one new log row; stderr={cp.stderr!r}")
        seen = json.loads(self.out.read_text(encoding="utf-8")) if self.out.exists() else {}
        return cp.returncode, seen, rows[-1]

    def pool_index(self, td, slots: int = 3) -> int:
        """`td` is `<td_root>/.cargo-target-pool-<k>` with 1 <= k <= slots; returns k."""
        self.assertTrue(td, "the child got no CARGO_TARGET_DIR")
        p = Path(real(td))
        self.assertEqual(real(p.parent), real(self.td_root), f"not under the td root: {td}")
        m = POOL_RE.match(p.name)
        self.assertIsNotNone(m, f"not a pool dir (.cargo-target-pool-<k>): {td}")
        k = int(m.group(1))
        self.assertTrue(1 <= k <= slots, f"pool index {k} outside 1..{slots}: {td}")
        return k

    def assert_pool(self, seen: dict) -> str:
        """The child ran with a pool dir; returns it (normalised)."""
        self.assertTrue(seen.get("td"), f"the child got no CARGO_TARGET_DIR: {seen}")
        self.pool_index(seen["td"])
        return real(seen["td"])

    def assert_no_branch_dirs(self) -> None:
        """The broker never creates a per-branch `.cargo-target-<slug>` any more."""
        stray = [p.name for p in self.td_root.iterdir()
                 if p.name.startswith(".cargo-target-") and not POOL_RE.match(p.name)]
        self.assertEqual(stray, [], "per-branch target dirs were created")

    # 1
    def test_injects_pool_dir_under_root(self) -> None:
        rc, seen, _ = self.run_broker()
        self.assertEqual(rc, 0)
        first = self.assert_pool(seen)
        # The same dir from a subdirectory of the same worktree (it is the same worktree).
        rc, seen, _ = self.run_broker(cwd=self.repo / "core")
        self.assertEqual(rc, 0)
        self.assertEqual(self.assert_pool(seen), first, seen)
        # Another worktree also gets a pool dir, not one named after its branch.
        solo = make_repo(self.tmp / "wt-solo", branch="solo")
        _, seen, _ = self.run_broker(cwd=solo)
        self.assert_pool(seen)
        self.assert_no_branch_dirs()

    def test_default_root_is_parent_of_main_checkout(self) -> None:
        # Without NUCLEOS_HEAVY_TD_ROOT the pool lives in the parent of the main checkout.
        # Asked of `_td_root()` directly: a black-box run here would lease a REAL pool dir
        # of this machine.
        import importlib.util
        spec = importlib.util.spec_from_file_location("heavy_td_root_probe", HEAVY)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        saved = os.environ.pop("NUCLEOS_HEAVY_TD_ROOT", None)
        try:
            # The main checkout, not ROOT: run from a linked worktree, ROOT is that worktree.
            common = subprocess.run(["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
                                    cwd=str(ROOT), capture_output=True, text=True, check=True)
            self.assertEqual(real(mod._td_root()), real(Path(common.stdout.strip()).parent.parent))
        finally:
            if saved is not None:
                os.environ["NUCLEOS_HEAVY_TD_ROOT"] = saved

    def test_detached_and_reserved_branch_names_get_a_pool_dir(self) -> None:
        # These used to fall back to a slug from the directory name; now they are ordinary
        # eligible runs and lease a pool dir like any other.
        detached = make_repo(self.tmp / "wt-detached", detach=True)
        _, seen, _ = self.run_broker(cwd=detached)
        self.assert_pool(seen)
        for name in ("test", "gates", "gate"):
            wt = make_repo(self.tmp / f"wt-for-{name}", branch=name)
            _, seen, _ = self.run_broker(cwd=wt)
            self.assert_pool(seen)
        self.assert_no_branch_dirs()

    # 2
    def test_explicit_target_dir_is_respected(self) -> None:
        mine = self.tmp / "mine"
        _, seen, row = self.run_broker(CARGO_TARGET_DIR=str(mine))
        self.assertEqual(real(seen.get("td") or ""), real(mine), seen)
        self.assertEqual(real(row.get("target_dir") or ""), real(mine), row)
        # CARGO_BUILD_TARGET_DIR counts as explicit: nothing is added next to it.
        other = self.tmp / "other"
        _, seen, row = self.run_broker(CARGO_BUILD_TARGET_DIR=str(other))
        self.assertIsNone(seen.get("td"), f"CARGO_TARGET_DIR must not be injected: {seen}")
        self.assertEqual(real(seen.get("btd") or ""), real(other), seen)
        self.assertEqual(real(row.get("target_dir") or ""), real(other), row)
        # `--target-dir` in argv is explicit too.
        flagged = self.tmp / "flagged"
        _, seen, row = self.run_broker(
            ["test", "-p", "nucleos-core", "--target-dir", str(flagged)])
        self.assertIsNone(seen.get("td"), f"CARGO_TARGET_DIR must not be injected: {seen}")
        self.assertIn("--target-dir", seen.get("argv", []), seen)
        self.assertEqual(real(row.get("target_dir") or ""), real(flagged), row)
        self.assertFalse((self.td_root / ".cargo-target-feat-x-y").exists())
        self.assertEqual(sorted(p.name for p in self.td_root.glob(".cargo-target-pool-*")), [],
                         "an explicit target dir leases no pool dir")

    # 3
    def test_disabled_by_env(self) -> None:
        rc, seen, _ = self.run_broker(NUCLEOS_HEAVY_TARGET="0")
        self.assertEqual(rc, 0)
        self.assertEqual(seen.get("argv"), ["test", "-p", "nucleos-core"], "the child never ran")
        self.assertIsNone(seen.get("td"), f"NUCLEOS_HEAVY_TARGET=0 must not inject: {seen}")
        # Control: the very same run without the switch does inject.
        _, seen, _ = self.run_broker()
        self.assert_pool(seen)

    # 4
    def test_non_cargo_kinds_get_nothing(self) -> None:
        for kind in ("node", "go"):
            rc, seen, row = self.run_broker(kind=kind)
            self.assertEqual(rc, 0)
            self.assertEqual(seen.get("argv"), ["test", "-p", "nucleos-core"], "the child never ran")
            self.assertIsNone(seen.get("td"), f"kind {kind} must not get CARGO_TARGET_DIR: {seen}")
            self.assertIsNone(seen.get("btd"), seen)
            self.assertFalse(row.get("target_dir"), f"kind {kind} logs no target_dir: {row}")
        # Control: a cargo run does.
        _, seen, _ = self.run_broker()
        self.assert_pool(seen)

    # 5
    def test_log_row_records_effective_target_dir(self) -> None:
        _, seen, row = self.run_broker()
        td = self.assert_pool(seen)
        self.assertTrue(row.get("target_dir"), f"cargo rows record target_dir: {row}")
        self.assertEqual(real(row["target_dir"]), td, row)

    # 6
    def test_fingerprint_uses_injected_dir(self) -> None:
        _, seen1, first = self.run_broker()
        self.assertFalse(first.get("fp_hit"), first)
        td = self.assert_pool(seen1)
        rc, _, second = self.run_broker()
        self.assertEqual(rc, 0)
        self.assertTrue(second.get("fp_hit") and second.get("weight") == 0,
                        f"an unchanged worktree must hit on the second run: {second}")
        entries = sorted((self.state / "td").glob("*.json"))
        self.assertEqual(len(entries), 1, "one target dir, one registry entry")
        rec = json.loads(entries[0].read_text(encoding="utf-8"))
        self.assertTrue(rec.get("target_dir"), f"the registry entry names its target dir: {rec}")
        self.assertEqual(real(rec["target_dir"]), td, rec)

    # 7
    def test_nested_cargo_inherits_injected_dir(self) -> None:
        if self.out.exists():
            self.out.unlink()
        cp = subprocess.run(
            [sys.executable, str(HEAVY), "hold-worktree", "--", sys.executable,
             str(self.tmp / "nested.py"), str(HEAVY), str(self.cargo), str(self.out)],
            env=self.env(), cwd=str(self.repo), capture_output=True, text=True, timeout=120)
        self.assertEqual(cp.returncode, 0, cp.stderr)
        self.assertTrue(self.out.exists(), f"the nested cargo never ran: {cp.stderr!r}")
        seen = json.loads(self.out.read_text(encoding="utf-8"))
        self.assert_pool(seen)


if __name__ == "__main__":
    unittest.main()
