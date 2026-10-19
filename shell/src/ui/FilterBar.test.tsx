import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { FilterBar } from "./FilterBar";

describe("FilterBar", () => {
  it("is a named group around whatever the page puts in it", () => {
    render(
      <FilterBar label="Filter runs">
        <span>inside</span>
      </FilterBar>,
    );
    expect(screen.getByRole("group", { name: "Filter runs" }).textContent).toContain("inside");
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("offers a clear control only when something is on, and says how much it clears", () => {
    const onClear = vi.fn();
    const { rerender } = render(
      <FilterBar label="Filter runs" activeCount={0} onClear={onClear}>
        <span>x</span>
      </FilterBar>,
    );
    expect(screen.queryByRole("button")).toBeNull();

    rerender(
      <FilterBar label="Filter runs" activeCount={1} onClear={onClear}>
        <span>x</span>
      </FilterBar>,
    );
    fireEvent.click(screen.getByRole("button", { name: "Clear 1 filter" }));
    expect(onClear).toHaveBeenCalledTimes(1);

    rerender(
      <FilterBar label="Filter runs" activeCount={3} onClear={onClear}>
        <span>x</span>
      </FilterBar>,
    );
    expect(screen.getByRole("button", { name: "Clear 3 filters" })).toBeTruthy();
  });
});
