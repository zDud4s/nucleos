import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * The armed ring, read out of the sheet.
 *
 * jsdom applies no stylesheet, so a component test can say a button is armed and cannot say
 * anybody could tell. This is where that claim lives. Comments are stripped first: the reason
 * this rule changed is a paragraph naming the colours it used to draw, and a scanner that read
 * prose would fail on the explanation of its own existence.
 */
const ui = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "ui.css"), "utf8").replace(
  /\/\*[\s\S]*?\*\//g,
  "",
);

describe("the armed ring", () => {
  it("the armed ring is one colour, and it is one you can see", () => {
    const outer = /\.ui-confirm-armed\s*>\s*\.ui-button\s*\{([^}]*)\}/.exec(ui);
    expect(outer).not.toBeNull();
    expect(outer?.[1]).toMatch(/box-shadow:\s*0\s+0\s+0\s+3px\s+var\(--text\)/);

    // Two rules and no more: the outer ring, and the inset one for the track that clips it.
    // A third would be a variant answering "is this live?" in its own colour again, which is
    // how 51 of 53 sites came to have a ring at 1.7:1 and 1.2:1.
    const armed = ui.split("}").filter((chunk) => chunk.includes("ui-confirm-armed"));
    expect(armed).toHaveLength(2);
    for (const chunk of armed) {
      expect(chunk).toMatch(/box-shadow:[^;]*var\(--text\)/);
      expect(chunk).not.toMatch(/var\(--border-strong\)|var\(--tone-danger/);
    }
  });
});
