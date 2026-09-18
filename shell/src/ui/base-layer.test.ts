import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/** The floor is layered, and the exceptions are named. */
const base = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "base.css"), "utf8").replace(
  /\/\*[\s\S]*?\*\//g,
  "",
);

/** Every top-level prelude in `base.css` that is deliberately NOT inside `@layer base`. */
const UNLAYERED_ON_PURPOSE = [
  ".sr-only",
  ":focus-visible",
  ":focus:not(:focus-visible)",
  "@media (prefers-reduced-motion: reduce)",
  "body[data-scroll-locked]",
  ".with-scroll-bars-hidden",
  ".right-scroll-bar-position",
  ".width-before-scroll-bar",
];

/** The prelude of every rule at depth 0, in source order. */
function topLevelPreludes(css: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < css.length; i++) {
    const c = css[i];
    if (c === "{") {
      if (depth === 0) out.push(css.slice(start, i).replace(/\s+/g, " ").trim());
      depth++;
    } else if (c === "}") {
      depth--;
      if (depth === 0) start = i + 1;
    }
  }
  return out;
}

describe("the floor is layered", () => {
  const tops = topLevelPreludes(base);

  it("every rule is inside @layer base, or named in the allowlist above", () => {
    const strays = tops.filter((p) => p !== "@layer base" && !UNLAYERED_ON_PURPOSE.includes(p));
    expect(strays.join(" | "), `unlayered rule(s) in base.css: ${strays.join(" | ")}`).toBe("");
  });

  it("the allowlist is exactly eight preludes, and every one of them is really there", () => {
    expect(UNLAYERED_ON_PURPOSE).toHaveLength(8);
    for (const prelude of UNLAYERED_ON_PURPOSE) expect(tops).toContain(prelude);
  });

  it("the margin reset still exists, and no longer stands above the utilities", () => {
    expect(base).toMatch(/\*\s*\{\s*margin:\s*0;?\s*\}/);
    expect(tops).not.toContain("*");
    expect(tops).not.toContain("*, *::before, *::after");
  });
});
