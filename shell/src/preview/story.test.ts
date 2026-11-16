// @vitest-environment node
import { describe, expect, it } from "vitest";
import { sumWaitingCount } from "../data/waiting";
import type { FleetEvent } from "../fleet/types";
import { STORY_WAITING_COUNT, answerStory, answerStoryFleet, storyActive, storySnapshot } from "./story";

const snap = () => storySnapshot() as Extract<FleetEvent, { t: "snapshot" }>;

describe("the demo story", () => {
  it("is off without the flag", () => {
    expect(storyActive()).toBe(false);
  });

  it("answers only the routes it names", () => {
    expect(answerStory("/files")).toBeUndefined();
    expect(answerStory("/health/readout")).toBeDefined();
  });

  it("forms one tree with a single root and consistent parents, roots and depths", () => {
    const { nodes } = snap();
    const byId = new Map(nodes.map((n) => [n.id, n]));
    expect(byId.size).toBe(nodes.length);
    const roots = nodes.filter((n) => n.parent_id === null);
    expect(roots).toHaveLength(1);
    for (const n of nodes) {
      expect(n.root_id).toBe(roots[0].id);
      if (n.parent_id !== null) expect(n.depth).toBe(byId.get(n.parent_id)!.depth + 1);
    }
    expect(nodes).toHaveLength(7);
    expect(nodes.filter((n) => nodes.every((m) => m.parent_id !== n.id))).toHaveLength(4);
    expect(Math.max(...nodes.map((n) => n.depth))).toBe(2);
    const states = new Set(nodes.map((n) => n.state));
    expect(states.has("busy") && states.has("idle") && states.has("done")).toBe(true);
  });

  it("ties the open decision to a node of that tree and counts it as waiting", () => {
    const { nodes } = snap();
    const open = answerStory("/decisions?status=open") as { tree_id: string; node_id: string }[];
    expect(open).toHaveLength(1);
    const ids = new Set(nodes.map((n) => n.id.split(":")[1]));
    expect(ids.has(open[0].node_id)).toBe(true);
    expect(open[0].tree_id).toBe(nodes.find((n) => n.parent_id === null)!.id.split(":")[1]);
    expect(nodes.find((n) => n.id.split(":")[1] === open[0].node_id)!.state).toBe("idle");
    expect(sumWaitingCount(STORY_WAITING_COUNT)).toBe(open.length);
  });

  it("gives the Waiting page one held action from a node of the tree, and every other queue empty", () => {
    const { nodes } = snap();
    const held = answerStory("/proposals") as { kind: string; status: string; project_id: string; node_id: string }[];
    expect(held).toHaveLength(1);
    expect(held[0]).toMatchObject({ kind: "action-approval", status: "pending", project_id: "invoicer" });
    expect(new Set(nodes.map((n) => n.id.split(":")[1])).has(held[0].node_id)).toBe(true);
    expect(sumWaitingCount(STORY_WAITING_COUNT)).toBe(held.length);
    expect(STORY_WAITING_COUNT).toMatchObject({ wheel: 0, team_actions: 0, recruits: 0, merges: 0, exclusions: 0, git: 0 });
    for (const route of [
      "/browser/sessions", "/proposals/team-actions", "/proposals/recruits", "/contacts/merges",
      "/fleet/exclusions/requests", "/waiting/git", "/vcs/requests", "/proposals/skipped-items", "/proposals/refused-actions",
    ]) {
      expect(answerStory(route)).toEqual([]);
    }
  });

  it("reads healthy on Home: nothing down or degraded, budget not tight, invoicer listed", () => {
    const readout = answerStory("/health/readout") as { status: string; subsystems: { status: string }[] };
    expect(readout.status).toBe("ok");
    expect(readout.subsystems.every((s) => s.status === "ok" || s.status === "disabled")).toBe(true);
    const budget = answerStory("/autopilot/budget") as { limit_usd: number; window_spend_usd: number; paused: boolean };
    expect(budget.paused).toBe(false);
    expect(budget.window_spend_usd / budget.limit_usd).toBeLessThan(0.5);
    const projects = answerStory("/projects") as { project_id: string }[];
    expect(projects.some((p) => p.project_id === "invoicer")).toBe(true);
  });

  it("quiets the sidebar: no captures, no held notifications, a comfortable quota", () => {
    expect(answerStory("/capture-requests")).toEqual([]);
    expect(answerStory("/notifications/pending")).toEqual([]);
    const q = answerStory("/quota") as { providers: { windows: { used_fraction: number }[] }[] };
    expect(q.providers.flatMap((p) => p.windows).every((w) => w.used_fraction < 0.5)).toBe(true);
  });

  it("streams a snapshot on /fleet/live", async () => {
    const res = answerStoryFleet("/fleet/live")!;
    const reader = res.body!.getReader();
    const { value } = await reader.read();
    await reader.cancel();
    expect(JSON.parse(new TextDecoder().decode(value!).trim()).t).toBe("snapshot");
  });
});
