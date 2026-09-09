import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const moduleUrl = import.meta.url.startsWith("file:") ? import.meta.url : `file://${import.meta.url}`;
const css = readFileSync(fileURLToPath(new URL("../ui.css", moduleUrl)), "utf8");
const start = css.indexOf(".ui-button-quiet {");
const block = css.slice(start, css.indexOf("}", start) + 1);

describe("ui-button-quiet", () => {
  it("the quiet variant has an affordance at rest and it is not a hue", () => {
    expect(block).toContain("text-decoration: underline");
    expect(block).toContain("var(--border-strong)");
    expect(block).not.toContain("var(--accent");
    expect(block).not.toContain("var(--tone-");
  });
});
