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
    stages_total: 3,
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
    stage1_status: "ok",
    stage1_error: null,
    answer: "yes, ship it — the tests carry the proof",
    stage2_status: "ok",
    stage2_error: null,
    rankings: [{ anon: "B", rank: 1 }],
    revision_status: "pending",
    revision_error: null,
    revised_answer: null,
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
    stages_total: 3,
    error: null,
    chairman_kind: "cloud",
    chairman_ref: "claude-opus-4",
    chairman_agent_id: null,
    chairman_agent_name: null,
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
  opts: {
    /** Handed the PARSED request body: the roster tests assert on what travelled. */
    onCreate?: (body: unknown) => unknown;
    onCancel?: (id: string) => unknown;
    /** `GET /agents`, read only while the roster control is open. */
    agents?: unknown[];
    /** `GET /assistant/models`, likewise. */
    models?: unknown[];
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

/* --------------------------------------------------------- the second round -- */

describe("Council - a council that runs a second round", () => {
  it("counts its phases out of four and draws the revision", async () => {
    const view = councilView({
      stage: 3,
      stages_total: 4,
      seats: [
        seatView({
          revision_status: "ok",
          revised_answer: "still yes, and the migration is the part to watch",
        }),
      ],
    });
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary({ stage: 3, stages_total: 4 })], { "c-1": view }),
    );

    await renderCouncil("/council/c-1");

    // The total is the council's own fact, read off the row. A page that had
    // gone on saying "of 3" would have reported a council on its third of four
    // phases as finished.
    expect(await screen.findByText("phase 3 of 4")).toBeDefined();

    const seats = await panelFor("Seats");
    expect(within(seats).getByText("revision")).toBeDefined();
    expect(
      within(seats).getByText("still yes, and the migration is the part to watch"),
    ).toBeDefined();
    // Beside the first answer, never instead of it: the ranking was cast over
    // the first one, so a card showing only the revision would be showing a
    // leaderboard of text it never displayed.
    expect(within(seats).getByText("yes, ship it — the tests carry the proof")).toBeDefined();
  });

  it("says the first answer stood when a seat did not revise", async () => {
    const view = councilView({
      stage: 4,
      stages_total: 4,
      seats: [seatView({ revision_status: "error", revision_error: "the seat timed out" })],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    // The chairman read this seat's FIRST answer, and the card says so. A blank
    // would read as text that went missing rather than as a seat whose revision
    // failed and whose original answer was used.
    const seats = await panelFor("Seats");
    expect(within(seats).getByText("the first answer stood")).toBeDefined();
    expect(within(seats).getByText("the seat timed out")).toBeDefined();
  });

  it("draws no revision at all on a council of one round", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], { "c-1": councilView() }),
    );

    await renderCouncil("/council/c-1");

    // The non-regression half, and the reason the card is TOLD the total rather
    // than reading `revision_status`: a one-round council leaves every seat at
    // `pending` forever, so a card deciding for itself would have drawn a
    // "waiting" badge for a phase that was never coming.
    const seats = await panelFor("Seats");
    expect(within(seats).queryByText("revision")).toBeNull();
    expect(await screen.findByText("phase 2 of 3")).toBeDefined();
  });
});
