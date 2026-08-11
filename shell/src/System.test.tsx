import { afterEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";

import System from "./System";
import type { SidecarState } from "./api";

const fetchMock = vi.fn();
vi.stubGlobal("fetch", fetchMock);

function sidecar(overrides: Partial<SidecarState> = {}): SidecarState {
  return {
    name: "email",
    state: "running",
    started_at: "2026-08-09T15:00:00Z",
    last_failure: null,
    last_failure_at: null,
    restarts: 0,
    last_line: null,
    last_line_at: null,
    ...overrides,
  };
}

/** A daemon answering the two reads the health section makes. */
async function show(sidecars: SidecarState[]) {
  fetchMock.mockImplementation(async (url: string) => {
    if (String(url).includes("/sidecars")) {
      return { ok: true, status: 200, json: async () => sidecars };
    }
    // The health readout is not what these tests are about; an empty one renders and moves on.
    return { ok: true, status: 200, json: async () => ({ subsystems: [] }) };
  });
  await act(async () => {
    render(<System token="t" connection="connected" />);
  });
}

afterEach(() => {
  fetchMock.mockReset();
});

/**
 * The panel's job is to answer "is this sidecar working", and for a poller that is not the same
 * question as "is the process alive". `state` and `last_failure` both describe the process: one
 * whose every cycle fails and never exits reads `running`, no failure, indefinitely — which is how
 * a mailbox went eight days unread with the panel showing nothing wrong.
 */
describe("the sidecars panel", () => {
  it("shows what a running sidecar last said", async () => {
    await show([
      sidecar({
        last_line: 'email: poll failed: inbound mailbox "INBOX": daemon returned 500:',
        last_line_at: "2026-08-09T15:25:15Z",
      }),
    ]);

    expect(screen.getByText(/poll failed/)).toBeTruthy();
    // And it is still reported as running, because it is. The line is the evidence that being up
    // and being fine are different things.
    expect(screen.getByText("running")).toBeTruthy();
  });

  /** A sidecar that has said nothing yet must not render an empty row where its words go. */
  it("says nothing when the sidecar has said nothing", async () => {
    await show([sidecar()]);
    expect(document.querySelector(".s-said")).toBeNull();
  });

  it("shows the line for a sidecar that is down as well as one that is up", async () => {
    await show([
      sidecar({
        name: "web",
        state: "down",
        started_at: null,
        last_failure: "exited: exit code: 1",
        last_line: "web-sidecar: search provider is not configured",
      }),
    ]);

    expect(screen.getByText("down")).toBeTruthy();
    expect(screen.getByText(/search provider is not configured/)).toBeTruthy();
    // Both are kept: how it ended and what it said are different facts, and the panel showed only
    // the first one for as long as it existed.
    expect(screen.getByText(/exited: exit code: 1/)).toBeTruthy();
  });
});
