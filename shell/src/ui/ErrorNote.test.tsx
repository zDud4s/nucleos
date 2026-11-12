import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { ErrorNote } from "./ErrorNote";

describe("ErrorNote", () => {
  it("renders no inline banner, and toasts an assertive alert without a retry", () => {
    const { container } = render(<ErrorNote>boom</ErrorNote>);
    expect(container.innerHTML).toBe("");
    expect(screen.getByRole("alert").textContent).toContain("boom");
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
  });

  it("offers Retry when given an action, and calls it", () => {
    const onRetry = vi.fn();
    render(<ErrorNote onRetry={onRetry}>boom</ErrorNote>);
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("lets the retry label be renamed", () => {
    render(
      <ErrorNote onRetry={() => {}} retryLabel="Try again">
        boom
      </ErrorNote>,
    );
    expect(screen.getByRole("button", { name: "Try again" })).toBeDefined();
  });
});
