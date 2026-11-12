// @vitest-environment node
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const dir = dirname(fileURLToPath(import.meta.url));
const sources = readdirSync(dir).filter((f) => /\.tsx?$/.test(f) && !/\.test\./.test(f));
const strip = (text: string) => text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|\s)\/\/.*$/gm, "$1");
const code = (file: string) => strip(readFileSync(join(dir, file), "utf8"));
const page = () => strip(readFileSync(join(dir, "../pages/Fleet.tsx"), "utf8"));
// The wire mirror, the one label formatter and the fixture builders may say the word; nothing else may.
const MAY_SAY_HARNESS = new Set(["types.ts", "identity.ts", "test-fixtures.ts"]);

describe("the fleet shell stays harness-blind", () => {
  it("says `harness` only in identity.ts (a label), types.ts (the wire) and the fixtures", () => {
    const files = [
      ...sources.filter((f) => !MAY_SAY_HARNESS.has(f)).map((f) => ({ name: f, text: code(f) })),
      { name: "pages/Fleet.tsx", text: page() },
    ];
    const offenders = files.filter((f) => /\bharness\b/.test(f.text)).map((f) => f.name);
    expect(offenders).toEqual([]);
  });

  it("decides what a node can do from capabilities, never from the harness name", () => {
    const named = sources.filter((f) => /["'`](claude-code|codex-cli|nucleos-runner|daemon)["'`]/.test(code(f)) && f !== "test-fixtures.ts");
    expect(named).toEqual([]);
  });

  it("never injects markup", () => {
    const risky = sources.filter((f) => /dangerouslySetInnerHTML|\.innerHTML|insertAdjacentHTML/.test(code(f)));
    expect(risky).toEqual([]);
  });

  it("has found the sources it is meant to police", () => {
    expect(sources).toContain("identity.ts");
    expect(page().length).toBeGreaterThan(0);
    expect(sources.length).toBeGreaterThan(8);
  });
});
