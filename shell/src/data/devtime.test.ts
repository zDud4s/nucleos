import { describe, expect, it } from "vitest";
import { ApiRefusal } from "./client";
import { devtimeFindingsPath, devtimeMarkPath, devtimePath } from "./devtime";
import {
  daemonFetch, daemonState, devtimeAgents, devtimeFindings, devtimeOverview, devtimeToolFailures, devtimeTurnClasses,
  devtimeUnexplained, project,
} from "../test/harness";

describe("devtime paths", () => {
  it("puts a project's routes under the project and global ones under /analytics", () => {
    expect(devtimePath(undefined, 7)).toBe("/analytics/devtime?days=7");
    expect(devtimePath("a b", 30)).toBe("/projects/a%20b/analytics/devtime?days=30");
  });

  it("sends only the findings parameters that are set", () => {
    expect(devtimeFindingsPath(undefined, 30, {})).toBe("/analytics/devtime/findings?days=30&order=cost&limit=50");
    expect(devtimeFindingsPath("p", 7, { order: "recent", cursor: "abc=", ruleId: "A1" })).toBe(
      "/projects/p/analytics/devtime/findings?days=7&order=recent&limit=50&rule_id=A1&cursor=abc%3D",
    );
  });

  it("marks globally, whatever view the button sits in", () => {
    expect(devtimeMarkPath("k/1", 90)).toBe("/analytics/devtime/findings/k%2F1/mark?days=90");
  });
});

describe("the fake daemon's devtime routes", () => {
  it("answers the overview with exactly `days` days and a partial today", async () => {
    const answer = daemonFetch(daemonState({ devtime: devtimeOverview({ days: 7 }) }));
    const body = (await answer("/analytics/devtime?days=7")) as ReturnType<typeof devtimeOverview>;
    expect(body.days_series).toHaveLength(7);
    expect(body.days_series[body.days_series.length - 1]?.partial).toBe(true);
    expect(body.days_series.every((day) => day.cost_usd === null)).toBe(true); // null, never 0
    expect(body.tokens.cost_estimated).toBe(true);
  });

  it("leaves `tool_failures` out by default and passes a set block through", async () => {
    const report = devtimeOverview();
    expect("tool_failures" in report).toBe(false); // absent, not null: an older daemon sends no key
    const block = devtimeToolFailures();
    const answer = daemonFetch(daemonState({ devtime: devtimeOverview({ tool_failures: block }) }));
    const body = (await answer("/analytics/devtime?days=30")) as ReturnType<typeof devtimeOverview>;
    expect(body.tool_failures).toEqual(block);
  });

  it("leaves `turn_classes` and `unexplained` out by default and passes set blocks through", async () => {
    const report = devtimeOverview();
    expect("turn_classes" in report).toBe(false); // absent, not null: an older daemon sends no key
    expect("unexplained" in report).toBe(false);
    const classes = devtimeTurnClasses();
    const slow = devtimeUnexplained();
    const answer = daemonFetch(daemonState({ devtime: devtimeOverview({ turn_classes: classes, unexplained: slow }) }));
    const body = (await answer("/analytics/devtime?days=30")) as ReturnType<typeof devtimeOverview>;
    expect(body.turn_classes).toEqual(classes);
    expect(body.unexplained).toEqual(slow);
    expect(JSON.stringify(body.unexplained)).not.toContain("session_id");
  });

  it("leaves `agents` out by default and passes a set block through", async () => {
    const report = devtimeOverview();
    expect("agents" in report).toBe(false); // absent, not null: an older daemon sends no key
    const block = devtimeAgents();
    const answer = daemonFetch(daemonState({ devtime: devtimeOverview({ agents: block }) }));
    const body = (await answer("/analytics/devtime?days=30")) as ReturnType<typeof devtimeOverview>;
    expect(body.agents).toEqual(block);
    expect(JSON.stringify(body.agents)).not.toContain("session_id");
  });

  it("404s a project the roster does not have and scopes a known one", async () => {
    const answer = daemonFetch(daemonState({ projects: [project({ project_id: "alpha" })] }));
    await expect(answer("/projects/nobody/analytics/devtime?days=30")).rejects.toBeInstanceOf(ApiRefusal);
    const body = (await answer("/projects/alpha/analytics/devtime?days=30")) as ReturnType<typeof devtimeOverview>;
    expect(body.scope).toBe("project");
    expect(body.project_id).toBe("alpha");
  });

  it("records a mark and answers the finding's new state, or the refusal the test set", async () => {
    const state = daemonState({ devtimeFindings: devtimeFindings() });
    const answer = daemonFetch(state);
    const key = state.devtimeFindings.items[0].finding_key;
    const out = (await answer(`/analytics/devtime/findings/${key}/mark?days=30`, {
      method: "POST",
      body: JSON.stringify({ verdict: "not_rework", cause: null }),
    })) as { feedback: { verdict: string } };
    expect(out.feedback.verdict).toBe("not_rework");
    expect(state.devtimeMarks).toEqual([{ findingKey: key, verdict: "not_rework", cause: null }]);
    state.devtimeMarkRefusal = { status: 500, code: "failed", detail: "analytics failed" };
    await expect(
      answer(`/analytics/devtime/findings/${key}/mark?days=30`, { method: "POST", body: JSON.stringify({ verdict: "confirmed", cause: null }) }),
    ).rejects.toBeInstanceOf(ApiRefusal);
  });
});
