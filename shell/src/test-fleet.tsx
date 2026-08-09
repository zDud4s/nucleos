import { act } from "@testing-library/react";
import { vi } from "vitest";

import type { Concurrency, Job, ProjectConcurrency, RunSearchResult } from "./api";

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
 */
export function respondWith(answers: Record<string, unknown>) {
  fetchMock.mockImplementation(async (url: string) => {
    const key = Object.keys(answers).find((fragment) => url.includes(fragment));
    const body = key === undefined ? [] : answers[key];
    if (body === null) return { ok: false, status: 500, json: async () => ({}) };
    return { ok: true, status: 200, json: async () => body };
  });
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
