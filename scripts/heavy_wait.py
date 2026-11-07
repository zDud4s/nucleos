#!/usr/bin/env python3
"""Helpers for lock-free waiting in the heavy broker (scripts/heavy.py).

Every waiter used to take the global state Mutex on each poll and, inside it, read every queue
and held record and probe every pid. With 8+ waiters the Mutex convoyed: 79% of the slow
`mutex.jsonl` rows were waiters that held it under 1 s but waited 1 s or more. This module holds
the small pieces that keep waiters off the Mutex:

- AliveCache: per-(pid, ctime) liveness results with a short TTL, so one poll does not pay an
  OpenProcess + GetProcessTimes per record.
- Backoff: a jittered poll interval that grows while the queue position is unchanged and resets
  when it improves.
- boost / restore: ABOVE_NORMAL priority for the process that holds the Mutex, so a holder
  starved by 4-5 heavy jobs gets CPU to finish its sub-millisecond critical section.
- Steps: perf_counter marks inside a hold, for the `steps` field of the slow trace row.

Nothing here raises into the broker: every failure degrades to the old behaviour.
"""

from __future__ import annotations

import os
import random
import time

_IS_WIN = os.name == "nt"
ABOVE_NORMAL = 0x00008000
NORMAL = 0x00000020


class AliveCache:
    """`check(pid, ctime)` -> alive, remembering each answer for `ttl` seconds. A stale
    "alive" only ever makes a waiter more conservative for at most `ttl`; the grant itself is
    re-verified under the Mutex with a cache of the same short life."""

    def __init__(self, probe, ttl: float = 1.5, clock=time.monotonic):
        self._probe = probe
        self.ttl = ttl
        self._clock = clock
        self._seen: dict[tuple, tuple[float, bool]] = {}
        self.probes = 0

    def check(self, pid, ctime=None) -> bool:
        key = (pid, ctime)
        now = self._clock()
        hit = self._seen.get(key)
        if hit is not None and now - hit[0] < self.ttl:
            return hit[1]
        self.probes += 1
        alive = bool(self._probe(pid, ctime))
        if len(self._seen) > 512:
            self._seen = {k: v for k, v in self._seen.items() if now - v[0] < self.ttl}
        self._seen[key] = (now, alive)
        return alive

    def clear(self) -> None:
        self._seen.clear()


class Backoff:
    """Poll interval growing from `base` to `cap` by x1.5 per unchanged poll, +-20% jitter.
    `reset()` goes back to `base` (queue position improved; the head stays at base)."""

    def __init__(self, base: float, cap: float, rng=random.random):
        self.base = base
        self.cap = max(cap, base)
        self._cur = base
        self._rng = rng

    def reset(self) -> None:
        self._cur = self.base

    def next(self, head: bool = False) -> float:
        cur = self.base if head else self._cur
        if not head:
            self._cur = min(self.cap, self._cur * 1.5)
        return cur * (0.8 + 0.4 * self._rng())


def _get_priority():
    import ctypes

    k32 = ctypes.windll.kernel32
    k32.GetCurrentProcess.restype = ctypes.c_void_p
    k32.GetPriorityClass.argtypes = [ctypes.c_void_p]
    return int(k32.GetPriorityClass(k32.GetCurrentProcess()))


def _set_priority(cls: int) -> bool:
    import ctypes

    k32 = ctypes.windll.kernel32
    k32.GetCurrentProcess.restype = ctypes.c_void_p
    k32.SetPriorityClass.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
    return bool(k32.SetPriorityClass(k32.GetCurrentProcess(), cls))


def boost():
    """Raise this process to ABOVE_NORMAL; returns the previous class (the restore token) or
    None when nothing was changed (not Windows, disabled, already non-normal, or a failure)."""
    if not _IS_WIN or os.environ.get("NUCLEOS_HEAVY_BOOST") == "0":
        return None
    try:
        prev = _get_priority()
        if prev != NORMAL:
            return None  # an operator or parent chose a class: leave it alone
        return prev if _set_priority(ABOVE_NORMAL) else None
    except Exception:
        return None


def restore(token) -> None:
    if token is None:
        return
    try:
        _set_priority(token)
    except Exception:
        pass


class Steps:
    """perf_counter marks: `mark(name)` stores the seconds since the previous mark."""

    def __init__(self):
        self._last = time.perf_counter()
        self.data: dict[str, float] = {}

    def mark(self, name: str) -> None:
        now = time.perf_counter()
        self.data[name] = round(self.data.get(name, 0.0) + (now - self._last), 4)
        self._last = now
