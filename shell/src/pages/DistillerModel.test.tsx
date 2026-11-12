import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { DistillerModel } from "./DistillerModel";
import { renderWithQuery } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("DistillerModel", () => {
  it("shows the stored choice, cloud when the daemon names none", async () => {
    daemon.apiFetch.mockResolvedValue({ model: "local" });
    const first = renderWithQuery(<DistillerModel />);

    const stored = (await screen.findByLabelText("Distiller model")) as HTMLSelectElement;
    await waitFor(() => expect(stored.value).toBe("local"));
    expect(daemon.apiFetch).toHaveBeenCalledWith("/config/distiller");
    first.unmount();

    // A daemon that names nothing is a daemon that has not been asked to change anything, and
    // the cloud is what the distiller has always used.
    daemon.apiFetch.mockReset();
    daemon.apiFetch.mockResolvedValue(undefined);
    renderWithQuery(<DistillerModel />);

    const unset = (await screen.findByLabelText("Distiller model")) as HTMLSelectElement;
    await waitFor(() => expect(daemon.apiFetch).toHaveBeenCalledWith("/config/distiller"));
    await waitFor(() => expect(unset.value).toBe("cloud"));
  });

  it("choosing another brain posts it to the daemon", async () => {
    daemon.apiFetch.mockImplementation(async (_path: string, init?: RequestInit) =>
      init?.method === "POST" ? { model: "openrouter" } : { model: "cloud" },
    );
    renderWithQuery(<DistillerModel />);

    const select = (await screen.findByLabelText("Distiller model")) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe("cloud"));
    fireEvent.change(select, { target: { value: "openrouter" } });

    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/config/distiller", {
        method: "POST",
        body: JSON.stringify({ model: "openrouter" }),
      }),
    );
  });
});
