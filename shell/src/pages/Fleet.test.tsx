import { act, createElement } from "react";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";

/**
 * jsdom 29 implements neither of these, and xyflow needs both: the observer to
 * learn how big its viewport is, the matrix to read a CSS transform back. They
 * are stubbed here rather than in `test-setup.ts` on purpose — the absence is a
 * fact about *this* page's dependency, and putting it in the global setup would
 * hand every other suite a fake it never asked for.
 */
beforeAll(() => {
  const scope = globalThis as unknown as Record<string, unknown>;
  scope.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
  scope.DOMMatrixReadOnly ??= class {
    m22 = 1;
    constructor(_transform?: string) {}
  };
});

/**
 * The real xyflow, with a tap on the two props the §9.2 spike is about.
 *
 * Wrapping rather than replacing: the canvas really mounts, so this also proves
 * the library renders under jsdom at all, and the recorded props let the test
 * assert the reference identity xyflow's own reconciliation depends on.
 */
const seen = vi.hoisted(() => ({ nodeTypes: [] as unknown[], edgeTypes: [] as unknown[] }));
vi.mock("@xyflow/react", async (original) => {
  const real = await original<typeof import("@xyflow/react")>();
  return {
    ...real,
    ReactFlow: (props: Record<string, unknown>) => {
      seen.nodeTypes.push(props.nodeTypes);
      seen.edgeTypes.push(props.edgeTypes);
      return createElement(real.ReactFlow as never, props);
    },
  };
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Fleet } from "./Fleet";
import { FleetCanvas } from "../canvas/FleetCanvas";
import { isValidConnection, loadLayout, saveLayout, slotDetail } from "../canvas/model";
import { ApiRefusal } from "../data/client";
import { keys } from "../data/keys";
import type {
  Concurrency,
  FleetExclusion,
  HeldSlot,
  Job,
  JobDetail,
  ProjectConcurrency,
  RunSearchResult,
} from "../data/fleet";
import { daemonFetch, daemonState, project, proposal, renderWithRouter } from "../test/harness";
import type { Proposal } from "../data/system";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  localStorage.clear();
});

/* ------------------------------------------------------------- fixtures -- */

function slot(overrides: Partial<HeldSlot> = {}): HeldSlot {
  return {
    project_id: "alpha",
    slot: 0,
    owner_kind: "job",
    owner_id: 41,
    claimed_at: "2026-08-17T09:00:00Z",
    // The daemon joins these on for an item and leaves them null for everything
    // else, so null is what the default fixture — a job's slot — really carries.
    job_id: null,
    ordinal: null,
    item_status: null,
    ...overrides,
  };
}

function column(overrides: Partial<ProjectConcurrency> = {}): ProjectConcurrency {
  return {
    project_id: "alpha",
    limit: 2,
    slots: [],
    collision: {
      declared: { state: "clean", overlaps: [] },
      observed: { state: "clean", overlaps: [] },
    },
    ...overrides,
  };
}

function job(overrides: Partial<Job> = {}): Job {
  return {
    id: 41,
    project_id: "alpha",
    rule_name: null,
    status: "implementing",
    wait_reason: null,
    max_items: 4,
    created_at: "2026-08-17T09:00:00Z",
    completed_at: null,
    slot: 0,
    round: 0,
    max_rounds: 3,
    ...overrides,
  };
}

function run(overrides: Partial<RunSearchResult> = {}): RunSearchResult {
  return {
    id: 7,
    project_id: "alpha",
    status: "running",
    mode: "worktree",
    created_at: "2026-08-17T09:00:00Z",
    completed_at: null,
    cost_usd: null,
    prompt_excerpt: "tidy the imports",
    ...overrides,
  };
}

interface FleetState {
  concurrency: Concurrency | undefined;
  jobs: Job[] | undefined;
  runs: RunSearchResult[] | undefined;
  exclusions: FleetExclusion[];
  requests: Proposal[];
  details: Record<number, JobDetail>;
}

function fleetState(overrides: Partial<FleetState> = {}): FleetState {
  return {
    concurrency: { house: { limit: 4, held: 0 }, projects: [] },
    jobs: [],
    runs: [],
    exclusions: [],
    requests: [],
    details: {},
    ...overrides,
  };
}

/**
 * The fleet's routes, over the foundation's responder.
 *
 * A local switch rather than an edit to `test/harness.tsx`: the harness is the
 * shared floor, and a page that teaches it seven routes of its own makes every
 * other suite carry them. Anything this does not know falls through to the
 * shared responder, so `/projects` and `/autopilot/budget` still answer.
 */
function fleetFetch(state: FleetState): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonFetch(daemonState({ projects: [project({ project_id: "alpha" })] }));
  return async (path, init) => {
    if (init?.method !== undefined && init.method !== "GET") return await shared(path, init);
    switch (path) {
      case "/concurrency":
        return state.concurrency;
      case "/jobs?live=true":
        return state.jobs;
      case "/runs?live=true&limit=200":
        return state.runs;
      case "/fleet/exclusions":
        return state.exclusions;
      case "/fleet/exclusions/requests":
        return state.requests;
      default: {
        const detail = /^\/jobs\/(\d+)$/.exec(path);
        if (detail !== null) return state.details[Number(detail[1])];
        return await shared(path, init);
      }
    }
  };
}

/* ------------------------------------------------------------ the columns -- */

describe("Fleet — columns", () => {
  it("shows the house capacity and gives an idle project a column at 0/N", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 5, held: 2 },
            projects: [
              column({
                project_id: "alpha",
                limit: 2,
                slots: [
                  slot({ slot: 0, owner_kind: "job", owner_id: 41 }),
                  slot({ slot: 1, owner_kind: "run", owner_id: 7 }),
                ],
              }),
              column({ project_id: "beta", limit: 3, slots: [] }),
            ],
          },
          jobs: [job()],
          runs: [run()],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);

    const house = await screen.findByRole("article", { name: "In flight" });
    expect(within(house).getByText("2/5")).toBeDefined();

    // Capacity is what the core says it is: the header counts slots, not cards.
    const alpha = await screen.findByRole("region", { name: "alpha column" });
    expect(within(alpha).getByText("2/2")).toBeDefined();
    expect(within(alpha).getByRole("article", { name: /job 41/ })).toBeDefined();
    // A worktree run holds a slot without being a job — a page that counted
    // jobs would say `1/2` about a project that is already full.
    const held = within(alpha).getByRole("article", { name: "slot 1 — run 7" });
    expect(within(held).getByText("tidy the imports")).toBeDefined();
    expect(within(held).getByRole("link", { name: "Open in Runs" })).toBeDefined();

    // An idle project KEEPS its column. One that vanished when it emptied would
    // move every other column across the screen every night that ended, and a
    // project's position is the one thing the reader memorises.
    const beta = await screen.findByRole("region", { name: "beta column" });
    expect(within(beta).getByText("0/3")).toBeDefined();
    expect(within(beta).getByText(/nothing in flight/)).toBeDefined();
  });

  it("keeps a slot with no description apart from a slot nothing is working in", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 4, held: 2 },
            projects: [
              column({
                limit: 2,
                slots: [
                  slot({ slot: 0, owner_kind: "job", owner_id: 41 }),
                  slot({ slot: 1, owner_kind: "run", owner_id: 7 }),
                ],
              }),
            ],
          },
          // The jobs listing answered in full and job 41 was not in it: the slot
          // is leaked, and `reconcile_orphaned_slots` has not swept it yet.
          jobs: [],
          // The runs listing did not answer at all. Ordinary — and a completely
          // different fact from the one above.
          runs: undefined,
        }),
      ),
    );

    await renderWithRouter(<Fleet />);

    const leaked = await screen.findByRole("article", { name: "slot 0 — job 41" });
    expect(within(leaked).getByText(/awaiting reconciliation/i)).toBeDefined();

    const undescribed = await screen.findByRole("article", { name: "slot 1 — run 7" });
    expect(within(undescribed).getByText(/detail unavailable/i)).toBeDefined();
    expect(undescribed.textContent).not.toMatch(/reconciliation/i);
  });

  it("reads a listing that arrived at its ceiling as undescribed, never as leaked", () => {
    // A list exactly at `LIVE_LIST_LIMIT` may have been cut, so the owner's
    // absence from it proves nothing. Asserted on the pure function because the
    // rendered version of this case needs two hundred fixtures to say the same
    // thing.
    const cut = slotDetail(slot({ owner_id: 41 }), [job({ id: 99 })], undefined, 1);
    expect(cut.kind).toBe("unknown");
    const whole = slotDetail(slot({ owner_id: 41 }), [job({ id: 99 })], undefined, 50);
    expect(whole.kind).toBe("orphaned");
  });

  it("describes an item's slot through its job, and never through the run that shares its number", () => {
    // One item of a job a team directs holds a slot of its own, and `owner_id`
    // for it is `job_items.id` — a number no route lists and no reader knows.
    // Until it had an arm here, anything that was not a job fell through to the
    // runs listing, so the card carried the status and prompt of an unrelated run
    // that happened to be numbered the same: the exact confusion `ownerKey` was
    // written to prevent, arriving through the door nobody was watching.
    const item = slot({ slot: 1, owner_kind: "item", owner_id: 7, job_id: 41, ordinal: 2, item_status: "running" });
    const detail = slotDetail(item, [job({ id: 41 })], [run({ id: 7 })]);

    expect(detail).toEqual({ kind: "item", job: job({ id: 41 }), ordinal: 2, status: "running" });

    // And the run that shares the number is still described as itself.
    const sharing = slot({ slot: 1, owner_kind: "run", owner_id: 7 });
    expect(slotDetail(sharing, [job()], [run({ id: 7 })]).kind).toBe("run");
  });

  it("says it cannot describe an item whose job it was not told, rather than guessing", () => {
    // `job_id` absent means the slot's item row has gone, which is a leaked slot
    // waiting on `reconcile_orphaned_slots` and not a description problem. It
    // reads `unknown` and never `orphaned`: the second is a claim about a LISTING
    // that answered without it, and here no listing was consulted at all.
    const orphanedSlot = slot({ slot: 1, owner_kind: "item", owner_id: 7, job_id: null });
    expect(slotDetail(orphanedSlot, [job()], [run({ id: 7 })]).kind).toBe("unknown");

    // And a job that IS named but is not in a complete listing is the leak the
    // job arm already reports, reported the same way.
    const gone = slot({ slot: 1, owner_kind: "item", owner_id: 7, job_id: 99, ordinal: 0, item_status: "running" });
    expect(slotDetail(gone, [job({ id: 41 })], undefined, 50).kind).toBe("orphaned");
  });

  it("shows an item's card its own collision, and no cancel it has no route for", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 4, held: 2 },
            projects: [
              column({
                slots: [
                  slot({ slot: 0, owner_kind: "item", owner_id: 7 }),
                  slot({ slot: 1, owner_kind: "item", owner_id: 8 }),
                ],
                collision: {
                  declared: { state: "clean", overlaps: [] },
                  // Two items of one job, named apart. While the daemon
                  // collapsed a job's items into `job:<id>`, this pair had
                  // nowhere to appear and the card said nothing at all.
                  observed: {
                    state: "collide",
                    overlaps: [
                      {
                        a: { kind: "item", id: 7 },
                        b: { kind: "item", id: 8 },
                        paths: ["core/src/job.rs"],
                      },
                    ],
                  },
                },
              }),
            ],
          },
        }),
      ),
    );

    await renderWithRouter(<Fleet />);

    const mine = await screen.findByRole("article", { name: "slot 0 — item 7" });
    expect(within(mine).getByText(/also touched by item 8/)).toBeDefined();
    expect(within(mine).getByText(/core\/src\/job\.rs/)).toBeDefined();
    // No `/items/<id>/cancel` exists, and the number would aim the one route
    // there is at somebody else's run. The gesture that stops this work is the
    // job's, on the job's card.
    expect(within(mine).queryByRole("button", { name: "Cancel" })).toBeNull();
  });

  it("an item's card says which item of which job it is", async () => {
    // The half that was missing while the card could only print `item 7`: an id
    // out of a sequence nobody reads, next to a slot number, next to nothing.
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 4, held: 2 },
            projects: [
              column({
                slots: [
                  slot({ slot: 0, owner_id: 41 }),
                  slot({
                    slot: 1,
                    owner_kind: "item",
                    owner_id: 7,
                    job_id: 41,
                    ordinal: 2,
                    item_status: "conflicted",
                  }),
                ],
              }),
            ],
          },
          jobs: [job({ id: 41, rule_name: "nightly-backlog" })],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);

    const card = await screen.findByRole("article", { name: "slot 1 — item 7" });
    // Counting from one on screen and from zero in the row, like every other
    // place this repository prints an ordinal to a person.
    expect(within(card).getByText(/item 3 of job 41/)).toBeDefined();
    expect(within(card).getByText(/nightly-backlog/)).toBeDefined();
    // A slot is held from the claim until the item is terminal, so "it holds a
    // slot" says nothing about whether it is working. This one is waiting on a
    // person, and until `itemReading` learned the four states of a parallel item
    // every one of them fell to its default and read as "to do".
    expect(within(card).getByText(/the merge conflicted/)).toBeDefined();
    expect(
      within(card).queryByText(/detail unavailable/i),
      "the card can describe itself now, so it must stop saying it cannot",
    ).toBeNull();
    expect(
      within(card).queryByText(/to do/),
      "an item mid-conflict is not work nobody has started",
    ).toBeNull();
  });

  it("keeps the last good cards and says the view is stale when a refetch fails", async () => {
    const state = fleetState({
      concurrency: {
        house: { limit: 4, held: 1 },
        projects: [column({ slots: [slot()] })],
      },
      jobs: [job()],
    });
    let answering = true;
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (!answering && path === "/concurrency") throw new ApiRefusal(503, "unavailable", "");
      return await fleetFetch(state)(path, init);
    });

    const { queryClient } = await renderWithRouter(<Fleet />);
    await screen.findByRole("article", { name: "slot 0 — job 41" });
    // The form is here while the reading is the daemon's.
    expect(screen.getByRole("button", { name: "New job" })).toBeDefined();

    answering = false;
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: keys.concurrency });
    });

    // `findBy` and not `getBy`: react-query hands observer notifications to a
    // `setTimeout(…, 0)`, so the refetch settling inside `act` is not the same
    // moment as the render that follows it.
    expect(await screen.findByText(/view is stale/)).toBeDefined();
    // Blanking would read as "there is room", which is the one wrong thing this
    // page can say. The card stays, and the note says how old it is.
    expect(screen.getByRole("article", { name: "slot 0 — job 41" })).toBeDefined();
    // And the action that would act on capacity nobody can vouch for is gone
    // rather than disabled — it fails before the click instead of after it.
    expect(screen.queryByRole("button", { name: "New job" })).toBeNull();
  });
});

/* -------------------------------------------------------------- new job -- */

describe("Fleet — asking for a job", () => {
  async function refuseWith(refusal: ApiRefusal): Promise<void> {
    const state = fleetState({
      concurrency: { house: { limit: 4, held: 0 }, projects: [column({ slots: [] })] },
    });
    const answer = fleetFetch(state);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/jobs" && init?.method === "POST") throw refusal;
      return await answer(path, init);
    });

    await renderWithRouter(<Fleet />);
    const prompt = await screen.findByLabelText("What to work on in alpha");
    fireEvent.change(prompt, { target: { value: "tidy the imports" } });
    fireEvent.click(screen.getByRole("button", { name: "New job" }));
  }

  it("says the kill switch and a full project differently, though both are 409", async () => {
    // `POST /jobs` refuses in prose for both, so the status cannot tell them
    // apart and neither can the status-derived code. Collapsing them into one
    // sentence would tell somebody to wait for room that is already there, or
    // to release a switch that was never engaged.
    await refuseWith(
      new ApiRefusal(409, "conflict", "the kill switch is engaged; nothing autonomous starts"),
    );
    const said = await screen.findByText(/kill switch is engaged/);
    expect(said).toBeDefined();
    // Not the shared floor's sentence for a 409 ("something about this has
    // already changed"), which is what both refusals would collapse to if the
    // page mapped the status-derived code to copy of its own.
    expect(said.textContent).not.toMatch(/already changed/);
    expect(said.textContent).not.toMatch(/piece\(s\) of work/);
  });

  it("shows the daemon's own sentence when the project is full", async () => {
    await refuseWith(
      new ApiRefusal(409, "conflict", "this project already has 2 piece(s) of work in flight"),
    );
    const said = await screen.findByText(/this project already has 2 piece\(s\) of work in flight/);
    expect(said).toBeDefined();
    expect(said.textContent).not.toMatch(/kill switch/);
  });
});

/* ----------------------------------------------------- taking a slot back -- */

describe("Fleet — cancelling", () => {
  it("reports no error when a run's cancel answers the way the route really answers", async () => {
    // The regression: `POST /runs/{id}/cancel` returns a bare `StatusCode::OK`
    // — a 200 with an **empty body** — and `apiFetch` exempts only 204/205 from
    // JSON parsing, so every successful cancel from this page threw and the
    // card reported a failure for a run that really had stopped.
    const state = fleetState({
      concurrency: {
        house: { limit: 4, held: 1 },
        projects: [column({ limit: 2, slots: [slot({ slot: 1, owner_kind: "run", owner_id: 7 })] })],
      },
      runs: [run()],
    });
    const answer = fleetFetch(state);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      // One answer, described at both seams the way the real client describes
      // it — so this test is about what the page does with the daemon's reply
      // and not about which function the hook happens to call. Through the JSON
      // call an empty 200 is `res.json()` throwing, which is exactly the failure
      // the user saw.
      if (path === "/runs/7/cancel") {
        throw new Error("the daemon answered /runs/7/cancel with a body that is not JSON");
      }
      return await answer(path, init);
    });
    // Through the text call the same answer is the empty string. The state moves
    // with it, so the invalidation that follows reads back a slot really gone.
    daemon.apiText.mockImplementation(async (path: string) => {
      if (path === "/runs/7/cancel") {
        state.concurrency = { house: { limit: 4, held: 0 }, projects: [column({ limit: 2 })] };
        state.runs = [];
      }
      return "";
    });

    const { queryClient } = await renderWithRouter(<Fleet />);
    const card = await screen.findByRole("article", { name: "slot 1 — run 7" });

    fireEvent.click(within(card).getByRole("button", { name: "Cancel" }));
    // Real timers rather than fake ones: the page polls, and the wait is the
    // interlock's 300ms dwell, which swallows a click arriving as the tail of a
    // double-click. Clicking through it without waiting would confirm nothing.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 350));
    });
    fireEvent.click(within(card).getByRole("button", { name: "Cancel run 7?" }));

    await waitFor(() => {
      expect(daemon.apiText).toHaveBeenCalledWith("/runs/7/cancel", { method: "POST" });
    });
    // Settles as a success, and not as the parse error the JSON call produced.
    await waitFor(() => {
      const [cancel] = queryClient.getMutationCache().getAll();
      expect(cancel?.state.status).toBe("success");
    });
    // And says nothing about a failure — the note the page draws on `isError`.
    expect(screen.queryByText(/could not be cancelled/)).toBeNull();
    // The slot is gone, optimistically and then for real.
    expect(screen.queryByRole("article", { name: "slot 1 — run 7" })).toBeNull();
  });
});

/* --------------------------------------------------------------- canvas -- */

describe("the fleet canvas", () => {
  it("hands xyflow the same nodeTypes and edgeTypes on every render", () => {
    // The §9.2 risk, in one assertion. xyflow compares these by reference and
    // rebuilds every node in the graph when either changes; declared inside the
    // component they would be new objects on every 3-second poll. With the
    // React Compiler memoising around them the failure would be intermittent
    // rather than constant, which is the worse kind.
    const props = {
      nodes: [],
      edges: [],
      ends: {},
      onPropose: () => {},
      onLayoutChange: () => {},
    };
    const { rerender } = render(<FleetCanvas {...props} />);
    rerender(<FleetCanvas {...props} />);

    expect(seen.nodeTypes.length).toBeGreaterThanOrEqual(2);
    for (const captured of seen.nodeTypes) expect(captured).toBe(seen.nodeTypes[0]);
    for (const captured of seen.edgeTypes) expect(captured).toBe(seen.edgeTypes[0]);
  });
});

describe("the canvas model", () => {
  it("round-trips an arrangement through its own storage key", () => {
    saveLayout({ "job:41": { x: 120, y: 40 } });
    expect(localStorage.getItem("nucleos.fleet.layout.v2")).not.toBeNull();
    expect(loadLayout()).toEqual({ "job:41": { x: 120, y: 40 } });
  });

  it("degrades to no layout rather than throwing on anything it did not write", () => {
    // `localStorage` holds text somebody else wrote — an older version of this
    // app, an extension, a person with the console open. A `NaN` that got
    // through would draw nothing and report nothing.
    localStorage.setItem("nucleos.fleet.layout.v2", "{not json");
    expect(loadLayout()).toEqual({});

    localStorage.setItem(
      "nucleos.fleet.layout.v2",
      JSON.stringify({ "job:41": { x: "left", y: 4 }, "job:42": { x: 1, y: 2 } }),
    );
    expect(loadLayout()).toEqual({ "job:42": { x: 1, y: 2 } });
  });

  it("refuses a line between two projects, and one from a job to itself", () => {
    const alphaOne = { key: "job:1", projectId: "alpha", jobId: 1 };
    const alphaTwo = { key: "job:2", projectId: "alpha", jobId: 2 };
    const betaOne = { key: "job:9", projectId: "beta", jobId: 9 };
    const aRun = { key: "run:7", projectId: "alpha", jobId: null };

    expect(isValidConnection(alphaOne, alphaTwo)).toBe(true);
    // The daemon answers this pair with a 400 — "these jobs belong to different
    // projects, so they share no slots to serialise". Offering a gesture whose
    // only possible outcome is a refusal is worse than not offering it.
    expect(isValidConnection(alphaOne, betaOne)).toBe(false);
    expect(isValidConnection(alphaOne, alphaOne)).toBe(false);
    // An exclusion names two jobs; a run holds a slot without being one.
    expect(isValidConnection(alphaOne, aRun)).toBe(false);
    expect(isValidConnection(alphaOne, undefined)).toBe(false);
  });
});

/* ------------------------------------------------------------ job detail -- */

describe("Fleet — a job's items", () => {
  it("marks where a round begins and keeps the gate apart from the item", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 4, held: 1 },
            projects: [column({ slots: [slot()] })],
          },
          jobs: [job({ round: 1, max_rounds: 3 })],
          details: {
            41: {
              ...job({ round: 1 }),
              branch: "nucleos/job-41",
              items: [
                {
                  ordinal: 0,
                  description: "rename the module",
                  status: "passed",
                  round: 0,
                  run_id: 100,
                  gate_status: "passed",
                },
                {
                  ordinal: 1,
                  description: "update the callers",
                  status: "passed",
                  round: 1,
                  run_id: 101,
                  // Nothing measured this one. `passed` with a NULL gate is
                  // "no gate configured", not "the tests passed".
                  gate_status: null,
                },
              ],
            },
          },
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    fireEvent.click(await screen.findByRole("button", { name: "Show items" }));

    const items = await screen.findByRole("list", { name: "items of job 41" });
    expect(within(items).getByText("round 2")).toBeDefined();
    expect(within(items).getByText(/no gate configured/i)).toBeDefined();
    expect(within(items).getByText(/gate passed/i)).toBeDefined();
  });

  it("says which of two excluded jobs is the one that waits", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 4, held: 2 },
            projects: [
              column({
                limit: 2,
                slots: [slot({ slot: 0, owner_id: 41 }), slot({ slot: 1, owner_id: 55 })],
              }),
            ],
          },
          jobs: [job({ id: 41 }), job({ id: 55 })],
          exclusions: [
            {
              id: 3,
              project_id: "alpha",
              job_low: 41,
              job_high: 55,
              proposal_id: 9,
              paths: null,
              created_at: "2026-08-17T09:00:00Z",
            },
          ],
          requests: [
            proposal({ id: 12, status: "pending", tool_input: JSON.stringify({ job_low: 41, job_high: 55 }) }),
          ],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);

    // The daemon parks the HIGHER id, so the same edge reads differently from
    // its two ends — a card that said "waiting on the other" at both ends would
    // describe a deadlock the daemon cannot produce.
    const low = await screen.findByRole("article", { name: "slot 0 — job 41" });
    expect(within(low).getByText(/not at the same time as job 55 — that one waits/)).toBeDefined();
    const high = await screen.findByRole("article", { name: "slot 1 — job 55" });
    expect(within(high).getByText(/not at the same time as job 41 — this one waits/)).toBeDefined();

    // The rule in force wins over the request that asked for the same pair:
    // drawing both would be one constraint rendered twice.
    expect(screen.queryByText(/waiting for a decision/)).toBeNull();
  });
});
