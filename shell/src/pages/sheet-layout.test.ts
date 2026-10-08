// @vitest-environment node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const sheet = (name: string) =>
  readFileSync(join(dirname(fileURLToPath(import.meta.url)), name), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
const feed = sheet("feed.css");
const autopilot = sheet("autopilot.css");
const body = (css: string, rule: RegExp) => rule.exec(css)?.[1] ?? "";
// Written plainly since round 12: `scripts/css-contract.mjs` masks `--custom-property` literals,
// so this no longer surfaces as a class nobody defined. It used to be built from three fragments
// to dodge that, with nothing here to say so.
const feedControlHeight = "--feed-control-h";

describe("two layout facts a stylesheet owns", () => {
  it("every control in the feed filter bar is one height, declared once", () => {
    expect(feed.match(new RegExp(`${feedControlHeight}:`, "g")) ?? []).toHaveLength(1);
    expect(feed).toContain(
      `${feedControlHeight}: calc(var(--text-sm) * var(--leading-normal) + 2 * var(--space-2) + 2px)`,
    );
    expect(body(feed, /\.feed-filter input,\s*\.feed-filter select\s*\{([^}]*)\}/)).toMatch(
      new RegExp(`height:\\s*var\\(${feedControlHeight}\\)`),
    );
    expect(body(feed, /\.feed-filter-submit\s*>\s*button\s*\{([^}]*)\}/)).toMatch(
      new RegExp(`min-height:\\s*var\\(${feedControlHeight}\\)`),
    );
  });

  it("the ask form is separated from the jobs list by a rule of its own", () => {
    const form = body(autopilot, /\.ap-form\s*\{([^}]*)\}/);
    expect(form).toMatch(/border-top:\s*1px solid var\(--border\)/);
    expect(form).toMatch(/padding-top:\s*var\(--space-4\)/);
    expect(form).toMatch(/margin-top:\s*var\(--space-4\)/);
  });
});
