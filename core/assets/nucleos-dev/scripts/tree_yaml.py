"""Minimal YAML reader for the orchestration tree engine (no PyYAML dependency).

Supports the subset used by speed.yaml and project.yaml (`tree:`, `nodes:`): nested block
mappings, block lists (also at the key's indent), flow lists/mappings of scalars, quoted and plain
scalars, ints, floats, booleans, null, and comments. Duplicate keys and tab indentation are errors.
"""

from __future__ import annotations

import re

__all__ = ["safe_load", "YAMLError"]


class YAMLError(Exception):
    """Raised for any input this reader refuses."""


_INT = re.compile(r"^[-+]?\d+$")
_FLOAT = re.compile(r"^[-+]?(\d+\.\d*|\.\d+)([eE][-+]?\d+)?$|^[-+]?\d+[eE][-+]?\d+$")
_NULLS = {"null", "Null", "NULL", "~"}
_TRUES = {"true", "True", "TRUE"}
_FALSES = {"false", "False", "FALSE"}


def _strip_comment(line: str) -> str:
    quote = None
    for i, ch in enumerate(line):
        if quote:
            if ch == quote:
                quote = None
        elif ch in "\"'":
            quote = ch
        elif ch == "#" and (i == 0 or line[i - 1] in " \t"):
            return line[:i]
    return line


def _find_colon(text: str) -> int:
    """Index of the key/value colon (followed by space or end), outside quotes; -1 if none."""
    quote = None
    for i, ch in enumerate(text):
        if quote:
            if ch == quote:
                quote = None
        elif ch in "\"'":
            quote = ch
        elif ch == ":" and (i + 1 == len(text) or text[i + 1] in " \t"):
            return i
    return -1


def _scalar(text: str):
    text = text.strip()
    if text == "":
        return None
    if text[0] in "\"'":
        if len(text) < 2 or text[-1] != text[0]:
            raise YAMLError(f"unterminated quoted string: {text}")
        body = text[1:-1]
        if text[0] == "'":
            return body.replace("''", "'")
        return re.sub(r"\\(.)", lambda m: {"n": "\n", "t": "\t"}.get(m.group(1), m.group(1)), body)
    if text[0] == "[":
        if text[-1] != "]":
            raise YAMLError(f"unterminated flow list: {text}")
        return [_scalar(p) for p in _split_flow(text[1:-1])]
    if text[0] == "{":
        if text[-1] != "}":
            raise YAMLError(f"unterminated flow mapping: {text}")
        out = {}
        for part in _split_flow(text[1:-1]):
            at = _find_colon(part)
            if at < 0:
                raise YAMLError(f"bad flow mapping entry: {part}")
            key = _key(part[:at])
            if key in out:
                raise YAMLError(f"duplicate key: {key}")
            out[key] = _scalar(part[at + 1:])
        return out
    if text in _NULLS:
        return None
    if text in _TRUES:
        return True
    if text in _FALSES:
        return False
    if _INT.match(text):
        return int(text)
    if _FLOAT.match(text):
        return float(text)
    return text


def _key(text: str) -> str:
    value = _scalar(text)
    return value if isinstance(value, str) else text.strip()


def _split_flow(body: str) -> list:
    parts, depth, quote, start = [], 0, None, 0
    for i, ch in enumerate(body):
        if quote:
            if ch == quote:
                quote = None
        elif ch in "\"'":
            quote = ch
        elif ch in "[{":
            depth += 1
        elif ch in "]}":
            depth -= 1
        elif ch == "," and depth == 0:
            parts.append(body[start:i])
            start = i + 1
    parts.append(body[start:])
    if len(parts) == 1 and not parts[0].strip():
        return []
    return [p for p in parts]


def _lines(text: str) -> list:
    out = []
    for raw in text.replace("\r\n", "\n").replace("\r", "\n").split("\n"):
        stripped = _strip_comment(raw).rstrip()
        if not stripped.strip():
            continue
        lead = stripped[:len(stripped) - len(stripped.lstrip(" \t"))]
        indent = len(lead)
        if "\t" in lead:
            raise YAMLError(f"tab indentation is not allowed: {raw!r}")
        out.append((indent, stripped[indent:]))
    return out


def _is_item(content: str) -> bool:
    return content == "-" or content.startswith("- ")


class _Parser:
    def __init__(self, lines: list) -> None:
        self.lines = lines
        self.pos = 0

    def block(self, indent: int):
        content = self.lines[self.pos][1]
        return self.seq(indent) if _is_item(content) else self.mapping(indent)

    def mapping(self, indent: int) -> dict:
        out: dict = {}
        while self.pos < len(self.lines):
            cur, content = self.lines[self.pos]
            if cur < indent:
                break
            if cur > indent:
                raise YAMLError(f"unexpected indentation: {content!r}")
            if _is_item(content):
                break
            at = _find_colon(content)
            if at < 0:
                raise YAMLError(f"expected 'key: value', got {content!r}")
            key = _key(content[:at])
            if key in out:
                raise YAMLError(f"duplicate key: {key}")
            rest = content[at + 1:].strip()
            self.pos += 1
            if rest:
                out[key] = _scalar(rest)
            elif self.pos < len(self.lines) and (
                self.lines[self.pos][0] > indent
                or (self.lines[self.pos][0] == indent and _is_item(self.lines[self.pos][1]))
            ):
                out[key] = self.block(self.lines[self.pos][0])
            else:
                out[key] = None
        return out

    def seq(self, indent: int) -> list:
        out: list = []
        while self.pos < len(self.lines):
            cur, content = self.lines[self.pos]
            if cur != indent or not _is_item(content):
                if cur > indent:
                    raise YAMLError(f"unexpected indentation: {content!r}")
                break
            rest = content[1:].strip()
            if not rest:
                self.pos += 1
                if self.pos < len(self.lines) and self.lines[self.pos][0] > indent:
                    out.append(self.block(self.lines[self.pos][0]))
                else:
                    out.append(None)
            elif rest[0] not in "\"'[{" and _find_colon(rest) >= 0:
                # "- key: value" opens an inline mapping whose keys sit two columns in.
                self.lines[self.pos] = (indent + 2, rest)
                out.append(self.mapping(indent + 2))
            else:
                out.append(_scalar(rest))
                self.pos += 1
        return out


def safe_load(text: str):
    """Parse `text`; None for an empty document."""
    lines = _lines(text)
    if not lines:
        return None
    parser = _Parser(lines)
    first = lines[0]
    if not _is_item(first[1]) and _find_colon(first[1]) < 0:
        if len(lines) == 1:
            return _scalar(first[1])
        raise YAMLError(f"expected 'key: value', got {first[1]!r}")
    value = parser.block(first[0])
    if parser.pos < len(lines):
        raise YAMLError(f"unexpected content: {lines[parser.pos][1]!r}")
    return value
