#!/usr/bin/env python3
"""Deterministic engine for the orchestration tree: manifest, ownership, prompts, review, merge.

    tree.py init <root_node_id> --feature-branch B --goal G
    tree.py branch add <name> --paths P... --layer L [--base B]
    tree.py branch bind <name> --node-id ID      (record the branch node after node_open)
    tree.py branch drop <name> --reason R
    tree.py prompt <node_id> --branch B --kind leaf|orchestrator|review --goal G ...
    tree.py status [--nodes FILE]
    tree.py review|merge|park|finish ...             (the 4c verbs, below)
    tree.py budget|open|close|gate|record ...        (tree_resources.py, tree_record.py)

No LLM, JSON on stdout, exit 0 on success and 2 on a refusal ({"verb": ..., "error": ...}), like
wave_engine.py. It never calls the MCP: when a node must be opened, updated or closed it returns
`{"mcp": [{"tool": ..., "args": {...}}]}` for the controller to make, in order.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

import nucleos_paths
import tree_record
import tree_resources
import tree_state as ts
from tree_state import TreeError

NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
_COMMENT = re.compile(r"<!--.*?-->", re.S)

CONTRACT_SKELETON = """# Contract

Tree: {tree_id}
Feature branch: {feature}

Only the controller writes this file. Nodes read it and ask for what is missing
(`Outcome: contract_request`); the controller decides and adds an E<n> entry.

## A. Superfícies partilhadas

## E. Decisões
"""


# --- registry and entry point -----------------------------------------------------------------

class Verbs(dict):
    """What `register(verbs)` receives. A dict `name -> (configure(parser), run(args) -> dict)`
    that also answers `add_parser(name, **kw)` like an argparse subparsers action (the caller adds
    arguments and `set_defaults(func=run)`), so both registration styles of chunks 5 and 6 work."""

    def __init__(self):
        super().__init__()
        self.parsers: dict[str, argparse.ArgumentParser] = {}

    def add_parser(self, name, **_ignored):
        parser = argparse.ArgumentParser(prog=name, add_help=False)
        self.parsers[name] = parser
        return parser


def build_parser(verbs: Verbs) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="verb", required=True)
    for name, (configure, run) in sorted(verbs.items()):
        p = sub.add_parser(name)
        configure(p)
        p.set_defaults(func=run)
    for name, standalone in sorted(verbs.parsers.items()):
        sub.add_parser(name, parents=[standalone])
    return parser


def main(argv: list[str] | None = None) -> int:
    verbs = core_verbs()
    tree_resources.register(verbs)
    tree_record.register(verbs)
    args = build_parser(verbs).parse_args(argv)
    try:
        answer = args.func(args)
    except (TreeError, nucleos_paths.ProjectNotFound, OSError) as error:
        print(json.dumps({"verb": args.verb, "error": str(error)}))
        return 2
    print(json.dumps(answer))
    return 0


# --- shared helpers ---------------------------------------------------------------------------

def open_manifest(args) -> tuple[str, dict]:
    tree_id = ts.resolve_tree_id(args)
    manifest = ts.load_manifest(tree_id)
    if manifest["state"] == "finished":
        raise TreeError(f"tree {tree_id} is finished")
    return tree_id, manifest


def normalize_paths(paths: list[str]) -> list[str]:
    cleaned = []
    for path in paths:
        text = path.replace("\\", "/").strip()
        text = text[2:] if text.startswith("./") else text
        if not text:
            raise TreeError("an empty path is not a path")
        # Mirror the daemon's `normalise_glob`, which refuses these as Invalid.
        if text.startswith("/") or text[1:2] == ":":
            raise TreeError(f"path {path!r} must be relative to the project root")
        if ".." in text.split("/"):
            raise TreeError(f"path {path!r} may not contain a '..' segment")
        cleaned.append(text)
    return cleaned


def mcp_close(node_id: str, status: str, summary: str, handoff_path) -> dict:
    """The `node_close` payload for the controller (its args use `id`, params.rs:473-495)."""
    return {"tool": "node_close", "args": {"id": node_id, "status": status,
                                           "summary": summary[:ts.SUMMARY_MAX], "handoff_path": str(handoff_path)}}


def dirty_files(root: Path) -> list[str]:
    """Tracked files with uncommitted changes in the project root; `[]` outside a git repo."""
    try:
        proc = subprocess.run(["git", "-C", str(root), "status", "--porcelain", "--untracked-files=no"],
                              capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return []
    if proc.returncode != 0:
        return []
    files = set()
    for line in proc.stdout.splitlines():
        path = line[3:]
        files.add((path.split(" -> ")[-1] if " -> " in path else path).strip().strip('"'))
    return sorted(files)


# --- init -------------------------------------------------------------------------------------

def configure_init(p) -> None:
    p.add_argument("root_node_id", help="the root node id from node_open; it is the tree id")
    p.add_argument("--feature-branch", required=True)
    p.add_argument("--goal", required=True)


def run_init(args) -> dict:
    tree_id = ts.check_id(args.root_node_id, "root node id")
    if ts.manifest_path(tree_id).exists():
        raise TreeError(f"tree {tree_id} already exists")
    root = nucleos_paths.project_root()
    dirty = dirty_files(root)
    ts.save_manifest(tree_id, ts.new_manifest(tree_id, args.feature_branch, args.goal, root, dirty))
    contract = ts.contract_path(tree_id)
    if not contract.exists():
        contract.write_text(CONTRACT_SKELETON.format(tree_id=tree_id, feature=args.feature_branch),
                            encoding="utf-8")
    return {"verb": "init", "tree_id": tree_id, "dir": str(ts.tree_dir(tree_id)),
            "contract": str(contract), "dirty_owner_files": dirty}


# --- branch -----------------------------------------------------------------------------------

def configure_branch(p) -> None:
    ts.add_tree_arg(p)
    p.add_argument("action", choices=["add", "bind", "drop"])
    p.add_argument("name")
    p.add_argument("--paths", nargs="+", default=None, help="add: owned globs; none may overlap another live branch")
    p.add_argument("--layer", default=None, help="add: core | shell | sidecar | ... (budget keys cost by layer)")
    p.add_argument("--base", default=None, help="add: a branch that must be merged before this one opens or merges")
    p.add_argument("--node-id", default=None, help="bind: the id node_open returned for the branch node")
    p.add_argument("--reason", default=None, help="drop: why the branch is abandoned")


def base_chain(manifest: dict, base: str | None) -> list[str]:
    chain: list[str] = []
    while base and base not in chain:
        chain.append(base)
        base = manifest["branches"].get(base, {}).get("base")
    return chain


def branch_add(args) -> dict:
    tree_id, manifest = open_manifest(args)
    if manifest["state"] == "parked":
        raise TreeError(f"tree {tree_id} is parked: `park tree --resume` before adding branches")
    if not args.paths or not args.layer:
        raise TreeError("branch add needs --paths and --layer")
    name = args.name
    if not NAME_RE.match(name):
        raise TreeError(f"branch name {name!r} must be letters, digits, '.', '_' or '-'")
    if name in manifest["branches"]:
        raise TreeError(f"branch {name!r} already exists in tree {tree_id}")
    if args.base is not None and args.base not in manifest["branches"]:
        raise TreeError(f"unknown base {args.base!r} (declare it first with `branch add`)")
    paths = normalize_paths(args.paths)
    related = set(base_chain(manifest, args.base))
    for other in manifest["branches"].values():
        if other["name"] in related or other["state"] in ("merged", "dropped"):
            continue
        hit = ts.sets_overlap(paths, other["paths"])
        if hit:
            raise TreeError(f"branch {name!r} path {hit[0]!r} overlaps branch {other['name']!r} path {hit[1]!r}")
    manifest["branches"][name] = ts.new_branch(name, paths, args.layer, args.base)
    ts.save_manifest(tree_id, manifest)
    return {"verb": "branch", "action": "add", "tree_id": tree_id, "branch": name, "state": "queued",
            "paths": paths, "base": args.base}


DROPPABLE = ("queued", "open", "building", "red", "fixing", "parked")


def branch_bind(args) -> dict:
    """Record the branch node the controller opened with node_open: the branch is then `building`."""
    tree_id, manifest = open_manifest(args)
    branch = ts.get_branch(manifest, args.name)
    if not args.node_id:
        raise TreeError("branch bind needs --node-id")
    node_id = ts.check_id(args.node_id, "node id")
    if branch.get("node_id"):
        raise TreeError(f"branch {args.name!r} is already bound to node {branch['node_id']}")
    if branch["state"] != "open":
        raise TreeError(f"branch {args.name!r} is {branch['state']}: bind its node after `open` (state open)")
    for other in manifest["branches"].values():
        if other.get("node_id") == node_id:
            raise TreeError(f"node {node_id} is already the node of branch {other['name']!r}")
    branch["node_id"], branch["state"] = node_id, "building"
    ts.save_manifest(tree_id, manifest)
    return {"verb": "branch", "action": "bind", "tree_id": tree_id, "branch": args.name,
            "node_id": node_id, "state": "building"}


def branch_drop(args) -> dict:
    """Abandon a branch: its paths are free again and its node (if any) is closed `failed`."""
    tree_id, manifest = open_manifest(args)
    branch = ts.get_branch(manifest, args.name)
    reason = " ".join((args.reason or "").split())
    if not reason:
        raise TreeError("branch drop needs --reason")
    if branch["state"] not in DROPPABLE:
        raise TreeError(f"branch {args.name!r} is {branch['state']}: only {', '.join(DROPPABLE)} can be dropped")
    mcp: list[dict] = []
    node_id = branch.get("node_id")
    if node_id:
        summary = f"dropped: {reason}"
        text = ts.render_handoff(node_id=node_id, root_id=tree_id, status="failed", role="orchestrator",
                                 outcome="failed", summary=summary[:ts.SUMMARY_MAX],
                                 decisions=[summary], blocked_reason=reason)
        mcp.append(mcp_close(node_id, "failed", summary, ts.write_handoff(tree_id, node_id, text)))
    branch["state"], branch["drop_reason"], branch["dropped_at"] = "dropped", reason, ts.now_iso()
    ts.save_manifest(tree_id, manifest)
    return {"verb": "branch", "action": "drop", "tree_id": tree_id, "branch": args.name,
            "state": "dropped", "mcp": mcp,
            "next": "close or abandon the branch node's running children before the node_close" if mcp else ""}


BRANCH_ACTIONS = {"add": branch_add, "bind": branch_bind, "drop": branch_drop}


def run_branch(args) -> dict:
    action = BRANCH_ACTIONS.get(args.action)
    if action is None:
        raise TreeError(f"branch {args.action} is not available")
    return action(args)


# --- prompt -----------------------------------------------------------------------------------

_FIELD = re.compile(r"^([A-Z][A-Za-z ]+):[ \t]*<!--.*-->[ \t]*$")


def configure_prompt(p) -> None:
    ts.add_tree_arg(p)
    p.add_argument("node_id", help="the id node_open returned for this node")
    p.add_argument("--branch", required=True)
    p.add_argument("--kind", required=True, choices=["leaf", "orchestrator", "review"])
    p.add_argument("--goal", required=True)
    p.add_argument("--paths", nargs="*", default=[], help="owned globs (leaf and orchestrator)")
    p.add_argument("--pointers", nargs="*", default=[], help="`<path>:<start>-<end> - why`, one per argument")
    p.add_argument("--sections", nargs="*", default=[], help="contract ids to read, e.g. A3 E2")
    p.add_argument("--title", default=None)
    p.add_argument("--parent", default=None, help="parent node id (default: the root)")
    p.add_argument("--depth", type=int, default=1)
    p.add_argument("--task-id", default="none")


def fill_fields(template: str, values: dict) -> str:
    """Replace the single-line `Key: <!-- hint -->` header lines of the packet with `Key: value`."""
    out = []
    for line in template.splitlines():
        match = _FIELD.match(line)
        out.append(f"{match.group(1)}: {values[match.group(1)]}" if match and match.group(1) in values else line)
    return "\n".join(out)


def split_sections(text: str) -> tuple[list[str], list[tuple[str, list[str]]]]:
    """(the lines before the first `## `, [(title, lines)] for each `## ` section)."""
    pre: list[str] = []
    sections: list[tuple[str, list[str]]] = []
    for line in text.splitlines():
        if line.startswith("## "):
            sections.append((line[3:].strip(), []))
        elif sections:
            sections[-1][1].append(line)
        else:
            pre.append(line)
    return pre, sections


def subsections(sections: list[tuple[str, list[str]]], title: str, needed: tuple[str, ...]) -> dict[str, list[str]]:
    """The `### x` blocks of the packet section `## <title>`; every name in `needed` must have lines."""
    block = dict(sections).get(title)
    if block is None:
        raise TreeError(f"packets/node.md has no `## {title}` section")
    subs: dict[str, list[str]] = {}
    current = None
    for line in block:
        if line.startswith("### "):
            current = line[4:].strip()
            subs[current] = []
        elif current is not None and line.strip():
            subs[current].append(line)
    for name in needed:
        if not subs.get(name):
            raise TreeError(f"packets/node.md: `## {title}` has nothing under `### {name}`")
    return subs


def role_rules(sections: list[tuple[str, list[str]]], role: str) -> list[str]:
    """`### all` plus `### <role>` from the packet's `## Rules by role`."""
    subs = subsections(sections, "Rules by role", ("all", role))
    return subs["all"] + subs[role]


def status_map() -> str:
    """`done and split -> done; ...`, grouped from ts.OUTCOME_STATUS (the validator's own table)."""
    groups: dict[str, list[str]] = {}
    for outcome, status in ts.OUTCOME_STATUS.items():
        groups.setdefault(status, []).append(outcome)
    return "; ".join(f"{' and '.join(outcomes)} -> {status}" for status, outcomes in groups.items())


def report_block(sections, kind: str, max_lines: int, handoff: str) -> list[str]:
    """The packet's `## Report format`: `### all` plus `### file` (a node that writes) or `### message`."""
    where = "message" if kind == "review" else "file"
    subs = subsections(sections, "Report format", ("all", where))
    values = {"{max_lines}": str(max_lines), "{role}": kind, "{status_map}": status_map(), "{handoff}": handoff}
    out = []
    for line in subs["all"] + subs[where]:
        for key, value in values.items():
            line = line.replace(key, value)
        out.append(line)
    return out


def contained(path: str, owned: list[str]) -> bool:
    """Whether a child's path lies inside the branch's paths (the daemon's L1 containment rule, not
    overlap). A literal path must be matched by an owned glob (a literal owned path is a file or a
    directory); a wildcard path (`core/src/x/**`) by an owned `/**` glob whose literal prefix is a
    prefix of the child's, or by an owned literal directory the child's literal prefix lies under
    (`<dir>/...`, segment boundary)."""
    if not any(c in path for c in "*?[{"):
        return any(ts.path_matches(path, glob) for glob in owned)
    prefix = ts.literal_prefix(path)
    for glob in owned:
        if any(c in glob for c in "*?[{"):
            if glob.endswith("/**") and prefix.startswith(ts.literal_prefix(glob)):
                return True
        else:
            base = ts.literal_prefix(glob).rstrip("/")  # glob has no wildcard: the whole glob
            if prefix.startswith(base + "/"):
                return True
    return False


def posix(path) -> str:
    return str(path).replace("\\", "/")


def run_prompt(args) -> dict:
    tree_id, manifest = open_manifest(args)
    branch = ts.get_branch(manifest, args.branch)
    kind = args.kind
    paths = normalize_paths(args.paths)
    if kind != "review":
        if not paths:
            raise TreeError(f"a {kind} node needs --paths")
        for path in paths:
            if not contained(path, branch["paths"]):
                raise TreeError(f"path {path!r} is outside branch {args.branch!r} ({branch['paths']})")
    else:
        paths = []
    template_path = nucleos_paths.workflow_dir() / "packets" / "node.md"
    try:
        template = template_path.read_text(encoding="utf-8")
    except OSError as error:
        raise TreeError(f"cannot read {template_path}: {error}") from error
    limits, cfg = ts.load_nodes_limits(), ts.load_config()
    node_id = ts.check_id(args.node_id, "node id")
    handoff = posix(ts.handoff_file(tree_id, node_id))
    values = {
        "Node ID": node_id, "Root ID": tree_id, "Parent ID": args.parent or tree_id, "Task ID": args.task_id,
        "Kind": "orchestrator" if kind == "orchestrator" else "leaf",
        "Title": args.title or args.goal[:60], "Owned paths": ", ".join(paths) or "none (read-only)",
        "Budget tokens": limits["orchestrator_budget" if kind == "orchestrator" else "leaf_budget"],
        "Current depth": args.depth, "Max depth": limits["max_depth"], "Max width": limits["max_width"],
        "Leaf budget": limits["leaf_budget"], "Orchestrator budget": limits["orchestrator_budget"],
        "Ceiling": limits["ceiling"], "Subtree verify": str(bool(limits["subtree_verify"])).lower()}
    pre, sections = split_sections(_COMMENT.sub("", fill_fields(template, values)))
    rules = role_rules(sections, kind)
    lines = list(pre)
    for title, body in sections:
        if title in ("Rules by role", "Report format"):
            continue
        if title == "Goal":
            body = [args.goal, ""]
        elif title == "Pointers":
            body = [*(args.pointers or ["none"]), ""]
        lines += [f"## {title}", *body]
    report = report_block(sections, kind, cfg["report_max_lines"], handoff)
    lines += [
        "", "## Tree", f"Tree: {tree_id}", f"Branch: {args.branch}", f"Role: {kind}",
        f"Worktree: {branch['worktree'] or 'not opened yet'}",
        f"Contract (read-only): {posix(ts.contract_path(tree_id))}",
        f"Contract sections to read: {', '.join(args.sections) or 'none'}", f"Handoff file: {handoff}",
        "", "## Rules", *rules,
        "", "## Report", *report]
    text = re.sub(r"\n{3,}", "\n\n", "\n".join(lines)).strip() + "\n"
    target = ts.tree_dir(tree_id) / "prompts" / f"{node_id}.md"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(text, encoding="utf-8")
    return {"verb": "prompt", "tree_id": tree_id, "node_id": node_id, "branch": args.branch, "kind": kind,
            "prompt_file": str(target), "handoff_file": handoff, "prompt": text}


# --- status -----------------------------------------------------------------------------------

def configure_status(p) -> None:
    ts.add_tree_arg(p)
    p.add_argument("--nodes", default=None, help="a node_tree output as JSON: flags dead `running` nodes")
    p.add_argument("--now", default=None, help=argparse.SUPPRESS)  # tests pin the clock


def load_nodes_file(path: str) -> list[dict]:
    try:
        data = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise TreeError(f"--nodes {path}: {error}") from error
    if isinstance(data, dict):
        data = data.get("nodes")
    if not isinstance(data, list) or not all(isinstance(n, dict) and n.get("id") for n in data):
        raise TreeError("--nodes must hold a node_tree output: a `nodes` list of objects that have an id")
    return data


def descendants(nodes: list[dict], root_id: str) -> set[str]:
    kids: dict = {}
    for node in nodes:
        kids.setdefault(node.get("parent_id"), []).append(node["id"])
    found: set[str] = set()
    stack = [root_id]
    while stack:
        for child in kids.get(stack.pop(), []):
            if child not in found:
                found.add(child)
                stack.append(child)
    return found


def stale_nodes(nodes: list[dict], now: datetime, minutes: float, skip: set[str]) -> tuple[list[dict], list[str]]:
    """(running nodes silent for longer than `minutes`, running nodes with no updated_at at all)."""
    suspects, unverifiable = [], []
    for node in nodes:
        if node.get("status") != "running" or node.get("kind") == "root" or node["id"] in skip:
            continue
        if not node.get("updated_at"):
            unverifiable.append(node["id"])
            continue
        age = (now - ts.parse_iso(node["updated_at"])).total_seconds() / 60
        if age > minutes:
            suspects.append({"id": node["id"], "title": node.get("title"), "kind": node.get("kind"),
                             "age_minutes": round(age, 1), "branch": None})
    return suspects, unverifiable


def branch_next(manifest: dict, b: dict, suspects: list[dict], open_count: int) -> str:
    name, state = b["name"], b["state"]
    if state in ("merged", "dropped"):
        return ""
    if suspects:
        ids = ", ".join(s["id"] for s in suspects)
        longest = max(s["age_minutes"] for s in suspects)
        return (f"node(s) {ids} look dead (running, no update for {longest:g} min): check the harness task, "
                f"close each one `handoff` and relaunch it, then `record`")
    if state == "parked":
        park = b.get("park") or {}
        if park.get("marker_node"):
            return f"parked ({park.get('needs')}); resume with `park branch {name} --resume`"
        return (f"parked ({park.get('needs')}) but the marker node is not closed: rerun `park branch {name}` "
                f"for the node_open payload, then `park branch {name} --marker-id <id>`")
    if state == "queued":
        if not ts.base_ready(manifest, b):
            return f"wait: base {b['base']!r} is not merged"
        return "run `budget`; if it allows, `open`"
    if state == "open":
        return ("bind the branch node (`branch bind NAME --node-id ID`, the id node_open returned) "
                "then dispatch its orchestrator (`prompt --kind orchestrator`)")
    if state == "building":
        return "wait for the node reports; `record` each one"
    if state == "gating":
        return "wait for the gate; `gate` registers its result"
    if state == "green":
        if not b["reviews"]:
            return "review the branch diff, then submit the findings with `review`"
        if open_count:
            return f"{open_count} open finding(s): run a fix tree or triage them with `review`"
        return "merge through the VCS queue, then `tree.py merge`"
    if state == "red":
        return "read the gate log; open a fix tree or `park`"
    if state == "reviewing":
        return "triage the findings with `review`"
    return "run the fix tree, `gate` again, then mark the findings fixed with `review`"  # fixing


def run_status(args) -> dict:
    tree_id = ts.resolve_tree_id(args)
    manifest = ts.load_manifest(tree_id)
    cfg = ts.load_config()
    now = ts.parse_iso(args.now) if args.now else datetime.now(timezone.utc)
    nodes = load_nodes_file(args.nodes) if args.nodes else []
    branches = manifest["branches"]
    keep_running = {tree_id} | {b["node_id"] for b in branches.values() if b.get("node_id")}
    suspects, unverifiable = stale_nodes(nodes, now, cfg["stale_minutes"], keep_running)
    owner = {}
    for name, b in branches.items():
        if b.get("node_id"):
            owner.update({nid: name for nid in descendants(nodes, b["node_id"])})
    for suspect in suspects:
        suspect["branch"] = owner.get(suspect["id"])
    summary, nxt = [], []
    for name, b in branches.items():
        mine = [s for s in suspects if s["branch"] == name]
        open_count = len(ts.open_findings(b))
        action = branch_next(manifest, b, mine, open_count)
        summary.append({"name": name, "state": b["state"], "layer": b["layer"], "base": b["base"],
                        "base_ready": ts.base_ready(manifest, b), "node_id": b.get("node_id"),
                        "worktree": b.get("worktree"), "open_findings": open_count,
                        "suspect_nodes": [s["id"] for s in mine], "next": action})
        if action:
            nxt.append(f"{name}: {action}")
    if manifest["state"] == "parked" and manifest.get("park"):
        nxt.insert(0, f"tree parked ({manifest['park'].get('needs')}); resume with `park tree --resume`")
    elif not branches:
        nxt.append("declare the top-level branches with `branch add`")
    elif all(b["state"] in ts.TERMINAL_STATES for b in branches.values()):
        nxt.append("all branches settled: run `tree.py finish`")
    return {"verb": "status", "tree_id": tree_id, "state": manifest["state"],
            "feature_branch": manifest["feature_branch"], "goal": manifest["goal"],
            "dirty_owner_files": manifest.get("dirty_owner_files", []), "branches": summary,
            "suspect_nodes": suspects, "unverifiable_nodes": unverifiable, "park": manifest.get("park"),
            "next": nxt}


# --- review -----------------------------------------------------------------------------------

def configure_review(p) -> None:
    ts.add_tree_arg(p)
    p.add_argument("branch", nargs="?", default=None)
    p.add_argument("--integration", action="store_true",
                   help="the review of the integrated feature branch: no branch, stored in the manifest")
    p.add_argument("--findings", default="-", help="JSON list of {id, state, file?, note?}; `-` is stdin")


def read_findings(source: str) -> list[dict]:
    # stdin as UTF-8 bytes, whatever the console codepage is (cp1252 on the owner's machine)
    raw = sys.stdin.buffer.read().decode("utf-8") if source == "-" else Path(source).read_text(encoding="utf-8")
    try:
        data = json.loads(raw)
    except ValueError as error:
        raise TreeError(f"the findings are not JSON: {error}") from error
    if isinstance(data, dict) and "findings" in data:
        data = data["findings"]
    if not isinstance(data, list):
        raise TreeError("the findings must be a JSON list")
    seen: set[str] = set()
    for finding in data:
        if not isinstance(finding, dict) or not isinstance(finding.get("id"), str) or not finding["id"].strip():
            raise TreeError("every finding needs an id")
        if finding.get("state") not in ts.FINDING_STATES:
            raise TreeError(f"finding {finding['id']}: state must be one of {list(ts.FINDING_STATES)}")
        if finding["id"] in seen:
            raise TreeError(f"duplicate finding id {finding['id']}")
        seen.add(finding["id"])
    return data


def fixed_ids(findings) -> list[str]:
    return sorted(f["id"] for f in findings if f.get("state") == "fixed")


def run_review(args) -> dict:
    tree_id, manifest = open_manifest(args)
    if args.integration:
        holder = {"reviews": manifest.setdefault("integration_reviews", [])}
        label = None
    elif args.branch:
        holder = ts.get_branch(manifest, args.branch)
        label = args.branch
        if holder["state"] not in ("green", "reviewing", "fixing"):
            raise TreeError(f"branch {args.branch!r} is {holder['state']}: it is reviewed after its gate is green")
    else:
        raise TreeError("review needs a branch name or --integration")
    submitted = read_findings(args.findings)
    before = list(holder["reviews"][-1]["findings"]) if holder["reviews"] else []
    current = {f["id"]: dict(f) for f in before}
    for finding in submitted:
        current[finding["id"]] = {**current.get(finding["id"], {}), **finding}
    holder["reviews"].append({"at": ts.now_iso(), "findings": list(current.values())})
    still_open = ts.open_findings(holder)
    if label is not None:
        if still_open and holder["state"] != "fixing":
            holder["state_before_fix"] = holder["state"]
            holder["fixed_at_entry"] = fixed_ids(before)
            holder["state"] = "fixing"
        elif not still_open and holder["state"] == "fixing":
            # Only drop/defer since the fix began: the code did not change, so no new gate is due.
            if fixed_ids(current.values()) == holder.get("fixed_at_entry") and holder.get("state_before_fix"):
                holder["state"] = holder.pop("state_before_fix")
                holder.pop("fixed_at_entry", None)
    ts.save_manifest(tree_id, manifest)
    counts: dict[str, int] = {}
    for finding in current.values():
        counts[finding["state"]] = counts.get(finding["state"], 0) + 1
    return {"verb": "review", "tree_id": tree_id, "scope": "integration" if label is None else "branch",
            "branch": label, "state": holder.get("state"), "open": [f["id"] for f in still_open],
            "counts": counts, "reviews": len(holder["reviews"])}


# --- merge ------------------------------------------------------------------------------------

def configure_merge(p) -> None:
    ts.add_tree_arg(p)
    p.add_argument("branch")
    p.add_argument("--sha", default=None, help="the merge commit on the feature branch")


def run_merge(args) -> dict:
    """Records a merge the controller already made through the VCS queue; refuses one it should not."""
    tree_id, manifest = open_manifest(args)
    name = args.branch
    branch = ts.get_branch(manifest, name)
    if branch["state"] == "merged":
        raise TreeError(f"branch {name!r} is already merged")
    if branch["state"] in ("parked", "dropped"):
        raise TreeError(f"branch {name!r} is {branch['state']}")
    if not ts.base_ready(manifest, branch):
        raise TreeError(f"branch {name!r} is based on {branch['base']!r}, which is not merged yet")
    pending = ts.open_findings(branch)
    if pending:
        raise TreeError(f"branch {name!r} has {len(pending)} open finding(s): "
                        + ", ".join(f["id"] for f in pending))
    if branch["state"] != "green":
        raise TreeError(f"branch {name!r} is {branch['state']}, not green: run the gate (again after any fix)")
    branch["state"] = "merged"
    branch["merged_at"] = ts.now_iso()
    if args.sha:
        branch["merge_sha"] = args.sha
    mcp: list[dict] = []
    node_id = branch.get("node_id")
    if node_id:
        # The handoff is written before the manifest is saved as merged, so a write failure is retry-safe.
        summary = f"Branch {name} merged into {manifest['feature_branch']}"
        text = ts.render_handoff(node_id=node_id, root_id=tree_id, status="done", role="orchestrator",
                                 outcome="done", summary=summary,
                                 done=[f"{name} merged", "paths: " + ", ".join(branch["paths"])])
        mcp.append(mcp_close(node_id, "done", summary, ts.write_handoff(tree_id, node_id, text)))
    ts.save_manifest(tree_id, manifest)
    unblocked = [n for n, b in manifest["branches"].items() if b["base"] == name and b["state"] == "queued"]
    result = {"verb": "merge", "tree_id": tree_id, "branch": name, "state": "merged", "unblocked": unblocked,
              "mcp": mcp, "warnings": [] if branch["reviews"] else ["unreviewed"]}
    if mcp:
        result["next"] = "run the node_close before opening the node of a dependent branch"
    return result


# --- park -------------------------------------------------------------------------------------

def mcp_open(parent_id: str, kind: str, title: str, goal: str, owned_paths=()) -> dict:
    return {"tool": "node_open", "args": {"parent_id": parent_id, "kind": kind, "title": title,
                                          "goal": goal, "owned_paths": list(owned_paths)}}


def mcp_update(node_id: str, *, decision: str | None = None, status: str | None = None) -> dict:
    args = {"id": node_id}
    if decision is not None:
        args["decision"] = decision
    if status is not None:
        args["status"] = status
    return {"tool": "node_update", "args": args}


def configure_park(p) -> None:
    ts.add_tree_arg(p)
    p.add_argument("target", choices=["node", "branch", "tree"])
    p.add_argument("name", nargs="?", default=None, help="node id or branch name (not for `tree`)")
    p.add_argument("--needs", default=None, help="the decision or input that is missing, from whom")
    p.add_argument("--marker-id", default=None, help="step 2: the id node_open returned for the marker")
    p.add_argument("--resume", action="store_true", help="undo the park of a branch or of the tree")
    p.add_argument("--role", choices=ts.ROLES, default="leaf", help="park node: the role of that node")


def blocked_close(node_id: str, tree_id: str, needs: str, role: str = "leaf") -> tuple[list[dict], Path]:
    """The payloads that close a node `blocked` with its decision, and the handoff they point at."""
    summary = f"Parked: {needs}"
    text = ts.render_handoff(node_id=node_id, root_id=tree_id, status="blocked", role=role, outcome="blocked",
                             summary=summary[:ts.SUMMARY_MAX], decisions=[needs], blocked_reason=needs)
    path = ts.write_handoff(tree_id, node_id, text)  # keeps a handoff the node wrote itself
    return [mcp_update(node_id, decision=needs), mcp_close(node_id, "blocked", summary, path)], path


def run_park(args) -> dict:
    tree_id, manifest = open_manifest(args)
    needs = " ".join(args.needs.split()) if args.needs else None
    if args.target == "node":
        if not args.name:
            raise TreeError("park node needs a node id")
        if not needs:
            raise TreeError("--needs is required to park a node")
        mcp, path = blocked_close(ts.check_id(args.name, "node id"), tree_id, needs, args.role)
        return {"verb": "park", "target": "node", "node_id": args.name, "handoff_file": str(path), "mcp": mcp}
    if args.target == "branch":
        if not args.name:
            raise TreeError("park branch needs a branch name")
        holder, label = ts.get_branch(manifest, args.name), args.name
        parent = holder.get("node_id") or tree_id
    else:
        holder, label, parent = manifest, "tree", tree_id
    park = holder.get("park")
    if args.resume:
        if not park:
            raise TreeError(f"nothing parked here ({label})")
        holder["state"] = park["prev_state"]
        holder["park"] = None
        ts.save_manifest(tree_id, manifest)
        return {"verb": "park", "target": args.target, "step": "resume", "state": holder["state"], "mcp": []}
    if args.marker_id:
        if not park:
            raise TreeError("park first: run `park` without --marker-id to get the node_open payload")
        if park["marker_node"]:
            raise TreeError(f"marker {park['marker_node']} is already recorded for {label}")
        marker = ts.check_id(args.marker_id, "marker id")
        mcp, path = blocked_close(marker, tree_id, park["needs"])
        park["marker_node"] = marker
        ts.save_manifest(tree_id, manifest)
        return {"verb": "park", "target": args.target, "step": 2, "handoff_file": str(path), "mcp": mcp}
    if park and park["marker_node"]:
        raise TreeError(f"{label} is already parked and its marker node is recorded; use --resume to reopen it")
    if park is None:
        if not needs:
            raise TreeError("--needs is required to park")
        if holder["state"] in ("merged", "dropped", "finished"):
            raise TreeError(f"{label} is {holder['state']}; it cannot be parked")
        park = {"needs": needs, "at": ts.now_iso(), "marker_node": None, "prev_state": holder["state"]}
        holder["park"], holder["state"] = park, "parked"
        ts.save_manifest(tree_id, manifest)
    return {"verb": "park", "target": args.target, "step": 1,
            "mcp": [mcp_open(parent, "leaf", f"park: {label}", park["needs"])],
            "next": "call node_open, then rerun this command with --marker-id <the returned id>. If node_open "
                    "is refused for max_width (the parent already has its maximum of open children), close "
                    "one finished child of that parent first, then call node_open again: the park is already "
                    "recorded and rerunning this step returns the same payload"}


# --- finish -----------------------------------------------------------------------------------

def configure_finish(p) -> None:
    ts.add_tree_arg(p)
    p.add_argument("--summary", default=None, help="the root's summary (default: counts per outcome)")


def run_finish(args) -> dict:
    tree_id = ts.resolve_tree_id(args)
    manifest = ts.load_manifest(tree_id)
    if manifest["state"] == "finished" and manifest.get("finish"):
        return {"verb": "finish", "tree_id": tree_id, "again": True, **manifest["finish"]}
    if manifest["state"] == "parked":
        raise TreeError(f"tree {tree_id} is parked ({manifest['park']['needs']}): `park tree --resume` first")
    branches = manifest["branches"]
    if not branches:
        raise TreeError(f"tree {tree_id} has no branches to finish")
    pending = [f"{name} ({b['state']})" for name, b in branches.items() if b["state"] not in ts.TERMINAL_STATES]
    if pending:
        raise TreeError("cannot finish, branches not settled: " + ", ".join(pending))
    count = {s: [n for n, b in branches.items() if b["state"] == s] for s in ts.TERMINAL_STATES}
    prior = manifest.get("finish")
    if manifest.get("root_closed") and prior:
        if sorted(count["parked"]) == prior["parked"]:  # nothing changed: replay, the close may have failed
            return {"verb": "finish", "tree_id": tree_id, "again": True, **prior}
        # A parked branch was resumed and integrated after the root was closed blocked: the root stays
        # closed (a closed node is never reopened); only the local tree is settled.
        if not count["parked"]:
            manifest["state"] = "finished"
        manifest["finish"] = {**prior, "parked": sorted(count["parked"])}
        ts.save_manifest(tree_id, manifest)
        return {"verb": "finish", "tree_id": tree_id, "status": prior["status"], "root_already_closed": True,
                "mcp": [], "parked": sorted(count["parked"]),
                "note": "the root node was already closed blocked; nothing to close again"}
    parked = {n: branches[n]["park"]["needs"] if branches[n].get("park") else "parked" for n in count["parked"]}
    status = "blocked" if parked else "done"
    summary = args.summary or (f"{manifest['goal']}: {len(count['merged'])} merged, "
                               f"{len(count['dropped'])} dropped, {len(count['parked'])} parked")
    decisions = [f"branch {name} parked: {needs}" for name, needs in parked.items()]
    text = ts.render_handoff(
        node_id=tree_id, root_id=tree_id, status=status, role="orchestrator", outcome=status,
        summary=summary[:ts.SUMMARY_MAX], done=[f"{name} {b['state']}" for name, b in branches.items()],
        decisions=decisions, blocked_reason="; ".join(decisions) or "none")
    path = ts.write_handoff(tree_id, tree_id, text, overwrite=True)
    mcp = [mcp_update(tree_id, decision=d) for d in decisions]
    mcp.append(mcp_close(tree_id, status, summary, path))
    if parked:
        manifest["root_closed"] = True  # closed blocked, but the tree stays open: `--resume` still works
    else:
        manifest["state"] = "finished"
    manifest["finish"] = {"status": status, "handoff_file": str(path), "summary": summary[:ts.SUMMARY_MAX],
                          "parked": sorted(parked), "mcp": mcp}
    ts.save_manifest(tree_id, manifest)
    return {"verb": "finish", "tree_id": tree_id, **manifest["finish"]}


# --- verbs registry ----------------------------------------------------------------------------

def core_verbs() -> Verbs:
    verbs = Verbs()
    verbs["init"] = (configure_init, run_init)
    verbs["branch"] = (configure_branch, run_branch)
    verbs["prompt"] = (configure_prompt, run_prompt)
    verbs["status"] = (configure_status, run_status)
    verbs["review"] = (configure_review, run_review)
    verbs["merge"] = (configure_merge, run_merge)
    verbs["park"] = (configure_park, run_park)
    verbs["finish"] = (configure_finish, run_finish)
    return verbs


if __name__ == "__main__":
    sys.exit(main())
