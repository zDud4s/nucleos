import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { StaleNote } from "./StaleNote";

describe("StaleNote", () => {
  it("prints the last good read from dataUpdatedAt", () => {
    const lastGood = new Date(2026, 7, 17, 14, 32, 9);
    render(<StaleNote dataUpdatedAt={lastGood.getTime()} />);

    // Built the same way the component builds it, so the assertion holds in any
    // time zone the suite happens to run in — the value under test is *which*
    // instant is shown, not how the runner's machine is configured.
    const clock = lastGood.toTimeString().slice(0, 8);
    expect(clock).toMatch(/^\d{2}:\d{2}:\d{2}$/);
    expect(screen.getByRole("status").textContent).toBe(`view is stale — last good read ${clock}`);
  });

  it("is polite, not an alarm — the data is old, not wrong", () => {
    render(<StaleNote dataUpdatedAt={Date.now()} />);
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("status")).toBeDefined();
  });

  it("says there is no good read rather than dating the app to 1970", () => {
    // react-query reports 0 for a query that has never succeeded, and a
    // formatted epoch zero reads as a broken clock rather than as an absence.
    render(<StaleNote dataUpdatedAt={0} />);
    const note = screen.getByRole("status").textContent ?? "";
    expect(note).toBe("view is stale — no good read yet");
    expect(note).not.toContain("1970");
  });
});
