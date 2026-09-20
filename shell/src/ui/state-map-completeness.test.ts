import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { RUN_STATUSES, runIsAlive } from "../data/runs";
import { readState, statesOf } from "./state-map";

const moduleUrl = import.meta.url.startsWith("file:") ? import.meta.url : `file://${import.meta.url}`;
const repoRoot = fileURLToPath(new URL("../../../", moduleUrl));
const jobRs = readFileSync(`${repoRoot}core/src/job.rs`, "utf8");
const teamRs = readFileSync(`${repoRoot}core/src/team.rs`, "utf8");
const concurrencyRs = readFileSync(`${repoRoot}core/src/concurrency.rs`, "utf8");
const runsRs = readFileSync(`${repoRoot}core/src/runs.rs`, "utf8");
const quotaRs = readFileSync(`${repoRoot}core/src/quota.rs`, "utf8");

function coreFiles(): { name: string; source: string }[] {
  const dir = `${repoRoot}core/src`;
  return readdirSync(dir, { withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(".rs"))
    .map((entry) => ({ name: entry.name, source: readFileSync(`${dir}/${entry.name}`, "utf8") }));
}

function cutAtTestModule(source: string): number {
  const at = source.search(/^#\[cfg\(test\)\]\r?\nmod /m);
  return at === -1 ? source.length : at;
}

function argumentAt(source: string, open: number, position: number): string | null {
  let depth = 0;
  let start = open + 1;
  let index = open;
  let found = 1;
  while (index < source.length) {
    const char = source[index];
    if (char === '"') {
      index += 1;
      while (index < source.length && source[index] !== '"') index += source[index] === "\\" ? 2 : 1;
    } else if (char === "/" && source[index + 1] === "/") {
      while (index < source.length && source[index] !== "\n") index += 1;
    } else if (char === "(" || char === "[" || char === "{") {
      depth += 1;
    } else if (char === ")" || char === "]" || char === "}") {
      if (char === ")" && depth === 1) return found === position ? source.slice(start, index).trim() : null;
      depth -= 1;
    } else if (char === "," && depth === 1) {
      if (found === position) return source.slice(start, index).trim();
      found += 1;
      start = index + 1;
    }
    index += 1;
  }
  return null;
}

const WRITERS: { name: string; position: number }[] = [
  { name: "append", position: 3 },
  { name: "append_on", position: 3 },
  { name: "append_for_errand", position: 3 },
  { name: "say", position: 3 },
  // `job.rs::say_once` forwards to `say` only when the same line is not already written.
  { name: "say_once", position: 3 },
  { name: "deliver_or_defer", position: 2 },
];

const FORWARDED: Record<string, Record<string, string[]>> = {
  "feed.rs": { kind: [] },
  "job.rs": { kind: [] },
  "notify.rs": { kind: [] },
  "runs.rs": { kind: ["shadow_run_completed", "worktree_run_completed"] },
  "triage.rs": { 'format!("email_{}",verdict.class)': [] },
};

const EMAIL_CLASS_KINDS = ["email_urgent"];

function writtenFeedKinds(): { kinds: Set<string>; files: number; unresolved: string[] } {
  const kinds = new Set<string>();
  const unresolved: string[] = [];
  const files = coreFiles();
  for (const { name, source } of files) {
    const body = source.slice(0, cutAtTestModule(source));
    const constants = new Map(
      [...body.matchAll(/const (\w+): &str = "([^"]+)";/g)].map(([, key, value]) => [key, value]),
    );
    for (const writer of WRITERS) {
      const call = new RegExp(`(?<![A-Za-z0-9_])${writer.name}\\s*\\(`, "g");
      for (const match of body.matchAll(call)) {
        const open = match.index! + match[0].length - 1;
        if (/fn\s+$/.test(body.slice(Math.max(0, match.index! - 8), match.index!))) continue;
        const argument = argumentAt(body, open, writer.position);
        if (argument === null) continue;
        if (argument.startsWith('"')) {
          kinds.add(argument.slice(1, -1));
          continue;
        }
        if (/^[A-Z][A-Z0-9_]*$/.test(argument)) {
          const value = constants.get(argument);
          if (value === undefined) unresolved.push(`${name}: ${argument}`);
          else kinds.add(value);
          continue;
        }
        const key = argument.replace(/\s+/g, "").replace(/^&/, "");
        const declared = FORWARDED[name]?.[key];
        if (declared === undefined) unresolved.push(`${name}: ${key}`);
        else for (const kind of declared) kinds.add(kind);
      }
    }
  }
  return { kinds, files: files.length, unresolved };
}

function statusConstants(source: string): Map<string, string> {
  return new Map(
    [...source.matchAll(/pub const (STATUS_\w+): &str = "([^"]+)";/g)].map(([, name, value]) => [name, value]),
  );
}

function literals(source: string, declaration: RegExp, constants = new Map<string, string>()): string[] {
  const match = declaration.exec(source);
  expect(match, `could not parse ${declaration}`).not.toBeNull();
  const values = match![1]
    .split(",")
    .map((value) => value.trim())
    .filter(Boolean)
    .map((value) => (value.startsWith('"') ? value.slice(1, -1) : constants.get(value)))
    .filter((value): value is string => value !== undefined);
  expect(values).not.toHaveLength(0);
  return values;
}

describe("state-map completeness", () => {
  it("every job and team-run literal in the Rust source has a reading", () => {
    const constants = statusConstants(jobRs);
    const jobLive = literals(jobRs, /pub const LIVE_STATUSES: \[&str; \d+\] = \[([^\]]*)\];/);
    const jobTerminal = literals(jobRs, /pub const TERMINAL_STATUSES: \[&str; \d+\] = \[([^\]]*)\];/, constants);
    const teamLive = literals(teamRs, /pub const LIVE_STATES: &\[&str\] = &\[([^\]]*)\];/);
    const teamTerminal = literals(teamRs, /pub const TERMINAL_STATES: &\[&str\] = &\[([^\]]*)\];/);

    expect(new Set(statesOf("job"))).toEqual(new Set([...jobLive, ...jobTerminal]));
    expect(new Set(statesOf("team_run"))).toEqual(new Set([...teamLive, ...teamTerminal]));
  });

  /**
   * `job_items.status` has no Rust list of its own — `LIVE_ITEM_STATUSES` is test-only and
   * covers only the unfinished half — so the vocabulary is read where the núcleo reads it back:
   * the arms of `item_state_from`. `pending` is the one stored value that function reaches
   * through `_ =>`, because it is the column's default, and it is asserted separately so the
   * scan cannot quietly lose it.
   */
  it("every job item status the núcleo reads back has a reading, and conflicted is not a summons", () => {
    const start = jobRs.indexOf("fn item_state_from(");
    expect(start, "item_state_from is gone from job.rs").toBeGreaterThan(-1);
    // The function's own closing brace, at column 0 — `\r?` because the checkout may be CRLF,
    // and a search for "\n}\n" that never matched ran on into the next function's arms.
    const rest = jobRs.slice(start);
    const end = rest.search(/\r?\n\}\r?\n/);
    expect(end, "could not find where item_state_from ends").toBeGreaterThan(0);
    const body = rest.slice(0, end);
    const constants = statusConstants(jobRs);
    const arms = new Set<string>();
    for (const [, literal] of body.matchAll(/"([a-z_]+)"(?:\s+if [^=]+)?\s*=>/g)) arms.add(literal);
    for (const [, name] of body.matchAll(/\b(STATUS_\w+)\s*=>/g)) {
      const value = constants.get(name);
      expect(value, `${name} is not a pub const in job.rs`).toBeDefined();
      arms.add(value!);
    }
    expect(arms.size).toBeGreaterThanOrEqual(12);
    expect(jobRs).toMatch(/_ => ItemState::Pending/);

    expect(new Set(statesOf("job_item"))).toEqual(new Set([...arms, "pending"]));
    // Put down rather than failed (`core/src/job.rs`, `ItemState::Conflicted`), so held and not
    // wrong; and not a summons either, because the queue starts the resolution run itself.
    expect(readState("job_item", "conflicted")?.tone).toBe("paused");
    expect(readState("job_item", "conflicted")?.tone).toBe(readState("feed", "job_item_conflicted")?.tone);
  });

  /**
   * The `quota` domain, which nothing else in this file covers.
   *
   * Every other block here fixes one domain and one shape in the Rust — an array by name, the arms
   * of one function, four fixed strings, the feed's emitters. None of them has a generic mechanism
   * over `StateDomain`, so a domain added to `state-map.ts` is checked by nothing unless somebody
   * writes the block for it. That was true of this one until this test existed, and the map would
   * have drifted from `QUOTA_STATES` without a single gate turning red.
   *
   * What a drift costs is worth naming: `readState` returns null for a state it has no row for,
   * `StateBadge` then renders the bare literal, and a ring falls back to a neutral tone. A quota
   * that is spent would be painted the same as one that is fine.
   */
  it("every quota state the núcleo can produce has a reading", () => {
    const states = literals(quotaRs, /pub const QUOTA_STATES: \[&str; \d+\] = \[([^\]]*)\];/);

    expect(states).toHaveLength(5);
    expect(new Set(statesOf("quota"))).toEqual(new Set(states));
    // The two that must never be stated with the confidence of a measurement: one is a number
    // about a window that has ended, the other is no number at all. A brake is later allowed to
    // act on this domain, so a quiet tone here is a safety property and not a taste.
    expect(readState("quota", "stale")?.tone).toBe("off");
    expect(readState("quota", "unmeasured")?.tone).toBe("off");
    // And the one that asks the owner for a decision wears the colour that means exactly that.
    expect(readState("quota", "warn")?.tone).toBe("pending");
  });

  it("a team item has four states and planned is not one of them", () => {
    const itemStates = ["done", "failed", "pending", "running"];
    expect(statesOf("team_item").sort()).toEqual(itemStates);
    for (const state of itemStates) {
      expect(teamRs.includes(`'${state}'`)).toBe(true);
    }
    expect(teamRs.includes("'planned'")).toBe(false);
  });

  it("every run literal in the Rust source has a reading, and the shell's two lists agree", () => {
    const live = literals(concurrencyRs, /pub const LIVE_RUN_STATUSES: \[&str; \d+\] = \[([^\]]*)\];/);
    const terminal = literals(runsRs, /pub const TERMINAL_RUN_STATUSES: &\[&str\] = &\[([^\]]*)\];/);
    const union = [...live, ...terminal];

    expect(new Set(statesOf("run"))).toEqual(new Set(union));
    expect(new Set(RUN_STATUSES)).toEqual(new Set(union));
  });

  it("only a running run changes on its own", () => {
    const live = literals(concurrencyRs, /pub const LIVE_RUN_STATUSES: \[&str; \d+\] = \[([^\]]*)\];/);
    const terminal = literals(runsRs, /pub const TERMINAL_RUN_STATUSES: &\[&str\] = &\[([^\]]*)\];/);

    expect([...live, ...terminal].filter(runIsAlive)).toEqual(["running"]);
  });

  it("the feed domain's map is the núcleo's own enumeration", () => {
    const { kinds, files } = writtenFeedKinds();
    expect(files).toBeGreaterThanOrEqual(100);
    expect(kinds.size).toBeGreaterThanOrEqual(60);
    const mapped = new Set(statesOf("feed"));
    expect([...kinds].filter((kind) => !mapped.has(kind)).sort()).toEqual([]);
    expect([...mapped].filter((kind) => !kinds.has(kind) && !EMAIL_CLASS_KINDS.includes(kind)).sort()).toEqual([]);
  });

  it("a feed kind the scan cannot read is named, not dropped", () => {
    expect(writtenFeedKinds().unresolved).toEqual([]);
  });

  it("the wrappers and the test-module cut are proven, not assumed", () => {
    const { kinds } = writtenFeedKinds();
    expect(kinds.has("job_waiting")).toBe(true);
    expect(kinds.has("job_finished")).toBe(true);
    expect(kinds.has("job_review_skipped")).toBe(true);
    expect(kinds.has("token_efficiency")).toBe(true);
    expect(kinds.has("land_resolution_failed")).toBe(true);
    expect(kinds.has("shadow_run_completed")).toBe(true);
    expect(kinds.has("worktree_run_completed")).toBe(true);
    expect(kinds.has("global")).toBe(false);
    expect(kinds.has("project")).toBe(false);
    expect(kinds.has("email_action")).toBe(false);
  });
});
