import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const daemon = vi.hoisted(() => ({ apiFetch: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { RestartDaemon } from "./RestartDaemon";
import { createAppQueryClient } from "../app/queryClient";
import { ApiRefusal, ApiUnavailable, type UpdateReadiness } from "../data/client";

function renderPanel() {
  return render(
    <QueryClientProvider client={createAppQueryClient()}>
      <RestartDaemon pollMs={10} />
    </QueryClientProvider>,
  );
}

const readiness = (over: Partial<UpdateReadiness> = {}): UpdateReadiness => ({
  version: "0.1.0",
  live_runs: 1,
  draining: true,
  waiting_on: [{ run_id: 7, chat_id: "c1" }],
  pid: 100,
  ...over,
});

const afterDwell = () => new Promise((resolve) => setTimeout(resolve, 350));

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("RestartDaemon", () => {
  it("shows what the restart waits on, from the readiness it polls", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) =>
      path === "/system/restart"
        ? readiness()
        : readiness({ waiting_on: [{ run_id: 9, chat_id: null }] }),
    );
    renderPanel();

    fireEvent.click(screen.getByRole("button", { name: "Restart when idle" }));

    expect(await screen.findByText(/Waiting on: run 7 in chat c1/)).toBeDefined();
    expect(await screen.findByText(/Waiting on: run 9\./)).toBeDefined();
    expect(daemon.apiFetch).toHaveBeenCalledWith("/system/restart", { method: "POST" });
  });

  it("does not restart now on a plain click, only after the confirmation", async () => {
    daemon.apiFetch.mockResolvedValue(readiness());
    renderPanel();

    fireEvent.click(screen.getByRole("button", { name: "Restart now" }));
    await afterDwell();
    expect(daemon.apiFetch).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Really restart — running work is cut" }));
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/system/restart?force=true", {
        method: "POST",
      });
    });
  });

  it("says plainly that a drain is already under way", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/system/restart") {
        throw new ApiRefusal(409, "conflict", "a drain is already under way");
      }
      return readiness();
    });
    renderPanel();

    fireEvent.click(screen.getByRole("button", { name: "Restart when idle" }));

    expect(await screen.findByText(/already waiting for running work/)).toBeDefined();
  });

  it("waits for the new daemon after the old one goes and says it is back", async () => {
    let calls = 0;
    daemon.apiFetch.mockImplementation(async (path: string) => {
      if (path === "/system/restart") return readiness();
      calls += 1;
      if (calls === 1) throw new ApiUnavailable("transport", "the daemon did not answer");
      if (calls === 2) throw new ApiUnavailable("transport", "the daemon did not answer");
      return readiness({ pid: 200, version: "0.2.0", live_runs: 0, draining: false, waiting_on: [] });
    });
    renderPanel();

    fireEvent.click(screen.getByRole("button", { name: "Restart when idle" }));

    expect(await screen.findByText(/is back, version 0\.2\.0/)).toBeDefined();
  });

  it("says the daemon carries on when the wait timed out", async () => {
    daemon.apiFetch.mockImplementation(async (path: string) =>
      path === "/system/restart" ? readiness() : readiness({ draining: false }),
    );
    renderPanel();

    fireEvent.click(screen.getByRole("button", { name: "Restart when idle" }));

    expect(await screen.findByText(/did not restart/)).toBeDefined();
  });
});
