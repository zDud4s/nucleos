import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import {
  Outlet,
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Council } from "./Council";
import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal } from "../data/client";
import type { CouncilSummary, CouncilView, SeatView } from "../data/council";
import { keys } from "../data/keys";
import { POLL } from "../data/poll";
import { daemonFetch, daemonState, renderApp } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

/* ------------------------------------------------------------ fixtures -- */

function councilSummary(overrides: Partial<CouncilSummary> = {}): CouncilSummary {
  return {
    id: "c-1",
    created_at: "2026-08-18T09:00:00Z",
    question: "should we ship the frontend rewrite?",
    status: "running",
    stage: 1,
    ...overrides,
  };
}

function seatView(overrides: Partial<SeatView> = {}): SeatView {
  return {
    seat_idx: 0,
    kind: "cloud",
    ref: "claude-opus-4",
    stage1_status: "ok",
    stage1_error: null,
    answer: "yes, ship it — the tests carry the proof",
    stage2_status: "ok",
    stage2_error: null,
    rankings: [{ anon: "B", rank: 1 }],
    ...overrides,
  };
}

function councilView(overrides: Partial<CouncilView> = {}): CouncilView {
  return {
    id: "c-1",
    created_at: "2026-08-18T09:00:00Z",
    question: "should we ship the frontend rewrite?",
    status: "running",
    stage: 2,
    error: null,
    chairman_kind: "cloud",
    chairman_ref: "claude-opus-4",
    synthesis: null,
    anon_map: { A: 0, B: 1 },
    leaderboard: [],
    seats: [seatView()],
    ...overrides,
  };
}

/**
 * The council routes, over mutable state — the same shape `chatsFetch` in
 * `Chats.test.tsx` uses, for the same reason: the shell polls, so a queue of
 * one-shot answers runs out halfway through the second tick.
 */
function councilFetch(
  summaries: CouncilSummary[],
  views: Record<string, CouncilView>,
  opts: { onCreate?: () => unknown; onCancel?: (id: string) => unknown } = {},
): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path, init) => {
    if (path === "/council" && init?.method === "POST") {
      if (opts.onCreate !== undefined) return opts.onCreate();
      return { id: "new-1" };
    }
    if (path === "/council") return summaries;
    const cancelMatch = /^\/council\/([^/]+)\/cancel$/.exec(path);
    if (cancelMatch !== null && init?.method === "POST") {
      if (opts.onCancel !== undefined) return opts.onCancel(cancelMatch[1]);
      return { cancelled: true };
    }
    const detailMatch = /^\/council\/([^/]+)$/.exec(path);
    if (detailMatch !== null) {
      const view = views[detailMatch[1]];
      if (view === undefined) throw new ApiRefusal(404, "not_found", "no such council");
      return view;
    }
    return undefined;
  };
}

/**
 * The page inside a two-route router, exactly like `Chats.test.tsx`'s
 * `renderChats`: `renderApp` mounts the gate, the rail and its own live
 * queries around every assertion, which this machine cannot pay for more than
 * once. Only the case that proves the real tree registers both routes uses it,
 * at the end of this file.
 */
async function renderCouncil(initialPath: string) {
  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({ getParentRoute: () => rootRoute, path: "/council", component: Council }),
    createRoute({ getParentRoute: () => rootRoute, path: "/council/$councilId", component: Council }),
  ];
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
    defaultPreload: false,
  });

  await router.load();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { ...result, router, queryClient };
}

/** The `<section>` a panel's own heading belongs to, so an assertion can be scoped to one card. */
async function panelFor(headingText: string): Promise<HTMLElement> {
  const heading = await screen.findByRole("heading", { level: 2, name: headingText });
  const panel = heading.closest("section");
  if (panel === null) throw new Error(`no panel section found for heading "${headingText}"`);
  return panel as HTMLElement;
}

/* --------------------------------------------------------- A10: abstained -- */

describe("Council - a seat that abstained", () => {
  it("reads a stage-2 ok seat with no rankings as an abstention, not a failure (A10)", async () => {
    const view = councilView({
      seats: [seatView({ stage1_status: "ok", stage2_status: "ok", rankings: [] })],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    const abstained = await screen.findByText("abstained");
    const seatCard = abstained.closest("li");
    if (seatCard === null) throw new Error("no seat card found");
    // Both stages read "answered" — a blank vote is a real answer to stage 2,
    // not a gap where an outcome should be.
    expect(within(seatCard as HTMLElement).getAllByText("answered")).toHaveLength(2);
    expect(within(seatCard as HTMLElement).queryByText(/fail/i)).toBeNull();
  });
});

/* --------------------------------------------------------- A11: expired -- */

describe("Council - an expired answer", () => {
  it("reads a stage-1 ok seat with a null answer as answered, not empty or failed (A11)", async () => {
    const view = councilView({
      seats: [seatView({ stage1_status: "ok", answer: null })],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    expect(await screen.findByText("answered — the text has expired")).toBeDefined();
  });
});

/* ---------------------------------------------------- A12: convene refusals -- */

describe("Council - refusals convening meets", () => {
  it("carries the daemon's own sentence on a 503 naming the missing roster", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, {
        onCreate: () => {
          throw new ApiRefusal(
            503,
            "unavailable",
            "no council is configured — write a roster to ~/.nucleos/council.yaml and restart",
          );
        },
      }),
    );

    await renderCouncil("/council");
    fireEvent.change(await screen.findByLabelText("Question"), { target: { value: "well?" } });
    fireEvent.click(screen.getByRole("button", { name: "Convene" }));

    // The whole sentence, path included. This page passes the daemon's prose through untouched,
    // so a refusal that names the wrong file is a refusal that sends somebody to the wrong file —
    // and matching only the first clause is what would let that ship.
    expect(
      await screen.findByText(/write a roster to ~\/\.nucleos\/council\.yaml and restart/),
    ).toBeDefined();
  });

  it("tells the reader where the roster lives, and it is not inside the repository", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}));

    await renderCouncil("/council");

    // The convene form's own copy, not the daemon's. It is a second place the path is written,
    // and the two saying different things is worse than either being wrong alone.
    const note = await screen.findByText(/One question, put to every seat in/);
    expect(note.textContent).toContain("~/.nucleos/council.yaml");
    expect(note.textContent).not.toContain(".ai/");
  });

  it("carries the budget sentence naming the limit and the spend on a 429", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, {
        onCreate: () => {
          throw new ApiRefusal(
            429,
            "too_many_requests",
            "the council budget of $10.00 today is exhausted; $10.42 has already been spent",
          );
        },
      }),
    );

    await renderCouncil("/council");
    fireEvent.change(await screen.findByLabelText("Question"), { target: { value: "well?" } });
    fireEvent.click(screen.getByRole("button", { name: "Convene" }));

    expect(await screen.findByText(/\$10\.00/)).toBeDefined();
    expect(screen.getByText(/already been spent/i)).toBeDefined();
  });

  it("carries the daemon's own sentence on a 400", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, {
        onCreate: () => {
          throw new ApiRefusal(400, "bad_request", "the question must not be empty or only whitespace");
        },
      }),
    );

    await renderCouncil("/council");
    fireEvent.change(await screen.findByLabelText("Question"), { target: { value: "well?" } });
    fireEvent.click(screen.getByRole("button", { name: "Convene" }));

    expect(await screen.findByText(/must not be empty/i)).toBeDefined();
  });
});

/* ------------------------------------------------------------- A13: cadence -- */

describe("Council - the detail's cadence", () => {
  it("asks again at 2s while running and stops on the tick that lands done", async () => {
    const summaries = [councilSummary({ status: "done" })];
    const views: Record<string, CouncilView> = { "c-1": councilView({ status: "running" }) };
    daemon.apiFetch.mockImplementation(councilFetch(summaries, views));

    const { queryClient } = await renderCouncil("/council/c-1");
    // Scoped to the detail panel: the list row for this same council can read
    // a different status than the one under test, and a bare `findByText`
    // would not know which badge it found.
    expect(within(await panelFor("This council")).getByText("deliberating")).toBeDefined();

    function computedInterval(): number | false {
      const query = queryClient
        .getQueryCache()
        .find({ queryKey: [...keys.council.all, "detail", "c-1"], exact: true });
      if (query === undefined) throw new Error("no cached query for c-1");
      // `refetchInterval` lives on `QueryObserverOptions`, not on the narrower
      // `QueryOptions` type `Query.options` is typed as — react-query stores
      // the observer's full options on the cached query at runtime, but the
      // type of `.options` does not say so.
      const option = (
        query.options as {
          refetchInterval?: number | false | ((q: typeof query) => number | false | undefined);
        }
      ).refetchInterval;
      if (typeof option !== "function") throw new Error("refetchInterval is not a function here");
      return option(query) ?? false;
    }

    expect(computedInterval()).toBe(POLL.council);

    views["c-1"] = councilView({ status: "done", stage: 3, synthesis: "the seats agree." });
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: [...keys.council.all, "detail", "c-1"] });
    });
    expect(within(await panelFor("This council")).getByText("settled")).toBeDefined();

    expect(computedInterval()).toBe(false);
  });
});

/* --------------------------------------------------- A14: chairman failed -- */

describe("Council - a chairman that failed", () => {
  it("still renders the seats and the leaderboard when synthesis is null and error is set", async () => {
    const view = councilView({
      status: "error",
      stage: 3,
      error: "the chairman's run ended in timeout",
      synthesis: null,
      seats: [
        seatView({ seat_idx: 0 }),
        seatView({ seat_idx: 1, kind: "local", ref: "local-model", rankings: [] }),
      ],
      leaderboard: [{ seat_idx: 0, avg_rank: 1.5, n: 2 }],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary({ status: "error" })], { "c-1": view }));

    await renderCouncil("/council/c-1");

    expect(await screen.findByRole("list", { name: "Seats" })).toBeDefined();
    expect(await screen.findByRole("list", { name: "Leaderboard" })).toBeDefined();
    expect(await screen.findByText(/chairman failed to write a synthesis/i)).toBeDefined();
    expect(screen.getByText(/ended in timeout/)).toBeDefined();
  });
});

/* ------------------------------------------------------------- the route -- */

describe("Council - the route and the list", () => {
  it("is registered for both paths, and the detail is reachable from the list", async () => {
    const summaries = [councilSummary({ id: "c-1" })];
    const views: Record<string, CouncilView> = { "c-1": councilView({ id: "c-1" }) };
    const shared = daemonFetch(daemonState());
    const council = councilFetch(summaries, views);
    daemon.apiFetch.mockImplementation(async (path, init) => {
      if (path === "/council" || path.startsWith("/council/")) return council(path, init);
      return await shared(path, init);
    });

    // The whole app here, and only here: a locally built router would prove
    // nothing about whether `/council` is in the real tree.
    const { router } = await renderApp({ initialPath: "/council" });

    expect(await screen.findByRole("heading", { level: 1, name: "Council" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/council");
    expect(screen.queryByText("Council is not built yet")).toBeNull();

    fireEvent.click(await screen.findByRole("link", { name: /should we ship the frontend rewrite\?/ }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/council/c-1"));
    expect(await screen.findByRole("heading", { level: 1, name: "Council" })).toBeDefined();
  });
});
