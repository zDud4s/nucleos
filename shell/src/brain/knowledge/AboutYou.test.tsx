import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { known } from "./test-helpers";
import { AboutYou } from "./AboutYou";
import { renderWithRouter } from "../../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../../data/client", async (original) => ({
  ...(await original<typeof import("../../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("AboutYou", () => {
  it("lists what is known about the owner and revokes through the revert route", async () => {
    daemon.apiFetch.mockImplementation((path: string, init?: RequestInit) => {
      if (path === "/knowledge")
        return Promise.resolve([
          known({
            id: 7,
            title: "Prefers small diffs",
            about: "owner",
            scope_kind: "machine",
            scope_id: null,
            source: "distiller",
          }),
        ]);
      if (path === "/distill/causes" || path === "/distill/duplicates") return Promise.resolve([]);
      if (init?.method === "POST") return Promise.resolve(undefined);
      return Promise.reject(new Error(`unexpected ${path}`));
    });
    await renderWithRouter(<AboutYou />, { initialPath: "/brain" });
    expect(await screen.findByText("About you")).toBeTruthy();
    expect(await screen.findByText("Prefers small diffs")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Revert" }));
    await new Promise((resolve) => setTimeout(resolve, 350)); // ConfirmButton dwell
    fireEvent.click(screen.getByRole("button", { name: /no longer applies/i }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/knowledge/7/revert", {
        method: "POST",
        body: "{}",
      }),
    );
  });

  it("draws nothing when nothing is known about the owner", async () => {
    daemon.apiFetch.mockImplementation(() => Promise.resolve([]));
    await renderWithRouter(<AboutYou />, { initialPath: "/brain" });
    await waitFor(() => expect(screen.queryByText("About you")).toBeNull());
  });
});
