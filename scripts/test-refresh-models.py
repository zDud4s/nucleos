#!/usr/bin/env python3
"""Cover the OpenRouter half of `scripts/refresh-models.py` and its daemon round trip.

The other two halves read a catalogue and write whatever it says. This one is the
opposite shape and needs its own cover: the hosted rows are somebody's deliberate
CHOICE of which models the picker offers, and no filter can regenerate them. The
live catalogue serves 423 models; "supports tools, 100k+ of context, not free"
still leaves 322, which is not a menu. So the list stays hand-written and the
catalogue's job is to CHECK it.

What that check has to get right, and what each mistake costs:

  dead id      An id that stopped existing is the whole reason this half was
               written. The models document lives in the daemon's settings store and in
               no repository, so a row this script silently dropped would leave no diff, no history and
               nothing to notice — while a row it keeps and complains about is a
               menu entry somebody can go fix. Kept, loudly, never dropped.
  stale label  Caught on the first run against the real catalogue:
               `deepseek/deepseek-chat` is served as "DeepSeek V3", and the row
               called it "DeepSeek Chat". The id was right and the name under it
               had moved, which is precisely the drift a human never re-reads.
  efforts      OpenRouter publishes a per-model effort vocabulary, and writing it
               in would be wrong TODAY: `openrouter.rs::exchange` builds its body
               from `model`, `messages` and `tools` and nothing else. `efforts` is
               what the picker draws its dial from, so writing them would put a
               slider on screen that reaches no wire. Read, reported, not written.
  ladder       When they ARE written, order is not cosmetic. OpenRouter lists them
               strongest-first (`['high','medium','low','minimal']`) and the row's
               contract is weakest-first, because the picker draws a magnitude.
  no catalogue An unreachable endpoint must leave every hosted row exactly as it
               was. Turning "the network was down" into "you have no hosted
               models" is a worse lie than the stale list this script prevents.

The document is read and written through `GET`/`PUT /settings/machine/models`, and that
round trip has its own mistakes: a `PUT` that loses a field this script does not own, or
that carries a version it did not read, or a stale refusal that turns into a blind retry.

Hermetic: every case feeds its catalogue payload and its daemon answers in by hand. No
network, no daemon, and no file of this machine is read or written.

Run:  python scripts/test-refresh-models.py
"""

import collections
import contextlib
import copy
import importlib.util
import io
import itertools
import json
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
    yield ("a dead id is KEPT rather than dropped — a document in no repository has no diff to notice",
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

    with fake_body('{"data": [{"id": "b/b"}, {"id": "a/a"}]}'):
        yield ("a category comes back as a LIST, keeping OpenRouter's own ranking",
               [row["id"] for row in refresh_models.openrouter_category("programming")],
               ["b/b", "a/a"])
    with no_network():
        yield ("an unreachable category suggests nothing rather than an empty shortlist",
               refresh_models.openrouter_category("programming"), None)

    # --- the daemon's price blocks are not ours -------------------------------
    yield ("the script owns assistant_choices alone: no price constant or helper is left",
           [name for name in ("OWNED_KEYS", "PRICING_DOC", "refresh_prices", "anthropic_prices",
                              "openrouter_prices", "render_prices") if hasattr(refresh_models, name)],
           [])


def refresh_hosted_one(model_id, name):
    """The one-row case, which most of the label assertions are."""
    return refresh_models.refresh_hosted(
        [hosted(model_id, "whatever was typed")],
        served(entry(model_id, name, reasoning=False)),
    )


class _Patch:
    """Swap one module attribute, `fetch` by default, for the duration of a block.

    `fetch` is the module's seam to the network and `daemon_request` its seam to the daemon. A name
    the module does not have raises here, before anything runs, rather than being added to it.

    Takes stderr with it: every case in here drives a failure path deliberately, and the
    complaints those paths print are correct — but printed among the results they read as
    the test having broken. What is asserted is the return value, not the wording.
    """

    def __init__(self, replacement, name="fetch"):
        self.replacement = replacement
        self.name = name

    def __enter__(self):
        self.original = getattr(refresh_models, self.name)
        setattr(refresh_models, self.name, self.replacement)
        self.stderr = sys.stderr
        sys.stderr = open(os.devnull, "w", encoding="utf-8")

    def __exit__(self, *_):
        setattr(refresh_models, self.name, self.original)
        sys.stderr.close()
        sys.stderr = self.stderr


def no_network():
    return _Patch(lambda _url: None)


def fake_bodies(by_url):
    """Answer each URL with its own body; an unknown URL is unreachable."""
    return _Patch(lambda url: by_url.get(url))


def fake_body(text):
    return _Patch(lambda _url: text)


MODELS_PATH = "/settings/machine/models"

CLAUDE_ROW = {"id": "claude-opus-5", "label": "Opus 5", "brain": "cloud",
              "efforts": ["low", "high"], "runner": "claude"}
CODEX_ROW = {"id": "gpt-5.5", "label": "GPT-5.5", "brain": "cloud",
             "efforts": ["low"], "runner": "codex"}
HOSTED_ROW = hosted("a/b", "B (OpenRouter)")
OTHER_HOSTED_ROW = hosted("c/d", "D (OpenRouter)")
LOCAL_ROW = {"id": "llama", "label": "Llama", "brain": "local"}
SERVED = served(entry("a/b", "A: B", reasoning=False), entry("c/d", "C: D", reasoning=False))

# What this script owns, as a refresh writes it from the stored document below.
REFRESHED = [CLAUDE_ROW, CODEX_ROW, HOSTED_ROW, LOCAL_ROW]


def document(**others):
    """A stored models document: rows due a refresh, and fields that belong to somebody else."""
    return {
        "local_assistant_model": "qwen",
        "assistant_choices": [
            {"id": "claude-opus-4", "label": "Opus 4", "brain": "cloud", "efforts": [],
             "runner": "claude"},
            {"id": "gpt-old", "label": "GPT old", "brain": "cloud", "efforts": [],
             "runner": "codex"},
            hosted("a/b", "whatever was typed"),
            LOCAL_ROW,
        ],
        "prices_as_of": "2026-01-01",
        "model_prices": {"old": {"input": 1.0}},
        "embedding_model": "x",
        **others,
    }


def others_of(body):
    """Every field of a document this script does not own."""
    return {key: value for key, value in body.items() if key != "assistant_choices"}


def read_answer(version, body=None):
    """`GET /settings/machine/models`, as `core/src/http/settings.rs` answers it."""
    return (200, {"body": body if body is not None else document(), "version": version,
                  "schema": {"type": "object"}})


def stale_answer(current):
    """The 409 of a `PUT` that carried an old version, as `settings.rs::stale` builds it."""
    return (409, {"refusal": "stale", "current": current})


class FakeDaemon:
    """Stands in for `daemon_request`: `(status, payload)`, or `None` when nothing answers.

    `None` is the contract `fetch` already has for "could not reach it". GET and PUT answer from
    their own queues, in order, and every call is recorded. A call no case scripted gets a 500, so
    a surplus request shows up in `calls` and in the exit status instead of ending the run.
    """

    def __init__(self, gets, puts=()):
        self.gets = list(gets)
        self.puts = list(puts)
        self.calls = []

    def __call__(self, method, path, body=None):
        self.calls.append((method, path, copy.deepcopy(body)))
        queue = self.gets if method == "GET" else self.puts
        return copy.deepcopy(queue.pop(0)) if queue else (500, {"refusal": "internal"})

    def methods(self):
        return [method for method, _, _ in self.calls]

    def sent(self):
        return [body for method, _, body in self.calls if method == "PUT"]


Run = collections.namedtuple("Run", "code out err")


def drive(daemon, *argv, fetch=None):
    """Run the script's `main` with the daemon, both CLI halves and the catalogue all faked.

    The `_Patch` and `CONFIG` raises are what keep this hermetic: a script without `daemon_request`,
    or still naming `CONFIG`, has a `main` that rewrites the real `~/.nucleos/nucleos-models.yaml`,
    and that `main` must not run.
    """
    out, err = io.StringIO(), io.StringIO()
    argv_before = sys.argv
    with _Patch(daemon, "daemon_request"), \
            _Patch(lambda **_: [dict(CLAUDE_ROW)], "claude_models"), \
            _Patch(lambda: [dict(CODEX_ROW)], "codex_models"), \
            _Patch(lambda: SERVED, "openrouter_catalogue"), \
            _Patch(fetch or (lambda _url: None)):
        if hasattr(refresh_models, "CONFIG"):
            raise AssertionError("the script still names the YAML file it used to write")
        sys.argv = ["refresh-models.py", *argv]
        try:
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = refresh_models.main()
        except SystemExit as stop:
            # `sys.exit("message")` is as good an exit as `return 1`.
            code = 1 if isinstance(stop.code, str) else (stop.code or 0)
            if isinstance(stop.code, str):
                err.write(stop.code)
        finally:
            sys.argv = argv_before
    return Run(code, out.getvalue(), err.getvalue())


def get_then_put_keeps_every_other_field():
    label = "get_then_put_keeps_every_other_field"
    daemon = FakeDaemon([read_answer(7)], [(200, {"version": 8})])
    run = drive(daemon)
    yield (f"{label}: the refresh succeeds", run.code, 0)
    yield (f"{label}: one read, then one write, both of the models document",
           [(method, path) for method, path, _ in daemon.calls],
           [("GET", MODELS_PATH), ("PUT", MODELS_PATH)])
    sent = daemon.sent()[0]
    yield (f"{label}: the write is `{{body, version}}` and nothing else",
           sorted(sent), ["body", "version"])
    yield (f"{label}: assistant_choices is the refreshed list",
           sent["body"]["assistant_choices"], REFRESHED)
    yield (f"{label}: every other field goes back exactly as it was read",
           others_of(sent["body"]), others_of(document()))


def put_carries_the_version_it_read():
    label = "put_carries_the_version_it_read"
    daemon = FakeDaemon([read_answer(7)], [(200, {"version": 4242})])
    run = drive(daemon)
    yield (f"{label}: the version of the read", daemon.sent()[0]["version"], 7)
    yield (f"{label}: the closing message names the version the daemon saved",
           "4242" in run.out, True)
    yield (f"{label}: the closing message no longer talks about re-reading a file",
           "re-reads" in run.out, False)

    daemon = FakeDaemon([read_answer(0)], [(200, {"version": 1})])
    drive(daemon)
    yield (f"{label}: version 0 is sent as read, since it means no document yet",
           daemon.sent()[0]["version"], 0)


def a_409_rereads_and_retries_once():
    label = "a_409_rereads_and_retries_once"
    # Somebody else wrote between this script's read and its write: a new field value and a row.
    theirs = document(embedding_model="theirs")
    theirs["assistant_choices"].insert(3, hosted("c/d", "typed by them"))
    daemon = FakeDaemon([read_answer(7), read_answer(9, theirs)],
                        [stale_answer(9), (200, {"version": 10})])
    run = drive(daemon)
    yield (f"{label}: the retry succeeds", run.code, 0)
    yield (f"{label}: read, stale write, read again, write again",
           daemon.methods(), ["GET", "PUT", "GET", "PUT"])
    first, second = (daemon.sent() + [{}, {}])[:2]
    yield (f"{label}: the first write carried the first read's version", first.get("version"), 7)
    yield (f"{label}: the retry carries the version of the second read", second.get("version"), 9)
    yield (f"{label}: the retry keeps the other fields of the SECOND read",
           others_of(second.get("body", {})), others_of(theirs))
    yield (f"{label}: only assistant_choices is recalculated, from the rows of the second read",
           second.get("body", {}).get("assistant_choices"),
           [CLAUDE_ROW, CODEX_ROW, HOSTED_ROW, OTHER_HOSTED_ROW, LOCAL_ROW])

    daemon = FakeDaemon([read_answer(7), read_answer(9), read_answer(11)],
                        [stale_answer(9), stale_answer(11), (200, {"version": 12})])
    run = drive(daemon)
    yield (f"{label}: a second 409 is not retried", daemon.methods(), ["GET", "PUT", "GET", "PUT"])
    yield (f"{label}: a second 409 is a failed run", run.code, 1)


def dry_run_prints_the_rows_and_makes_no_put():
    label = "dry_run_prints_the_rows_and_makes_no_put"
    daemon = FakeDaemon([read_answer(7)], [(200, {"version": 8})])
    run = drive(daemon, "--dry-run")
    yield (f"{label}: nothing is written", daemon.methods(), ["GET"])
    yield (f"{label}: the run succeeds", run.code, 0)
    # `llama` is the row no half lists as it goes, so only the printed rows can name it.
    yield (f"{label}: every row of the list is printed",
           [row["id"] for row in REFRESHED if row["id"] not in run.out], [])


def suggest_prints_json_rows():
    label = "suggest_prints_json_rows"
    shortlist = json.dumps({"data": [
        {"id": "a/b", "name": "A: B", "supported_parameters": ["tools"]},
        {"id": "moonshotai/kimi-k3", "name": "MoonshotAI: Kimi K3",
         "supported_parameters": ["tools"]},
        {"id": "x/y:free", "name": "Y (free)", "supported_parameters": ["tools"]},
        {"id": "z/z", "name": "Z", "supported_parameters": ["tools"]},
    ]})
    daemon = FakeDaemon([read_answer(7)], [(200, {"version": 8})])
    run = drive(daemon, "--suggest", fetch=lambda _url: shortlist)
    yield (f"{label}: the run succeeds", run.code, 0)
    yield (f"{label}: nothing is written", daemon.methods(), ["GET"])
    yield (f"{label}: one JSON line per choice, minus what the document already has",
           [json.loads(line) for line in run.out.splitlines() if line.lstrip().startswith("{")],
           [hosted("moonshotai/kimi-k3", "Kimi K3 (OpenRouter)"), hosted("z/z", "Z (OpenRouter)")])

    # `runner` is optional, and absent rather than null: only a row some CLI spawns has one to name.
    yield (f"{label}: a row with a runner carries it",
           json.loads(refresh_models.render_row(CODEX_ROW)), CODEX_ROW)
    yield (f"{label}: a row with no runner has no runner key",
           json.loads(refresh_models.render_row(HOSTED_ROW)), HOSTED_ROW)
    yield (f"{label}: a row whose runner is None has no runner key either",
           json.loads(refresh_models.render_row({**HOSTED_ROW, "runner": None})), HOSTED_ROW)


def daemon_down_exits_with_a_clear_message():
    label = "daemon_down_exits_with_a_clear_message"
    daemon = FakeDaemon([None])
    run = drive(daemon)
    yield (f"{label}: a failed run", run.code, 1)
    yield (f"{label}: stops at the read", daemon.methods(), ["GET"])
    # The fake says nothing of its own, so this is what `main` tells a person.
    yield (f"{label}: the message says it is the daemon", "daemon" in run.err.lower(), True)


def daemon_cases():
    """The round-trip cases, each isolated: one that raises is one failure, not the end of the run."""
    for case in (
        get_then_put_keeps_every_other_field,
        put_carries_the_version_it_read,
        a_409_rereads_and_retries_once,
        dry_run_prints_the_rows_and_makes_no_put,
        suggest_prints_json_rows,
        daemon_down_exits_with_a_clear_message,
    ):
        try:
            yield from case()
        except Exception as error:
            yield (case.__name__, f"raised {error!r}", "no exception")


def main() -> int:
    failures = 0
    total = 0
    for label, got, want in itertools.chain(cases(), daemon_cases()):
        total += 1
        if got != want:
            failures += 1
            print(f"FAIL {label}: expected {want!r}, got {got!r}")
    print(f"{total - failures}/{total} as expected")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
