import { createElement, type ReactNode } from "react";
import { QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const client = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  apiFetch: client.apiFetch,
}));

import { createAppQueryClient } from "../app/queryClient";
import {
  ceilingCalls,
  deanonymise,
  seatName,
  useCouncilConfig,
  type CouncilConfig,
  type SeatView,
} from "./council";
import { keys } from "./keys";

beforeEach(() => {
  client.apiFetch.mockReset();
});

function seat(overrides: Partial<SeatView> = {}): SeatView {
  return {
    seat_idx: 0,
    kind: "cloud",
    ref: "claude-opus-4",
    agent_id: null,
    agent_name: null,
    role: null,
    steps: [],
    ...overrides,
  };
}

/* ------------------------------------------------------------ ceilingCalls -- */

describe("ceilingCalls", () => {
  it("counts answers, critiques, revisions between rounds, and two chairman calls", () => {
    // N answers, R*N critiques, (R-1)*N revisions — the last round's critique is
    // not followed by a revision — plus the chairman's two calls. The form shows
    // this as the most a question can cost, so an off-by-one here is a promise
    // the page breaks on every council it convenes.
    expect(ceilingCalls(3, 1)).toBe(3 + 3 + 0 + 2);
    expect(ceilingCalls(3, 2)).toBe(3 + 6 + 3 + 2);
    expect(ceilingCalls(5, 3)).toBe(5 + 15 + 10 + 2);
  });

  it("grows with the members as well as the rounds", () => {
    expect(ceilingCalls(2, 2)).toBe(2 + 4 + 2 + 2);
    expect(ceilingCalls(8, 2)).toBeGreaterThan(ceilingCalls(2, 2));
  });
});

/* ------------------------------------------------------------- deanonymise -- */

describe("deanonymise", () => {
  it("maps every anonymous label back to the seat it stood for, in the order given", () => {
    const anon = { A: 2, B: 0, C: 1 };
    expect(deanonymise(["C", "A", "B"], anon)).toEqual([1, 2, 0]);
  });

  it("answers null for a label that names no seat, rather than dropping it", () => {
    // Dropping would shift every later position up one, so a ballot that named
    // a stray label would read as ranking the wrong seats.
    expect(deanonymise(["A", "Z"], { A: 3 })).toEqual([3, null]);
  });
});

/* ---------------------------------------------------------------- seatName -- */

describe("seatName", () => {
  it("is the agent's name when an agent sat", () => {
    expect(seatName(seat({ agent_id: "ag-1", agent_name: "the sceptic" }))).toBe("the sceptic");
  });

  it("is the model ref when the roster named a model", () => {
    expect(seatName(seat())).toBe("claude-opus-4");
  });

  it("falls back to the ref when the agent has been deleted", () => {
    expect(seatName(seat({ agent_id: "ag-1", agent_name: null }))).toBe("claude-opus-4");
  });

  it("appends the role the seat played", () => {
    const named = seatName(seat({ agent_id: "ag-1", agent_name: "the sceptic", role: "skeptic" }));
    expect(named.startsWith("the sceptic")).toBe(true);
    expect(named).toContain("skeptic");
    expect(named.length).toBeGreaterThan("the sceptic".length);
  });
});

/* ------------------------------------------------------- useCouncilConfig -- */

describe("useCouncilConfig", () => {
  it("reads GET /council/config under the council root's config key", async () => {
    const config: CouncilConfig = {
      configured: true,
      default_rounds: 2,
      max_rounds: 3,
      roles: ["skeptic", "pragmatist"],
      default_roster: null,
    };
    client.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/council/config") return config;
      throw new Error(`unexpected path ${path}`);
    });
    const queryClient = createAppQueryClient();
    const wrapper = ({ children }: { children: ReactNode }) =>
      createElement(QueryClientProvider, { client: queryClient }, children);

    const { result } = renderHook(() => useCouncilConfig(), { wrapper });

    await waitFor(() => expect(result.current.data).toEqual(config));
    expect(client.apiFetch.mock.calls.map((call) => call[0])).toContain("/council/config");
    expect(queryClient.getQueryData([...keys.council.all, "config"])).toEqual(config);
  });
});
