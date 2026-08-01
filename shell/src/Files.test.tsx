import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";

import Files from "./Files";
import type { FileEntry } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function entry(overrides: Partial<FileEntry>): FileEntry {
  return { name: "x", is_dir: false, size_bytes: 0, modified: null, ...overrides };
}

/**
 * A daemon holding one folder.
 *
 * `refusals` maps a request — method plus the path part of the URL — to the status it comes back
 * with, because most of what this page has to get right is what it says when the daemon says no.
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
    if (key === "POST /files/upload") {
      return {
        ok: true,
        status: 200,
        // The name the daemon actually stored it under, which is the point of reading the response
        // rather than echoing what was picked.
        json: async () => ({ filename: "guia (2).docx", folder: "" }),
      };
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

describe("the files folder, browsed", () => {
  beforeEach(() => fetchMock.mockReset());
  afterEach(() => fetchMock.mockReset());

  it("walks into a folder and back out by the breadcrumb", async () => {
    folder({
      "": [entry({ name: "BACMAT", is_dir: true }), entry({ name: "guia.docx", size_bytes: 2048 })],
      BACMAT: [entry({ name: "2026", is_dir: true })],
    });

    render(<Files token="t" connection="connected" />);
    await settle();

    expect(screen.getByRole("button", { name: /guia\.docx/ })).toBeTruthy();
    expect(screen.getByText("2.0 kB")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /BACMAT/ }));
    await settle();
    expect(screen.getByRole("button", { name: /2026/ })).toBeTruthy();

    // Back to the root by its own crumb, which is what makes the trail navigable rather than
    // decorative.
    fireEvent.click(screen.getByRole("button", { name: "Files" }));
    await settle();
    expect(screen.getByRole("button", { name: /guia\.docx/ })).toBeTruthy();
  });

  it("creates a folder under the folder being looked at, not under the root", async () => {
    folder({ "": [entry({ name: "BACMAT", is_dir: true })], BACMAT: [] });

    render(<Files token="t" connection="connected" />);
    await settle();
    fireEvent.click(screen.getByRole("button", { name: /BACMAT/ }));
    await settle();

    fireEvent.change(screen.getByPlaceholderText("New folder"), { target: { value: "2026" } });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));
    await settle();

    const [, init] = calls("POST", "/files/folder")[0] ?? [];
    expect(JSON.parse(String(init?.body))).toEqual({ path: "BACMAT/2026" });
  });

  /**
   * The name the daemon stored it under, not the one that was picked.
   *
   * A collision is numbered rather than allowed to overwrite, so echoing the chosen name would tell
   * someone their file is at a path where a different file is.
   */
  it("reports the name an upload actually landed under", async () => {
    folder({ "": [] });

    render(<Files token="t" connection="connected" />);
    await settle();

    const picker = document.querySelector<HTMLInputElement>("input[type=file]");
    expect(picker).not.toBeNull();
    const file = new File(["conteudo"], "guia.docx");
    Object.defineProperty(picker, "files", { value: [file], configurable: true });
    fireEvent.change(picker!);
    await settle();

    expect(screen.getByText("Uploaded guia (2).docx")).toBeTruthy();
    const [url] = calls("POST", "/files/upload")[0] ?? [];
    expect(String(url)).toContain("filename=guia.docx");
  });

  /** A rename is a move, so the box carries the whole path and the request names both ends. */
  it("renames through the same request that moves", async () => {
    folder({ "": [entry({ name: "guia.docx" })] });

    render(<Files token="t" connection="connected" />);
    await settle();
    fireEvent.click(screen.getByRole("button", { name: "Rename" }));

    fireEvent.change(screen.getByRole("textbox", { name: /New path/ }), {
      target: { value: "BACMAT/guia final.docx" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Move" }));
    await settle();

    const [, init] = calls("POST", "/files/move")[0] ?? [];
    expect(JSON.parse(String(init?.body))).toEqual({
      from: "guia.docx",
      to: "BACMAT/guia final.docx",
    });
  });

  it("says a taken name was refused rather than reporting a move that did not happen", async () => {
    folder({ "": [entry({ name: "guia.docx" })] }, { "POST /files/move": 409 });

    render(<Files token="t" connection="connected" />);
    await settle();
    fireEvent.click(screen.getByRole("button", { name: "Rename" }));
    fireEvent.change(screen.getByRole("textbox", { name: /New path/ }), {
      target: { value: "outro.docx" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Move" }));
    await settle();

    expect(
      screen.getByText(/already there\. Nothing was replaced\./),
    ).toBeTruthy();
  });

  /**
   * The two-step the daemon asks for, carried through to the screen.
   *
   * The first delete is refused because the folder has things in it; the button that replaces it
   * has to say so, or the second click is the same uninformed click as the first.
   */
  it("turns a refused folder delete into a question about what is inside it", async () => {
    // `ConfirmButton` discards a second click inside 300 ms, so a confirmation has to wait the way
    // a person would. Fake timers rather than a real sleep, since the same clock drives its
    // four-second disarm.
    vi.useFakeTimers();
    try {
      folder({ "": [entry({ name: "BACMAT", is_dir: true })] }, { "DELETE /files": 409 });

      render(<Files token="t" connection="connected" />);
      await settle();

      fireEvent.click(screen.getByRole("button", { name: "Delete" }));
      act(() => vi.advanceTimersByTime(400));
      fireEvent.click(screen.getByRole("button", { name: "Delete for good?" }));
      await settle();

      expect(screen.getByText("BACMAT still has things in it.")).toBeTruthy();
      const again = screen.getByRole("button", { name: "Delete anyway" });
      expect(again).toBeTruthy();

      // And the second ask is the one that carries `recursive`. The daemon now accepts it, and the
      // folder it leaves behind is empty.
      folder({ "": [] });
      fireEvent.click(again);
      act(() => vi.advanceTimersByTime(400));
      fireEvent.click(screen.getByRole("button", { name: "Delete it and everything in it?" }));
      await settle();

      const deletes = calls("DELETE", "/files");
      const [url] = deletes[deletes.length - 1] ?? [];
      expect(String(url)).toContain("recursive=true");
    } finally {
      vi.useRealTimers();
    }
  });

  it("explains a daemon with no folder instead of showing an empty one", async () => {
    folder({}, { "GET /files": 503 });

    render(<Files token="t" connection="connected" />);
    await settle();

    expect(screen.getByText(/could not create one at startup/)).toBeTruthy();
  });
});
