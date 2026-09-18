import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { NAV_ITEMS } from "./nav";
import { daemonFetch, daemonState, renderApp } from "../test/harness";

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();

  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  daemon.apiFetch.mockImplementation(daemonFetch(daemonState()));
  localStorage.clear();
});

/**
 * The whole app, every time, and that is the point of this file.
 *
 * The palette's own rules are unit-tested in `ui/Palette.test.tsx`. What cannot
 * be proven there is the only claim anybody actually cares about — that the
 * chord works on a page that has no palette of its own, and reaches every
 * destination the rail has — and proving it needs the real shell, the real rail
 * and the real router.
 */
const PALETTE = { name: "Go to anything" };
const GO_TO = "Go to anything, Ctrl K";

function chord(modifier: "ctrlKey" | "metaKey") {
  fireEvent.keyDown(window, { key: "k", [modifier]: true });
}

/* ------------------------------------------------------------- A1, A2: the key -- */

describe("AppShell - the chord", () => {
  it("Ctrl+K opens the palette from Home", async () => {
    await renderApp({ initialPath: "/" });
    await screen.findByRole("navigation", { name: "Sections" });

    // Home has no palette of its own and never had one. That is the whole
    // ticket: the fastest path in the app used to work on two screens out of
    // twenty-eight.
    expect(screen.queryByRole("dialog", PALETTE)).toBeNull();

    chord("ctrlKey");
    const palette = await screen.findByRole("dialog", PALETTE);
    expect(within(palette).getByRole("combobox")).toBeDefined();
  });

  it("Cmd+K opens it, and a second press puts it away", async () => {
    await renderApp({ initialPath: "/" });
    await screen.findByRole("navigation", { name: "Sections" });

    // The same fingers on a Mac keyboard.
    chord("metaKey");
    await screen.findByRole("dialog", PALETTE);

    chord("metaKey");
    await waitFor(() => {
      expect(screen.queryByRole("dialog", PALETTE)).toBeNull();
    });
  });
});

/* ------------------------------------------------------- A3, A4: the destinations -- */

describe("AppShell - every destination", () => {
  it("every destination in the rail is in the palette", async () => {
    await renderApp({ initialPath: "/" });
    await screen.findByRole("navigation", { name: "Sections" });

    chord("ctrlKey");
    const palette = await screen.findByRole("dialog", PALETTE);

    // Nineteen, read off the table the rail itself is built from, so a row
    // added to the nav cannot quietly fail to be reachable from here.
    expect(NAV_ITEMS.length).toBeGreaterThan(0);
    for (const item of NAV_ITEMS) {
      expect(within(palette).getByText(item.label)).toBeDefined();
    }
    expect(within(palette).getByText("Go to")).toBeDefined();
  });

  it("choosing Runs takes the router to /runs", async () => {
    const { router } = await renderApp({ initialPath: "/" });
    await screen.findByRole("navigation", { name: "Sections" });
    expect(router.state.location.pathname).toBe("/");

    chord("ctrlKey");
    const palette = await screen.findByRole("dialog", PALETTE);
    fireEvent.click(within(palette).getByText("Runs"));

    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/runs");
    });
    // Closed and cleared on the way, so the palette is never left standing over
    // the page it just opened.
    await waitFor(() => {
      expect(screen.queryByRole("dialog", PALETTE)).toBeNull();
    });
  });
});

/* ---------------------------------------------------- A5, A6: the way out and in -- */

describe("AppShell - the rail's control", () => {
  it("the rail's Go to control opens it without the key", async () => {
    await renderApp({ initialPath: "/" });
    const nav = await screen.findByRole("navigation", { name: "Sections" });

    // In the pinned footer, above the drawer and the stop — reachable at any
    // scroll position of the rail, on every route.
    const trigger = within(nav).getByRole("button", { name: GO_TO });
    expect(trigger.closest(".nav-footer")).not.toBeNull();

    fireEvent.click(trigger);
    await screen.findByRole("dialog", PALETTE);
  });

  it("Escape closes it and gives the focus back", async () => {
    await renderApp({ initialPath: "/" });
    const nav = await screen.findByRole("navigation", { name: "Sections" });

    const trigger = within(nav).getByRole("button", { name: GO_TO });
    trigger.focus();
    fireEvent.click(trigger);
    await screen.findByRole("dialog", PALETTE);

    // On the document: Radix's dismissable layer listens on the ownerDocument,
    // and an event dispatched on `window` never reaches it.
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => {
      expect(screen.queryByRole("dialog", PALETTE)).toBeNull();
    });

    // Restored on a frame rather than synchronously, which is why this is a
    // `waitFor` and not a bare assertion. A dialog that closes and leaves focus
    // on nothing strands anybody navigating by keyboard at the top of the page.
    await waitFor(() => {
      expect(document.activeElement).toBe(trigger);
    });
  });
});
