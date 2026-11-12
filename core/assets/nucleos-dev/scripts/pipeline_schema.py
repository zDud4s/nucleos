"""Pure-Python validator for pipeline YAML.

Used by the run-pipeline skill (pre-flight load-time validation).

A pipeline is a DAG of nodes. Each node is either an AGENT node (`agent:
<subagent_type>`) or a FLOW node (`kind:`). There is exactly one `kind: input`
source node and exactly one sink node (`kind` in synthesize/collect/passthrough);
the sink's kind decides how the final result is produced and its `depends_on`
edges decide what feeds it.

The function accepts an already-parsed Python dict (the caller runs
yaml.safe_load); returns (ok: bool, errors: list[str]). All errors share the
prefix `pipeline invalid:`.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path
from typing import Any

SINK_KINDS = ("synthesize", "collect", "passthrough")
FLOW_KINDS = ("input",) + SINK_KINDS


def validate(pipeline: dict[str, Any]) -> tuple[bool, list[str]]:
    """Validate the parsed-YAML shape of a pipeline against the schema."""
    errs: list[str] = []

    if not isinstance(pipeline, dict):
        return (False, ["pipeline invalid: pipeline must be a mapping"])

    if "nodes" not in pipeline:
        errs.append("pipeline invalid: missing key 'nodes'")

    nodes = pipeline.get("nodes")
    if nodes is not None and (not isinstance(nodes, list) or not nodes):
        errs.append("pipeline invalid: nodes must be a non-empty list")
        nodes = []
    elif nodes is None:
        nodes = []

    # Per-node shape: id + exactly one of agent/kind; valid kind value.
    seen: set[str] = set()
    for i, node in enumerate(nodes):
        if not isinstance(node, dict):
            errs.append(f"pipeline invalid: node #{i} must be a mapping")
            continue
        node_id = node.get("id")
        if not isinstance(node_id, str) or not node_id:
            errs.append(f"pipeline invalid: node #{i} missing id")
            continue
        if node_id in seen:
            errs.append(f"pipeline invalid: duplicate id '{node_id}'")
        seen.add(node_id)

        has_agent = isinstance(node.get("agent"), str) and bool(node.get("agent"))
        has_kind = "kind" in node
        if has_agent == has_kind:  # both or neither
            errs.append(
                f"pipeline invalid: node '{node_id}' must have either "
                "'agent' or 'kind', not both"
            )
        if has_kind and node.get("kind") not in FLOW_KINDS:
            errs.append(
                f"pipeline invalid: node '{node_id}' has unknown kind "
                f"'{node.get('kind')}'"
            )

    # Well-formed nodes only, for the structural passes below.
    valid = [
        n for n in nodes
        if isinstance(n, dict) and isinstance(n.get("id"), str) and n.get("id")
    ]
    ids = {n["id"] for n in valid}

    # depends_on resolution.
    for node in valid:
        deps = node.get("depends_on")
        if deps is None:
            continue
        if not isinstance(deps, list):
            errs.append(
                f"pipeline invalid: depends_on for '{node['id']}' must be a list"
            )
            continue
        for d in deps:
            if not isinstance(d, str) or d not in ids:
                errs.append(
                    f"pipeline invalid: '{node['id']}' depends on unknown '{d}'"
                )

    # Dependents map (parent id -> list of child ids).
    dependents: dict[str, list[str]] = {n["id"]: [] for n in valid}
    for n in valid:
        for d in (n.get("depends_on") or []):
            if isinstance(d, str) and d in dependents:
                dependents[d].append(n["id"])

    input_nodes = [n for n in valid if n.get("kind") == "input"]
    sink_nodes = [n for n in valid if n.get("kind") in SINK_KINDS]

    if len(input_nodes) != 1:
        errs.append(
            f"pipeline invalid: pipeline must have exactly one input node "
            f"(found {len(input_nodes)})"
        )
    if len(sink_nodes) != 1:
        errs.append(
            f"pipeline invalid: pipeline must have exactly one sink node "
            f"(found {len(sink_nodes)})"
        )

    for n in input_nodes:
        if n.get("depends_on"):
            errs.append(
                f"pipeline invalid: input node '{n['id']}' must not depend on anything"
            )
        if not dependents.get(n["id"]):
            errs.append(
                f"pipeline invalid: input node '{n['id']}' has no downstream nodes"
            )

    for n in sink_nodes:
        deps = n.get("depends_on") or []
        if not deps:
            errs.append(f"pipeline invalid: sink node '{n['id']}' has no inputs")
        if dependents.get(n["id"]):
            errs.append(f"pipeline invalid: sink node '{n['id']}' must be terminal")
        if n.get("kind") == "passthrough" and len(deps) != 1:
            errs.append(
                f"pipeline invalid: passthrough sink '{n['id']}' must have "
                "exactly one input"
            )

    # Cycle + orphan checks only when the graph is otherwise well-formed.
    if not errs:
        children = {n["id"]: list(dependents[n["id"]]) for n in valid}
        in_deg = {n["id"]: len(n.get("depends_on") or []) for n in valid}
        ready = [nid for nid, deg in in_deg.items() if deg == 0]
        stack = list(ready)
        visited = 0
        while stack:
            nid = stack.pop()
            visited += 1
            for child in children[nid]:
                in_deg[child] -= 1
                if in_deg[child] == 0:
                    stack.append(child)
        if visited != len(valid):
            errs.append("pipeline invalid: cycle detected")
        elif len(input_nodes) == 1 and len(sink_nodes) == 1:
            inp, snk = input_nodes[0]["id"], sink_nodes[0]["id"]
            parents = {n["id"]: list(n.get("depends_on") or []) for n in valid}
            from_input = _reach(inp, children)
            to_sink = _reach(snk, parents)
            for n in valid:
                if n.get("kind"):  # only agent nodes must be on a path
                    continue
                nid = n["id"]
                if nid not in from_input or nid not in to_sink:
                    errs.append(
                        f"pipeline invalid: node '{nid}' is not connected "
                        "between input and sink"
                    )

    return (not errs, errs)


def _reach(start: str, adj: dict[str, list[str]]) -> set[str]:
    """All nodes reachable from `start` following adjacency `adj` (inclusive)."""
    seen: set[str] = set()
    stack = [start]
    while stack:
        x = stack.pop()
        if x in seen:
            continue
        seen.add(x)
        for nxt in adj.get(x, []):
            stack.append(nxt)
    return seen

# ---- CLI: `python pipeline_schema.py <name>` -------------------------------------------------

SLUG_RE = re.compile(r"^[a-z0-9-]+$")


def _load_file(name: str) -> tuple[Path | None, list[str]]:
    """Resolve and parse `<state_dir>/pipelines/<name>.yaml`; (path, errors)."""
    if not SLUG_RE.match(name):
        return None, [f"pipeline invalid: name '{name}' must match ^[a-z0-9-]+$"]
    import nucleos_paths
    import yaml

    try:
        state = nucleos_paths.state_dir()
    except nucleos_paths.ProjectNotFound as exc:
        return None, [f"pipeline invalid: {exc}"]
    path = (state / "pipelines" / f"{name}.yaml").resolve()
    try:
        data = yaml.safe_load(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None, [f"pipeline invalid: {path} not found"]
    except (OSError, UnicodeError, yaml.YAMLError) as exc:
        return None, [f"pipeline invalid: {path} is unreadable: {exc}"]
    ok, errs = validate(data)
    return (path if ok else None), errs


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 1:
        print("usage: pipeline_schema.py <name>", file=sys.stderr)
        return 2
    path, errs = _load_file(args[0])
    if errs:
        for err in errs:
            print(err, file=sys.stderr)
        return 1
    print(path)
    return 0


if __name__ == "__main__":
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    raise SystemExit(main())