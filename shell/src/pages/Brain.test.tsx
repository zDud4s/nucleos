import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { Brain } from "./Brain";
import type { OwnerNote } from "../data/owner-notes";
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

function daemonWith(listed: OwnerNote[], matched: OwnerNote[] = []) {
  return (path: string, init?: RequestInit) => {
    if (path.startsWith("/owner-notes/search")) return Promise.resolve(matched);
    if (path.startsWith("/owner-notes?")) return Promise.resolve(listed);
    if (path === "/owner-notes" && init?.method === "POST") return Promise.resolve({ id: 9 });
    return Promise.reject(new Error(`unexpected ${path}`));
  };
}

describe("Brain", () => {
  it("saving the capture box creates a note from the shell", async () => {
    daemon.apiFetch.mockImplementation(daemonWith([note()]));
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

  it("a search shows matching notes only", async () => {
    daemon.apiFetch.mockImplementation(
      daemonWith(
        [note(), note({ id: 2, text: "Unrelated" })],
        [note({ id: 3, text: "Only the match" })],
      ),
    );
    await renderWithRouter(<Brain />, { initialPath: "/brain" });

    expect(await screen.findByText("Unrelated")).toBeTruthy();
    fireEvent.change(screen.getByRole("searchbox", { name: "Search notes" }), {
      target: { value: "match" },
    });

    expect(await screen.findByText("Only the match")).toBeTruthy();
    expect(screen.queryByText("Unrelated")).toBeNull();
  });

  it("a capture stamp focuses the capture box", async () => {
    daemon.apiFetch.mockImplementation(daemonWith([]));
    await renderWithRouter(<Brain />, { initialPath: "/brain?capture=1700000000000" });

    const box = await screen.findByRole("textbox", { name: "Capture a note" });
    await waitFor(() => expect(document.activeElement).toBe(box));
  });
});
