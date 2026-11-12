import { createElement, type ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("./client", async (original) => ({
  ...(await original<typeof import("./client")>()),
  ...daemon,
}));

import { teamRunQuery, useLiveTeamRuns } from "./teams";
import { keys } from "./keys";
import { createAppQueryClient } from "../app/queryClient";

/**
 * The fan-out the Roster tab needs, and the one definition both of its readers share.
 *
 * The key is asserted against `keys.teams.run` rather than against a literal, and that is the
 * whole point of the factory: `useTeamRun` and `useLiveTeamRuns` must not be able to drift apart,
 * and a literal here would keep passing while they did.
 *
 * `createElement` rather than JSX, so this file stays a `.ts` beside the rest of the pillar —
 * `data/project-policy.test.ts` mounts its wrapper the same way and for the same reason.
 */

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
});

function wrapper({ children }: { children: ReactNode }) {
  return createElement(QueryClientProvider, { client: createAppQueryClient() }, children);
}

describe("teamRunQuery", () => {
  it("keys a run the same way the single reader does", () => {
    expect(teamRunQuery("r1").queryKey).toEqual(keys.teams.run("r1"));
  });

  it("encodes an id that would otherwise break the path", () => {
    const query = teamRunQuery("a/b");
    void query.queryFn();

    expect(daemon.apiFetch).toHaveBeenCalledWith("/team-runs/a%2Fb");
  });
});

describe("useLiveTeamRuns", () => {
  it("reads every live run and hands them back in the order asked", async () => {
    daemon.apiFetch.mockImplementation((path: string) =>
      Promise.resolve({ id: decodeURIComponent(path.split("/").pop() as string), items: [] }),
    );

    const { result } = renderHook(() => useLiveTeamRuns(["r1", "r2"]), { wrapper });

    await waitFor(() => expect(result.current.runs).toHaveLength(2));
    expect(result.current.runs.map((run) => run.id)).toEqual(["r1", "r2"]);
    expect(daemon.apiFetch).toHaveBeenCalledTimes(2);
  });

  it("is pending until every run has answered, so the graph draws once", async () => {
    let answerSecond: ((value: unknown) => void) | undefined;
    daemon.apiFetch.mockImplementation((path: string) =>
      path.endsWith("r1")
        ? Promise.resolve({ id: "r1", items: [] })
        : new Promise((resolve) => {
            answerSecond = resolve;
          }),
    );

    const { result } = renderHook(() => useLiveTeamRuns(["r1", "r2"]), { wrapper });

    await waitFor(() => expect(result.current.runs).toHaveLength(1));
    expect(result.current.pending).toBe(true);

    answerSecond?.({ id: "r2", items: [] });
    await waitFor(() => expect(result.current.pending).toBe(false));
  });

  it("asks the daemon nothing when no run is alive", () => {
    const { result } = renderHook(() => useLiveTeamRuns([]), { wrapper });

    expect(result.current.runs).toEqual([]);
    expect(result.current.pending).toBe(false);
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });
});
