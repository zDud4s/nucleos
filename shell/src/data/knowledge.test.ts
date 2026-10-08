import { renderHook, waitFor } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: daemon.apiFetch,
}));

import { createAppQueryClient } from "../app/queryClient";
import { keys } from "./keys";
import {
  distilledOrigin,
  groupWaiting,
  measuredByGenerator,
  parseEvidence,
  useApproveKnowledge,
  useDecideKnowledgeBatch,
  useRejectKnowledge,
  useRevertKnowledge,
  type Known,
} from "./knowledge";

function known(over: Partial<Known> = {}): Known {
  return {
    id: 1,
    layer: "episodic",
    scope_kind: "project",
    scope_id: "alpha",
    source: "consolidator",
    generator: "gate",
    kind: "memory",
    title: "A measured fact",
    body: "The gate observed it.",
    status: "active",
    proposal_id: null,
    supersedes: null,
    origin_run_id: 900001,
    evidence: null,
    observations: 1,
    fingerprint: null,
    expires_after_runs: null,
    last_confirmed_at: null,
    shown_count: 0,
    outcome_count: 0,
    green_count: 0,
    last_shown_at: null,
    created_at: "2026-09-20T09:00:00+00:00",
    activated_at: "2026-09-20T09:05:00+00:00",
    ended_at: null,
    ...over,
  };
}

describe("parseEvidence", () => {
  it("reads a malformed value as absent and drops elements it cannot name", () => {
    expect(parseEvidence(null)).toEqual([]);
    expect(parseEvidence("not json")).toEqual([]);
    expect(parseEvidence("{}")).toEqual([]);
    expect(
      parseEvidence(
        JSON.stringify([
          { t: "run", id: 1 },
          { t: "nope", id: 2 },
          { t: "run" },
        ]),
      ),
    ).toEqual([{ t: "run", id: 1 }]);
  });

  it("an evidence of only unknown tags counts as empty", () => {
    expect(parseEvidence(JSON.stringify([{ t: "mystery", id: 7 }]))).toEqual([]);
  });

  it("parseEvidence keeps a job reference", () => {
    expect(parseEvidence('[{"t":"job","id":7},{"t":"run","id":3}]')).toEqual([
      { t: "job", id: 7 },
      { t: "run", id: 3 },
    ]);
  });
});

describe("distilledOrigin", () => {
  it("distilledOrigin names the job, the runs and the cause", () => {
    const row = known({
      id: 5,
      source: "distiller",
      evidence: '[{"t":"job","id":7},{"t":"run","id":3},{"t":"run","id":4}]',
    });

    expect(distilledOrigin(row, new Map([[5, "job_failed"]]))).toEqual({
      job: 7,
      runs: [3, 4],
      cause: "job_failed",
      causeLabel: "the job failed",
    });
    // A cause this build does not know is shown as it came, not hidden.
    expect(distilledOrigin(row, new Map([[5, "something_new"]]))).toMatchObject({
      cause: "something_new",
      causeLabel: "something_new",
    });
    // No cause on record: the origin still names its job.
    expect(distilledOrigin(row, new Map())).toMatchObject({
      job: 7,
      cause: null,
      causeLabel: null,
    });
  });

  it("distilledOrigin is null for a row the distiller did not write", () => {
    const row = known({
      id: 5,
      source: "run",
      evidence: '[{"t":"job","id":7}]',
    });

    expect(distilledOrigin(row, new Map([[5, "job_failed"]]))).toBeNull();
  });
});

describe("measuredByGenerator", () => {
  it("groups active measured rows by scope and generator and ignores the rest", () => {
    const rows = [
      known({ id: 1, generator: "gate" }),
      known({ id: 2, generator: "gate" }),
      known({ id: 3, generator: "refused-action" }),
      known({ id: 4, scope_kind: "machine", scope_id: null, generator: null }),
      known({ id: 5, source: "owner" }),
      known({ id: 6, status: "proposed" }),
      known({ id: 7, layer: "semantic" }),
    ];

    expect(measuredByGenerator(rows)).toEqual([
      {
        scope: "alpha",
        total: 3,
        byGenerator: [
          { generator: "gate", count: 2 },
          { generator: "refused-action", count: 1 },
        ],
      },
      {
        scope: "this machine",
        total: 1,
        byGenerator: [{ generator: "unknown", count: 1 }],
      },
    ]);
  });
});

describe("groupWaiting", () => {
  it("separates scopes and sources, orders groups by their first row and leaves undecidable rows out of proposalIds", () => {
    const rows = [
      known({ id: 8, status: "proposed", scope_id: "beta", source: "run", proposal_id: 80 }),
      known({ id: 2, status: "proposed", scope_id: "alpha", source: "owner", proposal_id: 20 }),
      known({ id: 6, status: "active", scope_id: "alpha", source: "owner", proposal_id: 60 }),
      known({ id: 4, status: "proposed", scope_id: "alpha", source: "run", proposal_id: 40 }),
      known({ id: 3, status: "proposed", scope_id: "alpha", source: "owner", proposal_id: null }),
      known({
        id: 5,
        status: "proposed",
        scope_kind: "machine",
        scope_id: null,
        source: "owner",
        proposal_id: 50,
      }),
    ];

    expect(groupWaiting(rows)).toEqual([
      {
        key: "project|alpha|owner",
        scope: "alpha",
        source: "owner",
        rows: [rows[1], rows[4]],
        proposalIds: [20],
      },
      {
        key: "project|alpha|run",
        scope: "alpha",
        source: "run",
        rows: [rows[3]],
        proposalIds: [40],
      },
      {
        key: "machine||owner",
        scope: "this machine",
        source: "owner",
        rows: [rows[5]],
        proposalIds: [50],
      },
      {
        key: "project|beta|run",
        scope: "beta",
        source: "run",
        rows: [rows[0]],
        proposalIds: [80],
      },
    ]);
  });
});

describe("knowledge decisions", () => {
  // A decision can change a note's standing in the Brain graph (a taught note
  // links to the row it produced), so every door also refreshes the notes.
  // The four hooks differ in result type; the test only needs `mutate` and `isSuccess`.
  type AnyMutation = { mutate: (value: unknown) => void; isSuccess: boolean };
  const cases: [string, () => AnyMutation, unknown][] = [
    ["approve", () => useApproveKnowledge() as unknown as AnyMutation, 5],
    ["reject", () => useRejectKnowledge() as unknown as AnyMutation, 5],
    [
      "batch",
      () => useDecideKnowledgeBatch() as unknown as AnyMutation,
      { action: "approve" as const, proposalIds: [5] },
    ],
    ["revert", () => useRevertKnowledge() as unknown as AnyMutation, 9],
  ];
  it.each(cases)("%s invalidates knowledge, proposals and owner notes", async (_name, hook, input) => {
    daemon.apiFetch.mockReset();
    daemon.apiFetch.mockResolvedValue(undefined);
    const client = createAppQueryClient();
    const wrapper = ({ children }: { children: ReactNode }) =>
      createElement(QueryClientProvider, { client }, children);
    const invalidate = vi.spyOn(client, "invalidateQueries");
    const { result } = renderHook(hook, { wrapper });

    result.current.mutate(input);

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(invalidate).toHaveBeenCalledWith({ queryKey: keys.knowledge.all });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: keys.proposals.all });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: keys.ownerNotes.all });
  });
});
