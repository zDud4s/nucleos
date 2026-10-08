import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { ApiRefusal } from "../data/client";
import { CaptureWait } from "./CaptureWait";
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

describe("CaptureWait", () => {
  it("shows the stored number of minutes", async () => {
    daemon.apiFetch.mockResolvedValue({ minutes: 120 });
    renderWithQuery(<CaptureWait />);

    const input = (await screen.findByLabelText("Capture wait (minutes)")) as HTMLInputElement;
    await waitFor(() => expect(input.value).toBe("120"));
    expect(daemon.apiFetch).toHaveBeenCalledWith("/config/capture-wait");
  });

  it("posts the new number on blur, and nothing when it did not change", async () => {
    daemon.apiFetch.mockImplementation(async (_path: string, init?: RequestInit) =>
      init?.method === "POST" ? { minutes: 30 } : { minutes: 120 },
    );
    renderWithQuery(<CaptureWait />);

    const input = (await screen.findByLabelText("Capture wait (minutes)")) as HTMLInputElement;
    await waitFor(() => expect(input.value).toBe("120"));
    fireEvent.change(input, { target: { value: "120" } });
    fireEvent.blur(input);
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/config/capture-wait", expect.anything());

    fireEvent.change(input, { target: { value: "30" } });
    fireEvent.blur(input);
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/config/capture-wait", {
        method: "POST",
        body: JSON.stringify({ minutes: 30 }),
      }),
    );
  });

  it("shows the daemon's refusal", async () => {
    daemon.apiFetch.mockImplementation(async (_path: string, init?: RequestInit) => {
      if (init?.method === "POST") throw new ApiRefusal(422, "out_of_range", "out_of_range");
      return { minutes: 120 };
    });
    renderWithQuery(<CaptureWait />);

    const input = (await screen.findByLabelText("Capture wait (minutes)")) as HTMLInputElement;
    await waitFor(() => expect(input.value).toBe("120"));
    fireEvent.change(input, { target: { value: "99999" } });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(await screen.findByText("out_of_range")).toBeTruthy();
  });
});
