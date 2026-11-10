#!/usr/bin/env python3
"""Move a repo's retired `.ai/` agent material into `<state_dir>` and archive the rest.

Spec 2026-11-09-migracao-ai-para-state-dir-design, D1, D4 and section 4:

* `specs|plans|notes|spikes|handoffs/**` move to `<state_dir>/<same rel>`; an identical copy
  already there is archived as a duplicate, a different one is a collision.
* `memory-archive.md` and `memory-retractions.jsonl` at the `.ai` root follow the same rule
  against `<state_dir>/<name>`, except that a different copy is archived with its diff printed.
* `memory.md` and `decisions.md` are always archived, never merged; a unified diff against the
  `<state_dir>` copy is printed so the owner can carry entries over by hand.
* Everything else is archived under `<archive>/<same rel>`.
* A symlink or junction is refused, never followed. An existing archive destination is a
  collision. Any collision or refusal: everything is reported and nothing changes.

Dry-run is the default. `--apply` never deletes a file and never overwrites anything; it only
removes the directories under `.ai` that the moves left empty. The owner runs it by hand with
the daemon stopped (the daemon scans specs and reads memory).

Run:  python scripts/migrate-ai-layout.py [--repo PATH] [--state-dir PATH] [--archive PATH] [--apply]
"""

import argparse
import difflib
import json
import os
import shutil
import sys

WARNING = "stop the daemon before --apply (it scans specs and reads memory)"
DRY_RUN_LAST = "dry run: nothing changed; re-run with --apply"
ARCHIVE_NAME = "ai-layout-2026-11-09"
DEFAULT_DEV_HOME = "C:\\nucleos-dev"

MOVED_DIRS = ("specs", "plans", "notes", "spikes", "handoffs")
ROOT_LOGS = ("memory-archive.md", "memory-retractions.jsonl")
DIFF_ONLY = ("memory.md", "decisions.md")


class Action:
    """One planned step. `verb` is `move`, `archive` or `refuse`."""

    def __init__(self, verb, src, dst, note=None, diff=None):
        self.verb = verb
        self.src = src
        self.dst = dst
        self.note = note
        self.diff = diff or []

    def line(self):
        text = f"{self.verb} {self.src} -> {self.dst}"
        return f"{text} ({self.note})" if self.note else text


def _is_link(path):
    if os.path.islink(path):
        return True
    isjunction = getattr(os.path, "isjunction", None)
    return bool(isjunction and isjunction(path))


def _same_bytes(a, b):
    with open(a, "rb") as fa, open(b, "rb") as fb:
        return fa.read() == fb.read()


def _read_text_lines(path):
    with open(path, "r", encoding="utf-8", errors="replace", newline="") as fh:
        return fh.read().splitlines()


def _diff(state_copy, ai_copy):
    """Unified diff from the `<state_dir>` copy to the `.ai` copy, as output lines."""
    return list(
        difflib.unified_diff(
            _read_text_lines(state_copy),
            _read_text_lines(ai_copy),
            fromfile=state_copy,
            tofile=ai_copy,
            lineterm="",
        )
    )


def _join(base, rel):
    return os.path.join(base, *rel.split("/"))


def _classify(src, rel, state_dir, archive):
    """The action for one regular file at `.ai/<rel>`."""
    archive_dst = _join(archive, rel)

    def to_archive(note=None, diff=None):
        if os.path.lexists(archive_dst):
            return Action("refuse", src, archive_dst, "collision: archive destination exists")
        return Action("archive", src, archive_dst, note, diff)

    top = rel.split("/", 1)[0]
    if "/" in rel and top in MOVED_DIRS or rel in ROOT_LOGS:
        state_dst = _join(state_dir, rel)
        if not os.path.lexists(state_dst):
            return Action("move", src, state_dst)
        if os.path.isfile(state_dst) and not _is_link(state_dst) and _same_bytes(src, state_dst):
            return to_archive("duplicate of the <state_dir> copy")
        if rel in ROOT_LOGS and os.path.isfile(state_dst) and not _is_link(state_dst):
            return to_archive("differs from the <state_dir> copy", _diff(state_dst, src))
        return Action("refuse", src, state_dst, "collision: different <state_dir> copy exists")

    if rel in DIFF_ONLY:
        state_copy = _join(state_dir, rel)
        if os.path.isfile(state_copy):
            return to_archive(None, _diff(state_copy, src))
        return to_archive(f"no <state_dir> copy of {rel}")

    return to_archive()


def build_plan(repo, state_dir, archive):
    """Classify every entry under `<repo>/.ai`. Pure: reads only, changes nothing."""
    ai = os.path.join(repo, ".ai")
    plan = []
    for dirpath, dirnames, filenames in os.walk(ai):
        dirnames.sort()
        kept = []
        for name in dirnames:
            full = os.path.join(dirpath, name)
            if _is_link(full):
                rel = os.path.relpath(full, ai).replace(os.sep, "/")
                plan.append(Action("refuse", full, _join(archive, rel), "symlink or junction"))
            else:
                kept.append(name)
        dirnames[:] = kept
        for name in sorted(filenames):
            full = os.path.join(dirpath, name)
            rel = os.path.relpath(full, ai).replace(os.sep, "/")
            if _is_link(full) or not os.path.isfile(full):
                plan.append(Action("refuse", full, _join(archive, rel), "symlink or not a file"))
                continue
            plan.append(_classify(full, rel, state_dir, archive))
    return plan


def apply(plan, ai):
    """Perform every move/archive, then prune the emptied directories under `ai`.

    Returns the paths still left under `ai` (empty when everything went).
    """
    for action in plan:
        if action.verb == "refuse":
            raise ValueError(f"refusing to apply a plan with a refusal: {action.line()}")
    for action in plan:
        if os.path.lexists(action.dst):
            raise FileExistsError(f"destination appeared since planning: {action.dst}")
        os.makedirs(os.path.dirname(action.dst), exist_ok=True)
        shutil.move(action.src, action.dst)
    for dirpath, _dirnames, _filenames in os.walk(ai, topdown=False):
        try:
            os.rmdir(dirpath)
        except OSError:
            pass
    left = []
    if os.path.lexists(ai):
        for dirpath, dirnames, filenames in os.walk(ai):
            left.extend(os.path.join(dirpath, n) for n in dirnames + filenames)
        if not left:
            left.append(ai)
    return left


def _state_dir_from_project(repo):
    path = os.path.join(repo, ".nucleos", "workflow", "project.json")
    try:
        with open(path, "r", encoding="utf-8") as fh:
            data = json.load(fh)
    except (OSError, ValueError):
        return None
    value = data.get("state_dir") if isinstance(data, dict) else None
    return value if isinstance(value, str) and value else None


def _default_archive():
    dev_home = os.environ.get("NUCLEOS_DEV_HOME") or DEFAULT_DEV_HOME
    return os.path.join(dev_home, "archive", ARCHIVE_NAME)


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    default_repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    parser.add_argument("--repo", default=default_repo)
    parser.add_argument("--state-dir", dest="state_dir")
    parser.add_argument("--archive")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args(argv)

    print(WARNING)
    repo = os.path.abspath(args.repo)
    state_dir = args.state_dir or _state_dir_from_project(repo)
    if not state_dir:
        print(
            "no state_dir: .nucleos/workflow/project.json is missing or unreadable; pass --state-dir"
        )
        return 2
    state_dir = os.path.abspath(state_dir)
    archive = os.path.abspath(args.archive) if args.archive else _default_archive()

    ai = os.path.join(repo, ".ai")
    if not os.path.isdir(ai):
        print("nothing to migrate")
        return 0

    plan = build_plan(repo, state_dir, archive)
    counts = {"move": 0, "archive": 0, "refuse": 0}
    for action in plan:
        counts[action.verb] += 1
        print(action.line())
        for line in action.diff:
            print(line)
    print(f"{counts['move']} move, {counts['archive']} archive, {counts['refuse']} refuse")

    if counts["refuse"]:
        print("refusals or collisions found: nothing changed")
        return 1
    if not args.apply:
        print(DRY_RUN_LAST)
        return 0

    left = apply(plan, ai)
    if left:
        print("left under .ai after apply:")
        for path in left:
            print(f"  {path}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
