#!/usr/bin/env python3
"""Background half of the `Stop` event: hands over to the timer of the project's pinned bundle.

Registered beside `ask_daemon.py` with `asyncRewake` (`autopilot.rs`'s `wire_stop_timer`): Claude
Code leaves it running after the turn ends and wakes the idle session if it exits 2. That budget
rules out the daemon's bundle hooks, which answer within 50 s, so the script is run from here.

Exit 2 is a wake-up, so nothing may reach it by accident: a project with no workflow, a bundle
older than the timer and any error all exit 0. Python's own exit status for a script it cannot
open is also 2, which is why the bundle's script is checked for before it is run.
"""

import json
import os
import runpy
import sys
from pathlib import Path


def main() -> None:
    root = Path(os.environ["CLAUDE_PROJECT_DIR"])
    project = json.loads((root / ".nucleos/workflow/project.json").read_text(encoding="utf-8"))
    scripts = Path(project["bundle_dir"]) / "scripts"
    timer = scripts / "spec_loop_timer.py"
    if not timer.is_file():
        return
    sys.path.insert(0, str(scripts))
    sys.argv = [str(timer)]
    runpy.run_path(str(timer), run_name="__main__")


if __name__ == "__main__":
    try:
        main()
    except SystemExit as stop:
        sys.exit(2 if stop.code == 2 else 0)
    except Exception:
        pass
    sys.exit(0)
