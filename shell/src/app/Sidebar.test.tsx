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
      "/projects/nucleos/state",
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

  it("says a project's open-proposal count out loud, since the badge is only drawn", async () => {
    await renderWithRouter(<Sidebar projects={[{ id: "sidecar", mode: "shadow", pending: 2 }]} />, {
      initialPath: "/projects",
    });

    // A roster badge is open proposals, not the Waiting queue's arithmetic.
    expect(screen.getByRole("link", { name: "sidecar, 2 items to review" })).toBeTruthy();
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
      initialPath: "/projects/sidecar/code",
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

  /**
   * Two rows said it, on every project screen there is.
   *
   * `isActive` prefix-matches so that `/runs/412` keeps Runs lit, and the row that owns the
   * roster is the one place that is wrong: inside `/projects/alpha/state` both `All projects`
   * and `alpha` matched, and both got the fill, the `--text` label, the glyph, the bar and
   * `aria-current="page"`. "You are here" twice is "you are here" nowhere.
   */
  it("says you are here once, even where a row owns the rows under it", async () => {
    await renderWithRouter(<Sidebar projects={[{ id: "alpha", mode: "active", pending: 0 }]} />, {
      initialPath: "/projects/alpha/state",
    });

    const here = screen
      .getAllByRole("link")
      .filter((link) => link.getAttribute("aria-current") === "page");
    expect(here).toHaveLength(1);
    expect(here[0].getAttribute("aria-label") ?? here[0].textContent).toContain("alpha");
  });

  /** And the group's own row is not given up — it is lit on the page it actually is. */
  it("and on the list itself it is All projects", async () => {
    await renderWithRouter(<Sidebar projects={[{ id: "alpha", mode: "active", pending: 0 }]} />, {
      initialPath: "/projects",
    });

    expect(
      screen.getByRole("link", { name: "All projects" }).getAttribute("aria-current"),
    ).toBe("page");
    const here = screen
      .getAllByRole("link")
      .filter((link) => link.getAttribute("aria-current") === "page");
    expect(here).toHaveLength(1);
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

  it("names chat and mail badges by their own arithmetic", async () => {
    await renderWithRouter(<Sidebar badges={{ chats: 2, mail: 3 }} />);

    expect(screen.getByRole("link", { name: "Chats, 2 unread" })).toBeDefined();
    expect(screen.getByRole("link", { name: "Mail, 3 untriaged" })).toBeDefined();
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

  /**
   * The switcher above the scroll.
   *
   * It exists because reaching a project from anywhere else costs a list and a
   * choice — the cost that promoted Projects to a group, and that the
   * conditional roster gave back everywhere outside the projects area. These
   * six are what stops it becoming decoration.
   */
  describe("project switcher", () => {
    const roster = [
      { id: "alpha", mode: "shadow" as const, pending: 0 },
      { id: "bravo", mode: "active" as const, pending: 0 },
    ];

    it("names the núcleo away from a project, and the project inside one", async () => {
      const away = await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });
      expect(screen.getByRole("button", { name: /^NucleOS/ })).toBeTruthy();
      away.unmount();

      await renderWithRouter(<Sidebar projects={roster} />, {
        initialPath: "/projects/bravo/workflows",
      });
      // Any of the three modes, like the rail's own rows: somebody reading
      // Workflows has not left the project.
      expect(screen.getByRole("button", { name: /^bravo/ })).toBeTruthy();
    });

    it("offers the núcleo, every project, and the way to a new one", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });

      const button = screen.getByRole("button", { name: /switch project/i });
      expect(button.getAttribute("aria-expanded")).toBe("false");
      fireEvent.click(button);
      expect(button.getAttribute("aria-expanded")).toBe("true");

      // Home, and it is reached by name rather than by "go back": the núcleo is
      // not a project, so it is not one of the choices being compared.
      expect(screen.getByRole("link", { name: "NucleOS" }).getAttribute("href")).toBe("/");
      expect(screen.getByRole("link", { name: "alpha, shadow" }).getAttribute("href")).toContain(
        "/projects/alpha/state",
      );
      expect(screen.getByRole("link", { name: "New project" }).getAttribute("href")).toContain(
        "/projects/new",
      );
    });

    it("says each project's mode out loud, since the menu only draws a dot", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });
      fireEvent.click(screen.getByRole("button", { name: /switch project/i }));

      // The owner asked for the state not to take space on screen, and a colour
      // that nobody can hear is a state only some readers get. The word costs
      // nothing in speech.
      expect(screen.getByRole("link", { name: "bravo, active" })).toBeTruthy();
    });

    it("marks the destination you are already on", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/projects/alpha/state" });
      fireEvent.click(screen.getByRole("button", { name: /switch project/i }));

      // `page`, the same word the rail's own rows use: the router marks the
      // link whose path it is on with that value anyway, and a switcher that
      // said `true` would announce the same row differently depending on which
      // of a project's three modes you were reading.
      expect(screen.getByRole("link", { name: "alpha, shadow" }).getAttribute("aria-current")).toBe(
        "page",
      );
      expect(screen.getByRole("link", { name: "bravo, active" }).getAttribute("aria-current")).toBeNull();
    });

    it("closes on Escape and hands focus back to the button", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });

      const button = screen.getByRole("button", { name: /switch project/i });
      fireEvent.click(button);
      const first = screen.getByRole("link", { name: "NucleOS" });
      first.focus();
      fireEvent.keyDown(first, { key: "Escape" });

      // A menu that closes and drops focus on the body leaves a keyboard reader
      // at the top of the page, which is worse than where they opened it from.
      expect(screen.queryByRole("link", { name: "NucleOS" })).toBeNull();
      expect(document.activeElement).toBe(button);
    });

    it("keeps its rows out of the rail's own arrow-key walk", async () => {
      const { container } = await renderWithRouter(<Sidebar projects={roster} />, {
        initialPath: "/feed",
      });
      const before = container.querySelectorAll("[data-nav-path]").length;

      fireEvent.click(screen.getByRole("button", { name: /switch project/i }));

      // An open menu has its own up-and-down. Rows that also walked the rail
      // behind it would leave focus somewhere the reader cannot see.
      expect(container.querySelectorAll("[data-nav-path]").length).toBe(before);

      const alpha = screen.getByRole("link", { name: "alpha, shadow" });
      alpha.focus();
      fireEvent.keyDown(alpha, { key: "ArrowDown" });
      expect(document.activeElement).toBe(screen.getByRole("link", { name: "bravo, active" }));
    });
  });

  /**
   * The dropdown on `All projects`.
   *
   * The route used to decide the roster on its own, which meant somebody working
   * out of the Feed could not see their projects at all and somebody who never
   * wanted them had to. The chevron makes that answer theirs — and these five
   * are what keep it from taking the destination away in exchange.
   */
  describe("the projects dropdown", () => {
    const roster = [
      { id: "alpha", mode: "shadow" as const, pending: 0 },
      { id: "bravo", mode: "active" as const, pending: 0 },
    ];

    it("opens the roster from a page that would not have shown it", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });
      // The rail's own rows are named by the word on them — the switcher's
      // "alpha, shadow" is the menu's naming, and these are not those rows.
      expect(screen.queryByRole("link", { name: "alpha" })).toBeNull();

      fireEvent.click(screen.getByRole("button", { name: "Show the project list" }));

      expect(screen.getByRole("link", { name: "alpha" })).toBeTruthy();
    });

    it("shuts it inside the projects area, where the route had opened it", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/projects" });
      expect(screen.getByRole("link", { name: "alpha" })).toBeTruthy();

      fireEvent.click(screen.getByRole("button", { name: "Hide the project list" }));

      expect(screen.queryByRole("link", { name: "alpha" })).toBeNull();
    });

    /**
     * The answer is the reader's from then on, on every page. A dropdown that
     * re-decided itself the moment you navigated would not be a control, it
     * would be a hint.
     */
    it("keeps the answer it was given, and stops following the route", async () => {
      const first = await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/projects" });
      fireEvent.click(screen.getByRole("button", { name: "Hide the project list" }));
      first.unmount();

      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/projects" });

      // The route says open. The reader said shut, and the reader was last.
      expect(screen.queryByRole("link", { name: "alpha" })).toBeNull();
      expect(screen.getByRole("button", { name: "Show the project list" }).getAttribute("aria-expanded")).toBe(
        "false",
      );
    });

    /**
     * The word is still a page. The chevron was added to open a list, not to
     * spend the one row that answers a question no single workspace can.
     */
    it("leaves All projects a link, and offers nothing to open before the roster answers", async () => {
      const { unmount } = await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });
      expect(screen.getByRole("link", { name: "All projects" }).getAttribute("href")).toBe("/projects");
      unmount();

      await renderWithRouter(<Sidebar />, { initialPath: "/projects" });
      // `undefined` is "the daemon has not spoken", and a disclosure that opens
      // onto nothing is an affordance that lies.
      expect(screen.queryByRole("button", { name: /project list/ })).toBeNull();
      expect(screen.getByRole("link", { name: "All projects" })).toBeTruthy();
    });

    it("takes the shut rows out of the rail's arrow walk, rather than hiding them in it", async () => {
      const { container } = await renderWithRouter(<Sidebar projects={roster} />, {
        initialPath: "/projects",
      });
      const open = container.querySelectorAll("[data-nav-path]").length;

      fireEvent.click(screen.getByRole("button", { name: "Hide the project list" }));

      // Two rows fewer in the DOM, not two rows hidden in it: the walk reads the
      // document, and `hidden` rows would send focus somewhere invisible.
      expect(container.querySelectorAll("[data-nav-path]").length).toBe(open - roster.length);
    });
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
