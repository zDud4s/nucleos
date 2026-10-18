import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * A `<Field label=...>` names its control with the visible label. An `aria-label` on the child
 * says a second, possibly different name, so the screen reader and the eye disagree. This catches
 * the child of a `Field` carrying one. The allowlist is a ratchet: unlisted files fail, and a
 * listed file that no longer violates fails too, so it can only shrink (emptied in W3).
 */
const shellSource = join(dirname(fileURLToPath(import.meta.url)), "..");

/** Files that violate today. Each is removed by the W3 page batch that drops the aria-label. */
const ALLOWED = new Set<string>(["pages/Files.tsx", "pages/Fleet.tsx", "pages/Mail.tsx", "pages/Runs.tsx"]);

/** Source of every `<Field ...>...</Field>` block that has a `label` attribute, with its line. */
export function fieldBlocks(source: string): { line: number; body: string }[] {
  const blocks: { line: number; body: string }[] = [];
  const open = /<Field\b/g;
  let m: RegExpExecArray | null;
  while ((m = open.exec(source)) !== null) {
    const end = source.indexOf("</Field>", m.index);
    if (end === -1) continue;
    const body = source.slice(m.index, end);
    const tagEnd = body.indexOf(">");
    // The opening tag may hold `=>` or `>` inside braces; walk it with depth and quotes.
    let depth = 0; let quote: string | null = null; let i = "<Field".length;
    for (; i < body.length; i += 1) {
      const c = body[i];
      if (quote !== null) { if (c === quote) quote = null; continue; }
      if (c === '"' || c === "'" || c === "`") quote = c;
      else if (c === "{") depth += 1; else if (c === "}") depth -= 1;
      else if (c === ">" && depth === 0) break;
    }
    void tagEnd;
    if (!/\blabel=/.test(body.slice(0, i))) continue;
    blocks.push({ line: source.slice(0, m.index).split("\n").length, body: body.slice(i) });
  }
  return blocks;
}

export const violates = (source: string): boolean => fieldBlocks(source).some((b) => /\baria-label=/.test(b.body));

function files(): string[] {
  return readdirSync(shellSource, { recursive: true })
    .filter((e): e is string => typeof e === "string" && e.endsWith(".tsx") && !e.endsWith(".test.tsx"))
    .map((e) => join(shellSource, e));
}
const keyOf = (file: string) => relative(shellSource, file).split(sep).join("/");

describe("Field label authorship", () => {
  it("the detector sees a Field whose child carries an aria-label", () => {
    expect(violates(`<Field label="Name"><input aria-label="Full name" /></Field>`)).toBe(true);
    expect(violates(`<Field label="Name" helper={<b>x</b>}>\n<input id="n" />\n</Field>`)).toBe(false);
    expect(violates(`<Field label="Name" labelHidden><input /></Field>`)).toBe(false);
    expect(violates(`<input aria-label="alone" />`)).toBe(false);
  });

  it("no page names a Field's control with an aria-label beside the visible label", () => {
    for (const file of files()) {
      const key = keyOf(file);
      if (ALLOWED.has(key)) continue;
      const hit = fieldBlocks(readFileSync(file, "utf8")).find((b) => /\baria-label=/.test(b.body));
      expect(hit, `${key}:${hit?.line}`).toBeUndefined();
    }
  });

  it("the allowlist is a ratchet: every listed file still violates", () => {
    for (const key of ALLOWED) expect(violates(readFileSync(join(shellSource, key), "utf8")), key).toBe(true);
  });
});
