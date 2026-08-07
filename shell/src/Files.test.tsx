import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, within } from "@testing-library/react";

import Files from "./Files";
import type { FileEntry } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

// The two Tauri roads into this page: the command that reads a dropped file, and the event that
// says one was dropped. Both are absent outside the app, which the page has to survive.
const invokeMock = vi.fn();
const listeners = new Map<string, (event: { payload: unknown }) => void>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, handler: (event: { payload: unknown }) => void) => {
    listeners.set(name, handler);
    return () => listeners.delete(name);
  },
}));

function entry(overrides: Partial<FileEntry>): FileEntry {
  return { name: "x", is_dir: false, size_bytes: 0, modified: null, ...overrides };
}

/**
 * A daemon holding one folder.
 *
 * `refusals` maps "METHOD /path" to the status it answers with, because most of what this page has
 * to get right is what it says when the daemon says no.
 */
function folder(
  listings: Record<string, FileEntry[]>,
  refusals: Record<string, number> = {},
) {
  fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
    const target = new URL(String(url));
    const method = init?.method ?? "GET";
    const key = `${method} ${target.pathname}`;
    const refused = refusals[key];
    if (refused !== undefined) return { ok: false, status: refused };

    if (key === "GET /files") {
      const listing = listings[target.searchParams.get("path") ?? ""];
      if (listing === undefined) return { ok: false, status: 404 };
      return { ok: true, status: 200, json: async () => listing };
    }
    if (key === "GET /files/search") {
      const needle = (target.searchParams.get("q") ?? "").toLowerCase();
      const hits = Object.entries(listings).flatMap(([where, rows]) =>
        rows
          .filter((row) => row.name.toLowerCase().includes(needle))
          .map((row) => ({ ...row, path: where === "" ? row.name : `${where}/${row.name}` })),
      );
      return { ok: true, status: 200, json: async () => ({ hits, truncated: false }) };
    }
    if (key === "POST /files/upload") {
      return { ok: true, status: 200, json: async () => ({ filename: "guia (2).docx", folder: "" }) };
    }
    return { ok: true, status: 204, json: async () => ({}) };
  });
}

async function settle() {
  await act(async () => {});
}

function calls(method: string, pathname: string) {
  return fetchMock.mock.calls.filter(([url, init]) => {
    const target = new URL(String(url));
    return (init?.method ?? "GET") === method && target.pathname === pathname;
  });
}

function grid() {
  return screen.getByRole("grid");
}

/** A row by the path it carries, which is what every action in this page acts on. */
function row(path: string): HTMLElement {
  const found = grid().querySelector<HTMLElement>(`[data-path="${path}"]`);
  if (found === null) throw new Error(`no row for ${path}`);
  return found;
}

/** The rows on screen, in the order they are drawn. */
function order(): string[] {
  return Array.from(grid().querySelectorAll<HTMLElement>("[data-path]")).map(
    (one) => one.dataset.path ?? "",
  );
}

/** A press-drag-release across two rows, as the pointer handlers see it. */
function drag(from: HTMLElement, to: HTMLElement) {
  fireEvent.pointerDown(from, { clientX: 10, clientY: 10 });
  act(() => {
    window.dispatchEvent(new MouseEvent("pointermove", { clientX: 90, clientY: 60 }));
  });
  fireEvent.pointerEnter(to);
  act(() => {
    window.dispatchEvent(new MouseEvent("pointerup"));
  });
}

describe("the files folder, as a file manager", () => {
  beforeEach(() => {
    fetchMock.mockReset();
    invokeMock.mockReset();
    listeners.clear();
  });
  afterEach(() => fetchMock.mockReset());

  it("shows the tree beside the folder, and walks either one", async () => {
    folder({
      "": [entry({ name: "BACMAT", is_dir: true }), entry({ name: "guia.docx", size_bytes: 2048 })],
      BACMAT: [entry({ name: "2026", is_dir: true })],
    });

    render(<Files token="t" connection="connected" />);
    await settle();

    // The tree draws itself from the listing rather than asking for it again.
    const tree = screen.getByRole("navigation", { name: "Folders" });
    expect(within(tree).getByRole("button", { name: "BACMAT" })).toBeTruthy();
    expect(calls("GET", "/files")).toHaveLength(1);

    // Expanding a branch looks inside it WITHOUT leaving the folder being viewed — the whole reason
    // the chevron and the name are separate targets.
    fireEvent.click(within(tree).getByRole("button", { name: "Expand BACMAT" }));
    await settle();
    expect(within(tree).getByRole("button", { name: "2026" })).toBeTruthy();
    expect(screen.getByText("2.0 kB")).toBeTruthy();

    // The name walks into it.
    fireEvent.click(within(tree).getByRole("button", { name: "BACMAT" }));
    await settle();
    expect(within(grid()).queryByText("guia.docx")).toBeNull();
  });

  it("sorts by a column and turns it around on a second click", async () => {
    folder({
      "": [
        entry({ name: "grande.bin", size_bytes: 9000 }),
        entry({ name: "pequeno.txt", size_bytes: 10 }),
        entry({ name: "Zulu", is_dir: true }),
      ],
    });

    render(<Files token="t" connection="connected" />);
    await settle();

    // Folders lead whatever the column says, because they are how you move.
    expect(order()).toEqual(["Zulu", "grande.bin", "pequeno.txt"]);

    fireEvent.click(screen.getByRole("button", { name: /Size/ }));
    expect(order()).toEqual(["Zulu", "pequeno.txt", "grande.bin"]);

    fireEvent.click(screen.getByRole("button", { name: /Size/ }));
    expect(order()).toEqual(["Zulu", "grande.bin", "pequeno.txt"]);
  });

  it("selects a range with shift and acts on the whole selection at once", async () => {
    folder({
      "": [entry({ name: "a.txt" }), entry({ name: "b.txt" }), entry({ name: "c.txt" })],
    });

    render(<Files token="t" connection="connected" />);
    await settle();

    fireEvent.pointerDown(row("a.txt"));
    fireEvent.pointerDown(row("c.txt"), { shiftKey: true });
    expect(screen.getByText(/3 selected/)).toBeTruthy();

    // `ConfirmButton` discards a second click inside 300 ms, so the confirmation waits the way a
    // person would.
    vi.useFakeTimers();
    try {
      fireEvent.click(screen.getByRole("button", { name: "Delete" }));
      act(() => vi.advanceTimersByTime(400));
      fireEvent.click(screen.getByRole("button", { name: "Delete 3 for good?" }));
      await settle();
    } finally {
      vi.useRealTimers();
    }

    // One request per entry, and all three of them: a bulk gesture must not quietly act on one.
    expect(calls("DELETE", "/files").map(([url]) => new URL(String(url)).searchParams.get("path")))
      .toEqual(["a.txt", "b.txt", "c.txt"]);
  });

  it("searches below the folder and says where each hit lives", async () => {
    folder({
      "": [entry({ name: "BACMAT", is_dir: true })],
      BACMAT: [entry({ name: "guia.docx" })],
    });

    render(<Files token="t" connection="connected" />);
    await settle();

    fireEvent.change(screen.getByPlaceholderText(/Search this folder/), {
      target: { value: "guia" },
    });
    // The box waits before asking, so a word typed letter by letter is one search, not four.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 250));
    });

    expect(calls("GET", "/files/search")).toHaveLength(1);
    // A hit is identified by its path under the root, not by its name: that is what lets a click on
    // a result in another folder act on the right file.
    const hit = row("BACMAT/guia.docx");
    expect(hit.textContent).toContain("BACMAT");
  });

  it("opens a menu on the row under the cursor and keeps it there", async () => {
    folder({ "": [entry({ name: "guia.docx" })] });

    render(<Files token="t" connection="connected" />);
    await settle();

    fireEvent.contextMenu(row("guia.docx"), { clientX: 40, clientY: 80 });
    const menu = screen.getByRole("menu");
    expect(within(menu).getByRole("menuitem", { name: "Download" })).toBeTruthy();
    expect(within(menu).getByRole("menuitem", { name: "Rename or move" })).toBeTruthy();
    // Right-clicking a row selects it, so the menu and the toolbar act on the same thing.
    expect(screen.getByText(/1 selected/)).toBeTruthy();

    fireEvent.click(window);
    await settle();
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("moves with the keyboard, opens with Enter and climbs with Backspace", async () => {
    folder({
      "": [entry({ name: "BACMAT", is_dir: true }), entry({ name: "guia.docx" })],
      BACMAT: [entry({ name: "dentro.txt" })],
    });

    render(<Files token="t" connection="connected" />);
    await settle();

    fireEvent.keyDown(grid(), { key: "ArrowDown" });
    expect(screen.getByText(/1 selected/)).toBeTruthy();
    fireEvent.keyDown(grid(), { key: "Enter" });
    await settle();
    expect(row("BACMAT/dentro.txt")).toBeTruthy();

    fireEvent.keyDown(grid(), { key: "Backspace" });
    await settle();
    expect(row("guia.docx")).toBeTruthy();
  });

  /** Dragging a row onto a folder is the move a file manager is FOR. */
  it("moves what is dragged onto the folder it was dropped on", async () => {
    folder({
      "": [entry({ name: "BACMAT", is_dir: true }), entry({ name: "guia.docx" })],
      BACMAT: [],
    });

    render(<Files token="t" connection="connected" />);
    await settle();

    drag(row("guia.docx"), row("BACMAT"));
    await settle();

    const [, init] = calls("POST", "/files/move")[0] ?? [];
    expect(JSON.parse(String(init?.body))).toEqual({ from: "guia.docx", to: "BACMAT/guia.docx" });
  });

  it("refuses to drop a folder inside itself without asking the daemon", async () => {
    folder({ "": [entry({ name: "BACMAT", is_dir: true })], BACMAT: [] });

    render(<Files token="t" connection="connected" />);
    await settle();

    const bacmat = row("BACMAT");
    // Dragging it onto itself: the row is both source and target.
    fireEvent.pointerDown(bacmat, { clientX: 10, clientY: 10 });
    act(() => {
      window.dispatchEvent(new MouseEvent("pointermove", { clientX: 90, clientY: 60 }));
    });
    const tree = screen.getByRole("navigation", { name: "Folders" });
    fireEvent.pointerEnter(within(tree).getByRole("button", { name: "BACMAT" }).parentElement!);
    act(() => {
      window.dispatchEvent(new MouseEvent("pointerup"));
    });
    await settle();

    expect(calls("POST", "/files/move")).toHaveLength(0);
    expect(screen.getByText(/cannot be moved inside itself/)).toBeTruthy();
  });

  /**
   * Windows' own drag. The page never sees an HTML drop — `drop.rs` resolves it and emits a
   * manifest, so this is the event the page actually has to handle.
   */
  it("rebuilds a dropped folder's shape before uploading into it", async () => {
    folder({ "": [] });
    invokeMock.mockResolvedValue(new ArrayBuffer(8));

    render(<Files token="t" connection="connected" />);
    await settle();

    await act(async () => {
      listeners.get("files://dropped")?.({
        payload: {
          files: [
            { path: "C:/tmp/Relatorios/2026/guia.docx", folder: "Relatorios/2026", name: "guia.docx", size: 8 },
            { path: "C:/tmp/solto.txt", folder: "", name: "solto.txt", size: 4 },
          ],
          truncated: false,
        },
      });
    });
    await settle();

    // The folder is made first: a file whose folder does not exist yet would be refused.
    const made = calls("POST", "/files/folder").map(([, init]) => JSON.parse(String(init?.body)).path);
    expect(made).toEqual(["Relatorios/2026"]);
    const into = calls("POST", "/files/upload").map(([url]) =>
      new URL(String(url)).searchParams.get("folder"),
    );
    expect(into).toEqual(["Relatorios/2026", ""]);
    expect(screen.getByText(/Uploaded 2 files/)).toBeTruthy();
  });

  it("says what a drop left behind instead of reporting a clean upload", async () => {
    folder({ "": [] });
    invokeMock.mockResolvedValue(new ArrayBuffer(4));

    render(<Files token="t" connection="connected" />);
    await settle();

    await act(async () => {
      listeners.get("files://dropped")?.({
        payload: {
          files: [
            { path: "C:/tmp/enorme.iso", folder: "", name: "enorme.iso", size: 900 * 1024 * 1024 },
            { path: "C:/tmp/ok.txt", folder: "", name: "ok.txt", size: 4 },
          ],
          truncated: true,
        },
      });
    });
    await settle();

    // One uploaded, one too big, and a walk that was cut short — all three said.
    expect(calls("POST", "/files/upload")).toHaveLength(1);
    const said = screen.getByText(/Uploaded 1 file/).textContent ?? "";
    expect(said).toContain("enorme.iso");
    expect(said).toContain("more than this drop would carry");
  });

  it("explains a daemon with no folder instead of showing an empty one", async () => {
    folder({}, { "GET /files": 503 });

    render(<Files token="t" connection="connected" />);
    await settle();

    expect(screen.getByText(/could not create one at startup/)).toBeTruthy();
  });
});
