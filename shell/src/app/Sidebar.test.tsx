import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { Sidebar } from "./Sidebar";
import { NAV_ITEMS } from "./nav";
import { renderWithRouter } from "../test/harness";

// The harness reaches the app's router, which reaches the data layer, which
// reaches the Tauri bridge for the daemon token. Nothing in this file calls it.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

beforeEach(() => {
  window.localStorage.clear();
});

describe("Sidebar", () => {
  it("renders every item of the nav table, in order", async () => {
    const { container } = await renderWithRouter(<Sidebar />);

    const rendered = Array.from(container.querySelectorAll<HTMLElement>("[data-nav-path]")).map(
      (link) => link.dataset.navPath,
    );
    // No `projects` prop, so the roster contributes nothing and the rail is
    // exactly the table. The roster's own rendering is four tests above.
    expect(rendered).toEqual(NAV_ITEMS.map((item) => item.path));
  });

  /**
   * The roster group is the one part of the rail that is not in the nav table,
   * so it is the one part a table test cannot defend. These four are its
   * replacement.
   */
  it("lists the projects it is given, under the group that declares the position", async () => {
    await renderWithRouter(
      <Sidebar projects={[{ id: "nucleos", mode: "active", pending: 0 }, { id: "sidecar", mode: "shadow", pending: 2 }]} />,
      { initialPath: "/projects" },
    );

    expect(screen.getByRole("link", { name: "nucleos" }).getAttribute("href")).toContain(
      "/projects/nucleos/estado",
    );
    // The roster page keeps its place at the head of the group: it answers a
    // fleet-wide question no single workspace can.
    expect(screen.getByRole("link", { name: "All projects" })).toBeTruthy();
  });

  it("draws no roster rows when the roster has not answered yet", async () => {
    await renderWithRouter(<Sidebar />);

    // Not an empty group with a heading and nothing under it, and not a
    // "no projects" line either: the daemon has not spoken, and inventing a
    // sentence about what it did not say is the failure mode this guards.
    expect(screen.queryByRole("link", { name: "nucleos" })).toBeNull();
    expect(screen.getByRole("link", { name: "All projects" })).toBeTruthy();
  });

  it("says a project's pending count out loud, since the badge is only drawn", async () => {
    await renderWithRouter(<Sidebar projects={[{ id: "sidecar", mode: "shadow", pending: 2 }]} />, {
      initialPath: "/projects",
    });

    expect(screen.getByRole("link", { name: "sidecar, 2 waiting" })).toBeTruthy();
  });

  /**
   * **The rail is destinations; the roster is content.** Every other row in the
   * sidebar is one of a fixed list; this group's length belongs to the daemon,
   * and fifteen projects would push Work and Pillars off the bottom edge to show
   * names nobody reading the Feed is looking for. So the rows are drawn where
   * they are the subject and nowhere else.
   *
   * The group is NOT conditional — heading and `All projects` stay put on every
   * page — because a group that came and went would move everything under it.
   */
  it("draws the roster only inside the projects area, and keeps the group everywhere", async () => {
    const roster = [{ id: "nucleos", mode: "active" as const, pending: 0 }];

    const away = await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });
    expect(screen.queryByRole("link", { name: "nucleos" })).toBeNull();
    // The way in is still there, in the place it has always been.
    expect(screen.getByRole("link", { name: "All projects" })).toBeTruthy();
    away.unmount();

    await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/projects" });
    expect(screen.getByRole("link", { name: "nucleos" })).toBeTruthy();
  });

  /**
   * Inside a workspace as much as on the list, because switching projects without
   * going back out to the list is the whole of what the rows are for — and the
   * wizard counts too: a rail that emptied while somebody was adding a project
   * would read as having lost the ones they had.
   */
  it("keeps the roster while you are in a workspace or adding one", async () => {
    const roster = [{ id: "nucleos", mode: "active" as const, pending: 0 }];

    const inside = await renderWithRouter(<Sidebar projects={roster} />, {
      initialPath: "/projects/sidecar/codigo",
    });
    expect(screen.getByRole("link", { name: "nucleos" })).toBeTruthy();
    inside.unmount();

    await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/projects/new" });
    expect(screen.getByRole("link", { name: "nucleos" })).toBeTruthy();
  });

  it("keeps a project lit while you are in any of its three modes", async () => {
    await renderWithRouter(<Sidebar projects={[{ id: "nucleos", mode: "active", pending: 0 }]} />, {
      initialPath: "/projects/nucleos/workflows",
    });

    expect(screen.getByRole("link", { name: "nucleos" }).getAttribute("aria-current")).toBe("page");
  });

  it("marks the page you are on, and only that one", async () => {
    await renderWithRouter(<Sidebar />, { initialPath: "/fleet" });

    expect(screen.getByRole("link", { name: "Fleet" }).getAttribute("aria-current")).toBe("page");
    expect(screen.getByRole("link", { name: "Runs" }).getAttribute("aria-current")).toBeNull();
    // `/` is exempt from the prefix rule: every path starts with it, and Home
    // lighting up on every page would say "you are nowhere in particular".
    expect(screen.getByRole("link", { name: "Home" }).getAttribute("aria-current")).toBeNull();
  });

  it("navigates on Enter from the keyboard", async () => {
    const { router } = await renderWithRouter(<Sidebar />);

    const runs = screen.getByRole("link", { name: "Runs" });
    runs.focus();
    fireEvent.keyDown(runs, { key: "Enter" });

    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/runs");
    });
  });

  it("walks the items with the arrow keys and wraps at the ends", async () => {
    const { container } = await renderWithRouter(<Sidebar />);

    const home = screen.getByRole("link", { name: "Home" });
    home.focus();
    fireEvent.keyDown(home, { key: "ArrowDown" });
    expect(document.activeElement).toBe(screen.getByRole("link", { name: "Fleet" }));

    // Up from the first item lands on the last one — System, at the bottom of
    // the footer — rather than dropping focus out of the rail entirely.
    home.focus();
    fireEvent.keyDown(home, { key: "ArrowUp" });
    const items = container.querySelectorAll<HTMLElement>("[data-nav-path]");
    expect(document.activeElement).toBe(items[items.length - 1]);
  });

  it("shows a count only when there is something to count", async () => {
    const { unmount } = await renderWithRouter(<Sidebar badges={{ proposals: 0 }} />);
    // Zero summons nobody, and a badge is a summons. The accessible name is the
    // label alone.
    expect(screen.getByRole("link", { name: "Waiting" })).toBeDefined();
    unmount();

    await renderWithRouter(<Sidebar badges={{ proposals: 7 }} />);
    // And when there is a count it is part of the name, not decoration a screen
    // reader steps over — a summons only some people get is not a summons.
    expect(screen.getByRole("link", { name: "Waiting, 7 waiting" })).toBeDefined();
  });

  it("renders no badge at all for a count the shell has no source for", async () => {
    await renderWithRouter(<Sidebar badges={{ proposals: 3 }} />);

    // Unread chats and untriaged mail arrive with their own slices. Absent is
    // not zero, and neither of them may be rendered as one.
    expect(screen.getByRole("link", { name: "Chats" })).toBeDefined();
    expect(screen.getByRole("link", { name: "Mail" })).toBeDefined();
  });

  it("pins the footer outside the scrolling list", async () => {
    const { container } = await renderWithRouter(<Sidebar>{<span>kill switch slot</span>}</Sidebar>);

    const scroll = container.querySelector(".nav-scroll");
    const footer = container.querySelector(".nav-footer");
    expect(scroll).not.toBeNull();
    expect(footer).not.toBeNull();
    // The mechanism behind "the kill switch is never hidden": a footer inside
    // the scroller would be reachable only from the bottom of a twenty-item
    // list, which is exactly where nobody is when they need to stop the machine.
    expect(scroll?.contains(footer as Node)).toBe(false);
    expect(footer?.textContent).toContain("kill switch slot");
    expect(screen.getByRole("link", { name: "System" })).toBeDefined();
  });

  it("remembers the icon collapse across sessions", async () => {
    const first = await renderWithRouter(<Sidebar />);
    fireEvent.click(screen.getByRole("button", { name: /collapse the sidebar/i }));
    expect(window.localStorage.getItem("nucleos.sidebar.collapsed")).toBe("1");
    first.unmount();

    const { container } = await renderWithRouter(<Sidebar />);
    expect(container.querySelector(".nav")?.className).toContain("nav-collapsed");
    // And the way back out survives the collapse — a rail with no expand
    // control is a trap.
    expect(screen.getByRole("button", { name: /expand the sidebar/i })).toBeDefined();
  });

  it("puts a dot on System when the daemon is not ok", async () => {
    const { unmount } = await renderWithRouter(<Sidebar />);
    expect(screen.getByRole("link", { name: "System" })).toBeDefined();
    unmount();

    await renderWithRouter(<Sidebar systemAlert />);
    // The dot carries a word, so it is not a colour-only signal.
    expect(screen.getByRole("link", { name: "System, needs attention" })).toBeDefined();
  });
});
