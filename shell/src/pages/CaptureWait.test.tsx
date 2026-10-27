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

const STORED = { answer_days: 5, digest_hour: 18 };

describe("CaptureWait", () => {
  it("shows the stored answer window and digest hour", async () => {
    daemon.apiFetch.mockResolvedValue(STORED);
    renderWithQuery(<CaptureWait />);

    const days = (await screen.findByLabelText("Answer window (days)")) as HTMLInputElement;
    const hour = (await screen.findByLabelText("Digest hour")) as HTMLInputElement;
    await waitFor(() => expect(days.value).toBe("5"));
    expect(hour.value).toBe("18");
    expect(daemon.apiFetch).toHaveBeenCalledWith("/config/capture");
  });

  it("posts only the changed field on blur, and nothing when it did not change", async () => {
    daemon.apiFetch.mockImplementation(async (_path: string, init?: RequestInit) =>
      init?.method === "POST" ? { ...STORED, answer_days: 7 } : STORED,
    );
    renderWithQuery(<CaptureWait />);

    const input = (await screen.findByLabelText("Answer window (days)")) as HTMLInputElement;
    await waitFor(() => expect(input.value).toBe("5"));
    fireEvent.change(input, { target: { value: "5" } });
    fireEvent.blur(input);
    expect(daemon.apiFetch).not.toHaveBeenCalledWith("/config/capture", expect.anything());

    fireEvent.change(input, { target: { value: "7" } });
    fireEvent.blur(input);
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/config/capture", {
        method: "POST",
        body: JSON.stringify({ answer_days: 7 }),
      }),
    );
  });

  it("shows the daemon's refusal", async () => {
    daemon.apiFetch.mockImplementation(async (_path: string, init?: RequestInit) => {
      if (init?.method === "POST") throw new ApiRefusal(422, "out_of_range", "out_of_range");
      return STORED;
    });
    renderWithQuery(<CaptureWait />);

    const input = (await screen.findByLabelText("Digest hour")) as HTMLInputElement;
    await waitFor(() => expect(input.value).toBe("18"));
    fireEvent.change(input, { target: { value: "99" } });
    fireEvent.keyDown(input, { key: "Enter" });

    expect(await screen.findByText("out_of_range")).toBeTruthy();
  });
});
