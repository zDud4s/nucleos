#!/usr/bin/env python3
"""Flag a stray literal `master` naming the landing branch in `scripts/` and `.nucleos/workflow/`.

The landing branch is configurable, so a script or workflow doc must not hard-code it. Prints one
`path:line: text` per hit (path relative to --root, forward slashes) and exits 1 on any hit.

Usage:  python scripts/check-landing-branch-literal.py [--root DIR]
"""

import argparse
import fnmatch
import re
import sys
from pathlib import Path

LITERAL = re.compile(r"\bmaster\b")

SCAN_DIRS = ("scripts", ".nucleos/workflow")

# Files skipped wholesale (glob over the root-relative posix path).
EXCLUDED_FILES = [
    "scripts/test-*.py",  # tests may name master freely
    "scripts/heavy_tests/*",  # heavy-broker fixtures build throwaway repos on master
    "scripts/heavy.py",  # the broker's NUCLEOS_HEAVY_MASTER_REF default and its docstring
    "scripts/check-landing-branch-literal.py",  # this checker names the literal itself
]

# (file glob, content regex) pairs: a matching line in a matching file is an audited literal.
EXCLUDED_LINES = [
    # installer fallback when no landing branch is configured
    ("scripts/install-hooks.sh", re.compile(r"refs/heads/master")),
    ("scripts/install-hooks.sh", re.compile(r'branch="master"')),
    # Rule 7 speaks of a red master / `master-red`, the shared-problem label, not a branch decision
    (".nucleos/workflow/workflow.md", re.compile(r"master-red|red master")),
    # gate base default when NUCLEOS_GATE_BASE is unset
    ("scripts/gate-diff.sh", re.compile(r"NUCLEOS_GATE_BASE:-master")),
]


def excluded(rel, line):
    if line.lstrip().startswith("#"):  # comment-only lines are prose
        return True
    return any(fnmatch.fnmatch(rel, g) and rx.search(line) for g, rx in EXCLUDED_LINES)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=str(Path(__file__).resolve().parent.parent))
    root = Path(ap.parse_args().root)
    hits = []
    for d in SCAN_DIRS:
        base = root / d
        if not base.is_dir():
            continue
        for path in sorted(p for p in base.rglob("*") if p.is_file()):
            rel = path.relative_to(root).as_posix()
            if any(fnmatch.fnmatch(rel, g) for g in EXCLUDED_FILES):
                continue
            try:
                text = path.read_text(encoding="utf-8")
            except (UnicodeDecodeError, OSError):
                continue  # binary or unreadable: not a script
            for n, line in enumerate(text.splitlines(), 1):
                if LITERAL.search(line) and not excluded(rel, line):
                    hits.append(f"{rel}:{n}: {line.strip()}")
    if hits:
        print("\n".join(hits))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
