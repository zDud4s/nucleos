#!/usr/bin/env python3
"""Cover `scripts/build-slot.sh` and the `slot_run` it takes from `scripts/gates.sh`.

Nine sessions once compiled nucleos-core at the same time. The slots cap that at two, and what a
mistake in them would cost is asserted here: a third holder must wait, a dead holder must not keep
its slot, a nested call must not deadlock on its own parent, and the bypass must really bypass.

Temp directories only; nothing is built. Holders are plain `bash -c` loops waiting on a stop file.

**Under a POSIX bash, never the `bash` on PATH on Windows**, which is WSL: see
`scripts/test-gates-summary.py`. `GIT_BASH` overrides the search.

Run:  python scripts/test-build-slot.py
"""

import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = (ROOT / "scripts" / "build-slot.sh").as_posix()
GATES = ROOT / "scripts" / "gates.sh"


def posix_bash() -> str:
    override = os.environ.get("GIT_BASH")
    if override:
        return override
    if os.name != "nt":
        found = shutil.which("bash")
        if found:
            return found
        sys.exit("no bash on PATH")
    candidates = []
    git = shutil.which("git")
    if git:
        here = Path(git).resolve().parent
        candidates += [folder / "bin" / "bash.exe" for folder in (here.parent, here.parent.parent)]
    candidates.append(Path("C:/Program Files/Git/bin/bash.exe"))
    for candidate in candidates:
        if candidate.is_file():
            return str(candidate)
    sys.exit("Git's bash.exe not found; set GIT_BASH to it (not the bash on PATH, which is WSL)")


BASH = posix_bash()
# A main checkout with no broker in it: the existing tests exercise the `slot_run` fallback, and
# must not reach the real broker of whatever checkout this file lives in.
NO_BROKER = tempfile.mkdtemp(prefix="no-broker-")
NL = chr(10)
HOLD = 'touch "$1"; while [ ! -e "$2" ]; do sleep 0.2; done'


def env_for(slots_dir: Path, **extra: str) -> dict:
    env = dict(os.environ)
    # The machine's own tuning must not reach the tests: a user-level NUCLEOS_BUILD_SLOTS=4 let a
    # third holder run beside two that were meant to fill every slot.
    for name in ("NUCLEOS_BUILD_SLOT_HELD", "NUCLEOS_BUILD_SLOTS", "NUCLEOS_BUILD_SLOT_TIMEOUT"):
        env.pop(name, None)
    env["NUCLEOS_BUILD_SLOTS_DIR"] = slots_dir.as_posix()
    env["NUCLEOS_HEAVY_MAIN"] = Path(NO_BROKER).as_posix()
    env.update(extra)
    return env


def wait_for(path: Path, seconds: float = 30) -> None:
    end = time.time() + seconds
    while not path.exists():
        if time.time() > end:
            raise AssertionError(f"timed out waiting for {path}")
        time.sleep(0.1)


def held(slots_dir: Path) -> list[str]:
    folder = slots_dir / "held"
    return sorted(p.name for p in folder.iterdir()) if folder.is_dir() else []


def start_holder(slots_dir: Path, work: Path, name: str, **extra: str):
    go, stop = work / f"{name}.go", work / f"{name}.stop"
    proc = subprocess.Popen(
        [BASH, SCRIPT, "bash", "-c", HOLD, "_", go.as_posix(), stop.as_posix()],
        env=env_for(slots_dir, **extra),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return proc, go, stop


def slot_run(slots_dir: Path, *command: str, timeout: int = 60, **extra: str):
    return subprocess.run(
        [BASH, SCRIPT, *command],
        env=env_for(slots_dir, **extra),
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def sh(script: str) -> None:
    subprocess.run([BASH, "-c", script], check=True, timeout=30)


def finish(holders: list) -> None:
    for proc, _go, stop in holders:
        stop.touch()
    for proc, _go, _stop in holders:
        try:
            proc.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.communicate()


def test_a_third_holder_waits_until_a_slot_frees() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        slots = work / "slots"
        a = start_holder(slots, work, "a")
        b = start_holder(slots, work, "b")
        holders = [a, b]
        third = None
        try:
            wait_for(a[1])
            wait_for(b[1])
            assert len(held(slots)) == 2, held(slots)
            third = start_holder(slots, work, "c")
            holders.append(third)
            time.sleep(3)
            assert not third[1].exists(), "a third holder ran while two slots were held"
            a[2].touch()
            wait_for(third[1])
            assert len(held(slots)) == 2, held(slots)
        finally:
            finish(holders)
    # the waiting message went to the waiter's stderr
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        slots = work / "slots"
        a = start_holder(slots, work, "a", NUCLEOS_BUILD_SLOTS="1")
        try:
            wait_for(a[1])
            result = slot_run(slots, "true", NUCLEOS_BUILD_SLOTS="1", NUCLEOS_BUILD_SLOT_TIMEOUT="2")
            assert "waiting for a build slot" in result.stderr, result.stderr
        finally:
            finish([a])


def test_a_waiter_gives_up_at_the_timeout() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        slots = work / "slots"
        marker = work / "ran"
        a = start_holder(slots, work, "a", NUCLEOS_BUILD_SLOTS="1")
        try:
            wait_for(a[1])
            result = slot_run(
                slots, "touch", marker.as_posix(), NUCLEOS_BUILD_SLOTS="1", NUCLEOS_BUILD_SLOT_TIMEOUT="2"
            )
            assert result.returncode == 75, (result.returncode, result.stderr)
            assert "gave up" in result.stderr, result.stderr
            assert not marker.exists(), "the command ran without a slot"
        finally:
            finish([a])


def test_a_killed_holder_is_reaped_and_a_terminated_one_releases() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        slots = work / "slots"
        a = start_holder(slots, work, "a", NUCLEOS_BUILD_SLOTS="1")
        holders = [a]
        try:
            wait_for(a[1])
            (pid,) = held(slots)
            sh(f"kill -9 {pid}")
            # Reap it. The holder is this process's own child, and on Linux a killed child stays a
            # zombie until its parent waits on it; `kill -0` answers yes for a zombie, so the slot
            # read as held for the whole timeout. MSYS has no zombies, which is why only CI saw it.
            a[0].wait(timeout=10)
            time.sleep(1)
            # The dead holder's file is still there: nothing cleaned up after a SIGKILL.
            assert held(slots) == [pid], held(slots)
            result = slot_run(slots, "echo", "ran", NUCLEOS_BUILD_SLOTS="1", NUCLEOS_BUILD_SLOT_TIMEOUT="20")
            assert result.returncode == 0 and "ran" in result.stdout, (result.returncode, result.stderr)
            assert "reaped slot of dead pid" in result.stderr, result.stderr
            assert held(slots) == [], held(slots)

            b = start_holder(slots, work, "b", NUCLEOS_BUILD_SLOTS="1")
            holders.append(b)
            wait_for(b[1])
            (pid,) = held(slots)
            sh(f"kill -TERM {pid}")
            end = time.time() + 20
            while held(slots) and time.time() < end:
                time.sleep(0.2)
            assert held(slots) == [], f"a terminated holder kept its slot: {held(slots)}"
        finally:
            finish(holders)


def test_a_nested_call_does_not_take_a_second_slot() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        slots = work / "slots"
        seen = work / "seen"
        inner = f'ls "{(slots / "held").as_posix()}" | wc -l > "{seen.as_posix()}"'
        result = slot_run(
            slots,
            "bash",
            SCRIPT,
            "bash",
            "-c",
            inner,
            NUCLEOS_BUILD_SLOTS="1",
            NUCLEOS_BUILD_SLOT_TIMEOUT="5",
        )
        assert result.returncode == 0, (result.returncode, result.stderr)
        assert seen.read_text().strip() == "1", seen.read_text()
        assert held(slots) == [], held(slots)


def test_status_and_arguments_pass_through_and_the_slot_is_released() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        slots = Path(tmp) / "slots"
        result = slot_run(slots, "bash", "-c", 'printf "%s|%s" "$1" "$2"; exit 7', "_", "a b", "c")
        assert result.returncode == 7, result.returncode
        assert result.stdout == "a b|c", result.stdout
        assert held(slots) == [], held(slots)
        result = slot_run(slots, "true")
        assert result.returncode == 0 and held(slots) == []


def test_bypass_and_a_relative_dir() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        slots = work / "slots"
        a = start_holder(slots, work, "a", NUCLEOS_BUILD_SLOTS="1")
        try:
            wait_for(a[1])
            result = slot_run(slots, "echo", "ran", NUCLEOS_BUILD_SLOTS="0", NUCLEOS_BUILD_SLOT_TIMEOUT="1")
            assert result.returncode == 0 and "ran" in result.stdout, (result.returncode, result.stderr)
        finally:
            finish([a])
        marker = work / "ran"
        env = env_for(slots)
        env["NUCLEOS_BUILD_SLOTS_DIR"] = "relative/slots"
        result = subprocess.run(
            [BASH, SCRIPT, "touch", marker.as_posix()], env=env, capture_output=True, text=True, timeout=30
        )
        assert result.returncode == 2, (result.returncode, result.stderr)
        assert not marker.exists()


def test_check_and_clippy_skip_the_slot_but_test_still_waits() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        slots = work / "slots"
        bindir = work / "bin"
        bindir.mkdir()
        fake = bindir / "cargo"
        fake.write_text('#!/bin/sh\necho "fake cargo $1"\nexit 0\n', encoding="utf-8", newline="\n")
        fake.chmod(0o755)
        path = bindir.as_posix() + os.pathsep + os.environ["PATH"]
        a = start_holder(slots, work, "a", NUCLEOS_BUILD_SLOTS="1")
        try:
            wait_for(a[1])
            for sub in ("check", "clippy"):
                start = time.time()
                result = slot_run(
                    slots, "cargo", sub, NUCLEOS_BUILD_SLOTS="1", NUCLEOS_BUILD_SLOT_TIMEOUT="30", PATH=path
                )
                assert result.returncode == 0, (sub, result.returncode, result.stderr)
                assert f"fake cargo {sub}" in result.stdout, result.stdout
                assert "waiting for a build slot" not in result.stderr, result.stderr
                assert time.time() - start < 15, f"cargo {sub} waited for a slot"
            result = slot_run(
                slots, "cargo", "test", NUCLEOS_BUILD_SLOTS="1", NUCLEOS_BUILD_SLOT_TIMEOUT="2", PATH=path
            )
            assert result.returncode == 75, (result.returncode, result.stderr)
            assert "fake cargo" not in result.stdout, result.stdout
        finally:
            finish([a])


def test_gates_wraps_only_the_building_steps() -> None:
    text = GATES.read_text(encoding="utf-8")
    lines = text.splitlines()
    # The classifier test reads every `run "core: ` line as a plain command, so no gate line may
    # carry the prefix: the broker is reached inside `run` itself.
    runs = [line.strip() for line in lines if line.strip().startswith("run ")]
    assert not [r for r in runs if "slot_run" in r or "heavy_run" in r], runs
    assert "heavy_run()" in text, "gates.sh defines no heavy_run"
    assert '[ "$1" = cargo ]' in text
    case = [l.strip() for l in lines if l.strip().startswith("case ") and "heavy_run" in l]
    assert len(case) >= 1, case
    subs = case[0].split(" in ", 1)[1].split(")", 1)[0].split("|")
    assert subs == ["build", "check", "clippy", "test", "nextest", "run", "doc"], subs
    for other in ("tsc", "npm", "go"):
        assert other in text.split("run()", 1)[1].split("kept_lines()", 1)[0], other
    for label in ("core: fmt", "core: clippy", "core: test", "shell/src-tauri: clippy", "shell/src-tauri: test"):
        # "core: test" appears twice on purpose: cargo nextest when installed, cargo test otherwise.
        want = 2 if label == "core: test" else 1
        assert len([r for r in runs if r.startswith(f'run "{label}"')]) == want, label


def fake_main(work: Path, with_classify: bool = True) -> tuple[Path, Path]:
    """A fake main checkout whose broker records its argv instead of running anything."""
    main = work / "main"
    (main / "scripts").mkdir(parents=True)
    (main / ".ai" / "scripts").mkdir(parents=True)
    record = work / "broker-argv"
    (main / "scripts" / "heavy.py").write_text(
        NL.join(["import sys", "open(%r, 'w').write(%r.join(sys.argv[1:]))" % (record.as_posix(), NL), ""]),
        encoding="utf-8",
    )
    if with_classify:
        (main / ".ai" / "scripts" / "heavy_classify.py").write_text("", encoding="utf-8")
    return main, record


def fake_cargo(work: Path) -> str:
    bindir = work / "bin"
    bindir.mkdir()
    fake = bindir / "cargo"
    fake.write_text(NL.join(["#!/bin/sh", 'echo "fake cargo $1"', "exit 0", ""]), encoding="utf-8", newline=NL)
    fake.chmod(0o755)
    return bindir.as_posix() + os.pathsep + os.environ["PATH"]


def test_gates_delegates_to_broker_when_present() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        slots = work / "slots"
        main, record = fake_main(work)
        path = fake_cargo(work)
        py = Path(sys.executable).as_posix()
        extra = dict(NUCLEOS_HEAVY_MAIN=main.as_posix(), NUCLEOS_HEAVY_PYTHON=py, PATH=path)
        # build-slot.sh goes through the broker...
        result = slot_run(slots, "cargo", "test", "-p", "x", **extra)
        assert result.returncode == 0, (result.returncode, result.stderr)
        assert record.is_file(), ("the broker was not called", result.stdout, result.stderr)
        assert record.read_text().split(NL) == ["--", "cargo", "test", "-p", "x"], record.read_text()
        assert "fake cargo" not in result.stdout, "the command ran in-process instead of via the broker"
        record.unlink()
        # ...and so does a step of gates.sh's own `run`.
        script = f'source "{GATES.as_posix()}" || exit 2; run "step" "{work.as_posix()}" cargo build'
        result = subprocess.run(
            [BASH, "-c", script], env=env_for(slots, **extra), capture_output=True, text=True, timeout=60
        )
        assert record.is_file(), ("gates.sh run did not call the broker", result.stdout, result.stderr)
        assert record.read_text().split(NL) == ["--", "cargo", "build"], record.read_text()


def test_gates_falls_back_when_broker_absent() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        slots = work / "slots"
        path = fake_cargo(work)
        py = Path(sys.executable).as_posix()
        # No broker at all: the slot is taken as before.
        a = start_holder(slots, work, "a", NUCLEOS_BUILD_SLOTS="1")
        try:
            wait_for(a[1])
            result = slot_run(
                slots, "cargo", "test", NUCLEOS_BUILD_SLOTS="1", NUCLEOS_BUILD_SLOT_TIMEOUT="2", PATH=path
            )
            assert result.returncode == 75, (result.returncode, result.stderr)
            assert "gave up" in result.stderr, result.stderr
        finally:
            finish([a])
        # heavy.py without heavy_classify.py (A7) is not a broker either: same fallback, and the
        # command still runs.
        main, record = fake_main(work, with_classify=False)
        result = slot_run(
            slots, "cargo", "test", NUCLEOS_HEAVY_MAIN=main.as_posix(), NUCLEOS_HEAVY_PYTHON=py, PATH=path
        )
        assert result.returncode == 0, (result.returncode, result.stderr)
        assert "fake cargo test" in result.stdout, result.stdout
        assert not record.exists(), "an incomplete broker was called"
        # a non-cargo command goes through gates.sh's run untouched when the broker is absent
        script = f'source "{GATES.as_posix()}" || exit 2; run "step" "{work.as_posix()}" echo plain'
        result = subprocess.run(
            [BASH, "-c", script], env=env_for(slots, PATH=path), capture_output=True, text=True, timeout=60
        )
        assert "plain" in result.stdout and "ok   step" in result.stdout, (result.stdout, result.stderr)


test_a_third_holder_waits_until_a_slot_frees()
test_a_waiter_gives_up_at_the_timeout()
test_a_killed_holder_is_reaped_and_a_terminated_one_releases()
test_a_nested_call_does_not_take_a_second_slot()
test_status_and_arguments_pass_through_and_the_slot_is_released()
test_bypass_and_a_relative_dir()
test_check_and_clippy_skip_the_slot_but_test_still_waits()
test_gates_wraps_only_the_building_steps()
test_gates_delegates_to_broker_when_present()
test_gates_falls_back_when_broker_absent()
print("build slot: ok")
