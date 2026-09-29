#!/usr/bin/env python3
"""Drive `layer.py` against a tree shaped like the ones `base.sh` lays out.

Wired into `scripts/gates.sh` as `eval: layer`. Also runs standalone:
`python scripts/eval/test-layer.py`.

The fake tree carries only what `layer.py` reads — `.claude/settings.json` with the reference's
`PreToolUse` entry, the hook script it names, and the `.gitignore` the archive brings — because the
property under test is what this script writes into a tree, not what cargo later makes of it.

What is under test here changed on 2026-09-27. A worktree cell needs its project in `shadow`, and
the daemon no longer asks for `.ai/workflow/workflow.md` to allow that: it asks for the project to
have been onboarded, which it records in its own state directory. So `layer.py` must plant nothing
under `.ai/` any more — not that file, not H3's gate in `.ai/autopilot.yaml`, which the daemon stopped
reading — and `ladder.py` must send the onboarding, with H3's gate and nobody else's.
"""

import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
# What layer.py used to write and must not any more: the daemon reads neither.
RETIRED = (os.path.join(".ai", "workflow", "workflow.md"), os.path.join(".ai", "autopilot.yaml"))

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

        for layer, (tree, _) in trees.items():
            left = [path for path in RETIRED if os.path.exists(os.path.join(tree, path))]
            check(f"{layer} plants nothing under .ai/ for the daemon to read", left == [])

        # The daemon learns the gate and the onboarding from ladder.py, through its own route.
        sys.path.insert(0, HERE)
        import ladder  # noqa: E402  -- safe to import: the loop lives in main()
        import layer  # noqa: E402

        h3 = ladder.onboarding_request("H3", "C:/t/H3")
        check("H3 is onboarded with the exit gate, rooted at its tree",
              h3 == {"project_root": "C:/t/H3", "gate_command": layer.GATE_COMMAND})
        check("H2 is onboarded with no gate confirmed",
              ladder.onboarding_request("H2", "C:/t/H2")["gate_command"] is None)

    total = 1 + 4 + 2
    print(f"\n{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
