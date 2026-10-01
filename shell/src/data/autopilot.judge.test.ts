import { describe, expect, it } from "vitest";
import { formatProbability, readJudgeBand, type JudgeVerdict } from "./autopilot";

function verdict(overrides: Partial<JudgeVerdict> = {}): JudgeVerdict {
  return {
    id: 5,
    run_id: 44,
    tool_name: "Bash",
    tool_input: JSON.stringify({ command: "cargo test --workspace | tee t.log" }),
    action_class: "unrecognized",
    classifier_decision: "pending_approval",
    judge: "observe",
    model: "jev-latest",
    p_in_scope: 0.97,
    p_safe: 0.94,
    p: 0.94,
    band: "allow",
    capped: false,
    final_decision: "pending_approval",
    enforced: false,
    created_at: "2026-09-27T10:00:00Z",
    ...overrides,
  };
}

describe("the judge's words", () => {
  it("says which way the judge leaned, and when a guard held it back", () => {
    expect(readJudgeBand(verdict())).toBe("would allow");
    expect(readJudgeBand(verdict({ capped: true }))).toBe("would allow — held back by a guard");
    expect(readJudgeBand(verdict({ band: "deny" }))).toBe("would refuse");
  });

  it("prints a probability with two places, and a dash for none", () => {
    expect(formatProbability(0.9412)).toBe("0.94");
    expect(formatProbability(null)).toBe("—");
  });
});
