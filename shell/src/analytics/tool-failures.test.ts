import { describe, expect, it } from "vitest";
import {
  classShares, ERROR_CLASS_LABELS, errorClassLabel, hasToolFailures, programText, toolText, topTools,
} from "./tool-failures";
import type { ToolFailureRow } from "../data/devtime";
import { devtimeToolFailures } from "../test/harness";

const TOKENS = [
  "exit_nonzero", "exit_75", "timeout", "permission_denied", "hook_block", "edit_not_found",
  "wrong_shell", "tool_error", "background_failed", "background_killed",
];

function row(tool_name: string, cmd_program: string | null = null): ToolFailureRow {
  return { tool_name, cmd_program, attempts: 2, failures: 1, failure_rate: 0.5, top_error_class: "tool_error" };
}

describe("error class labels", () => {
  it("names every class token of the contract in English", () => {
    for (const token of TOKENS) {
      expect(errorClassLabel(token)).toBe(ERROR_CLASS_LABELS[token]);
      expect(errorClassLabel(token)).not.toBe(token);
    }
    expect(errorClassLabel("exit_nonzero")).toBe("Non-zero exit");
    expect(errorClassLabel("background_killed")).toBe("Background task killed");
    expect(Object.keys(ERROR_CLASS_LABELS).sort()).toEqual([...TOKENS].sort());
  });

  it("returns a token it does not know as is", () => {
    expect(errorClassLabel("brand_new_class")).toBe("brand_new_class");
  });
});

describe("classShares", () => {
  it("turns each class into a share item keyed by its token", () => {
    expect(classShares(devtimeToolFailures().by_error_class)).toEqual([
      { key: "exit_nonzero", label: "Non-zero exit", value: 3 },
      { key: "edit_not_found", label: "Edit target not found", value: 1 },
    ]);
  });
});

describe("topTools", () => {
  it("keeps at most eight rows in the order given", () => {
    const rows = Array.from({ length: 10 }, (_, index) => row(`T${index}`));
    const top = topTools(rows);
    expect(top).toHaveLength(8);
    expect(top.map((line) => line.tool_name)).toEqual(rows.slice(0, 8).map((line) => line.tool_name));
  });

  it("labels the daemon's `__other__` row Other and gives each row a distinct key", () => {
    const top = topTools([row("Bash", "cargo test"), row("Bash", "git"), row("__other__")]);
    expect(top.map((line) => line.label)).toEqual(["Bash", "Bash", "Other"]);
    expect(new Set(top.map((line) => line.key)).size).toBe(3);
    expect(toolText("__other__")).toBe("Other");
    expect(toolText("Edit")).toBe("Edit");
  });
});

describe("programText", () => {
  it("writes a dash for no program and the program otherwise", () => {
    expect(programText(null)).toBe("—");
    expect(programText("cargo test")).toBe("cargo test");
  });
});

describe("hasToolFailures", () => {
  it("is false for a missing block and an all-zero one", () => {
    expect(hasToolFailures(undefined)).toBe(false);
    expect(hasToolFailures(devtimeToolFailures({
      attempts: 0, failures: 0, failure_rate: null, interrupted: 0, unresolved: 0, by_error_class: [], by_tool: [],
    }))).toBe(false);
  });

  it("is true as soon as attempts, interruptions or unresolved calls exist", () => {
    const zero = { attempts: 0, failures: 0, failure_rate: null, interrupted: 0, unresolved: 0, by_error_class: [], by_tool: [] };
    expect(hasToolFailures(devtimeToolFailures({ ...zero, attempts: 1 }))).toBe(true);
    expect(hasToolFailures(devtimeToolFailures({ ...zero, interrupted: 1 }))).toBe(true);
    expect(hasToolFailures(devtimeToolFailures({ ...zero, unresolved: 1 }))).toBe(true);
    expect(hasToolFailures(devtimeToolFailures())).toBe(true);
  });
});
