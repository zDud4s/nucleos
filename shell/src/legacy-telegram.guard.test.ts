import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

// Phase-4 guard: the shell no longer knows the v1 Telegram origin.
function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) {
      walk(path, out);
    } else if (/\.(ts|tsx)$/.test(name) && !name.endsWith(".guard.test.ts")) {
      out.push(path);
    }
  }
  return out;
}

function hits(needle: string): string[] {
  const found: string[] = [];
  for (const file of walk(__dirname)) {
    const lines = readFileSync(file, "utf8").split("\n");
    lines.forEach((line, i) => {
      if (line.includes(needle)) found.push(`${file}:${i + 1}`);
    });
  }
  return found;
}

describe("legacy telegram origin is gone from the shell", () => {
  it("has no telegram_origin", () => {
    expect(hits("telegram" + "_origin")).toEqual([]);
  });

  it("has no bare telegram NoteOrigin member", () => {
    expect(hits('"telegram" ' + "|")).toEqual([]);
  });

  it("has no Telegram's own conversations copy", () => {
    expect(hits("Telegram's own " + "conversations")).toEqual([]);
  });
});
