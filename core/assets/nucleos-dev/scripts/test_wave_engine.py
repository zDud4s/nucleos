#!/usr/bin/env python3
"""Tests for `wave_engine.py` -- the GREEN wave: manifest, unit trees, ceilings, landing, closing.

Run:  python test_wave_engine.py

Every git operation runs in throwaway repositories under the system temp directory. The lease,
the seeder and the worker are fakes: the lease is `FakeLease`, the seeder records what it was
asked, and the worker is this very script run as `--fake-worker`. Nothing here talks to the
daemon or touches the checkout it runs from.
"""
from __future__ import annotations

import contextlib
import copy
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import asdict
from pathlib import Path

import yaml

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

import evidence_gate  # noqa: E402
import wave_engine as we  # noqa: E402

FAILURES: list[str] = []
TESTS: list = []


def case(test):
    """Register a test; `main` runs them in the order they are defined."""
    TESTS.append(test)
    return test


def check(name: str, condition: bool, detail: str = "") -> None:
    if condition:
        print(f"  ok   {name}")
    else:
        print(f"  FAIL {name}: {detail}")
        FAILURES.append(f"{name}: {detail}")


def safely(call, *args, **kwargs):
    """The call's result, or the exception it raised -- so a mutant that crashes fails its
    named check instead of killing the whole script."""
    try:
        return call(*args, **kwargs)
    except Exception as error:  # noqa: BLE001
        return error


def _make_writable(func, target, *_):
    os.chmod(target, stat.S_IWRITE)
    func(target)


@contextlib.contextmanager
def scratch():
    """A temp directory, resolved to its long name (git never prints the 8.3 one), removed
    afterwards together with the read-only files git leaves in `.git/objects`."""
    path = Path(tempfile.mkdtemp(prefix="wave-engine-")).resolve()
    try:
        write_project(path, path / "_state")
        yield path
    finally:
        # A few tries: a worker interpreter orphaned by a killed launcher (Task 5's timeout case)
        # can hold a directory for a few seconds, and Windows refuses to remove it until then.
        for attempt in range(10):
            try:
                if sys.version_info >= (3, 12):
                    shutil.rmtree(path, onexc=_make_writable)
                else:
                    shutil.rmtree(path, onerror=_make_writable)
                break
            except OSError as error:
                if attempt == 9:
                    print(f"  note: scratch {path} left behind: {error}")
                time.sleep(1)


def write_project(root: Path, state: Path) -> Path:
    """`<root>/.nucleos/workflow/project.json` pointing at `state`; the state dir is created."""
    (root / ".nucleos" / "workflow").mkdir(parents=True, exist_ok=True)
    state.mkdir(parents=True, exist_ok=True)
    (root / ".nucleos" / "workflow" / "project.json").write_text(
        json.dumps({"id": "t", "state_dir": str(state), "bundle_dir": str(SCRIPTS.parent)}), encoding="utf-8")
    return state


def models_yaml(drop: tuple[str, ...] = (), **overrides) -> str:
    block = {"enabled": True, "green_enabled": True, "execute_enabled": False, "max_workers": 4,
             "max_units": 4, "worker_timeout_seconds": 60, "max_wave_seconds": 2400,
             "max_total_worker_tokens": 60000, "max_total_cost_usd": 2.0,
             "correction_timeout_seconds": 1800, "lane_violation_abort_ratio": 0.5,
             "scratch_root": "wt"}
    block.update(overrides)
    for key in drop:
        block.pop(key)
    return yaml.safe_dump({"parallel_execute": block})


def packet_text(commands: tuple[str, ...] = ("python -c \"print('unit ok')\"",)) -> str:
    """A filled GREEN execute packet in the shape of `packets/execute.md`, Handoff empty."""
    bullets = "\n".join(f"- `{command}`" for command in commands)
    return (
        "# Execution Packet\n\nTask ID: wave-test\nObjective: make the unit's tests pass\nSize: small\n\n"
        "## Steps\n1. Write the owned files.\n\n"
        "## Boundaries\nAllowed files: the unit's owned paths\n\n"
        f"## Validation\nCommands:\n{bullets}\nExpected result: exit 0\n\n"
        "## Handoff\n\nFiles changed:\nTests added:\nValidation evidence:\n"
        "Deviations from plan: none\nNew risks discovered: none\nMemory updates: none\n"
        "Pending deletions: none\n\n## Follow-ups\n"
    )


def manifest_unit(unit_id: str, owned_paths: list[str], **overrides) -> dict:
    unit = {"unit_id": unit_id, "objective": f"make unit {unit_id} pass", "packet_id": f"p-{unit_id}",
            "owned_paths": list(owned_paths), "depends_on": [],
            "acceptance_criteria": ["its tests pass"], "test_refs": [f"tests/test_{unit_id}.py"]}
    unit.update(overrides)
    return unit


def plan_text(units: list[dict]) -> str:
    manifest = {"schema_version": "parallel-execute/v1", "strategy": "single-wave",
                "reason": "disjoint units", "units": units}
    return ("# Plan\n\nSome prose.\n\n## Parallel execute manifest\n\n```json\n"
            + json.dumps(manifest, indent=2) + "\n```\n\n## Execute packets\n")


def write_task(where: Path, units: list[dict],
               commands: tuple[str, ...] | None = None) -> tuple[Path, Path]:
    """A persisted plan carrying the manifest, and a directory of one packet per unit."""
    packets = where / "packets"
    packets.mkdir(parents=True, exist_ok=True)
    for unit in units:
        text = packet_text(commands) if commands else packet_text()
        (packets / f"{unit['packet_id']}.md").write_text(text, encoding="utf-8")
    plan = where / "plan.md"
    plan.write_text(plan_text(units), encoding="utf-8")
    return plan, packets


@case
def test_config() -> None:
    with scratch() as tmp:
        path = tmp / "models.yaml"
        # Mutation M1: read `max_wave_seconds` from the `worker_timeout_seconds` key.
        path.write_text(models_yaml(worker_timeout_seconds=61, max_wave_seconds=2401), encoding="utf-8")
        read = safely(we.read_engine_config, path)
        check("config_reads_every_ceiling",
              isinstance(read, we.EngineConfig)
              and (read.enabled, read.green_enabled, read.max_units, read.worker_timeout_seconds,
                   read.max_wave_seconds, read.max_total_worker_tokens, read.max_total_cost_usd,
                   read.lane_violation_abort_ratio, read.scratch_root)
              == (True, True, 4, 61, 2401, 60000, 2.0, 0.5, "wt"),
              repr(read))

        # Mutation M2: drop the bool check on `enabled`/`green_enabled` in read_engine_config.
        refused = {}
        for label, body in (("enabled_not_bool", models_yaml(enabled="yes")),
                            ("zero_units", models_yaml(max_units=0)),
                            ("ratio_above_one", models_yaml(lane_violation_abort_ratio=1.5)),
                            ("absolute_scratch", models_yaml(scratch_root="C:/wt")),
                            ("escaping_scratch", models_yaml(scratch_root="../wt")),
                            ("missing_key", models_yaml(drop=("max_total_cost_usd",))),
                            ("no_block", "nothing: here\n")):
            path.write_text(body, encoding="utf-8")
            refused[label] = isinstance(safely(we.read_engine_config, path), we.ConfigError)
        check("a_malformed_parallel_execute_is_a_config_error", all(refused.values()), repr(refused))

    # With no project.json the default location is a clear ProjectNotFound, not a crash.
    with scratch() as empty:
        (empty / ".nucleos" / "workflow" / "project.json").unlink()
        gone = safely(we.nucleos_paths.state_dir, empty)
        check("a_missing_project_json_is_named", isinstance(gone, we.nucleos_paths.ProjectNotFound)
              and "project.json" in str(gone), repr(gone))


@case
def test_manifest_parsing() -> None:
    units = [manifest_unit("a", ["src/a.py"]), manifest_unit("b", ["src/b.py"])]
    parsed = safely(we.parse_manifest, plan_text(units))
    check("the_manifest_is_the_json_under_its_header",
          isinstance(parsed, dict) and [u["unit_id"] for u in parsed["units"]] == ["a", "b"],
          repr(parsed))
    broken = [safely(we.parse_manifest, text) for text in (
        "# Plan\n\nno manifest here\n",
        "## Parallel execute manifest\n\nprose only\n\n## Next\n```json\n{}\n```\n",
        "## Parallel execute manifest\n```json\n{\"units\": [\n```\n",
        "## Parallel execute manifest\n```json\n[1, 2]\n```\n",
        "## Parallel execute manifest\n```json\n{\"units\": []}\n",
    )]
    check("a_missing_or_broken_manifest_is_refused",
          all(isinstance(answer, we.ManifestError) for answer in broken), repr(broken))


@case
def test_manifest_validation() -> None:
    with scratch() as tmp:
        base_units = [manifest_unit("a", ["src/a.py"]), manifest_unit("b", ["src/b.py", "docs/b.md"])]
        _, packets = write_task(tmp, base_units)
        frozen = ["tests/test_a.py", "tests/test_b.py"]

        def validate(mutate, packets_dir: Path = packets, max_units: int = 4):
            manifest = we.parse_manifest(plan_text(copy.deepcopy(base_units)))
            mutate(manifest)
            return we.validate_manifest(manifest, packets_dir=packets_dir, frozen=frozen,
                                        risk_globs=["core/src/auth*"], max_units=max_units)

        def set_unit(index: int, **values):
            return lambda manifest: manifest["units"][index].update(values)

        def refused(name: str, mutate, needle: str, **kwargs) -> None:
            answer = safely(validate, mutate, **kwargs)
            ok = isinstance(answer, tuple) and answer[0] == [] and any(needle in p for p in answer[1])
            check(name, ok, repr(answer))

        units, problems = validate(lambda manifest: None)
        check("a_good_manifest_has_no_problems",
              not problems and [u.unit_id for u in units] == ["a", "b"]
              and units[1].owned_paths == ("src/b.py", "docs/b.md"), repr(problems))

        # Mutation M3: drop `.lower()` from both uses of the owner map in validate_manifest.
        refused("refuses_an_owned_path_shared_ignoring_case",
                set_unit(1, owned_paths=["SRC/A.py"]), "also owned by")
        # Mutation M4: accept any `depends_on` list.
        refused("refuses_depends_on", set_unit(1, depends_on=["a"]), "depends_on")
        # Mutation M5: drop the lockfile arm of forbidden_kind.
        refused("refuses_a_lockfile", set_unit(0, owned_paths=["Cargo.lock"]), "lockfile")
        # Mutation M6: drop the `__init__.py` arm of forbidden_kind.
        refused("refuses_a_package_init", set_unit(0, owned_paths=["pkg/__init__.py"]), "__init__")
        # Mutation M7: drop the frozen arm of forbidden_kind.
        refused("refuses_a_frozen_red_test", set_unit(0, owned_paths=["tests/TEST_A.py"]), "frozen")
        # Mutation M8: drop the risk-glob loop of forbidden_kind.
        refused("refuses_a_path_inside_a_risk_boundary",
                set_unit(0, owned_paths=["core/src/auth.rs"]), "risk boundary")
        refused("refuses_configuration", set_unit(0, owned_paths=["shell/package.json"]), "configuration")
        refused("refuses_workflow_files", set_unit(0, owned_paths=[".nucleos/workflow/x.py"]), "configuration")
        refused("refuses_a_path_listed_twice", set_unit(0, owned_paths=["src/a.py", "src/a.py"]),
                "listed twice")
        shapes = {}
        for raw in ("/abs/a.py", "C:/abs/a.py", "src\\a.py", "src/../a.py", "src/*.py", "src/",
                    "./a.py", "", 7):
            answer = safely(validate, set_unit(0, owned_paths=[raw]))
            shapes[repr(raw)] = isinstance(answer, tuple) and answer[0] == [] and bool(answer[1])
        check("refuses_a_path_that_is_not_a_normalized_file", all(shapes.values()), repr(shapes))
        refused("refuses_a_single_unit", lambda manifest: manifest["units"].pop(), "a wave takes 2..")
        refused("refuses_more_units_than_max_units",
                lambda manifest: manifest["units"].append(manifest_unit("c", ["src/c.py"])),
                "3 units; a wave takes 2..2", max_units=2)
        refused("refuses_a_duplicate_unit_id_ignoring_case", set_unit(1, unit_id="A"),
                "duplicate unit_id")
        refused("refuses_a_unit_id_git_cannot_name", set_unit(1, unit_id="x.lock"), "unit_id must")
        refused("refuses_a_missing_packet", set_unit(1, packet_id="p-zzz"), "no packet")
        refused("refuses_a_wrong_schema_version",
                lambda manifest: manifest.update(schema_version="v0"), "schema_version")

        controller_only = tmp / "catalog-gate"
        suite = tmp / "full-suite"
        unfilled = tmp / "unfilled"
        for directory, text in (
                (controller_only, packet_text(("python -m pytest tests/test_a.py",
                                               "python select_tests.py --gate --run"))),
                (suite, packet_text(("bash scripts/gates.sh all",))),
                (unfilled, packet_text().replace("- `python -c \"print('unit ok')\"`",
                                                  "<!-- exact commands -->"))):
            directory.mkdir()
            for unit in base_units:
                (directory / f"{unit['packet_id']}.md").write_text(text, encoding="utf-8")
        # Mutation M9: drop `select_tests.py` from CONTROLLER_ONLY_COMMANDS.
        refused("refuses_a_unit_that_runs_the_catalog_gate", lambda manifest: None,
                "only the controller", packets_dir=controller_only)
        # Mutation M9b: drop `gates.sh` from CONTROLLER_ONLY_COMMANDS.
        refused("refuses_a_unit_that_runs_the_full_suite", lambda manifest: None,
                "only the controller", packets_dir=suite)
        refused("refuses_a_packet_whose_validation_is_unwritten", lambda manifest: None,
                "packet p-a.md", packets_dir=unfilled)


LAPSED = (3, {"status": "lapsed", "degraded": ["slots_exhausted"],
              "unguarded_since": "2026-10-01T10:00:00Z"})
HELD = (0, {"status": "held", "degraded": [], "unguarded_since": None})


def git_out(cwd: Path, *args: str) -> str:
    proc = subprocess.run(["git", *args], cwd=str(cwd), capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} exited {proc.returncode}: {proc.stderr.strip()}")
    return proc.stdout


def make_repo(tmp: Path, hooks: Path | None = None) -> Path:
    """A repository with one commit. Its hooks are an empty directory unless `hooks` names the
    real ones: the developer's own `core.hooksPath` must never leak into a test repository."""
    repo = tmp / "repo"
    repo.mkdir()
    git_out(repo, "init", "-q")
    for key, value in (("user.name", "Wave Test"), ("user.email", "wave@test.invalid"),
                       ("commit.gpgsign", "false")):
        git_out(repo, "config", key, value)
    no_hooks = tmp / "no-hooks"
    no_hooks.mkdir(exist_ok=True)
    git_out(repo, "config", "core.hooksPath", str(no_hooks))
    (repo / ".gitignore").write_text(".nucleos/\n", encoding="utf-8")
    write_project(repo, tmp / "_state")
    (repo / "shared.txt").write_text("base\n", encoding="utf-8")
    git_out(repo, "add", "-A")
    git_out(repo, "commit", "-q", "-m", "init")
    if hooks is not None:
        git_out(repo, "config", "core.hooksPath", str(hooks))
    return repo


def state_of(repo: Path) -> Path:
    return we.nucleos_paths.state_dir(repo)


def write_models(repo: Path, **overrides) -> None:
    (state_of(repo) / "models.yaml").write_text(models_yaml(**overrides), encoding="utf-8")


class FakeLease:
    """`wave_lease.py`'s three verbs, answered from fields the test sets; every call is kept."""

    def __init__(self, workers: int = 2, wave_id: int | None = 7, opening: dict | None = None):
        self.opening = opening or {"workers": workers, "requested": workers, "granted": workers,
                                   "wave_id": wave_id, "lease_seconds": 120, "degraded": [],
                                   "reason": None}
        self.status_answer = HELD
        self.calls: list[tuple] = []

    def open(self, cwd: str, units: int, workers: int) -> dict:
        self.calls.append(("open", units, workers))
        return dict(self.opening)

    def status(self, wave_id: int) -> tuple[int, dict]:
        self.calls.append(("status", wave_id))
        code, body = self.status_answer
        return code, {"wave_id": wave_id, **body}

    def close(self, wave_id: int, cwd: str) -> dict:
        self.calls.append(("close", wave_id))
        body = self.status_answer[1]
        return {"wave_id": wave_id, "released": True, "ended_as": body.get("status", "held"),
                "unguarded_since": body.get("unguarded_since"),
                "degraded": list(body.get("degraded", []))}


class FakeSeeder:
    """Records (main, tree name, whether the tree is already a linked worktree) per call."""

    def __init__(self, fail_on: str | None = None):
        self.calls: list[tuple] = []
        self.fail_on = fail_on

    def __call__(self, main: Path, worktree: Path) -> None:
        worktree = Path(worktree)
        self.calls.append((Path(main), worktree.name, (worktree / ".git").is_file()))
        if worktree.name == self.fail_on:
            raise we.WaveError(f"seeding {worktree.name} failed")


def worktrees(repo: Path) -> list[str]:
    return [line for line in git_out(repo, "worktree", "list", "--porcelain").splitlines()
            if line.startswith("worktree ")]


def wave_branches(repo: Path) -> list[str]:
    return git_out(repo, "for-each-ref", "--format=%(refname:short)", "refs/heads/wave/").split()


def wave_files(repo: Path) -> list[Path]:
    directory = we.state_dir(repo)
    return sorted(directory.glob("*.json")) if directory.is_dir() else []


def prepared(tmp: Path, units=(("b", ["b.txt"]), ("a", ["a.txt"])), *, workers: int = 2,
             lease: FakeLease | None = None, seeder: FakeSeeder | None = None,
             hooks: Path | None = None, **models):
    """A repository whose controller tree holds an uncommitted RED test and an uncommitted edit,
    and a wave prepared over it. The manifest lists `b` before `a` on purpose."""
    repo = make_repo(tmp, hooks)
    write_models(repo, **models)
    (repo / "tests").mkdir()
    (repo / "tests" / "test_red.py").write_text("assert False  # RED\n", encoding="utf-8")
    (repo / "shared.txt").write_text("changed\n", encoding="utf-8")
    plan, packets = write_task(tmp / "task", [manifest_unit(uid, paths) for uid, paths in units])
    lease = lease or FakeLease(workers=workers)
    seeder = seeder or FakeSeeder()
    answer = we.prepare(cwd=repo, plan=plan, packets_dir=packets, workers=workers,
                        frozen=["tests/test_red.py"], risk_globs=[], lease=lease, seeder=seeder)
    return repo, lease, seeder, answer


STUB_LEASE = '''import json, sys
from pathlib import Path
with Path(__file__).with_suffix(".argv").open("a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
verb = sys.argv[1]
if verb == "open":
    print(json.dumps({"workers": 2, "requested": 2, "granted": 2, "wave_id": 5,
                      "lease_seconds": 120, "degraded": [], "reason": None, "state_file": "x"}))
elif verb == "status":
    print(json.dumps({"wave_id": 5, "status": "lapsed", "degraded": ["slots_exhausted"],
                      "unguarded_since": "t0"}))
    sys.exit(3)
else:
    print(json.dumps({"wave_id": 5, "released": True, "ended_as": "lapsed",
                      "unguarded_since": "t0", "degraded": ["slots_exhausted"]}))
'''
BROKEN_LEASE = 'import sys\nprint("Traceback: boom")\nsys.exit(1)\n'


@case
def test_lease_adapter() -> None:
    with scratch() as tmp:
        good, broken = tmp / "good_lease.py", tmp / "broken_lease.py"
        good.write_text(STUB_LEASE, encoding="utf-8")
        broken.write_text(BROKEN_LEASE, encoding="utf-8")
        lease = we.Lease(script=good)
        opening = lease.open("C:/x", 2, 3)
        argv = [json.loads(line) for line in good.with_suffix(".argv").read_text().splitlines()]
        check("the_lease_is_asked_through_its_cli",
              opening.get("wave_id") == 5
              and argv[0] == ["open", "--cwd", "C:/x", "--units", "2", "--workers", "3"],
              repr((opening, argv)))
        check("a_lease_stop_is_exit_3", lease.status(5)[0] == 3)
        dead = we.Lease(script=broken)
        answers = (dead.open("C:/x", 2, 2), dead.status(5), dead.close(5, "C:/x"))
        # Mutation M10: in Lease.status, return any exit code as it came instead of folding it into 3.
        check("an_unreadable_lease_answer_never_grants",
              answers[0]["workers"] == 1 and answers[0]["wave_id"] is None
              and bool(answers[0].get("lease_error")) and answers[1][0] == 3
              and answers[2]["released"] is False, repr(answers))


@case
def test_prepare_declines() -> None:
    with scratch() as tmp:
        repo = make_repo(tmp)
        plan, packets = write_task(tmp / "task", [manifest_unit("a", ["a.txt"]),
                                                  manifest_unit("b", ["b.txt"])])

        def ask(lease: FakeLease, plan_file: Path = plan) -> dict:
            return we.prepare(cwd=repo, plan=plan_file, packets_dir=packets, workers=2, frozen=[],
                              risk_globs=[], lease=lease, seeder=FakeSeeder())

        off = {}
        for label, models in (("enabled", {"enabled": False}), ("green_enabled", {"green_enabled": False})):
            write_models(repo, **models)
            lease = FakeLease()
            answer = ask(lease)
            off[label] = (answer["mode"], answer["degraded"], lease.calls, answer["metrics_args"])
        # Mutation M16: drop `config.green_enabled` from the gate in prepare.
        check("a_wave_needs_enabled_and_green_enabled",
              all(value == ("sequential", ["wave_disabled"], [],
                            {"workers_requested": 2, "workers_used": 1, "degraded": ["wave_disabled"]})
                  for value in off.values()), repr(off))

        (state_of(repo) / "models.yaml").write_text("parallel_execute: {enabled: maybe}\n", encoding="utf-8")
        lease = FakeLease()
        answer = ask(lease)
        check("an_unreadable_parallel_execute_is_a_config_error_not_a_reason",
              answer["mode"] == "sequential" and answer["degraded"] == [] and bool(answer["config_error"])
              and "config_error" in (answer["note"] or "") and lease.calls == [], repr(answer))

        write_models(repo)
        bad_plan = tmp / "task" / "overlapping.md"
        bad_plan.write_text(plan_text([manifest_unit("a", ["a.txt"]), manifest_unit("b", ["A.txt"])]),
                            encoding="utf-8")
        lease = FakeLease()
        answer = ask(lease, bad_plan)
        check("an_invalid_manifest_is_not_decomposable_and_asks_no_slot",
              answer["mode"] == "sequential" and answer["degraded"] == ["task_not_decomposable"]
              and "also owned by" in (answer["note"] or "") and lease.calls == [], repr(answer))

        none_granted = FakeLease(opening={"workers": 1, "requested": 2, "granted": 1, "wave_id": None,
                                          "lease_seconds": None, "degraded": ["slots_exhausted"],
                                          "reason": None})
        first = ask(none_granted)
        one_granted = FakeLease(opening={"workers": 1, "requested": 2, "granted": 1, "wave_id": 9,
                                         "lease_seconds": 120, "degraded": [], "reason": None})
        second = ask(one_granted)
        # Mutation M17: reduce prepare's test to `if wave_id is None:`.
        check("a_one_worker_answer_creates_nothing_and_gives_any_slot_back",
              first["mode"] == second["mode"] == "sequential"
              and first["degraded"] == ["slots_exhausted"] and ("close", 9) in one_granted.calls
              and len(worktrees(repo)) == 1 and not wave_branches(repo) and not wave_files(repo),
              repr((first, second, worktrees(repo))))


@case
def test_prepare_opens_a_wave() -> None:
    with scratch() as tmp:
        repo, lease, seeder, answer = prepared(tmp)
        check("prepare_opens_a_wave_of_the_granted_workers",
              answer.get("mode") == "wave" and answer.get("wave_id") == 7
              and answer.get("units") == ["a", "b"] and answer.get("workers") == 2
              and lease.calls[0] == ("open", 2, 2), repr(answer))
        state = we.read_state(we.state_path(repo, 7))
        tree = {uid: Path(state["units"][uid]["worktree"]) for uid in ("a", "b")}

        # Mutation M11: make snapshot_base return `git rev-parse HEAD` as the base.
        check("the_base_carries_the_uncommitted_red_tests",
              (tree["a"] / "tests" / "test_red.py").is_file()
              and (tree["a"] / "shared.txt").read_text(encoding="utf-8").strip() == "changed",
              repr(sorted(p.name for p in tree["a"].iterdir())))
        status = set(git_out(repo, "status", "--porcelain", "--untracked-files=all").splitlines())
        check("the_real_index_and_tree_are_untouched",
              status == {" M shared.txt", "?? tests/test_red.py"}
              and git_out(repo, "diff", "--cached", "--name-only") == "", repr(status))

        # Mutation M12: in create_unit_tree, call the seeder before `git worktree add`.
        check("each_tree_is_seeded_after_it_exists_in_unit_id_order",
              [(name, linked) for _, name, linked in seeder.calls] == [("a", True), ("b", True)]
              and all(main == repo for main, _, _ in seeder.calls), repr(seeder.calls))

        packet = Path(state["units"]["a"]["packet"])
        text = packet.read_text(encoding="utf-8")
        # Mutation M14: delete the `Co-Authored-By` line from WAVE_RULES.
        check("the_unit_packet_forbids_the_trailer",
              "## Wave unit rules" in text
              and any("Co-Authored-By" in line and "never" in line for line in text.splitlines()),
              text[-900:])
        local = Path(state["units"]["a"]["files_dir"])
        info = json.loads((local / "wave-unit.json").read_text(encoding="utf-8"))
        check("the_unit_files_sit_outside_the_tree_and_know_the_lanes",
              packet.parent == local and not (tree["a"] / ("." + "ai")).exists()
              and not (local / "active-packet").exists()
              and info == {"wave_id": 7, "unit_id": "a", "owned_paths": ["a.txt"]}
              and text.index("## Handoff") < text.index("## Wave unit rules"), repr((packet, info)))
        check("each_unit_has_its_branch", wave_branches(repo) == ["wave/7/a", "wave/7/b"],
              repr(wave_branches(repo)))
        we.close(root=repo, wave_id=7, lease=lease)


@case
def test_prepare_setup_failure() -> None:
    with scratch() as tmp:
        repo, lease, _, answer = prepared(tmp, seeder=FakeSeeder(fail_on="b"))
        # Mutation M15: drop the `close(...)` call from prepare's setup-failure branch.
        check("a_setup_failure_closes_the_wave",
              answer.get("mode") == "sequential" and "wave setup failed" in (answer.get("note") or "")
              and ("close", 7) in lease.calls and len(worktrees(repo)) == 1
              and not wave_branches(repo) and not wave_files(repo), repr((answer, worktrees(repo))))


def synthetic_state(statuses: dict[str, str], *, workers: int = 2, **config) -> dict:
    """A wave's state file without a repository behind it: what `decide` and `next_step` read."""
    ceilings = {"enabled": True, "green_enabled": True, "max_units": 4, "worker_timeout_seconds": 60,
                "max_wave_seconds": 100, "max_total_worker_tokens": 1000, "max_total_cost_usd": 1.0,
                "lane_violation_abort_ratio": 0.5, "scratch_root": "wt"}
    ceilings.update(config)
    units = {uid: {"status": status, "outcome": None, "tokens": None, "cost_usd": None,
                   "worktree": f"wt/{uid}", "packet": f"wt/{uid}/packet.md",
                   "started_at": None}
             for uid, status in statuses.items()}
    return {"wave_id": 3, "opened_at": 1000.0, "workers": workers, "requested": workers,
            "units": units, "stop_reasons": [], "aborted": False, "unguarded_since": None,
            "workers_used": 0, "degraded": [], "config": ceilings}


def finished(state: dict, uid: str, *, outcome: str = "done", tokens: int | None = None,
             cost: float | None = None) -> dict:
    state["units"][uid].update(status="done" if outcome == "done" else "failed", outcome=outcome,
                               tokens=tokens, cost_usd=cost)
    return state


@case
def test_decide() -> None:
    fresh = we.decide(synthetic_state({"a": "pending", "b": "pending", "c": "pending"}), 1010.0, *HELD)
    check("a_fresh_wave_opens_the_first_unit",
          fresh["action"] == "open" and fresh["unit"] == "a" and fresh["stop_reasons"] == [], repr(fresh))
    # Mutation M18: in decide, compare len(running) with len(units) instead of state["workers"].
    full = we.decide(synthetic_state({"a": "running", "b": "running", "c": "pending"}), 1010.0, *HELD)
    check("no_more_than_workers_units_run_at_once", full["action"] == "wait", repr(full))
    # Mutation M19: in decide, ignore lease_code.
    lapsed = we.decide(synthetic_state({"a": "running", "b": "pending"}), 1010.0, *LAPSED)
    check("a_lapsed_lease_opens_nothing_more",
          lapsed["action"] == "wait" and lapsed["stop_reasons"] == ["slots_exhausted"]
          and lapsed["unguarded_since"] == "2026-10-01T10:00:00Z", repr(lapsed))
    # Mutation M20: drop the elapsed-time arm of the budget test in decide.
    late = we.decide(synthetic_state({"a": "pending", "b": "pending"}), 1100.0, *HELD)
    check("the_wave_clock_is_a_ceiling",
          late["action"] == "drained" and late["stop_reasons"] == ["wave_budget_exhausted"], repr(late))
    # Mutation M21: drop the token arm of the budget test in decide.
    tokens = we.decide(finished(synthetic_state({"a": "pending", "b": "pending"}), "a", tokens=1000),
                       1010.0, *HELD)
    check("the_token_total_is_a_ceiling",
          tokens["action"] == "drained" and tokens["stop_reasons"] == ["wave_budget_exhausted"],
          repr(tokens))
    # Mutation M22: drop the cost arm of the budget test in decide.
    cost = we.decide(finished(synthetic_state({"a": "pending", "b": "pending"}), "a", cost=1.0),
                     1010.0, *HELD)
    check("the_metered_cost_is_a_ceiling",
          cost["action"] == "drained" and cost["stop_reasons"] == ["wave_budget_exhausted"], repr(cost))
    below = we.decide(finished(synthetic_state({"a": "pending", "b": "pending", "c": "pending"}), "a",
                               tokens=999, cost=0.99), 1099.0, *HELD)
    check("below_every_ceiling_the_wave_goes_on",
          below["action"] == "open" and below["unit"] == "b" and below["stop_reasons"] == [], repr(below))
    # Mutation M23: use `>` instead of `>=` for the lane-violation ratio in decide.
    lane = we.decide(finished(synthetic_state({"a": "pending", "b": "pending"}), "a",
                              outcome="lane_violation"), 1010.0, *HELD)
    check("half_the_units_out_of_lane_aborts_the_wave",
          lane["aborted"] and lane["action"] == "drained" and lane["stop_reasons"] == ["lane_violation"],
          repr(lane))
    sticky = synthetic_state({"a": "done", "b": "pending"})
    sticky["stop_reasons"] = ["slots_exhausted"]
    again = we.decide(sticky, 1010.0, *HELD)
    check("a_stop_is_never_undone",
          again["action"] == "drained" and again["stop_reasons"] == ["slots_exhausted"], repr(again))
    done = we.decide(finished(finished(synthetic_state({"a": "running", "b": "running"}), "a"), "b"),
                     1500.0, *LAPSED)
    check("a_finished_wave_is_drained_without_inventing_reasons",
          done["action"] == "drained" and done["stop_reasons"] == [], repr(done))
    stale = synthetic_state({"a": "running", "b": "pending"}, workers=1, max_wave_seconds=100000)
    stale["units"]["a"]["started_at"] = 1000.0
    # Mutation M23b: in decide, leave `expired` empty.
    gone = we.decide(stale, 1000.0 + 60 + 601, *HELD)
    check("a_unit_running_past_its_timeout_and_grace_is_expired_and_frees_its_slot",
          gone["expired"] == ["a"] and gone["running"] == [] and gone["action"] == "open"
          and gone["unit"] == "b", repr(gone))
    within = we.decide(stale, 1000.0 + 60 + 599, *HELD)
    check("a_unit_inside_its_grace_is_still_running",
          within["expired"] == [] and within["action"] == "wait", repr(within))


@case
def test_state_lock() -> None:
    with scratch() as tmp:
        path = tmp / "waves" / "3.json"
        we.write_state(path, {"count": 0})

        def bump(state: dict) -> None:
            value = state["count"]
            time.sleep(0.005)
            state["count"] = value + 1

        def worker() -> None:
            for _ in range(10):
                we.mutate_state(path, bump)

        threads = [threading.Thread(target=worker) for _ in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        final = we.read_state(path)
        # Mutation M24: open the lock file in StateLock without os.O_EXCL.
        check("concurrent_state_updates_are_never_lost",
              final == {"count": 40} and not we.lock_path(path).exists(), repr(final))


@case
def test_next_step() -> None:
    with scratch() as tmp:
        path = we.state_path(tmp, 3)
        we.write_state(path, synthetic_state({"a": "pending", "b": "pending", "c": "pending"}))
        lease = FakeLease(wave_id=3)

        def clock() -> float:
            return 1010.0

        steps = [we.next_step(root=tmp, wave_id=3, lease=lease, now=clock) for _ in range(3)]
        saved = we.read_state(path)
        check("next_opens_up_to_workers_then_waits",
              [step["action"] for step in steps] == ["open", "open", "wait"]
              and [step["unit"] for step in steps[:2]] == ["a", "b"]
              and saved["units"]["a"]["status"] == "running" and saved["units"]["a"]["started_at"] == 1010.0
              and saved["workers_used"] == 2, repr((steps, saved)))
        check("an_opened_unit_comes_with_its_tree_and_packet",
              steps[0].get("worktree") == "wt/a" and steps[0].get("packet") == "wt/a/packet.md", repr(steps[0]))
        lease.status_answer = LAPSED
        we.mutate_state(path, lambda state: finished(state, "a"))
        stopped = we.next_step(root=tmp, wave_id=3, lease=lease, now=clock)
        saved = we.read_state(path)
        check("next_records_the_lapse_and_opens_nothing",
              stopped["action"] == "wait" and saved["stop_reasons"] == ["slots_exhausted"]
              and saved["unguarded_since"] == "2026-10-01T10:00:00Z"
              and saved["units"]["c"]["status"] == "pending", repr(saved))

        stale_path = we.state_path(tmp, 4)
        stale = synthetic_state({"a": "running", "b": "pending"}, workers=1, max_wave_seconds=100000)
        stale["wave_id"] = 4
        stale["units"]["a"]["started_at"] = 1000.0
        we.write_state(stale_path, stale)
        reopened = we.next_step(root=tmp, wave_id=4, lease=FakeLease(wave_id=4), now=lambda: 1661.0)
        saved = we.read_state(stale_path)
        # Mutation M24b: in next_step, skip the loop that marks the decision's expired units.
        check("a_unit_an_exec_abandoned_is_recorded_errored",
              reopened["action"] == "open" and reopened["unit"] == "b"
              and saved["units"]["a"]["status"] == "failed" and saved["units"]["a"]["outcome"] == "errored",
              repr((reopened, saved["units"]["a"])))


WORKER = [sys.executable, str(Path(__file__).resolve()), "--fake-worker"]


@contextlib.contextmanager
def environ(**values):
    saved = {key: os.environ.get(key) for key in values}
    os.environ.update({key: str(value) for key, value in values.items()})
    try:
        yield
    finally:
        for key, value in saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


def fill_handoff(packet: Path) -> None:
    """Evidence for every Validation command, in the block shape `evidence_gate.py` reads."""
    lines = packet.read_text(encoding="utf-8").splitlines()
    commands = evidence_gate.validation_commands(lines)
    at = next(i for i, line in enumerate(lines) if line.startswith("Validation evidence:"))
    evidence: list[str] = []
    for command in commands:
        evidence += [f"$ {command}", "exit: 0", "tail: unit ok"]
    packet.write_text("\n".join(lines[:at + 1] + evidence + lines[at + 1:]) + "\n", encoding="utf-8")


def fake_worker() -> int:
    """What a worker does, cheaply. FAKE_WORKER_MODE is a comma list: `env` records what the
    worker saw, `sleep` outlives a timeout, `violate` writes outside the lanes, `nohandoff`
    leaves the Handoff empty, `claude` prints claude's JSON usage (else codex's stderr total),
    `fail` exits 1."""
    here = Path.cwd()
    files = here.parent / "~files" / here.name
    unit = json.loads((files / "wave-unit.json").read_text(encoding="utf-8"))
    modes = set(filter(None, os.environ.get("FAKE_WORKER_MODE", "").split(",")))
    prompt = sys.stdin.read()
    if "env" in modes:
        (files / "fake-env.json").write_text(json.dumps({
            "cwd": str(here.resolve()),
            "stdin": prompt.strip()}), encoding="utf-8")
    if "sleep" in modes:
        time.sleep(float(os.environ.get("FAKE_WORKER_SLEEP", "30")))
    for owned in unit["owned_paths"]:
        target = here / owned
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(f"{unit['unit_id']}\n", encoding="utf-8")
    if "violate" in modes:
        (here / "outside.txt").write_text("not mine\n", encoding="utf-8")
        (here / "shared.txt").write_text("touched\n", encoding="utf-8")
    if "nohandoff" not in modes:
        fill_handoff(files / "wave-unit-packet.md")
    if "claude" in modes:
        print(json.dumps({"result": "done", "total_cost_usd": 0.25, "usage": {
            "input_tokens": 100, "output_tokens": 50, "cache_read_input_tokens": 7,
            "cache_creation_input_tokens": 3}}))
    else:
        print("tokens used", file=sys.stderr)
        print("1 234", file=sys.stderr)
    return 1 if "fail" in modes else 0


def flag(args: list[str], name: str) -> str | None:
    return args[args.index(name) + 1] if name in args else None


@case
def test_usage() -> None:
    claude = json.dumps({"result": "ok", "total_cost_usd": 0.25, "usage": {
        "input_tokens": 100, "output_tokens": 50, "cache_read_input_tokens": 7,
        "cache_creation_input_tokens": 3}})
    # Mutation M25: map `.usage.output_tokens` to tokens_in in usage_from_output.
    check("claude_usage_and_cost_come_from_its_json",
          we.usage_from_output(claude, "") == {"tokens_in": 100, "tokens_out": 50, "cache_read": 7,
                                               "cache_creation": 3, "cost_usd": 0.25},
          repr(we.usage_from_output(claude, "")))
    codex = {label: we.usage_from_output("final answer\n", stderr) for label, stderr in (
        ("next_line_space", "working\ntokens used\n51 272\n"),
        ("same_line_comma", "tokens used: 1,234\n"),
        ("label_last", "x\ntokens used 9\n"))}
    # Mutation M26: drop the next-line arm of the codex parse in usage_from_output.
    check("codex_tokens_are_the_number_after_its_label_and_it_has_no_cost",
          {label: usage["tokens_out"] for label, usage in codex.items()}
          == {"next_line_space": 51272, "same_line_comma": 1234, "label_last": 9}
          and all(usage["tokens_in"] is None and usage["cost_usd"] is None for usage in codex.values()),
          repr(codex))
    nothing = we.usage_from_output("plain text", "no usage here")
    check("an_unmetered_worker_reports_nothing",
          nothing == {"tokens_in": None, "tokens_out": None, "cache_read": None,
                      "cache_creation": None, "cost_usd": None}, repr(nothing))


@case
def test_exec() -> None:
    with scratch() as tmp:
        units = (("a", ["a.txt"]), ("b", ["b.txt"]), ("c", ["c.txt"]), ("d", ["d.txt"]))
        repo, lease, _, answer = prepared(tmp, units, workers=4)
        path = we.state_path(repo, 7)
        opened = [we.next_step(root=repo, wave_id=7, lease=lease)["unit"] for _ in range(4)]
        check("four_workers_open_four_units", answer.get("mode") == "wave" and opened == ["a", "b", "c", "d"],
              repr((answer, opened)))
        state = we.read_state(path)
        archive = tmp / "prompt.md"
        archive.write_text("the unit prompt\n", encoding="utf-8")
        rows: dict[str, dict] = {}

        def run(uid: str, mode: str, **env):
            with environ(FAKE_WORKER_MODE=mode, **env):
                answer = safely(we.exec_unit, root=repo, wave_id=7, unit_id=uid, archive=archive,
                                tool="claude", model="claude-sonnet-5", argv=WORKER, lease=lease)
            if isinstance(answer, dict):
                rows[uid] = answer["record"]["payload"]
            return answer

        first = run("a", "env,claude")
        tree_a = Path(state["units"]["a"]["worktree"])
        seen = json.loads((Path(state["units"]["a"]["files_dir"]) / "fake-env.json").read_text(encoding="utf-8"))
        check("the_worker_runs_in_its_tree_with_the_prompt_on_stdin",
              Path(seen["cwd"]) == tree_a
              and seen["stdin"] == "the unit prompt", repr(seen))
        log = git_out(repo, "log", "-1", "--format=%s%n%b", "wave/7/a")
        check("a_finished_unit_is_committed_on_its_branch_without_a_trailer",
              isinstance(first, dict) and first["outcome"] == "done"
              and log.splitlines()[0] == "wave 7: unit a" and "co-authored-by" not in log.lower()
              and git_out(repo, "show", "wave/7/a:a.txt").strip() == "a", repr((first, log)))
        saved = we.read_state(path)
        check("claude_usage_reaches_the_state_and_the_row",
              saved["units"]["a"]["tokens"] == 150 and saved["units"]["a"]["cost_usd"] == 0.25
              and rows.get("a", {}).get("tokens_in") == 100
              and rows.get("a", {}).get("workers_used") == 4
              and rows.get("a", {}).get("prompt_file") == str(archive)
              and first["record"]["kind"] == "metric", repr(rows.get("a")))
        # Mutation M28: in exec_unit, add the lapse note and `slots_exhausted` to every row.
        check("a_row_before_any_lapse_carries_no_lapse",
              "slots_exhausted" not in rows.get("a", {"degraded": ["slots_exhausted"]})["degraded"]
              and rows.get("a", {}).get("note") == "wave 7 unit a: done", repr(rows.get("a")))

        lease.status_answer = LAPSED
        second = run("b", "nohandoff")
        b_commits = git_out(repo, "rev-list", "--count", f"{saved['base']}..wave/7/b").strip()
        # Mutation M29: in finish_unit, ignore the evidence gate's exit code.
        check("an_unproven_handoff_fails_the_unit",
              isinstance(second, dict) and second["outcome"] == "unproven" and b_commits == "0",
              repr((second, b_commits)))
        # Mutation M30: in exec_unit, skip the lease status check at row time.
        check("a_row_after_the_lapse_says_so",
              "slots_exhausted" in rows.get("b", {}).get("degraded", [])
              and "lease lapsed at 2026-10-01T10:00:00Z with workers running" in (rows.get("b", {}).get("note") or ""),
              repr(rows.get("b")))
        lease.status_answer = HELD

        third = run("c", "violate")
        c_commits = git_out(repo, "rev-list", "--count", f"{saved['base']}..wave/7/c").strip()
        # Mutation M31: in changed_paths, list only `git diff --name-only <base> HEAD` (no
        # uncommitted, no untracked files).
        check("a_change_outside_the_lane_is_a_violation_even_uncommitted",
              isinstance(third, dict) and third["outcome"] == "lane_violation" and c_commits == "0",
              repr((third, c_commits)))

        we.mutate_state(path, lambda s: s["config"].update(worker_timeout_seconds=1))

        started = time.monotonic()
        # A short sleep: if `sys.executable` is a launcher, the kill reaches the launcher only and
        # the real interpreter it started lives on, in tree d, until its sleep ends.
        fourth = run("d", "sleep", FAKE_WORKER_SLEEP="4")
        elapsed = time.monotonic() - started
        # Mutation M32: in run_worker, call `proc.wait()` without the timeout.
        check("a_worker_past_its_timeout_is_stopped",
              isinstance(fourth, dict) and fourth["outcome"] == "timeout"
              and fourth["exit_code"] == 124 and elapsed < 3.5, repr((fourth, elapsed)))
        check("a_timed_out_worker_still_yields_its_record",
              isinstance(fourth, dict) and fourth["record"]["payload"]["exit_code"] == 124, repr(fourth))
        statuses = {uid: entry["status"] for uid, entry in we.read_state(path)["units"].items()}
        check("every_finished_unit_is_recorded",
              statuses == {"a": "done", "b": "failed", "c": "failed", "d": "failed"}, repr(statuses))
        failed_run = we.WorkerRun(exit_code=1, timed_out=False, duration_ms=5, stdout="", stderr="")
        check("a_nonzero_worker_fails_the_unit",
              we.finish_unit(worktree=repo, base="HEAD", owned=[], packet=repo / "none.md", wave_id=7,
                             unit_id="x", run=failed_run)[0] == "failed")
        close_with_retry(repo, 7, lease)


def close_with_retry(repo: Path, wave_id: int, lease, attempts: int = 10) -> dict:
    """`close`, again until nothing is left: an interpreter orphaned by a killed launcher can
    hold a unit's tree for the rest of its sleep, and Windows refuses to remove it until then."""
    summary: dict = {}
    for _ in range(attempts):
        summary = safely(we.close, root=repo, wave_id=wave_id, lease=lease)
        if not isinstance(summary, dict) or summary.get("status") != "closing":
            break
        time.sleep(1)
    return summary if isinstance(summary, dict) else {"error": repr(summary)}


@case
def test_exec_errors() -> None:
    with scratch() as tmp:
        repo, lease, _, answer = prepared(tmp, (("e", ["e.txt"]), ("f", ["f.txt"])), workers=2)
        path = we.state_path(repo, 7)
        opened = [we.next_step(root=repo, wave_id=7, lease=lease)["unit"] for _ in range(2)]
        archive = tmp / "prompt.md"
        archive.write_text("the unit prompt\n", encoding="utf-8")
        rows: dict[str, dict] = {}

        def run(uid: str, evidence):
            with environ(FAKE_WORKER_MODE=""):
                answer = safely(we.exec_unit, root=repo, wave_id=7, unit_id=uid, archive=archive,
                                tool="claude", model="claude-sonnet-5", argv=WORKER, lease=lease,
                                evidence=evidence)
            if isinstance(answer, dict):
                rows[uid] = answer["record"]["payload"]
            return answer

        def crash(packet: Path) -> int:
            raise RuntimeError("the evidence gate crashed")

        broken = run("e", crash)
        entry = we.read_state(path)["units"]["e"]
        # Mutation M32b: in exec_unit, re-raise from the `except` around the worker's finish.
        check("an_engine_error_around_a_worker_settles_the_unit_as_errored_and_writes_its_row",
              opened == ["e", "f"] and isinstance(broken, dict) and broken["outcome"] == "errored"
              and entry["status"] == "failed" and entry["outcome"] == "errored"
              and rows.get("e", {}).get("note") == "wave 7 unit e: errored"
              and rows.get("e", {}).get("handoff_complete") is None,
              repr((broken, entry, rows.get("e"))))

        def close_meanwhile(packet: Path) -> int:
            close_with_retry(repo, 7, lease)
            return 1

        late = run("f", close_meanwhile)
        # Mutation M32c: in exec_unit, let the WaveError of `mutate_state(path, record)` propagate.
        check("a_unit_that_returns_after_its_wave_closed_still_writes_its_row",
              isinstance(late, dict) and late["outcome"] == "unproven" and wave_files(repo) == []
              and "the wave was closed before the unit was recorded" in (rows.get("f", {}).get("note") or ""),
              repr((late, rows.get("f"))))
        close_with_retry(repo, 7, lease)


@contextlib.contextmanager
def without_env(*names: str):
    saved = {name: os.environ.pop(name, None) for name in names}
    try:
        yield
    finally:
        for name, value in saved.items():
            if value is not None:
                os.environ[name] = value


def complete_by_hand(repo: Path, uid: str, files: dict[str, str], wave_id: int = 7) -> None:
    """A unit finished without a worker: its files written and committed, the unit marked done."""
    path = we.state_path(repo, wave_id)
    tree = Path(we.read_state(path)["units"][uid]["worktree"])
    for name, text in files.items():
        (tree / name).write_text(text, encoding="utf-8")
    we.commit_unit(tree, wave_id, uid)
    we.mutate_state(path, lambda state: state["units"][uid].update(status="done", outcome="done"))


def mark(repo: Path, uid: str, status: str, outcome: str, wave_id: int = 7) -> None:
    we.mutate_state(we.state_path(repo, wave_id),
                    lambda state: state["units"][uid].update(status=status, outcome=outcome))


def real_hooks() -> Path | None:
    """This checkout's hooks, when they include the queue guard -- else None, and the hook case
    says `skip` rather than `ok`."""
    proc = subprocess.run(["git", "config", "core.hooksPath"], cwd=str(SCRIPTS),
                          capture_output=True, text=True)
    value = proc.stdout.strip() if proc.returncode == 0 else ""
    hooks = Path(value) if value else None
    if hooks is None or not (hooks / "pre-merge-commit").is_file() \
            or not (hooks / "nucleos-queue-guard").is_file():
        return None
    return hooks


@case
def test_consolidate_lands() -> None:
    with scratch() as tmp:
        repo, lease, _, _ = prepared(tmp, (("b", ["b.txt"]), ("a", ["a.txt"]), ("c", ["c.txt"])), workers=3)
        head = git_out(repo, "rev-parse", "HEAD").strip()
        refused = safely(we.consolidate, root=repo, wave_id=7)
        check("consolidate_refuses_a_wave_that_is_not_drained", isinstance(refused, we.WaveError),
              repr(refused))
        complete_by_hand(repo, "a", {"a.txt": "from a\n"})
        complete_by_hand(repo, "b", {"b.txt": "from b\n"})
        mark(repo, "c", "failed", "unproven")
        result = safely(we.consolidate, root=repo, wave_id=7)
        integration = Path(we.read_state(we.state_path(repo, 7))["integration"] or tmp)
        subjects = git_out(integration, "log", "--first-parent", "--format=%s").splitlines() \
            if integration.name.startswith("integration-wave-") else []
        # Mutation M33: in consolidate, take the done units in manifest order (iterate
        # `state["units"]`) instead of sorted.
        check("units_merge_one_at_a_time_in_unit_id_order",
              len(subjects) >= 2 and subjects[1] == "wave 7: unit a", repr(subjects))
        check("consolidate_lands_done_units_and_returns_the_rest",
              isinstance(result, dict) and result["landed"] == ["a", "b"] and result["pending"] == ["c"],
              repr(result))
        status = set(git_out(repo, "status", "--porcelain", "--untracked-files=all").splitlines())
        check("the_landed_work_is_in_the_tree_uncommitted",
              (repo / "a.txt").read_text(encoding="utf-8") == "from a\n"
              and (repo / "b.txt").read_text(encoding="utf-8") == "from b\n"
              and git_out(repo, "rev-parse", "HEAD").strip() == head
              and git_out(repo, "diff", "--cached", "--name-only") == ""
              and {"?? a.txt", "?? b.txt"} <= status, repr(status))
        summary = we.close(root=repo, wave_id=7, lease=lease)
        check("close_reports_what_landed_and_what_is_pending",
              summary.get("landed") == ["a", "b"] and summary.get("pending") == ["c"], repr(summary))


@case
def test_land_refused_and_conflict() -> None:
    with scratch() as tmp:
        repo, lease, _, _ = prepared(tmp)
        complete_by_hand(repo, "a", {"a.txt": "from a\n"})
        mark(repo, "b", "failed", "failed")
        (repo / "shared.txt").write_text("edited during the wave\n", encoding="utf-8")
        result = safely(we.consolidate, root=repo, wave_id=7)
        patch = state_of(repo) / "wave" / "7.patch"
        # Mutation M34: drop the base_tree comparison from consolidate.
        check("a_tree_changed_during_the_wave_lands_nothing",
              isinstance(result, dict) and result["landed"] == [] and result["pending"] == ["a", "b"]
              and not (repo / "a.txt").exists() and patch.is_file()
              and "land_refused" in (result["note"] or ""), repr(result))
        we.close(root=repo, wave_id=7, lease=lease)
        check("the_refused_patch_outlives_close", patch.is_file())
    with scratch() as tmp:
        repo, lease, _, _ = prepared(tmp)
        complete_by_hand(repo, "a", {"shared.txt": "from a\n"})
        complete_by_hand(repo, "b", {"shared.txt": "from b\n"})
        result = safely(we.consolidate, root=repo, wave_id=7)
        saved = we.read_state(we.state_path(repo, 7))
        check("a_merge_conflict_aborts_the_wave_and_lands_nothing",
              isinstance(result, dict) and result["landed"] == [] and result["pending"] == ["a", "b"]
              and saved["aborted"] and "lane_violation" in saved["stop_reasons"]
              and (repo / "shared.txt").read_text(encoding="utf-8") == "changed\n", repr(result))
        summary = we.close(root=repo, wave_id=7, lease=lease)
        check("an_aborted_wave_closes_with_lane_violation",
              "lane_violation" in summary["degraded"] and summary["pending"] == ["a", "b"], repr(summary))


@case
def test_real_hooks() -> None:
    hooks = real_hooks()
    if hooks is None:
        print("  skip the_queue_guard_blocks_a_merge_outside_an_integration_tree: "
              "no core.hooksPath with the queue guard in this checkout")
        print("  skip the_consolidation_merge_passes_the_real_hooks: same")
        return
    with without_env("NUCLEOS_QUEUE_EXEC", "NUCLEOS_ALLOW_DIRECT_GIT"), scratch() as tmp:
        repo, lease, _, _ = prepared(tmp, hooks=hooks)
        complete_by_hand(repo, "a", {"a.txt": "from a\n"})
        complete_by_hand(repo, "b", {"b.txt": "from b\n"})
        base = we.read_state(we.state_path(repo, 7))["base"]
        control = tmp / "plain-tree"
        git_out(repo, "worktree", "add", "--detach", str(control), base)
        blocked = subprocess.run(["git", "merge", "--no-ff", "--no-edit", "wave/7/a"], cwd=str(control),
                                 capture_output=True, text=True)
        subprocess.run(["git", "merge", "--abort"], cwd=str(control), capture_output=True)
        git_out(repo, "worktree", "remove", "--force", str(control))
        check("the_queue_guard_blocks_a_merge_outside_an_integration_tree",
              blocked.returncode != 0 and "blocked" in blocked.stderr, blocked.stderr[-400:])
        result = safely(we.consolidate, root=repo, wave_id=7)
        # Mutation M35: make integration_name return f"merge-wave-{wave_id}".
        check("the_consolidation_merge_passes_the_real_hooks",
              isinstance(result, dict) and result["landed"] == ["a", "b"], repr(result))
        we.close(root=repo, wave_id=7, lease=lease)


@case
def test_close_after_an_interruption() -> None:
    with scratch() as tmp:
        repo, lease, _, _ = prepared(tmp)
        we.next_step(root=repo, wave_id=7, lease=lease)  # unit a is running when the session dies
        state = we.read_state(we.state_path(repo, 7))
        lease.status_answer = LAPSED
        summary = we.close(root=repo, wave_id=7, lease=lease)
        # Mutation M36: in close, replace the call `lease.close(wave_id, str(root))` with `{}`
        # (the expression stays, so the mutant cannot die of a NameError instead of this check).
        check("close_gives_the_lease_back", ("close", 7) in lease.calls, repr(lease.calls))
        # Mutation M37: delete both the `git worktree remove` loop and the `git worktree prune`
        # line from close.
        check("close_leaves_no_worktree",
              len(worktrees(repo)) == 1 and not Path(state["scratch"]).exists(), repr(worktrees(repo)))
        # Mutation M38: drop the branch-deletion loop from close.
        check("close_leaves_no_branch", wave_branches(repo) == [], repr(wave_branches(repo)))
        check("close_leaves_no_state",
              wave_files(repo) == [] and not we.lock_path(we.state_path(repo, 7)).exists())
        check("an_interrupted_wave_sends_every_unit_back_with_the_lapse",
              summary.get("landed") == [] and summary.get("pending") == ["a", "b"]
              and summary.get("unguarded_since") == "2026-10-01T10:00:00Z"
              and "slots_exhausted" in summary.get("degraded", [])
              and (summary.get("metrics_args") or {}).get("workers_requested") == 2 and (summary.get("metrics_args") or {}).get("workers_used") == 1
              and "lease lapsed at 2026-10-01T10:00:00Z with workers running" in (summary.get("note") or ""),
              repr(summary))
        # Mutation M40: in close, drop the `if state is None` early return.
        again = safely(we.close, root=repo, wave_id=7, lease=lease)
        check("a_second_close_is_a_no_op", again == {"wave_id": 7, "status": "closed"}, repr(again))


@case
def test_close_keeps_what_it_could_not_remove() -> None:
    with scratch() as tmp:
        repo, lease, _, _ = prepared(tmp)

        def refuse(path: str) -> None:  # Windows, while a running worker still holds the directory
            return None

        first = we.close(root=repo, wave_id=7, lease=lease, remove=refuse)
        kept = we.read_state(we.state_path(repo, 7))
        # Mutation M41b: in close, drop the `if leftovers:` block, so the state is always deleted.
        check("a_close_that_leaves_something_keeps_the_state_for_the_next_sweep",
              first.get("status") == "closing" and bool(first.get("leftovers"))
              and isinstance(kept, dict) and kept.get("status") == "closing"
              and kept.get("leftovers") == first.get("leftovers"), repr((first, kept)))
        swept = we.sweep(repo, lease)
        # Mutation M41c: in close, call `lease.close(...)` even when the state holds `lease_end`.
        check("the_next_sweep_finishes_the_close_and_gives_the_lease_back_once",
              [entry.get("status") for entry in swept] == ["closed"] and wave_files(repo) == []
              and lease.calls.count(("close", 7)) == 1, repr((swept, lease.calls)))


@case
def test_prepare_sweeps_a_leftover_wave() -> None:
    with scratch() as tmp:
        repo, _, _, first = prepared(tmp)
        second_lease = FakeLease(wave_id=8)
        # Mutation M41: drop the `sweep(root, lease)` call from prepare.
        answer = we.prepare(cwd=repo, plan=tmp / "task" / "plan.md", packets_dir=tmp / "task" / "packets",
                            workers=2, frozen=["tests/test_red.py"], risk_globs=[], lease=second_lease,
                            seeder=FakeSeeder())
        check("prepare_sweeps_a_leftover_wave",
              first.get("wave_id") == 7 and answer.get("mode") == "wave" and answer.get("wave_id") == 8
              and [swept.get("wave_id") for swept in answer.get("swept", [])] == [7]
              and ("close", 7) in second_lease.calls and wave_branches(repo) == ["wave/8/a", "wave/8/b"],
              repr((answer, wave_branches(repo))))
        we.close(root=repo, wave_id=8, lease=second_lease)


@case
def test_cli() -> None:
    with scratch() as tmp:
        repo = make_repo(tmp)
        write_models(repo, enabled=False)
        plan, packets = write_task(tmp / "task", [manifest_unit("a", ["a.txt"]), manifest_unit("b", ["b.txt"])])
        frozen = tmp / "frozen.txt"
        frozen.write_text("# the RED tests\ntests/test_a.py\n", encoding="utf-8")
        engine = [sys.executable, str(SCRIPTS / "wave_engine.py")]

        def cli(*args: str) -> subprocess.CompletedProcess:
            return subprocess.run(engine + list(args), capture_output=True, text=True)

        answer = cli("prepare", "--cwd", str(repo), "--plan", str(plan), "--packets-dir", str(packets),
                     "--workers", "2", "--frozen", str(frozen))
        body = safely(json.loads, answer.stdout)
        check("the_cli_prepare_answers_sequential_json",
              answer.returncode == 0 and isinstance(body, dict) and body.get("mode") == "sequential"
              and body.get("degraded") == ["wave_disabled"], answer.stdout + answer.stderr)
        closed = cli("close", "--wave-id", "99", "--cwd", str(repo))
        check("the_cli_close_always_exits_zero",
              closed.returncode == 0 and safely(json.loads, closed.stdout) == {"wave_id": 99, "status": "closed"},
              closed.stdout + closed.stderr)
        nowhere = cli("close", "--wave-id", "99", "--cwd", str(tmp))
        check("the_cli_close_exits_zero_even_outside_a_repository",
              nowhere.returncode == 0 and "close_failed" in nowhere.stdout, nowhere.stdout + nowhere.stderr)
        unknown = cli("next", "--wave-id", "99", "--cwd", str(repo))
        check("a_verb_on_an_unknown_wave_is_an_error_not_a_crash",
              unknown.returncode == 2 and "error" in (safely(json.loads, unknown.stdout) or {}),
              unknown.stdout + unknown.stderr)
        no_worker = cli("exec", "--wave-id", "99", "--unit", "a", "--archive", str(plan), "--tool", "codex",
                        "--model", "m", "--cwd", str(repo))
        check("exec_needs_the_worker_after_the_separator",
              no_worker.returncode == 2 and "after `--`" in no_worker.stderr, no_worker.stderr)


@case
def test_invariants() -> None:
    source = (SCRIPTS / "wave_engine.py").read_text(encoding="utf-8")
    gate_lines = [line for line in source.splitlines() if "gates.sh" in line or "select_tests" in line]
    check("the_engine_never_runs_the_catalog_gate_or_the_full_suite",
          len(gate_lines) == 1 and gate_lines[0].startswith("CONTROLLER_ONLY_COMMANDS"), repr(gate_lines))
    # Mutation M43: add "--no-verify" to the `git commit` in commit_unit.
    forbidden = ("review_gate", "--hook", "compute_merge", "merge_queue", "NUCLEOS_ALLOW_DIRECT_GIT",
                 "NUCLEOS_QUEUE_EXEC", "--no-verify", "core.hooksPath")
    found = [word for word in forbidden if word in source]
    check("the_engine_never_bypasses_a_gate_a_hook_or_the_merge_queue", found == [], repr(found))
    check("the_engine_runs_the_evidence_gate_on_the_unit_packet",
          'SCRIPTS / "evidence_gate.py"' in source and "str(packet)" in source)
    # Mutation M42: misspell BUDGET as "wave_budget_exceeded" in wave_engine.py.
    import wave_lease
    unknown = [reason for reason in we.REASONS if reason not in wave_lease.DEGRADED_VOCABULARY]
    check("every_reason_is_in_the_record_vocabulary", unknown == [], repr(unknown))


def main() -> int:
    if sys.argv[1:2] == ["--fake-worker"]:
        return fake_worker()
    for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"):
        os.environ.pop(name, None)
    for test in TESTS:
        test()
    if FAILURES:
        print(f"\n{len(FAILURES)} failure(s)")
        return 1
    print("\nall wave_engine checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
