import { readFileSync, readdirSync } from "node:fs";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * DESIGN.md reserves cyan for links. The earlier audit used `rg -- "--accent"`,
 * which cannot see Tailwind utility classes and left twelve non-link uses uncounted.
 */
const moduleUrl = import.meta.url.startsWith("file:") ? import.meta.url : `file://${import.meta.url}`;
const shellSource = fileURLToPath(new URL("./", moduleUrl));
const ACCENT = /\b(?:text|border|bg|stroke|fill|ring|outline)-accent(?:\/\d+)?\b/g;
const DEAD_DANGER = /\b(?:text|border|bg|stroke|fill|ring|outline)-danger\b/g;
const CSS_ACCENT = /var\(--accent(?:-quiet|-ink)?\)/g;

// DESIGN.md exempts the quiet form only as a current row's fill. `only` keeps a
// selector-shaped exception from becoming a hole for any future cyan in that file.
const ALLOWED = new Map<string, { reason: string; only?: string }>([
  ["base.css:a", { reason: "the bare anchor, the rule's own case" }],
  ["ui.css:.ui-button-link", { reason: "the shared link button" }],
  ["autopilot.css:.ap-link", { reason: "a Link" }],
  ["chats.css:.chats-rich-link", { reason: "a button that opens a URL through the OS" }],
  ["feed.css:.feed-link", { reason: "a run link, and the page a line's question is answered on" }],
  ["fleet.css:.fleet-card-link", { reason: "a fleet card link" }],
  ["projects.css:.pj-match-path", { reason: "a project path link" }],
  ["waiting.css:.waiting-row-link", { reason: "a waiting row link" }],
  [
    "app.css:.nav-item-active",
    { reason: "the rail's current row: where you are is identity, not state", only: "var(--accent-quiet)" },
  ],
  [
    "system.css:.sy-settings-item-current",
    { reason: "the System settings list's current row, the same case as the rail's", only: "var(--accent-quiet)" },
  ],
  [
    "system.css:.sy-token-level:has(input:checked)",
    { reason: "the chosen token level: the current choice in a set, like a current row", only: "var(--accent-quiet)" },
  ],
  ["contacts.css:.contacts-avatar", { reason: "the contact's initials disc, the owner's call on 2026-10-05" }],
]);

function tsxFiles(dir: string): string[] {
  return readdirSync(dir, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(".tsx"))
    .map((entry) => join(entry.parentPath, entry.name));
}

function sourceFiles(dir: string): string[] {
  return readdirSync(dir, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile() && /\.(?:ts|tsx)$/.test(entry.name) && !/\.test\./.test(entry.name))
    .map((entry) => join(entry.parentPath, entry.name));
}

function cssFiles(dir: string): string[] {
  return readdirSync(dir, { recursive: true, withFileTypes: true })
    .filter(
      (entry) => entry.isFile() && entry.name.endsWith(".css") && entry.name !== "tailwind.css",
    )
    .map((entry) => join(entry.parentPath, entry.name));
}

function selectorOf(text: string, index: number): string {
  const brace = text.lastIndexOf("{", index);
  const before = text.slice(0, brace);
  const ruleEnd = before.lastIndexOf("}");
  const commentEnd = before.lastIndexOf("*/");
  return before.slice(Math.max(ruleEnd + 1, commentEnd + 2)).trim();
}

function allowedSpend(file: string, selector: string, spend: string): boolean {
  return selector.split(",").some((part) => {
    const bare = part.trim().replace(/:(?:hover|focus-visible)(?::not\([^)]*\))?$/, "");
    const entry = ALLOWED.get(`${basename(file)}:${bare}`);
    return entry !== undefined && (entry.only === undefined || entry.only === spend);
  });
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

  it("no -danger utility survives outside the tone tokens", () => {
    const violations: string[] = [];
    // tailwind.css clears --color-* and readmits only named tokens, so bg-danger,
    // text-danger, stroke-danger, and border-danger compile to nothing. A dead
    // utility on an SVG is invisible, not merely the wrong colour.
    for (const file of sourceFiles(shellSource)) {
      for (const [index, line] of readFileSync(file, "utf8").split("\n").entries()) {
        for (const hit of line.matchAll(DEAD_DANGER)) violations.push(`${file}:${index + 1}: ${hit[0]}`);
      }
    }
    expect(violations, violations.join("\n")).toEqual([]);
  });

  it("stylesheets spend Signal Cyan only on links", () => {
    const violations: string[] = [];
    // tailwind.css declares the --color-accent aliases; the TSX audit covers what they enable.
    // app.css is read: its "here" fill is quiet-form-only, while round 9 moved the
    // label, glyph, and bar off the accent.
    for (const file of cssFiles(shellSource)) {
      const text = readFileSync(file, "utf8");
      for (const hit of text.matchAll(CSS_ACCENT)) {
        const selector = selectorOf(text, hit.index ?? 0);
        if (!allowedSpend(file, selector, hit[0])) violations.push(`${basename(file)}:${selector}`);
      }
    }
    expect(violations, violations.join("\n")).toEqual([]);
  });
});
