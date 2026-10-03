import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import type { Node } from "@xyflow/react";
import { BrainPanel } from "./BrainPanel";
import type { NoteDetail, NotesGraph } from "../data/owner-notes";
import { renderWithRouter } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiRefusal } from "../data/client";

const NOTE = {
  id: 4,
  text: "Rust owns the state",
  origin: "shell" as const,
  state: "active" as const,
  created_at: "2026-09-01T09:00:00+00:00",
  updated_at: "2026-09-01T09:00:00+00:00",
};

function detail(over: Partial<NoteDetail> = {}): NoteDetail {
  return { note: NOTE, links_out: [], links_in: [], events: [], ...over };
}

const graph: NotesGraph = { notes: [NOTE], links: [], targets: [] };
const nodes: Node[] = [{ id: "n:4", type: "note", position: { x: 0, y: 0 }, data: { note: NOTE } }];

function serve(d: NoteDetail, other?: (path: string, init?: RequestInit) => unknown) {
  daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) => {
    if (path === "/owner-notes/4" && init === undefined) return Promise.resolve(d);
    const answer = other?.(path, init);
    if (answer !== undefined) return answer;
    return Promise.reject(new Error(`unexpected ${path}`));
  });
}

async function panel(g: NotesGraph = graph) {
  await renderWithRouter(<BrainPanel nodeId="n:4" graph={g} nodes={nodes} edges={[]} />, {
    initialPath: "/brain",
  });
  await screen.findByText("Rust owns the state");
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("BrainPanel", () => {
  it("adding a link posts type kind and ref", async () => {
    serve(detail(), (path, init) =>
      path === "/owner-notes/4/links" && init?.method === "POST" ? Promise.resolve({ id: 1 }) : undefined,
    );
    await panel();

    fireEvent.change(screen.getByLabelText("Link type"), { target: { value: "supports" } });
    fireEvent.change(screen.getByLabelText("Target kind"), { target: { value: "project" } });
    fireEvent.change(screen.getByLabelText("Target ref"), { target: { value: "7" } });
    fireEvent.click(screen.getByRole("button", { name: "Add link" }));

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith(
        "/owner-notes/4/links",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ link_type: "supports", target_kind: "project", target_ref: "7" }),
        }),
      ),
    );
  });

  it("the history lists edits oldest first", async () => {
    serve(
      detail({
        events: [
          { id: 2, note_id: 4, kind: "archived", detail: null, at: "2026-09-02T09:00:00+00:00" },
          { id: 1, note_id: 4, kind: "created", detail: null, at: "2026-09-01T09:00:00+00:00" },
        ],
      }),
    );
    await panel();
    const items = (await screen.findByRole("list", { name: "History" })).querySelectorAll("li");
    expect(items[0].textContent).toContain("created");
    expect(items[1].textContent).toContain("archived");
  });

  it("a missing target reads as gone", async () => {
    serve(
      detail({
        links_out: [
          {
            id: 9,
            note_id: 4,
            link_type: "relates",
            target_kind: "project",
            target_ref: "12",
            created_at: "2026-09-01T09:00:00+00:00",
          },
        ],
      }),
    );
    await panel({
      ...graph,
      targets: [{ kind: "project", ref: "12", label: "old repo", missing: true }],
    });
    const list = await screen.findByRole("list", { name: "Links out" });
    expect(list.textContent).toContain("old repo");
    expect(list.textContent).toContain("gone");
  });

  it("teaching a note posts the chosen kind and points to Learned", async () => {
    serve(detail(), (path, init) =>
      path === "/owner-notes/4/teach" && init?.method === "POST"
        ? Promise.resolve({ knowledge_id: 1, proposal_id: 2, link_id: 3 })
        : undefined,
    );
    await panel();

    fireEvent.change(screen.getByLabelText("Lesson kind"), { target: { value: "skill" } });
    fireEvent.click(screen.getByRole("button", { name: "Teach the agent" }));

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith(
        "/owner-notes/4/teach",
        expect.objectContaining({ method: "POST", body: JSON.stringify({ kind: "skill" }) }),
      ),
    );
    expect(await screen.findByText(/Proposed — waiting for your approval in/)).toBeTruthy();
    expect(screen.getByRole("link", { name: "Learned" }).getAttribute("href")).toBe("/learned");
  });

  it("an already-taught refusal is shown", async () => {
    serve(detail(), (path, init) =>
      path === "/owner-notes/4/teach" && init?.method === "POST"
        ? Promise.reject(new ApiRefusal(409, "already_taught", ""))
        : undefined,
    );
    await panel();

    fireEvent.click(screen.getByRole("button", { name: "Teach the agent" }));
    expect(await screen.findByText(/already taught/)).toBeTruthy();
  });
});
