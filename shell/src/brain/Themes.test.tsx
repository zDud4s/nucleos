import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { ApiRefusal } from "../data/client";
import type { ThemeDetail, ThemeRow } from "../data/themes";
import { renderWithQuery } from "../test/harness";
import { ThemesAccepted } from "./ThemesAccepted";
import { ThemesProposed } from "./ThemesProposed";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

function row(id: number, state: ThemeRow["state"], over: Partial<ThemeRow> = {}): ThemeRow {
  return { id, name: `Theme ${id}`, state, merged_into: null, members: 4, created_at: "t", ...over };
}

function detail(id: number, synthesis: string | null): ThemeDetail {
  return {
    id,
    name: `Theme ${id}`,
    state: "accepted",
    merged_into: null,
    synthesis,
    created_at: "t",
    decided_at: "t",
    members: [],
  };
}

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
});

describe("ThemesProposed", () => {
  it("lists proposed themes with #id and accepts or refuses one", async () => {
    daemon.apiFetch.mockResolvedValue([row(3, "proposed", { name: "Untitled theme" })]);
    daemon.apiText.mockResolvedValue("");
    renderWithQuery(<ThemesProposed />);
    expect(await screen.findByText("Untitled theme")).toBeTruthy();
    expect(screen.getByText("#3")).toBeTruthy();
    expect(daemon.apiFetch).toHaveBeenCalledWith("/themes?state=proposed");
    fireEvent.click(screen.getByRole("button", { name: /accept/i }));
    await waitFor(() => expect(daemon.apiText).toHaveBeenCalledWith("/themes/3/accept", expect.objectContaining({ method: "POST" })));
    fireEvent.click(screen.getByRole("button", { name: /refuse/i }));
    await waitFor(() => expect(daemon.apiText).toHaveBeenCalledWith("/themes/3/refuse", expect.objectContaining({ method: "POST" })));
  });

  it("renders nothing when there are no proposed themes", async () => {
    daemon.apiFetch.mockResolvedValue([]);
    const { container } = renderWithQuery(<ThemesProposed />);
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalled());
    expect(container.querySelector(".themes-proposed")).toBeNull();
  });
});

describe("ThemesAccepted", () => {
  function serve(synthesis: string | null) {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path.startsWith("/themes?state=accepted")) return [row(5, "accepted")];
      if (path === "/themes/5") return detail(5, synthesis);
      if (path === "/themes/5/synthesise") return { synthesis: "Fresh." };
      throw new Error(`unexpected ${path}`);
    });
  }

  it("shows the synthesis of an accepted theme", async () => {
    serve("Sleep keeps coming up.");
    renderWithQuery(<ThemesAccepted />);
    expect(await screen.findByText("Sleep keeps coming up.")).toBeTruthy();
  });

  it("says there is no synthesis yet and lets the owner force one", async () => {
    serve(null);
    renderWithQuery(<ThemesAccepted />);
    expect(await screen.findByText(/No synthesis yet/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /synthesise theme 5 now/i }));
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/themes/5/synthesise", { method: "POST" }),
    );
  });

  it("explains a forced synthesis the daemon refuses within the hour", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path.startsWith("/themes?state=accepted")) return [row(5, "accepted")];
      if (path === "/themes/5") return detail(5, "Old.");
      throw new ApiRefusal(429, "too_many_requests", "");
    });
    renderWithQuery(<ThemesAccepted />);
    fireEvent.click(await screen.findByRole("button", { name: /synthesise theme 5 now/i }));
    expect(await screen.findByText(/less than an hour ago/)).toBeTruthy();
  });
});
