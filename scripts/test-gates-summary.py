#!/usr/bin/env python3
"""Cover the closing summary of `scripts/gates.sh`: a red step's own last lines, not just its name.

The daemon keeps only the last 4096 bytes of a red gate's output for the node that must answer it
(`GATE_OUTPUT_TAIL` in `core/src/job.rs`). Job 27, 2026-09-14: `core: fmt` failed first and the
full `core: test` step then printed about a megabyte, so the retry saw `core: fmt` only as a name in
the summary's list and never rustfmt's diff. Beside an unrelated flaky test it concluded the red was
not its own, left the file unformatted, and the second gate failed on fmt again. The summary now
repeats each red step's last lines under its label, and this is what holds it there.

Sources the real `scripts/gates.sh` — it returns before its gate groups when sourced — and drives
its `run` with fake steps, so nothing here compiles, formats or tests anything.

**Under a POSIX bash, never the `bash` on PATH on Windows.** There that name is
`C:\\Windows\\System32\\bash.exe`, WSL: another operating system, which would run the fake steps
somewhere else entirely or not at all. `GIT_BASH` overrides the search.

Run:  python scripts/test-gates-summary.py
"""

import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GATES = ROOT / "scripts" / "gates.sh"
TAIL = 4096  # GATE_OUTPUT_TAIL in core/src/job.rs


def posix_bash() -> str:
    override = os.environ.get("GIT_BASH")
    if override:
        return override
    if os.name != "nt":
        found = shutil.which("bash")
        if found:
            return found
        sys.exit("no bash on PATH")
    # Git's launcher, `<Git>/bin/bash.exe`, found from wherever `git` itself answers: `cmd/`,
    # `bin/` or `mingw64/bin/` under the same installation. The launcher rather than
    # `usr/bin/bash.exe` because it is what puts Git's `/usr/bin` on the PATH it hands on.
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


def drive(steps: str, workdir: Path) -> tuple[int, str]:
    """Source gates.sh, run `steps`, print the summary; return (status, stdout and stderr as one).

    One stream, the way the daemon reads a gate. `TMPDIR` points at `workdir/tmp` so the test can
    see whether the captures were cleaned up.
    """
    scratch = workdir / "tmp"
    scratch.mkdir()
    script = 'source "$1"\n' + steps + "\nprint_summary\n"
    done = subprocess.run(
        [BASH, "-c", script, "test-gates-summary", GATES.as_posix()],
        cwd=workdir,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=dict(os.environ, TMPDIR=scratch.as_posix()),
        timeout=120,
    )
    left = list(scratch.iterdir())
    assert not left, f"captures left behind in TMPDIR: {left}"
    return done.returncode, done.stdout.decode("utf-8", errors="replace").replace("\r\n", "\n")


def summary_blocks(output: str) -> dict[str, list[str]]:
    """Each label the summary names, with the evidence lines indented under it, in order."""
    assert "\ngates FAILED:\n" in output, output[-2000:]
    summary = output.split("\ngates FAILED:\n", 1)[1]
    blocks: dict[str, list[str]] = {}
    label = None
    for line in summary.splitlines():
        if re.match(r"^ {2}\S", line):
            label = line[2:]
            blocks[label] = []
        elif line.startswith("      ") and label is not None:
            blocks[label].append(line[6:])
        else:
            raise AssertionError(f"a summary line that is neither label nor evidence: {line!r}")
    return blocks


STEPS = r"""
passing() { echo "passing step says hello"; echo "passing step says goodbye"; }
noisy() {
  for ((i = 1; i <= 500; i++)); do echo "noisy line $i"; done
  echo "noisy's own verdict" >&2
  return 3
}
quiet() { return 1; }
run "fake: passing" . passing
run "fake: noisy"   . noisy
run "fake: quiet"   . quiet
"""


def test_a_red_steps_own_lines_follow_its_name() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        status, output = drive(STEPS, Path(tmp))

    assert status == 1, (status, output[-2000:])

    # Streaming preserved: every line of every step, in order, and the verdict lines as they were.
    stream = output.split("\ngates FAILED:\n", 1)[0]
    positions = [stream.index(f"noisy line {i}\n") for i in range(1, 501)]
    assert positions == sorted(positions), "the noisy step's lines were reordered"
    assert "noisy's own verdict\n" in stream, "a step's stderr no longer reaches the gate's output"
    assert "passing step says hello\npassing step says goodbye\n" in stream, stream[:500]
    for line in ("ok   fake: passing", "FAIL fake: noisy", "FAIL fake: quiet"):
        assert f"\n{line}\n" in stream, f"missing {line!r}"
    assert stream.index("noisy's own verdict") < stream.index("FAIL fake: noisy")

    blocks = summary_blocks(output)
    assert list(blocks) == ["fake: noisy", "fake: quiet"], blocks
    noisy = blocks["fake: noisy"]
    assert noisy and noisy[-1] == "noisy's own verdict", f"the noisy step's evidence: {noisy}"
    assert "noisy line 500" in noisy and "noisy line 1" not in noisy, noisy
    assert len(noisy) <= 12, noisy
    # The quiet step gets its own (empty) evidence, never the noisy step's leftovers.
    assert blocks["fake: quiet"] == ["(no output)"], blocks["fake: quiet"]
    assert "passing step says" not in output.split("\ngates FAILED:\n", 1)[1]


def test_three_red_steps_fit_well_inside_the_daemons_tail() -> None:
    # Long lines on purpose: 300 bytes each, so the width cut and the per-step budget both bind.
    steps = "\n".join(
        f"""step_{name}() {{
  for ((i = 1; i <= 300; i++)); do printf '{name} line %04d %s\\n' "$i" "$(printf 'x%.0s' {{1..280}})"; done
  printf '{name} final %s\\n' "$(printf 'y%.0s' {{1..280}})"
  return 1
}}
run "fake: {name}" . step_{name}"""
        for name in ("fmt", "clippy", "test")
    )
    with tempfile.TemporaryDirectory() as tmp:
        status, output = drive(steps, Path(tmp))

    assert status == 1, (status, output[-2000:])
    summary = output.split("\ngates FAILED:\n", 1)[1]
    size = len(summary.encode("utf-8"))
    assert size < 3072, f"the summary of three red steps is {size} bytes"

    blocks = summary_blocks(output)
    assert list(blocks) == ["fake: fmt", "fake: clippy", "fake: test"], list(blocks)
    for name, evidence in blocks.items():
        assert evidence and evidence[-1].startswith(name.split(": ")[1] + " final "), (name, evidence)
        assert all(len(line) <= 200 for line in evidence), (name, [len(line) for line in evidence])

    # And the property the whole change is for, read the way the daemon reads it.
    tail = output.encode("utf-8")[-TAIL:].decode("utf-8", errors="replace")
    for name in ("fmt", "clippy", "test"):
        assert f"  fake: {name}\n" in tail, f"fake: {name} is not in the last {TAIL} bytes"
        assert f"      {name} final y" in tail, f"fake: {name}'s last line is not in the last {TAIL} bytes"


def test_a_green_run_says_so_and_exits_zero() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        status, output = drive('run "fake: fine" . true', Path(tmp))
    assert status == 0, (status, output)
    assert "\nok   fake: fine\n" in output and output.endswith("\nall gates green.\n"), output
    assert "gates FAILED" not in output, output


test_a_red_steps_own_lines_follow_its_name()
test_three_red_steps_fit_well_inside_the_daemons_tail()
test_a_green_run_says_so_and_exits_zero()
print("gates summary: ok")
