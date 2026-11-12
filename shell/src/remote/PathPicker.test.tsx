import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const { apiFetch, fetchSession } = vi.hoisted(() => ({
  apiFetch: vi.fn(),
  fetchSession: vi.fn(),
}));

vi.mock("../data/client", () => ({ apiFetch }));
vi.mock("./session", () => ({ fetchSession }));

import { PathPicker } from "./PathPicker";

function ownerSession() {
  fetchSession.mockReset();
  apiFetch.mockReset();
  fetchSession.mockResolvedValue({
    kind: "ok",
    session: { owner: true },
  });
}

describe("PathPicker", () => {
  it("new_project_can_pick_a_tower_folder_remotely", async () => {
    ownerSession();
    apiFetch
      .mockResolvedValueOnce({ path: "C:\\", parent: "", dirs: ["Projects"], truncated: false })
      .mockResolvedValueOnce({ path: "C:\\Projects", parent: "C:\\", dirs: [], truncated: false });
    const selected = vi.fn();

    render(<PathPicker open onOpenChange={vi.fn()} onSelect={selected} />);

    expect(await screen.findByText("C:\\")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Projects" }));
    await waitFor(() => expect(apiFetch).toHaveBeenLastCalledWith("/fs/dirs?path=C%3A%5CProjects"));
    expect(await screen.findByText("C:\\Projects")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Select" }));

    expect(selected).toHaveBeenCalledWith("C:\\Projects");
  });

  it("picker_shows_a_sentence_to_a_device_that_is_not_the_owner", async () => {
    fetchSession.mockReset();
    apiFetch.mockReset();
    fetchSession.mockResolvedValue({
      kind: "ok",
      session: { owner: false },
    });

    render(<PathPicker open onOpenChange={vi.fn()} onSelect={vi.fn()} />);

    expect(await screen.findByText(/needs an owner device/)).toBeTruthy();
    expect(apiFetch).not.toHaveBeenCalled();
  });

  it("picker_shows_error_empty_and_truncated_states", async () => {
    ownerSession();
    apiFetch
      .mockRejectedValueOnce(new Error("The requested folder cannot be read."))
      .mockResolvedValueOnce({ path: "C:\\empty", parent: "C:\\", dirs: [], truncated: false })
      .mockResolvedValueOnce({ path: "C:\\full", parent: "C:\\", dirs: ["alpha"], truncated: true });
    const picker = render(<PathPicker open onOpenChange={vi.fn()} onSelect={vi.fn()} />);

    expect(await screen.findByText("The requested folder cannot be read.")).toBeTruthy();

    picker.rerender(<PathPicker open={false} onOpenChange={vi.fn()} onSelect={vi.fn()} />);
    picker.rerender(<PathPicker open onOpenChange={vi.fn()} onSelect={vi.fn()} initialPath="C:\\empty" />);
    expect(await screen.findByText("No folders are here.")).toBeTruthy();

    picker.rerender(<PathPicker open={false} onOpenChange={vi.fn()} onSelect={vi.fn()} />);
    picker.rerender(<PathPicker open onOpenChange={vi.fn()} onSelect={vi.fn()} initialPath="C:\\full" />);
    expect(await screen.findByText("Only the first 2,000 folders are shown.")).toBeTruthy();
  });

  it("picker_goes_up_to_the_root_from_a_drive", async () => {
    ownerSession();
    apiFetch
      .mockResolvedValueOnce({ path: "C:\\", parent: "", dirs: [], truncated: false })
      .mockResolvedValueOnce({ path: "", parent: null, dirs: ["C:\\"], truncated: false });

    render(<PathPicker open onOpenChange={vi.fn()} onSelect={vi.fn()} initialPath="C:\\" />);

    expect(await screen.findByText("C:\\")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Up" }));
    await waitFor(() => expect(apiFetch).toHaveBeenLastCalledWith("/fs/dirs?path="));
  });

  it("picker_falls_back_to_the_root_when_the_first_path_fails", async () => {
    ownerSession();
    apiFetch
      .mockRejectedValueOnce(new Error("The requested path is not a folder."))
      .mockResolvedValueOnce({ path: "", parent: null, dirs: ["C:\\"], truncated: false });

    render(<PathPicker open onOpenChange={vi.fn()} onSelect={vi.fn()} initialPath="C:\\not-a-folder" />);

    expect(await screen.findByText(/That path could not be opened/)).toBeTruthy();
    await waitFor(() => expect(apiFetch).toHaveBeenLastCalledWith("/fs/dirs?path="));
    expect(screen.getByRole("button", { name: "C:\\" })).toBeTruthy();
  });

  it("a_stale_listing_does_not_replace_a_newer_one", async () => {
    ownerSession();
    let resolveOld: (value: { path: string; parent: string; dirs: string[]; truncated: boolean }) => void;
    const oldListing = new Promise<{ path: string; parent: string; dirs: string[]; truncated: boolean }>((resolve) => {
      resolveOld = resolve;
    });
    apiFetch
      .mockReturnValueOnce(oldListing)
      .mockResolvedValueOnce({ path: "C:\\new", parent: "C:\\", dirs: [], truncated: false });
    const picker = render(<PathPicker open onOpenChange={vi.fn()} onSelect={vi.fn()} initialPath="C:\\old" />);

    await waitFor(() => expect(apiFetch).toHaveBeenCalledTimes(1));
    picker.rerender(<PathPicker open={false} onOpenChange={vi.fn()} onSelect={vi.fn()} />);
    picker.rerender(<PathPicker open onOpenChange={vi.fn()} onSelect={vi.fn()} initialPath="C:\\new" />);
    expect(await screen.findByText("C:\\new")).toBeTruthy();

    resolveOld!({ path: "C:\\old", parent: "C:\\", dirs: ["stale"], truncated: false });
    await waitFor(() => expect(screen.getByText("C:\\new")).toBeTruthy());
    expect(screen.queryByRole("button", { name: "stale" })).toBeNull();
  });
});
