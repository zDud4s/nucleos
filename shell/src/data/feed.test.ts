import { describe, expect, it } from "vitest";
import {
  FEED_TIMELINE_CAP,
  mergeFeedTimeline,
  newestFeedId,
  timelineQueryString,
  type FeedEntry,
} from "./feed";

function line(id: number, minute: number): FeedEntry {
  return {
    id,
    project_id: null,
    kind: "job_started",
    summary: `line ${id}`,
    run_id: null,
    errand_id: null,
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
