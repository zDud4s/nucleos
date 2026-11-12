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

  it("renames a theme: Save is disabled for an empty or unchanged name and posts {name}", async () => {
    daemon.apiFetch.mockResolvedValue([row(3, "proposed")]);
    daemon.apiText.mockResolvedValue("");
    renderWithQuery(<ThemesProposed />);
    fireEvent.click(await screen.findByRole("button", { name: "Rename theme 3" }));
    const input = screen.getByRole("textbox", { name: "New name for theme 3" }) as HTMLInputElement;
    const save = screen.getByRole("button", { name: "Save name of theme 3" }) as HTMLButtonElement;
    expect(input.value).toBe("Theme 3");
    expect(save.disabled).toBe(true);
    fireEvent.change(input, { target: { value: "   " } });
    expect(save.disabled).toBe(true);
    fireEvent.change(input, { target: { value: " Sleep " } });
    expect(save.disabled).toBe(false);
    fireEvent.click(save);
    await waitFor(() =>
      expect(daemon.apiText).toHaveBeenCalledWith(
        "/themes/3/rename",
        expect.objectContaining({ method: "POST", body: JSON.stringify({ name: "Sleep" }) }),
      ),
    );
  });

  it("shows the specific message when the daemon refuses a rename", async () => {
    daemon.apiFetch.mockResolvedValue([row(3, "proposed")]);
    daemon.apiText.mockRejectedValue(new ApiRefusal(422, "bad_name", ""));
    renderWithQuery(<ThemesProposed />);
    fireEvent.click(await screen.findByRole("button", { name: "Rename theme 3" }));
    fireEvent.change(screen.getByRole("textbox", { name: "New name for theme 3" }), { target: { value: "Sleep" } });
    fireEvent.click(screen.getByRole("button", { name: "Save name of theme 3" }));
    expect(await screen.findByText(/it must be 1 to 80 characters/)).toBeTruthy();
  });

  it("says the daemon is not responding when a rename fails without an answer", async () => {
    daemon.apiFetch.mockResolvedValue([row(3, "proposed")]);
    daemon.apiText.mockRejectedValue(new TypeError("network down"));
    renderWithQuery(<ThemesProposed />);
    fireEvent.click(await screen.findByRole("button", { name: "Rename theme 3" }));
    fireEvent.change(screen.getByRole("textbox", { name: "New name for theme 3" }), { target: { value: "Sleep" } });
    fireEvent.click(screen.getByRole("button", { name: "Save name of theme 3" }));
    expect(await screen.findByText(/is not responding: the name was not changed/)).toBeTruthy();
    expect(screen.queryByText(/it must be 1 to 80 characters/)).toBeNull();
  });

  it("cancels a rename on Escape without posting", async () => {
    daemon.apiFetch.mockResolvedValue([row(3, "proposed")]);
    renderWithQuery(<ThemesProposed />);
    fireEvent.click(await screen.findByRole("button", { name: "Rename theme 3" }));
    fireEvent.keyDown(screen.getByRole("textbox", { name: "New name for theme 3" }), { key: "Escape" });
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(daemon.apiText).not.toHaveBeenCalled();
  });

  it("merges into another theme: the select excludes the theme itself and posts {into}", async () => {
    daemon.apiFetch.mockResolvedValue([row(3, "proposed"), row(4, "proposed")]);
    daemon.apiText.mockResolvedValue("");
    renderWithQuery(<ThemesProposed />);
    fireEvent.click(await screen.findByRole("button", { name: "Merge theme 3" }));
    const select = screen.getByRole("combobox", { name: "Merge theme 3 into" }) as HTMLSelectElement;
    const values = Array.from(select.options).map((o) => o.value);
    expect(values).toEqual(["4"]);
    fireEvent.click(screen.getByRole("button", { name: "Confirm merging theme 3" }));
    await waitFor(() =>
      expect(daemon.apiText).toHaveBeenCalledWith(
        "/themes/3/merge",
        expect.objectContaining({ method: "POST", body: JSON.stringify({ into: 4 }) }),
      ),
    );
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
