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
  it("has exactly the three groups of §3.1, in reading order", () => {
    expect(NAV.map((group) => group.id)).toEqual(["operate", "work", "pillars"]);
    expect(NAV.map((group) => group.label)).toEqual(["Operate", "Work", "Pillars"]);
  });

  it("lists Operate exactly as the design draws it", () => {
    expect(NAV[0].items.map((item) => item.label)).toEqual([
      "Home",
      "Fleet",
      "Autopilot",
      "Waiting",
      "Runs",
      "Feed",
      "Projects",
    ]);
  });

  it("lists Work exactly as the design draws it", () => {
    expect(NAV[1].items.map((item) => item.label)).toEqual([
      "Chats",
      "Errands",
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
      "Browser",
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

  it("carries the three badges on the three items the design badges", () => {
    const badged = NAV_ITEMS.filter((item) => item.badge !== undefined);
    expect(badged.map((item) => [item.id, item.badge])).toEqual([
      ["waiting", "proposals"],
      ["chats", "chats"],
      ["mail", "mail"],
    ]);
  });

  it("keeps Teams in the sidebar and says why it cannot work yet", () => {
    // Verified against `core/src/http.rs`: the team tables exist and none of
    // the team routes are mounted. Dropping the item would hide a designed
    // feature; offering it without a note would offer controls that 404.
    const teams = navItemForPath("/teams");
    expect(teams).toBeDefined();
    expect(teams?.disabled).toMatch(/team routes/i);
  });

  it("gives every item a unique, rooted path", () => {
    for (const path of NAV_PATHS) expect(path.startsWith("/")).toBe(true);
    expect(new Set(NAV_PATHS).size).toBe(NAV_PATHS.length);
    expect(NAV_PATHS).toContain("/");
  });

  it("gives every item a distinct glyph, so the collapsed rail is readable", () => {
    // Four items start with C — Chats, Council, Contacts, Calendar — so a rail
    // that derived its monograms from the labels would show the same mark four
    // times.
    const glyphs = NAV_ITEMS.map((item) => item.glyph);
    expect(new Set(glyphs).size).toBe(glyphs.length);
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
});
