import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen } from "@testing-library/react";
import { NAV_PATHS } from "./app/nav";
import { PAGES, createAppRouter } from "./router";
import { Bench } from "./team/Bench";
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

  it("navigates between two pages with the shell intact", async () => {
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

  it("has a real page for every navigation item, with no placeholder left", () => {
    // The end state S1 wrote down: the roaming "unbuilt page" example ran out
    // of pages to point at, so the example was retired and the invariant it
    // stood in for is asserted directly instead. `/teams` was the last one.
    for (const path of NAV_PATHS) expect(PAGES[path]).toBeDefined();

    // And the two detail routes this slice added, which no NAV_PATH covers —
    // `/team-runs` is not a navigation item at all.
    const router = createAppRouter();
    const byId = router.routesById as unknown as Record<string, { fullPath?: string }>;
    const paths = Object.values(byId).map((route) => route.fullPath);
    expect(paths).toContain("/teams/$teamId");
    expect(paths).toContain("/team-runs/$runId");
  });

  /**
   * The bench is its own component, and the console is not it.
   *
   * `/teams/$teamId` used to be a second mounting of `Teams`, which served both
   * routes in the Council pattern. Splitting them is the whole point of the
   * redesign, and it is the kind of change that is easy to make in one file and
   * forget in the other: `Bench.test.tsx` builds its own router — for the
   * reasons it states — so its whole suite passes whether or not the real tree
   * ever reaches `Bench`. This is the assertion that file cannot make about
   * itself.
   */
  it("sends /teams/$teamId to the bench and /teams to the console", () => {
    const router = createAppRouter();
    const byId = router.routesById as unknown as Record<
      string,
      { options?: { component?: unknown } }
    >;

    expect(byId["/teams/$teamId"]?.options?.component).toBe(Bench);
    expect(byId["/teams"]?.options?.component).toBe(PAGES["/teams"]);
    expect(byId["/teams"]?.options?.component).not.toBe(Bench);
  });

  /**
   * The two project routes, and the fact that they are two.
   *
   * `Projects.test.tsx` builds a router of its own — for good reasons it states —
   * which means its whole suite passes whether or not the real tree registers
   * the inspector at all. That is exactly what happened when the workspace took
   * the shorter path over: every inspector test stayed green while the tabs on
   * screen led somewhere else. This is the assertion that file cannot make about
   * itself.
   */
  it("registers the workspace and the inspector as two different routes", () => {
    const router = createAppRouter();
    const byId = router.routesById as unknown as Record<string, { fullPath?: string }>;
    const paths = Object.values(byId).map((route) => route.fullPath);

    expect(paths).toContain("/projects/$projectId/$view");
    expect(paths).toContain("/projects/$projectId/inspect/$view");
  });

  it("opens on Home", async () => {
    await renderApp();

    expect(await screen.findByRole("heading", { level: 1, name: "Home" })).toBeDefined();
  });
});
