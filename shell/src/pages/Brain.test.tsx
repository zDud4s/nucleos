import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { Brain, validateBrainSearch } from "./Brain";
import type { NoteLink, NotesGraph, OwnerNote } from "../data/owner-notes";
import type { GModel } from "../brain/graph-types";
import { known } from "../brain/knowledge/test-helpers";
import { renderWithRouter } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

// The canvas is its own suite's business: here it is a list of the node ids it was handed, and
// each one a button that selects it.
vi.mock("../brain/ForceGraph", () => ({
  ForceGraph: ({ model, onSelect, compact }: { model: GModel; onSelect: (id: string) => void; compact?: boolean }) => (
    <ul aria-label={compact ? "Local graph nodes" : "Graph nodes"}>
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

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

function note(over: Partial<OwnerNote> = {}): OwnerNote {
  return {
    id: 1,
    text: "Rust owns the state",
    origin: "shell",
    state: "active",
    created_at: "2026-09-01T09:00:00+00:00",
    updated_at: "2026-09-01T09:00:00+00:00",
    ...over,
  };
}

function link(over: Partial<NoteLink> = {}): NoteLink {
  return {
    id: 1,
    note_id: 1,
    link_type: "relates",
    target_kind: "project",
    target_ref: "nucleos",
    created_at: "2026-09-01T09:00:00+00:00",
    ...over,
  };
}

const GRAPH: NotesGraph = {
  notes: [note(), note({ id: 2, text: "Second thought" })],
  links: [link(), link({ id: 2, note_id: 2, target_kind: "note", target_ref: "1" })],
  targets: [{ kind: "project", ref: "nucleos", label: "nucleos", missing: false }],
};

interface Served {
  listed?: OwnerNote[];
  matched?: OwnerNote[];
  graph?: NotesGraph;
  /** Absent answers `[]`; an Error rejects. */
  knowledge?: Error;
}

function daemonWith({ listed = [], matched = [], graph = GRAPH, knowledge }: Served = {}) {
  return (path: string, init?: RequestInit) => {
    if (path.startsWith("/owner-notes/search")) return Promise.resolve(matched);
    if (path.startsWith("/owner-notes/graph")) return Promise.resolve(graph);
    if (path.startsWith("/owner-notes?")) return Promise.resolve(listed);
    if (path === "/owner-notes" && init?.method === "POST") return Promise.resolve({ id: 9 });
    const detail = /^\/owner-notes\/(\d+)$/.exec(path);
    if (detail && init === undefined) {
      const found = graph.notes.find((n) => n.id === Number(detail[1]));
      if (found) return Promise.resolve({ note: found, links_out: [], links_in: [], events: [] });
    }
    if (path === "/knowledge") return knowledge ? Promise.reject(knowledge) : Promise.resolve([]);
    if (path === "/projects") return Promise.resolve([]);
    return Promise.reject(new Error(`unexpected ${path}`));
  };
}

describe("Brain", () => {
  it("saving the capture box creates a note from the shell", async () => {
    daemon.apiFetch.mockImplementation(daemonWith({ listed: [note()] }));
    await renderWithRouter(<Brain />, { initialPath: "/brain" });

    const box = await screen.findByRole("textbox", { name: "Capture a note" });
    fireEvent.change(box, { target: { value: "  a new thought " } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith(
        "/owner-notes",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({ text: "  a new thought ", origin: "shell" }),
        }),
      ),
    );
    await waitFor(() => expect((box as HTMLTextAreaElement).value).toBe(""));
  });

  it("the list view is the unified list, and ?item=knowledge opens the knowledge panel", async () => {
    const row = known({ id: 5, title: "Prefer small diffs", status: "active" });
    daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) =>
      path === "/knowledge" ? Promise.resolve([row]) : daemonWith({ listed: [note()] })(path, init),
    );
    await renderWithRouter(<Brain />, { initialPath: "/brain?item=knowledge:5" });

    expect(await screen.findByText("Rust owns the state")).toBeTruthy();
    expect((await screen.findAllByText(/Prefer small diffs/)).length).toBeGreaterThan(1);
  });

  it("a capture stamp focuses the capture box and keeps the view", async () => {
    daemon.apiFetch.mockImplementation(daemonWith());
    const { router } = await renderWithRouter(<Brain />, {
      initialPath: "/brain?view=graph&capture=1700000000000",
    });

    const box = await screen.findByRole("textbox", { name: "Capture a note" });
    await waitFor(() => expect(document.activeElement).toBe(box));
    await waitFor(() => expect(router.state.location.search).toEqual({ view: "graph" }));
  });

  it("the view comes from the address, and switching it writes the address", async () => {
    daemon.apiFetch.mockImplementation(daemonWith());
    const { router } = await renderWithRouter(<Brain />, { initialPath: "/brain?view=graph" });

    const drawn = await screen.findByRole("list", { name: "Graph nodes" });
    expect(drawn.textContent).toContain("n:1");
    expect(drawn.textContent).toContain("project:nucleos");

    fireEvent.click(screen.getByRole("button", { name: "List" }));
    await waitFor(() => expect(router.state.location.search).toEqual({}));
    expect(screen.queryByRole("list", { name: "Graph nodes" })).toBeNull();
  });

  it("clicking a note in the graph puts it in the address and opens it", async () => {
    daemon.apiFetch.mockImplementation(daemonWith());
    const { router } = await renderWithRouter(<Brain />, { initialPath: "/brain?view=graph" });

    await screen.findByRole("list", { name: "Graph nodes" });
    fireEvent.click(screen.getByRole("button", { name: "n:1" }));
    await waitFor(() => expect(router.state.location.search).toEqual({ view: "graph", item: "note:1" }));
    expect(await screen.findByRole("list", { name: "Local graph nodes" })).toBeTruthy();
  });

  it("an entity node opens its panel without touching the address", async () => {
    daemon.apiFetch.mockImplementation(daemonWith());
    const { router } = await renderWithRouter(<Brain />, { initialPath: "/brain?view=graph" });

    await screen.findByRole("list", { name: "Graph nodes" });
    fireEvent.click(screen.getByRole("button", { name: "project:nucleos" }));
    expect(await screen.findByRole("heading", { name: "project: nucleos" })).toBeTruthy();
    expect(router.state.location.search).toEqual({ view: "graph" });
  });

  it("a malformed item is ignored", async () => {
    expect(validateBrainSearch({ view: "graph", item: "knowledge:abc" })).toEqual({ view: "graph" });
    expect(validateBrainSearch({ view: "bogus", item: "note:0" })).toEqual({ view: "list" });
    expect(validateBrainSearch({ item: "note:45" })).toEqual({ view: "list", item: "note:45" });

    daemon.apiFetch.mockImplementation(daemonWith());
    await renderWithRouter(<Brain />, { initialPath: "/brain?view=graph&item=knowledge:abc" });
    expect(await screen.findByText("Select a node to see what it is linked to.")).toBeTruthy();
  });

  it("knowledge failing draws the notes anyway and says what is missing", async () => {
    daemon.apiFetch.mockImplementation(daemonWith({ knowledge: new Error("down") }));
    await renderWithRouter(<Brain />, { initialPath: "/brain?view=graph" });

    expect(await screen.findByRole("list", { name: "Graph nodes" })).toBeTruthy();
    expect(await screen.findByText(/knowledge did not answer/i)).toBeTruthy();
    expect(screen.queryByText(/the graph is not known/)).toBeNull();
  });
});
