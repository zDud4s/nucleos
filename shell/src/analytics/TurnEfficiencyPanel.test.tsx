import { describe, expect, it } from "vitest";
import { render, screen, within } from "@testing-library/react";

import { devtimeTurnClasses, devtimeUnexplained } from "../test/harness";
import { TurnEfficiencyPanel } from "./TurnEfficiencyPanel";

const cells = (row: HTMLElement) => Array.from(row.querySelectorAll("th, td")).map((cell) => cell.textContent);

describe("TurnEfficiencyPanel", () => {
  it("tables the turns by size, labelled by call range, with a dash for no per-call time", () => {
    render(<TurnEfficiencyPanel classes={devtimeTurnClasses()} unexplained={devtimeUnexplained()} showProject />);
    const panel = screen.getByRole("region", { name: "Turn efficiency" });
    const table = within(panel).getByRole("table", { name: "Turns by size" });
    const headers = within(table).getAllByRole("columnheader").map((cell) => cell.textContent);
    expect(headers).toEqual(["Size", "Turns", "Median active", "Per call", "Explained", "Slow"]);
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(3);
    expect(cells(rows[0])).toEqual(["No tool calls", "12", "1 min", "—", "0%", "0"]);
    expect(cells(rows[1])).toEqual(["1–5 calls", "30", "3 min", "56 s", "11%", "2"]);
    expect(cells(rows[2])).toEqual(["21+ calls", "15", "9 min", "19 s", "0%", "1"]);
  });

  it("lists the slow turns no rule explains, with the criteria, and never a session", () => {
    const { container } = render(
      <TurnEfficiencyPanel classes={devtimeTurnClasses()} unexplained={devtimeUnexplained()} showProject />,
    );
    const table = screen.getByRole("table", { name: "Slow turns no rule explains" });
    const headers = within(table).getAllByRole("columnheader").map((cell) => cell.textContent);
    expect(headers).toEqual(["Day", "Project", "Size", "Calls", "Active", "Class median", "Explained"]);
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(3);
    expect(cells(rows[0])).toEqual(["2026-10-16", "nucleos", "21+ calls", "40", "30 min", "9 min", "0%"]);
    expect(cells(rows[1])).toEqual(["2026-10-14", "nucleos", "1–5 calls", "4", "8 min", "3 min", "13%"]);
    expect(screen.getByText(/medians since 2026-09-17/)).toBeTruthy();
    expect(container.textContent ?? "").not.toMatch(/session/i);
    expect(screen.queryByText(/Showing/)).toBeNull();
  });

  it("drops the Project column outside the global scope", () => {
    render(<TurnEfficiencyPanel classes={devtimeTurnClasses()} unexplained={devtimeUnexplained()} showProject={false} />);
    const table = screen.getByRole("table", { name: "Slow turns no rule explains" });
    const headers = within(table).getAllByRole("columnheader").map((cell) => cell.textContent);
    expect(headers).toEqual(["Day", "Size", "Calls", "Active", "Class median", "Explained"]);
    expect(within(table).queryByText("nucleos")).toBeNull();
  });

  it("says so when no slow turn is unexplained, and counts the listed ones past the cap", () => {
    const { unmount } = render(
      <TurnEfficiencyPanel classes={devtimeTurnClasses()} unexplained={devtimeUnexplained({ turns: 0, active_ms: 0, items: [] })}
                           showProject />,
    );
    expect(screen.getByText("No slow turn without an explanation in this window")).toBeTruthy();
    expect(screen.queryByRole("table", { name: "Slow turns no rule explains" })).toBeNull();
    unmount();

    const { unmount: unmountMissing } = render(<TurnEfficiencyPanel classes={devtimeTurnClasses()} showProject />);
    expect(screen.getByText("No slow turn without an explanation in this window")).toBeTruthy();
    expect(screen.queryByRole("table", { name: "Slow turns no rule explains" })).toBeNull();
    unmountMissing();

    render(<TurnEfficiencyPanel classes={devtimeTurnClasses()} unexplained={devtimeUnexplained({ turns: 25 })} showProject />);
    expect(screen.getByText("Showing 3 of 25")).toBeTruthy();
  });
});
