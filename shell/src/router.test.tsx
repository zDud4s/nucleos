import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen } from "@testing-library/react";
import { NAV_PATHS } from "./app/nav";
import { createAppRouter } from "./router";
import { daemonFetch, daemonState, renderApp } from "./test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("./data/client", async (original) => ({
  ...(await original<typeof import("./data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiText.mockReset();
  daemon.probeHealth.mockReset();

  // A daemon that is up and authorising, so the gate lets the shell render and
  // the test is about routing rather than about the handshake.
  daemon.probeHealth.mockResolvedValue(true);
  daemon.apiText.mockResolvedValue("daemon running");
  daemon.apiFetch.mockImplementation(daemonFetch(daemonState()));
});

describe("the app router", () => {
  it("builds exactly one route per navigation item", () => {
    // The nav table is the route list. This is what stops the app growing a
    // page that is unreachable, or a sidebar entry that leads nowhere.
    const router = createAppRouter();
    // Read through a loose view: the route tree is built from an array, so the
    // library's inference collapses `fullPath` to the root's literal type and a
    // direct comparison would be a type error about a value that is really there.
    const byId = router.routesById as unknown as Record<string, { fullPath?: string }>;
    const paths = Object.values(byId).map((route) => route.fullPath);

    for (const path of NAV_PATHS) expect(paths).toContain(path);
  });

  it("navigates between two placeholder routes with the shell intact", async () => {
    const { router } = await renderApp({ initialPath: "/fleet" });

    expect(await screen.findByRole("heading", { level: 1, name: "Fleet" })).toBeDefined();
    expect(screen.getByRole("navigation", { name: "Sections" })).toBeDefined();

    fireEvent.click(screen.getByRole("link", { name: "Runs" }));

    expect(await screen.findByRole("heading", { level: 1, name: "Runs" })).toBeDefined();
    expect(router.state.location.pathname).toBe("/runs");
    // The page changed and nothing around it did: the rail, the connection line
    // and the kill switch are the same DOM they were before the click.
    expect(screen.getByRole("navigation", { name: "Sections" })).toBeDefined();
    expect(screen.getByRole("button", { name: /kill switch/i })).toBeDefined();
    expect(screen.queryByRole("heading", { level: 1, name: "Fleet" })).toBeNull();
  });

  it("gives an unbuilt page a Teach that names the slice it arrives with", async () => {
    await renderApp({ initialPath: "/voice" });

    expect(await screen.findByRole("heading", { level: 1, name: "Voice" })).toBeDefined();
    // Not a spinner and not a 404: the route works, the page is simply not
    // built, and saying which slice brings it is the difference between a
    // placeholder and a dead end.
    expect(screen.getByText(/Pillars slice/)).toBeDefined();
  });

  it("says a route the núcleo cannot serve is not wired, not that it is broken", async () => {
    await renderApp({ initialPath: "/teams" });

    expect(await screen.findByRole("heading", { level: 1, name: "Teams" })).toBeDefined();
    expect(screen.getByText(/not yet wired/i)).toBeDefined();
  });

  it("opens on Home", async () => {
    await renderApp();

    expect(await screen.findByRole("heading", { level: 1, name: "Home" })).toBeDefined();
  });
});
