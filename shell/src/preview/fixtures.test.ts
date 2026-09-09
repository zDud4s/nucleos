import { describe, expect, it } from "vitest";
import { READINESS_MIN_AGREE_PERCENT, READINESS_MIN_REVIEWED } from "../data/autopilot";
import { FEED, PROJECTS, SCOREBOARD, VCS_REQUESTS } from "./daemon";
import { readFeedKind } from "../data/feed";

/**
 * The states the preview must be able to photograph, pinned as facts about the fixtures.
 *
 * Not a test of the app — nothing under `src/` imports the preview daemon, and this file is the
 * only thing in the suite that does. It exists because a fixture is the only reason a shot shows
 * anything, and a state with no fixture is a state nobody has ever seen: the third mode segment
 * was disabled on all four projects, so no shot had ever photographed it enabled, and the armed
 * label that wrapped and grew the roster row shipped unseen. A missing fixture fails silently by
 * construction — the daemon answers `[]` for a route it has no branch for, and an empty panel is
 * a legitimate state — so the claim has to be made here, out loud.
 */
describe("the preview fixtures", () => {
  it("the feed serves lines across kinds, and exactly one the shell cannot read", () => {
    expect(FEED.length).toBeGreaterThanOrEqual(12);
    expect(FEED.filter((row) => readFeedKind(row.kind) === null).map((row) => row.kind)).toEqual(["map_stamp_recorded"]);
    for (const kind of ["job_finished", "job_started", "job_failed", "vcs_request_finished", "email_urgent", "token_efficiency", "web.read"]) expect(FEED.some((row) => row.kind === kind), kind).toBe(true);
    expect(new Set(FEED.map((row) => readFeedKind(row.kind)?.tone)).size).toBeGreaterThanOrEqual(4);
  });
  /*
    Exactly one, and it is the one the shots point at.

    One, because the roster has to photograph both answers at once: a project that has earned the
    third segment and three that have not. `10-project-state` and `11-project-code` shoot
    `/projects/alpha/...`, so the promotable row must be `alpha` and not any other — a state that
    has to be photographed on a project page can only live on the project that page opens.
  */
  it("has exactly one promotable project, and it is alpha", () => {
    const promotable = PROJECTS.filter((project) => project.promotable);
    expect(promotable.map((project) => project.project_id)).toEqual(["alpha"]);
  });

  /*
    And it is consistent with the numbers beside it, without the shell recomputing anything.

    `promotable` is the daemon's arithmetic and the shell only prints it — but a fixture that
    claimed promotable while its own columns said "3 of 5 classes still short of the bar" would
    photograph a row contradicting itself, which is a worse lie than a missing state. `shadow`
    because promotion is the crossing from shadow to active: a project already `active` has
    nothing left to earn, and an `off` one has recorded no evidence at all.
  */
  it("the promotable project is in shadow with every class clearing the bar", () => {
    const alpha = PROJECTS.find((project) => project.project_id === "alpha");
    expect(alpha).toBeDefined();
    expect(alpha?.mode).toBe("shadow");
    expect(alpha?.classes_ready).toBe(alpha?.classes_total);
    expect(alpha?.classes_total).toBeGreaterThan(0);
    // Restraint is the other half of the bar: at least one class the classifier withheld.
    expect(alpha?.withheld_classes_ready ?? 0).toBeGreaterThan(0);
  });

  /*
    The blocker keeps a row to be photographed in.

    Moving the 2-of-5 classes off alpha would otherwise have taken the "still short of the bar"
    sentence out of every shot — trading one unphotographed state for another.
  */
  it("still has a project short of the bar, so the blocker is photographed", () => {
    const short = PROJECTS.filter(
      (project) => project.classes_total > 0 && project.classes_ready < project.classes_total,
    );
    expect(short.length).toBeGreaterThan(0);
    expect(short.every((project) => !project.promotable)).toBe(true);
  });

  it("alpha's scoreboard agrees with its roster figures", () => {
    const alpha = PROJECTS.find((project) => project.project_id === "alpha");
    const alphaRows = SCOREBOARD.alpha;
    expect(alphaRows).toHaveLength(5);
    expect(alphaRows.every((row) => row.mode === "shadow")).toBe(true);
    expect(alphaRows.every((row) => row.reviewed >= READINESS_MIN_REVIEWED)).toBe(true);
    expect(
      alphaRows.every((row) => (row.agree / row.reviewed) * 100 >= READINESS_MIN_AGREE_PERCENT),
    ).toBe(true);
    expect(alphaRows.filter((row) => row.would_allow === 0).length).toBeGreaterThanOrEqual(2);
    expect(alphaRows).toHaveLength(alpha?.classes_total ?? 0);

    const deltaClearing = SCOREBOARD.delta.filter(
      (row) =>
        row.reviewed >= READINESS_MIN_REVIEWED &&
        (row.agree / row.reviewed) * 100 >= READINESS_MIN_AGREE_PERCENT,
    );
    expect(deltaClearing).toHaveLength(2);
  });

  /*
    The git queue answers something, and answers both kinds of row.

    `preview/daemon.ts` falls through to `[]` for any GET it has no branch for, so a missing route
    renders the empty state and the shot still looks correct. This route had no branch at all: the
    panel that lists every push, merge and rebase had never been seen holding anything.
  */
  it("serves a git queue holding rows that want a person and rows that do not", () => {
    expect(VCS_REQUESTS.length).toBeGreaterThan(0);
    const statuses = VCS_REQUESTS.map((row) => row.status);
    expect(statuses).toContain("escalated");
    expect(statuses).toContain("blocked");
    expect(statuses.some((status) => status !== "escalated" && status !== "blocked")).toBe(true);
    // Every row names a project, or `onlyProject()` drops it before the page ever draws it.
    expect(VCS_REQUESTS.every((row) => row.project_id !== "")).toBe(true);
  });
});
