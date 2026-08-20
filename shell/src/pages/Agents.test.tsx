import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Agents } from "./Agents";
import { ApiRefusal } from "../data/client";
import type { Agent } from "../data/agents";
import { daemonFetch, daemonState, renderApp, renderWithQuery } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

/* ------------------------------------------------------------ fixtures -- */

function agent(overrides: Partial<Agent> = {}): Agent {
  return {
    id: "copywriter",
    name: "copywriter",
    speciality: "writes short copy",
    prompt: "You write short, punchy copy.",
    engine: "claude",
    model: null,
    tool_policy: "mcp_only",
    created_at: "2026-08-01T09:00:00Z",
    updated_at: "2026-08-01T09:00:00Z",
    ...overrides,
  };
}

/**
 * The agent routes, over mutable state — the same shape `councilFetch` in
 * `Council.test.tsx` and `errandsFetch` in `Errands.test.tsx` use: a page that
 * refetches after a mutation needs the next `GET` to answer with the changed
 * row, not with whatever a one-shot mock happened to return first.
 */
function agentsFetch(
  rows: Agent[],
  opts: {
    onCreate?: (body: Record<string, unknown>) => unknown;
    onUpdate?: (id: string, body: Record<string, unknown>) => unknown;
    onDelete?: (id: string) => unknown;
  } = {},
): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path, init) => {
    if (path === "/agents" && init?.method === "POST") {
      const body = JSON.parse(init.body as string) as Record<string, unknown>;
      if (opts.onCreate !== undefined) return opts.onCreate(body);
      const created: Agent = {
        id: String(body.name),
        name: String(body.name),
        speciality: String(body.speciality),
        prompt: String(body.prompt),
        engine: String(body.engine),
        model: (body.model as string | null) ?? null,
        tool_policy: String(body.tool_policy),
        created_at: "2026-08-18T09:00:00Z",
        updated_at: "2026-08-18T09:00:00Z",
      };
      rows.push(created);
      return created;
    }
    if (path === "/agents") return rows;

    const idMatch = /^\/agents\/([^/]+)$/.exec(path);
    if (idMatch !== null && init?.method === "PUT") {
      const id = decodeURIComponent(idMatch[1]);
      const body = JSON.parse(init.body as string) as Record<string, unknown>;
      if (opts.onUpdate !== undefined) return opts.onUpdate(id, body);
      const row = rows.find((candidate) => candidate.id === id);
      if (row === undefined) throw new ApiRefusal(404, "not_found", "");
      row.name = String(body.name);
      row.speciality = String(body.speciality);
      row.prompt = String(body.prompt);
      row.engine = String(body.engine);
      row.model = (body.model as string | null) ?? null;
      row.tool_policy = String(body.tool_policy);
      return row;
    }
    if (idMatch !== null && init?.method === "DELETE") {
      const id = decodeURIComponent(idMatch[1]);
      if (opts.onDelete !== undefined) return opts.onDelete(id);
      const index = rows.findIndex((candidate) => candidate.id === id);
      if (index === -1) throw new ApiRefusal(404, "not_found", "");
      rows.splice(index, 1);
      return undefined;
    }
    return undefined;
  };
}

/** The dwell `ConfirmButton` needs between arming and confirming — see `KillSwitchControl.test.tsx`. */
async function afterDwell(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 350));
}

/* --------------------------------------------------------------- A20 -- */

describe("Agents - the policy control and the local-engine model requirement", () => {
  it("offers mcp_only and none and never unrestricted", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([]));
    renderWithQuery(<Agents />);

    const policySelect = await screen.findByLabelText("Tool policy");
    const optionLabels = within(policySelect as HTMLSelectElement)
      .getAllByRole("option")
      .map((option) => option.textContent);

    expect(optionLabels).toEqual(["mcp_only", "none"]);
    expect(optionLabels).not.toContain("unrestricted");
  });

  it("refuses submit when engine is local and no model is named", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([]));
    renderWithQuery(<Agents />);

    const newAgentPanel = (await screen.findByRole("heading", { level: 2, name: "New agent" })).closest(
      "section",
    ) as HTMLElement;

    fireEvent.change(within(newAgentPanel).getByLabelText("Name"), { target: { value: "analyst" } });
    fireEvent.change(within(newAgentPanel).getByLabelText("Speciality"), {
      target: { value: "reads spreadsheets" },
    });
    const submit = within(newAgentPanel).getByRole("button", { name: "Add agent" }) as HTMLButtonElement;
    expect(submit.disabled).toBe(false);

    fireEvent.change(within(newAgentPanel).getByLabelText("Engine"), { target: { value: "local" } });
    expect(submit.disabled).toBe(true);

    fireEvent.change(within(newAgentPanel).getByLabelText("Model"), { target: { value: "llama-local" } });
    expect(submit.disabled).toBe(false);
  });
});

/* --------------------------------------------------------------- A21 -- */

describe("Agents - editing a row", () => {
  it("sends a PUT carrying every field including the unchanged ones, and the list shows the returned agent", async () => {
    const rows = [agent({ id: "copywriter", name: "copywriter", speciality: "writes short copy" })];
    daemon.apiFetch.mockImplementation(agentsFetch(rows));
    renderWithQuery(<Agents />);

    const row = (await screen.findByText("copywriter")).closest("li") as HTMLElement;
    fireEvent.click(within(row).getByRole("button", { name: "Edit" }));

    fireEvent.change(within(row).getByLabelText("Speciality"), {
      target: { value: "writes long-form copy" },
    });
    fireEvent.click(within(row).getByRole("button", { name: "Save changes" }));

    await screen.findByText("writes long-form copy");

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
          tool_policy: "mcp_only",
        }),
      }),
    );
  });
});

describe("Agents - a delete answering 409", () => {
  it("renders a sentence naming a team as the holder, beside that row only", async () => {
    const rows = [agent({ id: "director", name: "director" }), agent({ id: "spare", name: "spare" })];
    daemon.apiFetch.mockImplementation(
      agentsFetch(rows, {
        onDelete: (id) => {
          if (id === "director") throw new ApiRefusal(409, "conflict", "");
          rows.splice(
            rows.findIndex((candidate) => candidate.id === id),
            1,
          );
          return undefined;
        },
      }),
    );
    renderWithQuery(<Agents />);

    const directorRow = (await screen.findByText("director")).closest("li") as HTMLElement;
    const spareRow = (await screen.findByText("spare")).closest("li") as HTMLElement;

    fireEvent.click(within(directorRow).getByRole("button", { name: "Delete" }));
    await afterDwell();
    fireEvent.click(within(directorRow).getByRole("button", { name: /Delete director/ }));

    expect(await within(directorRow).findByText("a team is standing on this agent")).toBeDefined();
    expect(within(spareRow).queryByText("a team is standing on this agent")).toBeNull();
    expect(screen.queryByText(/something about this has already changed/i)).toBeNull();
  });
});

describe("Agents - a create answering 409", () => {
  it("renders the duplicate-name sentence, proving the two 409s do not share copy", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([], {
        onCreate: () => {
          throw new ApiRefusal(409, "conflict", "");
        },
      }),
    );
    renderWithQuery(<Agents />);

    const newAgentPanel = (await screen.findByRole("heading", { level: 2, name: "New agent" })).closest(
      "section",
    ) as HTMLElement;
    fireEvent.change(within(newAgentPanel).getByLabelText("Name"), { target: { value: "copywriter" } });
    fireEvent.change(within(newAgentPanel).getByLabelText("Speciality"), {
      target: { value: "writes short copy" },
    });
    fireEvent.click(within(newAgentPanel).getByRole("button", { name: "Add agent" }));

    expect(await screen.findByText("an agent of that name already exists")).toBeDefined();
    expect(screen.queryByText("a team is standing on this agent")).toBeNull();
  });
});

/* --------------------------------------------------------------- A22 -- */

describe("Agents - the list", () => {
  it("renders no provenance column and reads a null model as the engine's default", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent({ model: null })]));
    renderWithQuery(<Agents />);

    expect(await screen.findByText("the engine's default")).toBeDefined();
    expect(screen.queryByText(/provenance/i)).toBeNull();
    expect(screen.queryByText(/recruit/i)).toBeNull();
  });

  it("teaches what an agent is for when the catalogue is empty", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([]));
    renderWithQuery(<Agents />);

    expect(await screen.findByRole("heading", { name: "No agent has been hired yet" })).toBeDefined();
    expect(screen.getByText(/a name, a speciality/i)).toBeDefined();
  });
});

/* ------------------------------------------------------------- the route -- */

describe("Agents - the route", () => {
  it("is registered in the real tree and is no longer the placeholder", async () => {
    const rows = [agent()];
    const shared = daemonFetch(daemonState());
    const agents = agentsFetch(rows);
    daemon.apiFetch.mockImplementation(async (path, init) => {
      if (path === "/agents" || path.startsWith("/agents/")) return agents(path, init);
      return await shared(path, init);
    });

    const { router } = await renderApp({ initialPath: "/agents" });

    expect(await screen.findByRole("heading", { level: 1, name: "Agents" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/agents");
    expect(screen.queryByText("Agents is not built yet")).toBeNull();
  });
});
