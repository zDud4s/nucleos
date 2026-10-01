import { describe, expect, it } from "vitest";
import {
  FEED_TIMELINE_CAP,
  NOTIFY_FAMILIES,
  groupKinds,
  mergeFeedTimeline,
  newestFeedId,
  timelineQueryString,
  type FeedEntry,
  type NotifyPolicy,
} from "./feed";

function line(id: number, minute: number): FeedEntry {
  return {
    id,
    project_id: null,
    kind: "job_started",
    summary: `line ${id}`,
    run_id: null,
    subject: null,
    created_at: new Date(Date.UTC(2026, 7, 24, 9, minute)).toISOString(),
  };
}

describe("the timeline's wire", () => {
  it("sends the window, and the cursor only when there is one", () => {
    expect(timelineQueryString({ since: "2026-08-24T00:00:00.000Z" })).toBe("?since=2026-08-24T00%3A00%3A00.000Z");
    const polled = new URLSearchParams(timelineQueryString({ since: "a", until: "b" }, 41).slice(1));
    expect(polled.get("until")).toBe("b");
    expect(polled.get("after_id")).toBe("41");
  });

  it("the cursor is the newest id held, whatever order the lines are in", () => {
    expect(newestFeedId([])).toBeNull();
    expect(newestFeedId([line(7, 3), line(9, 1), line(8, 2)])).toBe(9);
  });
});

describe("folding a poll into what is held", () => {
  it("adds new lines in the route's order and keeps each id once", () => {
    const held = { entries: [line(1, 1), line(2, 2)], truncated: false };
    // A line stamped a moment late arrives past the cursor but before the newest by time.
    const merged = mergeFeedTimeline(held, { entries: [line(2, 2), line(4, 4), line(3, 3)], truncated: false });
    expect(merged.entries.map((entry) => entry.id)).toEqual([1, 2, 3, 4]);
    expect(merged.truncated).toBe(false);
  });

  it("an empty poll keeps the same object, so nothing re-renders for nothing", () => {
    const held = { entries: [line(1, 1)], truncated: false };
    expect(mergeFeedTimeline(held, { entries: [], truncated: false })).toBe(held);
  });

  it("re-applies the cap, drops the oldest, and remembers that it did", () => {
    const many = Array.from({ length: FEED_TIMELINE_CAP }, (_, i) => ({ ...line(i + 1, 0), created_at: new Date(Date.UTC(2026, 7, 20) + i * 1000).toISOString() }));
    const merged = mergeFeedTimeline({ entries: many, truncated: false }, { entries: [line(FEED_TIMELINE_CAP + 1, 30)], truncated: false });
    expect(merged.entries).toHaveLength(FEED_TIMELINE_CAP);
    expect(merged.entries[0].id).toBe(2);
    expect(merged.truncated).toBe(true);
    // Sticky: the next poll, under the cap, does not bring the dropped line back into the claim.
    expect(mergeFeedTimeline(merged, { entries: [line(FEED_TIMELINE_CAP + 2, 31)], truncated: false }).truncated).toBe(true);
  });
});

const NO_POLICY: NotifyPolicy = { families: [], kinds: [] };

/**
 * The whole notifications tab is drawn from this function's return, so what the
 * screen can possibly show is decided here.
 *
 * The fixture deliberately includes kinds no table of this shell reads —
 * `team_message`, `web.fetched` — and every assertion about them would fail on a
 * screen built from a table instead of from the núcleo, which is what makes this
 * more than a restatement of the code.
 */
describe("groupKinds", () => {
  const observed = [
    "job_started",
    "job_failed",
    "team_message",
    "web.fetched",
    "config_written",
    "token_efficiency",
  ];

  it("puts each observed kind under the family whose prefix claims it", () => {
    const { families, loose } = groupKinds(observed, NO_POLICY);
    const byPrefix = new Map(families.map((f) => [f.selector, f]));

    expect(byPrefix.get("job_")!.kinds.map((k) => k.kind)).toEqual(["job_failed", "job_started"]);
    // The case a table of kinds cannot answer: `team_message` is not in one.
    expect(byPrefix.get("team_")!.kinds.map((k) => k.kind)).toEqual(["team_message"]);
    expect(byPrefix.get("web.")!.kinds.map((k) => k.kind)).toEqual(["web.fetched"]);
    // And the two with no family prefix fall out on their own, in order.
    expect(loose.map((k) => k.kind)).toEqual(["config_written", "token_efficiency"]);
  });

  it("shows a family with no kinds rather than hiding it", () => {
    const { families } = groupKinds(observed, NO_POLICY);
    const council = families.find((f) => f.selector === "council_");

    // "council: 0 kinds" says this machine has written none in ninety days,
    // which is information. A missing row would read as a bug, and its switch
    // still covers whatever it writes next.
    expect(council).toBeDefined();
    expect(council!.kinds).toEqual([]);
    expect(families).toHaveLength(NOTIFY_FAMILIES.length);
  });

  it("reads absence as no rule, not as an allowing rule", () => {
    const { families, loose } = groupKinds(observed, NO_POLICY);

    expect(families.every((f) => f.rule === null)).toBe(true);
    expect(loose.every((k) => k.verdict === "inherit")).toBe(true);
  });

  it("carries each switch's state through from the policy", () => {
    const { families, loose } = groupKinds(observed, {
      families: [{ selector: "job_", enabled: false }],
      kinds: [
        { selector: "job_failed", enabled: true },
        { selector: "config_written", enabled: false },
      ],
    });
    const jobs = families.find((f) => f.selector === "job_")!;

    expect(jobs.rule).toBe(false);
    expect(jobs.kinds.find((k) => k.kind === "job_failed")!.verdict).toBe("always");
    expect(jobs.kinds.find((k) => k.kind === "job_started")!.verdict).toBe("inherit");
    expect(loose.find((k) => k.kind === "config_written")!.verdict).toBe("never");
  });

  /**
   * The union of §4.2. The feed is pruned at ninety days, so a rule written a
   * year ago can name a kind that no longer appears in `observed`. Without this
   * the rule keeps silencing with no row on screen to undo it.
   */
  it("keeps a stored kind rule on screen after its kind falls out of the window", () => {
    const { families } = groupKinds(observed, {
      families: [],
      kinds: [{ selector: "job_gc_failed", enabled: false }],
    });
    const jobs = families.find((f) => f.selector === "job_")!;
    const stale = jobs.kinds.find((k) => k.kind === "job_gc_failed");

    expect(stale).toBeDefined();
    expect(stale!.recentlySeen).toBe(false);
    expect(stale!.verdict).toBe("never");
    expect(jobs.kinds.find((k) => k.kind === "job_started")!.recentlySeen).toBe(true);
  });

  it("draws a stored family this build has no name for, by its prefix", () => {
    const { families } = groupKinds(["whatsit_happened"], {
      families: [{ selector: "whatsit_", enabled: false }],
      kinds: [],
    });
    const unknown = families.find((f) => f.selector === "whatsit_")!;

    expect(unknown.label).toBeNull();
    expect(unknown.rule).toBe(false);
    expect(unknown.kinds.map((k) => k.kind)).toEqual(["whatsit_happened"]);
  });

  it("files a kind under the longest matching prefix, as the sidecar resolves it", () => {
    const { families } = groupKinds(["job_item_done"], {
      families: [{ selector: "job_item_", enabled: false }],
      kinds: [],
    });

    expect(families.find((f) => f.selector === "job_item_")!.kinds.map((k) => k.kind)).toEqual([
      "job_item_done",
    ]);
    expect(families.find((f) => f.selector === "job_")!.kinds).toEqual([]);
  });

  it("ignores an empty selector rather than letting it claim everything", () => {
    // An empty prefix matches every kind. The núcleo refuses one at the door
    // and the sidecar ignores one; the screen must not draw a family that
    // swallows the whole list either.
    const { families, loose } = groupKinds(observed, {
      families: [{ selector: "", enabled: false }],
      kinds: [{ selector: "", enabled: false }],
    });

    expect(families).toHaveLength(NOTIFY_FAMILIES.length);
    expect(loose.map((k) => k.kind)).toEqual(["config_written", "token_efficiency"]);
  });
});

/**
 * One family is exactly one prefix — the invariant that lets a family switch
 * write a single rule and keeps "half on" off the screen. A second entry
 * sharing a prefix would silently make two switches fight over one rule.
 */
describe("NOTIFY_FAMILIES", () => {
  it("has one entry per prefix, and no blank ones", () => {
    const selectors = NOTIFY_FAMILIES.map((f) => f.selector);
    expect(new Set(selectors).size).toBe(selectors.length);
    expect(selectors.every((s) => s.length > 0)).toBe(true);
    expect(NOTIFY_FAMILIES.every((f) => f.label.length > 0)).toBe(true);
  });
});
