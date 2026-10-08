// @vitest-environment node
// §spec novo-frontend
import { describe, expect, it } from "vitest";
import { NAV, NAV_ITEMS, NAV_PATHS, SYSTEM_ITEM, navItemForPath, sliceOf } from "./nav";

/**
 * The sidebar, checked as a table.
 *
 * These assertions are transcriptions of §3.1 of the design, and they are meant
 * to be annoying: an item that quietly disappears — a pillar someone thought
 * was dead, a page nobody got round to — is the failure mode this file exists
 * to catch, and it is invisible in a rendering test that only looks for what it
 * expects to find.
 */
describe("the nav table", () => {
  it("has the three groups of §3.1, in reading order", () => {
    expect(NAV.map((group) => group.id)).toEqual(["operate", "work", "pillars"]);
    expect(NAV.map((group) => group.label)).toEqual(["Operate", "Work", "Pillars"]);
  });

  /**
   * Projects is one plain row in Operate, right after Fleet, and nothing more.
   *
   * It was a group of its own until 2026-10-02, filled at runtime with one row
   * per project. The rail is a fixed list of destinations; the switcher is the
   * one list of projects. A project row creeping back into this table is the
   * regression this test exists to catch.
   */
  it("keeps Projects a plain Operate item, with no project rows in the table", () => {
    const projects = navItemForPath("/projects");
    expect(projects?.label).toBe("Projects");
    expect(sliceOf(projects!)).toBe("Operate");
    expect(NAV[0].items.map((item) => item.id).slice(0, 3)).toEqual(["home", "fleet", "projects"]);
  });

  /**
   * A project path is a parameterised detail route, never a static entry:
   * `router.tsx` builds one route per entry in `NAV_PATHS`.
   */
  it("keeps every path in the route list static", () => {
    expect(NAV_PATHS.some((path) => path.startsWith("/projects/"))).toBe(false);
  });

  it("lists Operate exactly as the design draws it", () => {
    expect(NAV[0].items.map((item) => item.label)).toEqual([
      "Home",
      "Fleet",
      // Not in §3.1's Operate: it was a group of its own until 2026-10-02, and
      // came back here as one row when the switcher took over the project list.
      "Projects",
      "Autopilot",
      "Waiting",
      "Runs",
      "Feed",
      // The owner's notes and the knowledge store in one place; `/learned` redirects here.
      "Brain",
    ]);
  });

  it("lists Work exactly as the design draws it", () => {
    expect(NAV[1].items.map((item) => item.label)).toEqual([
      "Chats",
      "Teams",
      "Agents",
      "Council",
    ]);
  });

  it("lists Pillars exactly as the design draws it", () => {
    expect(NAV[2].items.map((item) => item.label)).toEqual([
      "Mail",
      "Contacts",
      "Calendar",
      "Voice",
      "Web",
      "Files",
    ]);
  });

  it("keeps System out of the groups and last in the flat list", () => {
    // It is not a fourth group: System is where the footer sends you, and
    // grouping it with the pillars would file "the machine is broken" next to
    // "read your mail".
    expect(NAV.flatMap((group) => group.items).map((item) => item.id)).not.toContain("system");
    expect(NAV_ITEMS[NAV_ITEMS.length - 1]).toBe(SYSTEM_ITEM);
  });

  it("carries a badge on the items whose count somebody can clear", () => {
    const badged = NAV_ITEMS.filter((item) => item.badge !== undefined);
    expect(badged.map((item) => [item.id, item.badge])).toEqual([
      ["waiting", "proposals"],
      ["brain", "captures"],
      ["chats", "chats"],
    ]);
    // Mail lost its badge on 2026-09-22 and the reason is on its row in `nav.ts`: it counted
    // untriaged mail, which is the daemon's backlog rather than anything asked of the reader,
    // and a queue row has no "answered" state — so nothing anyone did could clear it.
    expect(NAV_ITEMS.find((item) => item.id === "mail")?.badge).toBeUndefined();
  });

  it("keeps Teams in the sidebar with nothing left holding it back", () => {
    // It carried `disabled: "the núcleo has no team routes yet"` from the
    // foundation until the núcleo mounted them. It has, so the entry is an
    // ordinary one now.
    const teams = navItemForPath("/teams");
    expect(teams).toBeDefined();
    expect(teams?.disabled).toBeUndefined();
  });

  it("gives every item a unique, rooted path", () => {
    for (const path of NAV_PATHS) expect(path.startsWith("/")).toBe(true);
    expect(new Set(NAV_PATHS).size).toBe(NAV_PATHS.length);
    expect(NAV_PATHS).toContain("/");
  });

  it("gives every item a distinct icon, so the collapsed rail is readable", () => {
    // The collapsed rail is the icon and nothing else, so two rows sharing a
    // mark are two rows nobody can tell apart at 56px. This is the same test
    // the two-letter monograms had before 2026-09-05 and it is kept for the
    // same reason: the failure it catches is a *reused* mark, which reviewing a
    // diff of twenty-one one-line entries reliably misses.
    const icons = NAV_ITEMS.map((item) => item.icon);
    expect(new Set(icons).size).toBe(icons.length);
  });

  it("names the slice that brings each page", () => {
    expect(sliceOf(NAV[0].items[1])).toBe("Operate");
    expect(sliceOf(NAV[1].items[0])).toBe("Work");
    expect(sliceOf(NAV[2].items[0])).toBe("Pillars");
    // System belongs to no group and must still answer.
    expect(sliceOf(SYSTEM_ITEM)).toBe("System");
  });

  it("answers with nothing for a path it does not own", () => {
    expect(navItemForPath("/runs/412")).toBeUndefined();
  });

  it("no nav entry points at /errands", () => {
    const paths = [...NAV.flatMap((group) => group.items), ...NAV_ITEMS].map((item) => item.path);
    expect(paths.filter((path) => path.startsWith("/errands"))).toEqual([]);
  });
});
