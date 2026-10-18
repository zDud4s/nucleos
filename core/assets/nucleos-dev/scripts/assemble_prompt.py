#!/usr/bin/env python3
"""Assemble a dispatched phase's prompt from the contract, and emit the dispatch that uses it.

## The problem this exists to remove

A controller that assembles an archive by hand and then composes the Task-tool prompt by hand
produces two artifacts that must agree and have nothing forcing them to: a `plan` archive once
carried a memory slice that was never sent, and an `execute` archive carried 59 memory entries
where the dispatch carried 20. This script is the mechanism: it writes the archive, and the
dispatch it prints is a POINTER to that archive plus its sha256. There is one artifact; the
prompt the phase reads and the prompt anything downstream checks are the same bytes.

## Why a pointer, and why that is not what dispatch.md forbids

`dispatch.md` forbids bare instructions like "use the planner skill" because a subprocess does
not inherit the session. In `dispatcher` mode the archive IS the stdin file, byte-identical by
construction (`--emit dispatcher`). In `agent` mode the reason does not apply -- a subagent shares
the controller's filesystem and every cockpit has `Read` -- and a pointer to an exact path pins
the content to bytes where "use the skill" leaves it to discovery.

## What it refuses to do

Assembly stops, and no file is written, when the result would violate the contract: a section
not in the phase's `allowed`, a missing `required`, anything in `never`, a `never_fingerprint`
anywhere in the body, a memory tag outside `memory.md`'s closed vocabulary, or a non-empty tag
list that selects no entries.

`memory_lines` is COUNTED here, from the slice this script itself produced, and the whole manifest
is written to `<archive>.manifest.json` instead of being printed: no value in it passes through a
model's hands on the way to the record the controller writes (`mcp__nucleos__record`, kind
`metric`, with the manifest's values in the payload).

Where things are: the project root is the git toplevel of the current directory
(`CLAUDE_PROJECT_DIR` or `NUCLEOS_PROJECT_ROOT` override it); `.nucleos/workflow/project.json`
there names `state_dir`, which holds `project.yaml`, `memory.md`, `decisions.md` and the
`prompts/` archive directory. The contract and packet schemas are under `.nucleos/workflow/`.

## Usage

    python <bundle_dir>/scripts/assemble_prompt.py --phase execute --slug my-task \\
        --objective /path/to/objective.md \\
        --packet .nucleos/workflow/packets/execute.md \\
        --memory-tags workflow,dispatch \\
        --prior-artifacts /path/to/plan-handoff.md

Writes the archive and its manifest, then prints the archive path, the sha256 and the exact
dispatch text. `--emit dispatcher` prints the stdin recipe instead of the pointer.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import sys
from pathlib import Path

import nucleos_paths


def sidecar_path(archive: Path) -> Path:
    """Where an archive's manifest goes: `x.md` -> `x.manifest.json` (not `*.md`, so it is never
    mistaken for a prompt)."""
    return archive.with_suffix(".manifest.json")

# Resolved by `locate()` at the start of main() (and by tests): nothing is read at import time, so
# importing this module never needs a project.
REPO_ROOT: Path
WORKFLOW: Path
STATE: Path
CONTRACT: Path
MEMORY: Path
DECISIONS: Path
PROJECT: Path
SKILLS: Path


def locate(root: Path | None = None) -> None:
    global REPO_ROOT, WORKFLOW, STATE, CONTRACT, MEMORY, DECISIONS, PROJECT, SKILLS
    REPO_ROOT = root or nucleos_paths.project_root()
    WORKFLOW = nucleos_paths.workflow_dir(REPO_ROOT)
    STATE = nucleos_paths.state_dir(REPO_ROOT)
    CONTRACT = WORKFLOW / "prompt-contract.yaml"
    MEMORY = STATE / "memory.md"
    DECISIONS = STATE / "decisions.md"
    PROJECT = STATE / "project.yaml"
    SKILLS = REPO_ROOT / ".claude" / "skills"


def shown(path: Path) -> str:
    """A path as the controller should see it: relative to the root when inside it."""
    try:
        return path.relative_to(REPO_ROOT).as_posix()
    except ValueError:
        return path.as_posix()


# CRLF, matching every archive already in `<state_dir>/prompts/`.
# Written through `write_bytes` only: `write_text` translates on the way out and `read_text`
# translates on the way in, so a text-mode round trip silently converts the file to LF and the
# sha recorded here would stop matching the sha on disk.
NL = b"\r\n"

# One-to-one, from dispatch.md's "The phase cockpit" table and its phase-to-skill note. The
# three orchestrate-tdd phases are `execute` under different objectives, so they take the
# executor's skill -- exactly as the contract gives them the executor's sections.
PHASE_SKILL = {
    "plan": "planner",
    "execute": "executor",
    "review": "reviewer",
    "rescue": "rescue",
    "maintenance": "maintenance",
    "bootstrap": "bootstrap",
    "red": "executor",
    "green": "executor",
    "refactor": "executor",
}

PHASE_COCKPIT = {
    "plan": "wf-planner",
    "execute": "wf-executor",
    "review": "wf-reviewer",
    "rescue": "wf-rescue",
    "maintenance": "wf-maintenance",
    "bootstrap": "wf-bootstrap",
    "red": "wf-executor",
    "green": "wf-executor",
    "refactor": "wf-executor",
}

ENTRY = re.compile(r"^- (\d{4}-\d{2}-\d{2}) \[([a-z-]+)\]")

# A Read with no explicit range stops at 2000 lines. An archive longer than that would be
# silently truncated for the phase while the sha still matched the whole file -- the manifest
# would be honest and the cockpit would not have what it claims. Refuse instead.
MAX_LINES = 1800


class Stop(Exception):
    """A contract violation. Nothing is written when one is raised."""


def load_contract() -> dict:
    try:
        import yaml
    except ImportError:  # pragma: no cover - PyYAML ships with the toolchain here
        raise Stop("PyYAML is required to read prompt-contract.yaml")
    return yaml.safe_load(CONTRACT.read_bytes().decode("utf-8"))


def read(path: Path) -> str:
    """Read a file as text without letting Python touch its line endings."""
    try:
        return path.read_bytes().decode("utf-8")
    except FileNotFoundError:
        raise Stop(f"{shown(path)} does not exist") from None


def vocabulary() -> set[str]:
    """The closed tag list from memory.md's `## Topic vocabulary` block.

    Canonical and legacy names both count: the orchestrate skill's check 2 accepts either, and
    consolidation is what retires a legacy name once its last entry is gone. The block holds two
    FENCED, space-separated lists (canonical, then legacy `old -> new` pairs).
    """
    text = read(MEMORY)
    start = text.find("## Topic vocabulary")
    if start < 0:
        raise Stop("memory.md has no `## Topic vocabulary` block -- the tag list is not closed")
    end = text.find("## Entries", start)
    block = text[start:end if end > 0 else len(text)]
    fenced = re.findall(r"```(.*?)```", block, re.S)
    if len(fenced) < 2:
        raise Stop("memory.md's vocabulary block needs two fenced lists (canonical, then legacy)")
    legacy = set(re.findall(r"(\S+)\s*(?:\u2192|->)", fenced[1]))
    return {tag for tag in set(fenced[0].split()) | legacy if tag}


def memory_header() -> str:
    """Everything through `## Entries`, so a slice reads as a well-formed memory.md.

    The orchestrate skill requires the header "so the dispatched phase understands the entry
    format and the closed tag list" -- a phase handed bare entries cannot tell a valid tag from
    one it just invented.
    """
    out = []
    for line in read(MEMORY).splitlines():
        out.append(line)
        if line.startswith("## Entries"):
            break
    return "\n".join(out)


def memory_slice(tags: list[str]) -> tuple[str, int]:
    """The header plus every entry whose topic tag is in `tags`. Returns (text, entry count)."""
    known = vocabulary()
    for tag in tags:
        if tag not in known:
            raise Stop(
                f"unknown memory tag {tag!r} -- not in memory.md's `## Topic vocabulary`. "
                f"The vocabulary is closed; a tag outside it matches no entry, and the slice "
                f"would be silently empty."
            )
    wanted = set(tags)
    picked = [ln for ln in read(MEMORY).splitlines()
              if (m := ENTRY.match(ln)) and m.group(2) in wanted]
    if not picked:
        raise Stop(
            f"memory slice empty for tags {tags} -- every tag is in the vocabulary but no "
            f"entry carries one. Naming a topic that exists and holds nothing is the failure "
            f"the full-memory fallback used to hide."
        )
    return memory_header() + "\n\n" + "\n".join(picked), len(picked)


def memory_index() -> str:
    """The header plus a per-tag entry COUNT -- never the entries themselves.

    `plan` chooses the tags every later phase is sliced by, and the contract handed it the
    WHOLE file for that, on the reasoning that it "cannot choose from a slice of its own".
    True, and it does not follow that it needs the text: choosing a tag needs the closed tag
    list and how much each tag holds. Measured 2026-09-18 the file was 667 entries and ~49k
    tokens -- a fifth of the 250k session ceiling CLAUDE.md fixes, spent before the planner
    read one line of code, on every medium/large task.

    The entries stay one grep away, deliberately: `.claude/agents/wf-planner.md` gives the
    planner Read and Grep, so an inventory plus the exact recipe is strictly MORE precise
    than the whole corpus -- it reads the tags it judges relevant and nothing else. The
    recipe is printed rather than described because a planner guessing at the pattern is the
    silently-empty grep the closed vocabulary exists to prevent.
    """
    counts: dict = {}
    total = 0
    for line in read(MEMORY).splitlines():
        if match := ENTRY.match(line):
            tag = match.group(2)
            counts[tag] = counts.get(tag, 0) + 1
            total += 1
    if not total:
        raise Stop(
            "memory.md carries no entries -- a plan phase asking for the index would be "
            "choosing tags from an empty inventory, and every later slice would be empty too"
        )
    widest = max(len(tag) for tag in counts)
    rows = [
        "  " + tag.ljust(widest) + "  " + str(counts[tag]).rjust(4)
        for tag in sorted(counts, key=lambda t: (-counts[t], t))
    ]
    parts = [
        memory_header(),
        "",
        "<!-- INVENTORY, not entries: " + str(total) + " entries across "
        + str(len(counts)) + " tags. -->",
        "",
        "```",
        *rows,
        "```",
        "",
        "Read the entries behind a tag with:",
        "",
        "```",
        "grep -E '^- [0-9-]+ \\[(tag1|tag2)\\]' " + MEMORY.as_posix(),
        "```",
        "",
        "Every tag above is in the closed vocabulary. A tag outside it matches nothing and "
        "greps silently empty, so name only these.",
        "",
    ]
    return "\n".join(parts)


def phase_skill_body(phase: str) -> str:
    name = PHASE_SKILL[phase]
    path = SKILLS / name / "SKILL.md"
    if not path.is_file():
        raise Stop(
            f"{shown(path)} not found -- the `{phase}` phase's skill. Do NOT "
            f"substitute the `claude`/`codex` bridge skill: it is named after a tool, is the "
            f"controller's invocation manual, and is in the contract's `never` list."
        )
    body = path.read_bytes().decode("utf-8")
    # Frontmatter is stripped: it arrives at a subagent as prompt text, and its `tools:` line is
    # inert there. The cockpit file is what the harness enforces, and it is named instead.
    if body.startswith("---"):
        end = body.find("\n---", 3)
        if end != -1:
            body = body[body.find("\n", end + 1) + 1:]
    cockpit = PHASE_COCKPIT[phase]
    note = (f"<!-- Cockpit: `.claude/agents/{cockpit}.md`. Its `tools:` line is the wall the "
            f"harness\n     enforces; it is not repeated here, because a skill's `tools:` "
            f"frontmatter is inert. -->")
    return note + "\n\n" + body.lstrip("\n")


def build(args, contract: dict) -> tuple[dict[str, str], list[str], int]:
    """Collect the sections this dispatch will carry. Returns (sections, order, memory_lines)."""
    phase = args.phase
    spec = (contract["phases"] or {}).get(phase)
    if spec is None:
        raise Stop(f"phase {phase!r} is not in prompt-contract.yaml")
    allowed = set(spec.get("allowed") or [])
    required = set(spec.get("required") or [])

    sections: dict[str, str] = {}
    memory_lines = 0

    sections["phase_skill"] = phase_skill_body(phase)

    if args.packet:
        bodies = []
        for raw in args.packet:
            path = (REPO_ROOT / raw) if not Path(raw).is_absolute() else Path(raw)
            if not path.is_file():
                raise Stop(f"packet schema {raw} does not exist")
            bodies.append(f"### {path.name}\n\n" + read(path))
        sections["packet_schema"] = "\n\n".join(bodies)

    # Bootstrap is the phase that writes project.yaml, so on a fresh project it runs without one;
    # every other phase that carries it stops when it is missing.
    if "project_yaml" in allowed and (PROJECT.is_file() or phase != "bootstrap"):
        sections["project_yaml"] = "```yaml\n" + read(PROJECT).rstrip("\n") + "\n```"

    if args.intent:
        path = Path(args.intent)
        if not path.is_file():
            raise Stop(f"intent {args.intent} does not exist")
        sections["intent"] = read(path)

    sections["objective"] = read(Path(args.objective))

    if args.memory_tags:
        tags = [t.strip() for t in args.memory_tags.split(",") if t.strip()]
        if not tags:
            raise Stop("`--memory-tags` was given but parsed to an empty list; "
                       "`Memory tags: []` is itself invalid planner output")
        sections["memory_slice"], memory_lines = memory_slice(tags)

    if args.memory_index:
        sections["memory_index"] = memory_index()

    if args.memory_full:
        sections["memory_full"] = read(MEMORY)

    if args.decisions:
        if not DECISIONS.is_file():
            raise Stop(f"{shown(DECISIONS)} does not exist")
        sections["decisions"] = read(DECISIONS)

    if args.prior_artifacts:
        bodies = []
        for raw in args.prior_artifacts:
            path = Path(raw)
            if not path.is_file():
                raise Stop(f"prior artifact {raw} does not exist")
            bodies.append(read(path))
        sections["prior_artifacts"] = "\n\n".join(bodies)

    never = set(contract["never"])
    used = set(sections)
    for leaked in sorted(used & never):
        raise Stop(f"`{leaked}` is in the contract's `never` list -- addressed to the "
                   f"controller, never to a phase")
    for extra in sorted(used - allowed - never):
        raise Stop(f"`{extra}` is not allowed for `{phase}`. Allowed: {sorted(allowed)}")
    for missing in sorted(required - used):
        raise Stop(f"`{phase}` requires `{missing}` and it was not supplied")

    order = [k for k in contract["order"] if k in sections]
    if set(order) != used:  # a section key the contract's `order` does not mention
        raise Stop(f"sections not in the contract's `order`: {sorted(used - set(order))}")
    return sections, order, memory_lines


def render(phase: str, sections: dict[str, str], order: list[str], marker: str) -> bytes:
    out = [f"# Dispatched phase: {phase}", ""]
    for key in order:
        out += ["", marker.format(key=key), "", sections[key].rstrip("\n"), ""]
    text = "\n".join(out).rstrip("\n") + "\n"
    # Normalize FIRST. `read()` deliberately does not translate line endings, so the section
    # bodies arrive with the CRLF their source files carry; joining those on CRLF without
    # flattening them produces `\r\r\n` on every one of those lines. Bare `\r` is folded too --
    # it is the ending an old Mac-era file or a bad rewrite leaves behind, and it would
    # otherwise survive into a file this script calls pure CRLF.
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    return NL.decode("ascii").join(text.split("\n")).encode("utf-8")


def fingerprint_scan(body: bytes, contract: dict) -> None:
    text = body.decode("utf-8")
    for key, needles in sorted((contract.get("never_fingerprints") or {}).items()):
        for needle in needles:
            if needle in text:
                raise Stop(
                    f"the assembled prompt contains {needle!r}, a fingerprint of `{key}` -- "
                    f"forbidden in any dispatched prompt, marker or no marker"
                )


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--phase", required=True, choices=sorted(PHASE_SKILL))
    p.add_argument("--slug", required=True, help="task slug, same across every phase of a task")
    p.add_argument("--n", type=int, default=1, help="iteration number for this phase")
    p.add_argument("--objective", required=True, help="file holding the task / filled objective")
    p.add_argument("--packet", action="append", help="packet schema path (repeatable)")
    p.add_argument("--intent", help="the task's filled, confirmed intent "
                                    "(medium/large or elevated risk; allowed for plan and review)")
    p.add_argument("--memory-tags", help="comma-separated, from the planner's `Memory tags:` line")
    p.add_argument("--memory-index", action="store_true",
                   help="memory.md header + a per-tag entry count: what `plan` needs to "
                        "choose tags, without the ~49k tokens of entries behind them")
    p.add_argument("--memory-full", action="store_true", help="whole memory.md (plan/maintenance)")
    p.add_argument("--decisions", action="store_true")
    p.add_argument("--prior-artifacts", action="append", help="previous phase output (repeatable)")
    p.add_argument("--emit", choices=["agent", "dispatcher"], default="agent")
    p.add_argument("--force", action="store_true", help="overwrite an existing archive")
    args = p.parse_args()

    try:
        locate()
    except nucleos_paths.ProjectNotFound as exc:
        print(f"STOP: {exc}", file=sys.stderr)
        return 2
    try:
        contract = load_contract()
        sections, order, memory_lines = build(args, contract)
        archive = contract.get("prompt_archive") or {}
        marker = archive.get("marker") or "<!-- section: {key} -->"
        body = render(args.phase, sections, order, marker)
        fingerprint_scan(body, contract)

        lines = body.count(NL) + 1
        if lines > MAX_LINES:
            raise Stop(
                f"the assembled prompt is {lines} lines; a Read with no explicit range stops "
                f"at 2000, so a phase pointed at this file would silently get less than the "
                f"manifest claims. Narrow the memory tags or the prior artifacts."
            )

        out_dir = STATE / "prompts"
        configured = str(archive.get("dir") or "")
        if configured and not configured.startswith("<state_dir>"):
            out_dir = STATE / configured
        out_dir.mkdir(parents=True, exist_ok=True)
        target = out_dir / f"{args.slug}-{args.phase}-{args.n}.md"
        if target.exists() and not args.force:
            raise Stop(f"{shown(target)} already exists. A dispatched archive is "
                       f"evidence; pass --force only to replace one that was never dispatched.")
        target.write_bytes(body)
    except Stop as exc:
        print(f"STOP: {exc}", file=sys.stderr)
        return 2

    digest = hashlib.sha256(body).hexdigest()
    rel = shown(target)
    tags = [t.strip() for t in (args.memory_tags or "").split(",") if t.strip()]

    # The manifest goes to a file, not to the screen: no value in it passes through a model's
    # hands on the way to the metric record the controller writes when the phase returns.
    sidecar_path(target).write_text(json.dumps({
        "task_slug": args.slug,
        "phase": args.phase,
        "prompt_sections": order,
        "memory_tags": tags,
        "memory_lines": memory_lines,
        "prompt_file": rel,
        "prompt_sha256": digest,
        "assembled_at": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    }, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    print(f"wrote    {rel}  ({lines} lines, {len(body)} bytes)")
    print(f"sha256   {digest}")
    print()
    print(f"manifest {shown(sidecar_path(target))}")
    print()
    print("When the phase returns, record it with mcp__nucleos__record (kind `metric`), taking the")
    print("prompt_sections / memory_tags / memory_lines / prompt_sha256 values from the manifest")
    print("above -- do not compose them yourself.")
    print()
    if args.emit == "dispatcher":
        print("dispatch (dispatcher mode -- the archive IS the stdin file):")
        print(f"  cat {rel} | <tool command>")
    else:
        print(f"dispatch (agent mode -- subagent_type={PHASE_COCKPIT[args.phase]}), "
              f"prompt parameter:")
        print()
        print(f"Your entire job for this phase is in `{rel}`.")
        print(f"Read that file in full -- it is {lines} lines -- and follow it. It contains "
              f"your phase skill, the")
        print("schema you will fill, the project configuration, your objective, and the "
              "context you were")
        print("given. Do not look for instructions anywhere else, and do not read another "
              "file in that")
        print("directory: they are other phases' prompts and none of them is addressed to you.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
