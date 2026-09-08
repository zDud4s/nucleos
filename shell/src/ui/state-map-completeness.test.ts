import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { statesOf } from "./state-map";

const moduleUrl = import.meta.url.startsWith("file:") ? import.meta.url : `file://${import.meta.url}`;
const repoRoot = fileURLToPath(new URL("../../../", moduleUrl));
const jobRs = readFileSync(`${repoRoot}core/src/job.rs`, "utf8");
const teamRs = readFileSync(`${repoRoot}core/src/team.rs`, "utf8");

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

  it("a team item has four states and planned is not one of them", () => {
    const itemStates = ["done", "failed", "pending", "running"];
    expect(statesOf("team_item").sort()).toEqual(itemStates);
    for (const state of itemStates) {
      expect(teamRs.includes(`'${state}'`)).toBe(true);
    }
    expect(teamRs.includes("'planned'")).toBe(false);
  });
});
