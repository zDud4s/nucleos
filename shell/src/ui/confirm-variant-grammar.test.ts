import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const moduleUrl = import.meta.url;
const repoRoot = fileURLToPath(new URL("../../../", moduleUrl));
const shellSource = join(repoRoot, "shell", "src");

function callSites(source: string): { line: number; attrs: string }[] {
  const sites: { line: number; attrs: string }[] = [];
  const NAME = "<ConfirmButton";
  let at = source.indexOf(NAME);
  while (at !== -1) {
    let i = at + NAME.length;
    let depth = 0;
    let quote: string | null = null;
    for (; i < source.length; i += 1) {
      const c = source[i];
      if (quote !== null) {
        if (c === quote) quote = null;
        continue;
      }
      if (c === '"' || c === "'" || c === "`") quote = c;
      else if (c === "{") depth += 1;
      else if (c === "}") depth -= 1;
      else if (c === ">" && depth === 0) break;
    }
    sites.push({
      line: source.slice(0, at).split("\n").length,
      attrs: source.slice(at + NAME.length, i),
    });
    at = source.indexOf(NAME, i);
  }
  return sites;
}

function tsxFiles(dir: string): string[] {
  return readdirSync(dir, { recursive: true })
    .filter((entry): entry is string => typeof entry === "string" && entry.endsWith(".tsx"))
    .map((entry) => join(dir, entry));
}

describe("ConfirmButton variant grammar", () => {
  it("ConfirmButtonProps requires variant and declares no default", () => {
    const source = readFileSync(join(shellSource, "ui", "ConfirmButton.tsx"), "utf8");

    expect(source).toContain("variant: ButtonVariant;");
    expect(source).not.toMatch(/variant\s*\?:\s*ButtonVariant/);
    expect(source).not.toMatch(/variant\s*=\s*["']danger["']/);
  });

  it("every <ConfirmButton under shell/src names a variant", () => {
    for (const file of tsxFiles(shellSource)) {
      const source = readFileSync(file, "utf8");
      for (const site of callSites(source)) {
        expect(site.attrs, `${file}:${site.line} is missing variant`).toMatch(/variant="([a-z-]+)"/);
      }
    }
  });

  it("Send, Archive, Requeue for triage, Revert and Restart are not danger", () => {
    const expected = new Map([
      ["Send", "approve"],
      ["Archive", "quiet"],
      ["Requeue for triage", "quiet"],
      ["Revert", "quiet"],
      ["Restart", "quiet"],
    ]);

    for (const file of tsxFiles(shellSource)) {
      const source = readFileSync(file, "utf8");
      for (const site of callSites(source)) {
        const label = site.attrs.match(/label="([^"]*)"/)?.[1];
        const expectedVariant = label === undefined ? undefined : expected.get(label);
        if (expectedVariant === undefined) continue;
        const variant = site.attrs.match(/variant="([a-z-]+)"/)?.[1];
        expect(variant, `${file}:${site.line} ${label}`).toBe(expectedVariant);
        expect(variant, `${file}:${site.line} ${label}`).not.toBe("danger");
      }
    }
  });
});
