import { describe, expect, it } from "vitest";
import { liveRuns, liveRunsStep, STEP_MS } from "./runs-live";

const row = (id: number, status: string) =>
  ({ id, project_id: null, status, mode: "real", created_at: "x", completed_at: null, cost_usd: null, prompt_excerpt: "" }) as never;
const base = [row(2, "running"), row(3, "awaiting_approval"), row(8, "completed")];
const view = (step: number | null) => liveRuns(base, step).map((r) => `${r.id}:${r.status}`);

describe("live runs cycle", () => {
  it("is off without ?runs=live", () => {
    expect(liveRunsStep("", 99_999)).toBeNull();
    expect(liveRunsStep("?runs=live", 2 * STEP_MS + 1)).toBe(2);
    expect(liveRuns(base, null)).toBe(base);
  });

  it("walks arrive, approve, complete, then restarts with a fresh id", () => {
    expect(view(0)).toEqual(["2:running", "3:awaiting_approval", "8:completed"]);
    expect(view(1)).toEqual(["100:running", "2:running", "3:awaiting_approval", "8:completed"]);
    expect(view(2)).toEqual(["100:running", "2:running", "3:running", "8:completed"]);
    expect(view(3)).toEqual(["100:running", "2:completed", "3:running", "8:completed"]);
    expect(view(4)).toEqual(view(0));
    expect(view(5)[0]).toBe("101:running");
  });
});
