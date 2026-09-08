import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * DESIGN.md reserves cyan for links. The earlier audit used `rg -- "--accent"`,
 * which cannot see Tailwind utility classes and left twelve non-link uses uncounted.
 */
const moduleUrl = import.meta.url.startsWith("file:") ? import.meta.url : `file://${import.meta.url}`;
const shellSource = fileURLToPath(new URL("./", moduleUrl));
const ACCENT = /\b(?:text|border|bg|stroke|fill|ring|outline)-accent(?:\/\d+)?\b/g;

function tsxFiles(dir: string): string[] {
  return readdirSync(dir, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(".tsx"))
    .map((entry) => join(entry.parentPath, entry.name));
}

describe("Reserved Cyan", () => {
  it("appears only on underlined-hover links", () => {
    const violations: string[] = [];
    for (const file of tsxFiles(shellSource)) {
      for (const [index, line] of readFileSync(file, "utf8").split("\n").entries()) {
        for (const hit of line.matchAll(ACCENT)) {
          if (hit[0] !== "text-accent" || !line.includes("hover:underline")) {
            violations.push(`${file}:${index + 1}: ${line.trim()}`);
          }
        }
      }
    }
    expect(violations, violations.join("\n")).toEqual([]);
  });
});
