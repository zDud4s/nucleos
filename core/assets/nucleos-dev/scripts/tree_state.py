"""State shared by the orchestration tree engine: config, paths, manifest, report validation.

Used by tree.py, tree_resources.py and tree_record.py. Never calls the daemon and never runs
cargo. The manifest lives at `<state_dir>/trees/<tree_id>/tree.json` (outside the working tree, so
it can never enter git); only tree.py verbs write it.
"""

from __future__ import annotations

import json
import os
import re
import time
from datetime import datetime, timezone
from pathlib import Path

import yaml

import nucleos_paths

MANIFEST_VERSION = 1
SUMMARY_MAX = 1500

# `tree:` in speed.yaml (generic numbers) and in <state_dir>/project.yaml (this project's).
DEFAULTS = {"floor_gb": 15, "max_live_branches": 4, "seed_gb_per_layer": 8,
            "lock_wait_minutes": 10, "stale_minutes": 30, "report_max_lines": 60}
PROJECT_DEFAULTS = {"worktrees_dir": ".nucleos/worktrees", "links": [], "heavy": None,
                    "base_branch": "master"}
NODES_DEFAULTS = {"max_depth": 3, "max_width": 6, "leaf_budget": 120000,
                  "orchestrator_budget": 150000, "ceiling": 200000, "subtree_verify": False}

BRANCH_STATES = ("queued", "open", "building", "gating", "green", "red", "reviewing", "fixing",
                 "merged", "parked", "dropped")
TERMINAL_STATES = ("merged", "parked", "dropped")
FINDING_STATES = ("open", "fixed", "drop", "defer")
ROLES = ("leaf", "orchestrator", "review")
OUTCOME_STATUS = {"done": "done", "split": "done", "contract_request": "blocked",
                  "blocked": "blocked", "failed": "failed"}


class TreeError(Exception):
    """A refusal the controller can act on; tree.py prints it as JSON and exits 2."""


def now_iso() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


def parse_iso(text: str) -> datetime:
    try:
        when = datetime.fromisoformat(str(text).replace("Z", "+00:00"))
    except ValueError as error:
        raise TreeError(f"{text!r} is not an ISO timestamp") from error
    return when if when.tzinfo else when.replace(tzinfo=timezone.utc)


# --- config -----------------------------------------------------------------------------------

def _read_yaml(path: Path) -> dict:
    try:
        data = yaml.safe_load(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}
    except (OSError, UnicodeError, yaml.YAMLError) as error:
        raise TreeError(f"{path} is unreadable: {error}") from error
    if data is None:
        return {}
    if not isinstance(data, dict):
        raise TreeError(f"{path} must hold a mapping")
    return data


def _overlay(cfg: dict, block, origin: str, *, project: bool) -> None:
    if block is None:
        return
    if not isinstance(block, dict):
        raise TreeError(f"{origin}: `tree` must be a mapping")
    allowed = set(DEFAULTS) | (set(PROJECT_DEFAULTS) if project else set())
    unknown = sorted(set(block) - allowed)
    if unknown:
        raise TreeError(f"{origin}: unknown tree keys {unknown}")
    for key, value in block.items():
        if key in DEFAULTS:
            if isinstance(value, bool) or not isinstance(value, (int, float)) or value < 0:
                raise TreeError(f"{origin}: tree.{key} must be a non-negative number")
        elif key == "links":
            if not isinstance(value, list) or not all(isinstance(v, str) for v in value):
                raise TreeError(f"{origin}: tree.links must be a list of paths")
            value = list(value)
        elif key == "heavy":
            if value is not None and not isinstance(value, str):
                raise TreeError(f"{origin}: tree.heavy must be a path")
        elif not isinstance(value, str) or not value:
            raise TreeError(f"{origin}: tree.{key} must be a non-empty string")
        cfg[key] = value


def load_config(root: Path | None = None) -> dict:
    """The flat resolved config: defaults, then speed.yaml `tree:`, then project.yaml `tree:`."""
    root = root or nucleos_paths.project_root()
    cfg = {**DEFAULTS, **{k: (list(v) if isinstance(v, list) else v) for k, v in PROJECT_DEFAULTS.items()}}
    speed = _read_yaml(nucleos_paths.workflow_dir(root) / "speed.yaml")
    _overlay(cfg, speed.get("tree"), "speed.yaml", project=False)
    project = _read_yaml(nucleos_paths.state_dir(root) / "project.yaml")
    _overlay(cfg, project.get("tree"), "project.yaml", project=True)
    return cfg


def load_nodes_limits(root: Path | None = None) -> dict:
    """The `nodes:` block of speed.yaml over its defaults (depth, width, budgets, ceiling)."""
    root = root or nucleos_paths.project_root()
    block = _read_yaml(nucleos_paths.workflow_dir(root) / "speed.yaml").get("nodes") or {}
    if not isinstance(block, dict):
        raise TreeError("speed.yaml: `nodes` must be a mapping")
    return {**NODES_DEFAULTS, **{k: v for k, v in block.items() if k in NODES_DEFAULTS}}


def dev_home() -> Path:
    """Where this machine's dev-only NucleOS state lives. Copy of heavy.py `_dev_home`
    (scripts/heavy.py): NUCLEOS_DEV_HOME, else `<system drive>/nucleos-dev` on Windows, else
    `$XDG_DATA_HOME/nucleos-dev` or `~/.local/share/nucleos-dev`."""
    env = os.environ.get("NUCLEOS_DEV_HOME")
    if env:
        return Path(env)
    if os.name == "nt":
        return Path((os.environ.get("SystemDrive") or "C:") + "\\") / "nucleos-dev"
    xdg = os.environ.get("XDG_DATA_HOME")
    return (Path(xdg) if xdg else Path.home() / ".local" / "share") / "nucleos-dev"


# --- paths and manifest -----------------------------------------------------------------------

_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")


def check_id(value: str, what: str = "id") -> str:
    if not isinstance(value, str) or not _ID_RE.match(value):
        raise TreeError(f"{what} {value!r} must be letters, digits, '.', '_' or '-'")
    return value


def trees_dir(root: Path | None = None) -> Path:
    return nucleos_paths.state_dir(root) / "trees"


def tree_dir(tree_id: str, root: Path | None = None) -> Path:
    return trees_dir(root) / check_id(tree_id, "tree id")


def manifest_path(tree_id: str, root: Path | None = None) -> Path:
    return tree_dir(tree_id, root) / "tree.json"


def contract_path(tree_id: str, root: Path | None = None) -> Path:
    return tree_dir(tree_id, root) / "contract.md"


def handoff_file(tree_id: str, node_id: str, root: Path | None = None) -> Path:
    """Where the daemon expects a node's handoff: `<state_dir>/nodes/<root_id>/<node_id>.md`."""
    return nucleos_paths.state_dir(root) / "nodes" / check_id(tree_id, "tree id") / f"{check_id(node_id, 'node id')}.md"


def write_handoff(tree_id: str, node_id: str, text: str, *, overwrite: bool = False) -> Path:
    """Write the handoff file unless the node already wrote its own (then keep that one)."""
    path = handoff_file(tree_id, node_id)
    if overwrite or not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return path


def new_branch(name: str, paths: list[str], layer: str, base: str | None) -> dict:
    return {"name": name, "paths": list(paths), "layer": layer, "base": base, "state": "queued",
            "git_branch": None, "worktree": None, "target_dir": None, "node_id": None,
            "gates": [], "reviews": [], "park": None}


def new_manifest(tree_id: str, feature_branch: str, goal: str, project_root, dirty: list[str]) -> dict:
    return {"version": MANIFEST_VERSION, "tree_id": tree_id, "feature_branch": feature_branch,
            "goal": goal, "created_at": now_iso(), "state": "open",
            "project_root": str(project_root).replace("\\", "/"),
            "dirty_owner_files": list(dirty), "branches": {}, "records": [], "attempts": {},
            "costs_gb": {}, "disk_samples": [], "park": None}


def load_manifest(tree_id: str) -> dict:
    path = manifest_path(tree_id)
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise TreeError(f"no tree {tree_id!r} (looked for {path}); run `tree.py init` first") from None
    except (OSError, ValueError) as error:
        raise TreeError(f"{path} is unreadable: {error}") from error
    if not isinstance(manifest, dict) or manifest.get("version") != MANIFEST_VERSION:
        raise TreeError(f"{path} has version {manifest.get('version') if isinstance(manifest, dict) else None}; "
                        f"this tree.py reads version {MANIFEST_VERSION}")
    return manifest


def save_manifest(tree_id: str, manifest: dict) -> None:
    """Atomic: write a sibling temporary file, then os.replace (as wave_engine.write_state)."""
    if manifest.get("tree_id") != tree_id:
        raise TreeError(f"manifest of {manifest.get('tree_id')!r} cannot be saved as tree {tree_id!r}")
    path = manifest_path(tree_id)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    for _ in range(50):
        try:
            os.replace(temporary, path)
            return
        except PermissionError:  # Windows: a reader holds it in this instant
            time.sleep(0.02)
    os.replace(temporary, path)


def list_trees() -> list[str]:
    base = trees_dir()
    if not base.is_dir():
        return []
    return sorted(p.name for p in base.iterdir() if (p / "tree.json").is_file())


def add_tree_arg(parser) -> None:
    parser.add_argument("--tree", default=None, help="tree id (default: $NUCLEOS_TREE_ID, else the only open tree)")


def resolve_tree_id(args) -> str:
    chosen = getattr(args, "tree", None) or os.environ.get("NUCLEOS_TREE_ID")
    if chosen:
        return chosen
    live = [t for t in list_trees() if load_manifest(t).get("state") != "finished"]
    if len(live) == 1:
        return live[0]
    raise TreeError("pass --tree: " + ("no open tree" if not live else f"open trees are {live}"))


def get_branch(manifest: dict, name: str) -> dict:
    branch = manifest["branches"].get(name)
    if branch is None:
        raise TreeError(f"no branch {name!r} in tree {manifest['tree_id']} (have {sorted(manifest['branches'])})")
    return branch


def base_ready(manifest: dict, branch: dict) -> bool:
    """A branch with a base may only open or merge once that base is `merged`."""
    base = branch.get("base")
    return base is None or manifest["branches"].get(base, {}).get("state") == "merged"


def open_findings(branch: dict) -> list[dict]:
    reviews = branch.get("reviews") or []
    return [f for f in (reviews[-1]["findings"] if reviews else []) if f.get("state") == "open"]


# --- path ownership ---------------------------------------------------------------------------

def _norm(path: str) -> str:
    text = str(path).replace("\\", "/")
    return text[2:] if text.startswith("./") else text


def literal_prefix(glob: str) -> str:
    """The text of a glob before its first wildcard, as work_nodes.rs `literal_prefix`."""
    text = _norm(glob)
    cut = min((i for i in (text.find(c) for c in "*?[{") if i >= 0), default=len(text))
    return text[:cut]


def globs_overlap(a: str, b: str) -> bool:
    """The daemon's rule (work_nodes.rs `globs_overlap`), verbatim: one literal prefix starts with
    the other. Segment-aware in practice, because the `/` before a `**` is part of the prefix
    (`core/a/**` and `core/ab/**` do not overlap). Keep it identical: a branch this accepts must
    never be one the daemon later refuses as a sibling."""
    pa, pb = literal_prefix(a), literal_prefix(b)
    return pa.startswith(pb) or pb.startswith(pa)


def sets_overlap(a: list[str], b: list[str]) -> tuple[str, str] | None:
    for left in a:
        for right in b:
            if globs_overlap(left, right):
                return left, right
    return None


def _glob_regex(glob: str) -> str:
    out: list[str] = []
    depth = i = 0
    while i < len(glob):
        c = glob[i]
        if glob.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
            continue
        if glob.startswith("**", i):
            out.append(".*")
            i += 2
            continue
        if c == "*":
            out.append("[^/]*")
        elif c == "?":
            out.append("[^/]")
        elif c == "{":
            out.append("(?:")
            depth += 1
        elif c == "}" and depth:
            out.append(")")
            depth -= 1
        elif c == "," and depth:
            out.append("|")
        else:
            out.append(re.escape(c))
        i += 1
    return "".join(out) + ")" * depth


def path_matches(path: str, glob: str) -> bool:
    """Whether a file path is covered by an owned-path glob (`**`, `*`, `?`, `{a,b}`); a glob with
    no wildcard names a file or a directory prefix."""
    p, g = _norm(path), _norm(glob)
    if not any(c in g for c in "*?{"):
        return p == g.rstrip("/") or p.startswith(g.rstrip("/") + "/")
    return re.fullmatch(_glob_regex(g), p) is not None


# --- contract ids -----------------------------------------------------------------------------

_HEADING_ID = re.compile(r"^#{1,6}\s+[`*]*([A-Z]\d+)[`*]*(?!\w)")
_TABLE_ID = re.compile(r"^\s*\|\s*[`*]*([A-Z]\d+)[`*]*\s*\|")


def contract_ids(text: str) -> set[str]:
    """The ids a contract defines: a markdown heading whose first token is `A3`/`E2` (letter plus
    digits, optionally bold or in backticks, then not a word character), or the first cell of a
    table row. Fenced code blocks are skipped; `## A. Foo` is a section title, not an id."""
    found: set[str] = set()
    fenced = False
    for line in text.splitlines():
        if line.lstrip().startswith(("```", "~~~")):
            fenced = not fenced
            continue
        if fenced:
            continue
        match = _HEADING_ID.match(line) or _TABLE_ID.match(line)
        if match:
            found.add(match.group(1))
    return found


# --- report = handoff -------------------------------------------------------------------------

_COMMENT = re.compile(r"<!--.*?-->", re.S)
_HEADER = re.compile(r"^(Node ID|Root ID|Status|Role|Outcome|Contract used):[ \t]*(.*?)[ \t]*$")
_ID_TOKEN = re.compile(r"^[A-Z]\d+$")


def _is_none(text: str) -> bool:
    return text.strip().rstrip(".").lower() in ("", "none")


def _items(text: str) -> list[str]:
    items = [re.sub(r"^[-*]\s+", "", line.strip()) for line in text.splitlines() if line.strip()]
    return [] if len(items) == 1 and _is_none(items[0]) else items


def parse_report(text: str) -> dict:
    """A filled `packets/handoff.md` as a dict. HTML comments count as absent, so an unfilled
    template yields empty fields. Keys: node_id, root_id, status, role, outcome, contract_used
    (list), contract_used_raw, summary, done, decisions, files, commands (lists), open_items,
    contract_request, blocked_reason (text, `none` read as ""), line_count."""
    lines = [line.rstrip() for line in _COMMENT.sub("", text).splitlines()]
    headers: dict[str, str] = {}
    sections: dict[str, list[str]] = {}
    current = None
    for line in lines:
        if line.startswith("## "):
            current = line[3:].strip().lower()
            sections[current] = []
        elif current is None:
            match = _HEADER.match(line)
            if match:
                headers[match.group(1)] = match.group(2)
        else:
            sections[current].append(line)

    def body(name: str) -> str:
        return "\n".join(sections.get(name, [])).strip()

    def prose(name: str) -> str:
        return "" if _is_none(body(name)) else body(name)

    raw = headers.get("Contract used", "")
    used = [t for t in re.split(r"[,\s]+", raw) if t]
    if [t.lower() for t in used] == ["none"]:
        used = []
    return {
        "node_id": headers.get("Node ID", ""), "root_id": headers.get("Root ID", ""),
        "status": headers.get("Status", "").lower(), "role": headers.get("Role", "").lower(),
        "outcome": headers.get("Outcome", "").lower(),
        "contract_used": used, "contract_used_raw": raw,
        "summary": body("summary"), "done": _items(body("what was done")),
        "decisions": _items(body("decisions")), "files": _items(body("files touched")),
        "commands": _items(body("commands for the root")),
        "open_items": prose("open items / next step"),
        "contract_request": prose("contract request"), "blocked_reason": prose("blocked reason"),
        "line_count": sum(1 for line in lines if line.strip()),
    }


def report_problems(report: dict, cfg: dict, known_ids: set[str] | None = None) -> list[str]:
    """Every reason the report is not acceptable (spec 5.1 / index 3.5); empty means valid."""
    problems = []
    for key, label in (("node_id", "Node ID"), ("root_id", "Root ID")):
        if not report.get(key):
            problems.append(f"{key} is empty (header line `{label}:`)")
    role, outcome, status = report.get("role"), report.get("outcome"), report.get("status")
    if role not in ROLES:
        problems.append(f"Role must be one of {list(ROLES)}, not {role!r}")
    if outcome not in OUTCOME_STATUS:
        problems.append(f"Outcome must be one of {list(OUTCOME_STATUS)}, not {outcome!r}")
    elif status != OUTCOME_STATUS[outcome]:
        problems.append(f"Status {status!r} does not match Outcome {outcome!r} "
                        f"(expected {OUTCOME_STATUS[outcome]!r})")
    if outcome == "split" and role in ("leaf", "review"):
        problems.append(f"only an orchestrator can split, not a {role}")
    if role in ("orchestrator", "review") and report.get("files"):
        problems.append(f"a {role} edits no file, yet Files touched lists {len(report['files'])}")
    if not str(report.get("contract_used_raw", "")).strip():
        problems.append("Contract used is empty: write the ids or `none`")
    for cid in report.get("contract_used", []):
        if not _ID_TOKEN.match(cid):
            problems.append(f"Contract used {cid!r} is not an id like A3 or E2")
        elif known_ids is not None and cid not in known_ids:
            problems.append(f"Contract used {cid} is not in contract.md")
    summary = report.get("summary", "")
    if not summary:
        problems.append("summary is empty")
    elif len(summary) > SUMMARY_MAX:
        problems.append(f"summary is {len(summary)} characters; the limit is {SUMMARY_MAX}")
    if outcome == "contract_request" and not report.get("contract_request"):
        problems.append("Outcome contract_request needs text under `## Contract request`")
    if outcome != "contract_request" and report.get("contract_request"):
        problems.append("`## Contract request` has text but Outcome is not contract_request")
    if outcome in ("blocked", "failed") and not report.get("blocked_reason"):
        problems.append(f"Outcome {outcome} needs text under `## Blocked reason`")
    cap = int(cfg["report_max_lines"])
    if report.get("line_count", 0) > cap:
        problems.append(f"report has {report['line_count']} lines; the limit is {cap} (HTML comments not counted)")
    return problems


def validate_report(report: dict, cfg: dict, known_ids: set[str] | None = None) -> None:
    """Raise TreeError listing every problem; return None when the report is valid."""
    problems = report_problems(report, cfg, known_ids)
    if problems:
        raise TreeError("invalid report: " + "; ".join(problems))


def render_handoff(*, node_id: str, root_id: str, status: str, role: str, outcome: str, summary: str,
                   done=(), decisions=(), files=(), commands=(), open_items: str = "none",
                   contract_request: str = "none", blocked_reason: str = "none",
                   contract_used=()) -> str:
    """A filled `packets/handoff.md` (the inverse of parse_report), for the handoffs tree.py writes
    itself: the park markers and the root."""
    def bullets(items) -> str:
        return "\n".join(f"- {item}" for item in items) or "none"

    return (
        f"# Node Handoff\n\nNode ID: {node_id}\nRoot ID: {root_id}\nStatus: {status}\nRole: {role}\n"
        f"Outcome: {outcome}\nContract used: {', '.join(contract_used) or 'none'}\n\n"
        f"## Summary\n{summary}\n\n## What was done\n{bullets(done)}\n\n## Decisions\n{bullets(decisions)}\n\n"
        f"## Files touched\n{bullets(files)}\n\n## Commands for the root\n{bullets(commands)}\n\n"
        f"## Open items / next step\n{open_items}\n\n## Contract request\n{contract_request}\n\n"
        f"## Blocked reason\n{blocked_reason}\n")
