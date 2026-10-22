#!/usr/bin/env python3
"""Keep the version of NucleOS in one place: the `VERSION` file at the repository root.

    python scripts/bump-version.py X.Y.Z            write X.Y.Z to VERSION and to every manifest
    python scripts/bump-version.py --check          every manifest must equal VERSION
    python scripts/bump-version.py --check --expect X.Y.Z    ... and VERSION itself must be X.Y.Z
    python scripts/bump-version.py --root DIR ...   act on a copy of the tree (the tests do)

The shell installer, the nucleo and their lock files each spell the same version in their own
syntax, and the update handshake compares the shell's version with the daemon's. This is the only
writer, so they cannot drift apart by a hand edit.

Exit 0 = done / all agree; 1 = an anchor is missing or a source disagrees (every offender is
named on stderr, nothing is written); 2 = usage error or a version that is not semver.
"""

import argparse
import re
import sys
from pathlib import Path

SEMVER = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z.-]+)?$")

# (path relative to the root, how the line carrying the version is found)
#   toml_package        first `version = "..."` after the `[package]` header
#   json_top            the line `  "version": "..."` indented by exactly two spaces
#   line_after A N      the line right after each line equal to A (exactly N of them)
TARGETS = [
    ("core/Cargo.toml", ("toml_package",)),
    ("core/crates/nucleos-daemon/Cargo.toml", ("toml_package",)),
    ("shell/src-tauri/Cargo.toml", ("toml_package",)),
    ("shell/package.json", ("json_top",)),
    ("shell/src-tauri/tauri.conf.json", ("json_top",)),
    ("Cargo.lock", ("line_after", 'name = "nucleos-core"', 1)),
    ("Cargo.lock", ("line_after", 'name = "nucleos-daemon"', 1)),
    ("shell/src-tauri/Cargo.lock", ("line_after", 'name = "shell"', 1)),
    ("shell/package-lock.json", ("line_after", '"name": "shell",', 2)),
]

TOML_VERSION = re.compile(r'^(version\s*=\s*")([^"]*)(".*)$')
JSON_VERSION = re.compile(r'^(  "version"\s*:\s*")([^"]*)(".*)$')
LOCK_VERSION = re.compile(r'^(\s*(?:version\s*=\s*|"version"\s*:\s*)")([^"]*)(".*)$')


def split_ending(line: str):
    for ending in ("\r\n", "\n"):
        if line.endswith(ending):
            return line[: -len(ending)], ending
    return line, ""


def find_lines(lines, how):
    """Return the indexes of the lines that carry the version, or raise ValueError."""
    kind = how[0]
    if kind == "toml_package":
        in_package = False
        for i, raw in enumerate(lines):
            text, _ = split_ending(raw)
            stripped = text.strip()
            if stripped.startswith("["):
                in_package = stripped == "[package]"
            elif in_package and TOML_VERSION.match(text):
                return [i]
        raise ValueError("no `version` under [package]")
    if kind == "json_top":
        hits = [i for i, raw in enumerate(lines) if JSON_VERSION.match(split_ending(raw)[0])]
        if not hits:
            raise ValueError('no top-level `  "version": "..."` line')
        return hits[:1]
    anchor, want = how[1], how[2]
    hits = [i for i, raw in enumerate(lines) if split_ending(raw)[0].strip() == anchor]
    if len(hits) != want:
        raise ValueError(f"expected {want} line(s) `{anchor}`, found {len(hits)}")
    out = []
    for i in hits:
        if i + 1 >= len(lines) or not LOCK_VERSION.match(split_ending(lines[i + 1])[0]):
            raise ValueError(f"the line after `{anchor}` (line {i + 1}) is not a version")
        out.append(i + 1)
    return out


def version_pattern(how):
    return {"toml_package": TOML_VERSION, "json_top": JSON_VERSION}.get(how[0], LOCK_VERSION)


def read_lines(path: Path):
    with open(path, encoding="utf-8", newline="") as fh:
        return fh.read().splitlines(keepends=True)


def versions_in(root: Path):
    """Yield (relpath, [versions found] or an error string) for every target."""
    for rel, how in TARGETS:
        try:
            lines = read_lines(root / rel)
            pat = version_pattern(how)
            yield rel, [pat.match(split_ending(lines[i])[0]).group(2) for i in find_lines(lines, how)]
        except (OSError, ValueError) as exc:
            yield rel, str(exc)


def read_version_file(root: Path) -> str:
    return (root / "VERSION").read_text(encoding="utf-8").strip()


def check(root: Path, expect):
    problems = []
    try:
        want = read_version_file(root)
    except OSError as exc:
        print(f"bump-version: cannot read VERSION: {exc}", file=sys.stderr)
        return 1
    if not SEMVER.match(want):
        problems.append(f"VERSION: `{want}` is not X.Y.Z")
    if expect is not None and want != expect:
        problems.append(f"VERSION is {want}, expected {expect}")
    for rel, found in versions_in(root):
        if isinstance(found, str):
            problems.append(f"{rel}: {found}")
        elif any(v != want for v in found):
            problems.append(f"{rel}: has {', '.join(found)}, VERSION is {want}")
    if problems:
        print("bump-version: the version sources disagree:", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        return 1
    print(f"all {len(TARGETS) + 1} version sources agree on {want}")
    return 0


def bump(root: Path, version: str):
    if not SEMVER.match(version):
        print(f"bump-version: `{version}` is not X.Y.Z (optionally -prerelease)", file=sys.stderr)
        return 2
    # Compute every edit first; write only when all of them applied.
    edits, problems = {}, []
    for rel, how in TARGETS:
        try:
            # A file with two targets (Cargo.lock) is edited on top of its earlier edit.
            lines = edits[rel].splitlines(keepends=True) if rel in edits else read_lines(root / rel)
            pat = version_pattern(how)
            for i in find_lines(lines, how):
                text, ending = split_ending(lines[i])
                m = pat.match(text)
                lines[i] = m.group(1) + version + m.group(3) + ending
            edits[rel] = "".join(lines)
        except (OSError, ValueError) as exc:
            problems.append(f"{rel}: {exc}")
    if problems:
        print("bump-version: nothing written:", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        return 1
    for rel, text in edits.items():
        with open(root / rel, "w", encoding="utf-8", newline="") as fh:
            fh.write(text)
    with open(root / "VERSION", "w", encoding="utf-8", newline="") as fh:
        fh.write(version + "\n")
    print(f"version is now {version} in VERSION and {len(edits)} manifests")
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("version", nargs="?", help="the new version, X.Y.Z")
    ap.add_argument("--check", action="store_true", help="verify instead of writing")
    ap.add_argument("--expect", help="with --check: VERSION must equal this")
    ap.add_argument("--root", default=str(Path(__file__).resolve().parent.parent))
    args = ap.parse_args(argv)
    root = Path(args.root)
    if args.check:
        if args.version is not None:
            ap.error("--check takes no version")
        return check(root, args.expect)
    if args.version is None or args.expect:
        ap.error("give a version to write, or --check")
    return bump(root, args.version)


if __name__ == "__main__":
    try:
        code = main()
    except SystemExit as exc:  # argparse exits 2 on a usage error; keep that contract
        code = exc.code
    sys.exit(code)
