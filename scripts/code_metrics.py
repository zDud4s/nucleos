#!/usr/bin/env python3
"""Structural metrics of the core's Rust sources, to tell whether a refactor paid off.

    python scripts/code_metrics.py snapshot <repo> [-o snapshot.json]
    python scripts/code_metrics.py compare <before.json> <after.json>

A snapshot reads `core/src` of the checkout at <repo> (a worktree works) and measures four things:

- layout: what a reader meets opening `core/src` -- entries at the root, folders, the widest folder;
- size: production code lines per file, counted exactly as `core/tests/file_size.rs` counts them;
- coupling: the import graph between core files -- edges, fan-in/fan-out, files caught in cycles;
- folders: how well the folders match that graph -- per folder, the share of its edges that stay
  inside it, and the modularity Q of the folder partition (0 = folders say nothing about who uses
  whom, higher = folders hold what belongs together).

Imports are resolved the way `core/src/map.rs` resolves them: every `crate::a::b` path,
landing on the longest prefix that is a file, plus `pub use x::y as z;` aliases declared in
`lib.rs` (the aliases a move leaves behind for code that still says `crate::z`).

What a move of files can and cannot change: it changes layout and folders, never coupling or
size, because the same code imports the same code from a different address. A refactor that
claims to reduce coupling has to show it in the coupling numbers.
"""

from __future__ import annotations

import argparse
import json
import re
import statistics
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

LIMIT = 1000
SPLIT_AT = 40  # shell/src/project/atlas-model.ts: a root this big is cut by its imports


def code_lines(text: str) -> int:
    lines = text.replace("\r\n", "\n").split("\n")
    count, in_block = 0, False
    for at, line in enumerate(lines):
        nxt = lines[at + 1] if at + 1 < len(lines) else ""
        if line == "#[cfg(test)]" and opens_module(nxt) and not declares_module(nxt.strip()):
            break
        stripped = line.strip()
        if in_block:
            in_block = "*/" not in stripped
            continue
        gates_a_declaration = stripped.startswith("#[") and declares_module(nxt.strip())
        if not stripped or stripped.startswith("//") or declares_module(stripped) or gates_a_declaration:
            continue
        if stripped.startswith("/*"):
            in_block = "*/" not in stripped
            continue
        count += 1
    return count


def declares_module(text: str) -> bool:
    return opens_module(text) and text.endswith(";") and "{" not in text


def opens_module(line: str) -> bool:
    return re.match(r"\s*(pub(\([^)]*\))?\s+)?mod ", line) is not None


def without_comments(source: str) -> str:
    """Comments blanked, strings kept, so `//` inside a literal does not eat the line."""
    out, i, n = [], 0, len(source)
    while i < n:
        c, nxt = source[i], source[i + 1] if i + 1 < n else ""
        if c == "r" and nxt in '"#':
            j, hashes = i + 1, 0
            while j < n and source[j] == "#":
                hashes, j = hashes + 1, j + 1
            if j < n and source[j] == '"':
                end = source.find('"' + "#" * hashes, j + 1)
                end = n if end < 0 else end + 1 + hashes
                out.append(source[i:end])
                i = end
                continue
        if c == '"':
            j = i + 1
            while j < n and source[j] != '"':
                j += 2 if source[j] == "\\" else 1
            out.append(source[i : j + 1])
            i = j + 1
            continue
        if c == "/" and nxt == "/":
            end = source.find("\n", i)
            i = n if end < 0 else end
            continue
        if c == "/" and nxt == "*":
            depth, j = 1, i + 2
            while j < n and depth:
                if source.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif source.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            out.append(" ")
            i = j
            continue
        out.append(c)
        i += 1
    return "".join(out)


IDENT = re.compile(r"[A-Za-z0-9_]+")


def segments(text: str) -> tuple[list[str], str]:
    found, rest = [], text
    while True:
        m = IDENT.match(rest)
        if not m:
            return found, rest
        found.append(m.group())
        after = rest[m.end() :]
        if not after.startswith("::"):
            return found, after
        rest = after[2:]


def rust_paths(source: str) -> set[tuple[str, ...]]:
    source = without_comments(source)
    found: set[tuple[str, ...]] = set()
    for m in re.finditer(r"crate::", source):
        prefix, after = segments(source[m.end() :])
        if after.startswith("{"):
            close = after.find("}")
            if close < 0:
                continue
            for part in after[1:close].split(","):
                tail, _ = segments(part.strip())
                if tail:
                    found.add(tuple(prefix + tail))
        elif prefix:
            found.add(tuple(prefix))
    return found


def module_of(rel: str) -> tuple[str, ...]:
    """`verify/batch.rs` -> (verify, batch); `x/mod.rs` -> (x,); `lib.rs` -> ()."""
    if rel in ("lib.rs", "main.rs"):
        return ()
    parts = rel[: -len(".rs")].split("/")
    if parts[-1] == "mod":
        parts.pop()
    return tuple(parts)


def aliases(lib: str) -> dict[str, tuple[str, ...]]:
    found = {}
    for m in re.finditer(r"pub use ([A-Za-z0-9_:]+) as ([A-Za-z0-9_]+);", without_comments(lib)):
        path = m.group(1).split("::")
        if path[0] == "crate":
            path = path[1:]
        if path and path[0] not in ("nucleos_base", "std"):
            found[m.group(2)] = tuple(path)
    return found


def block_of(rel: str, folders: set[str]) -> str:
    """The folder a file belongs to; a parent `verify.rs` beside `verify/` belongs to `verify`."""
    if "/" in rel:
        return rel.split("/")[0]
    stem = rel[: -len(".rs")]
    return stem if stem in folders else "(raiz)"


def sccs(nodes: list[str], out: dict[str, set[str]]) -> list[list[str]]:
    index, low, on, stack, result, counter = {}, {}, set(), [], [], [0]
    sys.setrecursionlimit(10000)

    def visit(v: str) -> None:
        index[v] = low[v] = counter[0]
        counter[0] += 1
        stack.append(v)
        on.add(v)
        for w in out.get(v, ()):
            if w not in index:
                visit(w)
                low[v] = min(low[v], low[w])
            elif w in on:
                low[v] = min(low[v], index[w])
        if low[v] == index[v]:
            comp = []
            while True:
                w = stack.pop()
                on.discard(w)
                comp.append(w)
                if w == v:
                    break
            result.append(comp)

    for v in nodes:
        if v not in index:
            visit(v)
    return result


def snapshot(repo: Path) -> dict:
    src = repo / "core" / "src"
    files = sorted(p.relative_to(src).as_posix() for p in src.rglob("*.rs"))
    texts = {rel: (src / rel).read_text(encoding="utf-8") for rel in files}
    folders = {p.relative_to(src).as_posix() for p in src.rglob("*") if p.is_dir()}
    top_folders = {f for f in folders if "/" not in f}

    by_module = {module_of(rel): rel for rel in files if module_of(rel)}
    alias = aliases(texts.get("lib.rs", ""))

    out: dict[str, set[str]] = defaultdict(set)
    for rel, text in texts.items():
        for path in rust_paths(text):
            if path[0] in alias:
                path = alias[path[0]] + path[1:]
            target = next(
                (by_module[path[:n]] for n in range(len(path), 0, -1) if path[:n] in by_module),
                None,
            )
            if target and target != rel:
                out[rel].add(target)
    edges = [(a, b) for a in sorted(out) for b in sorted(out[a])]
    fan_in: dict[str, int] = defaultdict(int)
    for _, b in edges:
        fan_in[b] += 1

    cycles = [c for c in sccs(files, out) if len(c) > 1]
    sizes = {rel: code_lines(text) for rel, text in texts.items()}
    size_values = sorted(sizes.values())

    entries: dict[str, int] = defaultdict(int)
    for p in src.rglob("*"):
        if p.is_dir() or p.suffix == ".rs":
            entries[p.parent.relative_to(src).as_posix() or "."] += 1

    blocks = {rel: block_of(rel, top_folders) for rel in files}
    per: dict[str, dict] = defaultdict(lambda: {"files": 0, "intra": 0, "out": 0, "in": 0})
    for rel in files:
        per[blocks[rel]]["files"] += 1
    degree: dict[str, int] = defaultdict(int)
    intra_total: dict[str, int] = defaultdict(int)
    for a, b in edges:
        ba, bb = blocks[a], blocks[b]
        degree[ba] += 1
        degree[bb] += 1
        if ba == bb:
            per[ba]["intra"] += 1
            intra_total[ba] += 1
        else:
            per[ba]["out"] += 1
            per[bb]["in"] += 1
    m = len(edges) or 1
    q = sum(intra_total[c] / m - (degree[c] / (2 * m)) ** 2 for c in per)
    for name, row in per.items():
        touching = row["intra"] + row["out"] + row["in"]
        row["cohesion"] = round(row["intra"] / touching, 3) if touching else None

    git = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "--short", "HEAD"], capture_output=True, text=True
    ).stdout.strip()
    return {
        "repo": str(repo),
        "commit": git,
        "layout": {
            "files": len(files),
            "root_entries": entries["."],
            "root_files": sum(1 for rel in files if "/" not in rel),
            "folders": len(folders),
            "widest_folder": max(entries.values()),
            "map_cuts_the_root": per["(raiz)"]["files"] >= SPLIT_AT,
        },
        "size": {
            "total": sum(size_values),
            "median": statistics.median(size_values),
            "p90": size_values[int(len(size_values) * 0.9)],
            "max": size_values[-1],
            "over_limit": sum(1 for v in size_values if v > LIMIT),
        },
        "coupling": {
            "edges": len(edges),
            "mean_fan_out": round(len(edges) / len(files), 2),
            "files_in_cycles": sum(len(c) for c in cycles),
            "largest_cycle": max((len(c) for c in cycles), default=0),
            "top_fan_out": sorted(((len(v), k) for k, v in out.items()), reverse=True)[:5],
            "top_fan_in": sorted(((v, k) for k, v in fan_in.items()), reverse=True)[:5],
        },
        "folders": {
            "modularity_q": round(q, 4),
            "blocks": len(per),
            "per_folder": dict(sorted(per.items())),
        },
    }


ROWS = [
    ("layout", "files", "ficheiros"),
    ("layout", "root_entries", "entradas na raiz de src/"),
    ("layout", "root_files", "ficheiros soltos na raiz"),
    ("layout", "folders", "pastas"),
    ("layout", "widest_folder", "pasta mais larga (entradas)"),
    ("layout", "map_cuts_the_root", "mapa tem de inventar grupos"),
    ("size", "total", "linhas de código"),
    ("size", "median", "mediana por ficheiro"),
    ("size", "p90", "p90 por ficheiro"),
    ("size", "max", "maior ficheiro"),
    ("size", "over_limit", f"ficheiros > {LIMIT}"),
    ("coupling", "edges", "imports entre ficheiros"),
    ("coupling", "mean_fan_out", "imports por ficheiro"),
    ("coupling", "files_in_cycles", "ficheiros em ciclos"),
    ("coupling", "largest_cycle", "maior ciclo"),
    ("folders", "blocks", "blocos (pastas + raiz)"),
    ("folders", "modularity_q", "modularidade Q das pastas"),
]


def compare(before: dict, after: dict) -> str:
    lines = [f"{'':34}{before['commit']:>12}{after['commit']:>12}"]
    for section, key, label in ROWS:
        a, b = before[section][key], after[section][key]
        mark = "" if a == b else "  *"
        lines.append(f"{label:34}{str(a):>12}{str(b):>12}{mark}")
    lines.append("")
    lines.append("coesão por pasta (imports que ficam dentro / todos os que lhe tocam):")
    for name, row in after["folders"]["per_folder"].items():
        if name != "(raiz)":
            lines.append(f"  {name:20} {row['files']:>3} ficheiros  coesão {row['cohesion']}")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    snap = sub.add_parser("snapshot")
    snap.add_argument("repo", type=Path)
    snap.add_argument("-o", "--out", type=Path)
    cmp_ = sub.add_parser("compare")
    cmp_.add_argument("before", type=Path)
    cmp_.add_argument("after", type=Path)
    args = parser.parse_args()

    if args.command == "snapshot":
        data = json.dumps(snapshot(args.repo), indent=2, ensure_ascii=False)
        if args.out:
            args.out.parent.mkdir(parents=True, exist_ok=True)
            args.out.write_text(data + "\n", encoding="utf-8")
        else:
            print(data)
    else:
        load = lambda p: json.loads(p.read_text(encoding="utf-8"))  # noqa: E731
        print(compare(load(args.before), load(args.after)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
