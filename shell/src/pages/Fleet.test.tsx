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
 * The real xyflow, with a tap on the props the §9.2 spike is about.
 *
 * Wrapping rather than replacing: the canvas really mounts, so this also proves
 * the library renders under jsdom at all, and the recorded props let the test
 * assert the reference identity xyflow's own reconciliation depends on.
 */
const seen = vi.hoisted(() => ({
  nodeTypes: [] as unknown[],
  edgeTypes: [] as unknown[],
  props: [] as Record<string, unknown>[],
}));
vi.mock("@xyflow/react", async (original) => {
  const real = await original<typeof import("@xyflow/react")>();
  return {
    ...real,
    ReactFlow: (props: Record<string, unknown>) => {
      seen.nodeTypes.push(props.nodeTypes);
      seen.edgeTypes.push(props.edgeTypes);
      seen.props.push(props);
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
import { buildFleet, isValidConnection, loadLayout, saveLayout, slotDetail, zonesFor } from "../canvas/model";
import { ApiRefusal } from "../data/client";
import { cancellableOwner } from "../data/fleet";
import { keys } from "../data/keys";
import type {
  Concurrency,
  FleetExclusion,
  HeldSlot,
  Job,
  JobDetail,
  JobItem,
  ProjectConcurrency,
  RunSearchResult,
} from "../data/fleet";
import { daemonFetch, daemonState, project, proposal, renderWithRouter } from "../test/harness";
import type { ProjectSummary, Proposal } from "../data/system";
import type { TeamView } from "../data/teams";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  localStorage.clear();
  seen.props.length = 0;
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
    wave_id: null,
    lease_renewed_at: null,
    ...overrides,
  };
}

/**
 * One item of a job's queue.
 *
 * The four directed fields default to *nobody was asked*, which is what the
 * daemon really sends for every item of every job without a team. A fixture
 * that filled them in would make every test in this file read as a directed
 * job, and the two are supposed to look different.
 */
function item(overrides: Partial<JobItem> = {}): JobItem {
  return {
    ordinal: 0,
    description: "rename the module",
    status: "passed",
    round: 0,
    run_id: 100,
    gate_status: "passed",
    agent_id: null,
    agent_name: null,
    depends_on: [],
    files: [],
    ...overrides,
  };
}

/**
 * One team of the catalogue, as `/teams` sends it.
 *
 * Local rather than shared with `Teams.test.tsx` for the reason `fleetFetch`
 * gives about routes: the shared harness is the floor, and a fixture that only
 * two suites want does not belong in it.
 */
function teamView(overrides: Partial<TeamView> = {}): TeamView {
  return {
    id: "atendimento",
    name: "Atendimento",
    mission: "answer the customers who write in",
    director_agent_id: "ana",
    max_rounds: 3,
    max_parallel: 2,
    budget_usd: 10,
    max_open_actions: 5,
    max_live_runs: 1,
    created_at: "2026-08-18T09:00:00Z",
    updated_at: "2026-08-18T09:00:00Z",
    members: [],
    grants: [],
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
    // Nearly every job. A team is what makes the other three fields non-null,
    // and the default fixture is the queue in one checkout that the product
    // has always run.
    team_id: null,
    team_name: null,
    team_max_parallel: null,
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

function exclusion(overrides: Partial<FleetExclusion> = {}): FleetExclusion {
  return {
    id: 3,
    project_id: "alpha",
    job_low: 41,
    job_high: 55,
    proposal_id: 9,
    paths: null,
    created_at: "2026-08-17T09:00:00Z",
    ...overrides,
  };
}

/**
 * A project the núcleo will start a job in: `POST /jobs` refuses one whose autopilot is not
 * `active` or which has no root (`core/src/job.rs`, `resolve_start`), and the harness's own
 * default project is `off` with no root — the right default for every other page, and a project
 * this page must offer only as disabled.
 */
function startable(project_id: string, overrides: Partial<ProjectSummary> = {}): ProjectSummary {
  return project({ project_id, mode: "active", project_root: `C:/repos/${project_id}`, ...overrides });
}

interface FleetState {
  concurrency: Concurrency | undefined;
  jobs: Job[] | undefined;
  runs: RunSearchResult[] | undefined;
  exclusions: FleetExclusion[];
  requests: Proposal[];
  details: Record<number, JobDetail>;
  /** The catalogue the new-job form offers. Empty in a house with none. */
  teams: TeamView[];
  /** What `/projects` answers: the autopilot mode, root and open proposals per project. */
  projects: ProjectSummary[];
  kill: boolean;
}

function fleetState(overrides: Partial<FleetState> = {}): FleetState {
  return {
    concurrency: { house: { limit: 4, held: 0 }, projects: [] },
    jobs: [],
    runs: [],
    exclusions: [],
    requests: [],
    details: {},
    // Empty by default, which is what most of this suite is about: with no
    // teams the picker is not drawn at all.
    teams: [],
    projects: [startable("alpha")],
    kill: false,
    ...overrides,
  };
}

/**
 * The fleet's routes, over the foundation's responder.
 *
 * A local switch rather than an edit to `test/harness.tsx`: the harness is the
 * shared floor, and a page that teaches it seven routes of its own makes every
 * other suite carry them. Anything this does not know falls through to the
 * shared responder, so `/projects`, `/autopilot/kill` and `/autopilot/budget` still answer.
 */
function fleetFetch(state: FleetState): (path: string, init?: RequestInit) => Promise<unknown> {
  const shared = daemonFetch(daemonState({ projects: state.projects, kill: { engaged: state.kill } }));
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
      case "/teams":
        return state.teams;
      default: {
        const detail = /^\/jobs\/(\d+)$/.exec(path);
        if (detail !== null) return state.details[Number(detail[1])];
        return await shared(path, init);
      }
    }
  };
}

/**
 * The page header — by its class, because every card and column head on this page is a
 * `<header>` too, and jsdom maps each of them to `banner`.
 */
async function pageHeader(): Promise<HTMLElement> {
  return await waitFor(() => {
    const header = document.querySelector<HTMLElement>(".ui-page-header");
    expect(header).not.toBeNull();
    return header!;
  });
}

/** The status region inside the page header — the sentence the page leads with. */
async function headlineText(): Promise<string> {
  return within(await pageHeader()).getByRole("status").textContent ?? "";
}

/** The interlock's 300ms dwell, which swallows a click arriving as the tail of a double-click. */
async function pastTheDwell(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 350));
  });
}

/* ------------------------------------------------------------ the columns -- */

describe("Fleet — columns", () => {
  it("shows the capacity, and gives an idle project a line in the rack rather than a column", async () => {
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
    expect(within(held).getByRole("link", { name: "Open in Runs" }).getAttribute("href")).toBe("/runs/7");

    // An idle project is a line in the rack, not a column: four empty columns were what pushed a
    // column with a problem in it off the screen.
    expect(screen.queryByRole("region", { name: "beta column" })).toBeNull();
    const rack = screen.getByRole("region", { name: "Slots by project" });
    const beta = within(rack).getByRole("link", { name: "beta" });
    expect(beta.getAttribute("href")).toBe("/projects/beta/state");
    expect(beta.closest("li")!.textContent).toContain("0 of 3 slots held, room for 3");
  });

  it("draws a wave's slot as a worker of its wave, with when its lease was renewed and no cancel", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 5, held: 1 },
            projects: [
              column({
                project_id: "alpha",
                limit: 2,
                slots: [
                  slot({ slot: 1, owner_kind: "wave", owner_id: 7, wave_id: 3, lease_renewed_at: "2026-09-29T10:00:00Z" }),
                ],
              }),
            ],
          },
          jobs: [job()],
          runs: [run()],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);

    const alpha = await screen.findByRole("region", { name: "alpha column" });
    const held = within(alpha).getByRole("article", { name: "slot 1 — worker of wave 3" });
    expect(within(held).getByText(/lease renewed/)).toBeDefined();
    expect(within(held).queryByRole("button", { name: /cancel/i })).toBeNull();
  });

  it("orders the columns by what is wrong in them before how busy they are", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 8, held: 5 },
            projects: [
              column({
                project_id: "busy",
                limit: 3,
                slots: [
                  slot({ project_id: "busy", slot: 0, owner_id: 1 }),
                  slot({ project_id: "busy", slot: 1, owner_id: 2 }),
                  slot({ project_id: "busy", slot: 2, owner_id: 3 }),
                ],
              }),
              column({
                project_id: "calm",
                limit: 2,
                slots: [slot({ project_id: "calm", slot: 0, owner_id: 4 })],
              }),
              // One slot, and the complete jobs listing does not have its owner: a leaked slot.
              column({
                project_id: "quiet",
                limit: 2,
                slots: [slot({ project_id: "quiet", slot: 0, owner_id: 99 })],
              }),
            ],
          },
          jobs: [
            job({ id: 1, project_id: "busy" }),
            job({ id: 2, project_id: "busy" }),
            job({ id: 3, project_id: "busy" }),
            job({ id: 4, project_id: "calm" }),
          ],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    await screen.findByRole("region", { name: "quiet column" });

    // The quietest project comes first, because it is the one with a problem.
    const order = screen
      .getAllByRole("region", { name: / column$/ })
      .map((region) => region.getAttribute("aria-label"));
    expect(order).toEqual(["quiet column", "busy column", "calm column"]);
  });

  it("puts the project's autopilot mode in its column head, and nothing when it cannot read it", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 4, held: 2 },
            projects: [
              column({ slots: [slot()] }),
              column({ project_id: "beta", slots: [slot({ project_id: "beta", owner_kind: "run", owner_id: 7 })] }),
            ],
          },
          jobs: [job()],
          runs: [run({ project_id: "beta" })],
          // `/projects` knows alpha and not beta.
          projects: [startable("alpha", { mode: "shadow" })],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);

    const alpha = await screen.findByRole("region", { name: "alpha column" });
    const head = within(alpha).getByRole("heading", { name: "alpha" }).parentElement!;
    await waitFor(() => expect(within(head).getByText("shadow").className).toContain("ui-badge-shadow"));
    expect(head.textContent).toContain("autopilot");

    // A guessed mode on a safety control is worse than none.
    const beta = screen.getByRole("region", { name: "beta column" });
    const betaHead = within(beta).getByRole("heading", { name: "beta" }).parentElement!;
    expect(betaHead.querySelector(".ui-badge")).toBeNull();
  });

  it("says a project-wide fact once in the column head, not on every card", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 4, held: 2 },
            projects: [
              column({
                slots: [slot({ slot: 0, owner_id: 41 }), slot({ slot: 1, owner_id: 55 })],
                collision: {
                  declared: { state: "not_measured", overlaps: [] },
                  observed: { state: "clean", overlaps: [] },
                },
              }),
            ],
          },
          jobs: [job({ id: 41 }), job({ id: 55 })],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);

    const alpha = await screen.findByRole("region", { name: "alpha column" });
    expect(within(alpha).getAllByText("overlap not measured")).toHaveLength(1);
    const source = within(alpha).getByText("predicted");
    // The source is a badge, so it wears its tone's whole triple rather than a bare colour.
    expect(source.className).toContain("ui-badge-paused");
    for (const card of within(alpha).getAllByRole("article")) {
      expect(within(card).queryByText("overlap not measured")).toBeNull();
    }
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

  it("names a wave's slot by its wave, never by the run that shares its number, and offers no cancel", () => {
    // `owner_id` is `wave_workers.id`. Among the runs it would take run 7's description; the
    // wave it belongs to is what the readout joins in for it.
    const wave = slot({
      slot: 1, owner_kind: "wave", owner_id: 7, wave_id: 3, lease_renewed_at: "2026-09-29T10:00:00Z",
    });

    expect(slotDetail(wave, [job()], [run({ id: 7 })])).toEqual({
      kind: "wave", waveId: 3, renewedAt: "2026-09-29T10:00:00Z",
    });
    expect(cancellableOwner(wave)).toBeNull();
  });

  it("says it cannot describe a wave's slot whose worker the daemon no longer has", () => {
    const wave = slot({ slot: 1, owner_kind: "wave", owner_id: 7 });

    expect(slotDetail(wave, [job()], [run({ id: 7 })]).kind).toBe("unknown");
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
    expect(within(mine).getByText("observed").className).toContain("ui-badge-danger");
    // No `/items/<id>/cancel` exists, and the number would aim the one route
    // there is at somebody else's run. The gesture that stops this work is the
    // job's, on the job's card.
    expect(within(mine).queryByRole("button", { name: "Cancel" })).toBeNull();
  });

  it("an item's card leads with which item of which job it is, and links to where the conflict is resolved", async () => {
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

    // Counting from one on screen and from zero in the row, like every other
    // place this repository prints an ordinal to a person.
    const card = await screen.findByRole("article", { name: "slot 1 — item 3 of job 41" });
    expect(within(card).getAllByText(/nightly-backlog/).length).toBeGreaterThan(0);
    // The id is still there, second and as a handle.
    expect(within(card).getByText("id 7")).toBeDefined();
    // Through the map, and in the feed's tone for the same event: `core/src/job.rs` puts a
    // conflicted item down rather than failing it, and the queue resolves it itself (`batch_of`),
    // so Held Ember — neither a fault nor a summons.
    const state = within(card).getByText("did not merge");
    expect(state.className).toContain("ui-badge-paused");
    expect(within(card).queryByText(/to do/), "an item mid-conflict is not work nobody has started").toBeNull();
    expect(
      within(card).queryByText(/detail unavailable/i),
      "the card can describe itself now, so it must stop saying it cannot",
    ).toBeNull();
    // A real door, to the runs of this project, where the resolution run is listed.
    const door = within(card).getByRole("link", { name: "Follow it in Runs" });
    expect(door.getAttribute("href")).toBe("/runs?project=alpha");
  });

  it("dims the cards, and takes away every control that would act on them, when the view is stale", async () => {
    const state = fleetState({
      concurrency: {
        house: { limit: 4, held: 2 },
        projects: [column({ slots: [slot({ slot: 0, owner_id: 41 }), slot({ slot: 1, owner_id: 55 })] })],
      },
      jobs: [job({ id: 41 }), job({ id: 55 })],
      exclusions: [exclusion()],
    });
    let answering = true;
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (!answering && path === "/concurrency") throw new ApiRefusal(503, "unavailable", "");
      return await fleetFetch(state)(path, init);
    });

    const { queryClient } = await renderWithRouter(<Fleet />);
    const card = await screen.findByRole("article", { name: "slot 0 — job 41" });
    // The controls are here while the reading is the daemon's.
    expect(screen.getByRole("button", { name: "New job" })).toBeDefined();
    expect(within(card).getByRole("button", { name: "Cancel" })).toBeDefined();
    expect(within(card).getByRole("button", { name: "Lift" })).toBeDefined();

    answering = false;
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: keys.concurrency });
    });

    // `findBy` and not `getBy`: react-query hands observer notifications to a
    // `setTimeout(…, 0)`, so the refetch settling inside `act` is not the same
    // moment as the render that follows it.
    expect(await screen.findByText(/view is stale — last good read/)).toBeDefined();
    expect(await headlineText()).toContain("the view is stale");
    // Blanking would read as "there is room", which is the one wrong thing this
    // page can say. The card stays, receded, and the note says how old it is.
    const after = screen.getByRole("article", { name: "slot 0 — job 41" });
    expect(after.className).toContain("fleet-card-stale");
    // And every action that would act on capacity nobody can vouch for is gone
    // rather than disabled — it fails before the click instead of after it.
    expect(screen.queryByRole("button", { name: "New job" })).toBeNull();
    expect(within(after).queryByRole("button", { name: "Cancel" })).toBeNull();
    expect(within(after).queryByRole("button", { name: "Lift" })).toBeNull();
  });

  it("lifts a rule in force only through the interlock", async () => {
    const state = fleetState({
      concurrency: {
        house: { limit: 4, held: 2 },
        projects: [column({ slots: [slot({ slot: 0, owner_id: 41 }), slot({ slot: 1, owner_id: 55 })] })],
      },
      jobs: [job({ id: 41 }), job({ id: 55 })],
      exclusions: [exclusion()],
    });
    daemon.apiFetch.mockImplementation(fleetFetch(state));

    await renderWithRouter(<Fleet />);
    const card = await screen.findByRole("article", { name: "slot 0 — job 41" });

    fireEvent.click(within(card).getByRole("button", { name: "Lift" }));
    // Armed, and nothing has happened yet: lifting takes effect at once, so one click is not it.
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/fleet/exclusions/3", expect.anything());
    await pastTheDwell();
    fireEvent.click(within(card).getByRole("button", { name: "Lift the rule" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/fleet/exclusions/3", { method: "DELETE" });
    });
  });

  it("asks the pairing question from the keyboard, with the same request the canvas sends", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 4, held: 2 },
            projects: [column({ slots: [slot({ slot: 0, owner_id: 41 }), slot({ slot: 1, owner_id: 55 })] })],
          },
          jobs: [job({ id: 41 }), job({ id: 55 })],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    const card = await screen.findByRole("article", { name: "slot 0 — job 41" });

    const partner = within(card).getByLabelText("Never at the same time as") as HTMLSelectElement;
    expect([...partner.options].map((option) => option.textContent)).toEqual(["job 55"]);
    fireEvent.click(within(card).getByRole("button", { name: "Ask" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/fleet/exclusions", {
        method: "POST",
        body: JSON.stringify({ job_a: 41, job_b: 55, paths: [] }),
      });
    });
  });

  it("draws the window spend against its ceiling", async () => {
    daemon.apiFetch.mockImplementation(fleetFetch(fleetState()));

    await renderWithRouter(<Fleet />);

    const spend = await screen.findByRole("article", { name: "Window spend" });
    await waitFor(() => expect(within(spend).getByText("$1.42")).toBeDefined());
    expect(within(spend).getByRole("img", { name: "Window spend: $1.42 of $5.00" })).toBeDefined();
  });

  it("teaches what the page is for when no project is registered, and offers neither control", async () => {
    daemon.apiFetch.mockImplementation(fleetFetch(fleetState()));

    await renderWithRouter(<Fleet />);

    expect(await screen.findByRole("heading", { name: "No projects yet" })).toBeDefined();
    expect(screen.getByRole("link", { name: "Projects" }).getAttribute("href")).toBe("/projects");
    expect(screen.queryByRole("group", { name: "How to look at the fleet" })).toBeNull();
    expect(screen.queryByRole("button", { name: "New job" })).toBeNull();
  });

  it("files a failed reading like a bug report, and offers no way of looking at nothing", async () => {
    const state = fleetState();
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/concurrency") throw new Error("Failed to fetch");
      return await fleetFetch(state)(path, init);
    });

    await renderWithRouter(<Fleet />);

    const note = await screen.findByRole("alert");
    expect(note.textContent).toContain("GET /concurrency");
    expect(note.textContent).toContain("Failed to fetch");
    expect(note.querySelector(".ui-reading-time")).not.toBeNull();
    expect(screen.queryByRole("group", { name: "How to look at the fleet" })).toBeNull();
    expect(screen.queryByRole("button", { name: "New job" })).toBeNull();
  });
});

/* -------------------------------------------------------------- the rack -- */

describe("Fleet — the slot rack", () => {
  async function findRack(): Promise<HTMLElement> {
    return await screen.findByRole("region", { name: "Slots by project" });
  }

  /** A project's line in the rack, found by its name's link. */
  function entryOf(rack: HTMLElement, name: string): HTMLElement {
    return within(rack).getByRole("link", { name }).closest("li")!;
  }

  /** Each pip's tone in order, `free` for a hollow one. */
  function pipsOf(entry: HTMLElement): string[] {
    return [...entry.querySelectorAll(".ui-pip")].map((pip) =>
      [...pip.classList].find((name) => name.startsWith("ui-pip-"))!.slice("ui-pip-".length),
    );
  }

  /**
   * Three busy projects the columns put in exceptions-first order, and one with nothing held.
   * `quiet`'s one slot belongs to a job the complete listing does not have: a leaked slot.
   */
  function mixedFleet(): FleetState {
    return fleetState({
      concurrency: {
        house: { limit: 9, held: 5 },
        projects: [
          column({
            project_id: "busy",
            limit: 3,
            slots: [
              slot({ project_id: "busy", slot: 0, owner_id: 1 }),
              slot({ project_id: "busy", slot: 1, owner_id: 2 }),
              slot({ project_id: "busy", slot: 2, owner_id: 3 }),
            ],
          }),
          column({ project_id: "calm", limit: 2, slots: [slot({ project_id: "calm", slot: 0, owner_id: 4 })] }),
          column({ project_id: "delta", limit: 2 }),
          column({ project_id: "quiet", limit: 2, slots: [slot({ project_id: "quiet", slot: 0, owner_id: 99 })] }),
        ],
      },
      jobs: [
        job({ id: 1, project_id: "busy" }),
        job({ id: 2, project_id: "busy" }),
        job({ id: 3, project_id: "busy" }),
        job({ id: 4, project_id: "calm" }),
      ],
      projects: [startable("busy"), startable("calm"), startable("delta"), startable("quiet")],
    });
  }

  it("lists every project, busy or idle, in one order that does not follow the columns", async () => {
    daemon.apiFetch.mockImplementation(fleetFetch(mixedFleet()));

    await renderWithRouter(<Fleet />);
    const rack = await findRack();
    await screen.findByRole("region", { name: "quiet column" });

    // The columns rank: the leak first, then the busiest.
    expect(
      screen.getAllByRole("region", { name: / column$/ }).map((region) => region.getAttribute("aria-label")),
    ).toEqual(["quiet column", "busy column", "calm column"]);
    // The rack does not. Every project, by id — the order `/concurrency` sends them in — so a
    // project is always where it was, whatever just went wrong in it.
    expect(within(rack).getAllByRole("link").map((link) => link.textContent)).toEqual([
      "busy",
      "calm",
      "delta",
      "quiet",
    ]);
  });

  it("lights a pip per held slot in the tone its card wears, and leaves a free one hollow", async () => {
    daemon.apiFetch.mockImplementation(fleetFetch(mixedFleet()));

    await renderWithRouter(<Fleet />);
    const rack = await findRack();
    await screen.findByRole("region", { name: "quiet column" });

    // One pip per slot of the limit: lit for what is held, hollow for what is free.
    expect(pipsOf(entryOf(rack, "busy"))).toEqual(["active", "active", "active"]);
    expect(pipsOf(entryOf(rack, "calm"))).toEqual(["active", "free"]);
    expect(pipsOf(entryOf(rack, "delta"))).toEqual(["free", "free"]);
    // A leaked slot is red in the rack because it is red on its card: one reading, two places.
    expect(pipsOf(entryOf(rack, "quiet"))).toEqual(["danger", "free"]);
    const leaked = screen.getByRole("article", { name: "slot 0 — job 99" });
    expect(within(leaked).getByText("awaiting reconciliation").className).toContain("ui-badge-danger");
  });

  it("says in words what the pips draw, and leaves the link named by the project alone", async () => {
    daemon.apiFetch.mockImplementation(fleetFetch(mixedFleet()));

    await renderWithRouter(<Fleet />);
    const rack = await findRack();
    const quiet = entryOf(rack, "quiet");

    // The drawing is hidden from assistive tech, and a sentence stands in for it, naming each held
    // slot by its badge's words.
    expect(quiet.querySelector(".ui-pips-row")?.getAttribute("aria-hidden")).toBe("true");
    await waitFor(() => expect(quiet.textContent).toContain("autopilot active"));
    expect(quiet.textContent).toContain("1 of 2 slots held (awaiting reconciliation), room for 1");
    expect(entryOf(rack, "delta").textContent).toContain("0 of 2 slots held, room for 2");
    expect(entryOf(rack, "busy").textContent).toContain(
      "3 of 3 slots held (implementing, implementing, implementing)",
    );
    expect(entryOf(rack, "busy").textContent).not.toContain("room for");
    // The link's own name is the project, and nothing else.
    expect(within(quiet).getByRole("link").textContent).toBe("quiet");
  });

  it("past eight slots draws only the held pips, with the count beside them", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 20, held: 3 },
            projects: [
              column({
                limit: 12,
                slots: [slot({ slot: 0, owner_id: 41 }), slot({ slot: 1, owner_id: 42 }), slot({ slot: 2, owner_id: 43 })],
              }),
            ],
          },
          jobs: [job({ id: 41 }), job({ id: 42 }), job({ id: 43 })],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    const alpha = entryOf(await findRack(), "alpha");

    expect(pipsOf(alpha)).toEqual(["active", "active", "active"]);
    expect(within(alpha).getByText("3/12").className).toBe("ui-pips-count");
    expect(alpha.textContent).toContain("3 of 12 slots held (implementing, implementing, implementing), room for 9");
  });

  it("offers New job wherever there is room, busy or idle, and opens the header's panel aimed at it", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 7, held: 3 },
            projects: [
              column({ project_id: "alpha", limit: 2, slots: [slot({ owner_id: 41 })] }),
              column({ project_id: "bravo", limit: 3 }),
              column({
                project_id: "charlie",
                limit: 2,
                slots: [
                  slot({ project_id: "charlie", slot: 0, owner_id: 61 }),
                  slot({ project_id: "charlie", slot: 1, owner_id: 62 }),
                ],
              }),
            ],
          },
          jobs: [job({ id: 41 }), job({ id: 61, project_id: "charlie" }), job({ id: 62, project_id: "charlie" })],
          projects: [startable("alpha"), startable("bravo"), startable("charlie")],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    const rack = await findRack();
    // A busy project with room is room: alpha holds one of two.
    const alpha = await within(rack).findByRole("button", { name: "New job in alpha" });
    expect(within(rack).getByRole("button", { name: "New job in bravo" })).toBeDefined();
    // A full one says nothing at all, neither a button nor a word: its pips already say it.
    const charlie = entryOf(rack, "charlie");
    expect(within(charlie).queryByRole("button")).toBeNull();
    expect(charlie.querySelector(".fleet-rack-why")).toBeNull();

    fireEvent.click(alpha);
    // The one panel, not a second form: the header's button says it is open.
    expect(document.querySelectorAll("form.fleet-new-job")).toHaveLength(1);
    expect(screen.getByRole("button", { name: "New job" }).getAttribute("aria-expanded")).toBe("true");
    // The entry's project, and not the roomiest, which is bravo.
    const select = screen.getByLabelText("Project for the new job") as HTMLSelectElement;
    expect(select.value).toBe("alpha");
    const prompt = screen.getByLabelText("Prompt");
    await waitFor(() => expect(document.activeElement).toBe(prompt));

    fireEvent.keyDown(prompt, { key: "Escape" });
    expect(document.activeElement).toBe(alpha);

    // Another entry while the panel is open: the pick moves, and focus goes in again.
    fireEvent.click(within(rack).getByRole("button", { name: "New job in bravo" }));
    expect(select.value).toBe("bravo");
    fireEvent.click(alpha);
    expect(select.value).toBe("alpha");
    await waitFor(() => expect(document.activeElement).toBe(prompt));
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(document.activeElement).toBe(alpha);
  });

  it("says in a few words why the núcleo would refuse a job there, and the select says it in full", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 8, held: 0 },
            projects: [
              column({ project_id: "alpha" }),
              column({ project_id: "charlie" }),
              column({ project_id: "delta" }),
              column({ project_id: "echo" }),
            ],
          },
          projects: [
            startable("alpha"),
            startable("charlie", { mode: "shadow" }),
            // Active with no root is a real state: `resolve_start` refuses it with `NoRoot`.
            startable("delta", { project_root: null }),
            project({ project_id: "echo", mode: "off" }),
          ],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    const rack = await findRack();

    // Shadow and off are said by the mode's badge on the same line, so the entry adds no word of
    // its own — only the absent button. A missing folder has no badge to say it, so it is written.
    const charlie = entryOf(rack, "charlie");
    await waitFor(() => expect(within(charlie).getByText("shadow")).toBeDefined());
    expect(within(charlie).queryByText("in shadow")).toBeNull();
    expect(within(charlie).queryByRole("button")).toBeNull();
    expect(within(entryOf(rack, "delta")).getByText("no folder")).toBeDefined();
    expect(within(entryOf(rack, "delta")).queryByRole("button")).toBeNull();
    // Off and holding nothing is not part of the fleet: not on the rack at all.
    expect(within(rack).queryByRole("link", { name: "echo" })).toBeNull();
    expect(within(entryOf(rack, "alpha")).getByRole("button", { name: "New job in alpha" })).toBeDefined();

    // One answer at two lengths: each option says in full what its entry says short, or leaves to
    // the badge beside it.
    fireEvent.click(screen.getByRole("button", { name: "New job" }));
    const select = screen.getByLabelText("Project for the new job") as HTMLSelectElement;
    const option = (value: string) => select.querySelector(`option[value="${value}"]`)?.textContent;
    expect(option("charlie")).toBe("charlie — in shadow — jobs start only when the autopilot is active");
    expect(option("delta")).toBe("delta — no folder recorded");
    expect(option("echo")).toBeUndefined();
  });

  it("keeps a switched-off project on the rack only while it still holds a slot", async () => {
    // Off refuses new work, but work started before the switch still holds what it claimed, and a
    // rack without it would be capacity vanishing in silence.
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 4, held: 1 },
            projects: [
              column({ project_id: "alpha", slots: [slot({ owner_id: 41 })] }),
              column({ project_id: "echo" }),
            ],
          },
          jobs: [job({ id: 41 })],
          projects: [
            project({ project_id: "alpha", mode: "off" }),
            project({ project_id: "echo", mode: "off" }),
          ],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    const rack = await findRack();

    await waitFor(() => expect(within(rack).queryByRole("link", { name: "echo" })).toBeNull());
    expect(within(rack).getByRole("link", { name: "alpha" })).toBeDefined();
    expect(pipsOf(entryOf(rack, "alpha"))).toEqual(["active", "free"]);
    expect(within(entryOf(rack, "alpha")).queryByRole("button")).toBeNull();
  });

  it("takes every New job out of the rack while the view is stale, and keeps the reasons and the pips", async () => {
    const state = fleetState({
      concurrency: {
        house: { limit: 4, held: 1 },
        projects: [column({ project_id: "alpha", slots: [slot({ owner_id: 41 })] }), column({ project_id: "charlie" })],
      },
      jobs: [job({ id: 41 })],
      projects: [startable("alpha"), startable("charlie", { mode: "shadow" })],
    });
    let answering = true;
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (!answering && path === "/concurrency") throw new ApiRefusal(503, "unavailable", "");
      return await fleetFetch(state)(path, init);
    });

    const { queryClient } = await renderWithRouter(<Fleet />);
    expect(await screen.findByRole("button", { name: "New job in alpha" })).toBeDefined();
    await waitFor(() => expect(screen.getByText("shadow")).toBeDefined());

    answering = false;
    await act(async () => {
      await queryClient.refetchQueries({ queryKey: keys.concurrency });
    });

    expect(await screen.findByText(/view is stale — last good read/)).toBeDefined();
    // Removed, not disabled: the room it would ask for is room nobody can vouch for.
    expect(screen.queryByRole("button", { name: "New job in alpha" })).toBeNull();
    const rack = screen.getByRole("region", { name: "Slots by project" });
    expect(within(rack).getByRole("list").className).toContain("fleet-rack-stale");
    // Still named, still reachable, still showing the mode that says why the other one cannot take
    // a job — and the pips stay, because a rack gone blank would read as every slot given back.
    expect(within(rack).getByRole("link", { name: "alpha" })).toBeDefined();
    expect(within(entryOf(rack, "charlie")).getByText("shadow")).toBeDefined();
    expect(pipsOf(entryOf(rack, "alpha"))).toEqual(["active", "free"]);
  });

  it("keeps the rack's New job while the kill switch is engaged, as the header keeps its own", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(fleetState({ kill: true, concurrency: { house: { limit: 4, held: 0 }, projects: [column()] } })),
    );

    await renderWithRouter(<Fleet />);
    await waitFor(async () => expect(await headlineText()).toContain("the kill switch is engaged"));

    // The panel is where the refusal is said, beside the button it disables.
    fireEvent.click(screen.getByRole("button", { name: "New job in alpha" }));
    expect(screen.getByText(/the núcleo refuses every new job until it is released/)).toBeDefined();
  });

  it("is the page's body when nothing is in flight anywhere, with no columns and no Idle list", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 6, held: 0 },
            projects: [
              column({ project_id: "charlie", limit: 1 }),
              column({ project_id: "alpha", limit: 3 }),
              column({ project_id: "bravo", limit: 2 }),
            ],
          },
          projects: [startable("alpha"), startable("bravo"), startable("charlie")],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    const rack = await findRack();

    expect(document.querySelector(".fleet-columns")).toBeNull();
    expect(screen.queryAllByRole("region", { name: / column$/ })).toHaveLength(0);
    expect(within(rack).getAllByRole("link").map((link) => link.textContent)).toEqual(["alpha", "bravo", "charlie"]);
    expect(pipsOf(entryOf(rack, "alpha"))).toEqual(["free", "free", "free"]);
    expect(await headlineText()).toBe("nothing in flight; room for 6");
    // The list the rack replaced is not drawn beside it, under any name.
    expect(screen.queryByRole("region", { name: /^Idle/ })).toBeNull();
    expect(screen.queryByRole("heading", { name: /^Idle/ })).toBeNull();
  });

  it("stands above the canvas too, with the same lamps", async () => {
    daemon.apiFetch.mockImplementation(fleetFetch(mixedFleet()));

    await renderWithRouter(<Fleet />);
    await screen.findByRole("region", { name: "quiet column" });
    fireEvent.click(screen.getByRole("button", { name: "Canvas" }));

    await waitFor(() => expect(document.querySelector(".fleet-canvas")).not.toBeNull());
    expect(screen.queryByRole("region", { name: "quiet column" })).toBeNull();
    const rack = screen.getByRole("region", { name: "Slots by project" });
    expect(pipsOf(entryOf(rack, "quiet"))).toEqual(["danger", "free"]);
    expect(within(rack).getByRole("button", { name: "New job in delta" })).toBeDefined();
  });
});

/* --------------------------------------------------------------- headline -- */

describe("Fleet — the headline", () => {
  it("says the worst fact first, in the ladder's order, and the capacity last", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          kill: true,
          concurrency: {
            house: { limit: 8, held: 5 },
            projects: [
              column({
                limit: 3,
                slots: [
                  slot({ slot: 0, owner_id: 41 }),
                  slot({ slot: 1, owner_kind: "item", owner_id: 7, job_id: 41, ordinal: 0, item_status: "conflicted" }),
                  // Not in the complete jobs listing: leaked.
                  slot({ slot: 2, owner_id: 60 }),
                ],
              }),
              column({
                project_id: "bravo",
                limit: 2,
                slots: [
                  slot({ project_id: "bravo", slot: 0, owner_id: 55 }),
                  slot({ project_id: "bravo", slot: 1, owner_id: 56 }),
                ],
                collision: {
                  declared: { state: "clean", overlaps: [] },
                  observed: {
                    state: "collide",
                    overlaps: [{ a: { kind: "job", id: 55 }, b: { kind: "job", id: 56 }, paths: ["a.rs"] }],
                  },
                },
              }),
            ],
          },
          jobs: [
            job({ id: 41, status: "awaiting_approval" }),
            job({ id: 55, project_id: "bravo", status: "waiting", wait_reason: "excluded" }),
            job({ id: 56, project_id: "bravo" }),
          ],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    await screen.findByRole("region", { name: "alpha column" });

    await waitFor(async () =>
      expect(await headlineText()).toBe(
        "1 slot held by nothing, awaiting reconciliation; 1 observed overlap between trees; " +
          "1 item did not merge; 1 job awaiting approval; 1 job held by an exclusion; " +
          "the kill switch is engaged — no new job starts; " +
          "5 in flight of 8 across all projects; room for 3 more",
      ),
    );
    // The one clause somebody can act on is a door to where it is acted on.
    const header = await pageHeader();
    // The faults wear the wrong-fact colour and the item that did not merge does not: it was put
    // down, not failed (`core/src/job.rs`, `ItemState::Conflicted`).
    expect([...header.querySelectorAll(".ui-wrong")].map((node) => node.textContent)).toEqual([
      "1 slot held by nothing, awaiting reconciliation",
      "1 observed overlap between trees",
    ]);
    expect(within(header).getByRole("link", { name: "1 job awaiting approval" }).getAttribute("href")).toBe("/waiting");
    expect((await headlineText()).toLowerCase()).not.toContain("waiting on you");
  });

  it("says the capacity alone when nothing is wrong", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: { house: { limit: 4, held: 1 }, projects: [column({ slots: [slot()] })] },
          jobs: [job()],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    await screen.findByRole("region", { name: "alpha column" });

    await waitFor(async () =>
      expect(await headlineText()).toBe("1 in flight of 4 across all projects; room for 3 more"),
    );
  });

  /**
   * The headline names what the five are.
   *
   * The Waiting queue's own phrase would be this page borrowing it for a number that is
   * not that queue: these are open proposals across the roster, and the bare phrase belongs
   * to `/waiting` alone. Two projects and not one, so the sentence is proving the sum rather
   * than echoing a single row.
   */
  it("names what the open proposals are", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: { house: { limit: 5, held: 2 }, projects: [] },
          projects: [
            // The queue total is `open_review_items` since master c0d03b3 — proposals are
            // one half of it and shadow decisions the other. The headline sums the total.
            project({ project_id: "alpha", open_review_items: 3 }),
            project({ project_id: "beta", open_review_items: 2 }),
          ],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);

    const said = await screen.findByText(/items to review across the roster/);
    expect(said.textContent).toBe(
      "2 in flight of 5 across all projects; room for 3 more; 5 items to review across the roster",
    );

    // The card is the same fact in the same words, so the two cannot drift apart.
    const card = screen.getByRole("article", { name: "To review" });
    expect(within(card).getByText("5")).toBeDefined();
    expect(within(card).getByText("across the roster")).toBeDefined();
  });
});

/* -------------------------------------------------------------- new job -- */

describe("Fleet — asking for a job", () => {
  async function openComposer(): Promise<HTMLElement> {
    const opener = await screen.findByRole("button", { name: "New job" });
    fireEvent.click(opener);
    return opener;
  }

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
    await openComposer();
    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "tidy the imports" } });
    fireEvent.click(screen.getByRole("button", { name: "Start job" }));
  }

  it("opens one panel from the header, takes focus into it, and gives focus back on Escape and Close", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(fleetState({ concurrency: { house: { limit: 4, held: 0 }, projects: [column()] } })),
    );

    await renderWithRouter(<Fleet />);
    const opener = await openComposer();
    expect(opener.getAttribute("aria-expanded")).toBe("true");
    // One form for the page, not one per column.
    expect(document.querySelectorAll("form.fleet-new-job")).toHaveLength(1);

    const prompt = screen.getByLabelText("Prompt");
    await waitFor(() => expect(document.activeElement).toBe(prompt));

    fireEvent.keyDown(prompt, { key: "Escape" });
    expect(opener.getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(opener);

    fireEvent.click(opener);
    await waitFor(() => expect(document.activeElement).toBe(prompt));
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(document.activeElement).toBe(opener);
  });

  it("waits for a prompt, and says why the button is not yet pressable", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(fleetState({ concurrency: { house: { limit: 4, held: 0 }, projects: [column()] } })),
    );

    await renderWithRouter(<Fleet />);
    await openComposer();

    const start = screen.getByRole("button", { name: "Start job" }) as HTMLButtonElement;
    expect(start.disabled).toBe(true);
    const hint = document.getElementById(start.getAttribute("aria-describedby")!.split(" ")[0])!;
    expect(hint.textContent).toMatch(/waits until this says what the job should work on/);

    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "tidy the imports" } });
    expect(start.disabled).toBe(false);
  });

  it("offers every project, disables the ones the núcleo would refuse and says why, and starts in the roomiest", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 10, held: 3 },
            projects: [
              column({ project_id: "alpha", limit: 2, slots: [slot({ owner_id: 41 })] }),
              column({ project_id: "bravo", limit: 3, slots: [] }),
              column({
                project_id: "charlie",
                limit: 2,
                slots: [
                  slot({ project_id: "charlie", slot: 0, owner_id: 61 }),
                  slot({ project_id: "charlie", slot: 1, owner_id: 62 }),
                ],
              }),
              column({ project_id: "delta", limit: 3, slots: [] }),
            ],
          },
          jobs: [job({ id: 41 }), job({ id: 61, project_id: "charlie" }), job({ id: 62, project_id: "charlie" })],
          projects: [
            startable("alpha"),
            startable("bravo"),
            startable("charlie"),
            project({ project_id: "delta", mode: "off" }),
          ],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    await openComposer();

    const select = screen.getByLabelText("Project for the new job") as HTMLSelectElement;
    // Off is not offered at all rather than offered disabled: once `/projects` says so, delta goes.
    await waitFor(() => expect(select.querySelector('option[value="delta"]')).toBeNull());
    const options = Object.fromEntries([...select.options].map((option) => [option.value, option]));
    expect(options.alpha.textContent).toBe("alpha — room for 1 (1 of 2)");
    expect(options.alpha.disabled).toBe(false);
    expect(options.charlie.textContent).toBe("charlie — full (2 of 2)");
    expect(options.charlie.disabled).toBe(true);
    // The most room of the projects that can take one.
    expect(select.value).toBe("bravo");

    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "tidy the imports" } });
    fireEvent.click(screen.getByRole("button", { name: "Start job" }));
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/jobs", {
        method: "POST",
        body: JSON.stringify({
          project_id: "bravo",
          prompt: "tidy the imports",
          budget_usd: null,
          max_rounds: null,
          team_id: null,
        }),
      });
    });
  });

  it("holds the button while the kill switch is engaged, and says so beside it and in the headline", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({ kill: true, concurrency: { house: { limit: 4, held: 0 }, projects: [column()] } }),
      ),
    );

    await renderWithRouter(<Fleet />);
    await waitFor(async () => expect(await headlineText()).toContain("the kill switch is engaged — no new job starts"));
    await openComposer();
    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "tidy the imports" } });

    const start = screen.getByRole("button", { name: "Start job" }) as HTMLButtonElement;
    expect(start.disabled).toBe(true);
    const note = screen.getByText(/the núcleo refuses every new job until it is released/);
    expect(start.getAttribute("aria-describedby")).toContain(note.id);
  });

  it("says the kill switch and a full project differently, though both are 409, and beside the button", async () => {
    // `POST /jobs` refuses in prose for both, so the status cannot tell them
    // apart and neither can the status-derived code. Collapsing them into one
    // sentence would tell somebody to wait for room that is already there, or
    // to release a switch that was never engaged.
    await refuseWith(
      new ApiRefusal(409, "conflict", "the kill switch is engaged; nothing autonomous starts"),
    );
    const said = await screen.findByText(/kill switch is engaged; nothing autonomous starts/);
    // Not the shared floor's sentence for a 409 ("something about this has
    // already changed"), which is what both refusals would collapse to if the
    // page mapped the status-derived code to copy of its own.
    expect(said.textContent).not.toMatch(/already changed/);
    expect(said.textContent).not.toMatch(/piece\(s\) of work/);
    // Next to the button that asked, not at the top of the page.
    const start = screen.getByRole("button", { name: "Start job" });
    expect(start.parentElement!.contains(said)).toBe(true);
  });

  it("shows the daemon's own sentence when the project is full", async () => {
    await refuseWith(
      new ApiRefusal(409, "conflict", "this project already has 2 piece(s) of work in flight"),
    );
    const said = await screen.findByText(/this project already has 2 piece\(s\) of work in flight/);
    expect(said.textContent).not.toMatch(/kill switch/);
  });

  /**
   * **The gap this closes.** `POST /jobs` has taken a `team_id` since the
   * parallel work landed and nothing in the product ever sent one, so a job
   * could only be directed by writing JSON by hand or a `graph:` rule into a
   * config file.
   */
  it("offers the catalogue's teams and sends the one chosen", async () => {
    const state = fleetState({
      concurrency: { house: { limit: 4, held: 0 }, projects: [column({ slots: [] })] },
      teams: [teamView(), teamView({ id: "infra", name: "Infra", max_parallel: 3 })],
    });
    daemon.apiFetch.mockImplementation(fleetFetch(state));

    await renderWithRouter(<Fleet />);
    await openComposer();
    const picker = await screen.findByLabelText("Team to direct the job");
    // The ceiling is on the option and not only the name, because it is the
    // whole difference between the two choices: a team is what the job runs
    // items *at once* with, and the number is how many.
    expect(within(picker as HTMLElement).getByText(/Infra — up to 3 at once/)).toBeDefined();

    fireEvent.change(picker, { target: { value: "infra" } });
    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "tidy the imports" } });
    fireEvent.click(screen.getByRole("button", { name: "Start job" }));

    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/jobs", {
        method: "POST",
        body: JSON.stringify({
          project_id: "alpha",
          prompt: "tidy the imports",
          budget_usd: null,
          max_rounds: null,
          team_id: "infra",
        }),
      });
    });
  });

  /**
   * Gone rather than disabled, for the reason `max_items` has no field at all:
   * a control whose only option is the default does nothing, and drawing it
   * would advertise a feature whose first step is on another page.
   */
  it("draws no team picker when there are no teams", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: { house: { limit: 4, held: 0 }, projects: [column({ slots: [] })] },
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    await openComposer();
    expect(screen.getByLabelText("Prompt")).toBeDefined();
    expect(screen.queryByLabelText("Team to direct the job")).toBeNull();
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
    await pastTheDwell();
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

  it("never tells a screen reader it can delete what nothing here may delete, and never zooms past legible", () => {
    render(<FleetCanvas nodes={[]} edges={[]} ends={{}} onPropose={() => {}} onLayoutChange={() => {}} />);

    const props = seen.props[seen.props.length - 1];
    const aria = props.ariaLabelConfig as Record<string, string>;
    for (const said of Object.values(aria)) expect(said.toLowerCase()).not.toContain("delete");
    expect(document.body.textContent?.toLowerCase()).not.toContain("press delete");
    expect((props.fitViewOptions as { minZoom: number }).minZoom).toBeGreaterThanOrEqual(0.8);
    // The gesture is explained where it is made.
    expect(screen.getByText(/drag a line from the dot on one card's edge/)).toBeDefined();
  });

  it("names every node and every edge in words a person would use", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 4, held: 2 },
            projects: [column({ slots: [slot({ slot: 0, owner_id: 41 }), slot({ slot: 1, owner_id: 55 })] })],
          },
          jobs: [job({ id: 41 }), job({ id: 55 })],
          exclusions: [exclusion()],
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    await screen.findByRole("region", { name: "alpha column" });
    fireEvent.click(screen.getByRole("button", { name: "Canvas" }));

    // Asserted on what xyflow is handed rather than on its DOM: jsdom gives the pane no size,
    // so the library lays out no node wrappers here. It turns `ariaLabel` into the wrapper's
    // `aria-label` — the element that takes focus on the surface — and the screenshot harness
    // checks that in a real browser.
    await waitFor(() => expect(seen.props.length).toBeGreaterThan(0));
    const handed = seen.props[seen.props.length - 1];
    const nodes = handed.nodes as Array<{ ariaLabel?: string }>;
    expect(nodes.map((node) => node.ariaLabel)).toEqual([
      "alpha, slot 0 — job 41",
      "alpha, slot 1 — job 55",
    ]);
    const edges = handed.edges as Array<{ ariaLabel?: string }>;
    expect(edges.map((edge) => edge.ariaLabel)).toEqual(["job 41 and job 55 never run at the same time"]);
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

  it("lays a project's cards out on one row and draws a labelled region around them", () => {
    const model = buildFleet({
      concurrency: {
        house: { limit: 6, held: 3 },
        projects: [
          column({ project_id: "bravo", limit: 2, slots: [slot({ project_id: "bravo", slot: 0, owner_id: 9 })] }),
          column({ slots: [slot({ slot: 0, owner_id: 41 }), slot({ slot: 1, owner_id: 55 })] }),
        ],
      },
      jobs: [job({ id: 41 }), job({ id: 55 }), job({ id: 9, project_id: "bravo" })],
      runs: [],
      exclusions: [],
      requests: [],
      layout: {},
    });

    const at = Object.fromEntries(model.nodes.map((node) => [node.id, node.position]));
    // Same project, same row; the next project, the next row.
    expect(at["job:41"].y).toBe(at["job:55"].y);
    expect(at["job:55"].x).toBeGreaterThan(at["job:41"].x);
    expect(at["job:9"].y).toBeGreaterThan(at["job:41"].y);

    const zones = zonesFor(model.nodes);
    const alpha = zones.find((zone) => zone.projectId === "alpha")!;
    expect(alpha.label).toBe("alpha 2/2");
    // The region contains both of its cards.
    expect(alpha.x).toBeLessThan(at["job:41"].x);
    expect(alpha.x + alpha.width).toBeGreaterThan(at["job:55"].x);
    expect(model.nodes.find((node) => node.id === "job:9")?.ariaLabel).toBe("bravo, slot 0 — job 9");
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
                item(),
                item({
                  ordinal: 1,
                  description: "update the callers",
                  round: 1,
                  run_id: 101,
                  // Nothing measured this one. `passed` with a NULL gate is
                  // "no gate configured", not "the tests passed".
                  gate_status: null,
                }),
                item({ ordinal: 2, description: "merge the importer", round: 1, status: "conflicted", gate_status: null }),
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
    expect(within(items).getAllByText(/no gate configured/i).length).toBeGreaterThan(0);
    expect(within(items).getByText(/gate passed/i)).toBeDefined();
    // Every item's own state through the one map, in the same words its slot card uses.
    expect(within(items).getByText("did not merge").className).toContain("ui-badge-paused");
    expect(within(items).getAllByText("done")[0].className).toContain("ui-badge-info");
  });

  /**
   * **Why these two and not those.** The question a parallel queue provokes and
   * a sequential one never did — and until the daemon put `agent_id`,
   * `depends_on` and `files` on the wire, the answer lived only inside the fold
   * that made the batch.
   *
   * The ordinals are shown `+1`, matching the number on the row above them: a
   * `depends_on` of `[0]` refers to the item drawn as `1`, and printing the raw
   * value would name a row that is not on screen.
   */
  it("a directed job's queue says who has each item and what it waits for", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 4, held: 1 },
            projects: [column({ slots: [slot()] })],
          },
          jobs: [job()],
          details: {
            41: {
              ...job({ team_id: "infra", team_name: "Infra", team_max_parallel: 3 }),
              branch: "nucleos/job-41",
              items: [
                item({
                  description: "widen the column",
                  agent_id: "ana",
                  agent_name: "Ana Field",
                  files: ["core/src/job.rs"],
                }),
                item({
                  ordinal: 1,
                  description: "update the callers",
                  status: "pending",
                  gate_status: null,
                  agent_id: "bo",
                  agent_name: "Bo Rivers",
                  depends_on: [0],
                  files: ["shell/src/data/fleet.ts"],
                }),
              ],
            },
          },
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    fireEvent.click(await screen.findByRole("button", { name: "Show items" }));

    // The ceiling is said once, above the queue: it is a fact about the job, and
    // it is what makes two rows running at once legible rather than alarming.
    expect(await screen.findByText(/up to 3 items at once/)).toBeDefined();
    expect(screen.getByRole("link", { name: "Infra" }).getAttribute("href")).toBe("/teams/infra");

    const items = screen.getByRole("list", { name: "items of job 41" });
    expect(within(items).getByText("Ana Field")).toBeDefined();
    expect(within(items).getByText("core/src/job.rs")).toBeDefined();
    // `after 1` and not `after 0` — the row it names is the one drawn as `1`.
    expect(within(items).getByText(/after 1 · shell\/src\/data\/fleet\.ts/)).toBeDefined();
  });

  /**
   * A job nobody directs has no answer to give, which is different from an
   * answer of nothing — so the line is absent rather than empty, and the team
   * heading with it.
   */
  it("says none of that about a job no team directs", async () => {
    daemon.apiFetch.mockImplementation(
      fleetFetch(
        fleetState({
          concurrency: {
            house: { limit: 4, held: 1 },
            projects: [column({ slots: [slot()] })],
          },
          jobs: [job()],
          details: {
            41: { ...job(), branch: "nucleos/job-41", items: [item()] },
          },
        }),
      ),
    );

    await renderWithRouter(<Fleet />);
    fireEvent.click(await screen.findByRole("button", { name: "Show items" }));

    await screen.findByRole("list", { name: "items of job 41" });
    expect(screen.queryByText(/directed by/)).toBeNull();
    expect(screen.queryByText(/items at once/)).toBeNull();
    expect(screen.queryByText(/^after /)).toBeNull();
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
          exclusions: [exclusion()],
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
    // A rule in force holds a job back: Held Ember, never the shadow tone.
    expect(within(low).getByText("rule in force").className).toContain("ui-badge-paused");

    // The rule in force wins over the request that asked for the same pair:
    // drawing both would be one constraint rendered twice.
    expect(screen.queryByText(/waiting for a decision/)).toBeNull();
    // And a pair already joined is not offered again.
    expect(within(low).queryByLabelText("Never at the same time as")).toBeNull();
  });
});
