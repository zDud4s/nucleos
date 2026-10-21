import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Autopilot } from "./Autopilot";
import { ExclusionsPanel } from "./AutopilotExclusions";
import type { FleetExclusion, Job } from "../data/fleet";
import { keys } from "../data/keys";
import { daemonFetch, daemonState, renderWithRouter } from "../test/harness";

function job(id: number, project_id = "alpha"): Job {
  return {
    id, project_id, rule_name: null, status: "implementing", wait_reason: null, max_items: 4,
    created_at: "2026-10-20T09:00:00Z", completed_at: null, slot: 0, round: 0, max_rounds: 3,
    team_id: null, team_name: null, team_max_parallel: null,
  };
}
function rule(over: Partial<FleetExclusion> = {}): FleetExclusion {
  return { id: 3, project_id: "alpha", job_low: 41, job_high: 55, proposal_id: 9, paths: null, created_at: "2026-10-20T09:00:00Z", ...over };
}

interface World { jobs: Job[]; rules: FleetExclusion[] }
function respond(world: World) {
  const shared = daemonFetch(daemonState());
  return async (path: string, init?: RequestInit) => {
    if (path === "/fleet/exclusions" && init?.method === "POST") return { proposal_id: 1 };
    if (path === "/fleet/exclusions") return world.rules;
    if (path === "/jobs?live=true") return world.jobs;
    return await shared(path, init);
  };
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
});

const pastTheDwell = () => act(async () => void (await new Promise((r) => setTimeout(r, 350))));

describe("Autopilot - exclusions", () => {
  it("lists the rules in force and says who waits", async () => {
    daemon.apiFetch.mockImplementation(respond({ jobs: [], rules: [rule()] }));
    await renderWithRouter(<ExclusionsPanel />);
    const list = await screen.findByRole("list", { name: "Exclusions in force" });
    expect(within(list).getByText("job 41 · job 55")).toBeDefined();
    expect(within(list).getByText(/job 55 waits/)).toBeDefined();
  });

  it("says so when no two jobs are kept apart", async () => {
    daemon.apiFetch.mockImplementation(respond({ jobs: [], rules: [] }));
    await renderWithRouter(<ExclusionsPanel />);
    expect(await screen.findByText("No two jobs are kept apart.")).toBeDefined();
  });

  it("lifts a rule only through the interlock", async () => {
    daemon.apiFetch.mockImplementation(respond({ jobs: [], rules: [rule()] }));
    await renderWithRouter(<ExclusionsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "Lift" }));
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/fleet/exclusions/3", expect.anything());
    await pastTheDwell();
    fireEvent.click(screen.getByRole("button", { name: "Lift the rule" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/fleet/exclusions/3", { method: "DELETE" }),
    );
  });

  it("asks the pairing question with the same request the canvas used to send", async () => {
    daemon.apiFetch.mockImplementation(respond({ jobs: [job(41), job(55)], rules: [] }));
    await renderWithRouter(<ExclusionsPanel />);
    const partner = (await screen.findByLabelText("Never at the same time as")) as HTMLSelectElement;
    expect([...partner.options].map((o) => o.textContent)).toEqual(["job 55"]);
    fireEvent.click(screen.getByRole("button", { name: "Ask" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/fleet/exclusions", {
        method: "POST",
        body: JSON.stringify({ job_a: 41, job_b: 55, paths: [] }),
      }),
    );
    expect(await screen.findByText(/Nothing changes until it is answered in/)).toBeDefined();
  });

  it("never offers a pair of different projects, nor a pair already kept apart", async () => {
    // The message also shows while the queries are loading, so wait for both answers before asserting.
    const answered = async (view: Awaited<ReturnType<typeof renderWithRouter>>) => {
      await waitFor(() => {
        expect(view.queryClient.getQueryState(keys.jobs.live)?.status).toBe("success");
        expect(view.queryClient.getQueryState(keys.fleet.exclusions)?.status).toBe("success");
      });
      // react-query tells React on the next macrotask; let that render land.
      await act(async () => void (await new Promise((resolve) => setTimeout(resolve, 0))));
    };

    daemon.apiFetch.mockImplementation(respond({ jobs: [job(41), job(9, "beta")], rules: [] }));
    const first = await renderWithRouter(<ExclusionsPanel />);
    await answered(first);
    expect(screen.getByText(/two live jobs in the same project/i)).toBeDefined();
    expect(screen.queryByRole("button", { name: "Ask" })).toBeNull();
    first.unmount();

    daemon.apiFetch.mockImplementation(respond({ jobs: [job(41), job(55)], rules: [rule()] }));
    const second = await renderWithRouter(<ExclusionsPanel />);
    await answered(second);
    expect(screen.getByText(/two live jobs in the same project/i)).toBeDefined();
    expect(screen.queryByRole("button", { name: "Ask" })).toBeNull();
  });

  it("keeps the form when one live job has no partner, and does not offer that job as the first end", async () => {
    daemon.apiFetch.mockImplementation(respond({ jobs: [job(41), job(55), job(9, "beta")], rules: [] }));
    await renderWithRouter(<ExclusionsPanel />);
    const first = (await screen.findByLabelText("Job")) as HTMLSelectElement;
    expect([...first.options].map((o) => o.textContent)).toEqual(["job 41 · alpha", "job 55 · alpha"]);
    expect(screen.getByRole("button", { name: "Ask" })).toBeDefined();
    expect(screen.queryByText(/two live jobs in the same project/i)).toBeNull();
  });

  it("is a section of the Autopilot page, beside the jobs", async () => {
    daemon.apiFetch.mockImplementation(respond({ jobs: [], rules: [] }));
    daemon.probeHealth.mockResolvedValue(true);
    await renderWithRouter(<Autopilot />, { initialPath: "/autopilot" });
    expect(await screen.findByRole("heading", { name: "Never at the same time" })).toBeDefined();
  });
});
