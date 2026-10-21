import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
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
    agreement_by_round: [],
    ballot_rounds: [],
    synthesis_run_id: null,
    synthesis: null,
    synthesis_structured: null,
    synthesis_status: null,
    anon_map: { A: 0, B: 1 },
    seats: [seatView()],
    ...overrides,
  };
}

/**
 * `GET /council/config` — a configured roster of three, two rounds by default.
 * The room opens on that roster, so a test that convenes untouched needs it; a
 * test that wants an empty room passes `default_roster: null`.
 */
function councilConfig(overrides: Partial<CouncilConfig> = {}): CouncilConfig {
  return {
    configured: true,
    default_rounds: 2,
    max_rounds: 3,
    roles: ["skeptic", "pragmatist"],
    default_roster: {
      chairman: { kind: "cloud", ref: "claude-opus-4" },
      members: [
        { kind: "cloud", ref: "claude-opus-4" },
        { kind: "cloud", ref: "claude-opus-5" },
        { kind: "local", ref: "qwen3.5:4b" },
      ],
    },
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

/** The Case record panel is shut by default; the deliberation's columns live inside it. */
async function openTheRecord() {
  const panel = await panelFor("Case record");
  fireEvent.click(within(panel).getByRole("button", { name: "Show case record" }));
}

/** Click a chair by its accessible name prefix ("Seat 4"). */
async function chooseChair(n: number) {
  const chair = await screen.findByRole("button", { name: new RegExp(`^Seat ${n},`) });
  fireEvent.click(chair);
  return chair;
}

async function chooseJudge() {
  fireEvent.click(await screen.findByRole("button", { name: /^Chairman,/ }));
}

/** The dossier aside. */
const dossier = () => screen.getByRole("complementary", { name: "Dossier" });

/** Convene waits for the config now: click it once it has lost `disabled`. */
async function conveneWhenReady() {
  const convene = await screen.findByRole("button", { name: "Convene" });
  await waitFor(() => expect(convene.hasAttribute("disabled")).toBe(false));
  fireEvent.click(convene);
}

/* --------------------------------------------------------- empty catalogue -- */

describe("Council - the empty catalogue", () => {
  it("an empty catalogue is the teach block alone", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}));

    await renderCouncil("/council");

    expect(await screen.findByRole("heading", { level: 3, name: "No council has met yet" })).toBeDefined();
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
    await openTheRecord();

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
    await openTheRecord();

    expect(await screen.findByText("answered, but the text has expired")).toBeDefined();
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
    await openTheRecord();

    const seats = await panelFor("Deliberation");
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
    await openTheRecord();

    // The ranking was cast over the FIRST answers, so a card that dropped the
    // round-0 answer for a later one would be showing a leaderboard of text it
    // never displayed.
    // The panel opens on the last round, and the answer lives in the Answers tab only.
    const seats = await panelFor("Deliberation");
    const answers = await openRound("Answers");
    expect(seats.contains(answers)).toBe(true);
    await waitFor(() =>
      expect(answers.textContent).toContain("yes, ship it — the tests carry the proof"),
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
    await openTheRecord();

    const seats = await panelFor("Deliberation");
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
    await openTheRecord();

    const seats = await panelFor("Deliberation");
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
    await openTheRecord();

    const seats = await panelFor("Deliberation");
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
    await openTheRecord();

    // The non-regression half: agents are an addition to this page, not a
    // migration of it, and a roster written the old way renders as it did.
    const seats = await panelFor("Deliberation");
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
    await openTheRecord();

    // `agent_name` is read from the catalogue as the view is built, so this
    // pair — an id with no name — is the deleted agent, and the daemon is not
    // wrong to serve it. The title falls back to the id: ugly, and still an
    // answer to who. An empty title would be the page pretending nobody sat.
    const seats = await panelFor("Deliberation");
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

    // The facts line: the synthesis is one seat's writing, and until now this
    // page never said whose.
    expect(await screen.findByText("chaired by the arbiter on gpt-5")).toBeDefined();
  });

  it("names the model alone when no agent chaired", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], { "c-1": councilView() }),
    );

    await renderCouncil("/council/c-1");

    // `chairman_ref` is printed either way. It is the fact that survives the
    // agent being renamed or deleted, which is why the row copies it.
    expect(await screen.findByText("chaired by claude-opus-4")).toBeDefined();
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
    expect(await screen.findByText("chaired by ag-1 on gpt-5")).toBeDefined();
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
    await conveneWhenReady();

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

    // The case file's own copy, not the daemon's. It is a second place the path is written,
    // and the two saying different things is worse than either being wrong alone.
    const note = await screen.findByText(/Roster from ~\/\.nucleos\/council\.yaml/);
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
    await conveneWhenReady();

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
    await conveneWhenReady();

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
    // The facts line's badge, found by its exact word: the page headline says
    // "deliberating" inside a longer sentence and does not match.
    expect(await screen.findByText("deliberating")).toBeDefined();

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
    expect(await screen.findByText("settled")).toBeDefined();

    expect(computedInterval()).toBe(false);
  });
});

/* --------------------------------------------------- A14: chairman failed -- */

describe("Council - a chairman that failed", () => {
  it("still draws every seat, and the dossier carries the failure, when synthesis is null and error is set", async () => {
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

    // The seats are read in the room, by their chairs; the chairman's failure is
    // the error note in the dossier, opened on the bench.
    expect(await screen.findByRole("button", { name: /^Seat 1,/ })).toBeDefined();
    expect(screen.getByRole("button", { name: /^Seat 2,/ })).toBeDefined();
    // The picture is aria-hidden, so the judge's state is read off the drawing itself.
    expect(screen.getByRole("button", { name: /^Chairman,/ })).toBeDefined();
    expect(document.querySelector(".council-court-judge")?.getAttribute("data-state")).toBe("failed");
    expect(await within(dossier()).findByText(/ended in timeout/)).toBeDefined();
    // The seats' own answers are still on the record.
    await openTheRecord();
    expect(await screen.findByRole("list", { name: "Seats" })).toBeDefined();
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

/* ------------------------------------------------------- the courtroom setup -- */

/** One catalogue agent. `model: null` is the one that cannot take a seat. */
function agentRow(overrides: Record<string, unknown> = {}) {
  return {
    id: "ag-1",
    name: "the sceptic",
    speciality: "asks what breaks",
    prompt: "be sceptical",
    engine: "claude",
    model: "claude-opus-5",
    capabilities: ["mcp.nucleos"],
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

/** The Convene button. */
const convene = () => screen.getByRole("button", { name: "Convene" });

/** The sentence a disabled Convene points at with `aria-describedby`. */
const reasonOf = (button: HTMLElement) =>
  document.getElementById(button.getAttribute("aria-describedby") ?? "")?.textContent;

/** The phases drawn on the process line, as text. */
const processPhaseLabels = () =>
  within(screen.getByRole("list", { name: "Process" }))
    .getAllByRole("listitem")
    .map((item) => item.textContent);

describe("Council - convening from the untouched room", () => {
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
    await conveneWhenReady();

    // The property this whole page is built around. `roster` is
    // `Option<RosterOverride>`, which reads an absent key and a `null` the
    // same way — so nothing on the daemon's side would ever have complained
    // if the key had started travelling as `null`, and the request this page
    // has always made would have quietly stopped being the request it makes.
    // Absent, not null, and asserted rather than assumed.
    await waitFor(() => expect(sent).toEqual({ question: "well?" }));
    expect(Object.keys(sent as object)).toEqual(["question"]);
  });

  it("asks for neither catalogue until a chair is chosen", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}, { agents: [agentRow()], models: [modelRow()] }));

    await renderCouncil("/council");
    await screen.findByLabelText("Question");
    await screen.findByRole("button", { name: /^Seat 1,/ });

    // A room most visits do not touch must not add two requests to every visit.
    const asked = () => daemon.apiFetch.mock.calls.map((call) => call[0]);
    expect(asked()).not.toContain("/agents");
    expect(asked()).not.toContain("/assistant/models");

    // Calling somebody to an empty chair is what reads them.
    await chooseChair(4);
    await waitFor(() => expect(asked()).toContain("/agents"));
    expect(asked()).toContain("/assistant/models");
  });

  it("seats the configured roster, on the chairs and the bench", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}));

    await renderCouncil("/council");

    expect(await screen.findByRole("button", { name: "Seat 1, claude-opus-4" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Seat 2, claude-opus-5" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Seat 3, qwen3.5:4b" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Seat 4, empty" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Chairman, claude-opus-4" })).toBeDefined();
    await waitFor(() => expect(processPhaseLabels()).toHaveLength(5));
  });
});

describe("Council - convening with a chosen panel", () => {
  it("travels as seats the daemon accepts", async () => {
    let sent: unknown = "nothing was posted";
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, {
        config: councilConfig({ default_roster: null }),
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

    // An empty room is not a panel: with nobody on the bench or in a chair the
    // button waits, and says why.
    await waitFor(() => expect(convene().hasAttribute("disabled")).toBe(true));

    await chooseJudge();
    fireEvent.click(await within(dossier()).findByRole("button", { name: "the sceptic" }));
    await chooseChair(1);
    fireEvent.click(await within(dossier()).findByRole("button", { name: "Qwen 4b" }));
    await conveneWhenReady();

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
        config: councilConfig({ default_roster: null }),
        agents: [agentRow(), agentRow({ id: "ag-2", name: "the mute", model: null })],
        models: [modelRow()],
      }),
    );

    await renderCouncil("/council");
    await chooseChair(1);
    const agents = await within(dossier()).findByRole("group", { name: "Agents" });

    // `council::start` refuses an agent that names no model, because a seat's
    // row records the model that answered and the column is NOT NULL. Offering
    // it here would be offering a 400 — so `canTakeASeat` filters it out, and
    // it is the shell's reading of that same refusal rather than a second rule.
    expect(within(agents).getByRole("button", { name: "the sceptic" })).toBeDefined();
    expect(within(agents).queryByRole("button", { name: "the mute" })).toBeNull();
  });

  it("has exactly eight chairs and no way to add a ninth", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, { agents: [agentRow()], models: [modelRow()] }),
    );

    await renderCouncil("/council");
    await screen.findByRole("button", { name: /^Seat 1,/ });

    // `MAX_COUNCIL_SEATS` counts members alone — the chairman is resolved apart
    // from them — so eight plus a chairman is a roster the daemon accepts and a
    // ninth member is a 400. The room has eight chairs and nothing to add one.
    expect(screen.getAllByRole("button", { name: /^Seat \d,/ })).toHaveLength(8);
    expect(screen.queryByRole("button", { name: "Add a seat" })).toBeNull();
  });

  it("a released chair leaves the roster, re-keys the roles and lowers the estimate", async () => {
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
    await chooseChair(1);
    fireEvent.click(await within(dossier()).findByRole("button", { name: "Release" }));
    // N=2, R=2: 2 answers + 4 critiques + 2 revisions + 2 chairman = 10.
    expect(await screen.findByText("up to 10 calls")).toBeDefined();

    // Seat 3 is now the second member, so a role given there is keyed "1".
    await chooseChair(3);
    fireEvent.click(await within(dossier()).findByRole("button", { name: "skeptic" }));
    await conveneWhenReady();

    await waitFor(() =>
      expect(sent).toEqual({
        question: "well?",
        roster: {
          chairman: { kind: "cloud", ref: "claude-opus-4" },
          members: [
            { kind: "cloud", ref: "claude-opus-5" },
            { kind: "local", ref: "qwen3.5:4b" },
          ],
        },
        roles: { "1": "skeptic" },
      }),
    );
  });
});

/* ------------------------------------------------- rounds, roles, estimate -- */

describe("Council - convening with rounds, roles and an estimate", () => {
  it("draws the phases for the config's default rounds and sends none when untouched", async () => {
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
    // The config answers after the page mounts: the default is read, not assumed.
    await waitFor(() =>
      expect(processPhaseLabels()).toEqual([
        "Opening",
        "Deliberation I",
        "Rebuttal I",
        "Deliberation II",
        "Verdict",
      ]),
    );

    fireEvent.change(screen.getByLabelText("Question"), { target: { value: "well?" } });
    await conveneWhenReady();

    // Choosing nothing is not choosing the default: the key stays off the request.
    await waitFor(() => expect(sent).toEqual({ question: "well?" }));
    expect(Object.keys(sent as object)).toEqual(["question"]);
  });

  it("adds a round up to the maximum and removes one down to one", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}));

    await renderCouncil("/council");
    await waitFor(() => expect(processPhaseLabels()).toHaveLength(5));

    const add = screen.getByRole("button", { name: "Add a round" });
    const remove = screen.getByRole("button", { name: "Remove the last round" });
    fireEvent.click(add);
    expect(processPhaseLabels()).toHaveLength(7);
    expect(add.hasAttribute("disabled")).toBe(true);

    fireEvent.click(remove);
    fireEvent.click(remove);
    expect(processPhaseLabels()).toEqual(["Opening", "Deliberation I", "Verdict"]);
    expect(remove.hasAttribute("disabled")).toBe(true);
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
    await waitFor(() => expect(processPhaseLabels()).toHaveLength(5));

    fireEvent.click(screen.getByRole("button", { name: "Add a round" }));
    fireEvent.change(screen.getByLabelText("Question"), { target: { value: "well?" } });
    await conveneWhenReady();

    await waitFor(() => expect(sent).toEqual({ question: "well?", rounds: 3 }));
  });

  it("sends the roles chosen per chair, keyed by the member's index, without a roster", async () => {
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
    await chooseChair(2);

    // The config's roles in the daemon's declared order, then "no role".
    const roles = await within(dossier()).findByRole("group", { name: "Roles" });
    expect(within(roles).getAllByRole("button").map((button) => button.textContent)).toEqual([
      "skeptic",
      "pragmatist",
      "no role",
    ]);
    fireEvent.click(within(roles).getByRole("button", { name: "skeptic" }));
    await conveneWhenReady();

    // A role alone does not touch the roster: `roles` travels against the file's.
    await waitFor(() => expect(sent).toEqual({ question: "well?", roles: { "1": "skeptic" } }));
  });

  it("sends no roles key when no chair was given a role", async () => {
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
    await chooseChair(2);
    await within(dossier()).findByRole("group", { name: "Roles" });
    await conveneWhenReady();

    await waitFor(() => expect(sent).not.toBe("nothing was posted"));
    expect(Object.keys(sent as object)).toEqual(["question"]);
  });

  it("shows up to X calls for the configured roster at the default rounds, and follows the rounds", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}));

    await renderCouncil("/council");
    // N=3, R=2: 3 answers + 6 critiques + 3 revisions + 2 chairman = 14.
    expect(await screen.findByText("up to 14 calls")).toBeDefined();
    expect(screen.getByText("5 steps in sequence")).toBeDefined();

    // N=3, R=3: 3 + 9 + 6 + 2 = 20.
    fireEvent.click(screen.getByRole("button", { name: "Add a round" }));
    expect(await screen.findByText("up to 20 calls")).toBeDefined();
    expect(screen.getByText("7 steps in sequence")).toBeDefined();
  });
});

/* ------------------------------------------------------------ the lectern -- */

describe("Council - the lectern", () => {
  it("shows the whole question in the dossier while the lectern is focused", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}));

    await renderCouncil("/council");
    const question = (await screen.findByLabelText("Question")) as HTMLTextAreaElement;
    expect(question.getAttribute("placeholder")).toBe("Ask the council…");

    const long = "should we ship the frontend rewrite before the migration is rehearsed twice over?";
    fireEvent.focus(question);
    fireEvent.change(question, { target: { value: long } });

    // The lectern holds a line; the dossier is where the question is read whole.
    expect(within(dossier()).getByText(long)).toBeDefined();
  });

  it("convenes on ctrl+enter", async () => {
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
    const question = await screen.findByLabelText("Question");
    fireEvent.change(question, { target: { value: "well?" } });
    await waitFor(() => expect(convene().hasAttribute("disabled")).toBe(false));
    fireEvent.keyDown(question, { key: "Enter", ctrlKey: true });

    await waitFor(() => expect(sent).toEqual({ question: "well?" }));
  });

  it("disables convene and gives the reason as the button's description when no roster is configured", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, { config: councilConfig({ configured: false }) }),
    );

    await renderCouncil("/council");
    fireEvent.change(await screen.findByLabelText("Question"), { target: { value: "well?" } });

    const button = await screen.findByRole("button", { name: "Convene" });
    const reason = "No roster in ~/.nucleos/council.yaml. Add one and restart the background service.";
    // The id is stable, so the description is read once it holds the reason.
    const describedBy = () => document.getElementById(button.getAttribute("aria-describedby") ?? "")?.textContent;
    await waitFor(() => expect(describedBy()).toBe(reason));
    expect(button.hasAttribute("disabled")).toBe(true);
    // And the case file says it in plain sight too.
    expect(within(dossier()).getByText(reason)).toBeDefined();
  });

  it("disables convene and draws no bounds sentence when the config is malformed", async () => {
    // A daemon that answers 200 with a body of the wrong shape: cast, as the wire would deliver it.
    const malformed = { configured: true, roles: [], default_roster: null } as unknown as CouncilConfig;
    daemon.apiFetch.mockImplementation(councilFetch([], {}, { config: malformed }));

    const { container } = await renderCouncil("/council");
    fireEvent.change(await screen.findByLabelText("Question"), { target: { value: "well?" } });

    expect((await screen.findAllByText("The council config could not be read.")).length).toBeGreaterThan(0);
    expect(convene().hasAttribute("disabled")).toBe(true);
    expect(container.textContent).not.toContain("at most");
    expect(container.textContent).not.toContain("undefined");
  });

  it("hides the courtroom setup while a council is open", async () => {
    const views: Record<string, CouncilView> = { "c-1": councilView({ id: "c-1" }) };
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], views));

    await renderCouncil("/council/c-1");

    await screen.findByRole("heading", { level: 1, name: "Council" });
    await screen.findAllByText(/should we ship the frontend rewrite\?/);
    expect(screen.queryByLabelText("Question")).toBeNull();
    expect(screen.queryByRole("button", { name: "Convene" })).toBeNull();
  });

  it("names chairs from 1", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}));

    await renderCouncil("/council");

    expect(await screen.findByRole("button", { name: /^Seat 1,/ })).toBeDefined();
    expect(screen.getByRole("button", { name: /^Seat 8,/ })).toBeDefined();
    expect(screen.queryByRole("button", { name: /^Seat 0,/ })).toBeNull();
  });

  it("keyboard: an arrow moves between chairs and Enter opens the chair in the dossier", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}));

    await renderCouncil("/council");
    const first = await screen.findByRole("button", { name: /^Seat 1,/ });
    first.focus();
    fireEvent.keyDown(first, { key: "ArrowRight" });

    const second = screen.getByRole("button", { name: /^Seat 2,/ });
    expect(document.activeElement).toBe(second);
    fireEvent.keyDown(second, { key: "Enter" });
    expect(within(dossier()).getByRole("heading", { level: 2, name: "Seat 2" })).toBeDefined();
  });
});

/* ------------------------------------------------------------- the verdict -- */

describe("Council - the verdict in the room", () => {
  it("sizes the chairs by Borda and ranks them at the verdict", async () => {
    const view = councilView({
      status: "done",
      current_phase: "synthesis",
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

    // The names come from the chairs' accessible names, not from a table.
    const first = await screen.findByRole("button", { name: /^Seat 2, the sceptic/ });
    const last = screen.getByRole("button", { name: /^Seat 1, gpt-5/ });
    // The drawn chairs are aria-hidden SVG groups, in seat order, under the HTML buttons.
    const groups = document.querySelectorAll(".council-court-seat");
    const firstDisc = groups[1]?.querySelector(".council-court-seat-disc");
    const lastDisc = groups[0]?.querySelector(".council-court-seat-disc");
    expect(firstDisc?.getAttribute("r")).toBe("24");
    expect(lastDisc?.getAttribute("r")).toBe("14");
    expect(first).toBeDefined();
    expect(last).toBeDefined();
    expect(groups[1]?.textContent).toContain("1st");
    expect(groups[0]?.textContent).toContain("2nd");
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

  it("a failed council's row says why it failed", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch(
        [councilSummary({ status: "error" })],
        { "c-1": councilView({ status: "error", error: "the chair ran out of budget" }) },
      ),
    );

    await renderCouncil("/council");

    const link = await screen.findByRole("link", { name: /should we ship the frontend rewrite\?/ });
    await waitFor(() => expect(link.textContent).toContain("the chair ran out of budget"));
  });

  it("a finished council's row carries its confidence and agreement", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch(
        [councilSummary({ status: "done", rounds_run: 1 })],
        {
          "c-1": synthesisView({
            status: "done",
            agreement: { tau: 0.42, level: "split", ballots: 4, comparisons: 6 },
          }),
        },
      ),
    );

    await renderCouncil("/council");

    const link = await screen.findByRole("link", { name: /should we ship the frontend rewrite\?/ });
    await waitFor(() => expect(link.textContent).toContain("high confidence · τ 0.42 · split"));
  });

  it("a running row says its round and phase and reads no detail", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch(
        [councilSummary({ rounds: 2, current_round: 1, current_phase: "critique" })],
        { "c-1": councilView() },
      ),
    );

    await renderCouncil("/council");

    const link = await screen.findByRole("link", { name: /should we ship the frontend rewrite\?/ });
    await waitFor(() => expect(link.textContent).toContain("round 1 · critique"));
    expect(daemon.apiFetch.mock.calls.map((call) => call[0])).not.toContain("/council/c-1");
  });

  it("reads the outcome of at most the ten newest settled councils", async () => {
    const summaries = Array.from({ length: 12 }, (_, i) =>
      councilSummary({ id: `c-${i + 1}`, status: "done", question: `question number ${i + 1}` }),
    );
    const views: Record<string, CouncilView> = {};
    for (const s of summaries) views[s.id] = synthesisView({ id: s.id, status: "done" });
    daemon.apiFetch.mockImplementation(councilFetch(summaries, views));

    await renderCouncil("/council");

    await screen.findByRole("link", { name: /question number 12/ });
    await waitFor(() =>
      expect(screen.getAllByText(/high confidence/).length).toBeGreaterThanOrEqual(10),
    );
    const details = daemon.apiFetch.mock.calls
      .map((call) => String(call[0]))
      .filter((path) => /^\/council\/c-\d+$/.test(path));
    expect(new Set(details).size).toBeGreaterThan(0);
    expect(new Set(details).size).toBeLessThanOrEqual(10);
  });

  it("with councils listed and none open draws no teach block", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], {}));

    await renderCouncil("/council");

    await screen.findByRole("link", { name: /should we ship the frontend rewrite\?/ });
    expect(screen.queryByRole("heading", { level: 3, name: "No council has met yet" })).toBeNull();
    expect(screen.queryByRole("heading", { level: 3, name: "Choose a council" })).toBeNull();
  });
});

/* ---------------------------------------------- the round-by-round timeline -- */

/**
 * Three named seats through two critique rounds. `anon_map` is label -> seat, so
 * "B" is beta and "C" is gamma. Every assertion below is scoped to the tab panel
 * or to a named group, because the seat grid above the timeline repeats the
 * answers and the latest step and would otherwise satisfy a query by accident.
 *
 * UI contract (what `CouncilRounds.tsx` renders as `CouncilDeliberation`):
 *  - a level-2 heading "Deliberation" (no "Seats" or "Rounds" heading) and a `tablist` named "Rounds"
 *    with one `tab` per round 0..lastRound: "Answers" (round 0), "Round 1", "Round 2"; the LAST round is
 *    selected on first render. Tabs are Radix (`ui/Tabs`), so they are selected by `mouseDown`, and only
 *    the selected `tabpanel` is mounted.
 *  - each tab panel holds a `list` named "Seats" with one `listitem` per seat, side by side: the seat's
 *    title, model, role, "Seat <n>" (1-based), then that round's step head and content.
 *  - round 0 panel: each seat's answer text, once, with a more/less button named
 *    "more of <seatName>'s answer" / "less of <seatName>'s answer".
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
    await openTheRecord();

    expect(await screen.findByRole("heading", { level: 2, name: "Deliberation" })).toBeDefined();
    const tabs = within(await screen.findByRole("tablist", { name: "Rounds" })).getAllByRole("tab");
    expect(tabs.map((tab) => tab.textContent)).toEqual(["Answers", "Round 1", "Round 2"]);
    expect(tabs[2].getAttribute("aria-selected")).toBe("true");

    const panel = await openRound("Answers");
    expect(panel.textContent).toContain("alpha");
    expect(panel.textContent).toContain("line one");
    expect(panel.textContent).toContain("beta says ship");
    expect(panel.textContent).toContain("gamma says wait");
  });

  it("draws one deliberation panel with the seats side by side and no separate seats panel", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": roundsView() }));

    await renderCouncil("/council/c-1");
    await openTheRecord();

    const panel = await panelFor("Deliberation");
    expect(screen.queryByRole("heading", { level: 2, name: "Seats" })).toBeNull();
    expect(screen.queryByRole("heading", { level: 2, name: "Rounds" })).toBeNull();
    const seats = within(panel).getByRole("list", { name: "Seats" });
    expect(within(seats).getAllByRole("listitem", { hidden: false }).filter((li) => li.parentElement === seats)).toHaveLength(3);
  });

  it("opens on the last round", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": roundsView() }));

    await renderCouncil("/council/c-1");
    await openTheRecord();

    const tab = await screen.findByRole("tab", { name: "Round 2" });
    expect(tab.getAttribute("aria-selected")).toBe("true");
    expect(screen.getByRole("tab", { name: "Answers" }).getAttribute("aria-selected")).toBe("false");
  });

  it("shows each seat's answer exactly once", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": roundsView() }));

    await renderCouncil("/council/c-1");
    await openTheRecord();
    await openRound("Answers");

    expect(screen.getAllByText("beta says ship")).toHaveLength(1);
  });

  it("names more and less after the seat", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": roundsView() }));

    await renderCouncil("/council/c-1");
    await openTheRecord();
    await openRound("Answers");

    const more = await screen.findByRole("button", { name: "more of alpha's answer" });
    expect(more.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(more);
    const less = await screen.findByRole("button", { name: "less of alpha's answer" });
    expect(less.getAttribute("aria-expanded")).toBe("true");
  });

  it("labels a seat by its 1-based position", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": roundsView() }));

    await renderCouncil("/council/c-1");
    await openTheRecord();

    const panel = await panelFor("Deliberation");
    expect(within(panel).getByText("Seat 1")).toBeDefined();
    expect(within(panel).queryByText("Seat 0")).toBeNull();
  });

  it("lays the seats out in a 16rem auto-fill grid and leaves no dead flex on the row question", () => {
    const css = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "council.css"), "utf8").replace(
      /\/\*[\s\S]*?\*\//g,
      "",
    );
    const body = (rule: RegExp) => rule.exec(css)?.[1] ?? "";

    expect(body(/\.council-seats\s*\{([^}]*)\}/)).toMatch(
      /grid-template-columns:\s*repeat\(\s*auto-fill\s*,\s*minmax\(\s*16rem\s*,\s*1fr\s*\)\s*\)/,
    );
    expect(body(/\.council-row-question\s*\{([^}]*)\}/)).not.toMatch(/(^|[;\s])flex(-[a-z]+)?\s*:/);
  });

  it("de-anonymises a ballot into seat names, best first", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": roundsView() }));

    await renderCouncil("/council/c-1");
    await openTheRecord();
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
    await openTheRecord();
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
    await openTheRecord();
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
    await openTheRecord();
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
    await openTheRecord();
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
    await openTheRecord();
    expect(await screen.findByText(/Stopped early at round 1/)).toBeDefined();
    first.unmount();

    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": roundsView() }));
    await renderCouncil("/council/c-1");
    await openTheRecord();
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
    const cases: Array<[string, RegExp]> = [
      ["strong", /τ 0\.40 · strong consensus/],
      ["split", /τ 0\.40 · split/],
      ["none", /τ 0\.40 · no consensus/],
      ["insufficient", /too few votes/],
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
    expect(within(panel).queryByText(/^τ |too few votes/)).toBeNull();
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
    // Nobody speaks any more, so nothing is followed: the seat is read by choosing it.
    fireEvent.click(await screen.findByRole("button", { name: /^Seat 1,/ }));
    expect(await within(dossier()).findByText("the settled answer")).toBeDefined();
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
    await screen.findByRole("complementary", { name: "Dossier" });
    expect(screen.queryByRole("log")).toBeNull();
    first.unmount();

    const ended = pendingView({ status: "error" });
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary({ status: "error" })], { "c-1": ended }, { tail }),
    );
    await renderCouncil("/council/c-1");
    await screen.findByRole("complementary", { name: "Dossier" });
    expect(screen.queryByRole("log")).toBeNull();
    expect(tailAsks()).toHaveLength(0);
  });

  it("keeps the deliberation columns' own tail working with the record open", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], { "c-1": pendingView() }, { tail: () => ({ text: "weighing", next: 8, live: true }) }),
    );

    await renderCouncil("/council/c-1");
    // The dossier follows the speaking seat; the opened record tails it a second time.
    await openTheRecord();
    await waitFor(() => expect(screen.getAllByRole("log", { name: "Live tail of alpha" })).toHaveLength(2));
  });

  it("follows the speaking seat and a click pins another, with Follow live to return", async () => {
    const view = pendingView({
      seats: [
        seatNamed("alpha", 0, [stepView({ status: "pending", run_id: 7, answer: null })]),
        seatNamed("beta", 1, [stepView({ run_id: 8, answer: "beta's opening answer" })]),
      ],
    });
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], { "c-1": view }, { tail: () => ({ text: "weighing", next: 8, live: true }) }),
    );

    await renderCouncil("/council/c-1");
    // Nobody pinned anything: the dossier is already on whoever speaks.
    await screen.findByRole("log", { name: "Live tail of alpha" });
    expect(screen.queryByRole("button", { name: "Follow live" })).toBeNull();

    fireEvent.click(await screen.findByRole("button", { name: /^Seat 2,/ }));
    expect(await within(dossier()).findByText("beta's opening answer")).toBeDefined();
    expect(screen.queryByRole("log", { name: "Live tail of alpha" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Follow live" }));
    expect(await screen.findByRole("log", { name: "Live tail of alpha" })).toBeDefined();
    expect(screen.queryByRole("button", { name: "Follow live" })).toBeNull();
  });

  it("gives every chair its state in its accessible name", async () => {
    const view = pendingView({
      seats: [
        seatNamed("alpha", 0, [stepView({ status: "pending", run_id: null, answer: null })]),
        seatNamed("beta", 1, [stepView({ status: "pending", run_id: 7, answer: null })]),
        seatNamed("gamma", 2, [stepView({ run_id: 8 })]),
        seatNamed("delta", 3, [stepView({ run_id: 9, status: "invalid", answer: null })]),
      ],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    expect(await screen.findByRole("button", { name: "Seat 1, alpha, thinking" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Seat 2, beta, speaking" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Seat 3, gamma, done" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Seat 4, delta, failed: unreadable answer" })).toBeDefined();
  });
});

describe("Council - the detail, redesigned", () => {
  it("draws the court, the dossier and the process line, and the record shut", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": synthesisView() }));

    await renderCouncil("/council/c-1");
    await panelFor("Synthesis");

    expect(screen.getByRole("region", { name: "Court" })).toBeDefined();
    expect(within(dossier()).getByRole("heading", { level: 2, name: "Synthesis" })).toBeDefined();
    expect(within(screen.getByRole("list", { name: "Process" })).getAllByRole("listitem")).toHaveLength(5);
    // The record is the last thing on the page and starts shut.
    const record = await panelFor("Case record");
    const toggle = within(record).getByRole("button", { name: "Show case record" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    // Shut, the panel is its heading alone: no empty padded box underneath.
    expect(record.querySelector(".ui-panel-body")).toBeNull();
    expect(screen.queryByRole("heading", { level: 2, name: "Deliberation" })).toBeNull();
    // The leaderboard panel is gone: the room's chairs carry the ranking.
    expect(screen.queryByRole("heading", { level: 2, name: "Leaderboard" })).toBeNull();

    fireEvent.click(toggle);
    expect(await screen.findByRole("heading", { level: 2, name: "Deliberation" })).toBeDefined();
    expect(within(record).getByRole("button", { name: "Hide case record" }).getAttribute("aria-expanded")).toBe("true");
  });

  it("replaces the list and composer with a way back while a council is open", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": synthesisView() }));

    await renderCouncil("/council/c-1");
    await panelFor("Synthesis");

    expect(screen.queryByRole("heading", { level: 2, name: "Councils" })).toBeNull();
    expect(screen.queryByRole("region", { name: "Councils" })).toBeNull();
    const back = screen.getByRole("link", { name: "Councils" });
    expect(back.getAttribute("href")).toBe("/council");
    expect(screen.queryByLabelText("Question")).toBeNull();
  });

  it("says why the chairman is confident in visible text", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": synthesisView() }));

    await renderCouncil("/council/c-1");
    const panel = await panelFor("Synthesis");

    const level = await within(panel).findByText("high confidence");
    expect(level.getAttribute("title")).toBeNull();
    expect(within(panel).getByText("every ballot agrees")).toBeDefined();
    expect(panel.querySelector("[title]")).toBeNull();
  });

  it("offers ask again on a settled council and fills the room with its question, panel and rounds", async () => {
    let sent: unknown = "nothing was posted";
    const view = councilView({
      status: "done",
      rounds: 3,
      rounds_run: 3,
      question: "is the migration safe?",
      chairman_kind: "cloud",
      chairman_ref: "claude-opus-5",
      chairman_agent_id: "ag-1",
      chairman_agent_name: "the sceptic",
      seats: [
        seatView({ seat_idx: 0, kind: "local", ref: "qwen3.5:4b", role: "skeptic" }),
        seatView({ seat_idx: 1, kind: "cloud", ref: "claude-opus-5", agent_id: "ag-1", agent_name: "the sceptic" }),
      ],
    });
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], { "c-1": view }, {
        agents: [agentRow()],
        models: [modelRow(), modelRow({ id: "qwen3.5:4b", label: "Qwen 4b", brain: "local" })],
        onCreate: (body) => {
          sent = body;
          return { id: "new-1" };
        },
      }),
    );

    const { router } = await renderCouncil("/council/c-1");
    fireEvent.click(await screen.findByRole("button", { name: "Ask again" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/council"));
    const question = (await screen.findByLabelText("Question")) as HTMLTextAreaElement;
    expect(question.value).toBe("is the migration safe?");
    // The room holds the old panel: chairs by their names, the chair presiding,
    // the role a chair played, and the rounds on the process line.
    expect(await screen.findByRole("button", { name: "Seat 1, qwen3.5:4b, skeptic" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Seat 2, ag-1" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Chairman, ag-1" })).toBeDefined();
    await waitFor(() => expect(processPhaseLabels()).toHaveLength(7));
    await conveneWhenReady();

    await waitFor(() =>
      expect(sent).toEqual({
        question: "is the migration safe?",
        roster: {
          chairman: { agent: "ag-1" },
          members: [
            { kind: "local", ref: "qwen3.5:4b" },
            { agent: "ag-1" },
          ],
        },
        roles: { "0": "skeptic" },
        rounds: 3,
      }),
    );
  });

  it("offers no ask again while a council runs", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], { "c-1": councilView({ status: "running" }) }),
    );

    await renderCouncil("/council/c-1");
    await screen.findByRole("button", { name: "Stop council" });

    expect(screen.queryByRole("button", { name: "Ask again" })).toBeNull();
  });

  it("ask again leaves a deleted agent's seat as an empty chair", async () => {
    const view = councilView({
      status: "done",
      seats: [
        seatView({ seat_idx: 0, kind: "cloud", ref: "claude-opus-5", agent_id: "ag-gone", agent_name: null }),
      ],
      chairman_ref: "claude-opus-5",
    });
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], { "c-1": view }, {
        agents: [agentRow()],
        models: [modelRow()],
      }),
    );

    const { router } = await renderCouncil("/council/c-1");
    fireEvent.click(await screen.findByRole("button", { name: "Ask again" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/council"));

    expect(await screen.findByRole("button", { name: "Seat 1, empty" })).toBeDefined();
    // Nobody else sits, and the minimum is one.
    const button = screen.getByRole("button", { name: "Convene" });
    await waitFor(() => expect(reasonOf(button)).toBe("Seat at least one member."));
    expect(button.hasAttribute("disabled")).toBe(true);
  });

  it("draws even chairs when nobody could be ranked", async () => {
    const view = councilView({ status: "done", current_phase: "synthesis", leaderboard: [] });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");

    const chair = await screen.findByRole("button", { name: /^Seat 1,/ });
    const disc = document.querySelector(".council-court-seat .council-court-seat-disc");
    expect(disc?.getAttribute("r")).toBe("18");
    expect(chair).toBeDefined();
    expect(document.querySelector(".council-court-seat")?.textContent).not.toMatch(/\d(st|nd|rd|th)/);
  });

  it("going back: a phase shows that round's masks and its scales, and the live phase returns to live", async () => {
    const view = synthesisView({
      ballot_rounds: [1, 2],
      leaderboard_by_round: [
        [
          { seat_idx: 0, score: 1, n: 3 },
          { seat_idx: 1, score: 0.5, n: 3 },
        ],
        [
          { seat_idx: 1, score: 1, n: 3 },
          { seat_idx: 0, score: 0.5, n: 3 },
        ],
      ],
      agreement_by_round: [
        { tau: 0.3, level: "split", ballots: 3, comparisons: 3 },
        { tau: 0.8, level: "strong", ballots: 3, comparisons: 3 },
      ],
    });
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": view }));

    await renderCouncil("/council/c-1");
    const room = await screen.findByRole("region", { name: "Court" });
    const process = screen.getByRole("list", { name: "Process" });
    // Live: the verdict, with the real names and the final scales.
    // The scale reads twice in the room (the drawn balance and the summary), so ask for all of them.
    expect(within(room).getAllByText("strong consensus · τ 0.90").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: /^Seat 1, alpha/ })).toBeDefined();
    expect(within(process).getByRole("button", { name: "Verdict" }).getAttribute("aria-current")).toBe("step");

    fireEvent.click(within(process).getByRole("button", { name: "Deliberation I" }));

    // Round 1: the critique masks the seats and the scales are that round's own.
    expect(await screen.findByRole("button", { name: /^Seat 1, A/ })).toBeDefined();
    expect(within(room).getAllByText("split · τ 0.30").length).toBeGreaterThan(0);
    expect(within(process).getByRole("button", { name: "Deliberation I" }).getAttribute("aria-current")).toBe("step");

    fireEvent.click(within(process).getByRole("button", { name: "Verdict" }));
    expect(await screen.findByRole("button", { name: /^Seat 1, alpha/ })).toBeDefined();
    expect(within(room).getAllByText("strong consensus · τ 0.90").length).toBeGreaterThan(0);
  });

  it("the verdict opens the structured synthesis in the dossier", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], { "c-1": synthesisView() }));

    await renderCouncil("/council/c-1");

    expect(await within(await screen.findByRole("complementary", { name: "Dossier" })).findByRole("heading", { level: 2, name: "Synthesis" })).toBeDefined();
    expect(within(dossier()).getByText("Ship it behind a flag.")).toBeDefined();
  });
});

/* ------------------------------------------------------- the review rework -- */

const DETAIL_KEY = [...keys.council.all, "detail", "c-1"];

describe("Council - review rework", () => {
  it("F1: a row whose council settled after its detail was cached running shows the confidence", async () => {
    const summaries = [councilSummary({ status: "running" })];
    const views: Record<string, CouncilView> = { "c-1": councilView({ status: "running" }) };
    daemon.apiFetch.mockImplementation(councilFetch(summaries, views));

    const { queryClient } = await renderCouncil("/council/c-1");
    await screen.findByRole("complementary", { name: "Dossier" });

    fireEvent.click(screen.getByRole("link", { name: "Councils" }));
    const link = await screen.findByRole("link", { name: /should we ship the frontend rewrite\?/ });
    expect(link.textContent).toContain("round");

    // The list poll lands the council as done; its detail is done with a synthesis.
    summaries[0] = councilSummary({ status: "done", rounds_run: 1 });
    views["c-1"] = synthesisView({ status: "done" });
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: [...keys.council.all, "list"] });
    });

    await waitFor(() => expect(link.textContent).toContain("high confidence"));
  });

  it("F2: the Round 1 tab and its live tail follow a council that advances while it is open", async () => {
    const views: Record<string, CouncilView> = {
      "c-1": pendingView({ rounds: 2, current_round: 0 }),
    };
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], views));

    const { queryClient } = await renderCouncil("/council/c-1");
    await openTheRecord();
    const first = await screen.findByRole("tab", { name: "Answers" });
    expect(first.getAttribute("aria-selected")).toBe("true");

    views["c-1"] = pendingView({
      rounds: 2,
      current_round: 1,
      current_phase: "critique",
      seats: [
        seatNamed("alpha", 0, [
          stepView({ status: "ok" }),
          stepView({ round: 1, phase: "critique", run_id: 8, status: "pending", answer: null }),
        ]),
      ],
    });
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: DETAIL_KEY });
    });

    await waitFor(() =>
      expect(screen.getByRole("tab", { name: "Round 1" }).getAttribute("aria-selected")).toBe("true"),
    );
    // The dossier tails the speaking seat and so does the opened column.
    expect((await screen.findAllByRole("log", { name: "Live tail of alpha" })).length).toBeGreaterThan(0);
  });

  it("F2: a tab the reader picked while the council runs is kept when the round advances", async () => {
    const views: Record<string, CouncilView> = {
      "c-1": pendingView({
        rounds: 2,
        current_round: 1,
        seats: [
          seatNamed("alpha", 0, [
            stepView({ status: "ok" }),
            stepView({ round: 1, phase: "critique", run_id: 8, status: "pending", answer: null }),
          ]),
        ],
      }),
    };
    daemon.apiFetch.mockImplementation(councilFetch([councilSummary()], views));

    const { queryClient } = await renderCouncil("/council/c-1");
    await openTheRecord();
    expect((await screen.findByRole("tab", { name: "Round 1" })).getAttribute("aria-selected")).toBe("true");
    await openRound("Answers");
    expect(screen.getByRole("tab", { name: "Answers" }).getAttribute("aria-selected")).toBe("true");

    views["c-1"] = pendingView({
      rounds: 2,
      current_round: 2,
      seats: [
        seatNamed("alpha", 0, [
          stepView({ status: "ok" }),
          critiqueStep(1, ["B"]),
          stepView({ round: 1, phase: "revise", run_id: 9, answer: "again", changed: true, why: "x" }),
          stepView({ round: 2, phase: "critique", run_id: 10, status: "pending", answer: null }),
        ]),
      ],
    });
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: DETAIL_KEY });
    });

    await screen.findByRole("tab", { name: "Round 2" });
    expect(screen.getByRole("tab", { name: "Answers" }).getAttribute("aria-selected")).toBe("true");
    expect(screen.getByRole("tab", { name: "Round 2" }).getAttribute("aria-selected")).toBe("false");
  });

  it("F3: with no roster configured the room is empty and never claims a default", async () => {
    daemon.apiFetch.mockImplementation(
      councilFetch([], {}, { config: councilConfig({ configured: false }) }),
    );

    await renderCouncil("/council");
    await screen.findAllByText("No roster in ~/.nucleos/council.yaml. Add one and restart the background service.");

    expect(screen.getAllByRole("button", { name: /^Seat \d, empty$/ })).toHaveLength(8);
    expect(screen.getByRole("button", { name: "Chairman, empty" })).toBeDefined();
    expect(screen.queryByRole("button", { name: /default/i })).toBeNull();
  });

  it("F4: a failed background refetch of the config leaves Convene enabled", async () => {
    let failing = false;
    const inner = councilFetch([], {});
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (failing && path === "/council/config") throw new ApiRefusal(500, "internal", "config blew up");
      return inner(path, init);
    });

    const { queryClient } = await renderCouncil("/council");
    fireEvent.change(await screen.findByLabelText("Question"), { target: { value: "well?" } });
    const convene = screen.getByRole("button", { name: "Convene" });
    await waitFor(() => expect(convene.hasAttribute("disabled")).toBe(false));

    failing = true;
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: [...keys.council.all, "config"] });
    });
    await waitFor(() =>
      expect(queryClient.getQueryState([...keys.council.all, "config"])?.status).toBe("error"),
    );

    expect(screen.queryByText("The council config could not be read.")).toBeNull();
    expect(screen.getByRole("button", { name: "Convene" }).hasAttribute("disabled")).toBe(false);
  });

  it("F5: ask again with a seat no longer offered leaves that chair empty and Convene does not wait", async () => {
    const view = councilView({
      status: "done",
      chairman_ref: "claude-opus-5",
      seats: [
        seatView({ seat_idx: 0, kind: "cloud", ref: "model-that-left" }),
        seatView({ seat_idx: 1, kind: "cloud", ref: "claude-opus-5" }),
      ],
    });
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], { "c-1": view }, {
        agents: [agentRow()],
        models: [modelRow()],
      }),
    );

    const { router } = await renderCouncil("/council/c-1");
    fireEvent.click(await screen.findByRole("button", { name: "Ask again" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/council"));

    // Both catalogues have to answer before a seat is judged gone.
    expect(await screen.findByRole("button", { name: "Seat 1, empty" })).toBeDefined();
    expect(screen.getByRole("button", { name: "Seat 2, claude-opus-5" })).toBeDefined();
    fireEvent.change(screen.getByLabelText("Question"), { target: { value: "well?" } });
    await waitFor(() => expect(convene().hasAttribute("disabled")).toBe(false));
  });

  it("F5: ask again with the chairman no longer offered waits for a chairman", async () => {
    const view = councilView({
      status: "done",
      chairman_ref: "model-that-left",
      seats: [seatView({ seat_idx: 0, kind: "cloud", ref: "claude-opus-5" })],
    });
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], { "c-1": view }, {
        agents: [agentRow()],
        models: [modelRow()],
      }),
    );

    const { router } = await renderCouncil("/council/c-1");
    fireEvent.click(await screen.findByRole("button", { name: "Ask again" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/council"));

    expect(await screen.findByRole("button", { name: "Chairman, empty" })).toBeDefined();
    fireEvent.change(screen.getByLabelText("Question"), { target: { value: "well?" } });
    const button = screen.getByRole("button", { name: "Convene" });
    await waitFor(() => expect(reasonOf(button)).toBe("Choose a chairman."));
    expect(button.hasAttribute("disabled")).toBe(true);
  });

  it("F5: ask again clamps draft rounds above the config's maximum", async () => {
    const view = councilView({ status: "done", rounds: 5, rounds_run: 5 });
    daemon.apiFetch.mockImplementation(
      councilFetch([councilSummary()], { "c-1": view }, { config: councilConfig({ max_rounds: 3 }) }),
    );

    const { router } = await renderCouncil("/council/c-1");
    fireEvent.click(await screen.findByRole("button", { name: "Ask again" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/council"));

    // Three rounds on the process line, not five.
    await waitFor(() => expect(processPhaseLabels()).toHaveLength(7));
  });

  it("F7: ctrl+enter while an IME is composing does not convene", async () => {
    daemon.apiFetch.mockImplementation(councilFetch([], {}));

    await renderCouncil("/council");
    const question = await screen.findByLabelText("Question");
    fireEvent.change(question, { target: { value: "well?" } });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Convene" }).hasAttribute("disabled")).toBe(false),
    );

    fireEvent.keyDown(question, { key: "Enter", ctrlKey: true, isComposing: true });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });

    const posted = daemon.apiFetch.mock.calls.filter(
      (call) => call[0] === "/council" && (call[1] as RequestInit | undefined)?.method === "POST",
    );
    expect(posted).toHaveLength(0);
  });
});
