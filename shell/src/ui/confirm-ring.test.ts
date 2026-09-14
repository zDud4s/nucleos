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

    // Four rules name the armed state now, and exactly two of them DRAW: the outer ring and
    // the inset one for the track that clips it. The other two give the ring a box and move
    // the focus ring off it, and neither paints anything. A third ring would be a variant
    // answering "is this live?" in its own colour again, which is how 51 of 53 sites came to
    // have one at 1.7:1 and 1.2:1.
    const armed = ui.split("}").filter((chunk) => chunk.includes("ui-confirm-armed"));
    expect(armed).toHaveLength(4);

    const rings = armed.filter((chunk) => chunk.includes("box-shadow"));
    expect(rings).toHaveLength(2);
    for (const chunk of rings) {
      expect(chunk).toMatch(/box-shadow:[^;]*var\(--text\)/);
    }
    for (const chunk of armed) {
      expect(chunk).not.toMatch(/var\(--border-strong\)|var\(--tone-danger/);
    }
  });

  it("a quiet interlock has a box for its ring to sit on", () => {
    const quiet = /\.ui-confirm-armed\s*>\s*\.ui-button-quiet\s*\{([^}]*)\}/.exec(ui);
    expect(quiet).not.toBeNull();

    // The primitive's own vertical, so an armed quiet button is as tall as a real one;
    // `--space-1` horizontally, because every container that holds a quiet interlock gives it
    // `gap: var(--space-2)` and a 3px ring at 4px still leaves 1px before the neighbour.
    expect(quiet?.[1]).toMatch(/padding:\s*0\.34rem\s+var\(--space-1\)/);

    // And negated, so the box grows outward from the word and the word does not move. This is
    // the assertion that would catch a row jumping when it arms, which no shot can show.
    expect(quiet?.[1]).toMatch(/margin:\s*-0\.34rem\s+calc\(\s*-1\s*\*\s*var\(--space-1\)\s*\)/);
  });

  it("the focus ring and the armed ring do not share a band", () => {
    const focus = /\.ui-confirm-armed\s*>\s*\.ui-button:focus-visible\s*\{([^}]*)\}/.exec(ui);
    expect(focus).not.toBeNull();

    // The armed shadow occupies 0-3px out and `base.css` draws focus at `outline-offset: 2px`,
    // so 2px of the cyan composited over the ring and measured 1.3:1 against it. At 4px the
    // outline is 4-6px out, on the surface, where it measures 3.9:1.
    expect(focus?.[1]).toMatch(/outline-offset:\s*4px/);
  });

  it("the two labels stack in one cell, so the button is the wider of them", () => {
    const stack = /\.ui-confirm-stack\s*\{([^}]*)\}/.exec(ui);
    expect(stack).not.toBeNull();
    expect(stack?.[1]).toMatch(/display:\s*inline-grid/);

    // Both children in the same area is the whole mechanism: the cell is the wider of the two
    // labels, and the one not showing still occupies it. jsdom applies no stylesheet, so the
    // component test can say a label is `aria-hidden` and only this can say it is invisible.
    const child = /\.ui-confirm-stack\s*>\s*\*\s*\{([^}]*)\}/.exec(ui);
    expect(child?.[1]).toMatch(/grid-area:\s*label/);

    const hidden = /\.ui-confirm-stack\s*>\s*\[aria-hidden="true"\]\s*\{([^}]*)\}/.exec(ui);
    expect(hidden?.[1]).toMatch(/visibility:\s*hidden/);
  });
});
