import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * The one thing `tailwind.css` copies rather than references, kept honest.
 *
 * Colours reach Tailwind through `@theme inline`, which emits `var()` — one
 * definition, in `tokens.css`, and the light theme keeps working. The fixed
 * scales cannot do that: their token names *are* Tailwind's namespace names, and
 * `--text-xs: var(--text-xs)` is a cycle. So those are written out a second time,
 * and a second copy of a value is a value that drifts.
 *
 * This is the test that makes the drift loud. It reads both stylesheets as plain
 * text — no bundler, nothing rendered — and fails the moment a scale is changed
 * in one file and not the other.
 */

/**
 * Where these stylesheets are.
 *
 * Resolved from `import.meta.url` in two steps rather than with the one-liner
 * `new URL("./x", import.meta.url)`, because the suite runs in jsdom: the global
 * `URL` is then jsdom's, and Node's `fileURLToPath` does not recognise it — it
 * refuses with "The URL must be of scheme file" about a URL whose scheme is file.
 */
const HERE = dirname(fileURLToPath(import.meta.url));

/**
 * A stylesheet with its comments taken out.
 *
 * Both files are heavily commented, and the comments talk *about* the CSS —
 * `tailwind.css` explains the `var()` cycle by writing one out. Left in, that
 * sentence makes a block of literals look like a block of references.
 */

const read = (name: string) =>
  readFileSync(join(HERE, name), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");

/** Every `--name: value;` in a chunk of CSS, as a map. Last wins, as in CSS. */
function declarations(css: string): Map<string, string> {
  const out = new Map<string, string>();
  for (const [, name, value] of css.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
    out.set(name, value.trim().replace(/\s+/g, " "));
  }
  return out;
}

/**
 * The `@theme` block that holds literals, which is the second one — the first
 * only clears namespaces and the `@theme inline` between them holds references.
 */
/**
 * The `@theme` block that holds the duplicated scales.
 *
 * Found by looking for `--text-xs` rather than by counting blocks. There are
 * three plain `@theme` blocks in that file — one clearing namespaces, one
 * readmitting black and white, and this one — and identifying it by position or
 * by count made the test fail the moment a fourth was added for a reason that
 * had nothing to do with drift.
 */
function literalTheme(css: string): Map<string, string> {
  const blocks = [...css.matchAll(/@theme\s*\{([^}]*)\}/g)].map((m) => m[1]);
  const scales = blocks.filter((b) => b.includes("--text-xs:"));
  expect(scales, "tailwind.css should hold one @theme block with the type scale").toHaveLength(1);
  return declarations(scales[0]);
}

/** `tokens.css` declares light under a media query; only the dark base is read. */
function rootTokens(css: string): Map<string, string> {
  const root = css.slice(css.indexOf(":root {"), css.indexOf("@media"));
  return declarations(root);
}

/** Token name in `tokens.css` → theme name in `tailwind.css`. */
const PAIRS: ReadonlyArray<readonly [string, string]> = [
  ["--font-display", "--font-display"],
  ["--font-body", "--font-body"],
  ["--font-mono", "--font-mono"],

  ["--text-xs", "--text-xs"],
  ["--text-sm", "--text-sm"],
  ["--text-base", "--text-base"],
  ["--text-md", "--text-md"],
  ["--text-lg", "--text-lg"],
  ["--text-xl", "--text-xl"],
  ["--text-2xl", "--text-2xl"],
  ["--text-3xl", "--text-3xl"],

  // Tailwind's namespace for weights is spelled out; this app's is not.
  ["--weight-regular", "--font-weight-regular"],
  ["--weight-medium", "--font-weight-medium"],
  ["--weight-semibold", "--font-weight-semibold"],
  ["--weight-bold", "--font-weight-bold"],

  ["--leading-tight", "--leading-tight"],
  ["--leading-snug", "--leading-snug"],
  ["--leading-normal", "--leading-normal"],

  ["--tracking-wide", "--tracking-wide"],
  ["--tracking-wider", "--tracking-wider"],

  ["--radius-sm", "--radius-sm"],
  ["--radius-md", "--radius-md"],
  ["--radius-lg", "--radius-lg"],
  ["--radius-pill", "--radius-pill"],
];

describe("the fixed scales in tailwind.css", () => {
  const tokens = rootTokens(read("tokens.css"));
  const theme = literalTheme(read("tailwind.css"));

  it.each(PAIRS)("%s matches %s", (tokenName, themeName) => {
    const token = tokens.get(tokenName);
    const themed = theme.get(themeName);
    expect(token, `${tokenName} is missing from tokens.css`).toBeDefined();
    expect(themed, `${themeName} is missing from tailwind.css`).toBeDefined();
    expect(themed).toBe(token);
  });

  /**
   * A scale added to `tailwind.css` and never listed above would be unguarded,
   * which is the failure this whole file exists to prevent — so the table has to
   * account for every literal in the block.
   */
  it("has no literal the table above does not cover", () => {
    const covered = new Set(PAIRS.map(([, themeName]) => themeName));
    expect([...theme.keys()].filter((k) => !covered.has(k))).toEqual([]);
  });
});

/**
 * The names that are facts about the layout rather than about the light.
 *
 * The header of `tokens.css` says only colour and shadow are redefined under
 * `prefers-color-scheme: light`, and until this case that sentence was a promise
 * nothing kept. It matters most for these four families: a duration or a
 * `z-index` that differed between the themes would be a bug nobody could see
 * without switching the room's lights on, and a measure that differed would
 * reflow every paragraph in the app at the same moment.
 *
 * Deliberately NOT in the `PAIRS` table above. That table guards the scales
 * Tailwind copies, and nothing in Tailwind needs any of these — adding them to
 * the `@theme` block would create a second copy to keep honest for no caller.
 */
const BASE_ONLY: readonly string[] = [
  "--ease",
  "--dur-quick",
  "--dur-settle",
  "--dur-slide",

  "--z-sticky",
  "--z-cover",
  "--z-overlay",
  "--z-top",

  "--opacity-disabled",
  "--opacity-quiet",

  "--measure",
  "--measure-prose",
  "--width-column",
  "--hairline",
];

/**
 * How many times a chunk of CSS declares one name.
 *
 * Counted rather than looked up, because the assertion is "exactly once": the
 * `declarations` map above is last-wins, like CSS itself, so a name declared
 * twice with different values reads there as a name declared once.
 *
 * `--measure` and `--measure-prose` are two names and not a prefix of each
 * other, because what follows the stem in a declaration is `:` and not `-`. The
 * left guard is for the other direction — a stem that is the *tail* of a longer
 * name, which is the pair this list will grow before it grows the first.
 */
function declaredTimes(css: string, name: string): number {
  return [...css.matchAll(new RegExp(`(^|[^\\w-])${name}\\s*:`, "g"))].length;
}

describe("the tokens that do not belong to a theme", () => {
  const css = read("tokens.css");
  const base = css.slice(css.indexOf(":root {"), css.indexOf("@media"));
  const light = css.slice(css.indexOf("@media"));

  it("declares the motion, stacking, opacity and measure tokens once, in :root only", () => {
    for (const name of BASE_ONLY) {
      expect(declaredTimes(base, name), `${name} in the base :root`).toBe(1);
      expect(declaredTimes(light, name), `${name} under the light media query`).toBe(0);
    }
  });
});
