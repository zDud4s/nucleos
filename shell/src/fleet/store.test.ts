// @vitest-environment node
import { describe, expect, it, vi } from "vitest";
import { FleetStore } from "./store";
import { child, line, metricsOf, node, snapshot } from "./test-fixtures";

function seeded() {
  const store = new FleetStore();
  const a = node("cc:a");
  const b = child("cc:b", a);
  const c = node("cc:c", { project: "other" });
  store.apply(snapshot([a, b, c], { "cc:a": [line("prompt", "go")] }, [metricsOf("cc:a")]));
  return { store, a, b, c };
}

describe("FleetStore", () => {
  it("loads a snapshot: nodes, lines and metrics", () => {
    const { store, a } = seeded();
    expect(store.getEntry("cc:a")?.node).toEqual(a);
    expect(store.getEntry("cc:a")?.lines).toEqual([line("prompt", "go")]);
    expect(store.getEntry("cc:a")?.metrics?.context_tokens).toBe(96_000);
    expect(store.getEntry("cc:b")?.lines).toEqual([]);
    expect(store.hasSnapshot()).toBe(true);
  });

  it("a lines event wakes only its node and never the layout", () => {
    const { store } = seeded();
    const forA = vi.fn(), forB = vi.fn(), layout = vi.fn();
    store.subscribeNode("cc:a", forA);
    store.subscribeNode("cc:b", forB);
    store.subscribeLayout(layout);
    const version = store.getLayoutVersion();
    store.apply({ t: "lines", node_id: "cc:a", lines: [line("tool", "Read x")] });
    expect(forA).toHaveBeenCalledTimes(1);
    expect(forB).not.toHaveBeenCalled();
    expect(layout).not.toHaveBeenCalled();
    expect(store.getLayoutVersion()).toBe(version);
    const shown = store.getEntry("cc:a")?.lines ?? [];
    expect(shown[shown.length - 1]).toEqual(line("tool", "Read x"));
  });

  it("a metrics event wakes only its node and never the layout", () => {
    const { store } = seeded();
    const forA = vi.fn(), forC = vi.fn(), layout = vi.fn();
    store.subscribeNode("cc:a", forA);
    store.subscribeNode("cc:c", forC);
    store.subscribeLayout(layout);
    store.apply({ t: "metrics", metrics: [metricsOf("cc:a", { cost_usd: 1.5 })] });
    expect(forA).toHaveBeenCalledTimes(1);
    expect(forC).not.toHaveBeenCalled();
    expect(layout).not.toHaveBeenCalled();
    expect(store.getEntry("cc:a")?.metrics?.cost_usd).toBe(1.5);
  });

  it("a state-only node event updates the node and announces a transition, without layout", () => {
    const { store, b } = seeded();
    const layout = vi.fn(), seen = vi.fn();
    store.subscribeLayout(layout);
    store.subscribeTransitions(seen);
    store.apply({ t: "node", node: { ...b, state: "idle" } });
    expect(store.getEntry("cc:b")?.node.state).toBe("idle");
    expect(layout).not.toHaveBeenCalled();
    expect(seen).toHaveBeenCalledWith({ node: { ...b, state: "idle" }, from: "busy" });
  });

  it("a structural node event (new node, new parent, leaving busy for done) bumps the layout", () => {
    const { store, a, b } = seeded();
    const layout = vi.fn();
    store.subscribeLayout(layout);
    store.apply({ t: "node", node: child("cc:d", a) });
    expect(layout).toHaveBeenCalledTimes(1);
    store.apply({ t: "node", node: { ...b, parent_id: "cc:c" } });
    expect(layout).toHaveBeenCalledTimes(2);
    store.apply({ t: "node", node: { ...b, parent_id: "cc:c", state: "done" } });
    expect(layout).toHaveBeenCalledTimes(3);
  });

  it("removed drops the whole tree and bumps the layout", () => {
    const { store } = seeded();
    const layout = vi.fn(), forB = vi.fn();
    store.subscribeLayout(layout);
    store.subscribeNode("cc:b", forB);
    store.apply({ t: "removed", node_id: "cc:a" });
    expect(store.getEntry("cc:a")).toBeUndefined();
    expect(store.getEntry("cc:b")).toBeUndefined();
    expect(store.getEntry("cc:c")).toBeDefined();
    expect(layout).toHaveBeenCalledTimes(1);
    expect(forB).toHaveBeenCalledTimes(1);
  });

  it("a snapshot replaces the whole state and keeps unchanged entries by identity", () => {
    const { store, a, c } = seeded();
    const before = store.getEntry("cc:c");
    store.apply(snapshot([a, c]));
    expect(store.getEntry("cc:b")).toBeUndefined();
    expect(store.getEntry("cc:c")).toBe(before);
    expect(store.getEntry("cc:a")?.lines).toEqual([]); // the snapshot said no lines
  });

  it("keeps at most 12 lines per node and reports each batch", () => {
    const { store } = seeded();
    const batch = vi.fn();
    store.onLines("cc:a", batch);
    const many = Array.from({ length: 15 }, (_, i) => line("text", `l${i}`));
    store.apply({ t: "lines", node_id: "cc:a", lines: many });
    expect(store.getEntry("cc:a")?.lines).toHaveLength(12);
    const kept = store.getEntry("cc:a")?.lines ?? [];
    expect(kept[kept.length - 1]?.text).toBe("l14");
    expect(batch).toHaveBeenCalledWith(many);
  });

  it("drops lines and metrics for a node it has not seen", () => {
    const { store } = seeded();
    store.apply({ t: "lines", node_id: "ghost", lines: [line("text", "x")] });
    store.apply({ t: "metrics", metrics: [metricsOf("ghost")] });
    expect(store.getEntry("ghost")).toBeUndefined();
  });

  it("remembers which node spoke last, for Follow", () => {
    const { store } = seeded();
    const spoke = vi.fn();
    store.subscribeActive(spoke);
    store.apply({ t: "lines", node_id: "cc:b", lines: [line("text", "x")] });
    expect(store.getActive()).toBe("cc:b");
    expect(spoke).toHaveBeenCalledTimes(1);
  });

  it("a snapshot that no longer holds the last-active node clears it and tells the listeners", () => {
    const { store, a, c } = seeded();
    store.apply({ t: "lines", node_id: "cc:b", lines: [line("text", "x")] });
    const spoke = vi.fn();
    store.subscribeActive(spoke);
    store.apply(snapshot([a, c]));
    expect(store.getActive()).toBeNull();
    expect(spoke).toHaveBeenCalledTimes(1);
  });

  it("counts consecutive failed reconnects, tells the listeners each time, and starts over on live", () => {
    const store = new FleetStore();
    const heard = vi.fn();
    store.subscribeConnection(heard);
    store.setConnection("reconnecting");
    store.setConnection("reconnecting");
    store.setConnection("reconnecting");
    expect(store.getFailedReconnects()).toBe(3);
    expect(heard).toHaveBeenCalledTimes(3); // the state did not change, the count did
    store.setConnection("live");
    expect(store.getFailedReconnects()).toBe(0);
    store.setConnection("reconnecting");
    expect(store.getFailedReconnects()).toBe(1);
  });

  it("an empty first snapshot still flips hasSnapshot and notifies", () => {
    const store = new FleetStore();
    const layout = vi.fn();
    store.subscribeLayout(layout);
    expect(store.hasSnapshot()).toBe(false);
    store.apply(snapshot([]));
    expect(store.hasSnapshot()).toBe(true);
    expect(layout).toHaveBeenCalledTimes(1);
  });
});
