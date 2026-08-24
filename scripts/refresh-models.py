#!/usr/bin/env python3
"""Write the models each agent CLI currently offers into `.ai/nucleos-models.yaml`.

The daemon's model picker is built from `assistant_choices` in that file, and it has to be written
by somebody: neither CLI can enumerate its own models on demand, so a list left to be maintained by
hand goes stale silently — a menu offering a model that no longer exists produces a turn that dies
at spawn, days later, for a reason nobody connects to the menu.

Each half reads a source its own vendor publishes, and neither needs a key:

  claude  Anthropic's own docs, served as markdown at `platform.claude.com/.../*.md`. The models
          overview gives the current table — id, alias and display name — and the effort page gives
          something no other source has: which models take the `effort` parameter AT ALL, and which
          of them take `xhigh` and `max`. Those differ. `claude-haiku-4-5` takes no effort; Opus 4.6
          takes `max` but not `xhigh`. A picker built on one flat list would offer levels that die.

  codex   `~/.codex/models_cache.json`, which the Codex CLI writes and refreshes itself. Local and
          free, and it carries each model's own reasoning levels the same way.

Sources considered and rejected for the Claude half, so nobody re-treads them: `~/.claude` holds no
catalogue; `stats-cache.json` is models USED and goes stale; the CLI binary contains every model it
has ever known — `claude-instant-1` included — so names read out of it are history, not
availability. `GET /v1/models` would work but needs `ANTHROPIC_API_KEY`, and Claude Code signs in
with a subscription rather than a key, so requiring one would be a key nobody here has.

What the docs cannot say is what YOUR plan may reach. They list what exists; entitlement is a
separate question no public source answers, so a model on the menu can still be refused at spawn.

Usage:
    python scripts/refresh-models.py [--dry-run] [--only claude|codex] [--limit N] [--legacy]

The daemon re-reads the file per request, so nothing needs restarting for a refresh to take.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import sys
import urllib.error
import urllib.request

REPO = pathlib.Path(__file__).resolve().parent.parent
CONFIG = REPO / ".ai" / "nucleos-models.yaml"
CODEX_CACHE = pathlib.Path.home() / ".codex" / "models_cache.json"

DOCS = "https://platform.claude.com/docs/en"
MODELS_DOC = f"{DOCS}/about-claude/models/overview.md"
EFFORT_DOC = f"{DOCS}/build-with-claude/effort.md"

# Invitation-only (Project Glasswing). It appears in the effort page's supported list, so it would
# otherwise be picked up as a model with a dial — and put on a menu nobody here can select from.
INVITE_ONLY = ("mythos",)

# What the daemon ships when the file names nothing (`config.rs::default_assistant_choices`).
#
# Used only to keep the Claude half from VANISHING when the docs cannot be reached and the file
# names none — writing a Claude-less list to a daemon running Claude would shrink its menu to one
# entry. Copied and not invented: these are aliases, so they do not go stale the way a pinned
# version does, and the script says out loud when it falls back to them.
SHIPPED_CLAUDE = ["opus", "sonnet", "fable"]

# The block this script owns. Everything else in the file is somebody's, and is preserved verbatim.
KEY = "assistant_choices"

BANNER = f"""# ─────────────────────────────────────────────────────────────
# Escrito por `scripts/refresh-models.py`. Editável à mão — o script substitui apenas este bloco e
# deixa o resto do ficheiro intacto — mas uma edição manual é desfeita na próxima passagem dele.
#
# `id` é o que vai para `--model`, textualmente. `efforts` são os níveis QUE AQUELE MODELO aceita,
# do mais fraco para o mais forte; vazio quer dizer que não tem mostrador. `runner` diz qual CLI o
# corre, e o daemon mostra só os do CLI com que foi arrancado — por isso as duas listas podem viver
# aqui lado a lado.
# ─────────────────────────────────────────────────────────────"""


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)


# --------------------------------------------------------------------------- claude --


def fetch(url: str) -> str | None:
    """One GET, as text. `None` on any failure, having said which."""
    try:
        with urllib.request.urlopen(
            urllib.request.Request(url, headers={"User-Agent": "nucleos-refresh-models"}),
            timeout=30,
        ) as response:
            return response.read().decode("utf-8")
    except (urllib.error.URLError, OSError, UnicodeDecodeError) as error:
        fail(f"could not read {url} ({error})")
        return None


def slug(display: str) -> str:
    """`Claude Opus 4.7` -> `claude-opus-4-7`, which is how the docs spell the same model as an id.

    Needed because the two pages name models differently: the overview's table gives ids, and the
    effort page names them in prose. Matching them by eye is what a human does once and a script
    has to do every time.
    """
    return re.sub(r"[^a-z0-9]+", "-", display.strip().lower()).strip("-")


def table_rows(markdown: str, start: int, stop: int) -> dict[str, list[str]]:
    """The markdown table between two offsets, as `{first cell: [rest of the row]}`.

    The header row keeps its own key — an empty first cell in the docs' tables is `Feature` — so a
    caller can read the column names out of the same structure as the values under them.
    """
    rows: dict[str, list[str]] = {}
    for line in markdown[start:stop].splitlines():
        line = line.strip()
        if not line.startswith("|"):
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if all(set(cell) <= {"-", ":", " "} for cell in cells):
            continue  # the ---|--- separator
        key = re.sub(r"[*`]", "", cells[0]).strip()
        rows.setdefault(key, [re.sub(r"[*`]", "", cell).strip() for cell in cells[1:]])
    return rows


def claude_effort_support() -> tuple[set[str], set[str], set[str]]:
    """Which models take effort at all, which take `xhigh`, and which take `max`.

    Three sets rather than one list, because the page is explicit that they are not the same:
    "`xhigh` is a newer level; some models that support `max` don't support `xhigh`." Guessing here
    would put a level on the menu that returns an error the first time somebody uses it.
    """
    text = fetch(EFFORT_DOC)
    if text is None:
        return set(), set(), set()

    supported: set[str] = set()
    line = re.search(r"Supported models:(.*)", text)
    if line is not None:
        supported = {slug(name) for name in re.findall(r"`([^`]+)`", line.group(1))}

    def available_on(level: str) -> set[str]:
        # The per-level rows read "... Available on Claude Fable 5, Claude Opus 5, and Claude
        # Sonnet 4.6." — model names with dots in them, so the sentence cannot be split on `.`.
        row = re.search(rf"^\|\s*`{level}`\s*\|(.*)$", text, re.M)
        if row is None:
            return set()
        sentence = re.search(r"Available on (.*?)(?:\.\s|\.$|\|)", row.group(1))
        if sentence is None:
            return set()
        return {
            slug(name)
            for name in re.split(r",\s*(?:and\s+)?|\s+and\s+", sentence.group(1))
            if name.strip().lower().startswith("claude")
        }

    return supported, available_on("xhigh"), available_on("max")


def claude_models(include_legacy: bool = False) -> list[dict] | None:
    """The models Anthropic's own docs currently list, strongest first."""
    text = fetch(MODELS_DOC)
    if text is None:
        fail("the Claude half is unchanged")
        return None

    current_at = text.find("### Latest models comparison")
    legacy_at = text.find('<Accordion title="Legacy models">')
    if current_at < 0:
        fail("the models page no longer has a 'Latest models comparison' table — nothing parsed")
        return None

    sections = [("current", current_at, legacy_at if legacy_at > 0 else len(text))]
    if include_legacy and legacy_at > 0:
        sections.append(("legacy", legacy_at, len(text)))

    supported, takes_xhigh, takes_max = claude_effort_support()
    if not supported:
        fail("the effort page could not be read — Claude models get no effort dial this pass")

    found: list[dict] = []
    for _, start, stop in sections:
        rows = table_rows(text, start, stop)
        names = rows.get("Feature") or []
        # The alias column, not the id column: for models before the 4.6 generation the alias is the
        # convenience pointer that resolves to a dated id, and for 4.6 and later the two are equal.
        ids = rows.get("Claude API alias") or rows.get("Claude API ID") or []
        for name, model_id in zip(names, ids):
            if not model_id or " " in model_id:
                continue
            if any(word in model_id for word in INVITE_ONLY):
                continue
            key = slug(name)
            levels: list[str] = []
            # Prefix-matched on a `-` boundary, not compared outright: the effort page names some
            # models by their DATED id (`claude-opus-4-5-20251101`) while the table gives the alias
            # (`claude-opus-4-5`). Comparing the two as equals silently drops the dial from a model
            # that has one. The boundary is what stops `claude-opus-4` matching all of 4.5 to 4.8.
            def listed(candidate: str) -> bool:
                return any(
                    entry == candidate or entry.startswith(f"{candidate}-") for entry in supported
                )

            if listed(key) or listed(slug(model_id)):
                levels = ["low", "medium", "high"]
                if key in takes_xhigh:
                    levels.append("xhigh")
                if key in takes_max:
                    levels.append("max")
            found.append(
                {
                    "id": model_id,
                    # `Claude Opus 5` reads as `Opus 5` in a menu that is entirely Claude models.
                    "label": re.sub(r"^Claude\s+", "", name),
                    "brain": "cloud",
                    "efforts": levels,
                    "runner": "claude",
                }
            )
    if not found:
        fail("the models page parsed to nothing — the Claude half is unchanged")
        return None
    return found


# ---------------------------------------------------------------------------- codex --


def codex_models() -> list[dict] | None:
    """What the Codex CLI's own cache offers, in the order it ranks them."""
    if not CODEX_CACHE.exists():
        fail(f"{CODEX_CACHE} is not there — run `codex` once to make it; the Codex half is unchanged")
        return None
    try:
        payload = json.loads(CODEX_CACHE.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"could not read the Codex cache ({error}) — the Codex half is unchanged")
        return None

    offered = [
        model
        for model in payload.get("models", [])
        # `hide` is the CLI's own word for a model it does not put on its menu — `codex-auto-review`
        # is one, an internal reviewer nobody picks. `supported_in_api` drops anything the runner
        # could not actually launch.
        if model.get("visibility") == "list" and model.get("supported_in_api")
    ]
    offered.sort(key=lambda model: model.get("priority", 1_000_000))
    return [
        {
            "id": model["slug"],
            "label": model.get("display_name") or model["slug"],
            "brain": "cloud",
            "efforts": [
                level["effort"]
                for level in (model.get("supported_reasoning_levels") or [])
                if level.get("effort")
            ],
            "runner": "codex",
        }
        for model in offered
    ]


# --------------------------------------------------------------------------- writing --


def existing_choices(text: str) -> list[dict]:
    """The `assistant_choices` already in the file, so a half that could not refresh survives."""
    import yaml

    try:
        loaded = yaml.safe_load(text) or {}
    except yaml.YAMLError:
        return []
    choices = loaded.get(KEY)
    return choices if isinstance(choices, list) else []


def render(choices: list[dict]) -> str:
    """One model per line, flow-style, because that is what a person scanning this file wants."""
    lines = [BANNER, f"{KEY}:"]
    for choice in choices:
        efforts = ", ".join(choice["efforts"])
        lines.append(
            f"  - {{ id: {json.dumps(choice['id'])}, label: {json.dumps(choice['label'])}, "
            f"brain: {choice['brain']}, runner: {choice['runner']}, efforts: [{efforts}] }}"
        )
    return "\n".join(lines)


def splice(text: str, block: str) -> str:
    """Replace the `assistant_choices` block, or append it. Everything else is left untouched.

    A surgical text edit and not a load-and-dump: this file is mostly prose, in Portuguese,
    explaining what each key costs — and `yaml.dump` would return it with every comment gone. The
    banner above the block is taken with it, so a rewrite does not stack banners.
    """
    lines = text.splitlines()
    start = next((i for i, line in enumerate(lines) if line.startswith(f"{KEY}:")), None)
    if start is None:
        trimmed = "\n".join(lines).rstrip()
        return f"{trimmed}\n\n{block}\n"

    # Back up over the banner this script wrote last time, and over the blank line above it.
    head = start
    while head > 0 and (lines[head - 1].startswith("#") or lines[head - 1].strip() == ""):
        head -= 1

    # Forward to the next top-level key: a line starting in column zero that is not a comment.
    tail = start + 1
    while tail < len(lines) and not re.match(r"^[A-Za-z_]", lines[tail]):
        tail += 1

    before = "\n".join(lines[:head]).rstrip()
    after = "\n".join(lines[tail:]).lstrip("\n")
    parts = [part for part in (before, block, after) if part]
    return "\n\n".join(parts).rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true", help="print the block, write nothing")
    parser.add_argument(
        "--only",
        choices=["claude", "codex"],
        help="refresh one half and leave the other exactly as it is",
    )
    parser.add_argument(
        "--legacy",
        action="store_true",
        help="also include the models Anthropic files under 'Legacy' — still available, and older",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=0,
        metavar="N",
        help="keep at most N models per CLI, strongest first (0 = all). Useful with --legacy, "
        "which is a long menu; the trim is printed rather than done silently",
    )
    args = parser.parse_args()

    if not CONFIG.exists():
        fail(f"{CONFIG} is not there")
        return 1
    text = CONFIG.read_text(encoding="utf-8")
    kept = existing_choices(text)

    def surviving(runner: str) -> list[dict]:
        return [
            choice
            for choice in kept
            # Absent `runner` has always meant `claude`, matching the daemon's own default.
            if (choice.get("runner") or "claude") == runner
        ]

    refreshed: list[dict] = []
    complained = False

    sources = (
        ("claude", lambda: claude_models(include_legacy=args.legacy)),
        ("codex", codex_models),
    )
    for runner, read in sources:
        if args.only is not None and args.only != runner:
            refreshed += surviving(runner)
            continue
        found = read()
        if found is None:
            complained = True
            survivors = surviving(runner)
            # A half that could not be refreshed keeps what the file already had. When the file had
            # nothing, the daemon's own shipped list stands in — otherwise this script would turn
            # "could not reach the API" into "this daemon offers one model", which is a worse lie
            # than the stale list it was run to prevent.
            if not survivors and runner == "claude":
                print(
                    "claude: falling back to the daemon's shipped aliases "
                    f"({', '.join(SHIPPED_CLAUDE)}) so the menu does not shrink"
                )
                survivors = [
                    {
                        "id": alias,
                        "label": alias.capitalize(),
                        "brain": "cloud",
                        # The Claude CLI's own documented set. Used only on this fallback path,
                        # where the docs could not be reached to say which model takes what.
                        "efforts": ["low", "medium", "high", "xhigh", "max"],
                        "runner": "claude",
                    }
                    for alias in SHIPPED_CLAUDE
                ]
            refreshed += survivors
            continue
        if args.limit > 0 and len(found) > args.limit:
            print(f"{runner}: {len(found)} offered, keeping the first {args.limit}")
            found = found[: args.limit]
        print(f"{runner}: {len(found)} model(s)")
        for choice in found:
            print(f"  {choice['id']:<28} {choice['label']:<22} efforts={choice['efforts']}")
        refreshed += found

    if not refreshed:
        fail("nothing to write — neither half could be read and the file names none")
        return 1

    block = render(refreshed)
    if args.dry_run:
        print()
        print(block)
        return 1 if complained else 0

    CONFIG.write_text(splice(text, block), encoding="utf-8", newline="\n")
    print(f"\nwrote {len(refreshed)} choice(s) to {CONFIG.relative_to(REPO)}")
    print("the daemon re-reads this file per request — nothing needs restarting")
    return 1 if complained else 0


if __name__ == "__main__":
    sys.exit(main())
