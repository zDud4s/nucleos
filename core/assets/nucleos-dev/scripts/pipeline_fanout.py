"""Run a JSON-described set of subprocess nodes in parallel.

Reads a spec from stdin, executes each node command with isolated error
handling, and writes an ordered JSON list of per-node results to stdout.
"""
from __future__ import annotations

import concurrent.futures
import json
import subprocess
import sys
import time
from typing import Any


DEFAULT_TIMEOUT = 600
DEFAULT_MAX_PARALLEL = 8


def _require_spec(spec: Any) -> tuple[list[dict[str, Any]], int]:
    if not isinstance(spec, dict):
        raise ValueError("spec must be a JSON object")
    if "nodes" not in spec:
        raise ValueError("missing key 'nodes'")

    nodes = spec["nodes"]
    if not isinstance(nodes, list):
        raise ValueError("'nodes' must be a list")
    if not nodes:
        # An empty fan-out is almost always an upstream construction bug that
        # produced no nodes; reject it (mirroring pipeline_schema.py) rather
        # than silently returning [] with exit 0.
        raise ValueError("'nodes' must be a non-empty list")

    max_parallel = spec.get("max_parallel", DEFAULT_MAX_PARALLEL)
    # bool is a subclass of int, so `True`/`False` would pass isinstance(int);
    # reject explicitly so `"max_parallel": true` isn't silently read as 1.
    if isinstance(max_parallel, bool) or not isinstance(max_parallel, int) or max_parallel < 1:
        raise ValueError("'max_parallel' must be a positive integer")

    seen: set[str] = set()
    for index, node in enumerate(nodes):
        if not isinstance(node, dict):
            raise ValueError(f"node #{index} must be an object")
        if "id" not in node:
            raise ValueError(f"node #{index} missing key 'id'")
        if "cmd" not in node:
            raise ValueError(f"node #{index} missing key 'cmd'")
        if not isinstance(node["id"], str):
            raise ValueError(f"node #{index} id must be a string")
        if node["id"] in seen:
            raise ValueError(f"node #{index} duplicate id '{node['id']}'")
        seen.add(node["id"])
        if not isinstance(node["cmd"], list) or not all(
            isinstance(part, str) for part in node["cmd"]
        ):
            raise ValueError(f"node #{index} cmd must be a list of strings")
        if "stdin" in node and not isinstance(node["stdin"], str):
            raise ValueError(f"node #{index} stdin must be a string")
        if "timeout" in node and (
            isinstance(node["timeout"], bool)
            or not isinstance(node["timeout"], int)
            or node["timeout"] < 1
        ):
            raise ValueError(f"node #{index} timeout must be a positive integer")

    return nodes, max_parallel


def _text_or_empty(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode(errors="replace")
    return str(value)


def _run_node(node: dict[str, Any]) -> dict[str, Any]:
    node_id = node["id"]
    started = time.perf_counter()
    try:
        completed = subprocess.run(
            node["cmd"],
            input=node.get("stdin"),
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=node.get("timeout", DEFAULT_TIMEOUT),
        )
        status = "ok" if completed.returncode == 0 else "error"
        return {
            "id": node_id,
            "status": status,
            "exit_code": completed.returncode,
            "stdout": completed.stdout,
            "stderr": completed.stderr,
            "error": None,
            "duration_s": time.perf_counter() - started,
        }
    except subprocess.TimeoutExpired as exc:
        return {
            "id": node_id,
            "status": "timeout",
            "exit_code": None,
            "stdout": _text_or_empty(exc.stdout),
            "stderr": _text_or_empty(exc.stderr),
            "error": str(exc),
            "duration_s": time.perf_counter() - started,
        }
    except Exception as exc:
        return {
            "id": node_id,
            "status": "error",
            "exit_code": None,
            "stdout": "",
            "stderr": "",
            "error": str(exc),
            "duration_s": time.perf_counter() - started,
        }


def run_fanout(spec: dict[str, Any]) -> list[dict[str, Any]]:
    nodes, max_parallel = _require_spec(spec)
    results: list[dict[str, Any] | None] = [None] * len(nodes)
    with concurrent.futures.ThreadPoolExecutor(max_workers=max_parallel) as pool:
        future_to_index = {
            pool.submit(_run_node, node): index for index, node in enumerate(nodes)
        }
        for future in concurrent.futures.as_completed(future_to_index):
            results[future_to_index[future]] = future.result()
    return [result for result in results if result is not None]


def main() -> int:
    try:
        spec = json.loads(sys.stdin.read())
        results = run_fanout(spec)
    except Exception as exc:
        print(f"pipeline_fanout error: {exc}", file=sys.stderr)
        return 1

    json.dump(results, sys.stdout)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
