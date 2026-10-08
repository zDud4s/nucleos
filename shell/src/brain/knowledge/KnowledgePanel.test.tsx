import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import type { NotesGraph } from "../../data/owner-notes";
import type { GModel } from "../graph-types";
import { known } from "./test-helpers";
import { KnowledgePanel } from "./KnowledgePanel";
import { renderWithRouter } from "../../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../../data/client", async (original) => ({
  ...(await original<typeof import("../../data/client")>()),
  ...daemon,
}));

vi.mock("../ForceGraph", () => ({
  ForceGraph: ({ model }: { model: GModel }) => (
    <ul aria-label="Local graph nodes">
      {model.nodes.map((node) => (
        <li key={node.id}>{node.id}</li>
      ))}
    </ul>
  ),
}));

const NOTE = {
  id: 12,
  text: "Remember the PATH trick",
  origin: "shell" as const,
  state: "active" as const,
  created_at: "2026-09-01T09:00:00+00:00",
  updated_at: "2026-09-01T09:00:00+00:00",
};

const graph: NotesGraph = {
  notes: [NOTE],
  links: [
    {
      id: 1,
      note_id: 12,
      link_type: "relates",
      target_kind: "knowledge",
      target_ref: "5",
      created_at: "2026-09-01T09:00:00+00:00",
    },
  ],
  targets: [],
};

function serve(rows: ReturnType<typeof known>[]) {
  daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) => {
    if (path === "/knowledge") return Promise.resolve(rows);
    if (path === "/owner-notes/graph?include_archived=true") return Promise.resolve(graph);
    if (path === "/projects") return Promise.resolve([]);
    if (path === "/distill/duplicates" || path === "/distill/causes") return Promise.resolve([]);
    if (init?.method === "POST") return Promise.resolve(undefined);
    return Promise.reject(new Error(`unexpected ${path}`));
  });
}

async function panel(id: number, onSelect: (ref: string) => void = () => {}) {
  await renderWithRouter(<KnowledgePanel id={id} onSelect={onSelect} />, { initialPath: "/brain" });
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("KnowledgePanel", () => {
  it("renders the fields of the row", async () => {
    serve([known({ id: 5, title: "Needs PATH", body: "Nine tests spawn echo.", status: "weird" as never })]);
    await panel(5);

    expect(await screen.findByText("Needs PATH")).toBeTruthy();
    expect(screen.getByText("Nine tests spawn echo.")).toBeTruthy();
    expect(screen.getByText("semantic")).toBeTruthy();
    expect(screen.getByText("nucleos")).toBeTruthy();
    expect(screen.getByText("weird")).toBeTruthy();
  });

  it("approving a proposed row calls approve on its proposal", async () => {
    serve([known({ id: 5, status: "proposed", proposal_id: 33 })]);
    await panel(5);

    fireEvent.click(await screen.findByRole("button", { name: "Approve" }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/proposals/33/approve", { method: "POST" }),
    );
    expect(screen.getByRole("button", { name: "Refuse" })).toBeTruthy();
  });

  it("an active row can be reverted", async () => {
    serve([known({ id: 5, status: "active" })]);
    await panel(5);

    fireEvent.click(await screen.findByRole("button", { name: "Revert" }));
    // ConfirmButton swallows a second click inside its 300ms dwell (the tail of a double-click).
    await new Promise((resolve) => setTimeout(resolve, 350));
    fireEvent.click(screen.getByRole("button", { name: /no longer applies/i }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/knowledge/5/revert", { method: "POST", body: "{}" }),
    );
  });

  it("a row absent from the store is gone", async () => {
    serve([known({ id: 5 })]);
    await panel(99);

    expect(await screen.findByText("gone")).toBeTruthy();
  });

  it("lists the notes pointing at the row and selects one", async () => {
    serve([known({ id: 5 })]);
    const onSelect = vi.fn();
    await panel(5, onSelect);

    fireEvent.click(await screen.findByRole("button", { name: "Remember the PATH trick" }));
    expect(onSelect).toHaveBeenCalledWith("n:12");
  });

  it("reserves a place for where the row was used", async () => {
    serve([known({ id: 5 })]);
    await panel(5);

    expect(await screen.findByRole("heading", { name: "Used in prompts" })).toBeTruthy();
    expect(screen.getByText(/not tracked here yet/i)).toBeTruthy();
  });
});
