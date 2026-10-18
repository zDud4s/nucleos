#!/usr/bin/env python3
"""Independent check of a planner's Size/Risk claim.

Called by the orchestrate skill at the end of Phase 1:

    python <bundle_dir>/scripts/triage_check.py --files <relevant files> \
        --claim-size <size> --claim-risk <risk>

Exit 0  the claim is at least as strict as what the boundaries imply.
Exit 1  the claim is too weak. The orchestrator adopts the stricter axis
        (promote Size and/or elevate Risk, NEVER downgrade) and logs
        `triage: promoted` (a `record` event).
Exit 2  usage / configuration error.

The two axes are independent and neither collapses into the other:
Size controls plan complexity, Risk controls whether review is mandatory.
Both definitions are the planner skill's; this file is a second opinion on
the same rules, not a second set of rules. When they disagree, the planner
skill (`.claude/skills/planner/SKILL.md`) is the source of truth and this
file is the bug.

Why a checker exists at all: the planner states its own Size and Risk, and
a planner that under-calls either one skips the review that would have
caught it. Somebody other than the claimant has to look.
"""

from __future__ import annotations

import argparse
import fnmatch
import sys
from pathlib import Path

import yaml

import nucleos_paths

# Set by main(): the project root (boundary paths resolve against it) and `<state_dir>/project.yaml`.
REPO_ROOT = nucleos_paths.project_root()
PROJECT_YAML = REPO_ROOT / "project.yaml"

# Order matters: index in this list IS the strictness ordering.
SIZES = ["trivial", "small", "medium", "large"]
RISKS = ["low", "elevated"]

# The boundary lists that make a change risky. `generated_files` is deliberately
# absent — touching a build artifact is noise, not risk.
RISK_LISTS = ("risky_areas", "security_sensitive", "migration_sensitive", "do_not_touch")


def normalize(path: str) -> str:
    """One spelling for one file.

    Windows hands over backslashes, planners write `./core/src/x.rs` about as
    often as `core/src/x.rs`, and a prefix comparison between two spellings of
    the same path silently answers "no match" — which reads exactly like "not
    risky". Every comparison in this file runs on the output of this function.
    """
    cleaned = path.strip().replace("\\", "/")
    while cleaned.startswith("./"):
        cleaned = cleaned[2:]
    return cleaned.lstrip("/")


def matches(file_path: str, pattern: str) -> bool:
    """Prefix OR glob, which is what `project.yaml` says its lists are."""
    pattern = normalize(pattern)
    if not pattern:
        return False
    if any(ch in pattern for ch in "*?["):
        # A bare `*.db` must match `core/data/x.db`, not just a top-level file.
        return fnmatch.fnmatch(file_path, pattern) or fnmatch.fnmatch(
            Path(file_path).name, pattern
        )
    if pattern.endswith("/"):
        return file_path.startswith(pattern)
    return file_path == pattern or file_path.startswith(pattern + "/")


def looks_like_prose(entry: str) -> bool:
    """`project.yaml` warns about this in its own comment:

        # Matched by PREFIX or GLOB against a task's `Relevant files`, so paths
        # only. Prose here matches nothing and silently disables the Risk gate.

    An entry with a space and no path or glob punctuation is a sentence
    somebody wrote where a path belongs. It matches nothing, forever, quietly.
    """
    entry = entry.strip()
    return " " in entry and not any(ch in entry for ch in "/*?[")


def audit_boundaries(boundaries: dict) -> list[str]:
    """Warnings about the lists themselves, printed on every run.

    A boundary entry naming a file that no longer exists protects nothing, and
    nothing else in the pipeline ever notices: the Risk gate just stops firing
    for that path. `core/src/mailfiles.rs` sat in two lists for weeks after the
    module was renamed to `files.rs`, and the gate read green the whole time.
    """
    warnings: list[str] = []
    for list_name in RISK_LISTS:
        for entry in boundaries.get(list_name) or []:
            if not isinstance(entry, str):
                continue
            if looks_like_prose(entry):
                warnings.append(
                    f"{list_name}: {entry!r} reads as prose, not a path — it matches nothing"
                )
                continue
            if any(ch in entry for ch in "*?["):
                continue  # a glob need not resolve today
            target = REPO_ROOT / normalize(entry)
            if not target.exists():
                warnings.append(
                    f"{list_name}: {entry!r} does not exist — this boundary protects nothing"
                )
    return warnings


def derive_risk(files: list[str], boundaries: dict) -> tuple[str, list[str]]:
    """Elevated if ANY file matches ANY risk list. Reports every match, not the
    first: a report naming one file when three are risky invites fixing one."""
    hits: list[str] = []
    for file_path in files:
        for list_name in RISK_LISTS:
            for entry in boundaries.get(list_name) or []:
                if isinstance(entry, str) and matches(file_path, entry):
                    hits.append(f"{file_path} -> {list_name}:{entry}")
    return ("elevated" if hits else "low"), hits


def derive_size(files: list[str]) -> str:
    """The FLOOR that a file count alone can justify.

    A path list cannot see line counts, so it cannot tell the planner's
    `trivial` (1 file, <10 lines) from its `small` (1 file, >=10 lines). This
    returns the weakest size consistent with the count, and the caller only
    ever promotes — so a planner that says `small` about one file is believed,
    and a planner that says `trivial` about eight files is not.
    """
    count = len(files)
    if count <= 1:
        return "trivial"
    if count <= 3:
        return "small"
    if count <= 10:
        return "medium"
    return "large"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--files", nargs="*", default=[], help="the planner's Relevant files")
    parser.add_argument("--claim-size", required=True, choices=SIZES)
    parser.add_argument("--claim-risk", required=True, choices=RISKS)
    args = parser.parse_args()

    global REPO_ROOT, PROJECT_YAML
    try:
        REPO_ROOT = nucleos_paths.project_root()
        PROJECT_YAML = nucleos_paths.state_dir(REPO_ROOT) / "project.yaml"
    except nucleos_paths.ProjectNotFound as error:
        print(f"triage_check: {error}", file=sys.stderr)
        return 2
    if not PROJECT_YAML.exists():
        print(f"triage_check: {PROJECT_YAML} not found", file=sys.stderr)
        return 2
    try:
        project = yaml.safe_load(PROJECT_YAML.read_text(encoding="utf-8")) or {}
    except yaml.YAMLError as error:
        print(f"triage_check: {PROJECT_YAML} is not readable YAML: {error}", file=sys.stderr)
        return 2

    boundaries = project.get("boundaries") or {}
    if not boundaries:
        # Fail loud rather than answering `low` for everything. A missing
        # boundaries block is a Risk gate that is off, not a repo with no risk.
        print("triage_check: project.yaml has no `boundaries` block", file=sys.stderr)
        return 2

    for warning in audit_boundaries(boundaries):
        print(f"warning: {warning}", file=sys.stderr)

    files = [normalize(f) for f in args.files if f.strip()]
    derived_risk, hits = derive_risk(files, boundaries)
    derived_size = derive_size(files)

    # The planner's own rule: an elevated trivial is promoted to small, because
    # trivial emits a one-line instruction and no packet — and an elevated
    # change with no packet is an elevated change nobody reviewed.
    if derived_risk == "elevated" and derived_size == "trivial":
        derived_size = "small"

    adopt_size = max(args.claim_size, derived_size, key=SIZES.index)
    adopt_risk = max(args.claim_risk, derived_risk, key=RISKS.index)

    print(f"files={len(files)}")
    print(f"claim_size={args.claim_size} derived_size={derived_size} adopt_size={adopt_size}")
    print(f"claim_risk={args.claim_risk} derived_risk={derived_risk} adopt_risk={adopt_risk}")
    for hit in hits:
        print(f"risk_match={hit}")

    if adopt_size != args.claim_size or adopt_risk != args.claim_risk:
        print("verdict=promote", file=sys.stderr)
        return 1
    print("verdict=ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
