import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { BrainPanel } from "./BrainPanel";
import type { GModel } from "./graph-types";
import type { NoteDetail, NotesGraph } from "../data/owner-notes";
import { renderWithRouter } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

// The canvas is its own suite's business: here it lists the node ids it was handed.
vi.mock("./ForceGraph", () => ({
  ForceGraph: ({ model, onSelect }: { model: GModel; onSelect: (id: string) => void }) => (
    <ul aria-label="Local graph nodes">
      {model.nodes.map((node) => (
        <li key={node.id}>
          <button type="button" onClick={() => onSelect(node.id)}>
            {node.id}
          </button>
        </li>
      ))}
    </ul>
  ),
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
const model: GModel = {
  nodes: [{ id: "n:4", kind: "note", ref: "4", label: NOTE.text, bucket: "in_force", missing: false, degree: 0 }],
  edges: [],
};

/** What `/owner-notes/graph?include_archived=true` answers; `panel` sets it. */
let wide: NotesGraph = graph;

function serve(d: NoteDetail, other?: (path: string, init?: RequestInit) => unknown) {
  daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) => {
    if (path === "/owner-notes/4" && init === undefined) return Promise.resolve(d);
    if (path === "/owner-notes/graph?include_archived=true") return Promise.resolve(wide);
    if (path === "/knowledge" || path === "/projects") return Promise.resolve([]);
    const answer = other?.(path, init);
    if (answer !== undefined) return answer;
    return Promise.reject(new Error(`unexpected ${path}`));
  });
}

async function panel(g: NotesGraph = graph, onSelect?: (id: string) => void) {
  wide = g;
  await renderWithRouter(<BrainPanel nodeId="n:4" graph={g} model={model} onSelect={onSelect} />, {
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

  it("removing a link takes two presses, and only the second deletes", async () => {
    const deleted: string[] = [];
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
      (path, init) => {
        if (init?.method !== "DELETE") return undefined;
        deleted.push(path);
        return Promise.resolve(undefined);
      },
    );
    await panel({ ...graph, targets: [{ kind: "project", ref: "12", label: "repo", missing: false }] });

    fireEvent.click(screen.getByRole("button", { name: /^Remove/ }));
    expect(deleted).toHaveLength(0);
    const armed = await screen.findByRole("button", { name: /Remove link/ });
    // A real gap: clicks inside the interlock's 300ms dwell are ignored.
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(armed);

    await waitFor(() => expect(deleted).toEqual(["/owner-notes/links/9"]));
  });

  it("the local graph shows the note's neighbours, and a neighbour click selects it", async () => {
    const neighbour = { ...NOTE, id: 7, text: "A neighbour" };
    const far = { ...NOTE, id: 8, text: "Two hops away" };
    const linked: NotesGraph = {
      notes: [NOTE, neighbour, far],
      links: [
        { id: 1, note_id: 4, link_type: "relates", target_kind: "note", target_ref: "7", created_at: NOTE.created_at },
        { id: 2, note_id: 7, link_type: "supports", target_kind: "note", target_ref: "8", created_at: NOTE.created_at },
      ],
      targets: [],
    };
    const picked: string[] = [];
    serve(detail());
    await panel(linked, (id) => picked.push(id));

    const local = await screen.findByRole("list", { name: "Local graph nodes" });
    expect(local.textContent).toContain("n:4");
    expect(local.textContent).toContain("n:7");
    expect(local.textContent).not.toContain("n:8");

    fireEvent.click(screen.getByRole("button", { name: "Depth 2" }));
    await waitFor(() =>
      expect(screen.getByRole("list", { name: "Local graph nodes" }).textContent).toContain("n:8"),
    );

    fireEvent.click(screen.getByRole("button", { name: "n:7" }));
    fireEvent.click(screen.getByRole("button", { name: "n:4" })); // the note itself selects nothing
    expect(picked).toEqual(["n:7"]);
  });

  it("links in include the ones from archived notes, marked as such", async () => {
    const archived = { ...NOTE, id: 5, text: "An old idea", state: "archived" as const };
    const fromArchived: NotesGraph = {
      notes: [NOTE, archived],
      links: [
        { id: 3, note_id: 5, link_type: "details", target_kind: "note", target_ref: "4", created_at: NOTE.created_at },
      ],
      targets: [],
    };
    serve(detail());
    await panel(fromArchived);

    const list = await screen.findByRole("list", { name: "Links in" });
    expect(list.textContent).toContain("details");
    expect(list.textContent).toContain("An old idea");
    expect(list.textContent).toContain("archived");
  });
});
