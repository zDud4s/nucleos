#!/usr/bin/env python3
"""What `review_gate.py` refuses, and what it must not.

Every case is a whole packet rather than a fragment, for the reason the sibling
suite gives: the gate reads a packet's SHAPE, and a fixture that is not one can
pass a check the real thing would fail.
"""

from __future__ import annotations

import hashlib
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GATE = ROOT / "scripts" / "review_gate.py"

failures: list[str] = []


def packet(
    verdict: str = "approve",
    gates_met: bool = True,
    regressions: str = "none",
    missing_tests: str = "none",
    limitations: str | None = "none",
    plan_path: str = "none",
    plan_sha: str = "none",
) -> str:
    box = "x" if gates_met else " "
    lines = [
        "# Review Packet",
        "Task ID: a-slug",
        "Plan summary: one sentence",
        "Risk level: low",
        f"Plan path: {plan_path}",
        f"Plan sha256: {plan_sha}",
        "Files changed: core/src/job.rs",
        "Deviations from plan: none",
        "## Hard gates",
        f"- [{box}] Handoff section present and non-empty",
        f"- [{box}] Every evidence block shows `exit: 0` or accepted skip",
        "## Review Checklist",
        "- [x] Scope respected; no unrelated changes",
        f"Potential regressions: {regressions}",
        f"Missing tests: {missing_tests}",
        "Simpler alternative: none",
        "Memory updates to apply: none",
    ]
    if limitations is not None:
        lines.append(f"Limitations: {limitations}")
    lines += [f"Verdict: {verdict}", "Recommendation: merge it"]
    return "\n".join(lines) + "\n"


def run(text: str) -> tuple[int, str]:
    done = subprocess.run(
        [sys.executable, str(GATE)],
        input=text,
        capture_output=True,
        text=True,
    )
    return done.returncode, done.stderr


def case(name: str, text: str, want_code: int, want_says: str = "") -> None:
    code, err = run(text)
    if code != want_code:
        failures.append(f"{name}: exit {code}, wanted {want_code}\n    {err.strip()}")
    elif want_says and want_says not in err:
        failures.append(f"{name}: message missing {want_says!r}\n    {err.strip()}")


# -------------------------------------------------- the contradiction itself

case("a clean approval passes", packet(), 0)

case(
    "approve beside an unmet hard gate is refused",
    packet(gates_met=False),
    1,
    "hard gate not met",
)
case(
    "approve beside a named regression is refused",
    packet(regressions="the retry budget can now go negative"),
    1,
    "Potential regressions",
)
case(
    "approve beside an unjustified coverage gap is refused",
    packet(missing_tests="nothing covers the migration path"),
    1,
    "Missing tests",
)

# The same findings under an honest verdict are the packet working, not failing.
# A gate that refused these would be refusing reviews for doing their job, and
# would teach a reviewer to delete findings rather than to change the verdict --
# which is the exact failure it exists to prevent, arriving from the other side.
for verdict in ("request-changes", "escalate"):
    case(
        f"{verdict} carrying the same findings is accepted",
        packet(verdict=verdict, gates_met=False, regressions="a real one"),
        0,
    )

# ------------------------------------------------------------- limitations

case(
    "a review that never says what it could not inspect is refused",
    packet(limitations=None),
    1,
    "unbounded claim",
)
case("`none` is an answer and passes", packet(limitations="none"), 0)
case(
    "a real limitation passes and does not itself block",
    packet(limitations="could not run the shell gate; the disk was full"),
    0,
)

# ----------------------------------------------------------- the plan's bytes

with tempfile.TemporaryDirectory(dir=ROOT) as tmp:
    plan = Path(tmp) / "plan.md"
    plan.write_text("the smallest safe plan\n", encoding="utf-8")
    where = plan.relative_to(ROOT).as_posix()
    pinned = hashlib.sha256(plan.read_bytes()).hexdigest()

    case(
        "a plan whose bytes still match is silent",
        packet(plan_path=where, plan_sha=pinned),
        0,
    )

    plan.write_text("the plan, quietly rewritten after approval\n", encoding="utf-8")
    case(
        "a plan edited after approval invalidates the review",
        packet(plan_path=where, plan_sha=pinned),
        1,
        "no longer exists",
    )

    case(
        "a pinned plan that has been deleted is refused, not ignored",
        packet(plan_path="does/not/exist.md", plan_sha=pinned),
        1,
        "cannot be read",
    )

# `none` on both is the ordinary small task, and must stay quiet -- otherwise the
# field becomes noise on every packet that has no file to pin.
case("a task with no persisted plan is not asked about one", packet(), 0)

# ------------------------------------------------- unwritten vs damaged

case(
    "a packet still carrying the schema's verdict comment is 'not yet authored'",
    packet(verdict="<!-- approve | request-changes | escalate -->"),
    1,
    "not yet authored",
)
case(
    "a packet still carrying the schema's limitations comment is too",
    packet(limitations="<!-- what this review could NOT inspect -->"),
    1,
    "not yet authored",
)

# A verdict somebody wrote and got wrong is a DIFFERENT reader's problem from one
# nobody wrote, so it is a different exit code and a different sentence.
case("a verdict outside the vocabulary is malformed", packet(verdict="lgtm"), 2, "not one of")
case("a packet with no verdict at all is malformed", packet().replace("Verdict: approve\n", ""), 2, "missing Verdict")
case("an empty packet is malformed", "", 2, "empty packet")

# ------------------------------------------------------------------- shape

# A line shaped like a field, inside the body, must not be read as the header's.
# A regression described in the reviewer's own words is the likeliest place for
# one, and mistaking it for the verdict would flip the gate's whole answer.
case(
    "a field-shaped sentence in the body is not mistaken for the verdict",
    packet(verdict="request-changes", regressions="Verdict: approve was wrong here"),
    0,
)

if failures:
    print("review gate tests FAILED:", file=sys.stderr)
    for line in failures:
        print(f"  - {line}", file=sys.stderr)
    raise SystemExit(1)
print("review gate tests passed")
