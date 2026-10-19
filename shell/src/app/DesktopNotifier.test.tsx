import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, waitFor } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { DesktopNotifier } from "./DesktopNotifier";
import { writeDesktopNotify } from "../data/desktop-notify-pref";
import { keys } from "../data/keys";
import { proposal, renderWithRouter } from "../test/harness";

/** Whether the window is in front, as `document.hasFocus()` answers it. */
let inFront = true;

function setFront(next: boolean) {
  inFront = next;
  act(() => {
    window.dispatchEvent(new Event(next ? "focus" : "blur"));
  });
}

beforeEach(() => {
  localStorage.clear();
  inFront = true;
  vi.spyOn(document, "hasFocus").mockImplementation(() => inFront);
  daemon.apiFetch.mockReset();
  // Every list the notifier reads answers "nothing" — a test then adds what it is about.
  daemon.apiFetch.mockImplementation(async (path: string) =>
    path === "/notifications/policy" ? { families: [], kinds: [] } : [],
  );
});

afterEach(() => {
  vi.restoreAllMocks();
});

/** Mounts the notifier and waits until its lists have answered once, so the next change is news. */
async function mounted(send: (toast: unknown) => void) {
  const view = await renderWithRouter(<DesktopNotifier send={send} />);
  await waitFor(() => {
    expect(view.queryClient.getQueryState(keys.proposals.all)?.status).toBe("success");
  });
  // The seeding reading happens in an effect after the lists settle.
  await act(async () => {});
  return view;
}

describe("the desktop notifier", () => {
  it("sends a toast when a proposal arrives and the window has no focus", async () => {
    const send = vi.fn();
    const view = await mounted(send);

    setFront(false);
    act(() => {
      view.queryClient.setQueryData(keys.proposals.all, [
        proposal({ id: 91, tool_name: "Bash", reasoning: "run the tests" }),
      ]);
    });

    await waitFor(() => expect(send).toHaveBeenCalledTimes(1));
    expect(send).toHaveBeenCalledWith({
      key: "proposal:91",
      title: "Waiting: Bash",
      body: "run the tests",
      to: "/waiting",
    });
  });

  it("sends nothing while the window is in front", async () => {
    const send = vi.fn();
    const view = await mounted(send);

    act(() => {
      view.queryClient.setQueryData(keys.proposals.all, [proposal({ id: 91 })]);
    });
    await act(async () => {});

    expect(send).not.toHaveBeenCalled();
  });

  it("sends nothing when switched off", async () => {
    writeDesktopNotify(false);
    const send = vi.fn();
    const view = await mounted(send);

    setFront(false);
    act(() => {
      view.queryClient.setQueryData(keys.proposals.all, [proposal({ id: 91 })]);
    });
    await act(async () => {});

    expect(send).not.toHaveBeenCalled();
  });

  it("opens where the last toast pointed when the window comes back to the front", async () => {
    const send = vi.fn();
    const view = await mounted(send);

    setFront(false);
    act(() => {
      view.queryClient.setQueryData(keys.proposals.all, [proposal({ id: 91 })]);
    });
    await waitFor(() => expect(send).toHaveBeenCalledTimes(1));
    expect(view.router.state.location.pathname).toBe("/");

    setFront(true);
    await waitFor(() => expect(view.router.state.location.pathname).toBe("/waiting"));
  });

  it("does not take the owner anywhere when the window returns with no toast outstanding", async () => {
    const send = vi.fn();
    const view = await mounted(send);

    setFront(false);
    setFront(true);
    await act(async () => {});

    expect(view.router.state.location.pathname).toBe("/");
  });
});
