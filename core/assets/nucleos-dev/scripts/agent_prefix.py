#!/usr/bin/env python3
"""Consume a generated Codex agent prefix when dispatching a pipeline node.

The prefix file (`<prefix_dir>/<agent>.json`, schema 1) is written by the core from an agent
package; this module only reads it and turns it into the stdin and the `codex exec` command of one
fan-out node. `pipeline_fanout.py` stays generic: `build_node` returns a plain
`{"id", "cmd", "stdin", "timeout"}` node it accepts unchanged.

Callers pass `<state_dir>/agent-prefix` as `prefix_dir`; where that directory lives is the
caller's decision, not this module's.

Ceiling limitation: for Codex, capabilities are a WRITE CEILING, not an exact tool list. A
`read-only` sandbox still allows shell reads, and `workspace-write` gives write and shell
together. The bypass flag `--dangerously-bypass-approvals-and-sandbox` is never emitted.

An agent with no generated file (`load_prefix` returns None) is the caller's signal to keep the
legacy path.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

SCHEMA = 1
ANCESTOR_OUTPUT_CHAR_LIMIT = 2000


def load_prefix(prefix_dir: Path, agent_name: str) -> dict[str, Any] | None:
    """Read `<prefix_dir>/<agent_name>.json`; None when absent, ValueError on a foreign schema."""
    path = Path(prefix_dir) / f"{agent_name}.json"
    if not path.is_file():
        return None
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or data.get("schema") != SCHEMA:
        found = data.get("schema") if isinstance(data, dict) else None
        raise ValueError(f"{path}: unsupported prefix schema {found!r}, expected {SCHEMA}")
    return data


def build_stdin(
    prefix: dict[str, Any],
    task: str,
    ancestors: list[tuple[str, str, str]],
    limit: int = ANCESTOR_OUTPUT_CHAR_LIMIT,
) -> str:
    """Prefix, then the task, then ancestor outputs (id, agent, output) cut to `limit` chars."""
    parts = [prefix["prefix"], "\n\n## Task\n", task]
    if ancestors:
        parts.append("\n\nRelevant context from prior steps:")
        for node_id, agent, output in ancestors:
            parts.append(f"\n  - {node_id} ({agent}):\n    {output[:limit]}")
    return "".join(parts)


def _quoted(value: str) -> str:
    # JSON string form, as codex.rs builds its `-c key="value"` overrides.
    return json.dumps(value)


def build_node(
    node_id: str,
    codex_bin: str,
    root: str | Path,
    prefix: dict[str, Any],
    model: str,
    effort: str,
    timeout: int,
    stdin: str,
) -> dict[str, Any]:
    """One fan-out node whose sandbox and web search come from the prefix, never a bypass."""
    web = "live" if prefix.get("web_search") else "disabled"
    cmd = [
        codex_bin,
        "exec",
        "--skip-git-repo-check",
        "-m",
        model,
        "-c",
        f"model_reasoning_effort={_quoted(effort)}",
        "-c",
        f"sandbox_mode={_quoted(prefix['sandbox'])}",
        "-c",
        f"web_search={_quoted(web)}",
        "-C",
        str(root),
    ]
    return {"id": node_id, "cmd": cmd, "stdin": stdin, "timeout": timeout}
