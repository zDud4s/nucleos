import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { Agents } from "./Agents";
import { ApiRefusal } from "../data/client";
import { agent, agentsFetch, team } from "./agentsTestHelpers";
import { daemonFetch, daemonState, renderApp, renderWithRouter } from "../test/harness";

/** The page needs a router around it: every name is a link. */
async function renderAgents() {
  return renderWithRouter(<Agents />, { initialPath: "/agents" });
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  localStorage.clear();
});

/* ------------------------------------------------------------ fixtures -- */

/** Open the create dialog, which is closed until somebody asks for it. */
async function openCreate(): Promise<HTMLElement> {
  fireEvent.click(await screen.findByRole("button", { name: "New agent" }));
  return screen.findByRole("dialog", { name: "New agent" });
}

/* ------------------------------------------------------------ the shape -- */

describe("Agents - the shape of the page", () => {
  it("does not open a form over a catalogue nobody has read yet", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent()]));
    await renderAgents();

    await screen.findByRole("link", { name: "copywriter" });
    expect(screen.queryByLabelText("Name")).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();

    const panel = await openCreate();
    expect(within(panel).getByLabelText("Name")).toBeDefined();
  });

  it("draws the catalogue as one table, so every row starts at the same height", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "one", name: "one" }), agent({ id: "two", name: "two" })]),
    );
    await renderAgents();

    const table = await screen.findByRole("table");
    // Header row plus one per agent, and no second table pretending to be one.
    expect(within(table).getAllByRole("row")).toHaveLength(3);
    expect(screen.getAllByRole("table")).toHaveLength(1);
  });

  it("the catalogue says what its marks mean", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent()]));
    await renderAgents();

    const key = await screen.findByText(/directs a team/);
    expect(key.textContent).toContain("on a team’s roster");
    expect(key.textContent).toContain("no tools");
    expect(key.textContent).toContain("renamed since");
  });

  it("the key spends one glyph per meaning", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent()]));
    await renderAgents();

    const key = await screen.findByText(/directs a team/);
    expect(key.textContent).toContain("■ has tools");
    expect(key.textContent).toContain("□ no tools");
    expect(key.textContent).not.toContain("● has tools");
  });

  it("links each name to the agent's own page", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "one", name: "one" }), agent({ id: "two", name: "two" })]),
    );
    await renderAgents();

    const link = await screen.findByRole("link", { name: "two" });
    expect(link.getAttribute("href")).toBe("/agents/two");
    expect(link.getAttribute("aria-haspopup")).toBeNull();
    fireEvent.click(link);
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

/* -------------------------------------------------------------- the id -- */

describe("Agents - the name and the id", () => {
  it("draws the id on every row, because that is what the rest of the app points at", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "head-of-content", name: "head of content" })]),
    );
    await renderAgents();

    expect(await screen.findByText("head-of-content")).toBeDefined();
  });

  it("says what the id IS, rather than leaving the same word twice with no reason", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent({ id: "tradutor", name: "tradutor" })]));
    await renderAgents();

    const cell = (await screen.findByRole("link", { name: "tradutor" })).closest("th") as HTMLElement;
    // Everything separating the name from the id on screen is spatial, and none
    // of it is read out. Without this the row is the same word, twice.
    expect(cell.textContent).toContain("known to the background service as tradutor");
  });

  it("marks the id once a rename has left it behind, and says what still points at it", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent({ id: "auditor", name: "Auditor Sénior" })]));
    await renderAgents();

    const id = await screen.findByText(/auditor/, { selector: ".agents-id-diverged" });
    expect(id.textContent).toContain("auditor");
    expect(screen.getByText(/rosters, team items, job items and council seats/i)).toBeDefined();
  });

  it("leaves the id unmarked while it is still the slug of the name", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "head-of-content", name: "Head of Content" })]),
    );
    await renderAgents();

    await screen.findByText("head-of-content");
    expect(document.querySelector(".agents-id-diverged")).toBeNull();
    expect(screen.getByText(/renamed since/i).className).toContain("agents-key");
  });
});

/* --------------------------------------------------- engine, model, tools -- */

describe("Agents - what an agent can do", () => {
  it("reads a null model as the engine's default and says what naming none costs", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent({ model: null })]));
    await renderAgents();

    expect(await screen.findByText("the engine’s default")).toBeDefined();
    expect(screen.getByText(/can never take a council seat/i)).toBeDefined();
  });

  it("says nothing about council seats for an agent that names a model", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent({ model: "gpt-5-codex", engine: "codex" })]));
    await renderAgents();

    expect(await screen.findByText("gpt-5-codex")).toBeDefined();
    expect(screen.queryByText(/council seat/i)).toBeNull();
  });

  it("draws a tool policy as a capability and never as a state badge", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([
        agent({ id: "armed", name: "armed", tool_policy: "mcp_only" }),
        agent({ id: "bare", name: "bare", capabilities: [], tool_policy: "none" }),
      ]),
    );
    await renderAgents();

    await screen.findByRole("link", { name: "armed" });
    expect(screen.getByText(/mcp_only: has tools/)).toBeDefined();
    expect(screen.getByText(/none: no tools at all/)).toBeDefined();
    // The seven tones are the núcleo's state vocabulary; nothing here is a state.
    expect(document.querySelector(".ui-badge")).toBeNull();
  });

  it("says so rather than guessing when the policy is one this shell has never heard of", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent({ tool_policy: "read_only" })]));
    await renderAgents();

    expect(await screen.findByText(/read_only: this shell has no reading for that policy/)).toBeDefined();
  });

  it("renders no provenance, because nothing in the núcleo stores it", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent()]));
    await renderAgents();

    await screen.findByRole("link", { name: "copywriter" });
    expect(screen.queryByText(/provenance/i)).toBeNull();
    expect(screen.queryByText(/recruit/i)).toBeNull();
  });
});

/* ---------------------------------------------------------- employment -- */

describe("Agents - how much of an agent is spoken for", () => {
  const departments = [
    team({ id: "financas", name: "Finanças", director_agent_id: "controller", members: ["auditor"] }),
    team({ id: "marketing", name: "Marketing", director_agent_id: "editor", members: ["auditor"] }),
  ];

  it("counts how much, and leaves where to the roster matrix on the console", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "auditor", name: "auditor" })], { teams: departments }),
    );
    await renderAgents();

    expect(await screen.findByText(/on the roster of Finanças, Marketing/)).toBeDefined();
    // The matrix answers "where" and is not redrawn here.
    expect(screen.queryByText("Who works where")).toBeNull();
  });

  it("counts directing apart from being on a roster", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "controller", name: "controller" })], { teams: departments }),
    );
    await renderAgents();

    expect(await screen.findByText(/directs Finanças/)).toBeDefined();
  });

  it("counts a department once when the same agent directs it and is on its roster", async () => {
    // The two figures sit side by side under a heading that invites adding
    // them, so they have to be addable. Drawn as `◉ 1 ● 1`, one department read
    // as two.
    const own = [team({ name: "Segurança", director_agent_id: "sysadmin", members: ["sysadmin"] })];
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "sysadmin", name: "sysadmin" })], { teams: own }),
    );
    await renderAgents();

    const cell = (await screen.findByRole("link", { name: "sysadmin" }))
      .closest("tr")
      ?.querySelector(".agents-figure") as HTMLElement;
    expect(cell.textContent).toContain("directs Segurança");
    expect(cell.textContent).not.toContain("on the roster of");
  });

  it("draws both marks only when they are genuinely two departments", async () => {
    const two = [
      team({ id: "seguranca", name: "Segurança", director_agent_id: "sysadmin", members: ["sysadmin"] }),
      team({ id: "informatica", name: "Informática", director_agent_id: "closer", members: ["sysadmin"] }),
    ];
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "sysadmin", name: "sysadmin" })], { teams: two }),
    );
    await renderAgents();

    expect(await screen.findByText(/directs Segurança/)).toBeDefined();
    expect(screen.getByText(/on the roster of Informática/)).toBeDefined();
  });

  it("marks an agent no team names, and says so in the headline", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "spare", name: "spare", model: "sonnet" })], { teams: departments }),
    );
    await renderAgents();

    expect(await screen.findByText("no team names this one")).toBeDefined();
    expect(screen.getByText(/1 nobody uses/)).toBeDefined();
  });

  it("never says nobody uses an agent while the team list has not answered", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/teams") return await new Promise(() => {});
      if (path === "/agents") return [agent({ id: "spare", name: "spare", model: "sonnet" })];
      return undefined;
    });
    await renderAgents();

    expect(await screen.findByText(/the team list has not answered/)).toBeDefined();
    expect(screen.queryByText("no team names this one")).toBeNull();
    expect(screen.queryByText(/nobody uses/)).toBeNull();
  });
});

/* --------------------------------------------------------------- create -- */

describe("Agents - the policy control and the local-engine model requirement", () => {
  it("offers every known capability as a checkbox and sends the ticked ones", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([]));
    await renderAgents();

    const panel = await openCreate();
    const group = within(panel).getByRole("group", { name: "Capabilities" });
    const labels = within(group)
      .getAllByRole("checkbox")
      .map((box) => box.parentElement?.textContent);

    expect(labels).toEqual(["fs.read", "fs.write", "shell", "web", "memory.read", "mcp.nucleos"]);
  });

  it("refuses submit when engine is local and no model is named", async () => {
    const rows = agentsFetch([]);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) =>
      path === "/assistant/models"
        ? { choices: [{ id: "llama-local", label: "Llama", brain: "local", efforts: [] }], configured: "sonnet", efforts: [] }
        : rows(path, init),
    );
    await renderAgents();

    const panel = await openCreate();
    fireEvent.change(within(panel).getByLabelText("Name"), { target: { value: "analyst" } });
    fireEvent.change(within(panel).getByLabelText("Speciality"), {
      target: { value: "reads spreadsheets" },
    });
    const submit = within(panel).getByRole("button", { name: "Add agent" }) as HTMLButtonElement;
    expect(submit.disabled).toBe(false);

    fireEvent.change(within(panel).getByLabelText("Engine"), { target: { value: "local" } });
    expect(submit.disabled).toBe(true);

    await within(panel).findByRole("option", { name: "llama-local" });
    fireEvent.change(within(panel).getByLabelText("Model"), { target: { value: "llama-local" } });
    expect(submit.disabled).toBe(false);
  });
});

describe("Agents - the model is chosen, not typed", () => {
  const optionsOf = (panel: HTMLElement) =>
    within(within(panel).getByLabelText("Model")).getAllByRole("option").map((option) => option.textContent);

  it("offers Default and the engine's models, and changes them with the engine", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([]));
    await renderAgents();

    const panel = await openCreate();
    expect(within(panel).getByLabelText("Model").tagName).toBe("SELECT");
    expect(optionsOf(panel)).toEqual(["Default", "opus", "sonnet", "haiku"]);

    fireEvent.change(within(panel).getByLabelText("Engine"), { target: { value: "codex" } });
    expect(optionsOf(panel)).toEqual(["Default", "gpt-5-codex", "gpt-5"]);
  });

  it("sends Default as no model", async () => {
    const onCreate = vi.fn((body: Record<string, unknown>) => ({ ...agent(), ...body }));
    daemon.apiFetch.mockImplementation(agentsFetch([], { onCreate }));
    await renderAgents();

    const panel = await openCreate();
    fireEvent.change(within(panel).getByLabelText("Name"), { target: { value: "analyst" } });
    fireEvent.change(within(panel).getByLabelText("Speciality"), { target: { value: "reads sheets" } });
    fireEvent.change(within(panel).getByLabelText("Model"), { target: { value: "opus" } });
    fireEvent.change(within(panel).getByLabelText("Model"), { target: { value: "" } });
    fireEvent.click(within(panel).getByRole("button", { name: "Add agent" }));

    await waitFor(() => expect(onCreate).toHaveBeenCalled());
    expect(onCreate.mock.calls[0][0].model).toBeNull();
  });
});

/* --------------------------------------------------------------- editor -- */

describe("Agents - the dialogs close", () => {
  it("closes the create dialog on Escape", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent()]));
    await renderAgents();

    fireEvent.keyDown(await openCreate(), { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
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
    await renderAgents();

    const panel = await openCreate();
    fireEvent.change(within(panel).getByLabelText("Name"), { target: { value: "copywriter" } });
    fireEvent.change(within(panel).getByLabelText("Speciality"), {
      target: { value: "writes short copy" },
    });
    fireEvent.click(within(panel).getByRole("button", { name: "Add agent" }));

    expect(await screen.findByText("an agent of that name already exists")).toBeDefined();
    expect(screen.queryByText(/directs or belongs to a team/)).toBeNull();
  });
});

/* ---------------------------------------------------------------- empty -- */

describe("Agents - an empty catalogue", () => {
  it("teaches what an agent is for", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([]));
    await renderAgents();

    expect(await screen.findByText("No agent has been hired yet")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Why?" }));
    expect(screen.getByText(/a name, a speciality/i)).toBeDefined();
    // The call to action sits beside the line as well as in the header.
    expect(screen.getAllByRole("button", { name: "New agent" })).toHaveLength(2);
    expect(screen.queryByRole("table")).toBeNull();
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

describe("Agents - filtering the catalogue", () => {
  it("narrows the table by a typed query and by the without-a-model chip", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([
        agent({ id: "copywriter", name: "copywriter", model: "opus" }),
        agent({ id: "auditor", name: "auditor", speciality: "checks the books", model: null }),
      ]),
    );
    await renderAgents();

    await screen.findByRole("link", { name: "copywriter" });
    fireEvent.change(screen.getByRole("searchbox", { name: "Search agents" }), { target: { value: "books" } });
    expect(screen.queryByRole("link", { name: "copywriter" })).toBeNull();
    expect(screen.getByRole("link", { name: "auditor" })).toBeDefined();

    fireEvent.change(screen.getByRole("searchbox", { name: "Search agents" }), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: /Without a model/ }));
    expect(screen.queryByRole("link", { name: "copywriter" })).toBeNull();
    expect(screen.getByRole("link", { name: "auditor" })).toBeDefined();
  });

  it("says so when nothing matches", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent()]));
    await renderAgents();

    await screen.findByRole("link", { name: "copywriter" });
    fireEvent.change(screen.getByRole("searchbox", { name: "Search agents" }), { target: { value: "zzz" } });
    expect(screen.getByText("No agent matches that filter.")).toBeDefined();
  });
});
