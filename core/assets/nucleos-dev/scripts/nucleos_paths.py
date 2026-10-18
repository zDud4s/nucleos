"""Where the workflow scripts find the project, its state and its bundle.

The ONE lookup every script in this directory shares. The project root is the git toplevel of
the current directory, or `CLAUDE_PROJECT_DIR` when the harness sets it (`NUCLEOS_PROJECT_ROOT`
overrides both; tests and wrappers use it). In that root, `.nucleos/workflow/project.json`
holds three keys:

    {"id": "<project id>", "state_dir": "<abs dir>", "bundle_dir": "<abs dir>"}

`state_dir` is per-project state kept OUTSIDE the working tree (loops, plans, specs, memory,
project.yaml, models.yaml, prompts, wave scratch), so it can never enter git. `bundle_dir` is
the library directory of the installed bundle version. Workflow docs and packets sit in the
tree, under `<root>/.nucleos/workflow/`.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

PROJECT_JSON = Path(".nucleos") / "workflow" / "project.json"


class ProjectNotFound(RuntimeError):
    """No usable `.nucleos/workflow/project.json` for the current project."""


def _git_toplevel(start: Path) -> Path | None:
    try:
        proc = subprocess.run(["git", "-C", str(start), "rev-parse", "--show-toplevel"],
                              capture_output=True, text=True, timeout=15)
    except (OSError, subprocess.SubprocessError):
        return None
    out = proc.stdout.strip()
    return Path(out).resolve() if proc.returncode == 0 and out else None


def project_root(start: str | os.PathLike | None = None) -> Path:
    """The project root. Never raises: with no git and no env it is the start directory."""
    forced = os.environ.get("NUCLEOS_PROJECT_ROOT")
    if forced:
        return Path(forced).resolve()
    begin = Path(start or os.environ.get("CLAUDE_PROJECT_DIR") or os.getcwd()).resolve()
    top = _git_toplevel(begin)
    if top is not None:
        return top
    for d in (begin, *begin.parents):
        if (d / PROJECT_JSON).is_file():
            return d
    return begin


def workflow_dir(root: Path | None = None) -> Path:
    """`<root>/.nucleos/workflow/`: the materialized docs, packets, speed table and contract."""
    return (root or project_root()) / ".nucleos" / "workflow"


def load_project(root: Path | None = None) -> dict:
    """Parse project.json; raise ProjectNotFound with a message that says what to do."""
    root = root or project_root()
    path = root / PROJECT_JSON
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise ProjectNotFound(
            f"{path} not found: this project has no materialized nucleos-dev workflow "
            f"(the daemon writes it when the bundle is installed)") from None
    except (OSError, UnicodeError, ValueError) as exc:
        raise ProjectNotFound(f"{path} is unreadable: {exc}") from exc
    if not isinstance(data, dict):
        raise ProjectNotFound(f"{path} must hold a JSON object")
    for key in ("id", "state_dir", "bundle_dir"):
        if not isinstance(data.get(key), str) or not data[key]:
            raise ProjectNotFound(f"{path} lacks a string `{key}`")
    return data


def state_dir(root: Path | None = None) -> Path:
    return Path(load_project(root)["state_dir"])


def bundle_dir(root: Path | None = None) -> Path:
    return Path(load_project(root)["bundle_dir"])


def die(exc: Exception, code: int = 2) -> "None":
    """Print a ProjectNotFound-style message and exit."""
    print(f"{Path(sys.argv[0]).name}: {exc}", file=sys.stderr)
    raise SystemExit(code)
