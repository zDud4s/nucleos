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
  it("has the three groups of §3.1 plus Projetos, in reading order", () => {
    expect(NAV.map((group) => group.id)).toEqual(["operate", "projects", "work", "pillars"]);
    expect(NAV.map((group) => group.label)).toEqual(["Operate", "Projects", "Work", "Pillars"]);
  });

  /**
   * The workspace design changes the sidebar in exactly one way, and this is the
   * test that holds it to that. "Projetos goes between Operate and Work" is easy
   * to honour while also nudging something else on the way past, and a rail that
   * drifts a little with each feature is how it ends up unrecognisable without
   * any single change having been wrong.
   */
  it("leaves the other three groups untouched", () => {
    const byId = Object.fromEntries(NAV.map((group) => [group.id, group]));
    expect(byId.operate.items.map((item) => item.id)).toEqual([
      // `projects` is gone from here on purpose — it was promoted into the group
      // below, not copied into it. Everything else is exactly as it was.
      "home", "fleet", "autopilot", "waiting", "runs", "feed", "learned",
    ]);
    expect(byId.work.items.map((item) => item.id)).toEqual([
      "chats", "errands", "teams", "agents", "council",
    ]);
    expect(byId.pillars.items.map((item) => item.id)).toEqual([
      "mail", "contacts", "calendar", "voice", "web", "browser", "files",
    ]);
  });

  /**
   * The one group whose contents are not in this table.
   *
   * Every other group's items are known when the app is compiled, which is what
   * lets `NAV_PATHS` be the route list. A project is a row in the daemon's
   * roster, so its path exists only at runtime — the group declares its
   * *position* here and is filled by whoever has the roster. `roster` is how the
   * sidebar knows which group that is, rather than hardcoding an id.
   */
  it("declares Projetos as a position, not a list", () => {
    const projects = NAV.find((group) => group.id === "projects");
    // One static entry — the roster page, which is a fleet-wide reading no
    // single workspace can give — and the project rows are appended to it.
    expect(projects?.items.map((item) => item.path)).toEqual(["/projects"]);
    expect(projects?.roster).toBe(true);
    expect(NAV.filter((group) => group.roster === true)).toHaveLength(1);
  });

  /**
   * The roster group must not smuggle paths into the route list: `router.tsx`
   * builds one route per entry in `NAV_PATHS`, and a project path is a
   * parameterised detail route instead.
   */
  it("keeps every path in the route list static", () => {
    expect(NAV_PATHS.some((path) => path.startsWith("/projects/"))).toBe(false);
  });

  it("lists Operate exactly as the design draws it", () => {
    expect(NAV[0].items.map((item) => item.label)).toEqual([
      "Home",
      "Fleet",
      "Autopilot",
      "Waiting",
      "Runs",
      "Feed",
      // **The one entry that is not a transcription of §3.1.** The knowledge
      // store postdates the design document, and it needs a door: what the
      // agent has been told is decided by a person and read by every later run,
      // and until this page it was reachable only over HTTP. Recorded as an
      // addition rather than folded in silently — the point of this file is
      // that the sidebar does not drift without somebody saying so.
      "Learned",
    ]);
  });

  it("lists Work exactly as the design draws it", () => {
    expect(NAV[2].items.map((item) => item.label)).toEqual([
      "Chats",
      "Errands",
      "Teams",
      "Agents",
      "Council",
    ]);
  });

  it("lists Pillars exactly as the design draws it", () => {
    expect(NAV[3].items.map((item) => item.label)).toEqual([
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

  it("carries a badge on the two items whose count somebody can clear", () => {
    const badged = NAV_ITEMS.filter((item) => item.badge !== undefined);
    expect(badged.map((item) => [item.id, item.badge])).toEqual([
      ["waiting", "proposals"],
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
    expect(sliceOf(NAV[2].items[0])).toBe("Work");
    expect(sliceOf(NAV[3].items[0])).toBe("Pillars");
    // System belongs to no group and must still answer.
    expect(sliceOf(SYSTEM_ITEM)).toBe("System");
  });

  it("answers with nothing for a path it does not own", () => {
    expect(navItemForPath("/runs/412")).toBeUndefined();
  });
});
