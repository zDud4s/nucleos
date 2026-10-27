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

describe("DistillerSources", () => {
  function answer(sources: { chat: boolean; calendar: boolean }, ingest = { seeded: 3, skipped: 1 }) {
    daemon.apiFetch.mockImplementation(async (path: string, init?: RequestInit) => {
      if (path === "/config/distiller-sources") {
        return init?.method === "POST" ? { ...sources, ...JSON.parse(String(init.body)) } : sources;
      }
      if (path === "/distill/chats/ingest") return ingest;
      if (path === "/projects") return [{ project_id: "alpha" }];
      return { minutes: 120 };
    });
  }

  it("the source switches default on and post only the changed field", async () => {
    answer({ chat: true, calendar: true });
    renderWithQuery(<CaptureWait />);

    const chat = (await screen.findByRole("switch", { name: "Conversations" })) as HTMLInputElement;
    const calendar = screen.getByRole("switch", { name: "Calendar" }) as HTMLInputElement;
    await waitFor(() => expect(chat.checked).toBe(true));
    expect(calendar.checked).toBe(true);

    fireEvent.click(chat);
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/config/distiller-sources", {
        method: "POST",
        body: JSON.stringify({ chat: false }),
      }),
    );
  });

  it("the ingest button posts all or one project and shows the counts", async () => {
    answer({ chat: true, calendar: true });
    renderWithQuery(<CaptureWait />);

    const button = await screen.findByRole("button", { name: "Ingest past conversations" });
    fireEvent.click(button);
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/distill/chats/ingest", {
        method: "POST",
        body: JSON.stringify({}),
      }),
    );
    expect(await screen.findByText("3 conversations queued, 1 already ingested")).toBeTruthy();

    await screen.findByRole("option", { name: "alpha" });
    fireEvent.change(screen.getByLabelText("Project to ingest"), { target: { value: "alpha" } });
    fireEvent.click(button);
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith("/distill/chats/ingest", {
        method: "POST",
        body: JSON.stringify({ project: "alpha" }),
      }),
    );
  });

  it("the ingest button is disabled while conversations are off", async () => {
    answer({ chat: false, calendar: true });
    renderWithQuery(<CaptureWait />);

    expect(await screen.findByText("Turn conversations on to ingest them")).toBeTruthy();
    const button = screen.getByRole("button", { name: "Ingest past conversations" }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(
      screen.getByText("Sends up to 40 exchanges per chat to the distiller model, a few chats a day."),
    ).toBeTruthy();
  });
});
