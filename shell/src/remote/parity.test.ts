import { readdirSync, readFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { NAV_ITEMS } from "../app/nav";
import { PARITY } from "./parity";

const moduleUrl = import.meta.url;
const shellSource = fileURLToPath(new URL("../", moduleUrl));

const skippedDirs = new Set([join(shellSource, "remote"), join(shellSource, "preview")]);

function walk(dir: string, out: string[]): void {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (skippedDirs.has(path)) continue;
      walk(path, out);
    } else if (/\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name)) {
      out.push(relative(shellSource, path).split(sep).join("/"));
    }
  }
}

/**
 * Whether a source file imports (or re-exports) `isRemote` or `isHostOnlyError` from a path ending
 * in `remote/host`. Comments are stripped first, so one that merely names them does not count; an
 * alias, a by-reference use and a re-export all still name the symbol in the statement.
 */
export function branchesOnHost(source: string): boolean {
  const code = source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:"'`])\/\/.*$/gm, "$1");
  const statement = /\b(?:import|export)\s+(?:type\s+)?[^;'"`]*?\bfrom\s*["']([^"']*remote\/host(?:\.[tj]sx?)?)["']/g;
  for (const match of code.matchAll(statement)) {
    if (/\b(?:isRemote|isHostOnlyError)\b/.test(match[0].slice(0, match[0].indexOf(match[1])))) return true;
  }
  return false;
}

describe("branchesOnHost", () => {
  it("sees a plain import", () => {
    expect(branchesOnHost(`import { isRemote } from "../remote/host";`)).toBe(true);
    expect(branchesOnHost(`import { isHostOnlyError } from "../remote/host";`)).toBe(true);
  });
  it("sees an alias", () => {
    expect(branchesOnHost(`import { isRemote as r } from "../remote/host";`)).toBe(true);
  });
  it("sees a use by reference", () => {
    expect(branchesOnHost(`import {\n  isRemote,\n} from "../remote/host";\nxs.filter(isRemote);`)).toBe(true);
  });
  it("sees a re-export", () => {
    expect(branchesOnHost(`export { isRemote } from "./remote/host";`)).toBe(true);
  });
  it("ignores a comment that only names it", () => {
    expect(branchesOnHost(`// import { isRemote } from "../remote/host";\n/* isRemote() */\nconst a = 1;`)).toBe(false);
    expect(branchesOnHost(`/*\nimport { isRemote } from "../remote/host";\n*/`)).toBe(false);
  });
  it("ignores another symbol from the same module", () => {
    expect(branchesOnHost(`import { somethingElse } from "../remote/host";`)).toBe(false);
  });
});

describe("remote parity table", () => {
  const rows = new Map(PARITY.map((row) => [row.where, row]));
  const files: string[] = [];
  walk(shellSource, files);
  const branching = files.filter((file) => branchesOnHost(readFileSync(join(shellSource, file), "utf8")));

  it("has a row for every file that branches on the host", () => {
    expect(branching.length).toBeGreaterThan(0);
    expect(branching.filter((file) => !rows.has(file))).toEqual([]);
  });

  it("has no file row whose file no longer branches (stale)", () => {
    const fileRows = PARITY.filter((row) => !row.where.startsWith("page:")).map((row) => row.where);
    expect(fileRows.filter((file) => !branching.includes(file))).toEqual([]);
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
