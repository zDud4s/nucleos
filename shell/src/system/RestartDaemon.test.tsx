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

function renderPanel(props: { restartDeadlineMs?: number } = {}) {
  return render(
    <QueryClientProvider client={createAppQueryClient()}>
      <RestartDaemon pollMs={10} {...props} />
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

const idle = () => readiness({ draining: false, live_runs: 0, waiting_on: [] });

/** The panel's read on mount sees an idle daemon; `rest` answers every call after it. */
function afterIdleMount(rest: (path: string) => Promise<UpdateReadiness>) {
  let mounted = false;
  daemon.apiFetch.mockImplementation(async (path: string) => {
    if (!mounted && path === "/system/update-readiness") {
      mounted = true;
      return idle();
    }
    return rest(path);
  });
}

const afterDwell = () => new Promise((resolve) => setTimeout(resolve, 350));
const down = () => new ApiUnavailable("transport", "the daemon did not answer");

beforeEach(() => {
  daemon.apiFetch.mockReset();
});

describe("RestartDaemon", () => {
  it("shows what the restart waits on, from the readiness it polls", async () => {
    afterIdleMount(async (path) =>
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
    daemon.apiFetch.mockImplementation(async (path: string) =>
      path === "/system/update-readiness" ? idle() : readiness(),
    );
    renderPanel();

    fireEvent.click(screen.getByRole("button", { name: "Restart now" }));
    await afterDwell();
    expect(daemon.apiFetch).not.toHaveBeenCalledWith(
      "/system/restart?force=true",
      expect.anything(),
    );

    fireEvent.click(screen.getByRole("button", { name: "Really restart — running work is cut" }));
    await waitFor(() => {
      expect(daemon.apiFetch).toHaveBeenCalledWith("/system/restart?force=true", {
        method: "POST",
      });
    });
  });

  it("says plainly that a stop is already waiting, without calling it a restart", async () => {
    afterIdleMount(async (path) => {
      if (path === "/system/restart") {
        throw new ApiRefusal(409, "conflict", "a drain is already under way");
      }
      return readiness();
    });
    renderPanel();

    fireEvent.click(screen.getByRole("button", { name: "Restart when idle" }));

    expect(await screen.findByText(/A stop is already waiting for running work/)).toBeDefined();
    expect(screen.queryByText(/is restarting/)).toBeNull();
  });

  it("shows the wait when remounted during a drain", async () => {
    daemon.apiFetch.mockResolvedValue(readiness());
    renderPanel();

    expect(await screen.findByText(/A stop is already waiting for running work/)).toBeDefined();
    expect(screen.getByText(/Waiting on: run 7 in chat c1/)).toBeDefined();
    expect(
      (screen.getByRole("button", { name: "Restart when idle" }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it("waits for the new daemon after the old one goes and says it is back", async () => {
    let calls = 0;
    afterIdleMount(async (path) => {
      if (path === "/system/restart") return readiness();
      calls += 1;
      if (calls <= 2) throw down();
      return readiness({ pid: 200, version: "0.2.0", live_runs: 0, draining: false, waiting_on: [] });
    });
    renderPanel();

    fireEvent.click(screen.getByRole("button", { name: "Restart when idle" }));

    expect(await screen.findByText(/is back, version 0\.2\.0/)).toBeDefined();
  });

  it("gives up with an honest message when the daemon never comes back", async () => {
    afterIdleMount(async (path) => {
      if (path === "/system/restart") throw new ApiRefusal(409, "conflict", "drain");
      if (path === "/system/update-readiness" && daemon.apiFetch.mock.calls.length < 5) {
        return readiness();
      }
      throw down();
    });
    renderPanel({ restartDeadlineMs: 150 });

    fireEvent.click(screen.getByRole("button", { name: "Restart when idle" }));

    expect(await screen.findByText(/did not come back/)).toBeDefined();
    expect(screen.getByText(/start it again/)).toBeDefined();
  });

  it("says the daemon carries on when the wait timed out", async () => {
    afterIdleMount(async (path) =>
      path === "/system/restart" ? readiness() : readiness({ draining: false }),
    );
    renderPanel();

    fireEvent.click(screen.getByRole("button", { name: "Restart when idle" }));

    expect(await screen.findByText(/did not restart/)).toBeDefined();
  });
});
