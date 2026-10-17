#!/usr/bin/env python3
"""Tests for `wave_lease.py` -- the controller's side of a wave's slot lease.

Run:  python test_wave_lease.py

The daemon here is a real HTTP server on a free local port answering from a script, so
the client's own urllib path is what runs -- the one that meets `core/src/http.rs`.
"""
from __future__ import annotations

import http.server
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import wave_lease as wl  # noqa: E402

FAILURES: list[str] = []


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


def limits(max_workers: int = 4, max_units: int = 4, max_wave_seconds: int = 2400) -> "wl.Limits":
    return wl.Limits(max_workers=max_workers, max_units=max_units, max_wave_seconds=max_wave_seconds)


class FakeDaemon:
    """A daemon on a free local port that answers from a script and remembers what it was asked."""

    def __init__(self) -> None:
        self.requests: list[tuple[str, str, dict, object]] = []
        self.answers: dict[tuple[str, str], tuple[int, object]] = {}
        daemon = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def _answer(self) -> None:
                length = int(self.headers.get("Content-Length") or 0)
                raw = self.rfile.read(length) if length else b""
                daemon.requests.append((self.command, self.path, dict(self.headers),
                                        json.loads(raw) if raw else None))
                status, body = daemon.answers.get((self.command, self.path), (404, None))
                payload = b"" if body is None else json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            do_POST = _answer
            do_DELETE = _answer

            def log_message(self, *args) -> None:
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()


def dead_url() -> str:
    """A local port nothing listens on: bound, read, and let go."""
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    return f"http://127.0.0.1:{port}"


def grant(wave_id, requested, granted, reason=None) -> dict:
    return {"wave_id": wave_id, "requested": requested, "granted": granted,
            "lease_seconds": 120, "reason": reason}


def test_limits() -> None:
    # Never assert a live models.yaml's values: it is per-project state.
    with tempfile.TemporaryDirectory() as tmp:
        # Mutation M1: read `max_units` from the `max_workers` key.
        path = Path(tmp) / "models.yaml"
        path.write_text("parallel_execute: {max_workers: 2, max_units: 4, max_wave_seconds: 2400}\n",
                        encoding="utf-8")
        read = wl.read_limits(path)
        check("limits_come_from_parallel_execute",
              (read.max_workers, read.max_units, read.max_wave_seconds) == (2, 4, 2400), repr(read))
    # With no project (no project.json), the default path is a LimitsError that says so, not a crash.
    with tempfile.TemporaryDirectory() as tmp:
        old = os.environ.get("NUCLEOS_PROJECT_ROOT")
        os.environ["NUCLEOS_PROJECT_ROOT"] = tmp
        try:
            err = safely(wl.read_limits)
        finally:
            os.environ.pop("NUCLEOS_PROJECT_ROOT") if old is None else os.environ.__setitem__("NUCLEOS_PROJECT_ROOT", old)
        check("a_missing_project_is_a_limits_error", isinstance(err, wl.LimitsError) and "project.json" in str(err), repr(err))

    # Mutation M2: drop the `value < 1` refusal in `read_limits`.
    with tempfile.TemporaryDirectory() as tmp:
        refused = []
        for body in ("parallel_execute: {max_workers: 2, max_units: 0, max_wave_seconds: 60}\n",
                     "parallel_execute: {max_workers: 2, max_wave_seconds: 60}\n",
                     "parallel_execute: {max_workers: two, max_units: 4, max_wave_seconds: 60}\n",
                     "nothing: here\n"):
            path = Path(tmp) / "models.yaml"
            path.write_text(body, encoding="utf-8")
            try:
                wl.read_limits(path)
                refused.append(False)
            except wl.LimitsError:
                refused.append(True)
        check("a_malformed_parallel_execute_is_a_config_error", all(refused), repr(refused))

    # Mutation M3: remove DAEMON_MAX_WORKERS from the min in `requested_workers`.
    # Mutation M3b: remove `decision_workers` from the same min.
    asked = (wl.requested_workers(3, 9, limits(max_workers=2)),
             wl.requested_workers(9, 9, limits(9, 9)),
             wl.requested_workers(1, 9, limits()),
             wl.requested_workers(3, 1, limits()))
    check("the_request_never_exceeds_units_decision_max_workers_or_the_daemon_cap",
          asked == (2, 4, 1, 1), repr(asked))


def test_request_wave() -> None:
    daemon = FakeDaemon()
    os.environ["NUCLEOS_DAEMON_URL"] = daemon.url
    try:
        # Mutation M4: move the `max_units` refusal after the POST.
        opened = wl.request_wave("C:/x", 5, 4, limits(max_units=4), "control")
        check("more_units_than_max_units_never_asks_the_daemon",
              opened.workers == 1 and opened.degraded == ["task_not_decomposable"]
              and daemon.requests == [], repr((opened, daemon.requests)))

        opened = wl.request_wave("C:/x", 2, 4, limits(max_workers=1), "control")
        check("a_wave_of_one_asks_for_nothing",
              opened.workers == 1 and opened.degraded == [] and daemon.requests == [], repr(opened))

        # `normal` resolves `workers: 1` (speed-cases.yaml): three units, and still no slot asked.
        opened = wl.request_wave("C:/x", 3, 1, limits(), "control")
        check("a_decision_of_one_worker_asks_for_nothing",
              opened.workers == 1 and opened.degraded == [] and daemon.requests == [], repr(opened))

        opened = wl.request_wave("C:/x", 3, 4, limits(), None)
        check("no_token_means_one_worker_and_no_request",
              opened.workers == 1 and opened.degraded == ["no_capacity_authority"]
              and daemon.requests == [], repr(opened))

        # Mutation M6: accept only 400/404 as refusals, and read any other status as a grant.
        # Every refusal carries a WELL-FORMED grant body, so a mutant that reads it succeeds in
        # reading it -- and the check sees three workers where there must be one.
        outcomes = {}
        for status in (400, 401, 403, 404, 500):
            daemon.answers[("POST", "/concurrency/waves")] = (status, grant(12, 3, 3))
            opened = wl.request_wave("C:/x", 3, 4, limits(), "control")
            outcomes[status] = (opened.workers, opened.degraded, opened.wave_id)
        check("every_refusal_status_means_one_worker_and_no_capacity_authority",
              all(value == (1, ["no_capacity_authority"], None) for value in outcomes.values()),
              repr(outcomes))

        # The contract's "403 inside a run": the run token reaches only the hook routes.
        daemon.answers[("POST", "/concurrency/waves")] = (403, grant(12, 3, 3))
        opened = wl.request_wave("C:/x", 3, 4, limits(), "run-token")
        check("a_run_token_is_refused_and_the_controller_runs_alone",
              opened.workers == 1 and opened.degraded == ["no_capacity_authority"], repr(opened))

        # Mutation M7: send `workers: units` instead of the capped request.
        daemon.requests.clear()
        daemon.answers[("POST", "/concurrency/waves")] = (200, grant(12, 2, 2))
        wl.request_wave("C:/work", 3, 4, limits(max_workers=2), "control")
        method, path, headers, body = daemon.requests[0]
        check("the_grant_request_carries_cwd_workers_and_the_bearer_token",
              (method, path, body) == ("POST", "/concurrency/waves", {"cwd": "C:/work", "workers": 2})
              and headers.get("Authorization") == "Bearer control", repr(daemon.requests[0]))

        # Mutation M8: delete the whole `if wave_id is None or granted < 1:` block.
        daemon.answers[("POST", "/concurrency/waves")] = (200, grant(None, 3, 0, "the machine is full"))
        opened = safely(wl.request_wave, "C:/x", 3, 4, limits(), "control")
        check("nothing_granted_is_one_worker_with_no_wave_to_give_back",
              isinstance(opened, wl.Opening)
              and (opened.workers, opened.wave_id, opened.degraded) == (1, None, ["slots_exhausted"]),
              repr(opened))

        # Mutation M9: `workers = requested` on a partial grant.
        daemon.answers[("POST", "/concurrency/waves")] = (200, grant(12, 3, 2, "project full"))
        opened = wl.request_wave("C:/x", 3, 4, limits(), "control")
        check("a_partial_grant_is_that_many_workers_and_slots_exhausted",
              (opened.workers, opened.wave_id, opened.degraded) == (2, 12, ["slots_exhausted"]),
              repr(opened))

        daemon.answers[("POST", "/concurrency/waves")] = (200, grant(13, 3, 3))
        opened = wl.request_wave("C:/x", 3, 4, limits(), "control")
        check("a_full_grant_is_the_min_of_units_decision_max_workers_and_granted",
              (opened.workers, opened.wave_id, opened.degraded, opened.lease_seconds)
              == (3, 13, [], 120), repr(opened))
    finally:
        daemon.close()

    # Mutation M5: on a network error, return `Opening(requested, ...)` instead of one worker.
    # (On Windows a refused connect costs ~2 s of SYN retries; that is this case's whole cost.)
    os.environ["NUCLEOS_DAEMON_URL"] = dead_url()
    opened = wl.request_wave("C:/x", 3, 4, limits(), "control")
    check("an_unreachable_daemon_means_one_worker_and_no_capacity_authority",
          opened.workers == 1 and opened.degraded == ["no_capacity_authority"], repr(opened))


def test_renew_and_release() -> None:
    daemon = FakeDaemon()
    os.environ["NUCLEOS_DAEMON_URL"] = daemon.url
    try:
        seen = {}
        # Mutation M10: map 404 to "no_answer".
        for status in (204, 410, 404, 403, 500):
            daemon.answers[("POST", "/concurrency/waves/12/renew")] = (status, None)
            seen[status] = wl.renew(12, "control")
        check("renew_reads_204_as_renewed_the_rest_as_lapsed_and_5xx_as_silence",
              seen == {204: "renewed", 410: "lapsed", 404: "lapsed", 403: "lapsed", 500: "no_answer"},
              repr(seen))

        # Mutation M11: raise on anything but 204 in `release`.
        daemon.answers[("DELETE", "/concurrency/waves/12")] = (204, None)
        first = wl.release(12, "control")
        daemon.answers[("DELETE", "/concurrency/waves/12")] = (404, None)
        second = safely(wl.release, 12, "control")
        check("release_is_never_an_error", (first, second) == (True, True), repr((first, second)))
    finally:
        daemon.close()
    os.environ["NUCLEOS_DAEMON_URL"] = dead_url()
    check("renew_hears_silence_from_a_dead_daemon", wl.renew(12, "control") == "no_answer")
    check("release_to_a_dead_daemon_is_false_not_raised", wl.release(12, "control") is False)

    # Mutation M12: return None from `control_token` before reading NUCLEOS_DAEMON_TOKEN.
    os.environ["NUCLEOS_DAEMON_TOKEN"] = "from-env"
    check("the_token_in_the_environment_wins", wl.control_token(".") == "from-env")
    del os.environ["NUCLEOS_DAEMON_TOKEN"]
    # Run from a git hook, GIT_DIR would make any directory look like this repository.
    git_env = {key: os.environ.pop(key) for key in list(os.environ) if key.startswith("GIT_")}
    try:
        with tempfile.TemporaryDirectory() as outside_git:
            check("outside_git_there_is_no_token_and_nothing_prints",
                  wl.control_token(outside_git) is None)
        with tempfile.TemporaryDirectory() as project:
            subprocess.run(["git", "init", "-q", project], check=True)
            hooks = Path(project) / ".claude" / "hooks"
            hooks.mkdir(parents=True)
            (hooks / "ask_daemon.py").write_text(
                "def read_control_token(cwd):\n    return 'from-hook', None\n", encoding="utf-8")
            check("the_token_comes_from_the_projects_installed_hook",
                  wl.control_token(project) == "from-hook")
            (hooks / "ask_daemon.py").write_text("raise RuntimeError('x')\n", encoding="utf-8")
            check("a_broken_hook_is_no_token_and_no_crash", wl.control_token(project) is None)
    finally:
        os.environ.update(git_env)


class Clock:
    """A clock that moves only when the loop sleeps, and refuses to sleep forever."""

    def __init__(self, start: float = 1_000_000.0, budget: int = 50) -> None:
        self.now, self.slept, self.budget = start, [], budget

    def time(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        if len(self.slept) >= self.budget:
            raise RuntimeError("the renewer never stopped")
        self.slept.append(seconds)
        self.now += seconds


def scripted(*answers: str):
    """A renew call answering from a script; its last answer repeats for ever."""
    queue, calls = list(answers), []

    def renew_call(wave_id: int, token: str) -> str:
        calls.append(wave_id)
        return queue.pop(0) if len(queue) > 1 else queue[0]

    return renew_call, calls


def test_renewer() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        wl.STATE_DIR = Path(tmp)
        path = wl.state_path(12)

        # Mutation M13: sleep `lease_seconds` instead of a quarter of it.
        clock = Clock()
        wl.write_state(path, {"wave_id": 12, "status": "held"})
        call, calls = scripted("renewed", "renewed", "lapsed")
        ended = wl.renew_loop(12, "t", 120, clock.time() + 2400, path,
                              now=clock.time, sleep=clock.sleep, renew_call=call)
        state = wl.read_state(path)
        check("the_renewer_renews_every_quarter_lease_and_stops_on_410",
              ended == "lapsed" and clock.slept == [30, 30, 30] and len(calls) == 3
              and state["status"] == "lapsed" and state["unguarded_since"] is not None,
              repr((ended, clock.slept, state)))

        # Mutation M14: drop the silence-outlasts-the-lease check.
        clock = Clock()
        wl.write_state(path, {"wave_id": 12, "status": "held"})
        call, _ = scripted("no_answer")
        started = clock.time()
        ended = safely(wl.renew_loop, 12, "t", 120, clock.time() + 2400, path,
                       now=clock.time, sleep=clock.sleep, renew_call=call)
        # Silent at 30, 60 and 90 s: from 90 the next try (120) would be too late.
        check("the_renewer_gives_up_when_silence_outlasts_the_lease",
              ended == "lapsed" and sum(clock.slept) == 90
              and wl.read_state(path)["unguarded_since"] == wl.stamp(started + 120),
              repr((ended, clock.slept, wl.read_state(path))))

        # Mutation M15: drop the deadline check.
        clock = Clock()
        wl.write_state(path, {"wave_id": 12, "status": "held"})
        call, _ = scripted("renewed")
        ended = safely(wl.renew_loop, 12, "t", 120, clock.time() + 100, path,
                       now=clock.time, sleep=clock.sleep, renew_call=call)
        check("the_renewer_stops_at_the_wave_deadline",
              ended == "deadline" and wl.read_state(path)["status"] == "deadline", repr(ended))

        clock = Clock()
        wl.write_state(path, {"wave_id": 12, "status": "closing"})
        call, calls = scripted("renewed")
        ended = wl.renew_loop(12, "t", 120, clock.time() + 2400, path,
                              now=clock.time, sleep=clock.sleep, renew_call=call)
        check("the_renewer_stops_when_the_wave_is_closed", ended == "closed" and calls == [],
              repr((ended, calls)))

        # Mutation M16: map `deadline` to slots_exhausted in DEGRADED_FOR.
        endings = {}
        for status, pid in (("held", os.getpid()), ("held", 0), ("lapsed", 0), ("deadline", 0)):
            wl.write_state(path, {"wave_id": 12, "status": status, "renewer_pid": pid})
            endings[(status, bool(pid))] = wl.wave_status(12)
        path.unlink()
        endings[("missing", False)] = wl.wave_status(12)
        summary = {key: (code, body["status"], body["degraded"]) for key, (code, body) in endings.items()}
        check("status_maps_each_ending_to_its_reason_and_exit_code", summary == {
            ("held", True): (0, "held", []),
            ("held", False): (3, "renewer_gone", ["slots_exhausted"]),
            ("lapsed", False): (3, "lapsed", ["slots_exhausted"]),
            ("deadline", False): (3, "deadline", ["wave_budget_exhausted"]),
            ("missing", False): (3, "closed", []),
        }, repr(summary))

    # Mutation M17: add the token to `renewer_argv`.
    argv = wl.renewer_argv(12, 120, 1_000_000.0, Path("s.json"))
    check("the_token_never_travels_in_argv", not any("secret" in part for part in argv)
          and "_renew" in argv, repr(argv))

    # Mutation M18: implement `pid_alive` with `os.kill(pid, 0)` on Windows.
    sleeper = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
    try:
        asked = wl.pid_alive(sleeper.pid) and wl.pid_alive(sleeper.pid)
        time.sleep(0.5)  # TerminateProcess is asynchronous: poll() right away would still say running
        check("pid_alive_does_not_kill_what_it_asks_about",
              asked and sleeper.poll() is None, repr((asked, sleeper.poll())))
    finally:
        sleeper.kill()
        sleeper.wait()
    check("a_dead_pid_is_not_alive", wl.pid_alive(sleeper.pid) is False)

    # Mutation M19: a typo in any reason (`slot_exhausted`).
    check("every_reason_is_in_the_record_vocabulary",
          set(wl.REASONS) <= set(wl.DEGRADED_VOCABULARY),
          repr(set(wl.REASONS) - set(wl.DEGRADED_VOCABULARY)))


def test_end_to_end() -> None:
    daemon = FakeDaemon()
    daemon.answers[("POST", "/concurrency/waves")] = (200, {**grant(12, 2, 2), "lease_seconds": 4})
    daemon.answers[("POST", "/concurrency/waves/12/renew")] = (204, None)
    daemon.answers[("DELETE", "/concurrency/waves/12")] = (204, None)
    script = str(Path(wl.__file__).resolve())
    with tempfile.TemporaryDirectory() as root:
        state = Path(root) / "state"
        state.mkdir()
        (state / "models.yaml").write_text(
            "parallel_execute: {max_workers: 2, max_units: 4, max_wave_seconds: 600}\n", encoding="utf-8")
        (Path(root) / ".nucleos" / "workflow").mkdir(parents=True)
        (Path(root) / ".nucleos" / "workflow" / "project.json").write_text(
            json.dumps({"id": "t", "state_dir": str(state), "bundle_dir": str(Path(wl.__file__).resolve().parent.parent)}),
            encoding="utf-8")
        env = {**os.environ, "NUCLEOS_PROJECT_ROOT": root, "NUCLEOS_DAEMON_URL": daemon.url,
               "NUCLEOS_DAEMON_TOKEN": "control"}

        def run(*args: str) -> tuple[int, dict]:
            done = subprocess.run([sys.executable, script, *args], capture_output=True, text=True,
                                  env=env, timeout=30)
            try:
                return done.returncode, json.loads(done.stdout)
            except ValueError:  # a traceback is a failed check with its text, not a crash here
                return done.returncode, {"stdout": done.stdout, "stderr": done.stderr[-800:]}

        try:
            code, opened = run("open", "--cwd", root, "--units", "2", "--workers", "2")
            check("open_answers_with_the_wave_and_its_workers",
                  code == 0 and opened.get("wave_id") == 12 and opened.get("workers") == 2,
                  repr(opened))

            # `open` has exited; the renewer it started has to be renewing without it.
            # Polled, not slept: a cold interpreter start can take seconds on a loaded machine.
            give_up = time.time() + 30
            renewals: list = []
            while not renewals and time.time() < give_up:
                time.sleep(0.25)
                renewals = [r for r in daemon.requests if r[1] == "/concurrency/waves/12/renew"]
            code, held = run("status", "--wave-id", "12")
            check("the_renewer_outlives_open_and_renews",
                  renewals and code == 0 and held.get("status") == "held", repr((renewals, held)))
            pid = held.get("renewer_pid")

            daemon.answers[("POST", "/concurrency/waves/12/renew")] = (410, None)
            deadline = time.time() + 5
            code = 0
            while code == 0 and time.time() < deadline:
                time.sleep(0.25)
                code, lapsed = run("status", "--wave-id", "12")
            check("a_410_reaches_the_engine_as_exit_3_and_slots_exhausted",
                  code == 3 and lapsed.get("status") == "lapsed"
                  and lapsed.get("degraded") == ["slots_exhausted"], repr((code, lapsed)))

            # Mutation M20: skip the release in `close_wave` when the wave already lapsed.
            code, closed = run("close", "--wave-id", "12")
            deletes = [r for r in daemon.requests if r[0] == "DELETE"]
            # A lapsed renewer writes `lapsed` and then exits on its own; on a loaded machine the
            # exit can trail the write, so its death is polled rather than read once.
            give_up = time.time() + 10
            while wl.pid_alive(pid) and time.time() < give_up:
                time.sleep(0.25)
            check("close_gives_the_wave_back_even_after_a_lapse_and_stops_the_renewer",
                  code == 0 and deletes and closed.get("ended_as") == "lapsed"
                  and closed.get("unguarded_since") is not None and not wl.pid_alive(pid)
                  and not (state / "wave" / "leases" / "12.json").exists(),
                  repr((closed, deletes)))

            code, again = run("close", "--wave-id", "12")
            check("a_second_close_is_not_an_error", code == 0, repr(again))
        finally:
            daemon.close()

    # Mutation M21: in `open_wave`, return the grant when the renewer cannot start, without release.
    daemon = FakeDaemon()
    daemon.answers[("POST", "/concurrency/waves")] = (200, grant(14, 2, 2))
    daemon.answers[("DELETE", "/concurrency/waves/14")] = (204, None)
    os.environ["NUCLEOS_DAEMON_URL"] = daemon.url
    os.environ["NUCLEOS_DAEMON_TOKEN"] = "control"
    try:
        with tempfile.TemporaryDirectory() as tmp:
            wl.STATE_DIR = Path(tmp)
            models = Path(tmp) / "models.yaml"
            models.write_text("parallel_execute: {max_workers: 2, max_units: 4, max_wave_seconds: 60}\n",
                              encoding="utf-8")

            def no_spawn(*args):
                raise OSError("no process for you")

            opened = wl.open_wave(tmp, 2, 2, models=models, spawn=no_spawn)
            check("open_gives_the_wave_back_when_the_renewer_cannot_start",
                  opened["workers"] == 1 and opened["wave_id"] is None
                  and any(r[0] == "DELETE" for r in daemon.requests), repr(opened))

            # Mutation M27: answer a broken `parallel_execute` with `degraded: ["wave_disabled"]`.
            # The wave is not disabled -- `open` never reads `enabled` (desvio 11) -- and a reason
            # on the GREEN row would hide a config error behind a degradation.
            daemon.requests.clear()
            models.write_text("parallel_execute: {max_workers: 0}\n", encoding="utf-8")
            opened = wl.open_wave(tmp, 2, 2, models=models, spawn=no_spawn)
            check("a_broken_parallel_execute_is_one_worker_and_a_config_error_not_a_reason",
                  (opened["workers"], opened["degraded"], opened["wave_id"]) == (1, [], None)
                  and "config_error" in opened and daemon.requests == [], repr(opened))
    finally:
        del os.environ["NUCLEOS_DAEMON_TOKEN"]
        daemon.close()


def main() -> int:
    saved = dict(os.environ)
    try:
        test_limits()
        test_request_wave()
        test_renew_and_release()
        test_renewer()
        test_end_to_end()
    finally:
        os.environ.clear()
        os.environ.update(saved)
    if FAILURES:
        print(f"\n{len(FAILURES)} failure(s)")
        return 1
    print("\nall wave_lease checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
