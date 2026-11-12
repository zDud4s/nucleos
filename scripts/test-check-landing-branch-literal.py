#!/usr/bin/env python3
"""Cover `scripts/check-landing-branch-literal.py`: no stray literal "master" for the landing branch.

The checker scans `scripts/` and `.nucleos/workflow/` under `--root` (default: the repository) and
exits 1, printing one `path:line: text` per hit, when a line names `master` outside an explicit
exceptions list. Exceptions: test files (`test-*.py`, `heavy_tests/`), comment-only lines, and the
audited literals kept on purpose (the broker's master ref, the installer's branch fallback).

Temp trees only, plus one pass over the real repository.

Run:  python scripts/test-check-landing-branch-literal.py
"""

import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts" / "check-landing-branch-literal.py"

failures = []


def check(name, condition, detail=""):
    if not condition:
        failures.append(f"{name}: {detail}")


def run_checker(root):
    return subprocess.run(
        [sys.executable, str(CHECKER), "--root", str(root)], capture_output=True, text=True
    )


def write(root, rel, text):
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8", newline="\n")


def test_flags_a_stray_literal():
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        write(root, "scripts/stray.sh", "#!/bin/sh\ngit merge --ff-only master\n")
        write(root, ".nucleos/workflow/stray.md", "Land on master with `git push origin master`.\n")
        done = run_checker(root)
        check("stray: exits non-zero", done.returncode != 0, done.stdout + done.stderr)
        check("stray: names the script", "scripts/stray.sh:2" in done.stdout.replace("\\", "/"), done.stdout)
        check("stray: names the workflow doc", ".nucleos/workflow/stray.md:1" in done.stdout.replace("\\", "/"), done.stdout)

        # A tree with no literal at all is clean.
        clean = Path(raw) / "clean"
        write(clean, "scripts/fine.sh", "#!/bin/sh\ngit merge --ff-only \"$LANDING_BRANCH\"\n")
        done = run_checker(clean)
        check("clean tree: exits zero", done.returncode == 0, done.stdout + done.stderr)


def test_exceptions_pass():
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        # Test files may name master freely.
        write(root, "scripts/test-thing.py", 'git("init", "-b", "master")\n')
        write(root, "scripts/heavy_tests/test_x.py", 'git(repo, "init", "-q", "-b", "master")\n')
        # Comment-only lines are prose, not a landing-branch decision.
        write(root, "scripts/gates.sh", "# master's staged copy is the broker\necho ok\n")
        # Audited literals kept on purpose.
        write(
            root,
            "scripts/heavy.py",
            'ref = os.environ.get("NUCLEOS_HEAVY_MASTER_REF") or "master"\n',
        )
        write(
            root,
            "scripts/install-hooks.sh",
            "  if git show-ref --verify --quiet refs/heads/master; then\n    branch=\"master\"\n",
        )
        write(root, ".nucleos/workflow/workflow.md", "7. Shared problems: a red master (`master-red`).\n")
        done = run_checker(root)
        check("exceptions: exit zero", done.returncode == 0, done.stdout + done.stderr)

    # The real repository passes once every hit is fixed or listed.
    done = run_checker(ROOT)
    check("real repo: exits zero", done.returncode == 0, done.stdout + done.stderr)


if __name__ == "__main__":
    for name, test in list(globals().items()):
        if name.startswith("test_") and callable(test):
            test()
    if failures:
        print("\n".join(failures))
        sys.exit(1)
    print("check-landing-branch-literal: ok")
