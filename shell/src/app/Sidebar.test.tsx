import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
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
    // The rail is exactly the table: projects live in the switcher, not here.
    expect(rendered).toEqual(NAV_ITEMS.map((item) => item.path));
  });

  /**
   * The rail is a fixed list of destinations; the switcher is the one list of
   * projects. A project row creeping back into the rail — under a group, a
   * disclosure, or anything else — is what these two hold off.
   */
  it("draws no project rows in the rail, whatever the roster and the route", async () => {
    const roster = [
      { id: "alpha", mode: "active" as const, pending: 3 },
      { id: "bravo", mode: "shadow" as const, pending: 0 },
    ];
    for (const initialPath of ["/feed", "/projects", "/projects/alpha/state", "/projects/new"]) {
      const { container, unmount } = await renderWithRouter(<Sidebar projects={roster} />, { initialPath });
      const rendered = Array.from(container.querySelectorAll<HTMLElement>("[data-nav-path]")).map(
        (link) => link.dataset.navPath,
      );
      expect(rendered).toEqual(NAV_ITEMS.map((item) => item.path));
      expect(container.querySelector(".nav-scroll a[href^='/projects/']")).toBeNull();
      unmount();
    }
  });

  it("draws Projects as a plain row in Operate, right after Fleet", async () => {
    const { container } = await renderWithRouter(<Sidebar />);

    const operate = screen.getByRole("list", { name: "Operate" });
    const labels = Array.from(operate.querySelectorAll<HTMLElement>("[data-nav-path]")).map(
      (link) => link.textContent,
    );
    expect(labels.slice(0, 3)).toEqual(["Home", "Fleet", "Projects"]);
    expect(screen.getByRole("link", { name: "Projects" }).getAttribute("href")).toBe("/projects");
    // No group of its own any more, and no disclosure beside it.
    expect(container.querySelector("#nav-group-projects")).toBeNull();
    expect(screen.queryByRole("button", { name: /project list/ })).toBeNull();
  });

  /**
   * The ordinary prefix rule, and on purpose: with no project rows in the rail,
   * the one row about projects is the right "you are here" anywhere under it.
   */
  it("keeps Projects lit inside a workspace, and says you are here once", async () => {
    await renderWithRouter(<Sidebar projects={[{ id: "alpha", mode: "active", pending: 0 }]} />, {
      initialPath: "/projects/alpha/state",
    });

    const here = screen
      .getAllByRole("link")
      .filter((link) => link.getAttribute("aria-current") === "page");
    expect(here).toHaveLength(1);
    expect(here[0].textContent).toBe("Projects");
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

  it("names the chat badge by its own arithmetic, and gives Mail none", async () => {
    await renderWithRouter(<Sidebar badges={{ chats: 2 }} />);

    expect(screen.getByRole("link", { name: "Chats, 2 unread" })).toBeDefined();
    // Mail carries no badge at all — see the comment on its row in `nav.ts`. Untriaged is the
    // daemon's backlog, and an Awaiting You count that nothing can clear teaches a reader to
    // ignore the tone everywhere it does mean them.
    expect(screen.getByRole("link", { name: "Mail" })).toBeDefined();
  });

  it("shows the open capture count on the Brain entry", async () => {
    await renderWithRouter(<Sidebar badges={{ captures: 3 }} />);

    expect(screen.getByRole("link", { name: "Brain, 3 to answer" })).toBeDefined();
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
   * The switcher above the scroll — since 2026-10-02 the one list of projects.
   */
  describe("project switcher", () => {
    const roster = [
      { id: "bravo", mode: "active" as const, pending: 0 },
      { id: "alpha", mode: "shadow" as const, pending: 2 },
      { id: "charlie", mode: "off" as const, pending: 0 },
    ];

    function openSwitcher() {
      fireEvent.click(screen.getByRole("button", { name: /switch project/i }));
      return screen.getByRole("combobox", { name: "Find a project" }) as HTMLInputElement;
    }

    function highlighted() {
      return screen.getAllByRole("option").find((option) => option.getAttribute("aria-selected") === "true");
    }

    it("names the núcleo away from a project, and the project inside one", async () => {
      const away = await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });
      expect(screen.getByRole("button", { name: /^NucleOS/ })).toBeTruthy();
      away.unmount();

      await renderWithRouter(<Sidebar projects={roster} />, {
        initialPath: "/projects/bravo/workflows",
      });
      // Any of the three modes: somebody reading Workflows has not left the project.
      const button = screen.getByRole("button", { name: /^bravo/ });
      // The mode in words under the name, and the initial in the lead box.
      expect(button.textContent).toContain("active");
      expect(button.querySelector(".nav-switch-initial")?.textContent).toBe("B");
    });

    it("does not take the wizard for a project called new", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/projects/new" });
      expect(screen.getByRole("button", { name: /^NucleOS/ })).toBeTruthy();
    });

    it("opens onto the search, then the núcleo, then the projects alphabetically", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });

      const button = screen.getByRole("button", { name: /switch project/i });
      expect(button.getAttribute("aria-expanded")).toBe("false");
      const input = openSwitcher();
      expect(button.getAttribute("aria-expanded")).toBe("true");
      expect(document.activeElement).toBe(input);
      expect(input.getAttribute("aria-controls")).toBe(screen.getByRole("listbox").id);

      const options = screen.getAllByRole("option");
      // The mode is a dot and the count a bare number on screen; both are words to the ear.
      expect(options.map((option) => option.getAttribute("aria-label") ?? option.textContent)).toEqual([
        "NucleOS",
        "alpha, shadow, 2 items to review",
        "bravo, active",
        "charlie, off",
      ]);
      expect(options[0].getAttribute("href")).toBe("/");
      expect(options[1].getAttribute("href")).toBe("/projects/alpha/state");
      // Off any project, the núcleo is where you are.
      expect(options[0].getAttribute("aria-current")).toBe("page");
      expect(screen.getByRole("group", { name: /Projects/ }).textContent).toContain("3");

      expect(screen.getByRole("link", { name: "All projects" }).getAttribute("href")).toBe("/projects");
      expect(screen.getByRole("link", { name: "New project" }).getAttribute("href")).toBe("/projects/new");
    });

    it("marks the project you are in, from any of its modes", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/projects/alpha/workflows" });
      openSwitcher();

      const current = screen
        .getAllByRole("option")
        .filter((option) => option.getAttribute("aria-current") === "page");
      expect(current.map((option) => option.getAttribute("aria-label"))).toEqual([
        "alpha, shadow, 2 items to review",
      ]);
    });

    it("filters as you type, marks the match, and moves one highlight with keys and pointer", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });
      const input = openSwitcher();
      expect(highlighted()?.textContent).toBe("NucleOS");

      fireEvent.change(input, { target: { value: "AR" } });
      // The núcleo is not a project, so a search among projects leaves it out.
      const options = screen.getAllByRole("option");
      expect(options.map((option) => option.getAttribute("aria-label"))).toEqual(["charlie, off"]);
      expect(options[0].querySelector("mark")?.textContent).toBe("ar");
      expect(input.getAttribute("aria-activedescendant")).toBe(options[0].id);

      fireEvent.change(input, { target: { value: "" } });
      fireEvent.keyDown(input, { key: "ArrowDown" });
      expect(highlighted()?.getAttribute("aria-label")).toBe("alpha, shadow, 2 items to review");
      // Wraps at both ends.
      fireEvent.keyDown(input, { key: "ArrowUp" });
      fireEvent.keyDown(input, { key: "ArrowUp" });
      expect(highlighted()?.getAttribute("aria-label")).toBe("charlie, off");
      fireEvent.keyDown(input, { key: "Home" });
      expect(highlighted()?.textContent).toBe("NucleOS");
      fireEvent.keyDown(input, { key: "End" });
      expect(highlighted()?.getAttribute("aria-label")).toBe("charlie, off");

      // The pointer moves the same highlight rather than drawing a second one.
      const bravo = screen.getByRole("option", { name: "bravo, active" });
      fireEvent.pointerMove(bravo);
      expect(highlighted()).toBe(bravo);
      expect(input.getAttribute("aria-activedescendant")).toBe(bravo.id);
    });

    it("says so quietly when nothing matches", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });
      const input = openSwitcher();
      fireEvent.change(input, { target: { value: "zulu" } });

      expect(screen.queryAllByRole("option")).toHaveLength(0);
      expect(screen.getByText("No project called “zulu”.")).toBeTruthy();
      expect(input.getAttribute("aria-activedescendant")).toBeNull();
    });

    it("navigates to the highlighted row on Enter, and closes", async () => {
      const { router } = await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });
      const input = openSwitcher();
      fireEvent.change(input, { target: { value: "bra" } });
      fireEvent.keyDown(input, { key: "Enter" });

      await waitFor(() => {
        expect(router.state.location.pathname).toBe("/projects/bravo/state");
      });
      expect(screen.queryByRole("combobox")).toBeNull();
    });

    it("closes on Escape and hands focus back to the button", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });
      const button = screen.getByRole("button", { name: /switch project/i });
      const input = openSwitcher();
      fireEvent.keyDown(input, { key: "Escape" });

      // A panel that closes and drops focus on the body leaves a keyboard reader
      // at the top of the page, which is worse than where they opened it from.
      expect(screen.queryByRole("combobox")).toBeNull();
      expect(document.activeElement).toBe(button);
    });

    it("closes on a press outside it", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });
      openSwitcher();
      fireEvent.pointerDown(document.body);
      expect(screen.queryByRole("combobox")).toBeNull();
    });

    it("keeps its rows out of the rail's own arrow-key walk", async () => {
      const { container } = await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });
      const before = container.querySelectorAll("[data-nav-path]").length;
      openSwitcher();
      expect(container.querySelectorAll("[data-nav-path]").length).toBe(before);
    });

    it("offers the núcleo and the footer alone before the roster answers", async () => {
      await renderWithRouter(<Sidebar />, { initialPath: "/feed" });
      openSwitcher();

      expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual(["NucleOS"]);
      // No count and no sentence: `undefined` is "the daemon has not spoken".
      expect(screen.queryByRole("group")).toBeNull();
      expect(screen.queryByText(/No project/)).toBeNull();
      expect(screen.getByRole("link", { name: "All projects" })).toBeTruthy();
      expect(screen.getByRole("link", { name: "New project" })).toBeTruthy();
    });

    it("opens on Ctrl+P from anywhere, and takes the chord from Print", async () => {
      await renderWithRouter(<Sidebar projects={roster} />, { initialPath: "/feed" });

      const event = new KeyboardEvent("keydown", { key: "p", ctrlKey: true, bubbles: true, cancelable: true });
      act(() => {
        document.body.dispatchEvent(event);
      });
      expect(event.defaultPrevented).toBe(true);
      expect(document.activeElement).toBe(screen.getByRole("combobox", { name: "Find a project" }));
    });

    it("expands a collapsed rail first, and leaves other people's fields alone", async () => {
      window.localStorage.setItem("nucleos.sidebar.collapsed", "1");
      const { container } = await renderWithRouter(
        <>
          <Sidebar projects={roster} />
          <input aria-label="somebody else's field" />
        </>,
        { initialPath: "/feed" },
      );

      const field = screen.getByRole("textbox", { name: "somebody else's field" });
      const typed = new KeyboardEvent("keydown", { key: "p", ctrlKey: true, bubbles: true, cancelable: true });
      act(() => {
        field.dispatchEvent(typed);
      });
      expect(typed.defaultPrevented).toBe(false);
      expect(screen.queryByRole("combobox")).toBeNull();

      act(() => {
        document.body.dispatchEvent(new KeyboardEvent("keydown", { key: "P", metaKey: true, bubbles: true }));
      });
      expect(container.querySelector(".nav")?.className).not.toContain("nav-narrow");
      expect(screen.getByRole("combobox", { name: "Find a project" })).toBeTruthy();
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
