#!/usr/bin/env python3
"""Prove a release directory is complete and write the SHA256SUMS the install scripts verify.

    python scripts/release-manifest.py verify --dir D --version V [--platforms P1,P2]
    python scripts/release-manifest.py sums   --dir D --version V [--platforms P1,P2]
    python scripts/release-manifest.py table                      (the asset table, as JSON)

Three runners each upload their own installers to one draft release. Before the draft is
published, `verify` checks that every platform contributed exactly what the updater and the
install scripts look for, and `sums` then writes `SHA256SUMS` (`<sha256>  <name>`, LF, sorted by
name, every file except itself). `install.sh` and `install.ps1` pick their asset out of that file
by the suffixes in ASSETS, so this table is the one place the suffixes are written down.

Exit 0 = ok; 1 = the release is not what it should be (every problem is named on stderr);
2 = usage error.
"""

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

# platform -> [(suffix of the asset's name, carries the version in its name?, needs a .sig?)]
# Names are Tauri's own, as tauri-action uploads them; the macOS tarball has no version in it.
ASSETS = {
    "windows-x64": [("-setup.exe", True, True)],
    "linux-x64": [(".AppImage", True, True), (".deb", True, False)],
    "macos-arm64": [(".app.tar.gz", False, True), (".dmg", True, False)],
}
UPDATER_FEED = "latest.json"
SUMS = "SHA256SUMS"
NAME_VERSION = re.compile(r"_(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)_")


def check_platform(files, platform, version):
    problems = []
    for suffix, versioned, signed in ASSETS[platform]:
        hits = [n for n in files if n.endswith(suffix)]
        if not hits:
            problems.append(f"{platform}: no asset ending in {suffix}")
            continue
        if len(hits) > 1:
            problems.append(f"{platform}: more than one asset ends in {suffix}: {', '.join(sorted(hits))}")
            continue
        name = hits[0]
        if versioned:
            m = NAME_VERSION.search(name)
            if not m or m.group(1) != version:
                problems.append(f"{platform}: {name} does not carry version {version} (found {m.group(1) if m else 'none'})")
        if signed and name + ".sig" not in files:
            problems.append(f"{platform}: {name} has no {name}.sig (the updater signature)")
    return problems


def verify(directory: Path, version: str, platforms):
    if not directory.is_dir():
        return [f"{directory} is not a directory"]
    files = {p.name for p in directory.iterdir() if p.is_file()}
    problems = []
    for platform in platforms:
        problems += check_platform(files, platform, version)
    feed = directory / UPDATER_FEED
    if feed.exists():
        try:
            found = json.loads(feed.read_text(encoding="utf-8")).get("version")
            if found != version:
                problems.append(f"{UPDATER_FEED} says version {found}, expected {version}")
        except (ValueError, AttributeError) as exc:
            problems.append(f"{UPDATER_FEED} is not valid JSON: {exc}")
    elif set(platforms) == set(ASSETS):
        problems.append(f"{UPDATER_FEED} is missing")
    return problems


def write_sums(directory: Path):
    rows = []
    for p in sorted(directory.iterdir(), key=lambda q: q.name):
        if p.is_file() and p.name != SUMS:
            rows.append(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n")
    with open(directory / SUMS, "w", encoding="utf-8", newline="\n") as fh:
        fh.write("".join(rows))
    return len(rows)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("verify", "sums"):
        p = sub.add_parser(name)
        p.add_argument("--dir", required=True)
        p.add_argument("--version", required=True)
        p.add_argument("--platforms", help="comma list; default is every platform")
    sub.add_parser("table")
    args = ap.parse_args(argv)

    if args.cmd == "table":
        print(json.dumps({"assets": ASSETS, "feed": UPDATER_FEED, "sums": SUMS}, indent=2))
        return 0

    platforms = args.platforms.split(",") if args.platforms else list(ASSETS)
    unknown = [p for p in platforms if p not in ASSETS]
    if unknown:
        print(f"release-manifest: unknown platform {', '.join(unknown)} (known: {', '.join(ASSETS)})", file=sys.stderr)
        return 2
    directory = Path(args.dir)
    problems = verify(directory, args.version, platforms)
    if problems:
        print("release-manifest: the release is not complete:", file=sys.stderr)
        for p in problems:
            print(f"  {p}", file=sys.stderr)
        return 1
    if args.cmd == "sums":
        print(f"wrote {SUMS} with {write_sums(directory)} entries")
    else:
        print(f"release {args.version} is complete for {', '.join(platforms)}")
    return 0


if __name__ == "__main__":
    try:
        code = main()
    except SystemExit as exc:
        code = exc.code
    sys.exit(code)
