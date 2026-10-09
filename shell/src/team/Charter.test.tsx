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

import { Charter, takeTheirs, teamFormFromView } from "./Charter";
import { detectDrift, snapshotFromView } from "./drift";
import { createAppQueryClient } from "../app/queryClient";
import type { TeamView } from "../data/teams";
import type { Known } from "../data/knowledge";
import { known } from "../brain/knowledge/test-helpers";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

function teamView(overrides: Partial<TeamView> = {}): TeamView {
  return {
    id: "financas",
    name: "Finanças",
    mission: "keep the books straight",
    director_agent_id: "controller",
    max_rounds: 4,
    max_parallel: 2,
    budget_usd: 5,
    max_open_actions: 5,
    max_live_runs: 1,
    created_at: "2026-08-18T09:00:00Z",
    updated_at: "2026-08-18T09:00:00Z",
    members: ["controller", "auditor"],
    grants: [{ kind: "send_email", mode: "propose" }],
    ...overrides,
  };
}

/**
 * The Charter over a fake daemon whose department can be swapped mid-test —
 * which is the whole subject: the guard exists because the daemon's copy can
 * change while the form is open.
 */
async function renderCharter(seed: TeamView, opts: { later?: TeamView; knowledge?: Known[] } = {}) {
  const state = { team: seed, reads: 0 };

  daemon.apiFetch.mockImplementation(async (path: string) => {
    if (path === "/agents") {
      return [
        { id: "controller", name: "controller", speciality: "", prompt: "", engine: "claude", model: null, tool_policy: "", created_at: "", updated_at: "" },
        { id: "auditor", name: "auditor", speciality: "", prompt: "", engine: "claude", model: null, tool_policy: "", created_at: "", updated_at: "" },
        { id: "tax-analyst", name: "tax-analyst", speciality: "", prompt: "", engine: "claude", model: null, tool_policy: "", created_at: "", updated_at: "" },
      ];
    }
    if (path === "/teams") return [state.team];
    if (path === "/knowledge") return opts.knowledge ?? [];
    if (/^\/teams\/[^/]+$/.exec(path) !== null) {
      state.reads += 1;
      // The second read is the guard's re-read at submit — where the hire that
      // happened on the Decisions tab in the meantime becomes visible.
      if (state.reads > 1 && opts.later !== undefined) state.team = opts.later;
      return state.team;
    }
    return undefined;
  });

  const queryClient = createAppQueryClient();
  const rootRoute = createRootRoute({ component: () => <Outlet /> });
  const routes = [
    createRoute({ getParentRoute: () => rootRoute, path: "/teams", component: () => null }),
    createRoute({
      getParentRoute: () => rootRoute,
      path: "/teams/$teamId",
      component: () => <Charter team={seed} runs={[]} />,
    }),
  ];
  const router = createRouter({
    routeTree: rootRoute.addChildren(routes),
    history: createMemoryHistory({ initialEntries: ["/teams/financas"] }),
    defaultPreload: false,
  });

  await router.load();
  const result = render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { ...result, router, queryClient, state };
}

/**
 * Every write the shell sent, as it sent it — the mock's calls are `any[]`, so
 * they are narrowed once here rather than at four call sites.
 */
function writes(): { path: string; init: RequestInit | undefined }[] {
  return (daemon.apiFetch.mock.calls as unknown[][]).map((call) => ({
    path: call[0] as string,
    init: call[1] as RequestInit | undefined,
  }));
}

function puts(): { path: string; init: RequestInit | undefined }[] {
  return writes().filter((call) => /^\/teams\/[^/]+$/.test(call.path) && call.init?.method === "PUT");
}

/** What the last PUT carried, parsed. */
function lastPut(): Record<string, unknown> {
  const sent = puts();
  const last = sent[sent.length - 1];
  return JSON.parse(last.init?.body as string) as Record<string, unknown>;
}

/* ---------------------------------------------------------------- save -- */

describe("Charter - saving", () => {
  it("sends the roster and the powers whole, because PUT is a full replace", async () => {
    await renderCharter(teamView());

    // One edit, to something that has nothing to do with either list.
    const mission = await screen.findByLabelText("Mission");
    fireEvent.change(mission, { target: { value: "keep the books straight, and file them" } });

    fireEvent.click(await screen.findByRole("button", { name: "Save" }));

    await waitFor(() => {
      const body = lastPut();
      // Omitting either would wipe it: `replace_roster` deletes and re-inserts.
      expect(body.members).toEqual(["controller", "auditor"]);
      expect(body.grants).toEqual([{ kind: "send_email", mode: "propose" }]);
      expect(body.mission).toBe("keep the books straight, and file them");
      // The daemon slugs the id from the name; sending one would be a claim.
      expect(body.id).toBeUndefined();
    });
  });

  it("keeps the save bar out of the way until something has changed", async () => {
    await renderCharter(teamView());

    await screen.findByLabelText("Mission");
    // A bar that is always there stops being a signal that anything changed.
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();

    fireEvent.change(screen.getByLabelText("Mission"), { target: { value: "and file them" } });
    expect(await screen.findByRole("button", { name: "Save" })).toBeDefined();
  });

  it("writes an absent budget ceiling as no ceiling and never as zero", async () => {
    await renderCharter(teamView({ budget_usd: null }));

    const box = (await screen.findByLabelText(/Budget ceiling/)) as HTMLInputElement;
    expect(box.value).toBe("");
    expect(screen.getByText("no ceiling")).toBeDefined();

    fireEvent.change(screen.getByLabelText("Mission"), { target: { value: "and file them" } });
    fireEvent.click(await screen.findByRole("button", { name: "Save" }));

    await waitFor(() => expect(lastPut().budget_usd).toBeNull());
  });
});

/* --------------------------------------------------------------- guard -- */

describe("Charter - the drift guard", () => {
  it("asks when the re-read differs from the seed in a field nobody was editing", async () => {
    // 09:41 seeded with two; 09:52 a recruit is hired on the Decisions tab.
    const after = teamView({ members: ["controller", "auditor", "tax-analyst"] });
    await renderCharter(teamView(), { later: after });

    fireEvent.change(await screen.findByLabelText("Mission"), { target: { value: "and file them" } });
    fireEvent.click(await screen.findByRole("button", { name: "Save" }));

    const guard = await screen.findByRole("alert", { name: "Changed while you were editing" });
    expect(within(guard).getByText("Staff")).toBeDefined();
    expect(within(guard).getByText(/auditor, controller, tax-analyst/)).toBeDefined();

    // Three answers, and none of them is a save that says nothing.
    expect(within(guard).getByRole("button", { name: "Take theirs and save" })).toBeDefined();
    expect(within(guard).getByRole("button", { name: "Reload the form" })).toBeDefined();
    expect(within(guard).getByRole("button", { name: "Save mine anyway" })).toBeDefined();

    // And nothing has been sent while the question is open.
    expect(puts()).toHaveLength(0);
  });

  it("takes the new roster and keeps the half-typed edit when told to", async () => {
    const after = teamView({ members: ["controller", "auditor", "tax-analyst"] });
    await renderCharter(teamView(), { later: after });

    fireEvent.change(await screen.findByLabelText("Mission"), { target: { value: "and file them" } });
    fireEvent.click(await screen.findByRole("button", { name: "Save" }));

    const guard = await screen.findByRole("alert", { name: "Changed while you were editing" });
    fireEvent.click(within(guard).getByRole("button", { name: "Take theirs and save" }));

    await waitFor(() => {
      const body = lastPut();
      // The hire survives...
      expect(body.members).toEqual(["controller", "auditor", "tax-analyst"]);
      // ...and so does what was being typed.
      expect(body.mission).toBe("and file them");
    });
  });

  it("stays quiet when the person edited the same field themselves", async () => {
    // Removing somebody on purpose is their decision, and there is nothing to
    // ask about — the guard must not turn every roster edit into a question.
    const after = teamView({ members: ["controller", "auditor", "tax-analyst"] });
    await renderCharter(teamView(), { later: after });

    const members = (await screen.findByLabelText("Members")) as HTMLSelectElement;
    for (const option of Array.from(members.options)) option.selected = option.value === "controller";
    fireEvent.change(members);

    fireEvent.click(await screen.findByRole("button", { name: "Save" }));

    await waitFor(() => expect(lastPut().members).toEqual(["controller"]));
    expect(screen.queryByRole("alert", { name: "Changed while you were editing" })).toBeNull();
  });

  it("sends nothing at all when the team cannot be re-read", async () => {
    await renderCharter(teamView());
    fireEvent.change(await screen.findByLabelText("Mission"), { target: { value: "and file them" } });

    // The re-read is what the guard compares against. Without it there is
    // nothing to compare, and this call is a full replace.
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/agents") return [];
      if (path === "/teams") return [teamView()];
      throw new Error("the daemon is not answering");
    });

    fireEvent.click(await screen.findByRole("button", { name: "Save" }));

    expect(await screen.findByText(/nothing was sent/)).toBeDefined();
    expect(puts()).toHaveLength(0);
  });
});

/* ------------------------------------------------------------- memory -- */

describe("Charter - the team's own memory", () => {
  it("the charter shows the team's own memory", async () => {
    await renderCharter(teamView(), {
      knowledge: [
        known({ id: 1, scope_kind: "team", scope_id: "financas", title: "Close the books monthly" }),
        known({ id: 2, scope_kind: "team", scope_id: "outra", title: "Another team's rule" }),
        known({ id: 3, scope_kind: "agent", scope_id: "financas", title: "An agent with the same id" }),
      ],
    });

    const memory = await screen.findByRole("region", { name: "Memory" });

    expect(await within(memory).findByText("Close the books monthly")).toBeDefined();
    expect(within(memory).queryByText("Another team's rule")).toBeNull();
    expect(within(memory).queryByText("An agent with the same id")).toBeNull();
  });
});

/* ----------------------------------------------------------- takeTheirs -- */

describe("takeTheirs", () => {
  it("copies only the drifted fields out of the daemon's answer", async () => {
    const seed = teamView();
    const fresh = teamView({ members: ["controller", "auditor", "tax-analyst"], max_rounds: 6 });
    const form = { ...teamFormFromView(seed), mission: "half a sentence" };

    // `maxRounds` is theirs too, but the person was editing it — so it is not
    // in the drift and must not be taken.
    const drifted = detectDrift(snapshotFromView(seed), snapshotFromView(fresh), ["maxRounds"]);
    const merged = takeTheirs(form, fresh, drifted);

    expect(merged.members).toEqual(["controller", "auditor", "tax-analyst"]);
    expect(merged.maxRounds).toBe("4");
    expect(merged.mission).toBe("half a sentence");
  });
});
