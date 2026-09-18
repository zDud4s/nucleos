import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

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
import type { Dropped, Entry, SavedFile } from "../data/files";
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

/** The dwell `ConfirmButton` needs between arming and confirming — a real gap. */
function afterDwell(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 350));
}

interface FilesDaemonConfig {
  list?: (path: string) => Entry[];
  onDelete?: (path: string, recursive: boolean) => void;
  onMove?: (from: string, to: string) => void;
  onUpload?: (folder: string, filename: string, body: unknown) => SavedFile;
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
      return undefined;
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
      return { hits: [], truncated: false };
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

    expect(await screen.findByText("No files root is configured")).toBeDefined();
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
    expect(screen.getByText("2 items, 2.0 KB")).toBeDefined();
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
    await screen.findByText("This folder is empty");

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
        list: () => [entry({ name: "report.docx" })],
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
    await screen.findByText("This folder is empty");

    const input = screen.getByLabelText("Upload");
    const file = new File([new Uint8Array([1, 2, 3])], "report.docx", { type: "application/octet-stream" });
    fireEvent.change(input, { target: { files: [file] } });

    await waitFor(() => expect(uploaded).toHaveLength(1));
    expect(uploaded[0]?.body).not.toBeInstanceOf(FormData);
    expect(uploaded[0]?.body).toBeInstanceOf(ArrayBuffer);
    expect(await screen.findByText(/saved as: report \(2\)\.docx/)).toBeDefined();
  });

  it("keeps the upload control as one labelled, styled control and shows chosen names", async () => {
    daemon.apiFetch.mockImplementation(makeFilesDaemon({ list: () => [] }));

    renderWithQuery(<Files />);
    await screen.findByText("This folder is empty");

    const input = screen.getByLabelText("Upload");
    expect(input.classList.contains("sr-only")).toBe(true);
    expect(input.getAttribute("aria-label")).toBeNull();
    expect(input.closest("label")?.textContent).toBe("Upload");

    const first = new File([new Uint8Array([1])], "first.txt", { type: "text/plain" });
    const second = new File([new Uint8Array([2])], "second.txt", { type: "text/plain" });
    fireEvent.change(input, { target: { files: [first, second] } });

    expect(await screen.findByText("first.txt, second.txt")).toBeDefined();
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
    fireEvent.click(await screen.findByRole("menuitem", { name: "Download" }));

    await waitFor(() => expect(daemon.apiBlob).toHaveBeenCalledWith("/files/download?path=report.docx"));
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/files/download?path=report.docx", expect.anything());
    expect(daemon.apiText).not.toHaveBeenCalledWith("/files/download?path=report.docx", expect.anything());
  });
});

/* --------------------------------------------------------------- delete -- */

describe("Files — deleting", () => {
  it("arms the Delete key rather than deleting on the first press", async () => {
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

    fireEvent.keyDown(wrap, { key: "Delete" });
    expect(deleted).toHaveLength(0);
    expect(screen.getByText(/press Delete again/)).toBeDefined();

    fireEvent.keyDown(wrap, { key: "Delete" });
    await waitFor(() => expect(deleted).toEqual(["report.docx"]));
  });

  it("offers a recursive confirm only after a plain delete meets a non-empty folder", async () => {
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

    expect(await screen.findByText(/something inside/)).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Delete with everything inside" }));
    await afterDwell();
    fireEvent.click(screen.getByRole("button", { name: "Really delete everything inside" }));

    await waitFor(() => expect(screen.queryByText(/something inside/)).toBeNull());
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
    await screen.findByText("This folder is empty");
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
});
