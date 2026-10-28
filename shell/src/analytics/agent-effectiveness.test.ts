import { describe, expect, it } from "vitest";

import type { AgentTypeRow } from "../data/devtime";
import { devtimeAgents } from "../test/harness";
import {
  OTHER_AGENT, TOP_AGENT_TYPES, UNKNOWN_AGENT, agentTypeText, checkedNote, hasAgents, medianText, passText, roleText, topAgentTypes,
} from "./agent-effectiveness";

const NOTE = "Implementer runs are checked by the next test or build in the main session; others show as not measured.";

function row(agent_type: string): AgentTypeRow {
  return {
    agent_type, role: null, attempts: 1, ms: 1000, median_ms: 1000,
    outcome: { ok: 1, failed: 0, interrupted: 0, other: 0 },
    verified: { passed: 0, failed: 0, not_measured: 1, unruled: 0 },
    failure_rate: 0, pass_rate: null,
  };
}

describe("agent effectiveness adapters", () => {
  it("names the folded and the unknown types, and keeps any other as is", () => {
    expect(agentTypeText(OTHER_AGENT)).toBe("Other");
    expect(agentTypeText(UNKNOWN_AGENT)).toBe("Unknown type");
    expect(agentTypeText("wf-executor")).toBe("wf-executor");
  });

  it("labels the roles, a dash for none and an unknown token as is", () => {
    expect(roleText("implementer")).toBe("Implementer");
    expect(roleText("reviewer")).toBe("Reviewer");
    expect(roleText(null)).toBe("—");
    expect(roleText("auditor")).toBe("auditor");
  });

  it("shows checks passed out of the checked runs, a dash when none was checked", () => {
    const [executor, explore] = devtimeAgents().by_type;
    expect(passText(executor)).toBe("71% (10 of 14)");
    expect(passText(explore)).toBe("—");
    expect(passText(devtimeAgents())).toBe("71% (10 of 14)");
  });

  it("shows a median run in seconds or minutes, a dash when unknown", () => {
    const [executor, explore] = devtimeAgents().by_type;
    expect(medianText(executor)).toBe("4 min");
    expect(medianText(explore)).toBe("2 min");
    expect(medianText({ ...explore, median_ms: 45_000 })).toBe("45 s");
    expect(medianText({ ...explore, median_ms: null })).toBe("—");
  });

  it("explains what is checked, and counts the unruled runs only when there are some", () => {
    expect(checkedNote(devtimeAgents())).toBe(`${NOTE} · 8 runs in sessions not ruled yet`);
    const ruled = devtimeAgents({ verified: { passed: 10, failed: 4, not_measured: 28, unruled: 0 } });
    expect(checkedNote(ruled)).toBe(NOTE);
    const one = devtimeAgents({ verified: { passed: 10, failed: 4, not_measured: 27, unruled: 1 } });
    expect(checkedNote(one)).toBe(`${NOTE} · 1 run in sessions not ruled yet`);
  });

  it("keeps the daemon's order and caps the types", () => {
    const rows = Array.from({ length: 21 }, (_, index) => row(index === 20 ? OTHER_AGENT : `t${index}`));
    const top = topAgentTypes(rows);
    expect(top).toHaveLength(TOP_AGENT_TYPES);
    expect(top.map((line) => line.agent_type)).toEqual(rows.slice(0, 8).map((line) => line.agent_type));
    expect(topAgentTypes(rows, 3).map((line) => line.agent_type)).toEqual(["t0", "t1", "t2"]);
    expect(topAgentTypes(rows.slice(0, 2))).toHaveLength(2);
  });

  it("has nothing to show for a missing block or one with no runs", () => {
    expect(hasAgents(undefined)).toBe(false);
    expect(hasAgents(devtimeAgents({ attempts: 0, by_type: [] }))).toBe(false);
    expect(hasAgents(devtimeAgents())).toBe(true);
  });
});
