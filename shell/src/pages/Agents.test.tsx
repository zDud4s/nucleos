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
import type { Agent, Employer } from "../data/agents";
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

function team(overrides: Partial<Employer> = {}): Employer {
  return { id: "financas", name: "Finanças", director_agent_id: "", members: [], ...overrides };
}

/**
 * The agent routes, over mutable state — the same shape `councilFetch` in
 * `Council.test.tsx` and `errandsFetch` in `Errands.test.tsx` use: a page that
 * refetches after a mutation needs the next `GET` to answer with the changed
 * row, not with whatever a one-shot mock happened to return first.
 *
 * `/teams` is answered here too, because the page reads it for one derived
 * number per row. An unanswered query would leave `data` undefined, which the
 * page draws as "not known" — a state worth testing on purpose and never worth
 * falling into by accident.
 */
function agentsFetch(
  rows: Agent[],
  opts: {
    teams?: Employer[];
    onCreate?: (body: Record<string, unknown>) => unknown;
    onUpdate?: (id: string, body: Record<string, unknown>) => unknown;
    onDelete?: (id: string) => unknown;
  } = {},
): (path: string, init?: RequestInit) => Promise<unknown> {
  /*
    Every read answers with FRESH objects, copied off the state this closure
    holds. Returning the array itself is what a hand-written fake reaches for
    first and it is quietly wrong: react-query keeps the previously stored value
    when the next one is reference-equal to it, so a fake that mutates a row in
    place and hands the same array back reports no change at all, and the page
    under test renders stale text that the store already disagrees with. A real
    daemon parses new objects out of JSON every time; so does this.
  */
  return async (path, init) => {
    if (path === "/teams") return (opts.teams ?? []).map((row) => ({ ...row }));

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
    if (path === "/agents") return rows.map((row) => ({ ...row }));

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

/** Open the create panel, which is closed until somebody asks for it. */
async function openCreate(): Promise<HTMLElement> {
  fireEvent.click(await screen.findByRole("button", { name: "New agent" }));
  return (await screen.findByRole("heading", { level: 2, name: "New agent" })).closest("section") as HTMLElement;
}

/** Select a row, which is what opens the one editor on the page. */
async function openEditor(name: string): Promise<HTMLElement> {
  fireEvent.click(await screen.findByRole("button", { name }));
  return (await screen.findByRole("heading", { level: 2, name: `Editing ${name}` })).closest(
    "section",
  ) as HTMLElement;
}

/* ------------------------------------------------------------ the shape -- */

describe("Agents - the shape of the page", () => {
  it("does not open a form over a catalogue nobody has read yet", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent()]));
    renderWithQuery(<Agents />);

    await screen.findByRole("button", { name: "copywriter" });
    expect(screen.queryByLabelText("Name")).toBeNull();
    expect(screen.queryByRole("heading", { level: 2, name: "New agent" })).toBeNull();

    const panel = await openCreate();
    expect(within(panel).getByLabelText("Name")).toBeDefined();
  });

  it("draws the catalogue as one table, so every row starts at the same height", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "one", name: "one" }), agent({ id: "two", name: "two" })]),
    );
    renderWithQuery(<Agents />);

    const table = await screen.findByRole("table");
    // Header row plus one per agent, and no second table pretending to be one.
    expect(within(table).getAllByRole("row")).toHaveLength(3);
    expect(screen.getAllByRole("table")).toHaveLength(1);
  });

  it("keeps one editor, below the table, and swaps it rather than stacking a second", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "one", name: "one" }), agent({ id: "two", name: "two", speciality: "second" })]),
    );
    renderWithQuery(<Agents />);

    await openEditor("one");
    expect(screen.getAllByLabelText("Prompt")).toHaveLength(1);

    await openEditor("two");
    expect(screen.getAllByLabelText("Prompt")).toHaveLength(1);
    expect(screen.queryByRole("heading", { level: 2, name: "Editing one" })).toBeNull();
    // The draft reseeded from the newly chosen agent rather than carrying over.
    expect((screen.getByLabelText("Speciality") as HTMLInputElement).value).toBe("second");
  });
});

/* -------------------------------------------------------------- the id -- */

describe("Agents - the name and the id", () => {
  it("draws the id on every row, because that is what the rest of the app points at", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "head-of-content", name: "head of content" })]),
    );
    renderWithQuery(<Agents />);

    expect(await screen.findByText("head-of-content")).toBeDefined();
  });

  it("says what the id IS, rather than leaving the same word twice with no reason", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent({ id: "tradutor", name: "tradutor" })]));
    renderWithQuery(<Agents />);

    const cell = (await screen.findByRole("button", { name: "tradutor" })).closest("th") as HTMLElement;
    // Everything separating the name from the id on screen is spatial, and none
    // of it is read out. Without this the row is the same word, twice.
    expect(cell.textContent).toContain("known to the núcleo as tradutor");
  });

  it("marks the id once a rename has left it behind, and says what still points at it", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent({ id: "auditor", name: "Auditor Sénior" })]));
    renderWithQuery(<Agents />);

    const id = await screen.findByText(/auditor/, { selector: ".agents-id-diverged" });
    expect(id.textContent).toContain("auditor");
    expect(screen.getByText(/rosters, team items, job items and council seats/i)).toBeDefined();
  });

  it("leaves the id unmarked while it is still the slug of the name", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "head-of-content", name: "Head of Content" })]),
    );
    renderWithQuery(<Agents />);

    await screen.findByText("head-of-content");
    expect(document.querySelector(".agents-id-diverged")).toBeNull();
    expect(screen.queryByText(/renamed since/i)).toBeNull();
  });
});

/* --------------------------------------------------- engine, model, tools -- */

describe("Agents - what an agent can do", () => {
  it("reads a null model as the engine's default and says what naming none costs", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent({ model: null })]));
    renderWithQuery(<Agents />);

    expect(await screen.findByText("the engine’s default")).toBeDefined();
    expect(screen.getByText(/can never take a council seat/i)).toBeDefined();
  });

  it("says nothing about council seats for an agent that names a model", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent({ model: "gpt-5-codex", engine: "codex" })]));
    renderWithQuery(<Agents />);

    expect(await screen.findByText("gpt-5-codex")).toBeDefined();
    expect(screen.queryByText(/council seat/i)).toBeNull();
  });

  it("draws a tool policy as a capability and never as a state badge", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([
        agent({ id: "armed", name: "armed", tool_policy: "mcp_only" }),
        agent({ id: "bare", name: "bare", tool_policy: "none" }),
      ]),
    );
    renderWithQuery(<Agents />);

    await screen.findByRole("button", { name: "armed" });
    expect(screen.getByText(/mcp_only: has tools/)).toBeDefined();
    expect(screen.getByText(/none: no tools at all/)).toBeDefined();
    // The seven tones are the núcleo's state vocabulary; nothing here is a state.
    expect(document.querySelector(".ui-badge")).toBeNull();
  });

  it("says so rather than guessing when the policy is one this shell has never heard of", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent({ tool_policy: "read_only" })]));
    renderWithQuery(<Agents />);

    expect(await screen.findByText(/read_only: this shell has no reading for that policy/)).toBeDefined();
  });

  it("renders no provenance, because nothing in the núcleo stores it", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent()]));
    renderWithQuery(<Agents />);

    await screen.findByRole("button", { name: "copywriter" });
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
    renderWithQuery(<Agents />);

    expect(await screen.findByText(/on the roster of Finanças, Marketing/)).toBeDefined();
    // The matrix answers "where" and is not redrawn here.
    expect(screen.queryByText("Who works where")).toBeNull();
  });

  it("counts directing apart from being on a roster", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "controller", name: "controller" })], { teams: departments }),
    );
    renderWithQuery(<Agents />);

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
    renderWithQuery(<Agents />);

    const cell = (await screen.findByRole("button", { name: "sysadmin" }))
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
    renderWithQuery(<Agents />);

    expect(await screen.findByText(/directs Segurança/)).toBeDefined();
    expect(screen.getByText(/on the roster of Informática/)).toBeDefined();
  });

  it("marks an agent no department names, and says so in the headline", async () => {
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "spare", name: "spare", model: "sonnet" })], { teams: departments }),
    );
    renderWithQuery(<Agents />);

    expect(await screen.findByText("no department names this one")).toBeDefined();
    expect(screen.getByText(/1 nobody uses/)).toBeDefined();
  });

  it("never says nobody uses an agent while the team list has not answered", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/teams") return await new Promise(() => {});
      if (path === "/agents") return [agent({ id: "spare", name: "spare", model: "sonnet" })];
      return undefined;
    });
    renderWithQuery(<Agents />);

    expect(await screen.findByText(/the team list has not answered/)).toBeDefined();
    expect(screen.queryByText("no department names this one")).toBeNull();
    expect(screen.queryByText(/nobody uses/)).toBeNull();
  });
});

/* --------------------------------------------------------------- create -- */

describe("Agents - the policy control and the local-engine model requirement", () => {
  it("offers mcp_only and none and never unrestricted", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([]));
    renderWithQuery(<Agents />);

    const panel = await openCreate();
    const policySelect = within(panel).getByLabelText("Tool policy");
    const optionLabels = within(policySelect as HTMLSelectElement)
      .getAllByRole("option")
      .map((option) => option.textContent);

    expect(optionLabels).toEqual(["mcp_only", "none"]);
    expect(optionLabels).not.toContain("unrestricted");
  });

  it("refuses submit when engine is local and no model is named", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([]));
    renderWithQuery(<Agents />);

    const panel = await openCreate();
    fireEvent.change(within(panel).getByLabelText("Name"), { target: { value: "analyst" } });
    fireEvent.change(within(panel).getByLabelText("Speciality"), {
      target: { value: "reads spreadsheets" },
    });
    const submit = within(panel).getByRole("button", { name: "Add agent" }) as HTMLButtonElement;
    expect(submit.disabled).toBe(false);

    fireEvent.change(within(panel).getByLabelText("Engine"), { target: { value: "local" } });
    expect(submit.disabled).toBe(true);

    fireEvent.change(within(panel).getByLabelText("Model"), { target: { value: "llama-local" } });
    expect(submit.disabled).toBe(false);
  });
});

/* --------------------------------------------------------------- editor -- */

describe("Agents - editing a row", () => {
  it("sends a PUT carrying every field including the unchanged ones, and the table shows the returned agent", async () => {
    const rows = [agent({ id: "copywriter", name: "copywriter", speciality: "writes short copy" })];
    daemon.apiFetch.mockImplementation(agentsFetch(rows));
    renderWithQuery(<Agents />);

    const editor = await openEditor("copywriter");
    fireEvent.change(within(editor).getByLabelText("Speciality"), {
      target: { value: "writes long-form copy" },
    });
    fireEvent.click(within(editor).getByRole("button", { name: "Save changes" }));

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

  it("says that saving reaches work already queued, because a prompt is read at dispatch", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent()]));
    renderWithQuery(<Agents />);

    const editor = await openEditor("copywriter");
    expect(within(editor).getByText(/reaches work that is already queued/i)).toBeDefined();
  });

  it("shows the prompt in the editor rather than behind a per-row disclosure", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent()]));
    renderWithQuery(<Agents />);

    await screen.findByRole("button", { name: "copywriter" });
    expect(document.querySelector("details")).toBeNull();

    const editor = await openEditor("copywriter");
    expect((within(editor).getByLabelText("Prompt") as HTMLTextAreaElement).value).toBe(
      "You write short, punchy copy.",
    );
  });
});

/* ------------------------------------------------------------- refusals -- */

describe("Agents - what stands on an agent", () => {
  it("names what it can see before the delete is armed, and admits what it cannot", async () => {
    const departments = [team({ name: "Finanças", director_agent_id: "controller" })];
    daemon.apiFetch.mockImplementation(
      agentsFetch([agent({ id: "controller", name: "controller" })], { teams: departments }),
    );
    renderWithQuery(<Agents />);

    const editor = await openEditor("controller");
    expect(within(editor).getByText(/directs Finanças/)).toBeDefined();
    expect(within(editor).getByText(/an item of a team run, or of a job/)).toBeDefined();
  });

  it("does not let an empty roster read as a clean delete", async () => {
    daemon.apiFetch.mockImplementation(agentsFetch([agent()], { teams: [] }));
    renderWithQuery(<Agents />);

    const editor = await openEditor("copywriter");
    expect(within(editor).getByText(/No department names copywriter/)).toBeDefined();
    expect(within(editor).getByText(/is not something this page can see/)).toBeDefined();
  });
});

describe("Agents - a delete answering 409", () => {
  it("names all four grounds the núcleo refuses on, not only the two about teams", async () => {
    const rows = [agent({ id: "director", name: "director" })];
    daemon.apiFetch.mockImplementation(
      agentsFetch(rows, {
        onDelete: () => {
          throw new ApiRefusal(409, "conflict", "");
        },
      }),
    );
    renderWithQuery(<Agents />);

    const editor = await openEditor("director");
    fireEvent.click(within(editor).getByRole("button", { name: "Delete" }));
    await afterDwell();
    fireEvent.click(within(editor).getByRole("button", { name: /Delete director/ }));

    expect(
      await within(editor).findByText(
        "this agent directs or belongs to a team, or is holding an item of a team run or of a job",
      ),
    ).toBeDefined();
    // The old sentence sent anybody who had already emptied every roster
    // looking for a team that does not exist.
    expect(screen.queryByText("a team is standing on this agent")).toBeNull();
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
    renderWithQuery(<Agents />);

    expect(await screen.findByRole("heading", { name: "No agent has been hired yet" })).toBeDefined();
    expect(screen.getByText(/a name, a speciality/i)).toBeDefined();
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
