#!/usr/bin/env python3
"""The speed profile resolver: `normal | fast | thorough` -> SpeedDecision.

A pure function over the table in `<project>/.nucleos/workflow/speed.yaml`. Callers never interpret the
word `fast` themselves; they read the decision. `speed-cases.yaml`, beside this script, is the fixture.

Capacity is not this table's business. A harness that runs sessions in parallel owns how many,
and hands its answer down in two neutral variables: `WORKFLOW_SPEED` (the speed it launched the
session at, used when `--speed` is absent) and `WORKFLOW_PARALLEL_CEILING` (the most parallel
work it will carry). This file only ever narrows to that ceiling -- it never computes, multiplies
or raises one, so a run with no harness above it simply has no ceiling to apply.

CLI (what a controller runs):
  python <bundle_dir>/scripts/resolve_speed.py --speed fast --size medium [--elevated] [--tdd-able]
         [--team-max-parallel N] [--max-workers N] [--table PATH]
  env: WORKFLOW_SPEED (default for --speed), WORKFLOW_PARALLEL_CEILING (upper bound)
Prints the decision as JSON, plus `config_error` when the table or the value was bad --
in which case the decision is `normal`'s, never a guess (section 6).
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass
from pathlib import Path

import yaml

import nucleos_paths


def default_table() -> Path:
    """The project's materialized speed table (needs no project.json, only the tree)."""
    return nucleos_paths.workflow_dir() / "speed.yaml"


SPEEDS = ("normal", "fast", "thorough")
# The codex-safe subset. `max` is claude-only and the execute phase runs on codex.
EFFORTS = ("low", "medium", "high", "xhigh")
SIZES = ("trivial", "small", "medium", "large")
FIELDS = ("reasoning_effort", "fast_path", "review", "wave")
# What `normal` must be, field for field: the configuration before this existed.
TODAY = {"reasoning_effort": "medium", "fast_path": "heuristic", "review": "rule6",
         "wave": False}


class SpeedError(ValueError):
    """The table could not be read. Callers resolve `normal` and report this."""


@dataclass(frozen=True)
class SpeedDecision:
    workers: int
    reasoning_effort: str
    fast_path_allowed: bool
    review_required: bool
    team_max_parallel: int | None
    degraded: tuple[str, ...]

    def as_dict(self) -> dict:
        return {
            "workers": self.workers,
            "reasoning_effort": self.reasoning_effort,
            "fast_path_allowed": self.fast_path_allowed,
            "review_required": self.review_required,
            "team_max_parallel": self.team_max_parallel,
            "degraded": list(self.degraded),
        }


def load_table(path: Path | None = None) -> dict:
    path = path or default_table()
    if not path.exists():
        raise SpeedError(f"{path} not found")
    try:
        table = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    except yaml.YAMLError as error:
        raise SpeedError(f"{path.name} is not readable YAML: {error}") from error
    problems = validate_table(table)
    if problems:
        raise SpeedError(f"{path.name}: " + "; ".join(problems))
    return table


def validate_table(table: dict) -> list[str]:
    """Every problem with the table, as sentences. Empty means coherent."""
    problems = []
    profiles = table.get("profiles") or {}
    if set(profiles) != set(SPEEDS):
        problems.append(f"profiles are {sorted(profiles)}, expected {sorted(SPEEDS)}")
    if table.get("default") != "normal":
        problems.append(f"default is {table.get('default')!r}; it must be `normal`")
    stale = sorted({"team_ceiling", "team_multiplier"} & set(table))
    if stale:
        problems.append(f"{stale} belong to the harness, not this table")
    for name in SPEEDS:
        profile = profiles.get(name)
        if not isinstance(profile, dict):
            continue
        missing = [field for field in FIELDS if field not in profile]
        extra = sorted(set(profile) - set(FIELDS))
        if missing:
            problems.append(f"profile `{name}` is missing {missing}")
        if extra:
            problems.append(f"profile `{name}` has unknown fields {extra}")
        if missing:
            continue
        if profile["reasoning_effort"] not in EFFORTS:
            problems.append(f"profile `{name}` effort {profile['reasoning_effort']!r} is not "
                            f"in the codex-safe set {EFFORTS}")
        if profile["fast_path"] not in ("heuristic", "forbidden"):
            problems.append(f"profile `{name}` fast_path must be heuristic | forbidden")
        if profile["review"] not in ("rule6", "always"):
            problems.append(f"profile `{name}` review must be rule6 | always")
        if "tests" in profile:
            problems.append(f"profile `{name}` has a `tests` key: test runs are not a speed setting")
        if not isinstance(profile["wave"], bool):
            problems.append(f"profile `{name}` wave must be a boolean")
    normal = profiles.get("normal")
    if isinstance(normal, dict) and {k: normal.get(k) for k in TODAY} != TODAY:
        problems.append(f"`normal` is no longer today's behaviour: {normal} != {TODAY}")
    return problems


def parse_speed(value: str | None, table: dict) -> tuple[str, str | None]:
    """(speed, error). Absent is the default; unknown is `normal` plus a reason."""
    if value is None or value == "":
        return table.get("default", "normal"), None
    if value in SPEEDS:
        return value, None
    return "normal", f"speed {value!r} is not one of {SPEEDS}; resolved `normal`"


def parse_ceiling(value: str | None) -> tuple[int | None, str | None]:
    """(ceiling, error). Absent means no harness set one; a bad value is ignored and said."""
    if value is None or value.strip() == "":
        return None, None
    try:
        ceiling = int(value)
    except ValueError:
        ceiling = 0
    if ceiling < 1:
        return None, f"WORKFLOW_PARALLEL_CEILING {value!r} is not an integer >= 1; ignored"
    return ceiling, None


def resolve(table: dict, speed: str, size: str, elevated: bool, tdd_able: bool,
            team_max_parallel: int | None = None, max_workers: int = 1,
            parallel_ceiling: int | None = None) -> SpeedDecision:
    profile = table["profiles"][speed]
    degraded: list[str] = []
    medium_or_large = size in ("medium", "large")

    workers = 1
    if profile["wave"]:
        # Section 4.1: waves exist only for medium/large TDD-able work.
        if medium_or_large and tdd_able:
            workers = max(1, max_workers)
            if parallel_ceiling is not None:
                workers = min(workers, parallel_ceiling)
        else:
            degraded.append("task_not_decomposable")

    # The team's width is the harness's number, passed through: speed never creates capacity
    # (invariant II), so the only thing done to it here is the ceiling, when one was handed down.
    team = None
    if team_max_parallel is not None:
        team = max(1, team_max_parallel)
        if parallel_ceiling is not None:
            team = min(team, parallel_ceiling)

    return SpeedDecision(
        workers=workers,
        reasoning_effort=profile["reasoning_effort"],
        fast_path_allowed=profile["fast_path"] != "forbidden",
        # Rule 6 is the floor under every profile; `always` only adds to it.
        review_required=profile["review"] == "always" or elevated or medium_or_large,
        team_max_parallel=team,
        degraded=tuple(degraded),
    )


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--speed", default=os.environ.get("WORKFLOW_SPEED"))
    parser.add_argument("--size", choices=SIZES, required=True)
    parser.add_argument("--elevated", action="store_true")
    parser.add_argument("--tdd-able", action="store_true")
    parser.add_argument("--team-max-parallel", type=int)
    parser.add_argument("--max-workers", type=int, default=1)
    parser.add_argument("--table", type=Path, default=None)
    args = parser.parse_args(argv)

    errors = []
    try:
        table = load_table(args.table)
    except SpeedError as error:
        errors.append(str(error))
        table = {"default": "normal", "profiles": {"normal": dict(TODAY)}}
    speed, error = parse_speed(args.speed, table)
    if error:
        errors.append(error)
    if speed not in table["profiles"]:
        speed = "normal"
    ceiling, error = parse_ceiling(os.environ.get("WORKFLOW_PARALLEL_CEILING"))
    if error:
        errors.append(error)
    decision = resolve(table, speed, args.size, args.elevated, args.tdd_able,
                       args.team_max_parallel, args.max_workers, ceiling).as_dict()
    decision["speed"] = speed
    if errors:
        decision["config_error"] = "; ".join(errors)
        print(f"resolve_speed: {decision['config_error']}", file=sys.stderr)
    print(json.dumps(decision))
    return 0


if __name__ == "__main__":
    sys.exit(main())
