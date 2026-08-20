import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { Learned } from "./Learned";
import type { Refinement, RefinementHistory } from "../data/refinements";
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

function refinement(over: Partial<Refinement> = {}): Refinement {
  return {
    id: 1,
    project_id: "nucleos",
    kind: "memory",
    title: "The suite needs Git's usr/bin on PATH",
    body: "Nine tests spawn echo as a program.",
    status: "active",
    proposal_id: 7,
    supersedes: null,
    origin_run_id: 900001,
    created_at: "2026-08-19T09:00:00+00:00",
    activated_at: "2026-08-19T09:05:00+00:00",
    ended_at: null,
    ...over,
  };
}

/** A daemon holding exactly these refinements, and answering the detail route from them. */
function daemonWith(rows: Refinement[], history?: Partial<RefinementHistory>) {
  return (path: string) => {
    if (path === "/refinements") return Promise.resolve(rows);
    if (path.startsWith("/refinements/")) {
      const id = Number(path.split("/")[2]);
      return Promise.resolve({
        refinement: rows.find((row) => row.id === id) ?? rows[0],
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
  it("keeps what is waiting apart from what is in force", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        refinement({ id: 1, status: "active", title: "already in force" }),
        refinement({ id: 2, status: "proposed", title: "still a question", proposal_id: 9 }),
        refinement({ id: 3, status: "reverted", title: "taken back" }),
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

  it("sends the two answers to the proposal and the revert to the refinement", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith([
        refinement({ id: 1, status: "active", title: "in force", proposal_id: 7 }),
        refinement({ id: 2, status: "proposed", title: "a question", proposal_id: 9 }),
      ]),
    );

    await renderWithRouter(<Learned />);
    await screen.findByText("a question");

    // Yes and no go through the proposal doors — the núcleo dispatches on the
    // proposal's kind — while a revert is aimed at the refinement, because the
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
      expect(daemon.apiFetch).toHaveBeenCalledWith("/refinements/1/revert", {
        method: "POST",
        body: "{}",
      }),
    );
  });

  it("does not ask for a chain until somebody opens one", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith(
        [refinement({ id: 4, status: "active", title: "current text", supersedes: 3 })],
        {
          replaced: [refinement({ id: 3, status: "superseded", title: "what it said before" })],
        },
      ),
    );

    await renderWithRouter(<Learned />);
    await screen.findByText("current text");

    // One request per row would make reading a list of forty cost forty-one
    // calls to answer a question nobody has asked yet.
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/refinements/4", expect.anything());
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/refinements/4");

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
});
