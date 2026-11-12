// @vitest-environment node
import { describe, expect, it } from "vitest";
import { NdjsonSplitter, parseEvent } from "../fleet/ndjson";
import { layoutForest } from "../fleet/layout";
import type { FleetEvent, TranscriptPage } from "../fleet/types";
import { answerFleet, previewSnapshot } from "./fleet-live";

const snap = () => previewSnapshot() as Extract<FleetEvent, { t: "snapshot" }>;

describe("the fleet preview fixtures", () => {
  it("is a well-formed forest: unique ids, known parents, consistent roots and depths", () => {
    const { nodes } = snap();
    const byId = new Map(nodes.map((n) => [n.id, n]));
    expect(byId.size).toBe(nodes.length);
    for (const n of nodes) {
      if (n.parent_id === null) {
        expect([n.root_id, n.depth]).toEqual([n.id, 0]);
      } else {
        const parent = byId.get(n.parent_id)!;
        expect(parent).toBeDefined();
        expect([n.root_id, n.depth]).toEqual([parent.root_id, parent.depth + 1]);
      }
    }
  });

  it("shows every state and the interesting shapes the page must be photographed in", () => {
    const { nodes } = snap();
    const states = new Set(nodes.map((n) => n.state));
    expect([...states].sort()).toEqual(["busy", "done", "idle", "lost"]);
    expect(new Set(nodes.map((n) => n.project)).size).toBeGreaterThanOrEqual(2);
    expect(nodes.some((n) => n.capabilities.stop && n.run_id !== null)).toBe(true);
    // A child on another harness than its parent, for the violet label.
    const byId = new Map(nodes.map((n) => [n.id, n]));
    expect(nodes.some((n) => n.parent_id !== null && byId.get(n.parent_id)!.harness !== n.harness)).toBe(true);
    // More than six done children under one parent: the folded group.
    const folded = nodes.filter((n) => n.state === "done" && n.parent_id !== null);
    const perParent = new Map<string, number>();
    for (const n of folded) perParent.set(n.parent_id!, (perParent.get(n.parent_id!) ?? 0) + 1);
    expect(Math.max(...perParent.values())).toBeGreaterThan(6);
    const out = layoutForest(nodes, { size: "terminal", onlyActive: false, project: null, expanded: new Set() });
    expect(out.nodes.some((n) => n.kind === "group")).toBe(true);
  });

  it("gives every live node twelve or fewer lines, and every finished one at most one", () => {
    const { nodes, lines } = snap();
    for (const n of nodes) {
      const count = (lines[n.id] ?? []).length;
      expect(count).toBeLessThanOrEqual(n.state === "busy" || n.state === "idle" ? 12 : 1);
    }
  });

  it("serves /fleet/live as NDJSON whose first event is the snapshot", async () => {
    const res = answerFleet("/fleet/live")!;
    expect(res.headers.get("content-type")).toBe("application/x-ndjson");
    const reader = res.body!.getReader();
    const { value } = await reader.read();
    await reader.cancel();
    const [first] = new NdjsonSplitter().push(value!);
    expect(parseEvent(first)?.t).toBe("snapshot");
  });

  it("serves a two-page transcript and answers pins with a null-body 204", async () => {
    const page1 = (await answerFleet("/fleet/nodes/claude-code%3Ar1/transcript")!.json()) as TranscriptPage;
    expect(page1.before).not.toBeNull();
    expect(page1.entries.some((e) => e.detail !== null)).toBe(true);
    const page2 = (await answerFleet(`/fleet/nodes/claude-code%3Ar1/transcript?before=${page1.before}`)!.json()) as TranscriptPage;
    expect(page2.before).toBeNull();
    const pin = answerFleet("/fleet/pins/claude-code%3Ar1")!;
    expect([pin.status, pin.body]).toEqual([204, null]);
  });

  it("leaves every other route to the table", () => {
    expect(answerFleet("/fleet/exclusions")).toBeNull();
    expect(answerFleet("/concurrency")).toBeNull();
  });
});
