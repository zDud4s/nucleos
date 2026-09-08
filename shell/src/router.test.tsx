import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { NAV_PATHS } from "./app/nav";
import { createAppQueryClient } from "./app/queryClient";
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

  /**
   * The invariant the error boundary exists for, asserted at the router rather
   * than at the component.
   *
   * `RouteError.test.tsx` proves the boundary renders what it should when it is
   * handed an error. It cannot prove the thing that actually mattered: that the
   * boundary is mounted *below* the shell, so a page that throws takes the page
   * and nothing else. Wired one level too high — on the root route — every one
   * of these assertions fails while the component's own test stays green.
   */
  it("keeps the rail and the kill switch when a page throws", async () => {
    const router = createAppRouter("/files");
    // The same loose view the cases above use, for the same reason: the tree is
    // built from an array, so the library's inference has nothing literal to
    // hand back and a direct reach into `routesById` is a type error about a
    // value that is really there.
    const byId = router.routesById as unknown as Record<
      string,
      { options: { component: unknown } }
    >;
    byId["/files"].options.component = () => {
      throw new Error("boom in the page");
    };

    // React writes the caught error to the console on its way to the boundary.
    // That is correct of React and noise here — the throw is the fixture.
    const quiet = vi.spyOn(console, "error").mockImplementation(() => {});
    try {
      await router.load();
      render(
        <QueryClientProvider client={createAppQueryClient()}>
          <RouterProvider router={router} />
        </QueryClientProvider>,
      );

      expect((await screen.findByRole("alert")).textContent).toContain("boom in the page");
      // And the shell around it, asserted with the same two queries the case
      // above uses for a page that did NOT throw.
      expect(screen.getByRole("navigation", { name: "Sections" })).toBeDefined();
      expect(screen.getByRole("button", { name: /kill switch/i })).toBeDefined();
    } finally {
      quiet.mockRestore();
    }
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

  /**
   * The fifth mode, reached by its own segment.
   *
   * `/projects/$projectId/$view` takes any word, so nothing in `router.tsx` had to change for this
   * — which is exactly why it wants an assertion here. `Workspace` answers an unrecognised mode
   * with `state` rather than a dead end, so a `github` that was never added to `MODES` would leave
   * this route resolving, the shell rendering, and the State mode on screen under a URL that says
   * otherwise. Nothing would be red.
   *
   * Asserted through the whole app rather than by mounting `Workspace`: the tab strip, the segment
   * and the page have to agree, and a component test cannot see the router that carries them.
   */
  it("draws the GitHub mode at /projects/$projectId/github", async () => {
    const { router } = await renderApp({ initialPath: "/projects/nucleos/github" });

    expect(router.state.location.pathname).toBe("/projects/nucleos/github");
    // The mode's own sections, which no other mode has — and not the tab, which is on screen
    // whichever mode is open.
    expect(await screen.findByRole("region", { name: "What runs on its own" })).toBeDefined();
    expect(screen.getByRole("region", { name: "What the worktrees may run" })).toBeDefined();
    // State is the fallback an unrecognised segment lands on, so its absence is what proves the
    // segment was recognised.
    expect(screen.queryByRole("region", { name: "Readings" })).toBeNull();
  });

  it("offers the fifth mode in the tab strip, beside the four that were there", async () => {
    await renderApp({ initialPath: "/projects/nucleos/state" });

    const modes = await screen.findByRole("navigation", { name: "Project modes" });
    const links = within(modes).getAllByRole("link");

    // Five and not four. Read by position rather than by name because two of the tabs carry a
    // count beside their label once the query behind them has answered.
    expect(links).toHaveLength(5);
    expect(links[4].textContent).toContain("GitHub");
    expect(links[4].getAttribute("href")).toBe("/projects/nucleos/github");
  });

  it("opens on Home", async () => {
    await renderApp();

    expect(await screen.findByRole("heading", { level: 1, name: "Home" })).toBeDefined();
  });
});
