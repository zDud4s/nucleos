// @vitest-environment node
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
  "--opacity-ghost",

  "--measure",
  "--measure-prose",
  "--width-column",
  "--well-cap",
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

/**
 * WCAG 2.x contrast, computed from the tokens themselves.
 *
 * Only two literal spellings are accepted, `#rrggbb` and `rgba(r, g, b, a)`,
 * and anything else throws: a token that became a `var()` or a named colour
 * must fail loudly here rather than be skipped and read as a pass.
 */
type Rgba = { r: number; g: number; b: number; a: number };

function colour(v: string | undefined): Rgba {
  if (v === undefined) throw new Error("colour: token is not declared");
  const hex = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(v);
  if (hex) {
    return { r: parseInt(hex[1], 16), g: parseInt(hex[2], 16), b: parseInt(hex[3], 16), a: 1 };
  }
  const rgba = /^rgba\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*([\d.]+)\s*\)$/.exec(v);
  if (rgba) {
    return { r: Number(rgba[1]), g: Number(rgba[2]), b: Number(rgba[3]), a: Number(rgba[4]) };
  }
  throw new Error(`colour: not a #rrggbb or rgba() literal: ${v}`);
}

/** `fg` painted over an opaque `ground`, channel by channel. The result is opaque. */
function over(fg: Rgba, ground: Rgba): Rgba {
  if (ground.a !== 1) throw new Error("over: the ground must be opaque");
  const mix = (f: number, g: number) => f * fg.a + g * (1 - fg.a);
  return { r: mix(fg.r, ground.r), g: mix(fg.g, ground.g), b: mix(fg.b, ground.b), a: 1 };
}

/** WCAG 2.x relative luminance of an opaque colour. */
function luminance(c: Rgba): number {
  if (c.a !== 1) throw new Error("luminance: composite a translucent colour with over() first");
  const lin = (v: number) => {
    const s = v / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b);
}

/** (Lmax + 0.05) / (Lmin + 0.05). */
function contrast(x: Rgba, y: Rgba): number {
  const [lx, ly] = [luminance(x), luminance(y)];
  return (Math.max(lx, ly) + 0.05) / (Math.min(lx, ly) + 0.05);
}

/**
 * The light theme, where these colours are read on white and near-white.
 *
 * AA is 4.5:1 for text at this size and 3:1 for a non-text indicator such as
 * the focus ring (WCAG 1.4.3 and 1.4.11).
 */
describe("the light theme clears AA where it is read", () => {
  const css = read("tokens.css");
  const lightTokens = declarations(css.slice(css.indexOf("@media")));
  const token = (name: string) => colour(lightTokens.get(name));

  it("light --tone-active-fg clears 4.5:1 on its own fill over surface and sunken", () => {
    const fg = token("--tone-active-fg");
    const fill = token("--tone-active-bg");
    for (const groundName of ["--surface", "--surface-sunken"]) {
      const ratio = contrast(fg, over(fill, token(groundName)));
      expect(ratio, `--tone-active-fg on --tone-active-bg over ${groundName}: ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("light --focus-ring clears 3:1 on surface, bg and sunken", () => {
    const ring = token("--focus-ring");
    for (const groundName of ["--surface", "--bg", "--surface-sunken"]) {
      const ground = token(groundName);
      const ratio = contrast(over(ring, ground), ground);
      expect(ratio, `--focus-ring over ${groundName}: ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(3);
    }
  });

  it("light --text-muted clears 4.5:1 on surface and sunken", () => {
    const muted = token("--text-muted");
    for (const groundName of ["--surface", "--surface-sunken"]) {
      const ratio = contrast(muted, token(groundName));
      expect(ratio, `--text-muted on ${groundName}: ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5);
    }
  });

  /**
   * A link is read wherever it lands: in the page headline on the ground, on a card, in a well.
   * `#0b8493` measured 4.10:1 on `--bg` — the headline's link was under the floor while the
   * same colour passed on a card, which is why every ground is asked and not only the white one.
   */
  it("light --accent clears 4.5:1 as link text on bg, surface and sunken", () => {
    const accent = token("--accent");
    for (const groundName of ["--bg", "--surface", "--surface-sunken"]) {
      const ratio = contrast(accent, token(groundName));
      expect(ratio, `--accent on ${groundName}: ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5);
    }
  });

  /** A Held Ember badge is its foreground on its own fill, over whatever the badge sits on. */
  it("light --tone-paused-fg clears 4.5:1 on its own fill over bg, surface and sunken", () => {
    const fg = token("--tone-paused-fg");
    const fill = token("--tone-paused-bg");
    for (const groundName of ["--bg", "--surface", "--surface-sunken"]) {
      const ratio = contrast(fg, over(fill, token(groundName)));
      expect(ratio, `--tone-paused-fg on --tone-paused-bg over ${groundName}: ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5);
    }
  });

  /**
   * Both rules are set at `--text-xs` (11 px), where `--text-faint` is under
   * the 4.5:1 floor; `--text-muted` is the step on the ladder that clears it.
   */
  it("the stat detail and the stale note wear --text-muted", () => {
    const ui = read("ui.css");
    const rules: ReadonlyArray<readonly [string, RegExp]> = [
      [".ui-stat-detail", /\.ui-stat-detail\s*\{([^}]*)\}/],
      [".ui-note-stale", /\.ui-note-stale\s*\{([^}]*)\}/],
    ];
    for (const [selector, pattern] of rules) {
      const rule = pattern.exec(ui);
      expect(rule, `${selector} should have a rule in ui.css`).not.toBeNull();
      expect(rule![1], `${selector} colour`).toMatch(/(^|[\s;])color\s*:\s*var\(--text-muted\)\s*;/);
    }
  });
});
