"""tree.py record: validate a node report against the real diff and assign file ownership.

Spec 8.2. Runs in series (one record at a time): it reads and writes the tree manifest and the
branch worktree. It never calls the MCP and never commits: it returns the WIP commit to make and
the node_close payload, and the controller executes both.
"""
from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

import tree_state
from tree_state import TreeError

MAX_ATTEMPTS = 2  # the second invalid report fails the node

# --- Node list (output of the mcp node_tree tool, see the chunk 6 notes) ---

def load_nodes(path):
    """Read the node_tree JSON (object, flat list or nested tree) -> (root_id, flat node list)."""
    try:
        data = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise TreeError(f"cannot read the nodes file {path}: {exc}")
    root_id = None
    if isinstance(data, dict) and isinstance(data.get("nodes"), list):
        root_id = data.get("root_id")
        items = data["nodes"]
    elif isinstance(data, list):
        items = data
    elif isinstance(data, dict) and "id" in data:
        items = [data]
    else:
        raise TreeError(f"{path}: expected the node_tree object, a node list or a nested tree")
    flat = []

    def walk(item, parent):
        node = dict(item)
        kids = node.pop("children", None) or []
        if parent is not None and not node.get("parent_id"):
            node["parent_id"] = parent
        flat.append(node)
        for kid in kids:
            walk(kid, node["id"])

    for item in items:
        if not isinstance(item, dict) or "id" not in item:
            raise TreeError(f"{path}: a node without an id")
        walk(item, None)
    if root_id is None:
        roots = [n["id"] for n in flat if not n.get("parent_id")]
        if len(roots) != 1:
            raise TreeError(f"{path}: expected exactly one root node, found {len(roots)}")
        root_id = roots[0]
    return root_id, flat

def attach_owned(nodes, owned_file):
    """Fill `owned_paths` of nodes that lack it from the controller's --owned file.

    The file is {"<node_id>": [globs]} or [{"id": ..., "owned_paths": [...]}].
    """
    if not owned_file:
        return
    try:
        data = json.loads(Path(owned_file).read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise TreeError(f"cannot read the owned file {owned_file}: {exc}")
    if isinstance(data, list):
        data = {item["id"]: item.get("owned_paths") for item in data if "id" in item}
    if not isinstance(data, dict):
        raise TreeError(f"{owned_file}: expected an object or a list of nodes")
    for node in nodes:
        if node.get("owned_paths") is None and data.get(node["id"]) is not None:
            node["owned_paths"] = list(data[node["id"]])

def subtree_ids(nodes, top):
    """`top` and all its descendants, by parent_id."""
    kids = {}
    for node in nodes:
        kids.setdefault(node.get("parent_id"), []).append(node["id"])
    seen = {top}
    stack = [top]
    while stack:
        for child in kids.get(stack.pop(), []):
            if child not in seen:
                seen.add(child)
                stack.append(child)
    return seen

# --- Git layer ---

def _git(worktree, *args):
    proc = subprocess.run(
        ["git", "-C", str(worktree), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if proc.returncode != 0:
        raise TreeError(f"git {' '.join(args)} failed in {worktree}: {proc.stderr.strip()}")
    return proc.stdout

def diff_entries(worktree):
    """`git status --porcelain` against HEAD as [(XY, path)].

    -uall lists untracked files one by one instead of their directory. A staged rename or copy
    becomes an add of the new path and, for a rename, a delete of the old one.
    """
    out = _git(worktree, "status", "--porcelain=v1", "-z", "-uall")
    parts = out.split("\0")
    entries = []
    i = 0
    while i < len(parts):
        raw = parts[i]
        i += 1
        if len(raw) < 4:
            continue
        code, path = raw[:2], raw[3:]
        if code[0] in "RC":
            source = parts[i] if i < len(parts) else ""
            i += 1
            entries.append(("A ", path))
            if code[0] == "R" and source:
                entries.append(("D ", source))
        else:
            entries.append((code, path))
    return entries

def revert(worktree, code, path):
    """Put one file back to the branch HEAD: restore tracked, unstage+delete added, delete untracked."""
    root = Path(worktree)
    full = root / path
    if code == "??":
        full.unlink(missing_ok=True)
    elif code[0] == "A":
        _git(worktree, "rm", "-q", "-f", "--cached", "--", path)
        full.unlink(missing_ok=True)
    else:
        _git(worktree, "checkout", "HEAD", "--", path)
        return
    parent = full.parent
    while parent != root and parent.is_dir() and not any(parent.iterdir()):
        parent.rmdir()
        parent = parent.parent

# --- Report checks ---

def _norm(path):
    """A declared file as a repo-relative path: no backticks, no trailing ` (new)` annotation."""
    text = re.sub(r"\s+\([^)]*\)\s*$", "", str(path).strip())
    text = text.strip().strip("`").strip().replace("\\", "/")
    while text.startswith("./"):
        text = text[2:]
    return text

def _check_report(text, node_id, root_id):
    """Parse and validate the report; every problem is a TreeError (an invalid report)."""
    report = tree_state.parse_report(text)
    # `contract_used` is already a list of ids (`none` reads as empty).
    used = list(report.get("contract_used") or [])
    contract = tree_state.contract_path(root_id)
    known = None
    if contract.is_file():
        known = tree_state.contract_ids(contract.read_text(encoding="utf-8"))
    elif used:
        raise TreeError(f"contract_used {used} but there is no contract.md at {contract}")
    tree_state.validate_report(report, tree_state.load_config(), known)
    if report.get("node_id") != node_id:
        raise TreeError(
            f"the report is for node {report.get('node_id')!r}, not {node_id!r}"
        )
    return report

def _write_report(path, text):
    """File the report where node_close's handoff check looks, unless the node already did."""
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

def _close_call(node_id, status, summary, path):
    return {
        "tool": "node_close",
        "args": {
            "id": node_id,
            "status": status,
            "summary": (summary or "").strip()[: tree_state.SUMMARY_MAX],
            "handoff_path": str(path),
        },
    }

# --- Ownership (spec 8.2 table) ---

def recorded_nodes(manifest):
    """Nodes whose record was accepted or failed: their files are committed or reverted."""
    return {
        r["node_id"]
        for r in manifest.get("records") or []
        if r.get("verdict") in ("accepted", "failed")
    }

def classify(entries, node_id, declared, nodes, subtree, branch_node, recorded=frozenset()):
    """Apply the 8.2 table to every changed file. Pure: nothing is touched on disk.

    Only unrecorded leaves own files, whatever the daemon says about them: a leaf of the branch
    subtree with no accepted/failed record, whose status is not `handoff` (a relaunched
    predecessor yields its paths to the successor). A leaf that closed itself (`done`) but is not
    recorded yet still owns its diff; the node being recorded is always a candidate. Owned paths
    are disjoint, so a file has at most one owner. Orchestrator and review nodes edit no file, so
    both can be charged for a foreign write.
    """
    by_id = {n["id"]: n for n in nodes}
    live = [
        by_id[i]
        for i in subtree
        if by_id[i].get("kind") == "leaf"
        and (
            i == node_id
            or (i not in recorded and by_id[i].get("status") != "handoff")
        )
    ]
    for leaf in live:
        if leaf.get("owned_paths") is None:
            raise TreeError(
                f"owned_paths of live leaf {leaf['id']} are unknown: "
                "pass them in the nodes file or with --owned"
            )
    orchestrators = sorted(
        (by_id[i] for i in subtree if by_id[i].get("kind") in ("orchestrator", "review")),
        key=lambda n: (-int(n.get("depth") or 0), n["id"]),
    )

    def owner_of(path):
        owners = [
            leaf["id"]
            for leaf in live
            if any(tree_state.path_matches(path, g) for g in leaf["owned_paths"])
        ]
        if len(owners) > 1:
            raise TreeError(
                f"{path} is owned by several live leaves ({', '.join(sorted(owners))}): "
                "owned_paths must be disjoint"
            )
        return owners[0] if owners else None

    def charge(path):
        for orch in orchestrators:
            if any(tree_state.path_matches(path, g) for g in (orch.get("owned_paths") or [])):
                return orch["id"]
        return branch_node

    # Resolve every owner first so an ambiguity stops the record before anything is reverted.
    ordered = sorted(set(entries), key=lambda e: e[1])
    owners = {path: owner_of(path) for _, path in ordered}

    result = {
        "accepted": [],
        "warnings": [],
        "refused": [],
        "revert": [],
        "foreign": [],
        "failed": set(),
    }
    for code, path in ordered:
        owner = owners[path]
        if owner == node_id:
            result["accepted"].append(path)
            if path not in declared:
                result["warnings"].append(
                    f"{path}: changed in the branch but missing from the report's files_changed"
                )
        elif path in declared:
            result["refused"].append(path)
            result["failed"].add(node_id)
            if owner is None:  # no owner at all: revert; a sibling's file is never reverted
                result["revert"].append((code, path))
        elif owner is not None:
            continue  # another unrecorded leaf: that leaf's record decides
        else:
            charged = charge(path)
            result["foreign"].append({"file": path, "charged": charged})
            result["failed"].add(charged)
            result["revert"].append((code, path))
    return result

# --- The verb ---

def _read_report_text(args):
    if getattr(args, "report_file", None):
        try:
            return Path(args.report_file).read_text(encoding="utf-8")
        except OSError as exc:
            raise TreeError(f"cannot read the report file {args.report_file}: {exc}")
    return sys.stdin.buffer.read().decode("utf-8")

def _append_record(manifest, node_id, branch, verdict, result):
    manifest.setdefault("records", []).append(
        {
            "node_id": node_id,
            "branch": branch,
            "at": tree_state.now_iso(),
            "verdict": verdict,
            "accepted": result["accepted"],
            "warnings": result["warnings"],
            "refused": result["refused"],
            "reverted": result["reverted"],
            "foreign": result["foreign"],
        }
    )

def _invalid(manifest, root_id, branch, node_id, text, error, report_path, running):
    """An invalid report: one more attempt, no diff check, nothing reverted."""
    attempts = manifest.setdefault("attempts", {})
    attempts[node_id] = attempts.get(node_id, 0) + 1
    count = attempts[node_id]
    verdict = "retry" if count < MAX_ATTEMPTS else "failed"
    result = {
        "verdict": verdict,
        "accepted": [],
        "warnings": [],
        "refused": [],
        "reverted": [],
        "foreign": [],
        "failed_nodes": [node_id] if verdict == "failed" else [],
        "wip_commit": {"message": f"wip({branch}): record {node_id}", "paths": []},
        "mcp": [],
        "error": str(error),
        "attempts": count,
    }
    if verdict == "failed":
        _write_report(report_path, text.strip() or "(no report was returned)")
        if running:  # a node that already closed itself needs no node_close
            result["mcp"] = [
                _close_call(
                    node_id,
                    "failed",
                    f"report invalid {count} times: {error}",
                    report_path,
                )
            ]
    _append_record(manifest, node_id, branch, verdict, result)
    tree_state.save_manifest(root_id, manifest)
    return result

def run_record(args):
    """record <node_id> --branch B --nodes FILE [--report-file F] [--owned FILE]."""
    node_id = args.node_id
    root_id, nodes = load_nodes(args.nodes)
    attach_owned(nodes, getattr(args, "owned", None))
    by_id = {n["id"]: n for n in nodes}
    if node_id not in by_id:
        raise TreeError(f"node {node_id} is not in {args.nodes}")
    tree_id = tree_state.resolve_tree_id(args)
    if tree_id != root_id:
        raise TreeError(f"tree {tree_id!r} is not the root {root_id!r} of {args.nodes}")

    manifest = tree_state.load_manifest(root_id)
    branch = tree_state.get_branch(manifest, args.branch)
    worktree, branch_node = branch.get("worktree"), branch.get("node_id")
    if not worktree or not branch_node:
        raise TreeError(f"branch {args.branch} has no worktree or no node_id yet")
    if branch_node not in by_id:
        raise TreeError(f"the branch node {branch_node} is not in {args.nodes}")
    subtree = subtree_ids(nodes, branch_node)
    if node_id not in subtree:
        raise TreeError(f"node {node_id} is not under the branch node {branch_node}")

    report_path = tree_state.handoff_file(root_id, node_id)
    # A leaf that closed itself (direct launch) must not get a second node_close.
    running = by_id[node_id].get("status") == "running"
    text = _read_report_text(args)
    try:
        report = _check_report(text, node_id, root_id)
    except TreeError as exc:
        return _invalid(manifest, root_id, args.branch, node_id, text, exc, report_path, running)

    declared = {_norm(f) for f in (report.get("files") or [])}
    verdicts = classify(
        diff_entries(worktree), node_id, declared, nodes, subtree, branch_node,
        recorded_nodes(manifest),
    )
    reverted = []
    for code, path in verdicts["revert"]:
        revert(worktree, code, path)
        reverted.append(path)

    failed_nodes = sorted(verdicts["failed"])
    verdict = "failed" if node_id in verdicts["failed"] else "accepted"
    result = {
        "verdict": verdict,
        "accepted": verdicts["accepted"],
        "warnings": verdicts["warnings"],
        "refused": verdicts["refused"],
        "reverted": reverted,
        "foreign": verdicts["foreign"],
        "failed_nodes": failed_nodes,
        "wip_commit": {
            "message": f"wip({args.branch}): record {node_id}",
            "paths": verdicts["accepted"],
        },
    }

    _write_report(report_path, text)
    status = report.get("status") or tree_state.OUTCOME_STATUS.get(report.get("outcome"), "done")
    summary = report.get("summary") or ""
    if verdict == "failed":
        status = "failed"
        problems = verdicts["refused"] or [f["file"] for f in verdicts["foreign"]]
        summary = f"{summary} [record: {', '.join(problems)}]"
    result["mcp"] = [_close_call(node_id, status, summary, report_path)] if running else []

    _append_record(manifest, node_id, args.branch, verdict, result)
    tree_state.save_manifest(root_id, manifest)
    return result

# --- CLI registration (tree.py calls tree_record.register(verbs)) ---

def add_arguments(parser):
    tree_state.add_tree_arg(parser)
    parser.add_argument("node_id", help="the node whose report is being recorded")
    parser.add_argument("--branch", required=True, help="branch name in the tree manifest")
    parser.add_argument(
        "--nodes",
        required=True,
        help="JSON file with the mcp node_tree output for the root (a list or a tree also works)",
    )
    parser.add_argument(
        "--report-file", default=None, help="read the report from this file instead of stdin"
    )
    parser.add_argument(
        "--owned",
        default=None,
        help='fallback: JSON {"<node_id>": [globs]} for owned_paths the node_tree output lacks',
    )

def register(verbs):
    verbs["record"] = (add_arguments, run_record)
