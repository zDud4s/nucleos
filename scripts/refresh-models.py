#!/usr/bin/env python3
"""Write the models each agent CLI currently offers into the daemon's models document.

The document is read and written through `GET`/`PUT /settings/machine/models`. The daemon's model
picker is built from its `assistant_choices`, and that list has to be written by somebody: neither
CLI can enumerate its own models on demand, so a list left to be maintained by
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

  openrouter
          `GET /api/v1/models`, public and keyless. This half is the OTHER SHAPE and the difference
          is the point: it does not choose, it CHECKS. The endpoint serves 423 models and no filter
          makes a menu of them — "supports tools, 100k+ of context, not free" still leaves 322 — so
          WHICH hosted models the picker offers stays a person's choice, written here by hand. What
          a person cannot do is re-read 423 entries to notice that one of theirs stopped existing.
          So the document is its own allowlist: every `brain: openrouter` row already in it is
          looked up, its label refreshed from the catalogue's own name, and an id that is GONE is
          reported along with the surviving ids nearest to it. Adding a hosted model is writing its
          id and running this — the rest fills itself in.

Sources considered and rejected for the Claude half, so nobody re-treads them: `~/.claude` holds no
catalogue; `stats-cache.json` is models USED and goes stale; the CLI binary contains every model it
has ever known — `claude-instant-1` included — so names read out of it are history, not
availability. `GET /v1/models` would work but needs `ANTHROPIC_API_KEY`, and Claude Code signs in
with a subscription rather than a key, so requiring one would be a key nobody here has.

What the docs cannot say is what YOUR plan may reach. They list what exists; entitlement is a
separate question no public source answers, so a model on the menu can still be refused at spawn.

Usage:
    python scripts/refresh-models.py [--dry-run] [--only claude|codex|openrouter] [--limit N]
                                     [--legacy]

Prices are NOT this script's: the daemon refreshes `prices_as_of` and `model_prices` in the same
document itself (`core/src/price_refresh`) — on startup when they are older than a week, daily, when
an unknown model appears, and from "Refresh prices" on the Devtime page. This script sends those two
keys back exactly as it read them; `--only prices` is kept only as a no-op that says so.

The daemon is reached on `127.0.0.1:<NUCLEOS_PORT or 8791>` with the token `nucleos-core
--print-token` prints, or `NUCLEOS_TOKEN` when it is set. A write that lost a race to another writer
(409) reads the document again and retries once; a second one is a failed run.

Exit status is 1 when the daemon cannot be reached or refuses, when any half could not be refreshed,
OR when a hosted id no longer exists, so a scheduled run reports the stale menu rather than only
printing about it.
"""

from __future__ import annotations

import argparse
import functools
import http.client
import importlib.util
import json
import os
import pathlib
import difflib
import re
import shutil
import subprocess
import sys
import urllib.error
import urllib.request

MODELS_PATH = "/settings/machine/models"
DEFAULT_PORT = "8791"  # daemon_client::DEFAULT_PORT
ROOT = pathlib.Path(__file__).resolve().parent.parent
CODEX_CACHE = pathlib.Path.home() / ".codex" / "models_cache.json"

DOCS = "https://platform.claude.com/docs/en"
MODELS_DOC = f"{DOCS}/about-claude/models/overview.md"
EFFORT_DOC = f"{DOCS}/build-with-claude/effort.md"

# The same endpoint `capabilities.rs::discover_openrouter` already reads at runtime for a model's
# context window. Public: no key, no account, no rate limit worth a retry.
OPENROUTER_MODELS = "https://openrouter.ai/api/v1/models"

# What `--suggest` narrows to when nobody names a category. Every category OpenRouter serves holds
# 19 or 20 models, and this is the one this app is for; the others (`roleplay`, `legal`, `finance`,
# `health`, and seven more) are a keystroke away for anyone who wants them.
DEFAULT_CATEGORY = "programming"

# Appended to every hosted label. The vendor is already in the model's name; what the row has to
# say, in a menu that also lists models the local CLI spawns, is WHERE this one runs.
HOSTED_SUFFIX = " (OpenRouter)"

# Weakest to strongest, which is the order `efforts` is documented in and the order the picker
# draws. OpenRouter serves seven words, listed strongest-first and per model: the daemon's own
# `config::EFFORT_LEVELS` plus `minimal` and `none`. Those two are fine to write when the time
# comes — `is_effort_level` checks the UNION of what the document declares, not that constant.
EFFORT_LADDER = ["none", "minimal", "low", "medium", "high", "xhigh", "max"]

# Where the hosted wire stands, and the ONE line to change when it moves.
#
# `openrouter.rs::exchange` builds its request body from `model`, `messages` and — when there are
# any — `tools`. There is no `reasoning` key and no `effort`. Meanwhile `efforts` on a choice is
# exactly what the picker draws its dial from (`Chats.tsx`), so filling these rows in from the
# catalogue would put a slider on screen that turns and reaches nothing. That is a worse answer
# than no slider, so the levels are read and REPORTED on every run and written by none of them.
# The day `exchange` learns to send one, this flips and the answer is already in hand.
HOSTED_TAKES_EFFORT = False

# Invitation-only (Project Glasswing). It appears in the effort page's supported list, so it would
# otherwise be picked up as a model with a dial — and put on a menu nobody here can select from.
INVITE_ONLY = ("mythos",)

# What the daemon ships when the document names nothing (`config.rs::default_assistant_choices`).
#
# Used only to keep the Claude half from VANISHING when the docs cannot be reached and the document
# names none — writing a Claude-less list to a daemon running Claude would shrink its menu to one
# entry. Copied and not invented: these are aliases, so they do not go stale the way a pinned
# version does, and the script says out loud when it falls back to them.
SHIPPED_CLAUDE = ["opus", "sonnet", "fable"]

# The one field this script owns. Everything else in the document is somebody's, and goes back as
# it was read — including `prices_as_of` and `model_prices`, which the daemon refreshes itself
# (`core/src/price_refresh`).
KEY = "assistant_choices"


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)


# --------------------------------------------------------------------------- daemon --


def daemon_url() -> str:
    return f"http://127.0.0.1:{os.environ.get('NUCLEOS_PORT') or DEFAULT_PORT}"


def printed_token(binary: str) -> str | None:
    try:
        printed = subprocess.run(
            [binary, "--print-token"], capture_output=True, text=True, timeout=20
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return (printed.stdout.strip() or None) if printed.returncode == 0 else None


@functools.lru_cache(maxsize=1)
def daemon_token() -> str | None:
    """`NUCLEOS_TOKEN`, or what `nucleos-core --print-token` prints. `None`, having said so.

    A binary on PATH comes first; a checkout has none there, and finding one in cargo's target dirs
    is `ask_daemon.py`'s job already. Cached because a run makes up to four requests and each lookup
    spawns a process.
    """
    override = os.environ.get("NUCLEOS_TOKEN")
    if override:
        return override
    on_path = shutil.which("nucleos-core")
    token = printed_token(on_path) if on_path else None
    if not token:
        hook_path = ROOT / "core" / "hooks" / "ask_daemon.py"
        spec = importlib.util.spec_from_file_location("ask_daemon", hook_path)
        hook = importlib.util.module_from_spec(spec)
        try:
            spec.loader.exec_module(hook)
        except OSError:
            pass
        else:
            token, _ = hook.read_control_token(str(ROOT))
    if not token:
        fail("could not read the daemon token — set NUCLEOS_TOKEN")
    return token


def decoded(raw: bytes) -> object:
    """An answer's JSON, or `None` when it is not JSON: the status still has to reach the caller."""
    try:
        return json.loads(raw)
    except ValueError:
        return None


def daemon_request(method: str, path: str, body: object = None) -> tuple[int, object] | None:
    """One request, as `(status, payload)` for ANY status. `None` when nothing answered.

    `None` is the contract `fetch` has, except that the caller is the one to say so: `main` knows
    which step of the round trip it was on.
    """
    token = daemon_token()
    request = urllib.request.Request(
        f"{daemon_url()}{path}",
        method=method,
        data=None if body is None else json.dumps(body).encode("utf-8"),
        headers={
            "Content-Type": "application/json",
            **({"Authorization": f"Bearer {token}"} if token else {}),
        },
    )
    # No proxy: a loopback request sent to one would report a live daemon as refusing or absent.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        try:
            with opener.open(request, timeout=30) as response:
                return response.status, decoded(response.read())
        except urllib.error.HTTPError as answer:
            with answer:
                return answer.code, decoded(answer.read())
    except (urllib.error.URLError, OSError, http.client.HTTPException):
        return None


def say_no_answer(method: str) -> None:
    fail(
        f"the daemon at {daemon_url()} did not answer {method} {MODELS_PATH} — is it running? "
        "scripts/run-daemon.ps1 starts it from a checkout"
    )


def say_refused(method: str, status: int, payload: object) -> None:
    """A status with no case of its own: named, with whatever reason the daemon gave for it."""
    why = payload if isinstance(payload, dict) else {}
    reason = ": ".join(str(why[key]) for key in ("refusal", "detail") if why.get(key))
    fail(f"the daemon answered {status} to {method} {MODELS_PATH}" + (f" ({reason})" if reason else ""))


def read_document() -> tuple[dict, int] | None:
    """The stored document as `(body, version)`. `None`, having said why, on anything else."""
    answer = daemon_request("GET", MODELS_PATH, None)
    if answer is None:
        say_no_answer("GET")
        return None
    status, payload = answer
    if status != 200:
        say_refused("GET", status, payload)
        return None
    fields = payload if isinstance(payload, dict) else {}
    # A document with no shipped default reads as a null body at version 0.
    body = fields.get("body") or {}
    version = fields.get("version")
    if not isinstance(body, dict) or not isinstance(version, int):
        fail(f"the daemon's answer to GET {MODELS_PATH} has no document in it")
        return None
    return body, version


def kept_rows(body: dict) -> list[dict]:
    """The rows already in the document, so a half that could not refresh survives."""
    choices = body.get(KEY)
    return choices if isinstance(choices, list) else []


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


# ----------------------------------------------------------------------- openrouter --


def openrouter_catalogue() -> dict[str, dict] | None:
    """Every model OpenRouter currently serves, keyed by id. `None` on any failure, having said why.

    `None` and an empty dict are deliberately not the same answer. Every caller of this treats
    `None` as "leave the hosted rows exactly as they are", so a catalogue that came back readable
    but EMPTY must not reach them as a fact — it would mark every id on the menu as dead in one
    pass, which is the loudest possible way to be wrong about a network hiccup.
    """
    text = fetch(OPENROUTER_MODELS)
    if text is None:
        fail("the hosted half is unchanged")
        return None
    try:
        payload = json.loads(text)
    except json.JSONDecodeError as error:
        fail(f"the OpenRouter catalogue did not parse ({error}) — the hosted half is unchanged")
        return None
    entries = payload.get("data") if isinstance(payload, dict) else None
    served = {
        entry["id"]: entry
        for entry in (entries or [])
        if isinstance(entry, dict) and isinstance(entry.get("id"), str) and entry["id"]
    }
    if not served:
        fail("the OpenRouter catalogue named no models — the hosted half is unchanged")
        return None
    return served


def openrouter_efforts(entry: dict) -> list[str]:
    """PURE: the levels THAT model takes, weakest-first. Empty when the catalogue does not say.

    Three shapes mean three different things and only the last one answers this question:
    `reasoning: null` is a model that does not reason, `reasoning: {"mandatory": false}` with no
    levels is one that reasons and publishes no vocabulary (`anthropic/claude-sonnet-4.5` is one
    today), and `supported_efforts` is the vocabulary itself — strongest-first, which is the
    opposite of the row's contract.

    Filtered THROUGH the ladder rather than sorted by it: a word this repo has never seen has no
    place on a slider, and putting it at a guessed magnitude is worse than leaving it off.
    """
    published = ((entry.get("reasoning") or {}).get("supported_efforts")) or []
    return [level for level in EFFORT_LADDER if level in published]


def nearest_surviving(gone: str, served: dict[str, dict]) -> list[str]:
    """PURE: ids that still exist and look like the one that does not, closest first.

    "It is gone" is only half an answer. The usual cause is a version bump under the same author,
    so the replacement is a few characters from the name that died — and naming it turns a
    complaint into something somebody can act on without opening a browser. Same author first,
    because the whole catalogue would happily rank another vendor's near-namesake above the real
    heir; the whole catalogue is the fallback, because an author gets renamed too.
    """
    author = gone.split("/")[0]
    same = [model_id for model_id in served if model_id.split("/")[0] == author]
    close = difflib.get_close_matches(gone, same, n=3, cutoff=0.5)
    return close or difflib.get_close_matches(gone, list(served), n=3, cutoff=0.6)


def openrouter_category(category: str) -> list[dict] | None:
    """OpenRouter's own shortlist for one category, in the order it ranks them. `None` on failure.

    A LIST and not a dict, because the order is the answer: this endpoint is the only thing between
    a person and 423 models, and it is somebody at OpenRouter having decided which twenty are worth
    a look. Nothing here can reproduce that judgement, and every mechanical stand-in tried for it
    failed — `supports tools, 100k+ of context, not free` leaves 322, and `order=top-weekly` is
    accepted and returns the identical order, so there is no popularity signal to sort on.

    Note it cannot be combined with `supported_parameters`: sending both is a 400 from the server,
    which is why `worth_offering` does that half here.
    """
    text = fetch(f"{OPENROUTER_MODELS}?category={category}")
    if text is None:
        fail(f"no suggestions for '{category}'")
        return None
    try:
        payload = json.loads(text)
    except json.JSONDecodeError as error:
        fail(f"the '{category}' shortlist did not parse ({error})")
        return None
    entries = payload.get("data") if isinstance(payload, dict) else None
    if not entries:
        fail(f"'{category}' named no models — is it a category OpenRouter has?")
        return None
    return [entry for entry in entries if isinstance(entry, dict) and entry.get("id")]


def worth_offering(entry: dict) -> bool:
    """PURE: whether a catalogue entry belongs on a menu a person has to read.

    Two rules, and both are about entries that would FAIL rather than merely disappoint:

    `:` marks a variant of a model whose plain id is already on the same shortlist. `:free` is
    rate-limited hard enough to die in the middle of a turn, and `:batch` is an asynchronous
    endpoint that does not answer a chat request at all — offering either is offering the same
    model twice, once in a form that breaks.

    `tools` because the assistant loop IS a tool loop: `openrouter.rs::exchange` sends a `tools`
    array, and a model that ignores it can describe the work but never do any of it. That is not a
    weaker model on the menu, it is a model that cannot do the one thing this app is for.
    """
    if ":" in entry.get("id", ""):
        return False
    return "tools" in (entry.get("supported_parameters") or [])


def suggest(entries: list[dict], already: set[str]) -> list[dict]:
    """PURE: the shortlist worth offering, minus what the document already has, as rows in its shape.

    Rows and not prose, so what is printed is what gets pasted — `render_row` writes each as the
    JSON the document holds, and a suggestion that has to be retyped is a suggestion with a typo in it.
    """
    return [
        {
            "id": entry["id"],
            "label": hosted_label(entry, entry["id"]),
            "brain": "openrouter",
            # Empty for the same reason every hosted row's is: see `HOSTED_TAKES_EFFORT`.
            "efforts": [],
        }
        for entry in entries
        if worth_offering(entry) and entry["id"] not in already
    ]


def hosted_label(entry: dict, fallback: str) -> str:
    """`Anthropic: Claude Sonnet 4.5` -> `Claude Sonnet 4.5 (OpenRouter)`.

    The vendor prefix goes because the model's own name carries it — `Claude Sonnet 4.5` in a menu
    is not ambiguous about who made it — and the suffix comes because that is the fact the row
    exists to state. Bounded at 30 characters so a colon in the middle of a long name is not read
    as a prefix; the longest real one is `Thinking Machines`, and 27 of the 423 names have none.
    """
    name = (entry.get("name") or "").strip()
    if not name:
        return fallback
    return re.sub(r"^[^:]{1,30}:\s*", "", name) + HOSTED_SUFFIX


def refresh_hosted(rows: list[dict], served: dict[str, dict]) -> tuple[list[dict], list[str]]:
    """PURE: each hand-written hosted row against the live catalogue.

    Returns the rows to write and the problems to shout about. A row whose id is GONE is KEPT, and
    that is the load-bearing decision: the models document is in no repository, so a row this
    script quietly deleted would leave no diff and nothing at all to notice — the exact
    silent staleness the script exists to prevent, committed by the script itself. Dropping a
    person's configuration is also not a refresher's call to make. It says so instead, and `main`
    turns any problem into a non-zero exit.
    """
    written: list[dict] = []
    problems: list[str] = []
    for row in rows:
        entry = served.get(row["id"])
        if entry is None:
            heirs = nearest_surviving(row["id"], served)
            suggestion = f" — nearest surviving: {', '.join(heirs)}" if heirs else ""
            problems.append(
                f"{row['id']} is no longer served by OpenRouter{suggestion}. It is still on the "
                "menu, and a turn that picks it will die at the first request."
            )
            written.append(dict(row))
            continue
        fresh = dict(row)
        fresh["label"] = hosted_label(entry, row.get("label", row["id"]))
        if HOSTED_TAKES_EFFORT:
            fresh["efforts"] = openrouter_efforts(entry)
        written.append(fresh)
    return written, problems


# --------------------------------------------------------------------------- writing --


def surviving(kept: list[dict], runner: str) -> list[dict]:
    """The rows a half keeps when it could not be refreshed, or was not asked to be."""
    return [
        choice
        for choice in kept
        # Absent `runner` has always meant `claude`, matching the daemon's own default.
        if (choice.get("runner") or "claude") == runner
        # ...but only among the CLOUD rows, which are the only ones the CLI halves refresh. A
        # `brain: openrouter` row carries no runner (it is reached over HTTP, not spawned as
        # either CLI), so without this it would be bucketed as `claude` and written back beside
        # the Claude rows, in the wrong half of the menu.
        and choice.get("brain", "cloud") == "cloud"
    ]


def hosted_rows(kept: list[dict]) -> list[dict]:
    """The rows the OpenRouter half checks: somebody's choice of which hosted models to offer."""
    return [choice for choice in kept if choice.get("brain") == "openrouter"]


def other_rows(kept: list[dict]) -> list[dict]:
    """Everything else — no half here reads it, so no half here may rewrite it.

    Empty today. It is the door for a `brain` this script has never heard of: carrying an unknown
    row through untouched costs nothing, and dropping one would delete configuration on every run
    for the crime of being newer than this script.
    """
    return [choice for choice in kept if choice.get("brain", "cloud") not in ("cloud", "openrouter")]


def render_row(choice: dict) -> str:
    """One model as one line of JSON: what `--suggest` and `--dry-run` print.

    The document's own shape, so a suggestion can be pasted in as it stands. A suggestion somebody
    has to retype is a suggestion with a typo in it.
    """
    row = {"id": choice["id"], "label": choice["label"], "brain": choice["brain"]}
    # OMITTED rather than written null when there is none: only a row some CLI spawns has a runner
    # to name, and a hosted row is reached over HTTP.
    if choice.get("runner"):
        row["runner"] = choice["runner"]
    row["efforts"] = choice.get("efforts") or []
    return json.dumps(row)


def refreshed_rows(kept: list[dict], args: argparse.Namespace) -> tuple[list[dict], bool]:
    """The new `assistant_choices`, built on the rows just read, and whether any half complained.

    Takes `kept` so that a retry after a 409 is built on the rows of the second read: a half that
    could not refresh, and every hosted row, come from there.
    """
    refreshed: list[dict] = []
    complained = False

    sources = (
        ("claude", lambda: claude_models(include_legacy=args.legacy)),
        ("codex", codex_models),
    )
    for runner, read in sources:
        if args.only is not None and args.only != runner:
            refreshed += surviving(kept, runner)
            continue
        found = read()
        if found is None:
            complained = True
            survivors = surviving(kept, runner)
            # A half that could not be refreshed keeps what the document already had. When it had
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

    # After the cloud models, because the menu is written in the order the window offers it and the
    # CLI's own models are what a person reaches for most.
    hosted = hosted_rows(kept)
    if hosted and (args.only is None or args.only == "openrouter"):
        served = openrouter_catalogue()
        if served is None:
            complained = True
        else:
            hosted, problems = refresh_hosted(hosted, served)
            print(f"openrouter: {len(hosted)} hand-written row(s) against {len(served)} served")
            for choice in hosted:
                entry = served.get(choice["id"])
                # The levels the catalogue publishes, printed on every pass and written on none —
                # see `HOSTED_TAKES_EFFORT`. Printing them is what keeps that gap visible instead
                # of leaving it as a comment nobody re-reads.
                levels = openrouter_efforts(entry) if entry else []
                dial = f"offers efforts={levels}" if levels else ""
                mark = " " if entry else "!"
                print(f"  {mark}{choice['id']:<34} {choice['label']:<30} {dial}".rstrip())
            for problem in problems:
                fail(problem)
            complained = complained or bool(problems)
    refreshed += hosted

    # Untouched, and last: no half here can read them, so no half here may rewrite them.
    unknown = other_rows(kept)
    if unknown:
        print(f"kept {len(unknown)} row(s) of a kind this script does not refresh")
    refreshed += unknown

    return refreshed, complained


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true", help="print the rows, write nothing")
    parser.add_argument(
        "--only",
        choices=["claude", "codex", "openrouter", "prices"],
        help="refresh one half and leave the others exactly as they are. `prices` is accepted for "
        "old invocations and does nothing: the daemon refreshes prices itself",
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
    parser.add_argument(
        "--suggest",
        nargs="?",
        const=DEFAULT_CATEGORY,
        metavar="CATEGORY",
        help="print the hosted models OpenRouter shortlists for a category (default: "
        f"{DEFAULT_CATEGORY}) as JSON lines, and write NOTHING. Which hosted models "
        "the picker offers stays a person's choice; this only narrows what that choice is made "
        "from, since the full catalogue is 400+ models and no filter makes a menu of it",
    )
    args = parser.parse_args()

    if args.only == "prices":
        print("prices are refreshed by the daemon now — use Refresh prices on the Devtime page")
        return 0

    if args.suggest:
        document = read_document()
        if document is None:
            return 1
        kept = kept_rows(document[0])
        entries = openrouter_category(args.suggest)
        if entries is None:
            return 1
        rows = suggest(entries, {choice.get("id") for choice in kept})
        offered = len([entry for entry in entries if worth_offering(entry)])
        print(
            f"openrouter: {len(entries)} shortlisted for '{args.suggest}', {offered} worth "
            f"offering, {len(rows)} not already in the document"
        )
        if not rows:
            print("nothing to add — the document already has every one of them")
            return 0
        print(f"\nAdd any of these to `{KEY}` in the models document:\n")
        for row in rows:
            print(render_row(row))
        return 0

    # One retry, and only for a 409: somebody else wrote between the read and the write, so the
    # whole round is redone on what they wrote. A second 409 is not a third try, because a writer
    # that keeps winning is something a person should hear about.
    for retry_left in (True, False):
        document = read_document()
        if document is None:
            return 1
        body, version = document

        refreshed, complained = refreshed_rows(kept_rows(body), args)
        if not refreshed:
            fail("nothing to write — no half could be read and the document names none")
            return 1

        if args.dry_run:
            print()
            for choice in refreshed:
                print(render_row(choice))
            return 1 if complained else 0

        answer = daemon_request("PUT", MODELS_PATH, {"body": {**body, KEY: refreshed}, "version": version})
        if answer is None:
            say_no_answer("PUT")
            return 1
        status, payload = answer
        if status == 409 and retry_left:
            print(f"the document changed at version {version} while this ran — reading it again")
            continue
        if status != 200:
            say_refused("PUT", status, payload)
            return 1

        saved = payload.get("version") if isinstance(payload, dict) else "unknown"
        print(f"\nsaved {len(refreshed)} choice(s) to the models document at version {saved}")
        return 1 if complained else 0

    return 1


if __name__ == "__main__":
    sys.exit(main())
