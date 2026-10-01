import { readdirSync, readFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * This catches quoted tone literals in Badge opening tags, including conditional expressions;
 * table lookups are deliberately invisible, hence `tables` below. The allowlist is a ratchet:
 * unlisted files fail, more literals fail, and fewer should be written down.
 */
const moduleUrl = import.meta.url;
const repoRoot = fileURLToPath(new URL("../../../", moduleUrl));
const shellSource = join(repoRoot, "shell", "src");
const TONES = "active|shadow|off|pending|paused|danger|info";
const LITERAL = new RegExp(`tone=(?:"(?:${TONES})"|\\{[^}]*"(?:${TONES})"[^}]*\\})`);
const ALLOWED = new Map<string, { literals: number; tables?: string[]; reason: string }>([
  ["calendar/DaySheet.tsx", { literals: 5, reason: "facts about a day — today, not a working day, a short or long one, a proposed occurrence. No núcleo state machine writes any of them." }],
  ["pages/Autopilot.tsx", { literals: 1, reason: "a shadow decision's action class is an identifier, not a domain state." }],
  ["pages/AutopilotJudge.tsx", { literals: 2, reason: "a verdict's action class is an identifier, and the judge's band is its opinion about one action (would allow, would refuse), not a lifecycle state of anything." }],
  ["pages/Browser.tsx", { literals: 3, reason: "a grant's kind and whether it submits forms are permissions the shell decides, and a write's HTTP method is an identifier." }],
  ["pages/Calendar.tsx", { literals: 1, reason: "busy or free right now is computed from the calendar in the browser; the daemon sends no such literal." }],
  ["pages/Contacts.tsx", { literals: 4, reason: "a contact's verdict, whether you write back, and a merge are facts about a person, not a lifecycle." }],
  ["pages/Feed.tsx", { literals: 1, tables: ["readFeedKind"], reason: "the ignorance device for an unknown feed kind — the same posture as StateBadge, in the slice that owns feed kinds." }],
  ["pages/System.tsx", { literals: 1, reason: "a token's authority level is an identifier off the wire, not a lifecycle state." }],
  ["pages/Voice.tsx", { literals: 8, reason: "the dictation phase and the spoken conversation's phase both live entirely in the webview (`lib/conversation.ts` for the second) — no núcleo literal exists for either." }],
  ["team/Decisions.tsx", { literals: 1, reason: "`granted — nobody decides` is a sentence about an absent proposal, not a state of one." }],
]);

function callSites(source: string): { line: number; attrs: string }[] {
  const sites: { line: number; attrs: string }[] = [];
  const NAME = "<Badge";
  let at = source.indexOf(NAME);
  while (at !== -1) {
    let i = at + NAME.length; let depth = 0; let quote: string | null = null;
    for (; i < source.length; i += 1) {
      const c = source[i];
      if (quote !== null) { if (c === quote) quote = null; continue; }
      if (c === '"' || c === "'" || c === "`") quote = c;
      else if (c === "{") depth += 1; else if (c === "}") depth -= 1;
      else if (c === ">" && depth === 0) break;
    }
    sites.push({ line: source.slice(0, at).split("\n").length, attrs: source.slice(at + NAME.length, i) });
    at = source.indexOf(NAME, i);
  }
  return sites;
}
function files(): string[] {
  return readdirSync(shellSource, { recursive: true })
    .filter((entry): entry is string => typeof entry === "string" && (entry.endsWith(".ts") || entry.endsWith(".tsx")) && !entry.endsWith(".test.ts") && !entry.endsWith(".test.tsx"))
    .map((entry) => join(shellSource, entry))
    .filter((file) => file !== join(shellSource, "ui", "StateBadge.tsx"));
}

describe("Badge tone authorship", () => {
  const OBJECT_ROW = new RegExp(`\\btone:\\s*"(?:${TONES})"`);
  const MAP = join(shellSource, "ui", "state-map.ts");
  it("a Badge outside the map's own primitive may not name a tone", () => {
    for (const file of files()) {
      const source = readFileSync(file, "utf8"); const sites = callSites(source).filter((site) => LITERAL.test(site.attrs));
      if (sites.length === 0) continue;
      const key = relative(shellSource, file).split(sep).join("/"); const entry = ALLOWED.get(key);
      expect(entry, `${key}:${sites[0].line}`).toBeDefined();
      expect(sites.length, key).toBeLessThanOrEqual(entry!.literals);
    }
  });
  it("the allowlist is a ratchet: every file is named, counted and reasoned", () => {
    for (const [key, entry] of ALLOWED) {
      const source = readFileSync(join(shellSource, key), "utf8");
      expect(source, key).toContain("<Badge"); expect(entry.reason, key).not.toBe("");
      for (const table of entry.tables ?? []) expect(source, key).toContain(table);
    }
  });
  it("no page spends Acting Green on a word through text-tone-active-fg", () => {
    for (const file of files()) expect(readFileSync(file, "utf8"), file).not.toContain("text-tone-active-fg");
    for (const file of ["canvas/WorkflowCanvas.tsx", "project/WorkflowGraph.tsx", "project/ModeState.tsx"]) expect(readFileSync(join(shellSource, file), "utf8"), file).toContain("bg-tone-active-fg");
  });
  it("a tone literal in a .ts object row is caught wherever it hides", () => {
    for (const file of files()) {
      if (file !== MAP) expect(OBJECT_ROW.test(readFileSync(file, "utf8")), relative(shellSource, file)).toBe(false);
    }
    expect(OBJECT_ROW.test(`const T = { job_started: { tone: "active", label: "x" } };`)).toBe(true);
    expect(OBJECT_ROW.test(`tone?: "danger";`)).toBe(false);
  });
  it("BadgeTone is named only inside ui/, so a tone table cannot be built elsewhere", () => {
    for (const file of files()) {
      const key = relative(shellSource, file).split(sep).join("/");
      if (!key.startsWith("ui/")) expect(readFileSync(file, "utf8").includes("BadgeTone"), key).toBe(false);
    }
  });
  it("one author for a feed kind: the embed and the drawer both reach Feed.tsx's KindBadge", () => {
    for (const key of ["app/FeedEmbed.tsx", "app/NotificationsDrawer.tsx"]) {
      const source = readFileSync(join(shellSource, key), "utf8");
      expect(source, key).toContain("KindBadge"); expect(source, key).not.toContain("<Badge");
    }
  });
});
