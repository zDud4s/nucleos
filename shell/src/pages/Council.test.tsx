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
import type {
  CouncilConfig,
  CouncilSummary,
  CouncilView,
  Critique,
  SeatView,
  StepView,
  Synthesis,
} from "../data/council";
import type { RunTailChunk } from "../data/runs";
import { keys } from "../data/keys";
import { POLL } from "../data/poll";
import { readState } from "../ui/state-map";
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
    rounds: 1,
    rounds_run: 0,
    current_round: 0,
    current_phase: "answer",
    ...overrides,
  };
}

/** One step of one seat. The default is the round-0 answer, settled. */
function stepView(overrides: Partial<StepView> = {}): StepView {
  return {
    round: 0,
    phase: "answer",
    run_id: 1,
    status: "ok",
    error: null,
    answer: "yes, ship it — the tests carry the proof",
    critique: null,
    changed: null,
    why: null,
    ...overrides,
  };
}

function seatView(overrides: Partial<SeatView> = {}): SeatView {
  return {
    seat_idx: 0,
    kind: "cloud",
    ref: "claude-opus-4",
    agent_id: null,
    agent_name: null,
    role: null,
    steps: [stepView()],
    ...overrides,
  };
}

function councilView(overrides: Partial<CouncilView> = {}): CouncilView {
  return {
    id: "c-1",
    created_at: "2026-08-18T09:00:00Z",
    question: "should we ship the frontend rewrite?",
    status: "running",
    rounds: 1,
    rounds_run: 0,
    stopped_early: false,
    current_round: 1,
    current_phase: "critique",
    error: null,
    chairman_kind: "cloud",
    chairman_ref: "claude-opus-4",
    chairman_agent_id: null,
    chairman_agent_name: null,
    agreement: null,
    leaderboard: [],
    leaderboard_by_round: [],
    synthesis: null,
    synthesis_structured: null,
    synthesis_status: null,
    anon_map: { A: 0, B: 1 },
    seats: [seatView()],
    ...overrides,
  };
}

/** `GET /council/config` — a configured roster, two rounds by default. */
function councilConfig(overrides: Partial<CouncilConfig> = {}): CouncilConfig {
  return {
    configured: true,
    default_rounds: 2,
    max_rounds: 3,
    roles: ["skeptic", "pragmatist"],
    default_roster: null,
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
  opts: {
    /** Handed the PARSED request body: the roster tests assert on what travelled. */
    onCreate?: (body: unknown) => unknown;
    onCancel?: (id: string) => unknown;
    /** `GET /agents`, read only while the roster control is open. */
    agents?: unknown[];
    /** `GET /assistant/models`, likewise. */
    models?: unknown[];
    /** `GET /council/config`. */
    config?: CouncilConfig;
    /** `GET /runs/<id>/tail?since=<n>`; `undefined` is the daemon's 204. Handed the run and the offset. */
    tail?: (runId: number, since: number) => RunTailChunk | undefined;
  } = {},
): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path, init) => {
    if (path === "/agents") return opts.agents ?? [];
    if (path === "/assistant/models") {
      return { choices: opts.models ?? [], configured: "claude-opus-5", efforts: [] };
    }
    if (path === "/council" && init?.method === "POST") {
      const body: unknown = JSON.parse(String(init.body ?? "null"));
      if (opts.onCreate !== undefined) return opts.onCreate(body);
      return { id: "new-1" };
    }
    if (path === "/council") return summaries;
    // BEFORE the detail regex: `/council/config` matches `^/council/([^/]+)$`
    // too, and answered there it would be a 404 for a council called "config".
    if (path === "/council/config") return opts.config ?? councilConfig();
    const tailMatch = /^\/runs\/(\d+)\/tail\?since=(\d+)$/.exec(path);
    if (tailMatch !== null) {
      return opts.tail === undefined ? undefined : opts.tail(Number(tailMatch[1]), Number(tailMatch[2]));
    }
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

/* --------------------------------------------------------- empty catalogue -- */

describe("Council - the empty catalogue", () => {
  it("an empty catalogue is the teach block alone", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}));

    await renderCouncil("/council");

    expect(await screen.findByRole("heading", { level: 3, name: "Choose a council" })).toBeDefined();
    expect(screen.queryByRole("region", { name: "Councils" })).toBeNull();
  });

  it("explains the three phases once", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}));

    await renderCouncil("/council");

    expect(await screen.findAllByText(/ranks the others blind/)).toHaveLength(1);
  });
});

/* --------------------------------------------------------- A10: abstained -- */

describe("Council - a seat that abstained", () => {
  it("reads an ok critique that ranked nobody as an abstention, not a failure (A10)", async () => {
    const view = councilView({
      seats: [
        seatView({
          steps: [
            stepView(),
            stepView({
              round: 1,
              phase: "critique",
              run_id: 2,
              answer: null,
              critique: { reviews: [], ranking: [] },
            }),
          ],
        }),
      ],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    const abstained = await screen.findByText("abstained");
    const seatCard = abstained.closest("li");
    if (seatCard === null) throw new Error("no seat card found");
    // A blank vote is a real answer to a critique round, not a gap where an
    // outcome should be.
    expect(within(seatCard as HTMLElement).queryByText(/fail/i)).toBeNull();
  });
});

/* --------------------------------------------------------- A11: expired -- */

describe("Council - an expired answer", () => {
  it("reads an ok round-0 answer with no text as answered, not empty or failed (A11)", async () => {
    const view = councilView({
      seats: [seatView({ steps: [stepView({ status: "ok", answer: null })] })],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    expect(await screen.findByText("answered — the text has expired")).toBeDefined();
  });
});

/* ------------------------------------------------ the answer, as rich text -- */

describe("Council - a seat's answer", () => {
  it("renders its markdown as rich text, and never as HTML", async () => {
    const view = councilView({
      seats: [
        seatView({
          steps: [stepView({ answer: "**ship it** — <img src=x onerror=alert(1)> is just text" })],
        }),
      ],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    const seats = await panelFor("Seats");
    // The emphasis is an element, and the asterisks are gone with it.
    await waitFor(() => {
      const strong = Array.from(seats.querySelectorAll("strong")).find(
        (node) => node.textContent === "ship it",
      );
      expect(strong).toBeDefined();
    });
    expect(seats.textContent).not.toContain("**ship it**");
    // Markup a seat wrote is text on the page. A model's answer is not trusted
    // input, and a renderer that passed HTML through would run whatever it said.
    expect(seats.querySelector("img")).toBeNull();
    expect(seats.textContent).toContain("<img src=x onerror=alert(1)>");
  });

  it("keeps the round-0 answer on the card after later steps have run", async () => {
    const view = councilView({
      rounds: 2,
      rounds_run: 2,
      seats: [
        seatView({
          steps: [
            stepView(),
            stepView({ round: 1, phase: "critique", run_id: 2, answer: null, critique: { reviews: [], ranking: ["B"] } }),
            stepView({
              round: 1,
              phase: "revise",
              run_id: 3,
              answer: "still yes, and the migration is the part to watch",
              changed: true,
              why: "the migration risk",
            }),
          ],
        }),
      ],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    // The ranking was cast over the FIRST answers, so a card that dropped the
    // round-0 answer for a later one would be showing a leaderboard of text it
    // never displayed.
    const seats = await panelFor("Seats");
    await waitFor(() =>
      expect(seats.textContent).toContain("yes, ship it — the tests carry the proof"),
    );
  });

  it("shows the error of a latest step that failed", async () => {
    const view = councilView({
      seats: [
        seatView({
          steps: [
            stepView(),
            stepView({
              round: 1,
              phase: "critique",
              run_id: 2,
              status: "error",
              error: "the seat timed out",
              answer: null,
            }),
          ],
        }),
      ],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    const seats = await panelFor("Seats");
    expect(within(seats).getByText("the seat timed out")).toBeDefined();
    expect(within(seats).getByText("failed")).toBeDefined();
  });
});

/* ------------------------------------------------------- an invalid step -- */

describe("Council - a step the daemon could not read", () => {
  it("has its own reading in the state map: paused, labelled invalid", () => {
    // `invalid` is a step whose run answered and whose payload did not parse.
    // Not a failure of the seat's run, and not silence either.
    expect(readState("council_seat", "invalid")).toEqual({ tone: "paused", label: "invalid" });
  });

  it("reads an invalid latest step as invalid on the seat card", async () => {
    const view = councilView({
      seats: [
        seatView({
          steps: [
            stepView(),
            stepView({
              round: 1,
              phase: "critique",
              run_id: 2,
              status: "invalid",
              error: "critique did not parse: expected value",
              answer: null,
            }),
          ],
        }),
      ],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    const seats = await panelFor("Seats");
    expect(within(seats).getByText("invalid")).toBeDefined();
    expect(within(seats).queryByText("failed")).toBeNull();
  });
});

/* ------------------------------------------------------- seats an agent took -- */

describe("Council - a seat an agent filled", () => {
  it("shows who answered and what ran, name above and model beneath", async () => {
    const view = councilView({
      seats: [seatView({ agent_id: "ag-7", agent_name: "the sceptic" })],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    const seats = await panelFor("Seats");
    // Both facts on the card, not one. The name answers who, and the model is
    // what a reader reaches for when the answer is bad — a card that showed
    // only the name would have taken that away to make room for it.
    expect(within(seats).getByText("the sceptic")).toBeDefined();
    expect(within(seats).getByText("claude-opus-4")).toBeDefined();
    // The kind was the title while no seat had a name of its own. It must not
    // still be it, or an agent roster reads as a list of "Cloud, Cloud, Cloud".
    expect(within(seats).queryByText("Cloud")).toBeNull();
  });

  it("keeps the kind as the title for a seat the roster named by model", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], { "c-1": councilView() }),
    );

    await renderCouncil("/council/c-1");

    // The non-regression half: agents are an addition to this page, not a
    // migration of it, and a roster written the old way renders as it did.
    const seats = await panelFor("Seats");
    expect(within(seats).getByText("Cloud")).toBeDefined();
    expect(within(seats).getByText("claude-opus-4")).toBeDefined();
  });
});

describe("Council - a seat whose agent was deleted", () => {
  it("says the agent is gone instead of going blank", async () => {
    const view = councilView({
      seats: [seatView({ agent_id: "ag-7", agent_name: null })],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    // `agent_name` is read from the catalogue as the view is built, so this
    // pair — an id with no name — is the deleted agent, and the daemon is not
    // wrong to serve it. The title falls back to the id: ugly, and still an
    // answer to who. An empty title would be the page pretending nobody sat.
    const seats = await panelFor("Seats");
    const title = seats.querySelector(".council-seat-name");
    expect(title?.textContent?.trim()).toBe("ag-7");
    expect(within(seats).getByText(/no longer in the catalogue/i)).toBeDefined();
    // And the model the seat recorded is untouched by the deletion — that is
    // the whole reason the row copies it instead of reading it back.
    expect(within(seats).getByText("claude-opus-4")).toBeDefined();
  });
});

describe("Council - who chaired", () => {
  it("names the chairman agent and the model it chaired on", async () => {
    const view = councilView({
      chairman_agent_id: "ag-1",
      chairman_agent_name: "the arbiter",
      chairman_ref: "gpt-5",
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    // Scoped to the detail panel: the synthesis is one seat's writing, and
    // until now this page never said whose.
    const panel = await panelFor("This council");
    expect(within(panel).getByText("chaired by the arbiter on gpt-5")).toBeDefined();
  });

  it("names the model alone when no agent chaired", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], { "c-1": councilView() }),
    );

    await renderCouncil("/council/c-1");

    // `chairman_ref` is printed either way. It is the fact that survives the
    // agent being renamed or deleted, which is why the row copies it.
    const panel = await panelFor("This council");
    expect(within(panel).getByText("chaired by claude-opus-4")).toBeDefined();
  });

  it("falls back to the id when the chairman's agent was deleted", async () => {
    const view = councilView({
      chairman_agent_id: "ag-1",
      chairman_agent_name: null,
      chairman_ref: "gpt-5",
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    // The same state a seat handles, at the chairman's position: the name is
    // read from the catalogue as the view is built, so a null beside a set id
    // is a deletion, not a gap. The line does not collapse to "chaired by
    // gpt-5" — that would say a model chaired when an agent did.
    const panel = await panelFor("This council");
    expect(within(panel).getByText("chaired by ag-1 on gpt-5")).toBeDefined();
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

    views["c-1"] = councilView({ status: "done", current_phase: "synthesis", synthesis: "the seats agree." });
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
      current_phase: "synthesis",
      error: "the chairman's run ended in timeout",
      synthesis: null,
      seats: [
        seatView({ seat_idx: 0 }),
        seatView({ seat_idx: 1, kind: "local", ref: "local-model" }),
      ],
      leaderboard: [{ seat_idx: 0, score: 1.5, n: 2 }],
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

/* ------------------------------------------------------- the roster control -- */

/** One catalogue agent. `model: null` is the one that cannot take a seat. */
function agentRow(overrides: Record<string, unknown> = {}) {
  return {
    id: "ag-1",
    name: "the sceptic",
    speciality: "asks what breaks",
    prompt: "be sceptical",
    engine: "claude",
    model: "claude-opus-5",
    tool_policy: "mcp_only",
    created_at: "2026-09-01T10:00:00Z",
    updated_at: "2026-09-01T10:00:00Z",
    ...overrides,
  };
}

/** One row of `GET /assistant/models`. `brain` is what decides a seat's `kind`. */
function modelRow(overrides: Record<string, unknown> = {}) {
  return { id: "claude-opus-5", label: "Opus 5", brain: "cloud", efforts: [], ...overrides };
}

/**
 * Tick the box that opens the control, and wait for the catalogues it then reads.
 *
 * The wait is the point. `RosterPicker` mounts with both queries in flight, so
 * for a tick every seat picker holds nothing but its own placeholder — and a
 * `fireEvent.change` naming an option that has not arrived is silently a
 * no-op, which reads in the failure output as the form ignoring a choice
 * somebody made.
 */
async function openTheRoster() {
  fireEvent.click(await screen.findByLabelText("Put this question to a chosen panel"));
  const chairman = await screen.findByLabelText("Chairman");
  await waitFor(() => expect(chairman.querySelectorAll("option").length).toBeGreaterThan(1));
  return chairman;
}

describe("Council - convening with the roster shut", () => {
  it("sends the question and nothing else", async () => {
    let sent: unknown = "nothing was posted";
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, {
        onCreate: (body) => {
          sent = body;
          return { id: "new-1" };
        },
      }),
    );

    await renderCouncil("/council");
    fireEvent.change(await screen.findByLabelText("Question"), { target: { value: "well?" } });
    fireEvent.click(screen.getByRole("button", { name: "Convene" }));

    // The property this whole packet is built around. `roster` is
    // `Option<RosterOverride>`, which reads an absent key and a `null` the
    // same way — so nothing on the daemon's side would ever have complained
    // if the key had started travelling as `null`, and the request this page
    // has always made would have quietly stopped being the request it makes.
    // Absent, not null, and asserted rather than assumed.
    await waitFor(() => expect(sent).toEqual({ question: "well?" }));
    expect(Object.keys(sent as object)).toEqual(["question"]);
  });

  it("asks for neither catalogue while it is shut", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}));

    await renderCouncil("/council");
    await screen.findByLabelText("Question");

    // The picker is not mounted, so its two queries never run. A control most
    // visits do not use must not add two requests to every visit.
    const asked = daemon.apiFetch.mock.calls.map((call) => call[0]);
    expect(asked).not.toContain("/agents");
    expect(asked).not.toContain("/assistant/models");
  });
});

describe("Council - convening with a chosen panel", () => {
  it("travels as seats the daemon accepts", async () => {
    let sent: unknown = "nothing was posted";
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, {
        agents: [agentRow()],
        models: [modelRow(), modelRow({ id: "qwen3.5:4b", label: "Qwen 4b", brain: "local" })],
        onCreate: (body) => {
          sent = body;
          return { id: "new-1" };
        },
      }),
    );

    await renderCouncil("/council");
    fireEvent.change(await screen.findByLabelText("Question"), { target: { value: "well?" } });
    await openTheRoster();

    // Half-chosen is not a panel: there is no `SeatSpec` meaning "nobody", so
    // convening now would either refuse or silently seat one fewer than the
    // rows on screen. The button waits instead.
    expect(screen.getByRole("button", { name: "Convene" }).hasAttribute("disabled")).toBe(true);

    fireEvent.change(screen.getByLabelText("Chairman"), { target: { value: "agent:ag-1" } });
    fireEvent.change(screen.getByLabelText("Seat 0"), { target: { value: "model:qwen3.5:4b" } });
    fireEvent.click(screen.getByRole("button", { name: "Convene" }));

    // An agent seat carries only `agent`, and a model seat only `kind`/`ref`:
    // `resolve_seat` refuses a seat naming both, and `SeatSpec` is
    // `deny_unknown_fields`, so a third key on either would be a 400 too.
    // `kind` comes from the menu's `brain`, which is the only place this app
    // knows a model's locality.
    await waitFor(() =>
      expect(sent).toEqual({
        question: "well?",
        roster: {
          chairman: { agent: "ag-1" },
          members: [{ kind: "local", ref: "qwen3.5:4b" }],
        },
      }),
    );
  });

  it("never offers an agent that names no model", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, {
        agents: [agentRow(), agentRow({ id: "ag-2", name: "the mute", model: null })],
        models: [modelRow()],
      }),
    );

    await renderCouncil("/council");
    const chairman = await openTheRoster();

    // `council::start` refuses an agent that names no model, because a seat's
    // row records the model that answered and the column is NOT NULL. Offering
    // it here would be offering a 400 — so `canTakeASeat` filters it out, and
    // it is the shell's reading of that same refusal rather than a second rule.
    expect(within(chairman).getByRole("option", { name: "the sceptic" })).toBeDefined();
    expect(within(chairman).queryByRole("option", { name: "the mute" })).toBeNull();
  });

  it("stops offering seats at the eighth", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, { agents: [agentRow()], models: [modelRow()] }),
    );

    await renderCouncil("/council");
    await openTheRoster();

    const add = screen.getByRole("button", { name: "Add a seat" });
    // One row exists already, so seven more reach the ceiling.
    for (let seat = 1; seat < 8; seat += 1) fireEvent.click(add);

    // `MAX_COUNCIL_SEATS` counts members alone — the chairman is resolved apart
    // from them — so eight plus a chairman is a roster the daemon accepts and a
    // ninth member is a 400. The button goes out rather than letting somebody
    // assemble a panel that cannot be convened and learn it on submit.
    expect(within(await screen.findByRole("list", { name: "Panel" })).getAllByRole("listitem")).toHaveLength(8);
    expect(add.hasAttribute("disabled")).toBe(true);
  });
});

/* ------------------------------------------------- rounds, roles, estimate -- */

/** A configured roster of three, so the estimate has a member count before anyone opens the picker. */
function threeSeatConfig(): CouncilConfig {
  return councilConfig({
    default_roster: {
      chairman: { kind: "cloud", ref: "claude-opus-4" },
      members: [
        { kind: "cloud", ref: "claude-opus-4" },
        { kind: "cloud", ref: "claude-opus-5" },
        { kind: "local", ref: "qwen3.5:4b" },
      ],
    },
  });
}

describe("Council - convening with rounds, roles and an estimate", () => {
  it("offers rounds 1 to max, defaulting to the config's, and sends none when untouched", async () => {
    let sent: unknown = "nothing was posted";
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, {
        onCreate: (body) => {
          sent = body;
          return { id: "new-1" };
        },
      }),
    );

    await renderCouncil("/council");
    const rounds = (await screen.findByLabelText("Rounds")) as HTMLSelectElement;
    // The config answers after the form mounts: the default is read, not assumed.
    await waitFor(() => expect(rounds.value).toBe("2"));
    expect(within(rounds).getAllByRole("option").map((option) => option.textContent)).toEqual([
      "1",
      "2",
      "3",
    ]);

    fireEvent.change(screen.getByLabelText("Question"), { target: { value: "well?" } });
    fireEvent.click(screen.getByRole("button", { name: "Convene" }));

    // Choosing nothing is not choosing the default: the key stays off the request.
    await waitFor(() => expect(sent).toEqual({ question: "well?" }));
    expect(Object.keys(sent as object)).toEqual(["question"]);
  });

  it("sends rounds only when it differs from the default", async () => {
    let sent: unknown = "nothing was posted";
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, {
        onCreate: (body) => {
          sent = body;
          return { id: "new-1" };
        },
      }),
    );

    await renderCouncil("/council");
    const rounds = (await screen.findByLabelText("Rounds")) as HTMLSelectElement;
    await waitFor(() => expect(rounds.value).toBe("2"));

    fireEvent.change(rounds, { target: { value: "3" } });
    fireEvent.change(screen.getByLabelText("Question"), { target: { value: "well?" } });
    fireEvent.click(screen.getByRole("button", { name: "Convene" }));

    await waitFor(() => expect(sent).toEqual({ question: "well?", rounds: 3 }));
  });

  it("sends the roles chosen per seat, keyed by the seat's index", async () => {
    let sent: unknown = "nothing was posted";
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, {
        agents: [agentRow()],
        models: [modelRow(), modelRow({ id: "qwen3.5:4b", label: "Qwen 4b", brain: "local" })],
        onCreate: (body) => {
          sent = body;
          return { id: "new-1" };
        },
      }),
    );

    await renderCouncil("/council");
    fireEvent.change(await screen.findByLabelText("Question"), { target: { value: "well?" } });
    await openTheRoster();
    fireEvent.click(screen.getByRole("button", { name: "Add a seat" }));

    // "no role" first, then the config's roles in the daemon's declared order.
    const second = (await screen.findByLabelText("Role for seat 1")) as HTMLSelectElement;
    expect(within(second).getAllByRole("option").map((option) => option.textContent)).toEqual([
      "no role",
      "skeptic",
      "pragmatist",
    ]);
    expect(second.value).toBe("");

    fireEvent.change(screen.getByLabelText("Chairman"), { target: { value: "agent:ag-1" } });
    fireEvent.change(screen.getByLabelText("Seat 0"), { target: { value: "model:claude-opus-5" } });
    fireEvent.change(screen.getByLabelText("Seat 1"), { target: { value: "model:qwen3.5:4b" } });
    // Only seat 1 is given a role; seat 0 keeps "no role" and must not appear.
    fireEvent.change(second, { target: { value: "skeptic" } });
    fireEvent.click(screen.getByRole("button", { name: "Convene" }));

    await waitFor(() =>
      expect(sent).toEqual({
        question: "well?",
        roster: {
          chairman: { agent: "ag-1" },
          members: [
            { kind: "cloud", ref: "claude-opus-5" },
            { kind: "local", ref: "qwen3.5:4b" },
          ],
        },
        roles: { "1": "skeptic" },
      }),
    );
  });

  it("sends no roles key when no seat was given a role", async () => {
    let sent: unknown = "nothing was posted";
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, {
        agents: [agentRow()],
        models: [modelRow()],
        onCreate: (body) => {
          sent = body;
          return { id: "new-1" };
        },
      }),
    );

    await renderCouncil("/council");
    fireEvent.change(await screen.findByLabelText("Question"), { target: { value: "well?" } });
    await openTheRoster();
    fireEvent.change(screen.getByLabelText("Chairman"), { target: { value: "agent:ag-1" } });
    fireEvent.change(screen.getByLabelText("Seat 0"), { target: { value: "model:claude-opus-5" } });
    fireEvent.click(screen.getByRole("button", { name: "Convene" }));

    await waitFor(() => expect(sent).not.toBe("nothing was posted"));
    expect(Object.keys(sent as object)).toEqual(["question", "roster"]);
  });

  it("shows up to X calls for the configured roster at the default rounds, and follows the rounds select", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}, { config: threeSeatConfig() }));

    await renderCouncil("/council");
    // N=3, R=2: 3 answers + 6 critiques + 3 revisions + 2 chairman = 14.
    expect(await screen.findByText("up to 14 calls")).toBeDefined();

    // N=3, R=3: 3 + 9 + 6 + 2 = 20.
    fireEvent.change(screen.getByLabelText("Rounds"), { target: { value: "3" } });
    expect(await screen.findByText("up to 20 calls")).toBeDefined();
  });

  it("counts the chosen members, not the configured roster, once a panel is chosen", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, {
        config: threeSeatConfig(),
        agents: [agentRow()],
        models: [modelRow()],
      }),
    );

    await renderCouncil("/council");
    expect(await screen.findByText("up to 14 calls")).toBeDefined();

    await openTheRoster();
    fireEvent.click(screen.getByRole("button", { name: "Add a seat" }));
    fireEvent.change(screen.getByLabelText("Chairman"), { target: { value: "agent:ag-1" } });
    fireEvent.change(screen.getByLabelText("Seat 0"), { target: { value: "model:claude-opus-5" } });
    fireEvent.change(screen.getByLabelText("Seat 1"), { target: { value: "model:claude-opus-5" } });

    // N=2, R=2: 2 + 4 + 2 + 2 = 10.
    expect(await screen.findByText("up to 10 calls")).toBeDefined();
  });
});

/* -------------------------------------------------------------- leaderboard -- */

describe("Council - the leaderboard", () => {
  it("names the seats rather than numbering them, with the score and n beside each", async () => {
    const view = councilView({
      status: "done",
      seats: [
        seatView({ seat_idx: 0, ref: "gpt-5" }),
        seatView({ seat_idx: 1, agent_id: "ag-7", agent_name: "the sceptic" }),
      ],
      leaderboard: [
        { seat_idx: 1, score: 1, n: 1 },
        { seat_idx: 0, score: 0, n: 1 },
      ],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    const board = await screen.findByRole("list", { name: "Leaderboard" });
    const rows = within(board).getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    // `seatName`: the agent's name, else the model that answered. "seat 1" is a
    // number the reader then has to carry up to the seat grid to decode.
    expect(rows[0].textContent).toContain("the sceptic");
    expect(rows[1].textContent).toContain("gpt-5");
    expect(within(board).queryByText(/^seat \d+$/)).toBeNull();
    // The Borda score, and n beside it always: one ballot and four are not the same claim.
    expect(rows[0].textContent).toContain("1.00");
    expect(rows[0].textContent).toContain("n = 1");
    expect(board.textContent).not.toMatch(/avg rank/);
  });
});

/* ------------------------------------------------ the list and the config -- */

describe("Council - the list row and the config", () => {
  it("reads the council config and the list row says the round and the phase", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch(
        [councilSummary({ rounds: 2, current_round: 1, current_phase: "critique" })],
        {},
      ),
    );

    await renderCouncil("/council");

    const link = await screen.findByRole("link", { name: /should we ship the frontend rewrite\?/ });
    await waitFor(() => expect(link.textContent).toContain("round 1 · critique"));
    // The phase-of-three counter belonged to the staged council and has no
    // meaning over rounds.
    expect(link.textContent).not.toMatch(/phase \d+ of \d+/);
    await waitFor(() =>
      expect(daemon.apiFetch.mock.calls.map((call) => call[0])).toContain("/council/config"),
    );
  });
});

/* ---------------------------------------------- the round-by-round timeline -- */

/**
 * Three named seats through two critique rounds. `anon_map` is label -> seat, so
 * "B" is beta and "C" is gamma. Every assertion below is scoped to the tab panel
 * or to a named group, because the seat grid above the timeline repeats the
 * answers and the latest step and would otherwise satisfy a query by accident.
 *
 * UI contract (what `CouncilRounds.tsx` must render):
 *  - a level-2 heading "Rounds" and a `tablist` named "Rounds" with one `tab` per round 0..rounds_run:
 *    "Answers" (round 0), "Round 1", "Round 2"; "Answers" is selected on first render. Tabs are Radix
 *    (`ui/Tabs`), so they are selected by `mouseDown`, and only the selected `tabpanel` is mounted.
 *  - round 0 panel: each seat's answer text, under the seat's `seatName`.
 *  - round n panel, per seat that voted: a `list` named "Ballot of <seatName>" whose `listitem`s are the
 *    DE-ANONYMISED ranking, best first, as seat names -- never the bare labels.
 *  - per reviewing seat a `group` named "Critiques by <seatName>"; inside, one `group` per reviewed
 *    answer named "On <seatName>'s answer"; its text carries each point's stance, claim and why.
 *  - per seat with a `revise` step in that round a `group` named "Revision by <seatName>": the text
 *    "changed its answer" or "kept its answer", the step's `why`, and -- only when it changed -- a `list`
 *    named "Diff" of `lineDiff(previous answer, new answer)` where each `listitem` carries
 *    `data-kind` = "context" | "removed" | "added" and its textContent contains the line's text.
 *  - a `table` named "Rank evolution", only when `leaderboard_by_round.length >= 2`, outside the tab
 *    panels: one `columnheader` per critique round ("Round 1", ...) after an empty first one, one row per
 *    seat with its `seatName` in the `rowheader` and one `cell` per round holding the seat's 1-based
 *    position in that round's leaderboard ("1" is best), or "-" (en dash) when the seat is not on it.
 *  - when `stopped_early`, a note reading "Stopped early at round <rounds_run>"; absent otherwise.
 */
function seatNamed(name: string, idx: number, steps: StepView[]): SeatView {
  return seatView({
    seat_idx: idx,
    ref: `model-${idx}`,
    agent_id: `ag-${idx}`,
    agent_name: name,
    steps,
  });
}

function critiqueStep(round: number, ranking: string[], reviews: Critique["reviews"] = []): StepView {
  return stepView({
    round,
    phase: "critique",
    run_id: 10 + round,
    answer: null,
    critique: { reviews, ranking },
  });
}

function roundsView(overrides: Partial<CouncilView> = {}): CouncilView {
  return councilView({
    status: "done",
    rounds: 2,
    rounds_run: 2,
    current_round: 2,
    current_phase: "synthesis",
    anon_map: { A: 0, B: 1, C: 2 },
    seats: [
      seatNamed("alpha", 0, [
        stepView({ answer: "line one\nline two" }),
        critiqueStep(1, ["C", "B"], [
          {
            label: "B",
            points: [{ claim: "beta overreaches", stance: "disagree", why: "no evidence given" }],
          },
          {
            label: "C",
            points: [{ claim: "gamma is sound", stance: "agree", why: "the numbers add up" }],
          },
        ]),
        stepView({
          round: 1,
          phase: "revise",
          run_id: 20,
          answer: "line one\nline three",
          changed: true,
          why: "folded in the evidence",
        }),
        critiqueStep(2, ["B", "C"]),
      ]),
      seatNamed("beta", 1, [
        stepView({ answer: "beta says ship" }),
        critiqueStep(1, ["A", "C"]),
        stepView({
          round: 1,
          phase: "revise",
          run_id: 21,
          answer: null,
          changed: false,
          why: "my answer stands",
        }),
        critiqueStep(2, ["A", "C"]),
      ]),
      seatNamed("gamma", 2, [
        stepView({ answer: "gamma says wait" }),
        critiqueStep(1, ["A", "B"]),
        critiqueStep(2, ["A", "B"]),
      ]),
    ],
    ...overrides,
  });
}

async function openRound(name: string) {
  // Radix selects a tab on pointer-down, not on click (see `Bench.test.tsx`).
  fireEvent.mouseDown(await screen.findByRole("tab", { name }));
  return screen.findByRole("tabpanel");
}

describe("Council - round by round", () => {
  it("offers one tab per round, with the seats' answers in round 0", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": roundsView() }));

    await renderCouncil("/council/c-1");

    expect(await screen.findByRole("heading", { level: 2, name: "Rounds" })).toBeDefined();
    const tabs = within(await screen.findByRole("tablist", { name: "Rounds" })).getAllByRole("tab");
    expect(tabs.map((tab) => tab.textContent)).toEqual(["Answers", "Round 1", "Round 2"]);
    expect(tabs[0].getAttribute("aria-selected")).toBe("true");

    const panel = await screen.findByRole("tabpanel");
    expect(panel.textContent).toContain("alpha");
    expect(panel.textContent).toContain("line one");
    expect(panel.textContent).toContain("beta says ship");
    expect(panel.textContent).toContain("gamma says wait");
  });

  it("de-anonymises a ballot into seat names, best first", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": roundsView() }));

    await renderCouncil("/council/c-1");
    const panel = await openRound("Round 1");

    // alpha ranked ["C", "B"]: gamma first, beta second.
    const ballot = within(panel).getByRole("list", { name: "Ballot of alpha" });
    const items = within(ballot).getAllByRole("listitem");
    expect(items).toHaveLength(2);
    expect(items[0].textContent).toContain("gamma");
    expect(items[1].textContent).toContain("beta");
    // The labels were the blinding; they never reach the reader.
    expect(within(ballot).queryByText(/^[ABC]$/)).toBeNull();
  });

  it("lists the critiques a seat gave, grouped by the answer reviewed", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": roundsView() }));

    await renderCouncil("/council/c-1");
    const panel = await openRound("Round 1");

    const given = within(panel).getByRole("group", { name: "Critiques by alpha" });
    const onBeta = within(given).getByRole("group", { name: "On beta's answer" });
    expect(onBeta.textContent).toContain("disagree");
    expect(onBeta.textContent).toContain("beta overreaches");
    expect(onBeta.textContent).toContain("no evidence given");
    const onGamma = within(given).getByRole("group", { name: "On gamma's answer" });
    expect(onGamma.textContent).toContain("agree");
    expect(onGamma.textContent).toContain("gamma is sound");
    // Reviews of beta do not leak into the group about gamma.
    expect(onGamma.textContent).not.toContain("beta overreaches");
  });

  it("shows a revision's changed/why and the line diff against the previous answer", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": roundsView() }));

    await renderCouncil("/council/c-1");
    const panel = await openRound("Round 1");

    const revision = within(panel).getByRole("group", { name: "Revision by alpha" });
    expect(revision.textContent).toContain("changed its answer");
    expect(revision.textContent).toContain("folded in the evidence");
    const diff = within(revision).getByRole("list", { name: "Diff" });
    const lines = within(diff).getAllByRole("listitem");
    expect(lines.map((item) => item.getAttribute("data-kind"))).toEqual([
      "context",
      "removed",
      "added",
    ]);
    expect(lines[0].textContent).toContain("line one");
    expect(lines[1].textContent).toContain("line two");
    expect(lines[2].textContent).toContain("line three");

    // A seat that kept its answer says so and draws no diff.
    const kept = within(panel).getByRole("group", { name: "Revision by beta" });
    expect(kept.textContent).toContain("kept its answer");
    expect(kept.textContent).toContain("my answer stands");
    expect(within(kept).queryByRole("list", { name: "Diff" })).toBeNull();
  });

  it("draws the rank evolution only once there are two votes", async () => {
    const one = roundsView({
      rounds: 1,
      rounds_run: 1,
      leaderboard_by_round: [
        [
          { seat_idx: 0, score: 2, n: 3 },
          { seat_idx: 1, score: 1, n: 3 },
        ],
      ],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": one }));

    const first = await renderCouncil("/council/c-1");
    await screen.findByRole("tablist", { name: "Rounds" });
    expect(screen.queryByRole("table", { name: "Rank evolution" })).toBeNull();
    first.unmount();

    const two = roundsView({
      leaderboard_by_round: [
        [
          { seat_idx: 0, score: 2, n: 3 },
          { seat_idx: 1, score: 1, n: 3 },
        ],
        [
          { seat_idx: 1, score: 2, n: 3 },
          { seat_idx: 0, score: 1, n: 3 },
          { seat_idx: 2, score: 0, n: 3 },
        ],
      ],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": two }));

    await renderCouncil("/council/c-1");
    const table = await screen.findByRole("table", { name: "Rank evolution" });
    expect(within(table).getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual([
      "",
      "Round 1",
      "Round 2",
    ]);
    const cellsOf = (name: string) => {
      const row = within(table).getByRole("rowheader", { name }).closest("tr");
      if (row === null) throw new Error(`no row for ${name}`);
      return within(row as HTMLElement)
        .getAllByRole("cell")
        .map((cell) => cell.textContent);
    };
    expect(cellsOf("alpha")).toEqual(["1", "2"]);
    expect(cellsOf("beta")).toEqual(["2", "1"]);
    // gamma was not on the first leaderboard at all.
    expect(cellsOf("gamma")).toEqual(["–", "3"]);
  });

  it("announces that it stopped early, at the round it stopped", async () => {
    const stopped = roundsView({ rounds: 3, rounds_run: 1, stopped_early: true });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": stopped }));

    const first = await renderCouncil("/council/c-1");
    expect(await screen.findByText(/Stopped early at round 1/)).toBeDefined();
    first.unmount();

    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": roundsView() }));
    await renderCouncil("/council/c-1");
    await screen.findByRole("tablist", { name: "Rounds" });
    expect(screen.queryByText(/Stopped early at round/)).toBeNull();
  });
});

/* ------------------------------------------------- S5: structured synthesis -- */

/**
 * UI contract (what `CouncilSynthesis.tsx` must render inside the panel headed "Synthesis", which
 * replaces the old `Synthesis` function in `Council.tsx`):
 *  - `synthesis_structured` set (and status not "degraded"): the `answer` through `CouncilRich`, then
 *    - a confidence badge whose whole text is "<level> confidence" ("high confidence", ...);
 *    - when `view.agreement` is set, an agreement badge whose whole text is, by `agreement.level`:
 *      "strong" -> "strong consensus", "split" -> "split", "none" -> "no consensus",
 *      "insufficient" -> "too few votes". No badge when `agreement` is null;
 *    - four `group`s named "Consensus", "Disagreements", "Minority", "Open questions". Consensus and
 *      Open questions hold one `listitem` per string; Disagreements holds each topic and, per position,
 *      one `listitem` carrying the `seatName` of EVERY seat in `position.seats` (seats resolved against
 *      `view.seats`) and the position's `view`; Minority holds the minority text. A card with nothing
 *      to say (empty list, `minority` null) is not rendered.
 *  - `synthesis_status === "degraded"`: the raw `synthesis` text plus a warning containing
 *    "unstructured synthesis"; none of the four groups and no badges.
 *  - `synthesis_structured === null` with text and status null (an old council): the rich text only,
 *    no warning, no badges, no groups.
 */
function structuredSynthesis(overrides: Partial<Synthesis> = {}): Synthesis {
  return {
    answer: "Ship it behind a flag.",
    consensus: ["the tests carry the proof"],
    disagreements: [
      {
        topic: "rollout speed",
        positions: [
          { seats: [0, 2], view: "ship now" },
          { seats: [1], view: "wait a sprint" },
        ],
      },
    ],
    minority: "the migration may be riskier than the seats assume",
    confidence: { level: "high", why: "every ballot agrees" },
    open_questions: ["who owns the rollback?"],
    ...overrides,
  };
}

function synthesisView(overrides: Partial<CouncilView> = {}): CouncilView {
  return roundsView({
    synthesis: "Ship it behind a flag.",
    synthesis_structured: structuredSynthesis(),
    synthesis_status: "ok",
    agreement: { tau: 0.9, level: "strong", ballots: 3, comparisons: 3 },
    ...overrides,
  });
}

describe("Council - the structured synthesis", () => {
  it("renders the answer, the consensus, the named disagreements, the minority and the open questions", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": synthesisView() }));

    await renderCouncil("/council/c-1");
    const panel = await panelFor("Synthesis");

    expect(await within(panel).findByText("Ship it behind a flag.")).toBeDefined();
    const consensus = within(panel).getByRole("group", { name: "Consensus" });
    expect(within(consensus).getAllByRole("listitem").map((li) => li.textContent)).toEqual([
      "the tests carry the proof",
    ]);

    const disagreements = within(panel).getByRole("group", { name: "Disagreements" });
    expect(disagreements.textContent).toContain("rollout speed");
    const positions = within(disagreements).getAllByRole("listitem");
    expect(positions).toHaveLength(2);
    // Seats are named, never numbered: "seat 2" would send the reader back to the grid.
    expect(positions[0].textContent).toContain("alpha");
    expect(positions[0].textContent).toContain("gamma");
    expect(positions[0].textContent).toContain("ship now");
    expect(positions[0].textContent).not.toContain("beta");
    expect(positions[1].textContent).toContain("beta");
    expect(positions[1].textContent).toContain("wait a sprint");

    const minority = within(panel).getByRole("group", { name: "Minority" });
    expect(minority.textContent).toContain("riskier than the seats assume");
    const open = within(panel).getByRole("group", { name: "Open questions" });
    expect(within(open).getAllByRole("listitem").map((li) => li.textContent)).toEqual(["who owns the rollback?"]);
  });

  it("badges the chairman's confidence", async () => {
    const view = synthesisView({
      synthesis_structured: structuredSynthesis({ confidence: { level: "low", why: "the seats barely overlap" } }),
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");
    const panel = await panelFor("Synthesis");

    expect(await within(panel).findByText("low confidence")).toBeDefined();
    expect(within(panel).queryByText("high confidence")).toBeNull();
  });

  it("badges how far the ballots agreed, in words", async () => {
    const cases: Array<[string, string]> = [
      ["strong", "strong consensus"],
      ["split", "split"],
      ["none", "no consensus"],
      ["insufficient", "too few votes"],
    ];
    for (const [level, words] of cases) {
      const view = synthesisView({
        agreement: { tau: level === "insufficient" ? null : 0.4, level, ballots: 3, comparisons: 3 },
      });
      daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

      const rendered = await renderCouncil("/council/c-1");
      const panel = await panelFor("Synthesis");
      expect(await within(panel).findByText(words)).toBeDefined();
      rendered.unmount();
    }
  });

  it("draws no agreement badge while there is no agreement to report", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": synthesisView({ agreement: null }) }));

    await renderCouncil("/council/c-1");
    const panel = await panelFor("Synthesis");

    expect(await within(panel).findByText("high confidence")).toBeDefined();
    for (const words of ["strong consensus", "split", "no consensus", "too few votes"]) {
      expect(within(panel).queryByText(words)).toBeNull();
    }
  });
});

describe("Council - a degraded synthesis", () => {
  it("shows the raw text with an unstructured-synthesis warning and none of the cards", async () => {
    const view = synthesisView({
      synthesis: "the chairman rambled without structure",
      synthesis_structured: null,
      synthesis_status: "degraded",
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");
    const panel = await panelFor("Synthesis");

    expect(await within(panel).findByText(/the chairman rambled without structure/)).toBeDefined();
    expect(within(panel).getByText(/unstructured synthesis/i)).toBeDefined();
    for (const name of ["Consensus", "Disagreements", "Minority", "Open questions"]) {
      expect(within(panel).queryByRole("group", { name })).toBeNull();
    }
    expect(within(panel).queryByText(/confidence$/)).toBeNull();
  });

  it("reads an old council, with text and no structure, as plain rich text without a warning", async () => {
    const view = synthesisView({
      synthesis: "an old synthesis from before structure",
      synthesis_structured: null,
      synthesis_status: null,
      agreement: null,
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");
    const panel = await panelFor("Synthesis");

    expect(await within(panel).findByText(/an old synthesis from before structure/)).toBeDefined();
    expect(within(panel).queryByText(/unstructured synthesis/i)).toBeNull();
    expect(within(panel).queryByRole("group", { name: "Consensus" })).toBeNull();
    expect(within(panel).queryByText(/confidence$/)).toBeNull();
  });
});

/* ----------------------------------------------------------- S5: live tail -- */

/**
 * UI contract for the live tail (rendered in the seat grid's card, by a `StepTail` component):
 *  - a seat step with `status === "pending"`, a `run_id`, while the council `status` is "running"
 *    renders `role="log"` named "Live tail of <seatName>" whose text is the chunks read so far,
 *    concatenated. It reads `useRunTail(run_id, since, true)`, takes each NEW chunk once (by identity)
 *    and asks next for `since = chunk.next`, exactly as `RunDetail.tsx::RunTail` does.
 *  - once the step settles (status no longer "pending") or the council is no longer running, the log
 *    is gone, the stored result is shown instead, and the tail query is no longer read.
 */
function pendingView(overrides: Partial<CouncilView> = {}): CouncilView {
  return councilView({
    status: "running",
    current_round: 0,
    current_phase: "answer",
    seats: [seatNamed("alpha", 0, [stepView({ status: "pending", run_id: 7, answer: null })])],
    ...overrides,
  });
}

function tailAsks(): string[] {
  return daemon.apiFetch.mock.calls.map((call) => String(call[0])).filter((path) => path.startsWith("/runs/7/tail"));
}

describe("Council - the live tail of a running step", () => {
  it("shows what the running step has written and continues from the chunk's own offset", async () => {
    const chunks: Record<number, RunTailChunk> = {
      0: { text: "reading the diff, ", next: 18, live: true },
      18: { text: "then weighing it", next: 34, live: true },
    };
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], { "c-1": pendingView() }, { tail: (_run, since) => chunks[since] }),
    );

    const { queryClient } = await renderCouncil("/council/c-1");
    const log = await screen.findByRole("log", { name: "Live tail of alpha" });
    await waitFor(() => expect(log.textContent).toContain("reading the diff, "));

    await act(async () => {
      await queryClient.refetchQueries({ queryKey: keys.runs.tail(7) });
    });
    await waitFor(() => expect(log.textContent).toBe("reading the diff, then weighing it"));
    // The offset is the daemon's byte cursor, handed back as it came.
    expect(tailAsks()).toContain("/runs/7/tail?since=18");
  });

  it("does not repeat a chunk the daemon sends twice", async () => {
    const same: RunTailChunk = { text: "once only", next: 9, live: true };
    daemon.apiFetch.mockImplementation(
      councilFetch(
        [councilSummary()],
        { "c-1": pendingView() },
        { tail: (_run, since) => (since === 0 ? same : { text: "", next: 9, live: true }) },
      ),
    );

    const { queryClient } = await renderCouncil("/council/c-1");
    const log = await screen.findByRole("log", { name: "Live tail of alpha" });
    await waitFor(() => expect(log.textContent).toBe("once only"));
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: keys.runs.tail(7) });
    });
    expect(log.textContent).toBe("once only");
  });

  it("stops reading the tail, and shows the stored answer, once the step settles", async () => {
    const views: Record<string, CouncilView> = { "c-1": pendingView() };
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], views, { tail: () => ({ text: "still thinking", next: 14, live: true }) }),
    );

    const { queryClient } = await renderCouncil("/council/c-1");
    const log = await screen.findByRole("log", { name: "Live tail of alpha" });
    await waitFor(() => expect(log.textContent).toContain("still thinking"));

    views["c-1"] = pendingView({
      seats: [seatNamed("alpha", 0, [stepView({ status: "ok", run_id: 7, answer: "the settled answer" })])],
    });
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: [...keys.council.all, "detail", "c-1"] });
    });

    await waitFor(() => expect(screen.queryByRole("log", { name: "Live tail of alpha" })).toBeNull());
    expect(await screen.findByText("the settled answer")).toBeDefined();
    const before = tailAsks().length;
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: keys.runs.tail(7) });
    });
    expect(tailAsks()).toHaveLength(before);
  });

  it("shows no tail for a step that is not pending, nor for a pending step of a council that has ended", async () => {
    const tail = () => ({ text: "x", next: 1, live: true });
    const settled = councilView({ status: "running", seats: [seatNamed("alpha", 0, [stepView()])] });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": settled }, { tail }));
    const first = await renderCouncil("/council/c-1");
    await panelFor("This council");
    expect(screen.queryByRole("log")).toBeNull();
    first.unmount();

    const ended = pendingView({ status: "error" });
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary({ status: "error" })], { "c-1": ended }, { tail }),
    );
    await renderCouncil("/council/c-1");
    await panelFor("This council");
    expect(screen.queryByRole("log")).toBeNull();
    expect(tailAsks()).toHaveLength(0);
  });
});
