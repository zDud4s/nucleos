#!/usr/bin/env python3
"""Cover the OpenRouter half of `scripts/refresh-models.py`.

The other two halves read a catalogue and write whatever it says. This one is the
opposite shape and needs its own cover: the hosted rows are somebody's deliberate
CHOICE of which models the picker offers, and no filter can regenerate them. The
live catalogue serves 423 models; "supports tools, 100k+ of context, not free"
still leaves 322, which is not a menu. So the list stays hand-written and the
catalogue's job is to CHECK it.

What that check has to get right, and what each mistake costs:

  dead id      An id that stopped existing is the whole reason this half was
               written. `~/.nucleos/nucleos-models.yaml` is in no repository, so a row this
               script silently dropped would leave no diff, no history and
               nothing to notice — while a row it keeps and complains about is a
               menu entry somebody can go fix. Kept, loudly, never dropped.
  stale label  Caught on the first run against the real catalogue:
               `deepseek/deepseek-chat` is served as "DeepSeek V3", and the file
               called it "DeepSeek Chat". The id was right and the name under it
               had moved, which is precisely the drift a human never re-reads.
  efforts      OpenRouter publishes a per-model effort vocabulary, and writing it
               in would be wrong TODAY: `openrouter.rs::exchange` builds its body
               from `model`, `messages` and `tools` and nothing else. `efforts` is
               what the picker draws its dial from, so writing them would put a
               slider on screen that reaches no wire. Read, reported, not written.
  ladder       When they ARE written, order is not cosmetic. OpenRouter lists them
               strongest-first (`['high','medium','low','minimal']`) and the file's
               contract is weakest-first, because the picker draws a magnitude.
  no catalogue An unreachable endpoint must leave every hosted row exactly as it
               was. Turning "the network was down" into "you have no hosted
               models" is a worse lie than the stale list this script prevents.

Hermetic: every case feeds a catalogue payload in by hand. No network.

Run:  python scripts/test-refresh-models.py
"""

import importlib.util
import os
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
TARGET = os.path.join(ROOT, "scripts", "refresh-models.py")

spec = importlib.util.spec_from_file_location("refresh_models", TARGET)
refresh_models = importlib.util.module_from_spec(spec)
spec.loader.exec_module(refresh_models)


def served(*entries):
    """A catalogue keyed by id, the shape `openrouter_catalogue` returns."""
    return {entry["id"]: entry for entry in entries}


def entry(model_id, name, efforts=None, reasoning=True):
    """One catalogue row. `reasoning=False` is a model that does not reason at all.

    The three shapes are real and mean different things: `reasoning: null` (does not
    reason), `reasoning: {"mandatory": false}` with no levels (reasons, publishes no
    vocabulary — `anthropic/claude-sonnet-4.5` is one), and a full `supported_efforts`.
    """
    row = {"id": model_id, "name": name, "reasoning": None}
    if reasoning:
        row["reasoning"] = {"mandatory": False}
        if efforts is not None:
            row["reasoning"]["supported_efforts"] = efforts
    return row


def hosted(model_id, label, efforts=None):
    return {
        "id": model_id,
        "label": label,
        "brain": "openrouter",
        "efforts": efforts if efforts is not None else [],
    }


def cases():
    # --- the label comes from the catalogue, not from whoever typed it ----------
    # The real drift, reproduced: the id is right and the name under it moved.
    rows, problems = refresh_models.refresh_hosted(
        [hosted("deepseek/deepseek-chat", "DeepSeek Chat (OpenRouter)")],
        served(entry("deepseek/deepseek-chat", "DeepSeek: DeepSeek V3", reasoning=False)),
    )
    yield ("a stale label is replaced by the name the catalogue serves",
           rows[0]["label"], "DeepSeek V3 (OpenRouter)")
    yield ("a refreshed label is not a problem to report", problems, [])

    # The vendor prefix goes: the menu already says which vendor by saying the model,
    # and `(OpenRouter)` is what the row is actually telling you — where it runs.
    rows, _ = refresh_hosted_one("openai/gpt-4o", "OpenAI: GPT-4o")
    yield ("the vendor prefix is dropped from the label", rows[0]["label"], "GPT-4o (OpenRouter)")

    rows, _ = refresh_hosted_one("z/ling-3", "Ling 3.0 Flash")
    yield ("a name with no vendor prefix survives whole",
           rows[0]["label"], "Ling 3.0 Flash (OpenRouter)")

    # --- an id that stopped existing -------------------------------------------
    rows, problems = refresh_models.refresh_hosted(
        [hosted("anthropic/claude-sonnet-4.5", "Claude Sonnet 4.5 (OpenRouter)")],
        served(entry("anthropic/claude-sonnet-4.6", "Anthropic: Claude Sonnet 4.6")),
    )
    yield ("a dead id is KEPT rather than dropped — a gitignored file has no diff to notice",
           [row["id"] for row in rows], ["anthropic/claude-sonnet-4.5"])
    yield ("a dead id keeps the label it had, since nothing better is known",
           rows[0]["label"], "Claude Sonnet 4.5 (OpenRouter)")
    yield ("a dead id is one problem", len(problems), 1)
    yield ("the problem names the id that died",
           "anthropic/claude-sonnet-4.5" in problems[0], True)
    yield ("...and the surviving id that looks like it, so the answer is actionable",
           "anthropic/claude-sonnet-4.6" in problems[0], True)

    # Same author first: a version bump is three characters away, and the whole
    # catalogue would rank some other vendor's near-namesake above the real heir.
    yield ("the nearest surviving id prefers the same author",
           refresh_models.nearest_surviving(
               "anthropic/claude-sonnet-4.5",
               served(entry("openai/claude-sonnet-4.5-clone", "x"),
                      entry("anthropic/claude-sonnet-4.6", "y"))),
           ["anthropic/claude-sonnet-4.6"])
    yield ("an author that vanished entirely still gets an answer from the whole catalogue",
           refresh_models.nearest_surviving(
               "gone/claude-sonnet-4-5",
               served(entry("gone-inc/claude-sonnet-4-5", "y"))),
           ["gone-inc/claude-sonnet-4-5"])
    yield ("nothing close is an empty list and not a wrong guess",
           refresh_models.nearest_surviving("a/b", served(entry("q/zzzzzzzz", "y"))), [])

    # --- efforts: read, reported, not written -----------------------------------
    rows, _ = refresh_models.refresh_hosted(
        [hosted("x/thinker", "old label", efforts=["low", "medium"])],
        served(entry("x/thinker", "X: Thinker", efforts=["high", "medium", "low", "minimal"])),
    )
    yield ("efforts are left EXACTLY as the file had them, because the hosted wire "
           "sends no effort (openrouter.rs::exchange)",
           rows[0]["efforts"], ["low", "medium"])
    yield ("the switch for that is one constant, not a scattered decision",
           refresh_models.HOSTED_TAKES_EFFORT, False)

    # --- the ladder, for the day the wire carries one ----------------------------
    yield ("the catalogue's strongest-first list comes back weakest-first",
           refresh_models.openrouter_efforts(
               entry("a/b", "x", efforts=["max", "xhigh", "high", "medium", "low"])),
           ["low", "medium", "high", "xhigh", "max"])
    yield ("`minimal` and `none` are real levels OpenRouter serves, and sit below `low`",
           refresh_models.openrouter_efforts(
               entry("a/b", "x", efforts=["high", "medium", "low", "minimal", "none"])),
           ["none", "minimal", "low", "medium", "high"])
    yield ("a word off the ladder is dropped rather than placed at a guessed magnitude",
           refresh_models.openrouter_efforts(entry("a/b", "x", efforts=["low", "ludicrous"])),
           ["low"])
    yield ("a model that reasons without publishing its levels yields none",
           refresh_models.openrouter_efforts(entry("a/b", "x")), [])
    yield ("a model that does not reason at all yields none",
           refresh_models.openrouter_efforts(entry("a/b", "x", reasoning=False)), [])

    # --- an unreachable catalogue -------------------------------------------------
    with no_network():
        yield ("an unreachable catalogue is None, so the caller can leave the rows alone",
               refresh_models.openrouter_catalogue(), None)
    with fake_body("not json at all"):
        yield ("a catalogue that does not parse is None and not an empty menu",
               refresh_models.openrouter_catalogue(), None)
    with fake_body('{"data": []}'):
        yield ("a catalogue that parses to no models is a failure, not a menu wipe",
               refresh_models.openrouter_catalogue(), None)
    with fake_body('{"data": [{"id": "a/b", "name": "A: B"}]}'):
        yield ("a catalogue that parses comes back keyed by id",
               sorted(refresh_models.openrouter_catalogue()), ["a/b"])

    # --- the row partition main() depends on ---------------------------------------
    kept = [
        {"id": "claude-opus-5", "label": "Opus 5", "brain": "cloud", "runner": "claude"},
        {"id": "gpt-5.5", "label": "GPT", "brain": "cloud", "runner": "codex"},
        {"id": "no-runner", "label": "Old", "brain": "cloud"},
        hosted("a/b", "A"),
        {"id": "llama", "label": "Llama", "brain": "local"},
    ]
    yield ("an absent runner has always meant claude",
           [row["id"] for row in refresh_models.surviving(kept, "claude")],
           ["claude-opus-5", "no-runner"])
    yield ("a hosted row is never bucketed as a CLI's, or it would be re-emitted with a runner",
           [row["id"] for row in refresh_models.surviving(kept, "claude")
            if row["brain"] != "cloud"], [])
    yield ("the hosted rows are the ones this half checks",
           [row["id"] for row in refresh_models.hosted_rows(kept)], ["a/b"])
    yield ("a row that is neither cloud nor hosted is nobody's to rewrite",
           [row["id"] for row in refresh_models.other_rows(kept)], ["llama"])

    # --- suggesting, which is the opposite of choosing ---------------------------------
    # `?category=programming` is OpenRouter's own shortlist — 19 of 423, and the only reason
    # this file has anything to suggest at all. What is left to do to it is drop the entries
    # that would fail rather than answer.
    yield ("a `:free` variant is not offered — it is rate-limited hard enough to die mid-turn",
           refresh_models.worth_offering(
               {"id": "x/y:free", "supported_parameters": ["tools"]}), False)
    yield ("a `:batch` variant is not offered — an async endpoint does not answer a chat turn",
           refresh_models.worth_offering(
               {"id": "x/y:batch", "supported_parameters": ["tools"]}), False)
    yield ("a model without tools is not offered — the assistant loop IS a tool loop",
           refresh_models.worth_offering(
               {"id": "x/y", "supported_parameters": ["reasoning"]}), False)
    yield ("a plain tool-capable model is offered",
           refresh_models.worth_offering(
               {"id": "x/y", "supported_parameters": ["tools", "reasoning"]}), True)

    shortlist = [
        {"id": "anthropic/claude-opus-5", "name": "Claude Opus 5", "supported_parameters": ["tools"]},
        {"id": "x/y:free", "name": "Y (free)", "supported_parameters": ["tools"]},
        {"id": "moonshotai/kimi-k3", "name": "MoonshotAI: Kimi K3", "supported_parameters": ["tools"]},
    ]
    yield ("a model already in the file is not suggested again",
           [row["id"] for row in refresh_models.suggest(shortlist, {"anthropic/claude-opus-5"})],
           ["moonshotai/kimi-k3"])
    yield ("a suggestion carries the label it would be written with",
           refresh_models.suggest(shortlist, set())[0]["label"], "Claude Opus 5 (OpenRouter)")
    yield ("a suggestion is a hosted row and never gains a runner",
           refresh_models.suggest(shortlist, set())[0].get("runner"), None)
    yield ("a suggested row renders as a line the file can take verbatim",
           refresh_models.render_row(refresh_models.suggest(shortlist, {"x"})[0]),
           '  - { id: "anthropic/claude-opus-5", label: "Claude Opus 5 (OpenRouter)", '
           "brain: openrouter, efforts: [] }")

    with fake_body('{"data": [{"id": "b/b"}, {"id": "a/a"}]}'):
        yield ("a category comes back as a LIST, keeping OpenRouter's own ranking",
               [row["id"] for row in refresh_models.openrouter_category("programming")],
               ["b/b", "a/a"])
    with no_network():
        yield ("an unreachable category suggests nothing rather than an empty shortlist",
               refresh_models.openrouter_category("programming"), None)

    # --- the shape the daemon has to read back ---------------------------------------
    # `runner: None` is read back by YAML as the STRING "None", which `config.rs` falls
    # to `claude` — filtering the row off the menu of any machine running Codex, for a
    # value nobody wrote. The reason the key is omitted rather than emitted empty.
    # The ROW, not the block: `render` returns the banner above it, and the banner's whole job is
    # to explain what `runner` means — so searching the block for that word finds the prose.
    row = refresh_models.render([hosted("a/b", "A (OpenRouter)")]).splitlines()[-1]
    yield ("a hosted row is written with no runner key at all", "runner" in row, False)
    yield ("a hosted row keeps its brain", "brain: openrouter" in row, True)
    yield ("a cloud row still carries the runner that says which CLI spawns it",
           "runner: codex" in refresh_models.render(
               [{"id": "g", "label": "G", "brain": "cloud", "runner": "codex", "efforts": []}]
           ).splitlines()[-1], True)


def refresh_hosted_one(model_id, name):
    """The one-row case, which most of the label assertions are."""
    return refresh_models.refresh_hosted(
        [hosted(model_id, "whatever was typed")],
        served(entry(model_id, name, reasoning=False)),
    )


class _Patch:
    """Swap `fetch` for the duration of a block. The module's only seam to the network.

    Takes stderr with it: every case in here drives a failure path deliberately, and the
    complaints those paths print are correct — but printed among the results they read as
    the test having broken. What is asserted is the return value, not the wording.
    """

    def __init__(self, replacement):
        self.replacement = replacement

    def __enter__(self):
        self.original = refresh_models.fetch
        refresh_models.fetch = self.replacement
        self.stderr = sys.stderr
        sys.stderr = open(os.devnull, "w", encoding="utf-8")

    def __exit__(self, *_):
        refresh_models.fetch = self.original
        sys.stderr.close()
        sys.stderr = self.stderr


def no_network():
    return _Patch(lambda _url: None)


def fake_body(text):
    return _Patch(lambda _url: text)


def main() -> int:
    failures = 0
    total = 0
    for label, got, want in cases():
        total += 1
        if got != want:
            failures += 1
            print(f"FAIL {label}: expected {want!r}, got {got!r}")
    print(f"{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
