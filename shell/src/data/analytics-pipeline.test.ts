import { describe, expect, it } from "vitest";
import { isBlockError, pipelinePath } from "./analytics-pipeline";
import { keys } from "./keys";
import { daemonFetch, daemonState, pipelineReport, project } from "../test/harness";

describe("pipeline path, error guard and keys", () => {
  it("asks the global route, or the project's, always with the window", () => {
    expect(pipelinePath(undefined, 7)).toBe("/analytics/pipeline?days=7");
    expect(pipelinePath("a b", 30)).toBe("/projects/a%20b/analytics/pipeline?days=30");
  });

  it("tells a failed block from a good one", () => {
    const report = pipelineReport();
    expect(isBlockError({ error: "query_failed" })).toBe(true);
    expect(isBlockError(report.landing)).toBe(false);
    expect(isBlockError(null)).toBe(false);
  });

  it("keys the pipeline apart from F1's analytics, and never with the word `global`", () => {
    expect(keys.analytics.pipeline(7)).toEqual(["analytics", "pipeline", 7]);
    expect(keys.projects.analyticsPipeline("p", 7)).toEqual(["projects", "p", "analytics-pipeline", 7]);
    expect(keys.analytics.pipeline(7)).not.toEqual(keys.analytics.global(7));
    expect(keys.projects.analyticsPipeline("p", 7)).not.toEqual(keys.projects.analytics("p", 7));
    expect(JSON.stringify(keys.analytics.pipeline(7))).not.toContain("global");
    expect(JSON.stringify(keys.projects.analyticsPipeline("p", 7))).not.toContain("global");
  });
});

describe("the harness answers the pipeline routes", () => {
  it("serves an empty machine: every daily series `days` long, every ratio null", async () => {
    const fetcher = daemonFetch(daemonState({ projects: [project({ project_id: "x" })] }));
    const global = (await fetcher("/analytics/pipeline?days=30")) as ReturnType<typeof pipelineReport>;
    expect(global).toEqual(pipelineReport());
    const scoped = (await fetcher("/projects/x/analytics/pipeline?days=7")) as ReturnType<typeof pipelineReport>;
    expect(scoped.scope).toBe("project");
    expect(scoped.project_id).toBe("x");

    const report = pipelineReport({ days: 7 });
    if (isBlockError(report.landing) || isBlockError(report.gate) || isBlockError(report.postgate)) throw new Error("block");
    if (isBlockError(report.jobs) || report.pressure === null || isBlockError(report.pressure)) throw new Error("block");
    expect(report.landing.daily).toHaveLength(7);
    expect(report.gate.duration_ms_daily).toHaveLength(7);
    expect(report.postgate.red_s_daily).toHaveLength(7);
    expect(report.jobs.daily).toHaveLength(7);
    expect(report.pressure.daily).toHaveLength(7);
    expect(report.landing.bad_share).toBeNull();
    expect(report.gate.pass_rate).toBeNull();
    expect(report.jobs.completion_rate).toBeNull();
    expect(report.landing.conflicts).toBeNull();
    expect(report.gate.cache).toBeNull();
  });

  it("answers a project the roster does not have with the núcleo's 404", async () => {
    const fetcher = daemonFetch(daemonState());
    await expect(fetcher("/projects/ghost/analytics/pipeline?days=7")).rejects.toMatchObject({ status: 404 });
  });
});
