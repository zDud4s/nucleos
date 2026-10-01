import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { Learned } from "./Learned";
import type { Known, KnowledgeHistory } from "../data/knowledge";
import { ApiRefusal } from "../data/client";
import { renderWithRouter } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

function known(over: Partial<Known> = {}): Known {
  return {
    id: 1,
    layer: "semantic",
    scope_kind: "project",
    scope_id: "nucleos",
    source: "run",
    generator: null,
    kind: "memory",
    title: "The suite needs Git's usr/bin on PATH",
    body: "Nine tests spawn echo as a program.",
    status: "active",
    proposal_id: 7,
    supersedes: null,
    origin_run_id: 900001,
    evidence: null,
    observations: null,
    fingerprint: null,
    expires_after_runs: null,
    last_confirmed_at: null,
    shown_count: 0,
    outcome_count: 0,
    green_count: 0,
    last_shown_at: null,
    created_at: "2026-08-19T09:00:00+00:00",
    activated_at: "2026-08-19T09:05:00+00:00",
    ended_at: null,
    ...over,
  };
}

/** A daemon holding exactly this much, and answering the detail route from it. */
function daemonWith(rows: Known[], history?: Partial<KnowledgeHistory>) {
  return (path: string) => {
    if (path === "/knowledge") return Promise.resolve(rows);
    if (path.startsWith("/knowledge/")) {
      const id = Number(path.split("/")[2]);
      return Promise.resolve({
        known: rows.find((row) => row.id === id) ?? rows[0],
        events: [],
        replaced: [],
        replaced_by: null,
        ...history,
      });
    }
    return Promise.resolve(undefined);
  };
}

/** The `<section>` a panel's own heading belongs to, so an assertion can be scoped to one. */
async function panelFor(headingText: string | RegExp): Promise<HTMLElement> {
  const heading = await screen.findByRole("heading", { level: 2, name: headingText });
  const panel = heading.closest("section");
  if (panel === null) throw new Error(`no panel section found for heading "${String(headingText)}"`);
  return panel as HTMLElement;
}

describe("Learned", () => {
  it("four kinds wear one tone, because a kind is not a state", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, kind: "prompt" }),
        known({ id: 2, kind: "memory" }),
        known({ id: 3, kind: "skill" }),
        known({ id: 4, kind: "subagent" }),
      ]),
    );

    await renderWithRouter(<Learned />);

    for (const [kind, word] of [["prompt", "instruction"], ["memory", "fact"], ["skill", "how-to"], ["subagent", "delegation"]] as const) {
      const badge = await screen.findByText(word);
      expect(badge.className, kind).toContain("ui-badge-info");
    }
    expect(screen.queryByText("instruction")?.className).not.toContain("ui-badge-shadow");
  });

  it("keeps what is waiting apart from what is in force", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, status: "active", title: "already in force" }),
        known({ id: 2, status: "proposed", title: "still a question", proposal_id: 9 }),
        known({ id: 3, status: "reverted", title: "taken back" }),
      ]),
    );

    await renderWithRouter(<Learned />);

    // The distinction the whole layer rests on: a proposed row is NOT reaching
    // any prompt, and a screen that mixed the two would make the approval look
    // like paperwork over something already happening.
    const waiting = await panelFor("Waiting for you");
    expect(within(waiting).getByText("still a question")).toBeDefined();
    expect(within(waiting).queryByText("already in force")).toBeNull();

    const force = await panelFor("In force");
    expect(within(force).getByText("already in force")).toBeDefined();
    expect(within(force).queryByText("taken back")).toBeNull();

    expect(within(await panelFor("No longer in force")).getByText("taken back")).toBeDefined();
  });

  it("the waiting queue is grouped by scope and source with a count per group", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 4, status: "proposed", scope_id: "beta", source: "run", title: "beta run" }),
        known({ id: 1, status: "proposed", scope_id: "alpha", source: "owner", title: "alpha owner one" }),
        known({ id: 3, status: "proposed", scope_id: "alpha", source: "run", title: "alpha run" }),
        known({ id: 2, status: "proposed", scope_id: "alpha", source: "owner", title: "alpha owner two" }),
      ]),
    );

    await renderWithRouter(<Learned />);
    const panel = await panelFor("Waiting for you");
    const groups = panel.querySelectorAll(".learned-group");

    expect(groups).toHaveLength(3);
    expect(groups[0].textContent).toContain("alpha");
    expect(groups[0].textContent).toContain("owner");
    expect(groups[0].textContent).toContain("2");
    expect(groups[0].textContent).toContain("alpha owner one");
    expect(groups[0].textContent).toContain("alpha owner two");
    expect(groups[1].textContent).toContain("alpha run");
    expect(groups[2].textContent).toContain("beta run");
  });

  it("approve all sends one approve per proposal in ascending order, one at a time", async () => {
    const rows = [
      known({ id: 3, status: "proposed", proposal_id: 30, title: "third" }),
      known({ id: 1, status: "proposed", proposal_id: 10, title: "first" }),
      known({ id: 2, status: "proposed", proposal_id: 20, title: "second" }),
    ];
    const read = daemonWith(rows);
    const releases: Array<() => void> = [];
    daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) => {
      if (init?.method === "POST") {
        return new Promise((resolve) => releases.push(() => resolve({ refinement_id: 1 })));
      }
      return read(path);
    });

    await renderWithRouter(<Learned />);
    fireEvent.click(await screen.findByRole("button", { name: "Approve all 3" }));
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(screen.getByRole("button", { name: "Let all 3 into every later prompt" }));

    await waitFor(() => {
      const posts = daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST");
      expect(posts.map(([path]) => path)).toEqual(["/proposals/10/approve"]);
    });
    releases[0]();
    await waitFor(() => {
      const posts = daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST");
      expect(posts.map(([path]) => path)).toEqual([
        "/proposals/10/approve",
        "/proposals/20/approve",
      ]);
    });
    releases[1]();
    await waitFor(() => {
      const posts = daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST");
      expect(posts.map(([path]) => path)).toEqual([
        "/proposals/10/approve",
        "/proposals/20/approve",
        "/proposals/30/approve",
      ]);
    });
    releases[2]();
    await waitFor(() =>
      expect(screen.getAllByRole("button", { name: "Approve" })[0].hasAttribute("disabled")).toBe(
        false,
      ),
    );
  });

  it("approve all asks for a second click first", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, status: "proposed", proposal_id: 10 }),
        known({ id: 2, status: "proposed", proposal_id: 20 }),
      ]),
    );

    await renderWithRouter(<Learned />);
    fireEvent.click(await screen.findByRole("button", { name: "Approve all 2" }));

    expect(daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST")).toEqual([]);
    expect(
      screen.getByRole("button", { name: "Let all 2 into every later prompt" }),
    ).toBeDefined();
  });

  it("refuse all sends one reject per proposal", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 2, status: "proposed", proposal_id: 22 }),
        known({ id: 1, status: "proposed", proposal_id: 11 }),
      ]),
    );

    await renderWithRouter(<Learned />);
    fireEvent.click(await screen.findByRole("button", { name: "Refuse all 2" }));

    await waitFor(() => {
      const posts = daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST");
      expect(posts.map(([path]) => path)).toEqual([
        "/proposals/11/reject",
        "/proposals/22/reject",
      ]);
    });
  });

  it("a batch that partly fails says how many were decided and keeps going", async () => {
    const rows = [
      known({ id: 1, status: "proposed", proposal_id: 11 }),
      known({ id: 2, status: "proposed", proposal_id: 22 }),
      known({ id: 3, status: "proposed", proposal_id: 33 }),
    ];
    const read = daemonWith(rows);
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (init?.method !== "POST") return read(path);
      if (path === "/proposals/22/reject") throw new ApiRefusal(409, "conflict", "");
      return undefined;
    });

    await renderWithRouter(<Learned />);
    fireEvent.click(await screen.findByRole("button", { name: "Refuse all 3" }));

    expect(await screen.findByText(/2 decided; 1 could not be/)).toBeDefined();
    const posts = daemon.apiFetch.mock.calls.filter(([, init]) => init?.method === "POST");
    expect(posts.map(([path]) => path)).toEqual([
      "/proposals/11/reject",
      "/proposals/22/reject",
      "/proposals/33/reject",
    ]);
  });

  it("a group of one has no batch buttons", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([known({ id: 1, status: "proposed", proposal_id: 11 })]),
    );

    await renderWithRouter(<Learned />);
    await screen.findByRole("button", { name: "Approve" });

    expect(screen.queryByRole("button", { name: /Approve all/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Refuse all/ })).toBeNull();
  });

  it("sends the two answers to the proposal and the revert to the row", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, status: "active", title: "in force", proposal_id: 7 }),
        known({ id: 2, status: "proposed", title: "a question", proposal_id: 9 }),
      ]),
    );

    await renderWithRouter(<Learned />);
    await screen.findByText("a question");

    // Yes and no go through the proposal doors — the núcleo dispatches on the
    // proposal's kind — while a revert is aimed at the row itself, because the
    // question was answered long ago and what changes now is the layer.
    // `waitFor` and not a bare assertion: react-query defers the mutation, so
    // the request has not been made in the tick the click returns in.
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/proposals/9/approve", { method: "POST" }),
    );

    fireEvent.click(screen.getByRole("button", { name: "Refuse" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/proposals/9/reject", { method: "POST" }),
    );

    fireEvent.click(screen.getByRole("button", { name: "Revert" }));
    // `ConfirmButton` swallows a second click inside its 300ms dwell — that
    // click is the tail of a double-click, not a decision. Waiting past it is
    // what makes this exercise the interlock rather than defeat it.
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(screen.getByRole("button", { name: /no longer applies/i }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/knowledge/1/revert", {
        method: "POST",
        body: "{}",
      }),
    );
  });

  it("does not ask for a chain until somebody opens one", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith(
        [known({ id: 4, status: "active", title: "current text", supersedes: 3 })],
        {
          replaced: [known({ id: 3, status: "superseded", title: "what it said before" })],
        },
      ),
    );

    await renderWithRouter(<Learned />);
    await screen.findByText("current text");

    // One request per row would make reading a list of forty cost forty-one
    // calls to answer a question nobody has asked yet.
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/knowledge/4", expect.anything());
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/knowledge/4");

    fireEvent.click(screen.getByRole("button", { name: /What it replaced/ }));

    expect(await screen.findByText("what it said before")).toBeDefined();
  });

  it("says the layer is empty rather than drawing three empty headings", async () => {
    daemon.apiFetch.mockImplementation(daemonWith([]));

    await renderWithRouter(<Learned />);

    // An empty layer is the normal state of a fresh machine, and a page that
    // met it with three blank panels would read as broken rather than as new.
    expect(await screen.findByText(/nothing has been learned yet/i)).toBeDefined();
    expect(screen.queryByRole("heading", { level: 2, name: "In force" })).toBeNull();
  });

  it("the headline names what is waiting", async () => {
    // "waiting on you" is the one queue's phrase, and this page counts one kind
    // of thing: a note somebody proposed. Naming it is what lets a reader
    // hold Home's number and this one at the same time without adding them up.
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, status: "active", title: "in force" }),
        known({ id: 2, status: "proposed", title: "one", proposal_id: 9 }),
        known({ id: 3, status: "proposed", title: "two", proposal_id: 10 }),
        known({ id: 4, status: "proposed", title: "three", proposal_id: 11 }),
      ]),
    );

    await renderWithRouter(<Learned />);

    expect(
      (await screen.findByText(/notes proposed/)).textContent,
    ).toBe("one note is in force; 3 notes proposed");
  });

  it("says nothing proposed when everything is settled", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, status: "active", title: "in force" }),
        known({ id: 2, status: "reverted", title: "taken back" }),
      ]),
    );

    await renderWithRouter(<Learned />);

    expect((await screen.findByText(/nothing proposed/)).textContent).toBe(
      "one note is in force; nothing proposed",
    );
  });

  it("shows the four layers, each row naming its layer and its source", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, layer: "semantic", status: "proposed", source: "owner", title: "a fact" }),
        known({ id: 2, layer: "episodic", status: "active", source: "consolidator", title: "a measurement" }),
        known({ id: 3, layer: "working", status: "live", source: "run", title: "job context" }),
        known({ id: 4, layer: "procedural", status: "rejected", source: "owner", title: "a method" }),
      ]),
    );

    await renderWithRouter(<Learned />);

    for (const [title, layer, source] of [
      ["a fact", "semantic", "owner"],
      ["a measurement", "episodic", "consolidator"],
      ["job context", "working", "run"],
      ["a method", "procedural", "owner"],
    ] as const) {
      const row = (await screen.findByText(title)).closest(".learned-row");
      expect(row).not.toBeNull();
      expect(within(row as HTMLElement).getByText(layer)).toBeDefined();
      expect(within(row as HTMLElement).getByText(source)).toBeDefined();
    }
  });

  it("a measured row says how many times it was measured", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([known({ layer: "episodic", source: "consolidator", observations: 3 })]),
    );

    await renderWithRouter(<Learned />);

    expect(await screen.findByText("measured 3 times")).toBeDefined();
  });

  it("the layer filter narrows every list and the headline", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, layer: "semantic", status: "active", title: "semantic active" }),
        known({ id: 2, layer: "episodic", status: "active", source: "consolidator", title: "measured active" }),
        known({ id: 3, layer: "episodic", status: "proposed", title: "measured proposal" }),
      ]),
    );

    await renderWithRouter(<Learned />);
    fireEvent.click(await screen.findByRole("button", { name: "Measured" }));

    expect(screen.queryByText("semantic active")).toBeNull();
    expect(screen.getByText("measured active")).toBeDefined();
    expect(screen.getByText("measured proposal")).toBeDefined();
    expect(screen.getByText("one note is in force; 1 note proposed; 1 measured")).toBeDefined();
  });

  it("the scope filter offers every scope even after one is chosen", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, scope_id: "alpha", title: "alpha fact" }),
        known({ id: 2, scope_id: "beta", title: "beta fact" }),
        known({ id: 3, scope_kind: "machine", scope_id: null, title: "machine fact" }),
      ]),
    );

    await renderWithRouter(<Learned />);
    const select = await screen.findByRole("combobox", { name: "Scope" });
    fireEvent.change(select, { target: { value: "alpha" } });

    expect(screen.getByText("alpha fact")).toBeDefined();
    expect(screen.queryByText("beta fact")).toBeNull();
    expect(within(select).getByRole("option", { name: "beta" })).toBeDefined();
    expect(within(select).getByRole("option", { name: "this machine" })).toBeDefined();
  });

  it("a working fact appears under Live in a job and nowhere else", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, status: "proposed", title: "question" }),
        known({ id: 2, status: "active", title: "fact" }),
        known({ id: 3, layer: "working", status: "live", title: "only this job" }),
        known({ id: 4, status: "closed", title: "finished" }),
      ]),
    );

    await renderWithRouter(<Learned />);

    expect(within(await panelFor("Live in a job")).getByText("only this job")).toBeDefined();
    for (const panel of ["Waiting for you", "In force", "No longer in force"]) {
      expect(within(await panelFor(panel)).queryByText("only this job")).toBeNull();
    }
  });

  it("counts measured rows per project and per generator", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({ id: 1, layer: "episodic", source: "consolidator", scope_id: "alpha", generator: "gate" }),
        known({ id: 2, layer: "episodic", source: "consolidator", scope_id: "alpha", generator: "gate" }),
        known({ id: 3, layer: "episodic", source: "consolidator", scope_id: "alpha", generator: "refused-action" }),
        known({ id: 4, layer: "episodic", source: "consolidator", scope_kind: "machine", scope_id: null, generator: null }),
      ]),
    );

    await renderWithRouter(<Learned />);
    const panel = await panelFor("Measured, by generator");
    const project = within(panel).getByText("alpha").closest(".learned-measured-row");
    const machine = within(panel).getByText("this machine").closest(".learned-measured-row");

    expect(project).not.toBeNull();
    expect(within(project as HTMLElement).getByText("gate: 2")).toBeDefined();
    expect(within(project as HTMLElement).getByText("refused-action: 1")).toBeDefined();
    expect(machine).not.toBeNull();
    expect(within(machine as HTMLElement).getByText("unknown: 1")).toBeDefined();
  });

  it("evidence for a run is a link to that run and an unknown tag draws nothing", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        known({
          id: 1,
          evidence: JSON.stringify([
            { t: "run", id: 900449 },
            { t: "unknown", id: 2 },
          ]),
        }),
      ]),
    );

    await renderWithRouter(<Learned />);
    const link = await screen.findByRole("link", { name: "run 900449" });

    expect(link.getAttribute("href")).toContain("/runs/900449");
    expect(screen.queryByText("unknown 2")).toBeNull();
  });

  it("knowledge evidence opens that row's history", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith(
        [
          known({ id: 1, evidence: JSON.stringify([{ t: "knowledge", id: 2 }]) }),
          known({ id: 2, title: "the evidence row" }),
        ],
        { replaced: [known({ id: 8, title: "older evidence" })] },
      ),
    );

    await renderWithRouter(<Learned />);
    fireEvent.click(await screen.findByRole("button", { name: "knowledge 2" }));

    expect(await screen.findByText("older evidence")).toBeDefined();
    expect(daemon.apiFetch).toHaveBeenCalledWith("/knowledge/2");
  });
});
