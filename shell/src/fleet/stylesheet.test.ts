// @vitest-environment node
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("../pages/fleet.css", import.meta.url), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
const rules = [...css.matchAll(/([^{}@]+)\{([^{}]*)\}/g)].map(([, selector, body]) => ({
  selectors: selector.split(",").map((s) => s.trim()),
  body,
}));
const rulesFor = (selector: string) => rules.filter((r) => r.selectors.includes(selector));

describe("fleet.css", () => {
  // xyflow gives a node wrapper inline `pointer-events: none` when nothing on it is selectable or
  // draggable and the flow has no onNodeClick, and the property is inherited: without these rules
  // every button inside a node is dead in the real window, while a DOM test would still click it.
  it("gives the node and the group node their pointer events back", () => {
    for (const selector of [".fleet-node", ".fleet-group"]) {
      expect(rulesFor(selector).some((r) => /pointer-events:\s*auto/.test(r.body)), selector).toBe(true);
    }
  });

  it("moves a project band by transform only, never by animating its size", () => {
    const zone = rulesFor(".fleet-zone").find((r) => /transition:/.test(r.body));
    expect(zone).toBeDefined();
    expect(zone!.body).toMatch(/transition:\s*transform\b/);
    expect(zone!.body).not.toMatch(/\b(width|height)\b[^;]*var\(--dur/);
  });

  it("defines the classes the node, the top bar and the page draw by name", () => {
    for (const selector of [".fleet-st", ".fleet-conn", ".fleet-term-card", ".fleet-node-card", ".fleet-note"]) {
      const defined = rules.some((r) => r.selectors.some((s) => s.split(/\s+/).includes(selector)));
      expect(defined, selector).toBe(true);
    }
  });
});
