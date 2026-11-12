#!/usr/bin/env python3
"""Exit criteria for a working session on a spec or plan: the session ends when its checklist
does, not when the model feels it has done enough.

Without this, a session asked to "work on spec X" stopped after a chunk, and the owner had to
type "segue", "o que falta" or "resume o que foi feito" over and over. Two hooks and one file:

- `<state_dir>/loops/<session_id>.md` -- the checklist, opened with `start` and kept by the model
  (`- [ ]` open, `- [x]` done, `- [!]` waits on a decision only the owner can make) plus a
  `status:` line (`active` | `blocked` | `handoff` | `done`).
- `--stop-hook` (Stop): refuses the stop while any `- [ ]` is left, unless the status is
  `blocked` (with at least one `[!]` item) or `handoff` (context ceiling), or background work the
  session launched is still running: its completion wakes the session and the hold resumes
  (`spec_loop_timer.py` wakes it first if the prompt cache is about to expire). It
  lets go after MAX_IDLE_PUSHES pushes that closed nothing, so a stuck model cannot loop forever;
  the timer then wakes that session once, at the end of its cache window, and the feed gets one line.
  Whenever the checklist changed since the last accepted report, the final message must carry the report
  headings (REPORT_HEADINGS); a done checklist is then closed (`<id>.closed.md`).
- `--prompt-hook` (UserPromptSubmit): a prompt that names a spec or plan, in a session with no
  open loop, gets the instruction to open one; a session with one gets its open count.

Fail-open: no loop file, no `.nucleos/workflow/project.json`, an unreadable payload or any
error says nothing and lets the stop go (the two hooks). `start` is the one entry point that
fails loudly when the project cannot be found.

    python <bundle_dir>/scripts/spec_loop.py start <spec-path> ["item" ...]
    python <bundle_dir>/scripts/test_spec_loop.py -v
"""

from __future__ import annotations

import json
import os
import re
import sys
import unicodedata
import urllib.request
from pathlib import Path

import nucleos_paths

MAX_IDLE_PUSHES = 2
LET_GO_FEED_KIND = "spec_loop_let_go"
FEED_TIMEOUT_SECS = 2
REPORT_HEADINGS = ("Feito", "Falta", "Verificação", "Próximo passo")
REPORT_TEMPLATE = (
    "## Feito\n- <item> — <evidência: ficheiro, commit ou teste>\n"
    "## Falta\n- <item aberto ou bloqueado> (ou: nada)\n"
    "## Verificação\n<comando corrido> → <resultado>\n"
    "## Próximo passo\n<o que acontece a seguir, ou a decisão que precisas do dono>"
)
_ITEM = re.compile(r"^\s*[-*]\s+\[( |x|X|!)\]\s+(.+?)\s*$", re.MULTILINE)
_STATUS = re.compile(r"^status:\s*([A-Za-z]+)", re.MULTILINE)
_SPEC_PROMPT = re.compile(r"(docs/)?(specs|plans)/|\bspecs?\b", re.IGNORECASE)
_TAIL_BYTES = 512 * 1024


def loops_dir(cwd: str | None) -> Path | None:
    """`<state_dir>/loops/` of the project `cwd` belongs to, or None when it has no workflow."""
    try:
        root = nucleos_paths.project_root(cwd)
        return nucleos_paths.state_dir(root) / "loops"
    except nucleos_paths.ProjectNotFound:
        return None


def loop_path(loops: Path, session: str) -> Path:
    return loops / f"{session}.md"


def parse(text: str) -> dict:
    items = _ITEM.findall(text)
    status = (_STATUS.search(text) or [None, "active"])[1].lower()
    return {
        "status": status,
        "open": [t for m, t in items if m == " "],
        "done": [t for m, t in items if m in "xX"],
        "blocked": [t for m, t in items if m == "!"],
    }


def _fold(s: str) -> str:
    return "".join(c for c in unicodedata.normalize("NFKD", s.lower()) if not unicodedata.combining(c))


def missing_headings(message: str) -> list[str]:
    heads = {_fold(h.lstrip("#").strip()) for h in re.findall(r"^#{1,4}\s+.+$", message, re.MULTILINE)}
    return [h for h in REPORT_HEADINGS if not any(x.startswith(_fold(h)) for x in heads)]


def last_assistant_text(payload: dict) -> str:
    text = payload.get("last_assistant_message")
    if isinstance(text, str) and text:
        return text
    path = payload.get("transcript_path")
    if not path:
        return ""
    try:
        with open(path, "rb") as f:
            f.seek(0, 2)
            size = f.tell()
            for start in (max(0, size - _TAIL_BYTES), 0):
                f.seek(start)
                found = _scan(f.read().decode("utf-8", "replace").splitlines())
                if found or start == 0:
                    return found
    except OSError:
        return ""
    return ""


def _scan(lines: list[str]) -> str:
    for line in reversed(lines):
        try:
            rec = json.loads(line)
        except ValueError:
            continue
        if rec.get("type") != "assistant":
            continue
        content = (rec.get("message") or {}).get("content")
        if isinstance(content, str) and content.strip():
            return content
        texts = [b.get("text", "") for b in content or [] if isinstance(b, dict) and b.get("type") == "text"]
        if any(t.strip() for t in texts):
            return "\n".join(texts)
    return ""


def running_tasks(payload: dict) -> list[str]:
    """Ids of the session's background work still running, as Claude Code reports it at Stop. A
    payload without the field reads as none: the push stays, which is the safe side."""
    tasks = payload.get("background_tasks")
    if not isinstance(tasks, list):
        return []
    return sorted(str(t.get("id")) for t in tasks if isinstance(t, dict) and t.get("status") == "running")


def _load_state(p: Path) -> dict:
    try:
        return json.loads(p.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}


def _block(reason: str) -> dict:
    return {"decision": "block", "reason": reason}


def report_let_go(payload: dict, n_open: int) -> None:
    """One feed line saying a session stopped with work open. One light request, silent on failure:
    the daemon may be down, and the stop must go either way."""
    try:
        import rehydrate

        cwd = payload.get("cwd")
        token = rehydrate.find_token(str(cwd or os.getcwd()))
        if not token:
            return
        base = (os.environ.get("NUCLEOS_DAEMON_URL") or rehydrate.DEFAULT_DAEMON_URL).rstrip("/")
        request = urllib.request.Request(
            f"{base}/conductor/feed",
            data=json.dumps({
                "kind": LET_GO_FEED_KIND,
                "session_id": payload.get("session_id"),
                "cwd": cwd,
                "tool_name": "spec loop",
                "reason": f"let go with {n_open} open item(s), pushes closed nothing",
            }).encode(),
            headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
            method="POST",
        )
        urllib.request.urlopen(request, timeout=FEED_TIMEOUT_SECS).close()
    except Exception:
        pass


def stop_decision(payload: dict) -> dict | None:
    """The hook's JSON answer, or None to let the stop go."""
    session = payload.get("session_id")
    loops = loops_dir(payload.get("cwd"))
    if not session or not loops:
        return None
    path = loop_path(loops, session)
    if not path.exists():
        return None
    state_path = path.with_suffix(".state.json")
    state = _load_state(state_path)
    text = path.read_text(encoding="utf-8")
    loop = parse(text)
    status, n_open = loop["status"], len(loop["open"])
    rel = path.as_posix()
    pushed = payload.get("stop_hook_active") is True

    def save(**kw):
        state.update(kw)
        state_path.write_text(json.dumps(state), encoding="utf-8")

    # 1. Work left and no legitimate exit: push back, unless the last pushes closed nothing.
    exit_ok = status == "handoff" or (status == "blocked" and loop["blocked"])
    no_items = not (loop["open"] or loop["done"] or loop["blocked"])
    if (n_open or no_items) and not exit_ok:
        # A push while background work runs buys only a "still waiting" reply, and the context
        # re-read that costs. Its completion notification starts a new turn, and this hook with it;
        # spec_loop_timer.py covers the cache running out first, and a completion that never wakes.
        if n_open and running_tasks(payload):
            return None
        idle = state.get("idle", 0) + 1 if pushed and n_open >= state.get("last_open", n_open + 1) else 0
        # The timer's one wake after a let-go is the extra turn; pushes on top of it would make three.
        # Progress earns the pushes back, and so does any turn the wake did not start.
        woken = state.pop("woken", False)
        if woken and n_open >= state.get("last_open", n_open + 1):
            idle = MAX_IDLE_PUSHES
        elif not pushed and not woken:
            state.pop("woken_for", None)
        if idle < MAX_IDLE_PUSHES:
            save(last_open=n_open, idle=idle, last_block="work", let_go=None)
            if no_items:
                return _block(f"Spec loop {rel} has no items. List the spec's deliverables as "
                              "`- [ ]` lines in it, then carry on working through them.")
            listed = "\n".join(f"- [ ] {t}" for t in loop["open"][:12])
            return _block(
                f"Spec loop not finished: {n_open} open item(s) in {rel}. Do not stop and do not "
                f"ask whether to continue -- carry on with the next one:\n{listed}\n"
                "Tick each item `- [x]` in the file as it lands. Legitimate exits only: every item "
                "done; an item needing a decision only the owner can make -> mark it `- [!] ... "
                "— precisa de: <decisão>` and set `status: blocked` (independent items still get "
                "done first); context past its ceiling -> `status: handoff` and run the "
                "nucleos-compact skill.")
        print(f"spec loop: {MAX_IDLE_PUSHES} pushes closed nothing; letting the stop go", file=sys.stderr)
        # spec_loop_timer.py reads this: nothing else would look at the session again.
        first = state.get("let_go") != n_open
        save(let_go=n_open or None)
        if first:
            report_let_go(payload, n_open)

    # 2. The checklist moved since the last accepted report: the final message must report it.
    mtime = path.stat().st_mtime
    if state.get("reported_mtime") != mtime:
        missing = missing_headings(last_assistant_text(payload))
        if missing and not (pushed and state.get("last_block") == "report"):
            save(last_block="report")
            return _block(
                "End the session with the report, in European Portuguese, using exactly these "
                f"headings (missing: {', '.join(missing)}):\n{REPORT_TEMPLATE}\n"
                f"Feito/Falta come from {rel}; Verificação is what was actually run, not what "
                "should pass.")
        save(reported_mtime=mtime, last_block=None, idle=0)

    if not loop["open"] and not loop["blocked"]:
        path.replace(path.with_name(f"{session}.closed.md"))
        state_path.unlink(missing_ok=True)
    return None


def prompt_context(payload: dict) -> str | None:
    session = payload.get("session_id")
    loops = loops_dir(payload.get("cwd"))
    if not session or not loops:
        return None
    path = loop_path(loops, session)
    if path.exists():
        loop = parse(path.read_text(encoding="utf-8"))
        return (f"Spec loop open: {path.as_posix()} (status {loop['status']}, "
                f"{len(loop['open'])} open, {len(loop['blocked'])} blocked). Keep it current; the "
                "Stop hook holds the session until it is done or legitimately parked.")
    if _SPEC_PROMPT.search(payload.get("prompt") or ""):
        return ("If this prompt asks you to work on a spec or plan (not merely to discuss one), "
                f"open a spec loop first: `python \"{Path(__file__).resolve().as_posix()}\" start "
                "<spec-path> \"<deliverable>\" ...` -- one item per deliverable the spec asks for. The Stop "
                "hook then holds the session until every item is `- [x]`, or one is `- [!]` with "
                "`status: blocked`, or `status: handoff` at the context ceiling, and requires the "
                f"final report:\n{REPORT_TEMPLATE}")
    return None


def start(argv: list[str]) -> int:
    session = os.environ.get("CLAUDE_CODE_SESSION_ID")
    name = None
    rest: list[str] = []
    i = 0
    while i < len(argv):
        if argv[i] == "--name" and i + 1 < len(argv):
            name = argv[i + 1]
            i += 2
        else:
            rest.append(argv[i])
            i += 1
    argv = rest
    if not argv or not session:
        print("usage: spec_loop.py start [--name <id>] <spec-path> [\"item\" ...]  "
              "(needs CLAUDE_CODE_SESSION_ID)", file=sys.stderr)
        return 2
    name = name or os.environ.get("NUCLEOS_LOOP_NAME") or session
    try:
        loops = nucleos_paths.state_dir() / "loops"
    except nucleos_paths.ProjectNotFound as exc:
        print(f"spec loop: {exc}", file=sys.stderr)
        return 2
    path = loop_path(loops, name)
    if path.exists():
        m = re.search(r"^session:\s*(\S+)", path.read_text(encoding="utf-8"), re.MULTILINE)
        if m and m.group(1) != session:
            print(f"spec loop '{name}' is owned by another session ({m.group(1)}); "
                  "pick a different --name", file=sys.stderr)
            return 2
        print(f"spec loop already open: {path}")
        return 0
    path.parent.mkdir(parents=True, exist_ok=True)
    items = "\n".join(f"- [ ] {i}" for i in argv[1:])
    path.write_text(
        f"# Spec loop\nspec: {argv[0]}\nsession: {session}\nstatus: active\n"
        "<!-- active | blocked (needs a [!] item) | handoff (context ceiling) | done -->\n\n"
        f"## Items\n{items}\n", encoding="utf-8")
    print(f"spec loop opened: {path}")
    return 0


def _payload() -> dict:
    try:
        # Bytes, decoded as UTF-8: on Windows sys.stdin uses the ANSI code page (cp1252), which
        # turns "Verificação" into "VerificaÃ§Ã£o" and fails the accented report headings.
        data = json.loads(sys.stdin.buffer.read().decode("utf-8") or "{}")
    except (OSError, UnicodeError, ValueError):
        return {}
    return data if isinstance(data, dict) else {}


def main(argv: list[str]) -> int:
    if argv[:1] == ["start"]:
        return start(argv[1:])
    if argv in (["--stop-hook"], ["--prompt-hook"]):
        try:
            payload = _payload()
            if argv[0] == "--stop-hook":
                out = stop_decision(payload)
            else:
                ctx = prompt_context(payload)
                out = ctx and {"hookSpecificOutput": {"hookEventName": "UserPromptSubmit",
                                                      "additionalContext": ctx}}
        except Exception as exc:  # fail-open: a broken guard must never wedge a session
            print(f"spec loop: {exc}", file=sys.stderr)
            return 0
        if out:
            # ASCII-escaped: a Windows console's code page cannot print the report's accents.
            print(json.dumps(out))
        return 0
    print("usage: spec_loop.py start <spec> [items...] | --stop-hook | --prompt-hook", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
