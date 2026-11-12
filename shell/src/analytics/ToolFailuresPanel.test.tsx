import { describe, expect, it } from "vitest";
import { render, screen, within } from "@testing-library/react";

import { devtimeToolFailures } from "../test/harness";
import { ToolFailuresPanel } from "./ToolFailuresPanel";

describe("ToolFailuresPanel", () => {
  it("leads with failed calls out of all calls, plus the interrupted and unresolved ones", () => {
    render(<ToolFailuresPanel failures={devtimeToolFailures()} />);
    const panel = screen.getByRole("region", { name: "Tool failures" });
    expect(within(panel).getByText(/4 of 40 tool calls failed \(10%\)/)).toBeTruthy();
    expect(within(panel).getByText(/1 interrupted/)).toBeTruthy();
    expect(within(panel).getByText(/1 without a result/)).toBeTruthy();
  });

  it("lists the failures by error class, by label", () => {
    render(<ToolFailuresPanel failures={devtimeToolFailures()} />);
    expect(screen.getByRole("img", { name: "Failures by error class" })).toBeTruthy();
    const keys = screen.getByRole("list", { name: "Failures by error class, by item" });
    expect(within(keys).getByText("Non-zero exit")).toBeTruthy();
    expect(within(keys).getByText("Edit target not found")).toBeTruthy();
  });

  it("tables the failures by tool, with a dash for no program or no rate and Other for the folded row", () => {
    const failures = devtimeToolFailures({
      by_tool: [
        { tool_name: "Bash", cmd_program: "cargo test", attempts: 20, failures: 3, failure_rate: 0.15, top_error_class: "exit_nonzero" },
        { tool_name: "Edit", cmd_program: null, attempts: 20, failures: 1, failure_rate: 0.05, top_error_class: "edit_not_found" },
        { tool_name: "__other__", cmd_program: null, attempts: 0, failures: 0, failure_rate: null, top_error_class: null },
      ],
    });
    render(<ToolFailuresPanel failures={failures} />);
    const table = screen.getByRole("table", { name: "Failures by tool" });
    const headers = within(table).getAllByRole("columnheader").map((cell) => cell.textContent);
    expect(headers).toEqual(["Tool", "Program", "Calls", "Failed", "Rate", "Most common"]);
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(3);
    const cells = (row: HTMLElement) => Array.from(row.querySelectorAll("th, td")).map((cell) => cell.textContent);
    expect(cells(rows[0])).toEqual(["Bash", "cargo test", "20", "3", "15%", "Non-zero exit"]);
    expect(cells(rows[1])).toEqual(["Edit", "—", "20", "1", "5%", "Edit target not found"]);
    expect(cells(rows[2])).toEqual(["Other", "—", "0", "0", "—", "—"]);
  });

  it("says so, and draws no class bar, when nothing failed", () => {
    const failures = devtimeToolFailures({
      failures: 0, failure_rate: 0, by_error_class: [],
      by_tool: [{ tool_name: "Read", cmd_program: null, attempts: 40, failures: 0, failure_rate: 0, top_error_class: null }],
    });
    render(<ToolFailuresPanel failures={failures} />);
    expect(screen.getByText("No failed tool calls in this window")).toBeTruthy();
    expect(screen.queryByRole("img", { name: "Failures by error class" })).toBeNull();
  });
});
