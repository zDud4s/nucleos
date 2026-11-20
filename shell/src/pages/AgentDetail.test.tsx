import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiRefusal } from "../data/client";
import { daemonFetch, daemonState, renderApp } from "../test/harness";
import { known } from "../brain/knowledge/test-helpers";
import { afterDwell, agent, agentsFetch, team } from "./agentsTestHelpers";
import type { Agent } from "../data/agents";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

/** The whole app at a path, with the agent routes answered by `agentsFetch` and the rest by the shared fake. */
async function openAgent(rows: Agent[], path: string, opts: Parameters<typeof agentsFetch>[1] = {}) {
  const shared = daemonFetch(daemonState());
  const agents = agentsFetch(rows, opts);
  daemon.apiFetch.mockImplementation(async (route: string, init?: RequestInit) => {
    if (route === "/agents" || route.startsWith("/agents/") || route === "/teams" || route.startsWith("/knowledge")) {
      return agents(route, init);
    }
    return await shared(route, init);
  });
  return renderApp({ initialPath: path });
}

describe("AgentDetail - the route", () => {
  it("is registered at /agents/$agentId and reached from the list name", async () => {
    const { router } = await openAgent([agent({ id: "one", name: "one" })], "/agents");

    fireEvent.click(await screen.findByRole("link", { name: "one" }));

    expect(await screen.findByRole("heading", { level: 1, name: "one" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/agents/one");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("goes back to the list from the back link", async () => {
    const { router } = await openAgent([agent()], "/agents/copywriter");

    fireEvent.click(await screen.findByRole("link", { name: "← Agents" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/agents"));
  });

  it("says Not found for an id that is not in the catalogue, and keeps the way back", async () => {
    await openAgent([agent()], "/agents/ghost");

    expect(await screen.findByText("Not found")).toBeDefined();
    expect(screen.getByRole("link", { name: "← Agents" })).toBeDefined();
    expect(screen.queryByLabelText("Name")).toBeNull();
  });
});

describe("AgentDetail - the definition", () => {
  it("sends a PUT carrying every field, and toasts Agent saved", async () => {
    await openAgent([agent()], "/agents/copywriter");

    fireEvent.change(await screen.findByLabelText("Speciality"), { target: { value: "writes long-form copy" } });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));

    expect(await screen.findByText("Agent saved")).toBeDefined();
    expect(daemon.apiFetch).toHaveBeenCalledWith(
      "/agents/copywriter",
      expect.objectContaining({
        method: "PUT",
        body: JSON.stringify({
          name: "copywriter",
          speciality: "writes long-form copy",
          prompt: "You write short, punchy copy.",
          engine: "claude",
          model: null,
          capabilities: ["mcp.nucleos"],
        }),
      }),
    );
  });

  it("says why Save is off, and Revert restores the saved fields", async () => {
    await openAgent([agent()], "/agents/copywriter");

    const name = (await screen.findByLabelText("Name")) as HTMLInputElement;
    fireEvent.change(name, { target: { value: "  " } });
    expect(screen.getByText("Needs a name")).toBeDefined();
    expect((screen.getByRole("button", { name: "Save changes" }) as HTMLButtonElement).disabled).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "Revert" }));
    expect(name.value).toBe("copywriter");
  });

  it("keeps the saving consequence on the Save button's title", async () => {
    await openAgent([agent()], "/agents/copywriter");

    const save = await screen.findByRole("button", { name: "Save changes" });
    expect(save.getAttribute("title")).toMatch(/reaches work that is already queued/i);
  });

  it("keeps the Model hint behind a tip beside the label", async () => {
    await openAgent([agent()], "/agents/copywriter");

    expect(await screen.findByLabelText("Model")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "About model" }));
    expect(screen.getByText(/lets the engine choose/)).toBeDefined();
  });

  it("shows the save status only when there is one", async () => {
    await openAgent([agent()], "/agents/copywriter");

    const name = (await screen.findByLabelText("Name")) as HTMLInputElement;
    expect(screen.queryByText("Unsaved changes")).toBeNull();
    fireEvent.change(name, { target: { value: "copywriter 2" } });
    expect(screen.getByText("Unsaved changes")).toBeDefined();
  });
});

describe("AgentDetail - what hangs off the agent", () => {
  it("draws memory, context, tools and loadout beside the definition, with no tabs", async () => {
    await openAgent([agent()], "/agents/copywriter", {
      knowledge: [
        known({ id: 1, scope_kind: "agent", scope_id: "copywriter", title: "Copy is short" }),
        known({ id: 2, scope_kind: "agent", scope_id: "other", title: "Someone else's habit" }),
      ],
    });

    const memory = await screen.findByRole("region", { name: "Memory" });
    expect(await within(memory).findByText("Copy is short")).toBeDefined();
    expect(within(memory).queryByText("Someone else's habit")).toBeNull();
    expect(await screen.findByRole("region", { name: "Tools" })).toBeDefined();
    expect(await screen.findByRole("region", { name: "Briefing preview" })).toBeDefined();
    expect(screen.queryByRole("heading", { level: 2, name: "Definition" })).toBeNull();
    expect(screen.queryByRole("tab")).toBeNull();
  });
});

describe("AgentDetail - the model", () => {
  it("keeps a model the catalogue lacks as an extra option", async () => {
    await openAgent([agent({ model: "claude-legacy-3" })], "/agents/copywriter");

    const select = (await screen.findByLabelText("Model")) as HTMLSelectElement;
    expect(select.value).toBe("claude-legacy-3");
    expect(within(select).getAllByRole("option").map((option) => option.textContent)).toEqual([
      "Default",
      "opus",
      "sonnet",
      "haiku",
      "claude-legacy-3",
    ]);
  });

  it("reads a null model as Default", async () => {
    await openAgent([agent({ model: null })], "/agents/copywriter");

    expect(((await screen.findByLabelText("Model")) as HTMLSelectElement).value).toBe("");
  });
});

describe("AgentDetail - delete", () => {
  it("arms into a short Confirm and keeps the agent's name in the accessible name", async () => {
    await openAgent([agent()], "/agents/copywriter");

    const button = await screen.findByRole("button", { name: "Delete" });
    expect(button.textContent).toContain("Delete");
    fireEvent.click(button);
    await afterDwell();

    const armed = screen.getByRole("button", { name: "Delete copywriter" });
    expect(armed.getAttribute("aria-label")).toBe("Delete copywriter");
    expect(armed.textContent).toContain("Confirm");
    expect(armed.textContent).not.toContain("copywriter");
  });

  it("names what stands on the agent behind a tip", async () => {
    await openAgent([agent({ id: "controller", name: "controller" })], "/agents/controller", {
      teams: [team({ name: "Finanças", director_agent_id: "controller" })],
    });

    fireEvent.click(await screen.findByRole("button", { name: "What stands on this agent" }));
    expect(await screen.findByText(/directs Finanças/)).toBeDefined();
    expect(screen.getByText(/an item of a team run, or of a job/)).toBeDefined();
  });

  it("deletes and goes back to the list", async () => {
    const { router } = await openAgent([agent()], "/agents/copywriter");

    fireEvent.click(await screen.findByRole("button", { name: "Delete" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: /Delete copywriter/ }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/agents"));
    expect(daemon.apiFetch).toHaveBeenCalledWith("/agents/copywriter", expect.objectContaining({ method: "DELETE" }));
  });

  it("names all four grounds when the daemon answers 409, and stays on the page", async () => {
    const { router } = await openAgent([agent({ id: "director", name: "director" })], "/agents/director", {
      onDelete: () => {
        throw new ApiRefusal(409, "conflict", "");
      },
    });

    fireEvent.click(await screen.findByRole("button", { name: "Delete" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: /Delete director/ }));

    expect(
      await screen.findByText("this agent directs or belongs to a team, or is holding an item of a team run or of a job"),
    ).toBeDefined();
    expect(router.state.location.pathname).toBe("/agents/director");
  });
});
