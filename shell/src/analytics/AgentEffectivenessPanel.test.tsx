import { describe, expect, it } from "vitest";
import { render, screen, within } from "@testing-library/react";

import { devtimeAgents } from "../test/harness";
import { AgentEffectivenessPanel } from "./AgentEffectivenessPanel";

const cells = (row: HTMLElement) => Array.from(row.querySelectorAll("th, td")).map((cell) => cell.textContent);

describe("AgentEffectivenessPanel", () => {
  it("leads with the runs, the failed ones and the interrupted ones", () => {
    render(<AgentEffectivenessPanel agents={devtimeAgents()} />);
    const panel = screen.getByRole("region", { name: "Agent effectiveness" });
    expect(within(panel).getByText("Agent effectiveness")).toBeTruthy();
    expect(within(panel).getByText(/42 agent runs · 4 failed \(10%\)/)).toBeTruthy();
    expect(within(panel).getByText(/2 interrupted/)).toBeTruthy();
  });

  it("tables the runs by type, with the role, checks passed and the median run", () => {
    render(<AgentEffectivenessPanel agents={devtimeAgents()} />);
    const table = screen.getByRole("table", { name: "Agents by type" });
    const headers = within(table).getAllByRole("columnheader").map((cell) => cell.textContent);
    expect(headers).toEqual(["Type", "Role", "Runs", "Failed", "Checks passed", "Median time"]);
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(2);
    expect(cells(rows[0])).toEqual(["wf-executor", "Implementer", "20", "2", "71% (10 of 14)", "4 min"]);
    expect(cells(rows[1])).toEqual(["Explore", "—", "22", "2", "—", "2 min"]);
  });

  it("says what is checked and how many runs are not ruled yet", () => {
    render(<AgentEffectivenessPanel agents={devtimeAgents()} />);
    expect(screen.getByText(
      "Implementer runs are checked by the next test or build in the main session; others show as not measured. · 8 runs in sessions not ruled yet",
    )).toBeTruthy();
  });

  it("labels the folded row Other and an unrecorded type Unknown type", () => {
    const base = devtimeAgents().by_type[1];
    const agents = devtimeAgents({
      by_type: [
        ...devtimeAgents().by_type,
        { ...base, agent_type: "unknown", attempts: 3 },
        { ...base, agent_type: "__other__", attempts: 5 },
      ],
    });
    render(<AgentEffectivenessPanel agents={agents} />);
    const rows = within(screen.getByRole("table", { name: "Agents by type" })).getAllByRole("row").slice(1);
    expect(cells(rows[2])[0]).toBe("Unknown type");
    expect(cells(rows[3])[0]).toBe("Other");
  });

  it("draws at most eight types of the daemon's 21 rows", () => {
    const base = devtimeAgents().by_type[1];
    const by_type = Array.from({ length: 21 }, (_, index) => ({ ...base, agent_type: index === 20 ? "__other__" : `type-${index}` }));
    render(<AgentEffectivenessPanel agents={devtimeAgents({ by_type })} />);
    const rows = within(screen.getByRole("table", { name: "Agents by type" })).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(8);
  });

  it("never draws a session id, an agent id, a worktree or a prompt", () => {
    const { container } = render(<AgentEffectivenessPanel agents={devtimeAgents()} />);
    const text = container.textContent ?? "";
    // "session" itself appears in the checked note ("the main session"); no identifier or path does.
    expect(text).not.toMatch(/session_id|agent_id|worktree|prompt/i);
    expect(text).not.toMatch(/[0-9a-f]{8}-[0-9a-f]{4}-/i);
    expect(text).not.toMatch(/[\\/]/);
  });
});
