#!/usr/bin/env python3
"""Tests for `resolve_speed.py` -- the speed profile resolver.

Run:  python test_resolve_speed.py

The parity fixture `speed-cases.yaml` (beside this file) is the same file core/src/speed.rs
reproduces; a case added there is a case both resolvers must answer identically.
"""
from __future__ import annotations

import copy
import json
import subprocess
import sys
from pathlib import Path

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parent))

import resolve_speed as rs  # noqa: E402

FAILURES: list[str] = []
HERE = Path(__file__).resolve().parent
CASES = HERE / "speed-cases.yaml"
# The table the bundle ships; a project gets the same file under `.nucleos/workflow/`.
TABLE = HERE.parent / ".nucleos" / "workflow" / "speed.yaml"


def check(name: str, condition: bool, detail: str = "") -> None:
    if condition:
        print(f"  ok   {name}")
    else:
        print(f"  FAIL {name}: {detail}")
        FAILURES.append(f"{name}: {detail}")


def main() -> int:
    table = rs.load_table(TABLE)

    # 1 -- the live table is coherent. Mutation: change normal's effort to `low`.
    problems = rs.validate_table(table)
    check("the live table validates", problems == [], "; ".join(problems))

    # 2 -- normal is today. Mutation: drop `normal` from TODAY's comparison in validate_table.
    drifted = copy.deepcopy(table)
    drifted["profiles"]["normal"]["reasoning_effort"] = "low"
    check("a drifted normal is refused",
          any("today" in p for p in rs.validate_table(drifted)),
          f"{rs.validate_table(drifted)}")

    # 3 -- max is claude-only and the execute phase runs on codex (section 3.3).
    maxed = copy.deepcopy(table)
    maxed["profiles"]["thorough"]["reasoning_effort"] = "max"
    check("`max` effort is refused as not codex-safe",
          any("codex" in p for p in rs.validate_table(maxed)), f"{rs.validate_table(maxed)}")

    # 4 -- a profile missing a field would make a SpeedDecision field write-only.
    holed = copy.deepcopy(table)
    del holed["profiles"]["fast"]["wave"]
    check("a profile missing a field is refused",
          any("fast" in p and "wave" in p for p in rs.validate_table(holed)),
          f"{rs.validate_table(holed)}")

    # 4b -- test runs are not a speed setting: a `tests` key is refused.
    loose = copy.deepcopy(table)
    loose["profiles"]["fast"]["tests"] = "batched"
    check("a tests key is refused",
          any("tests" in p for p in rs.validate_table(loose)), f"{rs.validate_table(loose)}")

    # 5 -- every fixture case, field by field.
    cases = yaml.safe_load(CASES.read_text(encoding="utf-8"))["cases"]
    for entry in cases:
        given = entry["input"]
        decision = rs.resolve(
            table, given["speed"], given["size"], given["elevated"], given["tdd_able"],
            given["team_max_parallel"], given["max_workers"], given.get("parallel_ceiling"),
        )
        check(f"fixture: {entry['name']}", decision.as_dict() == entry["expected"],
              f"got {decision.as_dict()}")

    # 6 -- unknown and absent values. Configuration is never guessed (section 6).
    check("an absent speed is the table default", rs.parse_speed(None, table) == ("normal", None))
    speed, error = rs.parse_speed("fsat", table)
    check("an unknown speed resolves normal and says why",
          speed == "normal" and error is not None and "fsat" in error, f"{speed!r} {error!r}")

    # 7 -- the CLI a controller calls. A missing table resolves normal, exits 0 and says so.
    completed = subprocess.run(
        [sys.executable, str(Path(__file__).with_name("resolve_speed.py")),
         "--speed", "fast", "--size", "medium", "--tdd-able", "--max-workers", "2",
         "--table", str(HERE / "no-such-speed.yaml")],
        capture_output=True, text=True,
    )
    output = json.loads(completed.stdout or "{}")
    check("a missing table resolves normal through the CLI",
          completed.returncode == 0 and output.get("reasoning_effort") == "medium"
          and "not found" in (output.get("config_error") or ""),
          f"code={completed.returncode} out={completed.stdout[:200]} err={completed.stderr[:200]}")

    print(f"\ntest_resolve_speed: {'FAIL' if FAILURES else 'ok'} ({len(FAILURES)} failure(s))")
    return 1 if FAILURES else 0


if __name__ == "__main__":
    sys.exit(main())
