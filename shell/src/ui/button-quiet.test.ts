// @vitest-environment node
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const moduleUrl = import.meta.url.startsWith("file:") ? import.meta.url : `file://${import.meta.url}`;
const css = readFileSync(fileURLToPath(new URL("../ui.css", moduleUrl)), "utf8");
const start = css.indexOf(".ui-button-quiet {");
const block = css.slice(start, css.indexOf("}", start) + 1);
const askStart = css.indexOf(".ui-quiet-ask {");
const askBlock = css.slice(askStart, css.indexOf("}", askStart) + 1);

describe("ui-button-quiet", () => {
  it("one quiet treatment", () => {
    for (const treatment of [block, askBlock]) {
      expect(treatment).toContain("text-decoration: underline");
      expect(treatment).toContain("var(--border-strong)");
      expect(treatment).toContain("var(--text-muted)");
      expect(treatment).not.toContain("var(--accent");
      expect(treatment).not.toContain("var(--tone-");
    }
  });

  it("keeps a 1.75rem target, the icon button is 1.75rem, and create draws a plus", () => {
    expect(block).toContain("min-height: 1.75rem");

    const iconStart = css.indexOf(".ui-icon-button {");
    const icon = css.slice(iconStart, css.indexOf("}", iconStart) + 1);
    expect(iconStart).toBeGreaterThan(-1);
    expect(icon).toContain("1.75rem");

    const createStart = css.indexOf(".ui-button-create::before {");
    expect(createStart).toBeGreaterThan(-1);
    const create = css.slice(createStart, css.indexOf("}", createStart) + 1);
    expect(create).toContain('content: "+"');
  });
});
