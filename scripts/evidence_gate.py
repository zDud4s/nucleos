#!/usr/bin/env python3
"""Refuse an executor handoff whose validation evidence is incomplete.

This gate can refuse but it can never approve.  In ``--hook`` mode it reads the
live packet path from ``.ai/local/active-packet``; when that file is absent it
prints nothing and exits successfully.  Silence for an absent marker is
intentional: a Stop hook has no packet to assess until the controller creates
that marker immediately before dispatching execute.

Malformed packets fail closed (exit 2), while a well-formed packet with missing
evidence exits 1 and names each unproven validation command.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
ACTIVE_PACKET = ROOT / ".ai" / "local" / "active-packet"
HEADING = re.compile(r"^##\s+(.+?)\s*$")
EXIT = re.compile(r"^exit:\s*\d+\s*$")


class MalformedPacket(ValueError):
    """The packet cannot be safely interpreted."""


def section(lines: list[str], name: str) -> list[str]:
    """Return a level-two heading's body, or fail closed when it is absent."""
    start = None
    for index, line in enumerate(lines):
        match = HEADING.match(line)
        if match and match.group(1) == name:
            start = index + 1
            break
    if start is None:
        raise MalformedPacket(f"missing ## {name} section")
    end = len(lines)
    for index in range(start, len(lines)):
        if HEADING.match(lines[index]):
            end = index
            break
    return lines[start:end]


def validation_commands(lines: list[str]) -> list[str]:
    body = section(lines, "Validation")
    try:
        start = next(index for index, line in enumerate(body) if line.startswith("Commands:"))
    except StopIteration as exc:
        raise MalformedPacket("missing Validation Commands:") from exc

    commands: list[str] = []
    for line in body[start + 1 :]:
        if line.startswith("Expected result:"):
            break
        match = re.match(r"^\s*-\s+`(.+?)`\s*$", line)
        if not match:
            continue
        command = match.group(1).strip()
        if not command:
            raise MalformedPacket("empty validation command")
        commands.append(command)
    if not commands:
        raise MalformedPacket("Validation Commands: has no commands")
    if len(commands) != len(set(commands)):
        raise MalformedPacket("Validation Commands: contains a duplicate command")
    return commands


def evidence_blocks(lines: list[str]) -> dict[str, tuple[bool, bool]]:
    body = section(lines, "Handoff")
    try:
        start = next(index for index, line in enumerate(body) if line.startswith("Validation evidence:"))
    except StopIteration as exc:
        raise MalformedPacket("missing Handoff Validation evidence:") from exc

    blocks: dict[str, tuple[bool, bool]] = {}
    command: str | None = None
    has_exit = False
    has_tail = False
    awaiting_multiline_tail = False

    def finish() -> None:
        if command is not None:
            blocks[command] = (has_exit, has_tail)

    def is_tail_content(line: str) -> bool:
        stripped = line.strip()
        return bool(stripped) and not stripped.startswith("```") and not line.startswith("$ ") and not EXIT.match(line)

    for line in body[start + 1 :]:
        if line.startswith(("Deviations from plan:", "New risks discovered:", "Memory updates:", "Pending deletions:", "Files changed:", "Tests added:")):
            break
        if line.startswith("$ "):
            finish()
            command = line[2:].strip()
            has_exit = False
            has_tail = False
            awaiting_multiline_tail = False
        elif command is not None and EXIT.match(line):
            has_exit = True
        elif command is not None and line.startswith("tail:"):
            awaiting_multiline_tail = not line[5:].strip()
            has_tail = not awaiting_multiline_tail
        elif command is not None and awaiting_multiline_tail and is_tail_content(line):
            has_tail = True
            awaiting_multiline_tail = False
    finish()
    return blocks


def check_packet(text: str) -> list[str]:
    lines = text.splitlines()
    if not lines:
        raise MalformedPacket("empty packet")
    commands = validation_commands(lines)
    blocks = evidence_blocks(lines)
    return [command for command in commands if blocks.get(command) != (True, True)]


def check_path(path: Path) -> int:
    try:
        missing = check_packet(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, MalformedPacket) as exc:
        print(f"evidence gate: malformed input: {exc}", file=sys.stderr)
        return 2
    if missing:
        print("evidence gate: unproven validation commands:", file=sys.stderr)
        for command in missing:
            print(f"- {command}", file=sys.stderr)
        return 1
    return 0


def main(argv: list[str]) -> int:
    if argv == ["--hook"]:
        if not ACTIVE_PACKET.exists():
            return 0
        try:
            packet_text = ACTIVE_PACKET.read_text(encoding="utf-8").strip()
        except (OSError, UnicodeError) as exc:
            print(f"evidence gate: malformed active-packet: {exc}", file=sys.stderr)
            return 2
        if not packet_text:
            print("evidence gate: malformed active-packet: empty path", file=sys.stderr)
            return 2
        return check_path(Path(packet_text))
    if len(argv) > 1 or (argv and argv[0] == "--hook"):
        print("usage: evidence_gate.py [packet-path] | --hook", file=sys.stderr)
        return 2
    if argv:
        return check_path(Path(argv[0]))
    try:
        missing = check_packet(sys.stdin.read())
    except MalformedPacket as exc:
        print(f"evidence gate: malformed input: {exc}", file=sys.stderr)
        return 2
    if missing:
        print("evidence gate: unproven validation commands:", file=sys.stderr)
        for command in missing:
            print(f"- {command}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
