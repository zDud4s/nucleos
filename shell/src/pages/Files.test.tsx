import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/api/path", () => ({ localDataDir: vi.fn(async () => "C:\\Users\\ana\\AppData\\Local") }));
const opener = vi.hoisted(() => ({ openPath: vi.fn(), openUrl: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => opener);

const daemon = vi.hoisted(() => ({
  apiFetch: vi.fn(),
  apiText: vi.fn(),
  apiBlob: vi.fn(),
  probeHealth: vi.fn(),
}));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Files } from "./Files";
import { ApiRefusal } from "../data/client";
import type { Dropped, Entry, Hit, SavedFile, Trashed } from "../data/files";
import { renderWithQuery } from "../test/harness";

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.apiBlob.mockReset();
  daemon.probeHealth.mockReset();
  mockInvoke.mockReset();
  mockListen.mockReset();
  opener.openPath.mockReset();
  opener.openPath.mockResolvedValue(undefined);
  // The three OS-drop subscriptions resolve to a harmless unlisten by
  // default; the one test that needs to fire a handler overrides this.
  mockListen.mockResolvedValue(() => {});
  // jsdom implements neither — `downloadFile` (`data/files.ts`) calls both,
  // and without a stub the promise it returns rejects with "not a function"
  // the moment a test exercises the download path.
  URL.createObjectURL = vi.fn(() => "blob:mock-url");
  URL.revokeObjectURL = vi.fn();
});

/* ------------------------------------------------------------- fixtures -- */

function entry(overrides: Partial<Entry> = {}): Entry {
  return {
    name: "report.docx",
    is_dir: false,
    size_bytes: 2048,
    modified: "2026-08-17T09:00:00Z",
    ...overrides,
  };
}

interface FilesDaemonConfig {
  list?: (path: string) => Entry[];
  onDelete?: (path: string, recursive: boolean) => void;
  onMove?: (from: string, to: string) => void;
  onUpload?: (folder: string, filename: string, body: unknown) => SavedFile;
  trash?: () => Trashed[];
  onRestore?: (id: string) => void;
  search?: (q: string) => Hit[];
}

/** What the daemon hands back for a delete: the entry, now in the trash. */
function trashed(path: string, overrides: Partial<Trashed> = {}): Trashed {
  return { id: `t-${path}`, path, is_dir: false, size_bytes: 0, deleted_at: "2026-09-24T09:00:00Z", ...overrides };
}

/**
 * A stand-in for every `/files*` route `apiFetch` reaches, dispatching on the
 * method and the path the way `core/src/http.rs` actually splits them —
 * `GET /files` lists, `DELETE /files` takes query params and no body,
 * `POST /files/move` and `POST /files/upload` each read their own shape.
 * `POST /files/folder` is deliberately absent: it never goes through
 * `apiFetch` at all (see the "text reader" test below).
 */
function makeFilesDaemon(config: FilesDaemonConfig): (path: string, init?: RequestInit) => Promise<unknown> {
  return async (path, init) => {
    const url = new URL(path, "http://files.local");
    const method = init?.method ?? "GET";

    if (url.pathname === "/files" && method === "GET") {
      const p = url.searchParams.get("path") ?? "";
      return (config.list ?? (() => []))(p);
    }
    if (url.pathname === "/files" && method === "DELETE") {
      const p = url.searchParams.get("path") ?? "";
      const recursive = url.searchParams.get("recursive") === "true";
      (config.onDelete ?? (() => {}))(p, recursive);
      return trashed(p);
    }
    if (url.pathname === "/files/trash" && method === "GET") {
      return (config.trash ?? (() => []))();
    }
    if (url.pathname === "/files/restore" && method === "POST") {
      const body = JSON.parse(String(init?.body)) as { id: string };
      (config.onRestore ?? (() => {}))(body.id);
      return { path: body.id.replace(/^t-/, "") };
    }
    if (url.pathname === "/files/move" && method === "POST") {
      const body = JSON.parse(String(init?.body)) as { from: string; to: string };
      (config.onMove ?? (() => {}))(body.from, body.to);
      return undefined;
    }
    if (url.pathname === "/files/upload" && method === "POST") {
      const folder = url.searchParams.get("folder") ?? "";
      const filename = url.searchParams.get("filename") ?? "";
      return (config.onUpload ?? ((f: string, n: string) => ({ filename: n, folder: f })))(
        folder,
        filename,
        init?.body,
      );
    }
    if (url.pathname === "/files/search" && method === "GET") {
      return { hits: (config.search ?? (() => []))(url.searchParams.get("q") ?? ""), truncated: false };
    }
    return [];
  };
}

/* ----------------------------------------------------------- the no-root -- */

describe("Files — no root configured", () => {
  it("teaches when the daemon answers with a bodyless 503", async () => {
    daemon.apiFetch.mockImplementation(async () => {
      // `client.ts` fills `detail` from `res.statusText` for a body-less
      // refusal — "Service Unavailable" is two words, under the four-word
      // floor, so this page's own sentence must win regardless.
      throw new ApiRefusal(503, "unavailable", "Service Unavailable");
    });

    renderWithQuery(<Files />);

    expect(await screen.findByText("The files folder could not be opened")).toBeDefined();
    expect(screen.getByText(/%LOCALAPPDATA%\\nucleos\\NucleOS\\data\\files/)).toBeDefined();
    // Never an invitation to retry — there is nothing to retry towards.
    expect(screen.queryByText(/Service Unavailable/)).toBeNull();
  });
});

/* --------------------------------------------------------------- listing -- */

describe("Files — the listing", () => {
  it("lists a folder's entries with a count and total size", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [
          entry({ name: "report.docx", is_dir: false, size_bytes: 2048 }),
          entry({ name: "archive", is_dir: true, size_bytes: 0, modified: null }),
        ],
      }),
    );

    renderWithQuery(<Files />);

    expect(await screen.findByText("report.docx")).toBeDefined();
    expect(screen.getByText("archive/")).toBeDefined();
    // Said once, in the headline — the panel repeated it in its corner.
    expect(screen.getByText("1 folder, 1 file totalling 2.0 KB")).toBeDefined();
    expect(screen.queryByText(/2 items/)).toBeNull();
  });

  it("toggles aria-sort when a column header is clicked", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [
          entry({ name: "b.txt", size_bytes: 10 }),
          entry({ name: "a.txt", size_bytes: 20 }),
        ],
      }),
    );

    renderWithQuery(<Files />);
    await screen.findByText("a.txt");

    const nameHeader = screen.getByRole("columnheader", { name: /Name/ });
    expect(nameHeader.getAttribute("aria-sort")).toBe("ascending");

    fireEvent.click(within(nameHeader).getByRole("button"));
    expect(nameHeader.getAttribute("aria-sort")).toBe("descending");
  });

  it("descends into a folder and returns to the root via breadcrumbs", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) =>
          path === ""
            ? [entry({ name: "archive", is_dir: true, size_bytes: 0, modified: null })]
            : [entry({ name: "old.txt", is_dir: false, size_bytes: 100 })],
      }),
    );

    renderWithQuery(<Files />);

    fireEvent.click(await screen.findByText("archive/"));
    expect(await screen.findByText("old.txt")).toBeDefined();
    expect(screen.getByText("/ archive")).toBeDefined();

    // Both the tree and the breadcrumbs offer a root link named "files" —
    // scope to the trail, which is the one under test here.
    const crumbs = screen.getByRole("navigation", { name: "Folder path" });
    fireEvent.click(within(crumbs).getByRole("button", { name: "files" }));
    expect(await screen.findByText("archive/")).toBeDefined();
  });
});

/* --------------------------------------------------------- new folder -- */

describe("Files — making a folder", () => {
  it("creates a folder through the text reader, not the JSON one", async () => {
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [] }));
    // `POST /files/folder` is 201 with an EMPTY body — `apiFetch` would throw
    // trying to parse nothing as JSON, which is exactly why this route must
    // go through `apiText`.
    daemon.apiText.mockResolvedValue("");

    renderWithQuery(<Files />);
    await screen.findByText("Nothing has been filed yet");

    fireEvent.click(screen.getByRole("button", { name: "New folder" }));
    fireEvent.change(screen.getByLabelText("New folder name"), { target: { value: "reports" } });
    fireEvent.click(screen.getByRole("button", { name: "Create folder" }));

    await waitFor(() =>
      expect(daemon.apiText).toHaveBeenCalledWith(
        "/files/folder",
        expect.objectContaining({ method: "POST", body: JSON.stringify({ path: "reports" }) }),
      ),
    );
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/files/folder", expect.anything());
  });
});

/* -------------------------------------------------------------- moving -- */

describe("Files — moving", () => {
  it("names a move refused by an existing destination", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) => (path === "" ? [entry({ name: "archive", is_dir: true, size_bytes: 0 }), entry({ name: "report.docx" })] : []),
        onMove: () => {
          // `PathError::Exists` — 409, empty body. A move never overwrites.
          throw new ApiRefusal(409, "conflict", "");
        },
      }),
    );

    renderWithQuery(<Files />);
    const row = (await screen.findByText("report.docx")).closest("tr");
    if (row === null) throw new Error("row not found");

    fireEvent.contextMenu(row);
    fireEvent.click(await screen.findByRole("menuitem", { name: "Move…" }));
    const picker = screen.getByRole("navigation", { name: "Destination folder" });
    fireEvent.click(await within(picker).findByRole("button", { name: "archive" }));
    fireEvent.click(screen.getByRole("button", { name: "Move" }));

    expect(await screen.findByText(/there is already something there/)).toBeDefined();
  });
});

/* -------------------------------------------------------------- upload -- */

describe("Files — uploading", () => {
  it("uploads raw bytes, not FormData, and shows the name the daemon actually saved it under", async () => {
    const uploaded: { folder: string; filename: string; body: unknown }[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [],
        onUpload: (folder, filename, body) => {
          uploaded.push({ folder, filename, body });
          // The daemon sanitises and de-collides — the name it hands back can
          // legitimately differ from the one that was dropped in.
          return { filename: "report (2).docx", folder };
        },
      }),
    );

    renderWithQuery(<Files />);
    await screen.findByText("Nothing has been filed yet");

    const input = screen.getByLabelText("Upload");
    const file = new File([new Uint8Array([1, 2, 3])], "report.docx", { type: "application/octet-stream" });
    fireEvent.change(input, { target: { files: [file] } });

    await waitFor(() => expect(uploaded).toHaveLength(1));
    expect(uploaded[0]?.body).not.toBeInstanceOf(FormData);
    expect(uploaded[0]?.body).toBeInstanceOf(ArrayBuffer);
    expect(await screen.findByText("report (2).docx")).toBeDefined();
    expect(screen.getByText(/The file from that upload was uploaded as/)).toBeDefined();
  });

  it("keeps the upload control as one labelled, styled control", async () => {
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [] }));

    renderWithQuery(<Files />);
    await screen.findByText("Nothing has been filed yet");

    const input = screen.getByLabelText("Upload");
    expect(input.classList.contains("sr-only")).toBe(true);
    expect(input.getAttribute("aria-label")).toBeNull();
    expect(input.closest("label")?.textContent).toBe("Upload");
  });

  it("reports every picked file, and names the one refused among them", async () => {
    // Each upload is awaited in turn. Fired together, only the last call's
    // success was ever reported, and an earlier refusal vanished under it.
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [],
        onUpload: (folder, filename) => {
          if (filename === "second.txt") throw new ApiRefusal(500, "internal", "");
          return { filename, folder };
        },
      }),
    );

    renderWithQuery(<Files />);
    await screen.findByText("Nothing has been filed yet");

    const files = ["first.txt", "second.txt", "third.txt"].map(
      (name) => new File([new Uint8Array([1])], name, { type: "text/plain" }),
    );
    fireEvent.change(screen.getByLabelText("Upload"), { target: { files } });

    expect(await screen.findByText(/1 of 3 files from that upload stayed behind/)).toBeDefined();
    expect(screen.getByText(/second\.txt — the núcleo could not write that file to disk/)).toBeDefined();
    expect(screen.getByText("first.txt")).toBeDefined();
    expect(screen.getByText("third.txt")).toBeDefined();
  });
});

/* ------------------------------------------------------------ download -- */

describe("Files — downloading", () => {
  it("downloads through apiBlob, not apiFetch or apiText", async () => {
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [entry({ name: "report.docx" })] }));
    daemon.apiBlob.mockResolvedValue(new Blob(["hello"]));

    renderWithQuery(<Files />);
    const row = (await screen.findByText("report.docx")).closest("tr");
    if (row === null) throw new Error("row not found");

    fireEvent.contextMenu(row);
    fireEvent.click(await screen.findByRole("menuitem", { name: "Download a copy" }));

    await waitFor(() => expect(daemon.apiBlob).toHaveBeenCalledWith("/files/download?path=report.docx"));
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/files/download?path=report.docx", expect.anything());
    expect(daemon.apiText).not.toHaveBeenCalledWith("/files/download?path=report.docx", expect.anything());
  });
});

/* ----------------------------------------------------------------- open -- */

describe("Files — opening in Windows", () => {
  it("opens a double-clicked file with its app, by its full path, and downloads nothing", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) => (path === "" ? [entry({ name: "invoices", is_dir: true, size_bytes: 0 })] : [entry({ name: "march.pdf" })]),
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(within(await screen.findByRole("table")).getByText("invoices/"));
    fireEvent.doubleClick(await screen.findByText("march.pdf"));

    await waitFor(() =>
      expect(opener.openPath).toHaveBeenCalledWith("C:\\Users\\ana\\AppData\\Local\\nucleos\\NucleOS\\data\\files\\invoices\\march.pdf"),
    );
    expect(daemon.apiBlob).not.toHaveBeenCalled();
    expect(await screen.findByText("opened march.pdf")).toBeDefined();
  });

  it("never runs a program, and says so by name", async () => {
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [entry({ name: "setup.exe" })] }));

    renderWithQuery(<Files />);
    fireEvent.doubleClick(await screen.findByText("setup.exe"));

    expect(await screen.findByText(/a program is never run from here/)).toBeDefined();
    expect(opener.openPath).not.toHaveBeenCalled();
  });

  it("says Windows could not open it when the opener refuses", async () => {
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [entry({ name: "notes.xyz" })] }));
    opener.openPath.mockRejectedValue("no application is associated");

    renderWithQuery(<Files />);
    fireEvent.doubleClick(await screen.findByText("notes.xyz"));

    expect(await screen.findByText("Windows could not open it — no application is associated")).toBeDefined();
  });

  it("opens nothing on one mouse click on a file — only a folder opens on one", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) =>
          path === "" ? [entry({ name: "archive", is_dir: true, size_bytes: 0 }), entry({ name: "report.docx" })] : [],
      }),
    );

    renderWithQuery(<Files />);
    // `detail: 1` is a pointer click; the keyboard's click carries 0.
    fireEvent.click(await screen.findByText("report.docx"), { detail: 1 });
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(opener.openPath).not.toHaveBeenCalled();

    fireEvent.click(screen.getByText("archive/"), { detail: 1 });
    expect(await screen.findByText("/ archive")).toBeDefined();
  });

  it("copies every selected path, one per line, from the menu of a row inside the selection", async () => {
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [entry({ name: "a.txt" }), entry({ name: "b.txt" })] }));

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select a.txt" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Select b.txt" }));
    const row = screen.getByText("b.txt").closest("tr");
    if (row === null) throw new Error("row not found");
    fireEvent.contextMenu(row);
    fireEvent.click(await screen.findByRole("menuitem", { name: "Copy 2 full paths" }));

    const root = "C:\\Users\\ana\\AppData\\Local\\nucleos\\NucleOS\\data\\files\\";
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(`${root}a.txt\n${root}b.txt`));
    expect(await screen.findByText("copied 2 paths, one per line")).toBeDefined();
  });

  it("copies the full path on disk, not the part under the root", async () => {
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [entry({ name: "report.docx" })] }));

    renderWithQuery(<Files />);
    const row = (await screen.findByText("report.docx")).closest("tr");
    if (row === null) throw new Error("row not found");
    fireEvent.contextMenu(row);
    fireEvent.click(await screen.findByRole("menuitem", { name: "Copy full path" }));

    const full = "C:\\Users\\ana\\AppData\\Local\\nucleos\\NucleOS\\data\\files\\report.docx";
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(full));
    expect(await screen.findByText(`copied ${full}`)).toBeDefined();
  });
});

/* --------------------------------------------------------------- delete -- */

describe("Files — deleting", () => {
  it("moves to the trash on one press of Delete, and Undo brings it back", async () => {
    const deleted: string[] = [];
    const restored: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "report.docx" })],
        onDelete: (path) => {
          deleted.push(path);
        },
        onRestore: (id) => {
          restored.push(id);
        },
      }),
    );

    renderWithQuery(<Files />);
    const checkbox = await screen.findByRole("checkbox", { name: "Select report.docx" });
    fireEvent.click(checkbox);

    const wrap = checkbox.closest(".fi-table-wrap");
    if (wrap === null) throw new Error("table wrap not found");

    fireEvent.keyDown(wrap, { key: "Delete" });
    await waitFor(() => expect(deleted).toEqual(["report.docx"]));
    expect(await screen.findByText(/to Recently deleted/)).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(restored).toEqual(["t-report.docx"]));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Undo" })).toBeNull());
    expect(await screen.findByText("restored report.docx")).toBeDefined();
  });

  it("ignores a held Delete key's auto-repeat", async () => {
    const deleted: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "report.docx" })],
        onDelete: (path) => {
          deleted.push(path);
        },
      }),
    );

    renderWithQuery(<Files />);
    const checkbox = await screen.findByRole("checkbox", { name: "Select report.docx" });
    fireEvent.click(checkbox);
    const wrap = checkbox.closest(".fi-table-wrap");
    if (wrap === null) throw new Error("table wrap not found");

    fireEvent.keyDown(wrap, { key: "Delete", repeat: true });
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(deleted).toHaveLength(0);
  });

  it("names the folders it will not empty, lands on Leave, and deletes them in one click", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "archive", is_dir: true, size_bytes: 0, modified: null })],
        onDelete: (_path, recursive) => {
          if (!recursive) throw new ApiRefusal(409, "conflict", "");
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select archive" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));

    const note = await screen.findByRole("group", { name: /still has things inside/ });
    expect(within(note).getByText("archive/")).toBeDefined();
    await waitFor(() => expect(document.activeElement).toBe(within(note).getByRole("button", { name: "Keep it" })));

    fireEvent.click(within(note).getByRole("button", { name: "Delete with everything inside" }));

    await waitFor(() => expect(screen.queryByRole("group", { name: /still has things inside/ })).toBeNull());
    expect(await screen.findByText(/to Recently deleted/)).toBeDefined();
  });

  it("deletes several in one click, and one Undo brings them all back", async () => {
    const deleted: string[] = [];
    const restored: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "a.txt" }), entry({ name: "b.txt" })],
        onDelete: (path) => {
          deleted.push(path);
        },
        onRestore: (id) => {
          restored.push(id);
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select a.txt" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Select b.txt" }));

    fireEvent.click(screen.getByRole("button", { name: "Delete 2" }));
    await waitFor(() => expect(deleted).toEqual(["a.txt", "b.txt"]));
    expect(await screen.findByText("moved 2 items to Recently deleted")).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() => expect(restored).toEqual(["t-a.txt", "t-b.txt"]));
  });

  it("deletes from the right-click menu the same way, undo included", async () => {
    const deleted: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "report.docx" })],
        onDelete: (path) => {
          deleted.push(path);
        },
      }),
    );

    renderWithQuery(<Files />);
    const row = (await screen.findByText("report.docx")).closest("tr");
    if (row === null) throw new Error("row not found");
    fireEvent.contextMenu(row);
    fireEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));

    await waitFor(() => expect(deleted).toEqual(["report.docx"]));
    expect(await screen.findByRole("button", { name: "Undo" })).toBeDefined();
  });

  it("deletes the whole selection from the menu of a row inside it", async () => {
    const deleted: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "a.txt" }), entry({ name: "b.txt" }), entry({ name: "c.txt" })],
        onDelete: (path) => {
          deleted.push(path);
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select a.txt" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Select b.txt" }));
    const row = screen.getByText("b.txt").closest("tr");
    if (row === null) throw new Error("row not found");
    fireEvent.contextMenu(row);

    expect(await screen.findByRole("menu", { name: "Actions for 2 selected items" })).toBeDefined();
    fireEvent.click(screen.getByRole("menuitem", { name: "Delete 2" }));
    await waitFor(() => expect(deleted).toEqual(["a.txt", "b.txt"]));
  });

  it("folds the refusals of a batch delete to one counted line that still names each item", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "a.txt" }), entry({ name: "b.txt" })],
        onDelete: () => {
          throw new ApiRefusal(500, "internal", "");
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select all" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete 2" }));

    const summary = await screen.findByText("2 deletes did not go through");
    const note = summary.closest("details");
    if (note === null) throw new Error("the refusals are not folded");
    expect(within(note).getByText("a.txt")).toBeDefined();
    expect(within(note).getByText("b.txt")).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "Dismiss these notes" }));
    expect(screen.queryByText("2 deletes did not go through")).toBeNull();
  });

  it("says why a restore could not go back", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "report.docx" })],
        onRestore: () => {
          throw new ApiRefusal(409, "conflict", "");
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select report.docx" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    fireEvent.click(await screen.findByRole("button", { name: "Undo" }));

    expect(await screen.findByText(/a restore never overwrites/)).toBeDefined();
  });

  it("restores from Recently deleted after the undo has gone", async () => {
    const restored: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [],
        trash: () => [trashed("invoices/old.pdf", { id: "t-old" })],
        onRestore: (id) => {
          restored.push(id);
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByText(/Recently deleted/));
    fireEvent.click(await screen.findByRole("button", { name: "Restore invoices/old.pdf" }));

    await waitFor(() => expect(restored).toEqual(["t-old"]));
  });

  it("announces only the count, never the buttons beside it", async () => {
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [entry({ name: "a.txt" })] }));

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select a.txt" }));

    const live = screen.getByText("1 selected");
    expect(live.getAttribute("role")).toBe("status");
    expect(within(live).queryByRole("button")).toBeNull();
  });
});

/* ------------------------------------------------- keyboard and the tree -- */

describe("Files — reachable without a mouse", () => {
  it("opens Move from the selection bar, not only from the right-click menu", async () => {
    const moved: [string, string][] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) => (path === "" ? [entry({ name: "archive", is_dir: true, size_bytes: 0 }), entry({ name: "report.docx" })] : []),
        onMove: (from, to) => {
          moved.push([from, to]);
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select report.docx" }));
    fireEvent.click(screen.getByRole("button", { name: "Move…" }));

    // The folder it is already in is no destination at all.
    expect((screen.getByRole("button", { name: "Move" }) as HTMLButtonElement).disabled).toBe(true);
    const picker = screen.getByRole("navigation", { name: "Destination folder" });
    fireEvent.click(await within(picker).findByRole("button", { name: "archive" }));
    expect(screen.getByText("files/archive/report.docx")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Move" }));

    await waitFor(() => expect(moved).toEqual([["report.docx", "archive/report.docx"]]));
  });

  it("moves the whole selection into the folder picked", async () => {
    const moved: [string, string][] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) =>
          path === ""
            ? [entry({ name: "archive", is_dir: true, size_bytes: 0 }), entry({ name: "a.txt" }), entry({ name: "b.txt" })]
            : [],
        onMove: (from, to) => {
          moved.push([from, to]);
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select a.txt" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Select b.txt" }));
    fireEvent.click(screen.getByRole("button", { name: "Move…" }));
    // Named, and not a refusal before anything was picked.
    expect(screen.getByText("a.txt, b.txt")).toBeDefined();
    expect(screen.getByText(/Pick the folder to move them into/)).toBeDefined();
    const picker = screen.getByRole("navigation", { name: "Destination folder" });
    fireEvent.click(await within(picker).findByRole("button", { name: "archive" }));
    fireEvent.click(screen.getByRole("button", { name: "Move" }));

    await waitFor(() =>
      expect(moved).toEqual([
        ["a.txt", "archive/a.txt"],
        ["b.txt", "archive/b.txt"],
      ]),
    );
    await waitFor(() => expect(screen.queryByRole("navigation", { name: "Destination folder" })).toBeNull());
  });

  const withArchive = (path: string) =>
    path === "" ? [entry({ name: "archive", is_dir: true, size_bytes: 0 }), entry({ name: "report.docx" })] : [];

  it("moves on a second pick of the same folder, without the button", async () => {
    const moved: [string, string][] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: withArchive,
        onMove: (from, to) => {
          moved.push([from, to]);
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select report.docx" }));
    fireEvent.click(screen.getByRole("button", { name: "Move…" }));
    const picker = screen.getByRole("navigation", { name: "Destination folder" });
    const archive = await within(picker).findByRole("button", { name: "archive" });
    fireEvent.click(archive);
    expect(moved).toEqual([]);
    fireEvent.click(archive);

    await waitFor(() => expect(moved).toEqual([["report.docx", "archive/report.docx"]]));
  });

  it("cuts with Ctrl+X and moves into the folder on screen with Ctrl+V", async () => {
    const moved: [string, string][] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: withArchive,
        onMove: (from, to) => {
          moved.push([from, to]);
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select report.docx" }));
    fireEvent.keyDown(document.body, { key: "x", ctrlKey: true });
    expect(await screen.findByText(/report\.docx cut — open the folder it goes to/)).toBeDefined();
    // Pasting where it already is moves nothing.
    fireEvent.keyDown(document.body, { key: "v", ctrlKey: true });
    expect(moved).toEqual([]);

    const tree = screen.getByRole("navigation", { name: "Folders" });
    fireEvent.click(await within(tree).findByRole("button", { name: "archive" }));
    expect(await screen.findByText(/report\.docx cut from files\//)).toBeDefined();
    fireEvent.keyDown(document.body, { key: "v", ctrlKey: true });

    await waitFor(() => expect(moved).toEqual([["report.docx", "archive/report.docx"]]));
    expect(await screen.findByText("moved report.docx to files/archive/")).toBeDefined();
  });

  it("moves a row dragged onto a folder of the side tree", async () => {
    const moved: [string, string][] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: withArchive,
        onMove: (from, to) => {
          moved.push([from, to]);
        },
      }),
    );

    renderWithQuery(<Files />);
    const row = (await screen.findByText("report.docx")).closest("tr");
    const tree = screen.getByRole("navigation", { name: "Folders" });
    const target = (await within(tree).findByRole("button", { name: "archive" })).closest<HTMLElement>("[data-drop-path]");
    if (row === null || target === null) throw new Error("row or drop target missing");

    fireEvent.pointerDown(row, { button: 0, clientX: 10, clientY: 10 });
    // A few pixels is still a click, not a drag.
    fireEvent.pointerMove(target, { clientX: 12, clientY: 11 });
    expect(target.classList.contains("fi-drop-over")).toBe(false);
    fireEvent.pointerMove(target, { clientX: 60, clientY: 80 });
    expect(target.classList.contains("fi-drop-over")).toBe(true);
    expect(screen.getByText("move report.docx to files/archive/")).toBeDefined();
    fireEvent.pointerUp(target, { clientX: 60, clientY: 80 });

    await waitFor(() => expect(moved).toEqual([["report.docx", "archive/report.docx"]]));
    expect(target.classList.contains("fi-drop-over")).toBe(false);
  });

  it("marks the folder you are in as the current location, not a disabled button", async () => {
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [] }));

    renderWithQuery(<Files />);
    await screen.findByText("Nothing has been filed yet");

    const crumbs = screen.getByRole("navigation", { name: "Folder path" });
    expect(within(crumbs).queryByRole("button")).toBeNull();
    expect(within(crumbs).getByText("files").getAttribute("aria-current")).toBe("location");
  });

  it("says a tree branch could not be read instead of drawing it empty", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) => {
          if (path === "scans") throw new ApiRefusal(500, "internal", "");
          return [entry({ name: "scans", is_dir: true, size_bytes: 0 })];
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("button", { name: "Expand scans" }));

    const tree = screen.getByRole("navigation", { name: "Folders" });
    expect(await within(tree).findByText("unread")).toBeDefined();
  });

  it("moves real focus down the rows, so each is read by its name", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({ list: () => [entry({ name: "a.txt" }), entry({ name: "b.txt" })] }),
    );

    renderWithQuery(<Files />);
    await screen.findByText("a.txt");
    const wrap = document.querySelector(".fi-table-wrap");
    if (!(wrap instanceof HTMLElement)) throw new Error("table wrap not found");

    fireEvent.keyDown(wrap, { key: "ArrowDown" });
    expect(document.activeElement?.textContent).toBe("a.txt");
    fireEvent.keyDown(document.activeElement ?? wrap, { key: "ArrowDown" });
    expect(document.activeElement?.textContent).toBe("b.txt");
    fireEvent.keyDown(document.activeElement ?? wrap, { key: "Home" });
    expect(document.activeElement?.textContent).toBe("a.txt");
  });

  it("opens a row once when Enter lands on its own name", async () => {
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [entry({ name: "report.docx" })] }));
    daemon.apiBlob.mockResolvedValue(new Blob(["hello"]));

    renderWithQuery(<Files />);
    const wrap = (await screen.findByText("report.docx")).closest(".fi-table-wrap");
    if (!(wrap instanceof HTMLElement)) throw new Error("table wrap not found");
    fireEvent.keyDown(wrap, { key: "ArrowDown" });

    const name = document.activeElement;
    if (!(name instanceof HTMLElement)) throw new Error("no focused row");
    // What a browser does: the keydown bubbles, then the button clicks itself.
    fireEvent.keyDown(name, { key: "Enter" });
    fireEvent.click(name);

    await waitFor(() => expect(opener.openPath).toHaveBeenCalledTimes(1));
  });

  it("selects with Space and extends with Shift+ArrowDown", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({ list: () => [entry({ name: "a.txt" }), entry({ name: "b.txt" }), entry({ name: "c.txt" })] }),
    );

    renderWithQuery(<Files />);
    await screen.findByText("a.txt");
    const wrap = document.querySelector(".fi-table-wrap");
    if (!(wrap instanceof HTMLElement)) throw new Error("table wrap not found");

    fireEvent.keyDown(wrap, { key: "ArrowDown" });
    fireEvent.keyDown(document.activeElement ?? wrap, { key: " " });
    fireEvent.keyDown(document.activeElement ?? wrap, { key: "ArrowDown", shiftKey: true });

    expect(await screen.findByText("2 selected")).toBeDefined();
    const all = screen.getByRole("checkbox", { name: "Select all" }) as HTMLInputElement;
    expect(all.indeterminate).toBe(true);
  });

  it("opens the row menu from the keyboard and hands focus back on Escape", async () => {
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [entry({ name: "report.docx" })] }));

    renderWithQuery(<Files />);
    await screen.findByText("report.docx");
    const wrap = document.querySelector(".fi-table-wrap");
    if (!(wrap instanceof HTMLElement)) throw new Error("table wrap not found");

    fireEvent.keyDown(wrap, { key: "ArrowDown" });
    const name = document.activeElement;
    fireEvent.keyDown(name ?? wrap, { key: "F10", shiftKey: true });

    const menu = await screen.findByRole("menu", { name: "Actions for report.docx" });
    expect(document.activeElement).toBe(within(menu).getByRole("menuitem", { name: "Open in Windows" }));
    fireEvent.keyDown(document.activeElement ?? menu, { key: "ArrowDown" });
    expect(document.activeElement).toBe(within(menu).getByRole("menuitem", { name: "Download a copy" }));

    fireEvent.keyDown(document.activeElement ?? menu, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
    expect(document.activeElement).toBe(name);
  });

  it("keeps a failed move inside its own form, and clears it on Cancel", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) => (path === "" ? [entry({ name: "archive", is_dir: true, size_bytes: 0 }), entry({ name: "report.docx" })] : []),
        onMove: () => {
          throw new Error("offline");
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select report.docx" }));
    fireEvent.click(screen.getByRole("button", { name: "Move…" }));
    const picker = screen.getByRole("navigation", { name: "Destination folder" });
    fireEvent.click(await within(picker).findByRole("button", { name: "archive" }));
    fireEvent.click(screen.getByRole("button", { name: "Move" }));
    expect(await screen.findByText(/that move did not go through/)).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByText(/did not go through/)).toBeNull());
  });

  it("gives the listing one Tab stop, not two per row", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({ list: () => [entry({ name: "a.txt" }), entry({ name: "b.txt" }), entry({ name: "c.txt" })] }),
    );

    renderWithQuery(<Files />);
    const table = await screen.findByRole("table");
    await within(table).findByText("c.txt");
    const rowBoxes = within(table)
      .getAllByRole("checkbox")
      .filter((box) => box.getAttribute("aria-label") !== "Select all");
    expect(rowBoxes.every((box) => box.tabIndex === -1)).toBe(true);
    const names = table.querySelectorAll<HTMLElement>("[data-row-open]");
    expect([...names].filter((name) => name.tabIndex === 0)).toHaveLength(1);
    // Named, and described by the keys: on a role-less wrapper ARIA drops both.
    const listing = screen.getByRole("group", { name: "Folder contents" });
    expect(listing.getAttribute("aria-describedby")).toBe(listing.querySelector(".fi-keys")?.id);
  });

  it("walks the folder tree with the arrows, and Enter goes to the folder", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) =>
          path === "" ? [entry({ name: "archive", is_dir: true, size_bytes: 0 }), entry({ name: "invoices", is_dir: true, size_bytes: 0 })] : [],
      }),
    );

    renderWithQuery(<Files />);
    const tree = screen.getByRole("tree", { name: "Folders" });
    const root = within(tree).getByRole("treeitem", { name: "files" });
    expect(root.tabIndex).toBe(0);
    await within(tree).findByRole("treeitem", { name: "invoices" });

    root.focus();
    fireEvent.keyDown(root, { key: "ArrowDown" });
    expect(document.activeElement).toBe(within(tree).getByRole("treeitem", { name: "archive" }));
    fireEvent.keyDown(document.activeElement ?? tree, { key: "End" });
    const invoices = within(tree).getByRole("treeitem", { name: "invoices" });
    expect(document.activeElement).toBe(invoices);

    fireEvent.keyDown(invoices, { key: "Enter" });
    expect(await screen.findByText("/ invoices")).toBeDefined();
  });

  it("says a refused rename under the field it came from", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "report.docx" })],
        onMove: () => {
          throw new Error("offline");
        },
      }),
    );

    renderWithQuery(<Files />);
    const row = (await screen.findByText("report.docx")).closest("tr");
    if (row === null) throw new Error("row not found");
    fireEvent.contextMenu(row);
    fireEvent.click(await screen.findByRole("menuitem", { name: "Rename" }));
    const field = within(row).getByLabelText("New name");
    fireEvent.change(field, { target: { value: "final.docx" } });
    fireEvent.click(within(row).getByRole("button", { name: "Rename" }));

    expect(await within(row).findByText(/that rename did not go through/)).toBeDefined();
    expect(field.getAttribute("aria-invalid")).toBe("true");
  });

  it("opens the tree down to the folder on screen", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) =>
          path === ""
            ? [entry({ name: "invoices", is_dir: true, size_bytes: 0 })]
            : path === "invoices"
              ? [entry({ name: "2026", is_dir: true, size_bytes: 0 })]
              : [],
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(within(await screen.findByRole("table")).getByText("invoices/"));
    fireEvent.click(await screen.findByText("2026/"));

    // Two levels down, reached through the table: the branch above it opened by itself.
    const tree = screen.getByRole("navigation", { name: "Folders" });
    expect(await within(tree).findByText("2026")).toBeDefined();
    expect(within(tree).getByRole("button", { name: "Collapse invoices" })).toBeDefined();
  });
});

/* -------------------------------------------------------------- os drop -- */

describe("Files — the OS drop", () => {
  it("reports every file an OS drop leaves behind, never a silent partial upload", async () => {
    let dropHandler: ((event: { payload: Dropped }) => void) | undefined;
    mockListen.mockImplementation(async (eventName, handler) => {
      if (eventName === "files://dropped") {
        dropHandler = handler as unknown as (event: { payload: Dropped }) => void;
      }
      return () => {};
    });

    const uploaded: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [],
        onUpload: (folder, filename) => {
          uploaded.push(filename);
          return { filename, folder };
        },
      }),
    );
    mockInvoke.mockImplementation(async (cmd, args) => {
      if (cmd === "read_dropped") {
        const p = (args as { path: string }).path;
        if (p === "/tmp/ok.txt") return new ArrayBuffer(4);
        // The exact wording `drop.rs` gives a stale path.
        throw "that file was not dropped on this window";
      }
      return undefined;
    });

    renderWithQuery(<Files />);
    await screen.findByText("Nothing has been filed yet");
    await waitFor(() => expect(dropHandler).toBeDefined());

    await dropHandler?.({
      payload: {
        truncated: true,
        files: [
          { path: "/tmp/ok.txt", folder: "", name: "ok.txt", size: 4 },
          { path: "/tmp/stale.txt", folder: "", name: "stale.txt", size: 4 },
        ],
      },
    });

    await waitFor(() => expect(uploaded).toEqual(["ok.txt"]));
    expect(await screen.findByText(/stale\.txt/)).toBeDefined();
    expect(screen.getByText(/cut short/)).toBeDefined();
  });
});

/* -------------------------------------------------------------- search -- */

describe("Files — searching", () => {
  it("debounces the search box before asking the daemon", async () => {
    let searchCalls = 0;
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path.startsWith("/files/search")) {
        searchCalls += 1;
        return { hits: [], truncated: false };
      }
      if (path.startsWith("/files?path=")) return [];
      return [];
    });

    renderWithQuery(<Files />);
    const input = await screen.findByLabelText("Search files by name");

    fireEvent.change(input, { target: { value: "r" } });
    fireEvent.change(input, { target: { value: "re" } });
    fireEvent.change(input, { target: { value: "rep" } });
    expect(searchCalls).toBe(0);

    await new Promise((resolve) => setTimeout(resolve, 300));
    await waitFor(() => expect(searchCalls).toBe(1));
  });

  const hit: Hit = { path: "invoices/march.pdf", name: "march.pdf", is_dir: false, size_bytes: 10, modified: null };

  it("shows a hit in its folder with the file picked", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) => (path === "invoices" ? [entry({ name: "april.pdf" }), entry({ name: "march.pdf" })] : []),
        search: () => [hit],
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.change(await screen.findByLabelText("Search files by name"), { target: { value: "march" } });
    fireEvent.click(await screen.findByRole("button", { name: "Show march.pdf in its folder" }));

    const box = await screen.findByRole("checkbox", { name: "Select march.pdf" });
    await waitFor(() => expect((box as HTMLInputElement).checked).toBe(true));
    expect((screen.getByRole("checkbox", { name: "Select april.pdf" }) as HTMLInputElement).checked).toBe(false);
    await waitFor(() => expect(document.activeElement?.getAttribute("aria-label")).toBe("march.pdf, selected"));
  });

  it("offers the row menu on a hit, and deletes the hit itself", async () => {
    const deleted: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        search: () => [hit],
        onDelete: (path) => {
          deleted.push(path);
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.change(await screen.findByLabelText("Search files by name"), { target: { value: "march" } });
    fireEvent.contextMenu(await screen.findByText("invoices/march.pdf"));

    const menu = await screen.findByRole("menu");
    // No Rename: its field lives in a row of the table, which is not on screen.
    expect(within(menu).queryByRole("menuitem", { name: /Rename/ })).toBeNull();
    fireEvent.click(within(menu).getByRole("menuitem", { name: /^Delete/ }));
    await waitFor(() => expect(deleted).toEqual(["invoices/march.pdf"]));
  });

  it("goes to the search on Ctrl+F", async () => {
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [entry({ name: "a.txt" })] }));
    renderWithQuery(<Files />);
    const input = await screen.findByLabelText("Search files by name");
    fireEvent.keyDown(document.body, { key: "f", ctrlKey: true });
    expect(document.activeElement).toBe(input);
  });
});

/* ------------------------------------------------------ what a row says -- */

describe("Files — the selection shows and is said", () => {
  it("marks a picked row, and its name says so to whoever is arrowing down", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({ list: () => [entry({ name: "a.txt" }), entry({ name: "docs", is_dir: true, size_bytes: 0 })] }),
    );

    renderWithQuery(<Files />);
    const checkbox = await screen.findByRole("checkbox", { name: "Select a.txt" });
    expect(screen.getByRole("button", { name: "a.txt" })).toBeDefined();
    expect(screen.getByRole("button", { name: "docs, folder" })).toBeDefined();

    fireEvent.click(checkbox);
    expect(checkbox.closest("tr")?.classList.contains("fi-row-selected")).toBe(true);
    expect(screen.getByRole("button", { name: "a.txt, selected" })).toBeDefined();
    expect(screen.getByRole("button", { name: "docs, folder, not selected" })).toBeDefined();
  });

  it("puts focus on the next row once a delete has landed", async () => {
    let names = ["a.txt", "b.txt", "c.txt"];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => names.map((name) => entry({ name })),
        onDelete: (path) => {
          names = names.filter((name) => name !== path);
        },
      }),
    );

    renderWithQuery(<Files />);
    await screen.findByText("b.txt");
    const wrap = document.querySelector(".fi-table-wrap");
    if (!(wrap instanceof HTMLElement)) throw new Error("table wrap not found");
    fireEvent.keyDown(wrap, { key: "ArrowDown" });
    fireEvent.keyDown(document.activeElement ?? wrap, { key: "ArrowDown" });
    expect(document.activeElement?.textContent).toBe("b.txt");

    fireEvent.keyDown(document.activeElement ?? wrap, { key: "Delete" });
    await waitFor(() => expect(document.activeElement?.textContent).toBe("c.txt"));
  });

  it("takes the last delete back on Ctrl+Z", async () => {
    const restored: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "report.docx" })],
        onRestore: (id) => {
          restored.push(id);
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select report.docx" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    await screen.findByRole("button", { name: "Undo" });

    fireEvent.keyDown(document.body, { key: "z", ctrlKey: true });
    await waitFor(() => expect(restored).toEqual(["t-report.docx"]));
  });
});

/* ---------------------------------------------------- a name already there -- */

describe("Files — an upload whose name is taken", () => {
  function pick(name: string) {
    fireEvent.change(screen.getByLabelText("Upload"), {
      target: { files: [new File([new Uint8Array([1])], name, { type: "text/plain" })] },
    });
  }

  it("asks first, and Replace moves the old one to the trash before sending", async () => {
    const calls: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "report.docx" })],
        onDelete: (path) => {
          calls.push(`delete ${path}`);
        },
        onUpload: (folder, filename) => {
          calls.push(`upload ${filename}`);
          return { filename, folder };
        },
      }),
    );

    renderWithQuery(<Files />);
    await screen.findByText("report.docx");
    pick("report.docx");

    const note = await screen.findByRole("group", { name: /already in/ });
    expect(calls).toEqual([]);
    fireEvent.click(within(note).getByRole("button", { name: "Replace" }));

    await waitFor(() => expect(calls).toEqual(["delete report.docx", "upload report.docx"]));
    expect(await screen.findByText(/The older copy is in Recently/)).toBeDefined();
  });

  it("lists five clashing names and counts the rest", async () => {
    const names = Array.from({ length: 8 }, (_, index) => `n${String(index)}.txt`);
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => names.map((name) => entry({ name })) }));

    renderWithQuery(<Files />);
    await screen.findByText("n7.txt");
    fireEvent.change(screen.getByLabelText("Upload"), {
      target: { files: names.map((name) => new File([new Uint8Array([1])], name, { type: "text/plain" })) },
    });

    const note = await screen.findByRole("group", { name: /already in/ });
    expect(within(note).getAllByRole("listitem")).toHaveLength(6);
    expect(within(note).getByText("and 3 more")).toBeDefined();
  });

  it("lands on Keep both, and Escape cancels the whole upload", async () => {
    const uploaded: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "report.docx" })],
        onUpload: (folder, filename) => {
          uploaded.push(filename);
          return { filename, folder };
        },
      }),
    );

    renderWithQuery(<Files />);
    await screen.findByText("report.docx");
    pick("report.docx");

    const note = await screen.findByRole("group", { name: /already in/ });
    await waitFor(() => expect(document.activeElement).toBe(within(note).getByRole("button", { name: "Keep both" })));
    fireEvent.keyDown(document.activeElement ?? note, { key: "Escape" });

    await waitFor(() => expect(screen.queryByRole("group", { name: /already in/ })).toBeNull());
    expect(uploaded).toEqual([]);
  });

  it("says which name the daemon stored when both are kept", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "report.docx" })],
        onUpload: (folder) => ({ filename: "report (2).docx", folder }),
      }),
    );

    renderWithQuery(<Files />);
    await screen.findByText("report.docx");
    pick("report.docx");

    fireEvent.click(await screen.findByRole("button", { name: "Keep both" }));
    expect(await screen.findByText("report (2).docx")).toBeDefined();
    expect(screen.getByText(/renamed from report\.docx/)).toBeDefined();
  });

  it("sends nothing for a skipped name, and says it was skipped", async () => {
    const uploaded: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "report.docx" })],
        onUpload: (folder, filename) => {
          uploaded.push(filename);
          return { filename, folder };
        },
      }),
    );

    renderWithQuery(<Files />);
    await screen.findByText("report.docx");
    pick("report.docx");

    fireEvent.click(await screen.findByRole("button", { name: "Skip it" }));
    expect(await screen.findByText(/Skipped, already there: report\.docx/)).toBeDefined();
    expect(uploaded).toEqual([]);
  });

  it("offers to upload the rest when only some names are taken", async () => {
    const uploaded: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "report.docx" })],
        onUpload: (folder, filename) => {
          uploaded.push(filename);
          return { filename, folder };
        },
      }),
    );

    renderWithQuery(<Files />);
    await screen.findByText("report.docx");
    fireEvent.change(screen.getByLabelText("Upload"), {
      target: {
        files: [
          new File([new Uint8Array([1])], "report.docx", { type: "text/plain" }),
          new File([new Uint8Array([1])], "notes.md", { type: "text/plain" }),
        ],
      },
    });

    fireEvent.click(await screen.findByRole("button", { name: "Upload the rest" }));
    await waitFor(() => expect(uploaded).toEqual(["notes.md"]));
  });
});

/* ------------------------------------------------ undo, and picking rows -- */

describe("Files — every move can be taken back", () => {
  it("closes Move when the page goes to another folder, so it never moves a file from there", async () => {
    const moved: [string, string][] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) =>
          path === ""
            ? [entry({ name: "invoices", is_dir: true, size_bytes: 0 }), entry({ name: "notes.md" })]
            : [entry({ name: "notes.md" })],
        onMove: (from, to) => {
          moved.push([from, to]);
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select notes.md" }));
    fireEvent.click(screen.getByRole("button", { name: "Move…" }));
    expect(screen.getByRole("navigation", { name: "Destination folder" })).toBeDefined();

    const side = screen.getByRole("tree", { name: "Folders" });
    fireEvent.click(await within(side).findByRole("button", { name: "invoices" }));
    expect(await screen.findByText("/ invoices")).toBeDefined();
    expect(screen.queryByRole("navigation", { name: "Destination folder" })).toBeNull();
    expect(moved).toEqual([]);
  });

  it("carries a move back where it came from on Undo", async () => {
    const moved: [string, string][] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) => (path === "" ? [entry({ name: "archive", is_dir: true, size_bytes: 0 }), entry({ name: "a.txt" })] : []),
        onMove: (from, to) => {
          moved.push([from, to]);
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select a.txt" }));
    fireEvent.click(screen.getByRole("button", { name: "Move…" }));
    const picker = screen.getByRole("navigation", { name: "Destination folder" });
    fireEvent.click(await within(picker).findByRole("button", { name: "archive" }));
    fireEvent.click(screen.getByRole("button", { name: "Move" }));

    expect(await screen.findByText("files/archive/")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() =>
      expect(moved).toEqual([
        ["a.txt", "archive/a.txt"],
        ["archive/a.txt", "a.txt"],
      ]),
    );
    await waitFor(() => expect(screen.queryByRole("button", { name: "Undo" })).toBeNull());
  });

  it("takes a rename back on Ctrl+Z", async () => {
    const moved: [string, string][] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "report.docx" })],
        onMove: (from, to) => {
          moved.push([from, to]);
        },
      }),
    );

    renderWithQuery(<Files />);
    const row = (await screen.findByText("report.docx")).closest("tr");
    if (row === null) throw new Error("row not found");
    fireEvent.contextMenu(row);
    fireEvent.click(await screen.findByRole("menuitem", { name: "Rename" }));
    fireEvent.change(within(row).getByLabelText("New name"), { target: { value: "final.docx" } });
    fireEvent.click(within(row).getByRole("button", { name: "Rename" }));

    // The right-click picked the row, so the undo stands in the selection bar.
    expect(await screen.findByRole("button", { name: "Undo: renamed report.docx to final.docx" })).toBeDefined();
    fireEvent.keyDown(document, { key: "z", ctrlKey: true });
    await waitFor(() =>
      expect(moved).toEqual([
        ["report.docx", "final.docx"],
        ["final.docx", "report.docx"],
      ]),
    );
  });

  it("keeps Undo within reach while rows are picked", async () => {
    const restored: string[] = [];
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [entry({ name: "a.txt" }), entry({ name: "b.txt" })],
        onRestore: (id) => {
          restored.push(id);
        },
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select a.txt" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    await screen.findByRole("button", { name: "Undo" });

    fireEvent.click(screen.getByRole("checkbox", { name: "Select b.txt" }));
    fireEvent.click(screen.getByRole("button", { name: "Undo: moved a.txt to Recently deleted" }));
    await waitFor(() => expect(restored).toEqual(["t-a.txt"]));
  });
});

describe("Files — rows pick the way Explorer's do", () => {
  it("picks a row on a click, adds with Ctrl, and takes the run with Shift", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => ["a.txt", "b.txt", "c.txt", "d.txt"].map((name) => entry({ name })),
      }),
    );

    renderWithQuery(<Files />);
    const box = (name: string) => screen.getByRole("checkbox", { name: `Select ${name}` }) as HTMLInputElement;
    const row = (name: string) => {
      const found = screen.getByText(name).closest("tr");
      if (found === null) throw new Error("row not found");
      return found;
    };
    await screen.findByText("d.txt");

    // On the name: one click on a file picks its row and opens nothing.
    fireEvent.click(screen.getByText("a.txt"), { detail: 1 });
    expect(box("a.txt").checked).toBe(true);
    expect(opener.openPath).not.toHaveBeenCalled();

    fireEvent.click(row("c.txt"), { detail: 1, ctrlKey: true });
    expect([box("a.txt"), box("c.txt")].every((b) => b.checked)).toBe(true);

    fireEvent.click(row("d.txt"), { detail: 1, shiftKey: true });
    expect(box("a.txt").checked).toBe(false);
    expect([box("c.txt"), box("d.txt")].every((b) => b.checked)).toBe(true);

    fireEvent.click(row("b.txt"), { detail: 1 });
    expect(["a.txt", "c.txt", "d.txt"].map((name) => box(name).checked)).toEqual([false, false, false]);
    expect(box("b.txt").checked).toBe(true);
  });

  it("adds a folder with Ctrl on its name rather than going into it", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) =>
          path === "" ? [entry({ name: "invoices", is_dir: true, size_bytes: 0 }), entry({ name: "notes.md" })] : [],
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select notes.md" }));
    fireEvent.click(within(screen.getByRole("table")).getByText("invoices/"), { detail: 1, ctrlKey: true });

    expect(screen.queryByText("/ invoices")).toBeNull();
    const box = (name: string) => screen.getByRole("checkbox", { name: `Select ${name}` }) as HTMLInputElement;
    expect([box("notes.md"), box("invoices")].every((b) => b.checked)).toBe(true);
  });

  it("lets go of the undo once the page goes to another folder", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: (path) =>
          path === "" ? [entry({ name: "invoices", is_dir: true, size_bytes: 0 }), entry({ name: "notes.md" })] : [],
      }),
    );

    renderWithQuery(<Files />);
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select notes.md" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect(await screen.findByRole("button", { name: "Undo" })).toBeDefined();

    const side = screen.getByRole("tree", { name: "Folders" });
    fireEvent.click(await within(side).findByRole("button", { name: "invoices" }));
    expect(await screen.findByText("/ invoices")).toBeDefined();
    expect(screen.queryByRole("button", { name: /^Undo/ })).toBeNull();
  });

  it("offers only the selection's verbs inside it, and picks the row outside it", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({ list: () => ["a.txt", "b.txt", "c.txt"].map((name) => entry({ name })) }),
    );

    renderWithQuery(<Files />);
    const box = (name: string) => screen.getByRole("checkbox", { name: `Select ${name}` }) as HTMLInputElement;
    const row = (name: string) => {
      const found = screen.getByText(name).closest("tr");
      if (found === null) throw new Error("row not found");
      return found;
    };
    fireEvent.click(await screen.findByRole("checkbox", { name: "Select a.txt" }));
    fireEvent.click(box("b.txt"));

    fireEvent.contextMenu(row("a.txt"));
    const menu = await screen.findByRole("menu");
    expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "Copy 2 full paths",
      "Move 2…",
      "Delete 2Del",
    ]);
    fireEvent.keyDown(menu, { key: "Escape" });

    fireEvent.contextMenu(row("c.txt"));
    expect(await within(await screen.findByRole("menu")).findByRole("menuitem", { name: "Rename" })).toBeDefined();
    expect(["a.txt", "b.txt", "c.txt"].map((name) => box(name).checked)).toEqual([false, false, true]);
  });
});

describe("Files — the upload report keeps to its size", () => {
  function pickMany(names: string[]) {
    fireEvent.change(screen.getByLabelText("Upload"), {
      target: { files: names.map((name) => new File([new Uint8Array([1])], name, { type: "text/plain" })) },
    });
  }

  it("says a clean upload in one passing line, not a note to dismiss", async () => {
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [] }));

    renderWithQuery(<Files />);
    await screen.findByText("Nothing has been filed yet");
    pickMany(["report.docx"]);

    expect(await screen.findByText("uploaded report.docx")).toBeDefined();
    expect(screen.queryByText(/was uploaded/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Dismiss" })).toBeNull();
  });

  it("lists five stored names and folds the rest behind a count", async () => {
    daemon.apiFetch.mockImplementation(
      makeFilesDaemon({
        list: () => [],
        onUpload: (folder, filename) => ({ filename: `renamed-${filename}`, folder }),
      }),
    );

    renderWithQuery(<Files />);
    await screen.findByText("Nothing has been filed yet");
    pickMany(["1.txt", "2.txt", "3.txt", "4.txt", "5.txt", "6.txt", "7.txt"]);

    const more = await screen.findByText("and 2 more");
    expect(more.closest("details")).not.toBeNull();
    expect(screen.getByText("renamed-1.txt").closest("details")).toBeNull();
    expect(screen.getByText("renamed-7.txt").closest("details")).not.toBeNull();
  });
});
