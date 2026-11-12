import { describe, expect, it } from "vitest";
import { nextRunsCursor, RUN_LIST_LIMIT, runsQuery, type RunSearchResult } from "./runs";

function row(id: number): RunSearchResult {
  return {
    id,
    project_id: "alpha",
    status: "completed",
    mode: "real",
    created_at: "2026-08-17T09:00:00Z",
    completed_at: "2026-08-17T09:02:00Z",
    cost_usd: null,
    prompt_excerpt: `run ${id}`,
  };
}

describe("runs index paging", () => {
  it("asks for the newest page exactly as before paging existed", () => {
    expect(runsQuery({ status: "failed" })).toBe(`?status=failed&limit=${RUN_LIST_LIMIT}`);
  });

  it("asks for an older page through the before cursor", () => {
    expect(runsQuery({ group: "chat" }, 51)).toBe(`?group=chat&limit=${RUN_LIST_LIMIT}&before=51`);
  });

  it("continues after the last row of a full page", () => {
    const page = Array.from({ length: RUN_LIST_LIMIT }, (_, index) => row(100 - index));
    expect(nextRunsCursor(page)).toBe(100 - RUN_LIST_LIMIT + 1);
  });

  it("stops at a short page, the end of the index", () => {
    expect(nextRunsCursor([row(3), row(2)])).toBeUndefined();
    expect(nextRunsCursor([])).toBeUndefined();
  });
});
