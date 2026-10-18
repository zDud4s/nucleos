#!/usr/bin/env python3
"""The wave engine: a GREEN phase fanned out to one isolated worktree per unit.

Every decision a wave takes lives here, where it is tested; the orchestrate-tdd skill only
sequences the verbs and dispatches the workers this opens. The wave's slots come from the
lease client, `wave_lease.py`, which this calls through its CLI and nothing else.

Per-project state lives under `<state_dir>/wave/` (found through `.nucleos/workflow/project.json`):
the engine's wave files in `engine/`, each unit's packet and worker output in the wave's scratch
directory, the unit worktrees under `parallel_execute.scratch_root` (relative to `<state_dir>`).

Usage:
  python <bundle_dir>/scripts/wave_engine.py prepare --cwd . --plan <plan.md> --packets-dir <dir> \
      --workers 2 --frozen <file> [--risk-glob G ...]
  python <bundle_dir>/scripts/wave_engine.py next --wave-id 12
  python <bundle_dir>/scripts/wave_engine.py exec --wave-id 12 --unit a --archive <prompt> \
      --tool codex --model <m> -- <worker argv>
  python <bundle_dir>/scripts/wave_engine.py consolidate --wave-id 12
  python <bundle_dir>/scripts/wave_engine.py close --wave-id 12        # always; exit 0

`prepare` answers `"mode": "sequential"` whenever there is no wave to run, and then it has
created nothing. `close` is the `finally` of every wave that `prepare` opened.
"""
from __future__ import annotations

import argparse
import fnmatch
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import asdict, dataclass, fields
from pathlib import Path
from typing import Callable

import yaml

import nucleos_paths

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

import evidence_gate  # noqa: E402  -- its packet parser only; the gate itself runs as a process

# The closed `degraded` vocabulary of the `record` tool: the part this engine emits. Named once,
# so a typo is a failing test and not a record the daemon refuses.
WAVE_DISABLED = "wave_disabled"
NOT_DECOMPOSABLE = "task_not_decomposable"
LANE = "lane_violation"
EXHAUSTED = "slots_exhausted"
BUDGET = "wave_budget_exhausted"
REASONS = (WAVE_DISABLED, NOT_DECOMPOSABLE, LANE, EXHAUSTED, BUDGET)

MANIFEST_HEADER = "## Parallel execute manifest"
SCHEMA_VERSION = "parallel-execute/v1"
# Stricter than the spec's pattern: a unit id names a git branch and a directory on a
# case-insensitive file system .
UNIT_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,39}$")
GLOB_CHARS = frozenset("*?[]{}")
LOCKFILES = frozenset({"package-lock.json", "go.sum", "pnpm-lock.yaml", "yarn.lock"})
CONFIG_FILES = frozenset({"cargo.toml", "package.json", "go.mod", "tsconfig.json", "pyproject.toml"})
WORKFLOW_DIRS = (".nucleos/", ".claude/", ".agents/")
# A unit's tree holds every other unit's RED tests, failing by construction: a unit proves only
# its own tests, and the only verification is the controller's `verify` after landing.
CONTROLLER_ONLY_COMMANDS = ("select_tests.py", "gates.sh", "mcp__nucleos__verify")


class ConfigError(ValueError):
    """`parallel_execute` in `<state_dir>/models.yaml` is missing or not usable."""


class ManifestError(ValueError):
    """The plan carries no readable `## Parallel execute manifest`."""


class WaveError(RuntimeError):
    """A wave step that cannot go on. The caller closes the wave."""


@dataclass(frozen=True)
class EngineConfig:
    enabled: bool
    green_enabled: bool
    max_units: int
    worker_timeout_seconds: int
    max_wave_seconds: int
    max_total_worker_tokens: int
    max_total_cost_usd: float
    lane_violation_abort_ratio: float
    scratch_root: str


INT_KEYS = ("max_units", "worker_timeout_seconds", "max_wave_seconds", "max_total_worker_tokens")


def _is_number(value) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def read_engine_config(path: Path) -> EngineConfig:
    """The part of `parallel_execute` the engine obeys. `max_workers` and `max_wave_seconds`
    are also read by `wave_lease.py`, which applies its own copy of them to the lease."""
    try:
        block = (yaml.safe_load(path.read_text(encoding="utf-8")) or {})["parallel_execute"]
        values = {item.name: block[item.name] for item in fields(EngineConfig)}
    except (OSError, yaml.YAMLError, KeyError, TypeError) as error:
        raise ConfigError(f"parallel_execute is unreadable in {path}: {error!r}") from error
    for name in ("enabled", "green_enabled"):
        if not isinstance(values[name], bool):
            raise ConfigError(f"parallel_execute.{name} must be true or false, not {values[name]!r}")
    for name in INT_KEYS:
        value = values[name]
        if not isinstance(value, int) or isinstance(value, bool) or value < 1:
            raise ConfigError(f"parallel_execute.{name} must be an integer >= 1, not {value!r}")
    cost, ratio = values["max_total_cost_usd"], values["lane_violation_abort_ratio"]
    if not _is_number(cost) or cost <= 0:
        raise ConfigError(f"parallel_execute.max_total_cost_usd must be > 0, not {cost!r}")
    if not _is_number(ratio) or not 0 < ratio <= 1:
        raise ConfigError(f"parallel_execute.lane_violation_abort_ratio must be in (0, 1], not {ratio!r}")
    root = values["scratch_root"]
    if (not isinstance(root, str) or not root or root.startswith(("/", "\\"))
            or re.match(r"^[A-Za-z]:", root) or ".." in re.split(r"[\\/]", root)):
        raise ConfigError(f"parallel_execute.scratch_root must be a relative path inside the state directory, not {root!r}")
    values["max_total_cost_usd"] = float(cost)
    values["lane_violation_abort_ratio"] = float(ratio)
    return EngineConfig(**values)


@dataclass(frozen=True)
class Unit:
    unit_id: str
    packet_id: str
    owned_paths: tuple[str, ...]
    test_refs: tuple[str, ...]
    objective: str


def parse_manifest(text: str) -> dict:
    """The first ```json fence under `## Parallel execute manifest` (planner/SKILL.md:131-153)."""
    lines = text.splitlines()
    try:
        start = next(i for i, line in enumerate(lines) if line.strip() == MANIFEST_HEADER)
    except StopIteration:
        raise ManifestError(f"no '{MANIFEST_HEADER}' section") from None
    body: list[str] = []
    inside = closed = False
    for line in lines[start + 1:]:
        stripped = line.strip()
        if not inside:
            if stripped.startswith("## "):
                break
            if stripped == "```json":
                inside = True
            continue
        if stripped == "```":
            closed = True
            break
        body.append(line)
    if not inside:
        raise ManifestError("no ```json fence under the manifest header")
    if not closed:
        raise ManifestError("the manifest's ```json fence is never closed")
    try:
        data = json.loads("\n".join(body))
    except ValueError as error:
        raise ManifestError(f"the manifest is not JSON: {error}") from error
    if not isinstance(data, dict):
        raise ManifestError("the manifest must be a JSON object")
    return data


def normalize(path: str) -> str:
    """The comparable spelling of a path from the frozen list: forward slashes, no `./`."""
    path = path.strip().replace("\\", "/")
    while path.startswith("./"):
        path = path[2:]
    return path


def path_problem(path) -> str | None:
    """Why an owned path is not a normalized, repo-relative file -- or None."""
    if not isinstance(path, str) or not path:
        return f"{path!r}: an owned path must be a non-empty string"
    if path.startswith(("/", "\\")) or re.match(r"^[A-Za-z]:", path):
        return f"{path}: absolute"
    if "\\" in path:
        return f"{path}: backslash; owned paths use '/'"
    if path.endswith("/"):
        return f"{path}: a directory, not a file"
    if GLOB_CHARS & set(path):
        return f"{path}: a glob, not a file"
    if any(part in ("", ".", "..") for part in path.split("/")):
        return f"{path}: not normalized"
    return None


def forbidden_kind(path: str, frozen_lower: set[str], risk_globs: list[str]) -> str | None:
    """What makes an owned path one no unit may own -- or None (planner/SKILL.md:135-137)."""
    lower = path.lower()
    name = lower.rsplit("/", 1)[-1]
    if lower in frozen_lower:
        return "a frozen RED test"
    if name == "__init__.py":
        return "a package __init__ (a shared registry)"
    if name.endswith(".lock") or name in LOCKFILES:
        return "a lockfile"
    if name in CONFIG_FILES or lower.startswith(WORKFLOW_DIRS):
        return "configuration"
    for glob in risk_globs:
        if fnmatch.fnmatchcase(lower, glob.lower()):
            return f"inside the risk boundary {glob}"
    return None


def packet_problems(label: str, packet: Path) -> list[str]:
    """A unit packet whose Validation is unreadable, or runs what only the controller may run."""
    try:
        commands = evidence_gate.validation_commands(packet.read_text(encoding="utf-8").splitlines())
    except (OSError, UnicodeError, evidence_gate.MalformedPacket, evidence_gate.UnfilledPacket) as error:
        return [f"unit {label}: packet {packet.name}: {error}"]
    return [f"unit {label}: its Validation runs {command!r}, which only the controller may run "
            "(the other units' RED tests fail in a unit's tree)"
            for command in commands if any(word in command for word in CONTROLLER_ONLY_COMMANDS)]


def unit_id_ok(value) -> bool:
    return (isinstance(value, str) and bool(UNIT_ID.match(value)) and ".." not in value
            and not value.lower().endswith(".lock"))


def validate_manifest(manifest: dict, *, packets_dir: Path, frozen: list[str],
                      risk_globs: list[str], max_units: int) -> tuple[list[Unit], list[str]]:
    """The units, or no units and every reason why not. Shared registries the planner did not
    name are the planner's to find (planner/SKILL.md:135-137); this checks what a file name can."""
    problems: list[str] = []
    if manifest.get("schema_version") != SCHEMA_VERSION:
        problems.append(f"schema_version must be {SCHEMA_VERSION!r}, not {manifest.get('schema_version')!r}")
    raw_units = manifest.get("units")
    if not isinstance(raw_units, list):
        return [], problems + ["units must be a list"]
    if not 2 <= len(raw_units) <= max_units:
        problems.append(f"{len(raw_units)} units; a wave takes 2..{max_units}")
    frozen_lower = {normalize(path).lower() for path in frozen if path.strip()}
    units: list[Unit] = []
    seen_ids: set[str] = set()
    seen_packets: set[str] = set()
    owner: dict[str, str] = {}
    for index, raw in enumerate(raw_units):
        if not isinstance(raw, dict):
            problems.append(f"unit #{index}: not an object")
            continue
        unit_id = raw.get("unit_id")
        label = unit_id if isinstance(unit_id, str) else f"#{index}"
        if not unit_id_ok(unit_id):
            problems.append(f"unit {label}: unit_id must start with a letter or digit, use only "
                            "letters, digits, '.', '_' or '-', be at most 40 long, and contain no "
                            "'..' nor end in '.lock'")
        elif unit_id.lower() in seen_ids:
            problems.append(f"unit {label}: duplicate unit_id (ignoring case)")
        else:
            seen_ids.add(unit_id.lower())
        packet_id = raw.get("packet_id")
        if not unit_id_ok(packet_id):
            problems.append(f"unit {label}: packet_id must be a name like a unit_id")
        elif packet_id in seen_packets:
            problems.append(f"unit {label}: duplicate packet_id {packet_id}")
        else:
            seen_packets.add(packet_id)
            packet = packets_dir / f"{packet_id}.md"
            if packet.is_file():
                problems.extend(packet_problems(label, packet))
            else:
                problems.append(f"unit {label}: no packet {packet_id}.md in {packets_dir}")
        if raw.get("depends_on") != []:
            problems.append(f"unit {label}: depends_on must be [] (the units of a wave run in any order)")
        paths = raw.get("owned_paths")
        if not isinstance(paths, list) or not paths:
            problems.append(f"unit {label}: owned_paths must be a non-empty list")
            paths = []
        for path in paths:
            bad = path_problem(path)
            if bad:
                problems.append(f"unit {label}: {bad}")
                continue
            kind = forbidden_kind(path, frozen_lower, risk_globs)
            if kind:
                problems.append(f"unit {label}: {path} is {kind}")
            other = owner.get(path.lower())
            if other == label:
                problems.append(f"unit {label}: {path} is listed twice")
            elif other is not None:
                problems.append(f"unit {label}: {path} is also owned by unit {other}")
            else:
                owner[path.lower()] = label
        test_refs = raw.get("test_refs", [])
        if not isinstance(test_refs, list) or not all(isinstance(ref, str) for ref in test_refs):
            problems.append(f"unit {label}: test_refs must be a list of paths")
            test_refs = []
        if isinstance(unit_id, str) and isinstance(packet_id, str):
            units.append(Unit(unit_id=unit_id, packet_id=packet_id,
                              owned_paths=tuple(p for p in paths if isinstance(p, str)),
                              test_refs=tuple(test_refs), objective=str(raw.get("objective", ""))))
    return ([] if problems else units), problems


# --- git, paths and state ----------------------------------------------------------------------

def _git(cwd: Path, args: list[str], *, env: dict | None = None,
         data: bytes | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *args], cwd=str(cwd), capture_output=True, env=env, input=data)


def git(cwd: Path, *args: str, env: dict | None = None, check: bool = True) -> str:
    proc = _git(cwd, list(args), env=env)
    if check and proc.returncode != 0:
        raise WaveError(f"git {' '.join(args)} exited {proc.returncode}: "
                        f"{proc.stderr.decode('utf-8', 'replace').strip()}")
    return proc.stdout.decode("utf-8", "replace")


def toplevel(cwd: Path) -> Path:
    return Path(git(Path(cwd), "rev-parse", "--show-toplevel").strip()).resolve()


def main_checkout(root: Path) -> Path:
    """The main checkout: the parent of the shared git dir, whichever worktree `root` is."""
    common = git(root, "rev-parse", "--path-format=absolute", "--git-common-dir").strip()
    return Path(common).resolve().parent


def wave_home(root: Path) -> Path:
    """`<state_dir>/wave/`: everything per-project the wave writes. Outside the working tree."""
    return nucleos_paths.state_dir(root) / "wave"


def state_dir(root: Path) -> Path:
    return wave_home(root) / "engine"


def state_path(root: Path, wave_id: int) -> Path:
    return state_dir(root) / f"{wave_id}.json"


def lock_path(path: Path) -> Path:
    return path.with_suffix(".lock")


def unit_branch(wave_id: int, unit_id: str) -> str:
    return f"wave/{wave_id}/{unit_id}"


def integration_name(wave_id: int) -> str:
    """The consolidation tree's name. The queue guard's `*/integration-*` arm is what lets a
    merge commit be made in it, and nothing else does."""
    return f"integration-wave-{wave_id}"


def read_state(path: Path) -> dict | None:
    for _ in range(50):
        try:
            return json.loads(path.read_text(encoding="utf-8"))
        except FileNotFoundError:
            return None
        except PermissionError:  # Windows: a writer is replacing it in this instant
            time.sleep(0.02)
    return json.loads(path.read_text(encoding="utf-8"))


def write_state(path: Path, state: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(state, indent=2), encoding="utf-8")
    for _ in range(50):
        try:
            os.replace(temporary, path)
            return
        except PermissionError:  # Windows: a reader holds the file open in this instant
            time.sleep(0.02)
    os.replace(temporary, path)


def dedupe(items) -> list[str]:
    seen: list[str] = []
    for item in items:
        if item and item not in seen:
            seen.append(item)
    return seen


def metrics_args(requested: int, used: int, degraded: list[str], note: str | None) -> dict:
    """The payload fields every GREEN `metric` record of this task carries (the controller
    writes them with `mcp__nucleos__record`; this engine records nothing itself)."""
    fields_ = {"workers_requested": requested, "workers_used": used, "degraded": list(degraded)}
    if note:
        fields_["note"] = note
    return fields_


# --- the base and the unit trees ----------------------------------------------------------------

def snapshot_tree(root: Path) -> str:
    """The tree of everything the controller's checkout holds now -- tracked, modified, and
    untracked but not ignored -- written through a throwaway copy of the index, so the real
    index and the working tree are never touched."""
    real_index = Path(git(root, "rev-parse", "--path-format=absolute", "--git-path", "index").strip())
    with tempfile.TemporaryDirectory() as tmp:
        index = Path(tmp) / "index"
        if real_index.is_file():
            shutil.copyfile(real_index, index)
        env = {**os.environ, "GIT_INDEX_FILE": str(index)}
        git(root, "add", "-A", env=env)
        return git(root, "write-tree", env=env).strip()


def snapshot_base(root: Path, wave_id: int) -> tuple[str, str]:
    """(base commit, base tree). The RED tests are uncommitted in the controller's tree, so
    HEAD alone would hand every unit a tree without them."""
    tree = snapshot_tree(root)
    commit = git(root, "commit-tree", tree, "-p", "HEAD", "-m", f"wave {wave_id} base").strip()
    return commit, tree


WAVE_RULES = """\
## Wave unit rules

You are one worker of wave {wave_id}, unit `{unit_id}`. These rules come from the wave engine and win over anything above that disagrees with them.

- Work only in this worktree: `{worktree}`.
- Change only these files: {owned}. A change to any other file fails the unit as a lane violation.
- Do not commit: the engine commits your work when you return.
- If you commit anyway, never add a `Co-Authored-By` trailer: the repository's commit-msg hook refuses the commit.
- Run no tests: mark each of this packet's Validation commands `deferred: batched`. The other units' new tests fail in this tree by construction, and the controller verifies once, after the wave lands.
- Fill the `## Handoff` section of THIS file, `{packet}`. The engine runs the evidence gate on it when you return.
"""


def write_unit_files(files_dir: Path, worktree: Path, wave_id: int, unit: Unit, packet_text: str) -> Path:
    """The unit's packet with the wave's rules appended, and the unit's identity for whoever runs
    in the tree. They live in `files_dir` (under the state directory), never in the worktree, so
    they can neither be committed by the unit nor counted as one of its changes."""
    local = files_dir
    local.mkdir(parents=True, exist_ok=True)
    packet = local / "wave-unit-packet.md"
    rules = WAVE_RULES.format(wave_id=wave_id, unit_id=unit.unit_id, worktree=worktree.as_posix(),
                              owned=", ".join(f"`{path}`" for path in unit.owned_paths),
                              packet=packet.as_posix())
    packet.write_text(packet_text.rstrip("\n") + "\n\n" + rules, encoding="utf-8")
    (local / "wave-unit.json").write_text(json.dumps(
        {"wave_id": wave_id, "unit_id": unit.unit_id, "owned_paths": list(unit.owned_paths)}),
        encoding="utf-8")
    return packet


MATERIALIZED = (".nucleos/workflow", ".claude/skills", ".claude/agents", ".agents/skills")


def seed_from_main(main: Path, worktree: Path) -> None:
    """The materialized workflow (the bundle's own directories), copied into a unit tree that
    does not have it yet. A git worktree carries only tracked files, and the unit's worker needs
    the skills, the cockpits and `.nucleos/workflow/project.json`. Entries already present are left alone."""
    for relative in MATERIALIZED:
        source, target = Path(main) / relative, Path(worktree) / relative
        if source.is_dir() and not target.exists():
            try:
                shutil.copytree(source, target)
            except OSError as error:
                raise WaveError(f"copying {relative} into {worktree} failed: {error}") from error


def create_unit_tree(root: Path, main: Path, wave_id: int, unit: Unit, base: str,
                     worktree: Path, seeder: Callable[[Path, Path], None]) -> None:
    """Spec §4.2 order: the worktree, then the seeding from main."""
    worktree.parent.mkdir(parents=True, exist_ok=True)
    git(root, "worktree", "add", "-b", unit_branch(wave_id, unit.unit_id), str(worktree), base)
    seeder(main, worktree)


# --- the lease, through its CLI ------------------------------------------------------------

class Lease:
    """The plan-3 client, `wave_lease.py`, through its CLI and nothing else: argv in, a JSON body
    and an exit code out. Whatever it cannot read never counts as a grant."""

    def __init__(self, script: Path | None = None, python: str | None = None):
        self.script = Path(script) if script else SCRIPTS / "wave_lease.py"
        self.python = python or sys.executable

    def _call(self, *args: str) -> tuple[int, dict]:
        try:
            proc = subprocess.run([self.python, str(self.script), *args], capture_output=True,
                                  text=True, timeout=60)
        except (OSError, subprocess.TimeoutExpired) as error:
            return -1, {"error": f"wave_lease.py {args[0]}: {error!r}"}
        try:
            body = json.loads(proc.stdout)
        except ValueError:
            body = None
        if not isinstance(body, dict):
            return -1, {"error": f"wave_lease.py {args[0]} exited {proc.returncode}: "
                                 f"{(proc.stdout + proc.stderr).strip()[-300:]}"}
        return proc.returncode, body

    def open(self, cwd: str, units: int, workers: int) -> dict:
        code, body = self._call("open", "--cwd", cwd, "--units", str(units), "--workers", str(workers))
        if code != 0 or not isinstance(body.get("workers"), int):
            return {"workers": 1, "requested": workers, "granted": 0, "wave_id": None,
                    "lease_seconds": None, "degraded": [], "reason": None,
                    "lease_error": body.get("error") or f"wave_lease.py open exited {code}"}
        return body

    def status(self, wave_id: int) -> tuple[int, dict]:
        code, body = self._call("status", "--wave-id", str(wave_id))
        if code not in (0, 3):
            return 3, {"status": "unknown", "degraded": [], **body}
        return code, body

    def close(self, wave_id: int, cwd: str) -> dict:
        code, body = self._call("close", "--wave-id", str(wave_id), "--cwd", cwd)
        if code != 0 or "wave_id" not in body:
            return {"wave_id": wave_id, "released": False, "ended_as": "unknown",
                    "unguarded_since": None, "degraded": [],
                    "error": body.get("error") or f"wave_lease.py close exited {code}"}
        return body


# --- prepare --------------------------------------------------------------------------------------

def sequential(requested: int, *, degraded=(), note: str | None = None,
               config_error: str | None = None, swept=()) -> dict:
    """The answer that means: no wave; run GREEN the ordinary way, with these row flags."""
    notes = [part for part in (note, f"config_error: {config_error}" if config_error else None) if part]
    joined = "; ".join(notes) or None
    reasons = dedupe(degraded)
    return {"mode": "sequential", "workers_requested": requested, "workers_used": 1,
            "degraded": reasons, "note": joined, "config_error": config_error, "swept": list(swept),
            "metrics_args": metrics_args(requested, 1, reasons, joined)}


def build_wave(root: Path, wave_id: int, units: list[Unit], packets_dir: Path, opening: dict,
               requested: int, config: EngineConfig, seeder: Callable[[Path, Path], None],
               now: Callable[[], float]) -> dict:
    main = main_checkout(root)
    scratch = nucleos_paths.state_dir(root) / config.scratch_root / str(wave_id)
    state = {
        "wave_id": wave_id, "cwd": str(root), "main": str(main), "scratch": str(scratch),
        "base": None, "base_tree": None, "opened_at": now(), "requested": requested,
        "workers": opening["workers"], "degraded": dedupe(opening.get("degraded") or []),
        "lease_seconds": opening.get("lease_seconds"), "config": asdict(config),
        "units": {unit.unit_id: {
            "status": "pending", "worktree": str(scratch / unit.unit_id),
            "branch": unit_branch(wave_id, unit.unit_id),
            "packet": None,
            "files_dir": str(scratch / "~files" / unit.unit_id),
            "owned_paths": list(unit.owned_paths), "started_at": None, "duration_ms": None,
            "tokens": None, "cost_usd": None, "exit_code": None, "outcome": None,
        } for unit in units},
        "workers_used": 0, "unguarded_since": None, "stop_reasons": [], "aborted": False,
        "integration": None, "landed": None, "pending": None, "note": None,
    }
    path = state_path(root, wave_id)
    write_state(path, state)  # first, so `close` finds whatever exists if anything below fails
    state["base"], state["base_tree"] = snapshot_base(root, wave_id)
    write_state(path, state)
    for unit in sorted(units, key=lambda item: item.unit_id):
        entry = state["units"][unit.unit_id]
        worktree = Path(entry["worktree"])
        create_unit_tree(root, main, wave_id, unit, state["base"], worktree, seeder)
        text = (packets_dir / f"{unit.packet_id}.md").read_text(encoding="utf-8")
        entry["packet"] = str(write_unit_files(Path(entry["files_dir"]), worktree, wave_id, unit, text))
        write_state(path, state)
    return state


def prepare(*, cwd: Path, plan: Path, packets_dir: Path, workers: int, frozen: list[str],
            risk_globs: list[str], lease, seeder: Callable[[Path, Path], None] = seed_from_main,
            now: Callable[[], float] = time.time) -> dict:
    """A wave, or the sequential answer. Never both, and the sequential answer has created nothing."""
    root = toplevel(Path(cwd))
    swept = sweep(root, lease)
    try:
        config = read_engine_config(nucleos_paths.state_dir(root) / "models.yaml")
    except (ConfigError, nucleos_paths.ProjectNotFound) as error:
        return sequential(workers, config_error=str(error), swept=swept)
    if not (config.enabled and config.green_enabled):
        return sequential(workers, degraded=[WAVE_DISABLED], swept=swept)
    try:
        manifest = parse_manifest(Path(plan).read_text(encoding="utf-8"))
    except (OSError, UnicodeError, ManifestError) as error:
        return sequential(workers, degraded=[NOT_DECOMPOSABLE], note=f"manifest: {error}", swept=swept)
    units, problems = validate_manifest(manifest, packets_dir=Path(packets_dir), frozen=frozen,
                                        risk_globs=list(risk_globs), max_units=config.max_units)
    if problems:
        return sequential(workers, degraded=[NOT_DECOMPOSABLE],
                          note="manifest: " + "; ".join(problems), swept=swept)
    opening = lease.open(str(root), len(units), workers)
    wave_id = opening.get("wave_id")
    if wave_id is None or opening.get("workers", 1) <= 1:
        if wave_id is not None:  # the lease contract says this cannot happen; give it back anyway
            lease.close(wave_id, str(root))
        return sequential(workers, degraded=opening.get("degraded") or [],
                          note=opening.get("lease_error"), config_error=opening.get("config_error"),
                          swept=swept)
    try:
        state = build_wave(root, wave_id, units, Path(packets_dir), opening, workers, config,
                           seeder, now)
    except Exception as error:  # noqa: BLE001 -- any failure after `open` closes the wave
        close(root=root, wave_id=wave_id, lease=lease)
        return sequential(workers, degraded=opening.get("degraded") or [],
                          note=f"wave setup failed and the wave was closed: {error}", swept=swept)
    return {"mode": "wave", "wave_id": wave_id, "workers": state["workers"],
            "workers_requested": workers, "units": sorted(state["units"]),
            "degraded": state["degraded"], "state_file": str(state_path(root, wave_id)),
            "swept": swept}


# --- close and sweep -----------------------------------------------------------------------------

def close_summary(state: dict, lease_end: dict) -> dict:
    units = sorted(state["units"])
    landed = state.get("landed") or []
    pending = state["pending"] if state.get("pending") is not None else [u for u in units if u not in landed]
    unguarded = lease_end.get("unguarded_since") or state.get("unguarded_since")
    degraded = dedupe([*state.get("degraded", []), *state.get("stop_reasons", []),
                       *(lease_end.get("degraded") or [])])
    notes = [state.get("note")]
    if unguarded:
        notes.append(f"lease lapsed at {unguarded} with workers running")
    if lease_end.get("error"):
        notes.append(f"lease close: {lease_end['error']}")
    note = "; ".join(part for part in notes if part) or None
    used = max(1, state.get("workers_used") or 0)
    return {"wave_id": state["wave_id"], "released": lease_end.get("released", False),
            "ended_as": lease_end.get("ended_as"), "unguarded_since": unguarded,
            "workers_requested": state["requested"], "workers_used": used, "degraded": degraded,
            "landed": landed, "pending": pending, "note": note,
            "metrics_args": metrics_args(state["requested"], used, degraded, note)}


def remove_dir(path: str) -> None:
    shutil.rmtree(path, ignore_errors=True)


def _same_path(path: str) -> str:
    return os.path.normcase(str(Path(path).resolve()))


def close_leftovers(root: Path, state: dict, trees: list[str]) -> list[str]:
    """What cleanup could not give back. On Windows a directory a running worker still holds
    refuses removal, and its tree stays listed: the state is kept, and the next `sweep` retries."""
    listed = {_same_path(line[len("worktree "):])
              for line in git(root, "worktree", "list", "--porcelain", check=False).splitlines()
              if line.startswith("worktree ")}
    left = [f"worktree still listed: {tree}" for tree in trees if _same_path(tree) in listed]
    left += [f"branch still present: {branch}" for branch in git(
        root, "for-each-ref", "--format=%(refname:short)", f"refs/heads/wave/{state['wave_id']}/",
        check=False).split()]
    left += [f"directory still present: {directory}"
             for directory in [state["scratch"]]
             if Path(directory).exists()]
    return left


def close(*, root: Path, wave_id: int, lease, remove: Callable[[str], None] = remove_dir) -> dict:
    """Give everything back: the lease, every tree, every branch, the state.
    Idempotent; a call after a complete close answers `{"status": "closed"}`. Running workers
    are not killed (owner option A); their trees are removed under them. When something
    survives the cleanup, the state stays behind as `closing` with its `leftovers`, and the
    next `sweep` retries -- the lease is given back once, and its answer is kept."""
    path = state_path(root, wave_id)
    state = read_state(path)
    if state is None:
        return {"wave_id": wave_id, "status": "closed"}
    lease_end = state.get("lease_end") or lease.close(wave_id, str(root))
    trees = [entry["worktree"] for entry in state["units"].values()]
    if state.get("integration"):
        trees.append(state["integration"])
    for tree in trees:
        git(root, "worktree", "remove", "--force", tree, check=False)
    remove(state["scratch"])
    git(root, "worktree", "prune", check=False)
    for branch in git(root, "for-each-ref", "--format=%(refname:short)",
                      f"refs/heads/wave/{wave_id}/", check=False).split():
        git(root, "branch", "-D", branch, check=False)
    leftovers = close_leftovers(root, state, trees)
    summary = {**close_summary(state, lease_end), "status": "closing" if leftovers else "closed",
               "leftovers": leftovers}
    if leftovers:
        try:
            mutate_state(path, lambda current: current.update(status="closing", leftovers=leftovers,
                                                              lease_end=lease_end))
        except WaveError:
            pass  # closed by someone else meanwhile: nothing left to keep
        return summary
    for leftover in (path, lock_path(path)):
        try:
            leftover.unlink(missing_ok=True)
        except OSError:
            pass
    return summary


def sweep(root: Path, lease) -> list[dict]:
    """Close every wave this checkout left open: a session interrupted mid-wave never reached
    `close`, and a `closing` wave is one whose cleanup is retried here."""
    directory = state_dir(root)
    closed = []
    for path in sorted(directory.glob("*.json")) if directory.is_dir() else []:
        try:
            wave_id = int(path.stem)
        except ValueError:
            continue
        closed.append(close(root=root, wave_id=wave_id, lease=lease))
    return closed


# --- the state lock and the scheduler -------------------------------------------------------------

LOCK_TIMEOUT_SECONDS = 60
STALE_LOCK_SECONDS = 600
RUNNING_GRACE_SECONDS = 600  # past worker_timeout_seconds + this, a `running` unit is `errored`
ERRORED = "errored"  # the engine failed around a unit (its exec died or raised): a failed unit


class StateLock:
    """An exclusive lock beside a wave's state file. Two `exec`s can finish at the same moment,
    and each reads, changes and writes the same state."""

    def __init__(self, path: Path, timeout: float = LOCK_TIMEOUT_SECONDS):
        self.path = lock_path(path)
        self.timeout = timeout

    def __enter__(self) -> "StateLock":
        self.path.parent.mkdir(parents=True, exist_ok=True)
        deadline = time.monotonic() + self.timeout
        while True:
            try:
                handle = os.open(self.path, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
            except (FileExistsError, PermissionError):  # PermissionError: Windows, delete pending
                try:
                    if time.time() - self.path.stat().st_mtime > STALE_LOCK_SECONDS:
                        self.path.unlink(missing_ok=True)  # held by an exec that died
                        continue
                except OSError:
                    pass
                if time.monotonic() > deadline:
                    raise WaveError(f"the wave state lock {self.path} was held for over "
                                    f"{self.timeout}s") from None
                time.sleep(0.01)
                continue
            os.write(handle, str(os.getpid()).encode())
            os.close(handle)
            return self

    def __exit__(self, *_exc) -> None:
        try:
            self.path.unlink()
        except OSError:
            pass


def mutate_state(path: Path, change: Callable[[dict], object]) -> tuple[dict, object]:
    """Read, change and write a wave's state under its lock. Returns (state, change's result)."""
    with StateLock(path):
        state = read_state(path)
        if state is None:
            raise WaveError(f"no wave state at {path}: the wave was closed")
        result = change(state)
        write_state(path, state)
        return state, result


def lease_reasons(body: dict) -> list[str]:
    """What a stopped lease degrades the wave by: its own reasons, else `slots_exhausted`."""
    return list(body.get("degraded") or []) or [EXHAUSTED]


def decide(state: dict, now: float, lease_code: int, lease_body: dict) -> dict:
    """What the controller does next: `open` a unit, `wait` for one, or the wave is `drained`.
    Pure; `next_step` persists it. Running units are never stopped (owner option A): only
    opening more is, and every stop is final."""
    config, units = state["config"], state["units"]
    # A unit `running` past its timeout plus a grace was left by an `exec` that died before
    # recording it: it is `errored` (a failed unit, so it comes back sequentially), never a slot.
    limit = config["worker_timeout_seconds"] + RUNNING_GRACE_SECONDS
    expired = [uid for uid in sorted(units) if units[uid]["status"] == "running"
               and units[uid].get("started_at") is not None and now - units[uid]["started_at"] > limit]
    running = [uid for uid in sorted(units) if units[uid]["status"] == "running" and uid not in expired]
    waiting = [uid for uid in sorted(units) if units[uid]["status"] == "pending"]
    stop = list(state.get("stop_reasons") or [])
    aborted = bool(state.get("aborted"))
    unguarded = state.get("unguarded_since")
    lanes = sum(1 for entry in units.values() if entry.get("outcome") == LANE)
    if lanes and lanes / len(units) >= config["lane_violation_abort_ratio"]:
        aborted = True
        stop.append(LANE)
    if waiting and not aborted and not stop:
        if lease_code != 0:
            stop += lease_reasons(lease_body)
            unguarded = unguarded or lease_body.get("unguarded_since")
        tokens = sum(entry.get("tokens") or 0 for entry in units.values())
        cost = sum(entry["cost_usd"] for entry in units.values() if entry.get("cost_usd") is not None)
        if (now - state["opened_at"] >= config["max_wave_seconds"]
                or tokens >= config["max_total_worker_tokens"]
                or cost >= config["max_total_cost_usd"]):
            stop.append(BUDGET)
    stop = dedupe(stop)
    if aborted or stop or not waiting:
        action, unit = ("wait" if running else "drained"), None
    elif len(running) < state["workers"]:
        action, unit = "open", waiting[0]
    else:
        action, unit = "wait", None
    return {"action": action, "unit": unit, "running": running, "expired": expired,
            "stop_reasons": stop, "aborted": aborted, "unguarded_since": unguarded}


def next_step(*, root: Path, wave_id: int, lease, now: Callable[[], float] = time.time) -> dict:
    """Ask the lease, decide, persist. A unit this opens is `running` from here on."""
    path = state_path(root, wave_id)
    code, body = lease.status(wave_id)

    def apply(state: dict) -> dict:
        moment = now()
        decision = decide(state, moment, code, body)
        state["stop_reasons"] = decision["stop_reasons"]
        state["aborted"] = decision["aborted"]
        state["unguarded_since"] = decision["unguarded_since"]
        for uid in decision["expired"]:
            state["units"][uid].update(status="failed", outcome=ERRORED)
        if decision["action"] == "open":
            entry = state["units"][decision["unit"]]
            entry["status"] = "running"
            entry["started_at"] = moment
            state["workers_used"] = max(state.get("workers_used") or 0, len(decision["running"]) + 1)
            decision.update(worktree=entry["worktree"], packet=entry["packet"])
        return decision

    _, decision = mutate_state(path, apply)
    return {"wave_id": wave_id, **decision}


# --- exec: one worker, from its prompt to its metrics row ---------------------------------------

EXIT_TIMEOUT = 124  # what `timeout <T>s` answers, which a worker meets today
EXIT_NOT_STARTED = 127
EXIT_ENGINE_ERROR = 125  # the engine itself raised before the worker produced an exit code


@dataclass(frozen=True)
class WorkerRun:
    exit_code: int
    timed_out: bool
    duration_ms: int
    stdout: str
    stderr: str


def run_worker(argv: list[str], *, cwd: Path, archive: Path,
               timeout: int, out_dir: Path | None = None) -> WorkerRun:
    """The worker, with the prompt archive on stdin, in its tree.
    On timeout only the direct child is killed: parity with `timeout <T>s`."""
    local = out_dir or cwd.parent / "~files" / cwd.name
    local.mkdir(parents=True, exist_ok=True)
    out_path, err_path = local / "worker.out", local / "worker.err"
    env = dict(os.environ)
    # On Windows a `.cmd` shim (npm's codex, claude) starts only by its full path.
    command = [shutil.which(argv[0]) or argv[0], *argv[1:]]
    started = time.monotonic()
    timed_out = False
    with open(archive, "rb") as stdin, open(out_path, "wb") as out, open(err_path, "wb") as err:
        try:
            proc = subprocess.Popen(command, cwd=str(cwd), stdin=stdin, stdout=out, stderr=err, env=env)
        except OSError as error:
            err.write(f"wave_engine: cannot start the worker: {error}\n".encode())
            code = EXIT_NOT_STARTED
        else:
            try:
                code = proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
                code, timed_out = EXIT_TIMEOUT, True
    duration_ms = int((time.monotonic() - started) * 1000)
    return WorkerRun(code, timed_out, duration_ms,
                     out_path.read_text(encoding="utf-8", errors="replace"),
                     err_path.read_text(encoding="utf-8", errors="replace"))


def _last_json_object(text: str) -> dict | None:
    candidates = [text.strip()] + [line.strip() for line in reversed(text.splitlines())
                                   if line.strip().startswith("{")]
    for candidate in candidates:
        try:
            value = json.loads(candidate)
        except ValueError:
            continue
        if isinstance(value, dict):
            return value
    return None


def _count(value) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def usage_from_output(stdout: str, stderr: str) -> dict:
    """claude's JSON object (`.usage.*`, plus its `total_cost_usd`); else
    codex's `tokens used` total (tokens_out only, digits of the label line
    or of the line after it). codex reports no cost: unmetered, `cost_usd` None."""
    usage = {"tokens_in": None, "tokens_out": None, "cache_read": None, "cache_creation": None,
             "cost_usd": None}
    body = _last_json_object(stdout)
    if body is not None and isinstance(body.get("usage"), dict):
        counts = body["usage"]
        usage.update(tokens_in=_count(counts.get("input_tokens")),
                     tokens_out=_count(counts.get("output_tokens")),
                     cache_read=_count(counts.get("cache_read_input_tokens")),
                     cache_creation=_count(counts.get("cache_creation_input_tokens")))
        if _is_number(body.get("total_cost_usd")):
            usage["cost_usd"] = float(body["total_cost_usd"])
        return usage
    lines = stderr.splitlines()
    labels = [index for index, line in enumerate(lines) if "tokens used" in line.lower()]
    if labels:
        at = labels[-1]
        digits = "".join(ch for ch in lines[at].lower().split("tokens used", 1)[1] if ch.isdigit())
        if not digits and at + 1 < len(lines):
            digits = "".join(ch for ch in lines[at + 1] if ch.isdigit())
        if digits:
            usage["tokens_out"] = int(digits)
    return usage


def run_evidence_gate(packet: Path) -> int:
    """`evidence_gate.py <packet>`, the same gate a sequential execute Handoff meets.
    Exit 1 or 2 fails the unit, never the wave."""
    proc = subprocess.run([sys.executable, str(SCRIPTS / "evidence_gate.py"), str(packet)],
                          capture_output=True, text=True)
    return proc.returncode


def _paths(output: str) -> list[str]:
    return [path for path in output.split("\0") if path]


def changed_paths(worktree: Path, base: str) -> list[str]:
    """Every file the worker changed against the base: committed or not, tracked or new.
    Ignored files are no unit's to change and do not count."""
    tracked = _paths(git(worktree, "diff", "--name-only", "--no-renames", "-z", base))
    untracked = _paths(git(worktree, "ls-files", "--others", "--exclude-standard", "-z"))
    return sorted(set(tracked) | set(untracked))


def lane_violations(changed: list[str], owned: list[str]) -> list[str]:
    allowed = {path.lower() for path in owned}
    return [path for path in changed if path.lower() not in allowed]


def uncommitted_paths(worktree: Path) -> list[str]:
    tracked = _paths(git(worktree, "diff", "--name-only", "--no-renames", "-z", "HEAD"))
    untracked = _paths(git(worktree, "ls-files", "--others", "--exclude-standard", "-z"))
    return sorted(set(tracked) | set(untracked))


def commit_unit(worktree: Path, wave_id: int, unit_id: str) -> None:
    """Commit what the worker left uncommitted -- only those paths, through the repository's
    hooks, with no trailer. Called after the lane check, so every path is an owned one."""
    paths = uncommitted_paths(worktree)
    if not paths:
        return
    git(worktree, "add", "--", *paths)
    git(worktree, "commit", "-q", "-m", f"wave {wave_id}: unit {unit_id}")


def finish_unit(*, worktree: Path, base: str, owned: list[str], packet: Path, wave_id: int,
                unit_id: str, run: WorkerRun,
                evidence: Callable[[Path], int] = run_evidence_gate) -> tuple[str, str]:
    """(outcome, handoff_complete for the row). Only `done` is merged."""
    if run.timed_out:
        return "timeout", "null"
    if run.exit_code != 0:
        return "failed", "null"
    if evidence(packet) != 0:
        return "unproven", "false"
    if lane_violations(changed_paths(worktree, base), owned):
        return LANE, "true"
    try:
        commit_unit(worktree, wave_id, unit_id)
    except WaveError:
        return "commit_failed", "true"
    return "done", "true"


def record_payload(*, archive: Path, tool: str, model: str, run: WorkerRun, usage: dict, handoff: str,
                   requested: int, used: int, degraded: list[str], note: str,
                   reasoning_effort: str | None = None, size: str | None = None,
                   risk: str | None = None) -> dict:
    """The `payload` of the worker's `metric` record. The engine only builds it: recording is the
    controller's job (`mcp__nucleos__record`), from the `record` field of `exec`'s answer."""
    payload = {"prompt_file": str(archive), "tool": tool, "model": model,
               "exit_code": run.exit_code, "duration_ms": run.duration_ms,
               "handoff_complete": {"true": True, "false": False}.get(handoff),
               "workers_requested": requested, "workers_used": used, "degraded": list(degraded),
               "note": note}
    for name, value in (("reasoning_effort", reasoning_effort), ("size", size), ("risk", risk),
                        ("tokens_in", usage["tokens_in"]), ("tokens_out", usage["tokens_out"]),
                        ("cache_read", usage["cache_read"]),
                        ("cache_creation", usage["cache_creation"])):
        if value is not None:
            payload[name] = value
    return payload


def exec_unit(*, root: Path, wave_id: int, unit_id: str, archive: Path, tool: str, model: str,
              argv: list[str], lease, reasoning_effort: str | None = None, size: str | None = None,
              risk: str | None = None,
              evidence: Callable[[Path], int] = run_evidence_gate) -> dict:
    """Run one opened unit's worker and settle the unit. The worker's `metric` record payload is
    built here, when it returns, with the lease as it stands at that moment, and handed back in
    the answer's `record` field for the controller to write. Any engine error around the worker
    settles the unit as `errored`, and the payload is built even when the wave was closed
    meanwhile."""
    path = state_path(root, wave_id)
    with StateLock(path):
        state = read_state(path)
    if state is None:
        raise WaveError(f"no wave {wave_id} in {root}")
    entry = state["units"].get(unit_id)
    if entry is None or entry["status"] != "running":
        raise WaveError(f"unit {unit_id} of wave {wave_id} was not opened by `next`")
    worktree = Path(entry["worktree"])
    started = time.monotonic()
    run: WorkerRun | None = None
    usage = usage_from_output("", "")  # every field None until the worker reports
    try:
        run = run_worker(argv, cwd=worktree, archive=Path(archive),
                         timeout=state["config"]["worker_timeout_seconds"],
                         out_dir=Path(entry["files_dir"]) if entry.get("files_dir") else None)
        usage = usage_from_output(run.stdout, run.stderr)
        outcome, handoff = finish_unit(worktree=worktree, base=state["base"],
                                       owned=entry["owned_paths"], packet=Path(entry["packet"]),
                                       wave_id=wave_id, unit_id=unit_id, run=run, evidence=evidence)
    except Exception as error:  # noqa: BLE001 -- a unit is never left `running` by its own exec
        print(f"wave_engine: unit {unit_id} errored: {error!r}", file=sys.stderr)
        outcome, handoff = ERRORED, "null"
        if run is None:
            run = WorkerRun(EXIT_ENGINE_ERROR, False, int((time.monotonic() - started) * 1000),
                            "", repr(error))
    tokens = (None if usage["tokens_in"] is None and usage["tokens_out"] is None
              else (usage["tokens_in"] or 0) + (usage["tokens_out"] or 0))

    def record(current: dict) -> None:
        current["units"][unit_id].update(
            status="done" if outcome == "done" else "failed", outcome=outcome,
            exit_code=run.exit_code, duration_ms=run.duration_ms, tokens=tokens,
            cost_usd=usage["cost_usd"])

    note = f"wave {wave_id} unit {unit_id}: {outcome}"
    try:
        state, _ = mutate_state(path, record)
    except WaveError:  # the wave was closed while the worker ran: the row is still owed
        note += "; the wave was closed before the unit was recorded"
    code, body = lease.status(wave_id)
    degraded = list(state.get("degraded") or [])
    if code != 0:
        degraded += lease_reasons(body)
        if body.get("unguarded_since"):
            note += f"; lease lapsed at {body['unguarded_since']} with workers running"
        else:
            note += f"; lease {body.get('status', 'unknown')} with workers running"
    payload = record_payload(archive=Path(archive), tool=tool, model=model, run=run, usage=usage,
                             handoff=handoff, requested=state["requested"],
                             used=max(1, state.get("workers_used") or 0), degraded=dedupe(degraded),
                             note=note, reasoning_effort=reasoning_effort, size=size, risk=risk)
    return {"wave_id": wave_id, "unit": unit_id, "outcome": outcome, "exit_code": run.exit_code,
            "duration_ms": run.duration_ms, "tokens": tokens, "cost_usd": usage["cost_usd"],
            "record": {"kind": "metric", "phase": "green", "payload": payload}}


# --- consolidate and land -------------------------------------------------------------------------

def is_drained(state: dict) -> bool:
    """Nothing runs, and nothing more will be opened."""
    statuses = [entry["status"] for entry in state["units"].values()]
    if "running" in statuses:
        return False
    return "pending" not in statuses or bool(state.get("stop_reasons")) or bool(state.get("aborted"))


def land(root: Path, integration: Path, base: str, head: str) -> list[str]:
    """Write the merged work into the controller's tree, uncommitted, as a sequential GREEN
    would leave it. Each file goes through `cat-file --filters`, so the checkout's own
    end-of-line conversion applies. The real index is never touched."""
    parts = git(integration, "diff", "--name-status", "--no-renames", "-z", base, head).split("\0")
    if parts and parts[-1] == "":
        parts.pop()
    changes = list(zip(parts[0::2], parts[1::2]))
    for status, relative in changes:
        target = root / relative
        if status == "D":
            target.unlink(missing_ok=True)
            continue
        blob = _git(root, ["cat-file", "--filters", f"{head}:{relative}"])
        if blob.returncode != 0:
            raise WaveError(f"cannot read {relative} from {head}: "
                            f"{blob.stderr.decode('utf-8', 'replace').strip()}")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(blob.stdout)
    return sorted(relative for _, relative in changes)


def _settle(path: Path, *, landed: list[str], pending: list[str], note: str | None,
            abort: bool = False) -> dict:
    def record(state: dict) -> None:
        state["landed"], state["pending"] = landed, pending
        if abort:
            state["aborted"] = True
            state["stop_reasons"] = dedupe([*(state.get("stop_reasons") or []), LANE])
        if note:
            state["note"] = "; ".join(part for part in (state.get("note"), note) if part)

    state, _ = mutate_state(path, record)
    return {"wave_id": state["wave_id"], "landed": landed, "pending": pending,
            "aborted": bool(state.get("aborted")), "note": state.get("note")}


def consolidate(*, root: Path, wave_id: int) -> dict:
    """Merge the finished units one at a time, in unit_id order, in an `integration-wave-<id>`
    worktree, then land the result in the
    controller's tree -- only if that tree is still exactly the base."""
    path = state_path(root, wave_id)
    with StateLock(path):
        state = read_state(path)
    if state is None:
        raise WaveError(f"no wave {wave_id} in {root}")
    if not is_drained(state):
        raise WaveError(f"wave {wave_id} is not drained: call `next` until it answers `drained`")
    units = sorted(state["units"])
    if state.get("aborted"):
        return _settle(path, landed=[], pending=units,
                       note="aborted: too many units left their lanes; nothing merged")
    done = sorted(uid for uid in units if state["units"][uid]["outcome"] == "done")
    rest = [uid for uid in units if uid not in done]
    if not done:
        return _settle(path, landed=[], pending=units, note=None)
    integration = Path(state["scratch"]) / integration_name(wave_id)
    mutate_state(path, lambda current: current.update(integration=str(integration)))
    git(root, "worktree", "add", "--detach", str(integration), state["base"])
    for unit_id in done:
        merged = _git(integration, ["merge", "--no-edit", unit_branch(wave_id, unit_id)])
        if merged.returncode != 0:
            _git(integration, ["merge", "--abort"])
            why = (merged.stderr or merged.stdout).decode("utf-8", "replace").strip()[:300]
            return _settle(path, landed=[], pending=units, abort=True,
                           note=f"merging unit {unit_id} failed, nothing merged: {why}")
    head = git(integration, "rev-parse", "HEAD").strip()
    if snapshot_tree(root) != state["base_tree"]:
        patch = wave_home(root) / f"{wave_id}.patch"
        patch.parent.mkdir(parents=True, exist_ok=True)
        patch.write_bytes(_git(integration, ["diff", "--binary", state["base"], head]).stdout)
        return _settle(path, landed=[], pending=units,
                       note=f"land_refused: the controller's tree changed during the wave; "
                            f"the merged work is kept at {patch}")
    land(root, integration, state["base"], head)
    return _settle(path, landed=done, pending=rest, note=None)


# --- CLI ------------------------------------------------------------------------------------------

def read_frozen(path: Path) -> list[str]:
    """The frozen RED test paths, one per line; blank lines and `#` comments skipped."""
    return [line.strip() for line in path.read_text(encoding="utf-8").splitlines()
            if line.strip() and not line.lstrip().startswith("#")]


def main(argv: list[str] | None = None) -> int:
    given = list(sys.argv[1:] if argv is None else argv)
    worker: list[str] = []
    if "--" in given:
        cut = given.index("--")
        given, worker = given[:cut], given[cut + 1:]
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    verbs = parser.add_subparsers(dest="verb", required=True)
    preparing = verbs.add_parser("prepare")
    preparing.add_argument("--cwd", default=".")
    preparing.add_argument("--plan", type=Path, required=True, help="the persisted plan with the manifest")
    preparing.add_argument("--packets-dir", type=Path, required=True,
                           help="one filled GREEN packet per unit, named <packet_id>.md")
    preparing.add_argument("--workers", type=int, required=True,
                           help="the speed decision's workers (resolve_speed.py)")
    preparing.add_argument("--frozen", type=Path, required=True, help="the RED test paths, one per line")
    preparing.add_argument("--risk-glob", action="append", default=[],
                           help="a `Risk matches` boundary glob; repeatable")
    for name in ("next", "consolidate", "close"):
        verb = verbs.add_parser(name)
        verb.add_argument("--wave-id", type=int, required=True)
        verb.add_argument("--cwd", default=".")
    running = verbs.add_parser("exec", help="... -- <the worker's command>")
    running.add_argument("--wave-id", type=int, required=True)
    running.add_argument("--unit", required=True)
    running.add_argument("--archive", type=Path, required=True, help="the unit's assembled prompt")
    running.add_argument("--tool", required=True)
    running.add_argument("--model", required=True)
    running.add_argument("--reasoning-effort")
    running.add_argument("--size")
    running.add_argument("--risk")
    running.add_argument("--cwd", default=".")
    args = parser.parse_args(given)
    if args.verb == "exec" and not worker:
        parser.error("exec needs the worker's command after `--`")
    lease = Lease()
    if args.verb == "close":
        try:
            answer = close(root=toplevel(Path(args.cwd)), wave_id=args.wave_id, lease=lease)
        except Exception as error:  # noqa: BLE001 -- close always answers, and always exits 0
            answer = {"wave_id": args.wave_id, "status": "close_failed", "error": str(error)}
        print(json.dumps(answer))
        return 0
    try:
        if args.verb == "prepare":
            answer = prepare(cwd=Path(args.cwd).resolve(), plan=args.plan, packets_dir=args.packets_dir,
                             workers=args.workers, frozen=read_frozen(args.frozen),
                             risk_globs=args.risk_glob, lease=lease)
        else:
            root = toplevel(Path(args.cwd))
            if args.verb == "next":
                answer = next_step(root=root, wave_id=args.wave_id, lease=lease)
            elif args.verb == "consolidate":
                answer = consolidate(root=root, wave_id=args.wave_id)
            else:
                answer = exec_unit(root=root, wave_id=args.wave_id, unit_id=args.unit,
                                   archive=args.archive.resolve(), tool=args.tool, model=args.model,
                                   argv=worker, lease=lease, reasoning_effort=args.reasoning_effort,
                                   size=args.size, risk=args.risk)
    except (WaveError, OSError, nucleos_paths.ProjectNotFound) as error:
        print(json.dumps({"verb": args.verb, "error": str(error)}))
        return 2
    print(json.dumps(answer))
    return 0


if __name__ == "__main__":
    sys.exit(main())
