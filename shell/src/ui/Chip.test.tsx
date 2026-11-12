import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { Chip } from "./Chip";

describe("Chip", () => {
  it("is a toggle button that reports its state and its count", () => {
    const onToggle = vi.fn();
    const { rerender } = render(
      <Chip pressed={false} onToggle={onToggle} count={3}>
        Failed
      </Chip>,
    );

    const chip = screen.getByRole("button", { name: "Failed 3" });
    expect(chip.getAttribute("aria-pressed")).toBe("false");
    expect(chip.getAttribute("type")).toBe("button");
    fireEvent.click(chip);
    expect(onToggle).toHaveBeenCalledTimes(1);

    rerender(
      <Chip pressed onToggle={onToggle} count={3}>
        Failed
      </Chip>,
    );
    expect(screen.getByRole("button", { name: "Failed 3" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("draws no count when none is given, and a zero count is still drawn", () => {
    const { container, rerender } = render(
      <Chip pressed={false} onToggle={() => {}}>
        All
      </Chip>,
    );
    expect(container.querySelector(".ui-chip-count")).toBeNull();

    rerender(
      <Chip pressed={false} onToggle={() => {}} count={0}>
        All
      </Chip>,
    );
    expect(container.querySelector(".ui-chip-count")?.textContent).toBe("0");
  });
});
