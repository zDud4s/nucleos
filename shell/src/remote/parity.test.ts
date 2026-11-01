import { readdirSync, readFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { NAV_ITEMS } from "../app/nav";
import { PARITY } from "./parity";

const moduleUrl = import.meta.url;
const shellSource = fileURLToPath(new URL("../", moduleUrl));

function walk(dir: string, out: string[]): void {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "remote" || entry.name === "preview") continue;
      walk(path, out);
    } else if (/\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name)) {
      out.push(relative(shellSource, path).split(sep).join("/"));
    }
  }
}

describe("remote parity table", () => {
  const rows = new Map(PARITY.map((row) => [row.where, row]));

  it("has a row for every file that branches on isRemote()", () => {
    const files: string[] = [];
    walk(shellSource, files);
    const branching = files.filter((file) =>
      /\bisRemote\s*\(/.test(readFileSync(join(shellSource, file), "utf8")),
    );
    expect(branching.length).toBeGreaterThan(0);
    expect(branching.filter((file) => !rows.has(file))).toEqual([]);
  });

  it("has a row for every nav page", () => {
    expect(NAV_ITEMS.map((item) => `page:${item.id}`).filter((key) => !rows.has(key))).toEqual([]);
  });

  it("has no stale row and every row says why", () => {
    const known = new Set([...NAV_ITEMS.map((item) => `page:${item.id}`)]);
    const files: string[] = [];
    walk(shellSource, files);
    for (const file of files) known.add(file);
    for (const row of PARITY) {
      expect(known.has(row.where), `${row.where} names nothing`).toBe(true);
      expect(row.why.trim(), `${row.where} has no reason`).not.toBe("");
    }
  });
});
