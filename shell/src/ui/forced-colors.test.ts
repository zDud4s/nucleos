// @vitest-environment node
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const moduleUrl = import.meta.url.startsWith("file:") ? import.meta.url : `file://${import.meta.url}`;
const css = readFileSync(fileURLToPath(new URL("../ui.css", moduleUrl)), "utf8");
const tokens = readFileSync(fileURLToPath(new URL("../tokens.css", moduleUrl)), "utf8");

/** The body of the first `@media (forced-colors: active) { ... }` block, braces balanced. */
function forcedColorsBlock(): string {
  const start = css.indexOf("@media (forced-colors: active)");
  if (start === -1) return "";
  const open = css.indexOf("{", start);
  let depth = 0;
  for (let i = open; i < css.length; i += 1) {
    if (css[i] === "{") depth += 1;
    if (css[i] === "}") {
      depth -= 1;
      if (depth === 0) return css.slice(open, i + 1);
    }
  }
  return "";
}

describe("forced colors", () => {
  it("ui.css answers forced-colors for badges, focus ring, gauge and pressed switch", () => {
    const block = forcedColorsBlock();
    expect(block).not.toBe("");
    expect(block).toContain(".ui-badge");
    expect(block).toContain(":focus-visible");
    expect(block).toContain(".ui-gauge-track");
    expect(block).toContain(".ui-gauge-fill");
    expect(block).toContain('.ui-switch-seg[aria-pressed="true"]');
  });

  it("field borders use --border-field, and the token is defined", () => {
    expect(tokens).toContain("--border-field:");
    for (const control of ["input", "select", "textarea"]) {
      const at = css.search(new RegExp(String.raw`\.ui-field\s+${control}\b[^{]*\{[^}]*var\(--border-field\)`));
      expect(at, `.ui-field ${control} border`).toBeGreaterThan(-1);
    }
  });
});
