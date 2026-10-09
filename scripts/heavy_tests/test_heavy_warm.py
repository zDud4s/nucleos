#!/usr/bin/env python3
"""RED tests for `python scripts/heavy.py warm [--cwd PATH] [-- <argv>]`: an idle-priority,
detached pre-build of a worktree (default `cargo test -p nucleos-core --no-run`, kind cargo,
weight 2) that is queued at priority 3, so it never delays any real request (prio <= 2),
runs at below-normal CPU priority, is deduped per worktree, is skipped when the worktree is
already warm (fingerprint registry hit), inherits priority from a real run waiting on its
worktree lock, does nothing under NUCLEOS_HEAVY=0, and is marked `"warm": true` in log.jsonl.

`warm` does not exist yet, so every test here fails for that reason (argparse-style failure,
or the build is never admitted). Hermetic: temp NUCLEOS_HEAVY_DIR, throwaway git repos as
worktrees, and a fake `cargo` on PATH (a sleeper that records its argv/priority class and
waits on a stop file). Nothing real is compiled. Same fixture style as test_heavy_queue.py,
test_heavy_lock.py and test_heavy_fingerprint.py.

Fixture contract: the fake cargo records, per run, `<tag>.start` (pid), a line in
`order.txt`, `<tag>.argv` and (Windows) `<tag>.pclass`; `<tag>` is the last argv element with
leading dashes stripped (`--no-run` -> `no-run`). It runs until `<tag>.stop` or `all.stop`
exists, or exits at once when FAKE_FAST is set. The default warm argv is resolved as plain
`cargo` through PATH, so the fake `cargo.cmd` must be found the way run_child() already
resolves `npm`/`npx` (shutil.which with the child's PATH).
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HEAVY = ROOT / "scripts" / "heavy.py"

# Windows priority classes accepted for a warm build.
BELOW_NORMAL = 0x4000
IDLE = 0x40


def own_priority_class() -> int:
    """This process's Windows priority class, which a child started with no class flag inherits."""
    import ctypes

    k = ctypes.windll.kernel32
    k.GetCurrentProcess.restype = ctypes.c_void_p
    k.GetPriorityClass.argtypes = [ctypes.c_void_p]
    return k.GetPriorityClass(k.GetCurrentProcess())

FAKE = r'''
import os, pathlib, sys, time

d = pathlib.Path(os.environ["SLEEPER_DIR"])
tag = sys.argv[-1].lstrip("-")
(d / f"{tag}.start").write_text(str(os.getpid()))
(d / f"{tag}.argv").write_text(" ".join(sys.argv[1:]))
(d / f"{tag}.cwd").write_text(os.getcwd())
with open(d / "order.txt", "a") as f:
    f.write(tag + "\n")
if os.name == "nt":
    import ctypes
    k = ctypes.windll.kernel32
    k.GetCurrentProcess.restype = ctypes.c_void_p
    k.GetPriorityClass.argtypes = [ctypes.c_void_p]
    (d / f"{tag}.pclass").write_text(str(k.GetPriorityClass(k.GetCurrentProcess())))
if not os.environ.get("FAKE_FAST"):
    while not ((d / f"{tag}.stop").exists() or (d / "all.stop").exists()):
        time.sleep(0.05)
'''

FILES = {
    "Cargo.toml": '[workspace]\nmembers = ["core"]\n',
    "Cargo.lock": "# lock v1\n",
    "rust-toolchain.toml": '[toolchain]\nchannel = "stable"\n',
    "core/Cargo.toml": '[package]\nname = "nucleos-core"\n',
    "core/src/main.rs": "fn main() {}\n",
    "scripts/gates.sh": "#!/bin/sh\necho gates\n",
}


def git(path: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(path), *args], check=True, capture_output=True)


def make_repo(path: Path) -> Path:
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
    return path


def wait_for(pred, timeout: float = 120.0, step: float = 0.1) -> bool:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if pred():
            return True
        time.sleep(step)
    return pred()


def same_path(a: str, b: Path) -> bool:
    return os.path.normcase(os.path.realpath(a)) == os.path.normcase(os.path.realpath(str(b)))


def kill_pid(pid: int) -> None:
    try:
        if os.name == "nt":
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(pid)],
                           capture_output=True, timeout=20)
        else:
            os.kill(pid, 9)
    except Exception:
        pass


class WarmTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="heavy-w-"))
        self.state = self.tmp / "state"
        self.d = self.tmp / "marks"
        self.d.mkdir()
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        self.cargo_home = self.tmp / "cargo-home"
        self.cargo_home.mkdir()
        self.td = self.tmp / "target-1"
        self.repo_a = make_repo(self.tmp / "wt-a")
        self.repo_b = make_repo(self.tmp / "wt-b")
        self.repo_c = make_repo(self.tmp / "wt-c")
        self.procs: list[subprocess.Popen] = []
        self.outs: list = []
        (self.tmp / "fake.py").write_text(FAKE, encoding="utf-8")
        if os.name == "nt":
            self.cargo = self.bin / "cargo.cmd"
            self.cargo.write_text(
                f'@echo off\r\n"{sys.executable}" "{self.tmp / "fake.py"}" %*\r\n'
                "exit /b %ERRORLEVEL%\r\n",
                encoding="utf-8",
            )
        else:
            self.cargo = self.bin / "cargo"
            self.cargo.write_text(
                f'#!/bin/sh\nexec "{sys.executable}" "{self.tmp / "fake.py"}" "$@"\n',
                encoding="utf-8",
            )
            self.cargo.chmod(0o755)
        self.addCleanup(self._teardown)

    def _teardown(self) -> None:
        (self.d / "all.stop").write_text("x")
        # The warm broker is detached, so nobody holds a handle to it: wait for the queue and
        # the locks to drain, then kill whatever recorded itself and is still around.
        def drained() -> bool:
            q = self.state / "queue"
            return not (q.exists() and any(q.iterdir()))
        wait_for(drained, timeout=15)
        time.sleep(0.5)
        for p in self.procs:
            try:
                p.wait(timeout=10)
            except Exception:
                p.kill()
        pids: list[int] = []
        for f in self.d.glob("*.start"):
            try:
                pids.append(int(f.read_text()))
            except ValueError:
                pass
        q = self.state / "queue"
        if q.exists():
            for f in q.iterdir():
                try:
                    pids.append(int(f.name.rsplit("-", 1)[1]))
                except (ValueError, IndexError):
                    pass
        for pid in pids:
            kill_pid(pid)
        for fh in self.outs:
            try:
                fh.close()
            except Exception:
                pass
        time.sleep(0.3)
        shutil.rmtree(self.tmp, True)

    # ---- helpers -------------------------------------------------------------------
    def env(self, capacity: int = 4, **extra: str) -> dict:
        env = dict(os.environ)
        for k in list(env):
            if (k in ("NUCLEOS_HEAVY", "NUCLEOS_HEAVY_TOKEN", "NUCLEOS_HEAVY_HELD",
                      "NUCLEOS_HEAVY_PRIO", "NUCLEOS_HEAVY_WAIT_MAX", "NUCLEOS_HEAVY_MAIN",
                      "RUSTFLAGS", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR")
                    or k.startswith(("CARGO_PROFILE_", "CARGO_FEATURES", "FAKE_"))):
                env.pop(k)
        env.update(
            NUCLEOS_HEAVY_DIR=str(self.state),
            NUCLEOS_HEAVY_CAPACITY=str(capacity),
            NUCLEOS_HEAVY_POLL_S="0.1",
            CARGO_HOME=str(self.cargo_home),
            CARGO_TARGET_DIR=str(self.td),
            PATH=str(self.bin) + os.pathsep + env.get("PATH", ""),
            SLEEPER_DIR=str(self.d),
        )
        env.update(extra)
        return env

    def log_rows(self) -> list[dict]:
        p = self.state / "log.jsonl"
        if not p.exists():
            return []
        return [json.loads(x) for x in p.read_text(encoding="utf-8").splitlines() if x.strip()]

    def broker(self, tag: str, repo: Path, prio: int = 1, capacity: int = 4,
               **extra_env: str) -> subprocess.Popen:
        """A real (non-warm) cargo request through the broker; tag is the fake's last arg."""
        p = subprocess.Popen(
            [sys.executable, str(HEAVY), "--prio", str(prio), "--kind", "cargo", "--",
             "cargo", "check", tag],
            env=self.env(capacity, **extra_env), cwd=str(repo),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.procs.append(p)
        return p

    def warm(self, repo: Path | None = None, cwd: Path | None = None,
             argv: list[str] | None = None, capacity: int = 4,
             **extra_env: str) -> tuple[int, str, float]:
        """Run `heavy.py warm`; output goes to a file (never a pipe a detached child could
        keep open), and the command itself must return quickly."""
        args = [sys.executable, str(HEAVY), "warm"]
        if repo is not None:
            args += ["--cwd", str(repo)]
        if argv:
            args += ["--", *argv]
        out = open(self.tmp / f"warm-{len(self.outs)}.out", "w+", encoding="utf-8")
        self.outs.append(out)
        t0 = time.time()
        p = subprocess.Popen(args, env=self.env(capacity, **extra_env),
                             cwd=str(cwd or self.tmp), stdout=out, stderr=subprocess.STDOUT,
                             text=True)
        self.procs.append(p)
        try:
            rc = p.wait(timeout=60)
        except subprocess.TimeoutExpired:
            p.kill()
            self.fail("`warm` did not return: it must detach and exit within ~3 s")
        elapsed = time.time() - t0
        out.flush()
        out.seek(0)
        return rc, out.read(), elapsed

    def started(self, tag: str) -> bool:
        # The fake writes `<tag>.start` BEFORE its line in order.txt; "started" means both, or a
        # test that reads order() right after seeing the marker races the fake under load.
        return (self.d / f"{tag}.start").exists() and tag in self.order()

    def stop(self, tag: str) -> None:
        (self.d / f"{tag}.stop").write_text("x")

    def order(self) -> list[str]:
        f = self.d / "order.txt"
        return f.read_text().split() if f.exists() else []

    def assert_order(self, expected: list[str]) -> None:
        """`<tag>.start` is written BEFORE the order line, so a marker existing does not mean
        the order is complete: poll (bounded) until the order has every expected entry."""
        wait_for(lambda: len(self.order()) >= len(expected))
        self.assertEqual(self.order(), expected)

    def queue_names(self) -> list[str]:
        q = self.state / "queue"
        return sorted(p.name for p in q.iterdir()) if q.exists() else []

    def waiter_files(self) -> list[Path]:
        wt = self.state / "wt"
        out: list[Path] = []
        if wt.exists():
            for d in wt.glob("*.waiters"):
                out += [f for f in d.iterdir() if f.is_file()]
        return out

    def assert_started(self, tag: str) -> None:
        self.assertTrue(wait_for(lambda: self.started(tag)), f"{tag} never started")

    def assert_queued_prio(self, prio: int, n: int = 1) -> None:
        self.assertTrue(
            wait_for(lambda: len([x for x in self.queue_names()
                                  if x.startswith(f"{prio}-")]) >= n),
            f"no queued entry of prio {prio}: {self.queue_names()}",
        )

    def fill_capacity(self, tag: str = "blk") -> subprocess.Popen:
        """A prio-1 cargo (weight 2) in repo_b fills a capacity-2 machine."""
        p = self.broker(tag, self.repo_b, prio=1, capacity=2)
        self.assert_started(tag)
        return p

    # 1
    def test_warm_detaches_runs_default_argv_and_logs_warm(self) -> None:
        rc, out, elapsed = self.warm(self.repo_a)
        self.assertEqual(rc, 0, out)
        self.assertIn("warming", out)
        self.assertLess(elapsed, 4.0, "warm must return within ~3 s, not wait for the build")
        # The build continues in a background broker.
        self.assert_started("no-run")
        self.assertEqual((self.d / "no-run.argv").read_text(), "test -p nucleos-core --no-run")
        self.assertTrue(same_path((self.d / "no-run.cwd").read_text(), self.repo_a),
                        "the build must run in the --cwd worktree")
        self.assertEqual(self.log_rows(), [], "the row is written when the build ends")
        self.stop("no-run")
        self.assertTrue(wait_for(lambda: len(self.log_rows()) == 1), "no log row for the warm build")
        row = self.log_rows()[0]
        self.assertTrue(row.get("warm") is True or row.get("agent") == "warm", row)
        self.assertEqual(row.get("kind"), "cargo", row)
        self.assertEqual(row.get("weight"), 2, row)
        self.assertEqual(row.get("prio"), 3, row)
        self.assertTrue(same_path(row.get("worktree", ""), self.repo_a), row)

    # 1b
    def test_warm_defaults_to_the_current_directory_and_takes_argv_after_dashdash(self) -> None:
        rc, out, _ = self.warm(cwd=self.repo_a, argv=["cargo", "check", "custom"])
        self.assertEqual(rc, 0, out)
        self.assertIn("warming", out)
        self.assert_started("custom")
        self.assertEqual((self.d / "custom.argv").read_text(), "check custom")
        self.assertTrue(same_path((self.d / "custom.cwd").read_text(), self.repo_a))
        self.assertFalse(self.started("no-run"), "the default argv must not run when overridden")

    # 2
    def test_warm_is_idle_priority_a_real_request_goes_first(self) -> None:
        blk = self.fill_capacity()
        rc, out, _ = self.warm(self.repo_a, capacity=2)
        self.assertEqual(rc, 0, out)
        self.assertIn("warming", out)
        self.assert_queued_prio(3)
        # A prio-2 request arriving AFTER the warm still goes first.
        real = self.broker("real", self.repo_c, prio=2, capacity=2)
        self.assert_queued_prio(2)
        time.sleep(0.8)
        self.assertFalse(self.started("no-run"), "warm ran ahead of capacity / a prio-2 request")
        self.stop("blk")
        self.assert_started("real")
        time.sleep(0.8)
        self.assertFalse(self.started("no-run"),
                         "warm was admitted while a prio-2 request held the whole capacity")
        self.stop("real")
        self.assert_started("no-run")
        self.assert_order(["blk", "real", "no-run"])
        self.assertEqual(blk.wait(timeout=20), 0)
        self.assertEqual(real.wait(timeout=20), 0)

    # 2b
    def test_warm_waits_for_every_queued_real_request_not_only_the_first(self) -> None:
        self.fill_capacity()
        self.warm(self.repo_a, capacity=2)
        self.assert_queued_prio(3)
        # Distinct worktrees: the per-worktree lock admits only its holder to the token queue.
        repo_d = make_repo(self.tmp / "wt-d")
        repo_e = make_repo(self.tmp / "wt-e")
        for tag, repo, prio in (("r2", self.repo_c, 2), ("r1", repo_d, 1),
                                ("r0", repo_e, 0)):
            self.broker(tag, repo, prio=prio, capacity=2)
        self.assertTrue(wait_for(lambda: len(self.queue_names()) >= 4),
                        f"queue never filled: {self.queue_names()}")
        self.stop("blk")
        for tag in ("r0", "r1", "r2"):
            self.assert_started(tag)
            self.assertFalse(self.started("no-run"), f"warm ran before {tag} finished")
            self.stop(tag)
        self.assert_started("no-run")
        self.assert_order(["blk", "r0", "r1", "r2", "no-run"])

    # 3
    @unittest.skipUnless(os.name == "nt", "priority classes are a Windows contract")
    def test_warm_child_runs_below_normal_cpu_priority(self) -> None:
        rc, out, _ = self.warm(self.repo_a)
        self.assertEqual(rc, 0, out)
        self.assert_started("no-run")
        self.assertTrue(wait_for(lambda: (self.d / "no-run.pclass").exists()))
        pclass = int((self.d / "no-run.pclass").read_text())
        self.assertIn(pclass, (BELOW_NORMAL, IDLE), f"priority class {pclass:#x}")
        # A normal request is NOT lowered: it runs at whatever class this test runs at. Compared
        # with our own class rather than "not below normal", because a CI runner may start the
        # whole job below normal already, and every child of it inherits that.
        real = self.broker("real", self.repo_b, prio=1)
        self.assert_started("real")
        self.assertTrue(wait_for(lambda: (self.d / "real.pclass").exists()))
        self.assertEqual(int((self.d / "real.pclass").read_text()), own_priority_class())
        self.stop("real")
        self.assertEqual(real.wait(timeout=20), 0)

    # 4
    def test_second_warm_while_queued_is_deduped(self) -> None:
        self.fill_capacity()
        rc1, out1, _ = self.warm(self.repo_a, capacity=2)
        self.assertEqual(rc1, 0, out1)
        self.assertIn("warming", out1)
        self.assertNotIn("already", out1)
        self.assert_queued_prio(3)
        rc2, out2, _ = self.warm(self.repo_a, capacity=2)
        self.assertEqual(rc2, 0, out2)
        self.assertIn("already warming", out2)
        time.sleep(0.5)
        self.assertEqual(len([x for x in self.queue_names() if x.startswith("3-")]), 1,
                         f"a second warm was queued: {self.queue_names()}")
        # Another worktree is not a duplicate.
        rc3, out3, _ = self.warm(self.repo_c, capacity=2)
        self.assertEqual(rc3, 0, out3)
        self.assertIn("warming", out3)
        self.assertNotIn("already", out3)
        self.stop("blk")
        self.assert_started("no-run")
        self.stop("no-run")
        self.assertTrue(wait_for(lambda: len([r for r in self.log_rows()
                                              if r.get("warm") or r.get("agent") == "warm"
                                              ]) == 2), self.log_rows())
        warm_rows = [r for r in self.log_rows() if r.get("warm") or r.get("agent") == "warm"]
        self.assertEqual(sorted(os.path.normcase(os.path.realpath(r["worktree"]))
                                for r in warm_rows),
                         sorted(os.path.normcase(os.path.realpath(str(p)))
                                for p in (self.repo_a, self.repo_c)),
                         "exactly one warm per worktree")

    # 4b
    def test_second_warm_while_running_is_deduped(self) -> None:
        rc1, out1, _ = self.warm(self.repo_a)
        self.assertEqual(rc1, 0, out1)
        self.assert_started("no-run")
        rc2, out2, _ = self.warm(self.repo_a)
        self.assertEqual(rc2, 0, out2)
        self.assertIn("already warming", out2)
        time.sleep(1.0)
        self.assertEqual(self.order(), ["no-run"], "a second child run was started")
        self.stop("no-run")
        self.assertTrue(wait_for(lambda: len(self.log_rows()) == 1), self.log_rows())
        time.sleep(0.5)
        self.assertEqual(len(self.log_rows()), 1, "exactly one warm row")

    # 5
    def test_warm_worktree_prints_already_warm_and_runs_nothing(self) -> None:
        # Prime the registry: a real run of the same argv to exit 0 in the same target dir.
        prime = subprocess.run(
            [sys.executable, str(HEAVY), "--kind", "cargo", "--",
             "cargo", "test", "-p", "nucleos-core", "--no-run"],
            env=self.env(FAKE_FAST="1"), cwd=str(self.repo_a),
            capture_output=True, text=True, timeout=60,
        )
        self.assertEqual(prime.returncode, 0, prime.stderr)
        self.assertEqual(self.order(), ["no-run"])
        self.assertEqual(len(self.log_rows()), 1)
        rc, out, elapsed = self.warm(self.repo_a)
        self.assertEqual(rc, 0, out)
        self.assertIn("already warm", out)
        self.assertNotIn("already warming", out)
        self.assertLess(elapsed, 4.0)
        time.sleep(1.5)
        self.assertEqual(self.order(), ["no-run"], "a warm worktree must not run the build again")
        self.assertEqual(self.queue_names(), [])
        self.assertEqual(len(self.log_rows()), 1, "no warm row for a no-op")

    # 5b
    def test_a_changed_worktree_is_not_warm(self) -> None:
        subprocess.run(
            [sys.executable, str(HEAVY), "--kind", "cargo", "--",
             "cargo", "test", "-p", "nucleos-core", "--no-run"],
            env=self.env(FAKE_FAST="1"), cwd=str(self.repo_a),
            capture_output=True, text=True, timeout=60, check=True,
        )
        (self.repo_a / "core" / "src" / "main.rs").write_text("fn main() { /* edit */ }\n")
        rc, out, _ = self.warm(self.repo_a)
        self.assertEqual(rc, 0, out)
        self.assertIn("warming", out)
        self.assertNotIn("already warm", out)
        self.assertTrue(wait_for(lambda: self.order().count("no-run") == 2),
                        "the edited worktree should have been warmed")

    # 7
    def test_a_prio2_waiter_on_the_worktree_lock_lifts_the_warm(self) -> None:
        self.fill_capacity()
        rc, out, _ = self.warm(self.repo_a, capacity=2)
        self.assertEqual(rc, 0, out)
        self.assert_queued_prio(3)
        # A prio-2 request for ANOTHER worktree, queued after the warm: without inheritance
        # it would beat the prio-3 warm.
        self.broker("other", self.repo_c, prio=2, capacity=2)
        self.assert_queued_prio(2)
        # A prio-2 run of the SAME worktree waits on the worktree lock the warm holds: the
        # warm is the compile it needs, so the warm's effective priority becomes 2 and, being
        # older, it goes before the other prio-2 request.
        self.broker("same", self.repo_a, prio=2, capacity=2)
        self.assertTrue(wait_for(lambda: len(self.waiter_files()) >= 1),
                        "the same-worktree run never queued on the lock")
        time.sleep(0.6)
        self.stop("blk")
        self.assert_started("no-run")
        self.assertEqual(self.order()[:2], ["blk", "no-run"],
                         "the lifted warm should be admitted before the other prio-2 request")
        self.assertFalse(self.started("other"))

    # 8
    def test_warm_does_nothing_when_the_broker_is_off(self) -> None:
        rc, out, elapsed = self.warm(self.repo_a, NUCLEOS_HEAVY="0")
        self.assertEqual(rc, 0, out)
        self.assertNotIn("already", out)
        self.assertLess(elapsed, 4.0)
        time.sleep(1.5)
        self.assertEqual(self.order(), [], "NUCLEOS_HEAVY=0 must not build anything")
        self.assertEqual(self.queue_names(), [])
        self.assertEqual(self.log_rows(), [])

    # ---- a running warm yields to a real request -------------------------------------
    def seed_log(self, argv: list[str], runs: int = 3, run_s: float = 600.0) -> None:
        self.state.mkdir(parents=True, exist_ok=True)
        with open(self.state / "log.jsonl", "a", encoding="utf-8") as f:
            for _ in range(runs):
                f.write(json.dumps({"v": 1, "argv": argv, "run_s": run_s, "exit": 0}) + "\n")

    def preempted_warm_rows(self) -> list[dict]:
        return [r for r in self.log_rows()
                if r.get("warm") and r.get("preempted") and r.get("exit") == 75]

    def lock_records(self) -> list[dict]:
        wt = self.state / "wt"
        out: list[dict] = []
        for f in (sorted(wt.glob("*.lock")) if wt.exists() else []):
            try:
                out.append(json.loads(f.read_text(encoding="utf-8")))
            except (OSError, ValueError):
                pass
        return out

    # 9
    def test_a_running_warm_is_preempted_for_a_capacity_waiter_and_requeues(self) -> None:
        # The warm holds the whole machine (cargo = weight 2 of 2). A real request that can
        # only wait for capacity must not sit behind a background pre-build for its length:
        # the warm is stopped, the request runs, and the warm queues again behind it.
        rc, out, _ = self.warm(self.repo_a, capacity=2)
        self.assertEqual(rc, 0, out)
        self.assert_started("no-run")
        real = self.broker("real", self.repo_c, prio=1, capacity=2)
        self.assertTrue(wait_for(lambda: self.started("real"), timeout=30),
                        "a real request stayed behind a running warm")
        self.assertTrue(wait_for(lambda: bool(self.preempted_warm_rows()), timeout=30),
                        f"no preempted warm row: {self.log_rows()}")
        self.stop("real")
        self.assertEqual(real.wait(timeout=30), 0)
        self.assertTrue(wait_for(lambda: self.order().count("no-run") == 2),
                        f"the warm did not queue again: {self.order()}")
        self.assert_order(["no-run", "real", "no-run"])
        self.stop("no-run")
        self.assertTrue(
            wait_for(lambda: any(r.get("warm") and r.get("exit") == 0 for r in self.log_rows())),
            f"the requeued warm never finished: {self.log_rows()}")
        last = [r for r in self.log_rows() if r.get("warm")][-1]
        self.assertEqual(last.get("exit"), 0, last)
        self.assertFalse(last.get("preempted"), last)

    # 10
    def test_a_running_warm_is_preempted_for_a_waiter_on_its_worktree_lock(self) -> None:
        # Room for both (capacity 4), but they want the same worktree: the run waits for the
        # lock the warm holds. The warm is the compile it wants, yet it is estimated at ten
        # minutes - the agent must neither be told to give up (a warm is exempt from the
        # long-holder exit) nor wait for it.
        argv = ["cargo", "check", "w2"]
        self.seed_log(argv)
        rc, out, _ = self.warm(self.repo_a, argv=argv, capacity=4)
        self.assertEqual(rc, 0, out)
        self.assert_started("w2")
        same = subprocess.Popen(
            [sys.executable, str(HEAVY), "--prio", "1", "--agent", "main", "--kind", "cargo",
             "--wait-max", "60", "--", "cargo", "check", "same"],
            env=self.env(4), cwd=str(self.repo_a),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.procs.append(same)
        self.assertTrue(wait_for(lambda: self.started("same"), timeout=30),
                        "a real run stayed behind a running warm on its worktree lock")
        self.assertIsNone(same.poll(), "the run exited instead of waiting for its turn")
        self.assertTrue(wait_for(lambda: bool(self.preempted_warm_rows()), timeout=30),
                        f"no preempted warm row: {self.log_rows()}")
        self.stop("same")
        out_same, err_same = same.communicate(timeout=30)
        self.assertEqual(same.returncode, 0, err_same)
        self.assertTrue(wait_for(lambda: self.order().count("w2") == 2),
                        f"the warm did not run again after the real run: {self.order()}")
        self.assert_order(["w2", "same", "w2"])

    # 11
    def test_a_warm_with_room_beside_it_is_not_preempted(self) -> None:
        # capacity 4: the warm (2) and a real cargo run (2) fit side by side, and they are in
        # different worktrees - there is nothing to yield for.
        rc, out, _ = self.warm(self.repo_a, capacity=4)
        self.assertEqual(rc, 0, out)
        self.assert_started("no-run")
        real = self.broker("real", self.repo_c, prio=1, capacity=4)
        self.assert_started("real")
        time.sleep(2.0)
        self.assertEqual([r for r in self.log_rows() if r.get("preempted")], [])
        self.assert_order(["no-run", "real"])
        # While it runs the warm says so on its lock record: that is how a waiter tells a
        # background pre-build from a real run.
        self.assertTrue(any(r.get("warm") is True for r in self.lock_records()),
                        f"no lock record marks the warm: {self.lock_records()}")
        self.stop("real")
        self.assertEqual(real.wait(timeout=30), 0)

    # 12
    def test_the_logged_target_dir_is_normalised(self) -> None:
        # `C:/x/./` and `c:\x` are one directory; the log must give one spelling, or a
        # report grouping rows by target_dir counts it twice.
        # A tree with no crate in it gets no fingerprint, so the directory is logged as the
        # caller spelled it - the case where the spelling matters (a fingerprinted run logs a
        # resolved path already).
        plain = self.tmp / "plain"
        plain.mkdir()
        subprocess.run(["git", "init", "-q", str(plain)], check=True, capture_output=True)
        base = os.path.realpath(self.td).replace("\\", "/")
        value = base + "/./"
        p = self.broker("td", plain, FAKE_FAST="1", CARGO_TARGET_DIR=value)
        out, err = p.communicate(timeout=60)
        self.assertEqual(p.returncode, 0, err)
        rows = [r for r in self.log_rows() if "td" in (r.get("argv") or [])]
        self.assertEqual(len(rows), 1, self.log_rows())
        self.assertEqual(rows[0].get("target_dir"), os.path.normcase(os.path.normpath(value)))


if __name__ == "__main__":
    unittest.main()
