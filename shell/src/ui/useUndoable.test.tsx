import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { useUndoable, type UndoableOptions } from "./useUndoable";

function Harness({ options }: { options: UndoableOptions }) {
  const { run, toast } = useUndoable();
  return (
    <>
      <button onClick={() => run(options)}>go</button>
      {toast}
    </>
  );
}

describe("useUndoable", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("does not commit on the click, only when the window closes", () => {
    const commit = vi.fn();
    render(<Harness options={{ message: "Deleted", commit, ms: 1000 }} />);
    fireEvent.click(screen.getByText("go"));
    expect(commit).not.toHaveBeenCalled();
    expect(screen.getByRole("status").textContent).toContain("Deleted");
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(commit).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("Undo restores and never commits", () => {
    const commit = vi.fn();
    const onUndo = vi.fn();
    render(<Harness options={{ message: "Deleted", commit, onUndo, ms: 1000 }} />);
    fireEvent.click(screen.getByText("go"));
    fireEvent.click(screen.getByText("Undo"));
    act(() => {
      vi.advanceTimersByTime(5000);
    });
    expect(onUndo).toHaveBeenCalledTimes(1);
    expect(commit).not.toHaveBeenCalled();
  });

  it("commits a pending action when the page unmounts", () => {
    const commit = vi.fn();
    const { unmount } = render(<Harness options={{ message: "Deleted", commit }} />);
    fireEvent.click(screen.getByText("go"));
    unmount();
    expect(commit).toHaveBeenCalledTimes(1);
  });
});
