import { act } from "@testing-library/react";
import { vi } from "vitest";

import type {
  Concurrency,
  Job,
  JobDetail,
  JobItem,
  ProjectConcurrency,
  RunSearchResult,
} from "./api";

/**
 * The harness both fleet test files share.
 *
 * Extracted rather than copied: two copies of `job()` would diverge at the first field `Job` gains
 * after this, and the divergence would show up as a test that passes against a shape the daemon no
 * longer sends.
 */
export const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

/** Timer callbacks touch React state, so they go inside an `act()`. */
export function advance(ms: number) {
  act(() => {
    vi.advanceTimersByTime(ms);
  });
}

export async function settle(rounds = 4) {
  for (let round = 0; round < rounds; round += 1) await act(async () => {});
}

/**
 * Dispatch by URL, in the shape `Approvals.test.tsx` uses.
 *
 * `answers` maps a URL fragment to a body, or to `null`, which becomes a 500. Anything unmatched
 * yields `[]` — so a test does not have to describe calls it is not talking about.
 *
 * A key may name a method — `"POST /fleet/exclusions"` — and a method-qualified key is tried before
 * a bare one. Without that, one route reached by two verbs (asking for an exclusion and listing
 * them) would answer both with whichever key happened to be declared first.
 *
 * `text()` is answered as well as `json()`, on both branches: the daemon writes its refusals as
 * plain text and several callers read them, so a mock that only spoke JSON would send them all down
 * their catch arm and report an unreachable daemon.
 */
export function respondWith(answers: Record<string, unknown>) {
  fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
    const method = (init?.method ?? "GET").toUpperCase();
    // Longest match wins among the bare keys, so `/fleet/exclusions` does not swallow
    // `/fleet/exclusions/requests` — one is a prefix of the other, and declaration order deciding
    // which route answers is a trap a reader would spend an afternoon on.
    const keys = [...Object.keys(answers)].sort((left, right) => right.length - left.length);
    const key =
      keys.find((fragment) => qualified(fragment, url, method)) ??
      keys.find((fragment) => !fragment.includes(" ") && url.includes(fragment));
    const body = key === undefined ? [] : answers[key];
    if (body === null) {
      return { ok: false, status: 500, json: async () => ({}), text: async () => "" };
    }
    return {
      ok: true,
      status: 200,
      json: async () => body,
      text: async () => JSON.stringify(body),
    };
  });
}

function qualified(fragment: string, url: string, method: string): boolean {
  const [head, rest] = fragment.split(" ");
  return rest !== undefined && head.toUpperCase() === method && url.includes(rest);
}

export const CONCURRENCY = "/concurrency";
export const JOBS = "/jobs?live=true";
export const RUNS = "/runs?live=true";

export function column(over: Partial<ProjectConcurrency> = {}): ProjectConcurrency {
  return {
    project_id: "alpha",
    limit: 2,
    slots: [
      {
        project_id: "alpha",
        slot: 0,
        owner_kind: "job",
        owner_id: 41,
        claimed_at: "2026-08-09T00:00:00Z",
      },
    ],
    collision: {
      declared: { state: "not_measured", overlaps: [] },
      observed: { state: "clean", overlaps: [] },
    },
    ...over,
  };
}

export function readout(projects: ProjectConcurrency[]): Concurrency {
  const held = projects.reduce((total, project) => total + project.slots.length, 0);
  return { house: { limit: 3, held }, projects };
}

export function job(over: Partial<Job> = {}): Job {
  return {
    id: 41,
    project_id: "alpha",
    rule_name: null,
    status: "implementing",
    wait_reason: null,
    max_items: 5,
    slot: 0,
    round: 0,
    max_rounds: 1,
    created_at: "2026-08-09T00:00:00Z",
    completed_at: null,
    ...over,
  };
}

export function item(over: Partial<JobItem> = {}): JobItem {
  return {
    ordinal: 0,
    description: "an item",
    status: "pending",
    round: 0,
    run_id: null,
    gate_status: null,
    ...over,
  };
}

/**
 * A job of two rounds with all three gate outcomes represented.
 *
 * `round: 1, max_rounds: 2` and not `job()`'s `0`/`1`: rounds are counted from zero, and a job is
 * retired as soon as `round + 1 >= max_rounds`. A job at `round: 0, max_rounds: 1` can **never**
 * have round-1 items — it would be a fixture describing a state the daemon cannot emit.
 */
export function detail(over: Partial<JobDetail> = {}): JobDetail {
  return {
    ...job({ round: 1, max_rounds: 2 }),
    branch: "nucleos/job-41",
    items: [
      item({ ordinal: 0, status: "passed", gate_status: "passed", run_id: 1 }),
      item({ ordinal: 1, status: "passed", gate_status: null, run_id: 2 }),
      item({ ordinal: 2, status: "gate_failed", gate_status: "failed", run_id: 3 }),
      item({ ordinal: 3, status: "gate_errored", gate_status: "errored", run_id: 4 }),
      item({ ordinal: 4, status: "skipped", round: 1, run_id: 5 }),
      item({ ordinal: 5, status: "running", round: 1, run_id: 6 }),
    ],
    ...over,
  };
}

export function run(over: Partial<RunSearchResult> = {}): RunSearchResult {
  return {
    id: 7,
    project_id: "alpha",
    status: "awaiting_approval",
    mode: "worktree",
    created_at: "2026-08-09T00:00:00Z",
    completed_at: null,
    cost_usd: null,
    prompt_excerpt: "a prompt",
    ...over,
  };
}
