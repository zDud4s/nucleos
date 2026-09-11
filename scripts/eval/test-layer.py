#!/usr/bin/env python3
"""Drive `layer.py` against a tree shaped like the ones `base.sh` lays out.

Wired into `scripts/gates.sh` as `eval: layer`. Also runs standalone:
`python scripts/eval/test-layer.py`.

The fake tree carries only what `layer.py` reads — `.claude/settings.json` with the reference's
`PreToolUse` entry, the hook script it names, and the `.gitignore` the archive brings — because the
property under test is what this script writes into a tree, not what cargo later makes of it.

What is under test here is new as of the daemon's project gate (`a840181`, 2026-08-26): a worktree
cell now needs its project in `shadow`, and `activation_prerequisites` refuses a root with no
`.ai/workflow/workflow.md`. `.ai/` is gitignored, so no tree `base.sh` produces has one. This file
holds `layer.py` to planting it where activation looks and nowhere the agent does.
"""

import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
PLANTED = os.path.join(".ai", "workflow", "workflow.md")

# The entry every reference commit carries, as `classifier_hook_is_wired` recognises it.
SETTINGS = {
    "hooks": {
        "PreToolUse": [{
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "command": 'python "${CLAUDE_PROJECT_DIR}/.claude/hooks/ask_daemon.py"',
            }],
        }],
    },
}


def lay_out(work, layer):
    """A tree as `base.sh` would leave it, then put into `layer` by the real `layer.py`."""
    tree = os.path.join(work, layer)
    os.makedirs(os.path.join(tree, ".claude", "hooks"))
    with open(os.path.join(tree, ".claude", "settings.json"), "w", encoding="utf-8") as handle:
        json.dump(SETTINGS, handle)
    with open(os.path.join(tree, ".claude", "hooks", "ask_daemon.py"), "w", encoding="utf-8") as handle:
        handle.write("#\n")
    with open(os.path.join(tree, ".gitignore"), "w", encoding="utf-8") as handle:
        handle.write(".ai/\ntarget/\n")
    with open(os.path.join(tree, "README.md"), "w", encoding="utf-8") as handle:
        handle.write("base\n")
    done = subprocess.run(
        [sys.executable, os.path.join(HERE, "layer.py"), "--layer", layer, "--tree", tree],
        capture_output=True, text=True,
    )
    return tree, done


def main():
    failures = 0

    def check(label, condition):
        nonlocal failures
        print(("ok   " if condition else "FAIL ") + label)
        failures += 0 if condition else 1

    with tempfile.TemporaryDirectory() as work:
        trees = {layer: lay_out(work, layer) for layer in ("H0", "H1", "H2", "H3")}

        for layer, (_, done) in trees.items():
            if done.returncode != 0:
                print(f"     {layer}: layer.py exited {done.returncode}: {done.stderr.strip()[-300:]}")
        check("layer.py still finishes cleanly for every layer",
              all(done.returncode == 0 for _, done in trees.values()))

        planted = {layer: os.path.join(tree, PLANTED) for layer, (tree, _) in trees.items()}
        check("H2 plants the file the daemon's activation checks for",
              os.path.isfile(planted["H2"]))
        check("H3 plants it as well", os.path.isfile(planted["H3"]))
        check("H0 plants nothing — a real-mode run is never activated",
              not os.path.exists(planted["H0"]))
        check("H1 plants nothing — without the hook no file makes it activatable",
              not os.path.exists(planted["H1"]))

        # It exists to satisfy one `is_file()` check, and it must say so to whoever opens it rather
        # than pass itself off as the workflow the check is there to confirm.
        content = open(planted["H2"], encoding="utf-8").read() if os.path.isfile(planted["H2"]) else ""
        check("the planted file names what planted it and does not pretend to be the workflow",
              "scripts/eval/layer.py" in content and "# AI workflow" not in content)

        # Activation reads the project ROOT; the agent works in a worktree the daemon checks out
        # from HEAD. Gitignored means the file is in the first and never in the second.
        tracked = subprocess.run(["git", "-C", trees["H2"][0], "ls-files", ".ai"],
                                 capture_output=True, text=True).stdout.strip()
        check("the planted file never reaches the commit the daemon checks out", tracked == "")

    total = 7
    print(f"\n{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
