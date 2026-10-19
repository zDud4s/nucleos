import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { useUndoToast, UNDO_MS, type UndoToastOptions } from "./useUndoToast";

function Harness({ options }: { options: UndoToastOptions }) {
  const { show, toast } = useUndoToast();
  return (
    <div>
      <button type="button" onClick={() => show(options)}>
        go
      </button>
      {toast}
    </div>
  );
}

describe("useUndoToast", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("renders nothing until shown", () => {
    render(<Harness options={{ message: "Removed", onUndo: () => {} }} />);
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("announces the message with an Undo action", () => {
    render(<Harness options={{ message: "Removed Ada", onUndo: () => {} }} />);
    fireEvent.click(screen.getByText("go"));
    expect(screen.getByRole("status").textContent).toContain("Removed Ada");
    expect(screen.getByRole("button", { name: "Undo" })).toBeTruthy();
  });

  it("Undo reverts and never expires the action", () => {
    const onUndo = vi.fn();
    const onExpire = vi.fn();
    render(<Harness options={{ message: "Removed", onUndo, onExpire }} />);
    fireEvent.click(screen.getByText("go"));
    fireEvent.click(screen.getByRole("button", { name: "Undo" }));
    act(() => {
      vi.advanceTimersByTime(UNDO_MS * 2);
    });
    expect(onUndo).toHaveBeenCalledTimes(1);
    expect(onExpire).not.toHaveBeenCalled();
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("lets the action stand once the window closes", () => {
    const onUndo = vi.fn();
    const onExpire = vi.fn();
    render(<Harness options={{ message: "Removed", onUndo, onExpire }} />);
    fireEvent.click(screen.getByText("go"));
    act(() => {
      vi.advanceTimersByTime(UNDO_MS - 1);
    });
    expect(onExpire).not.toHaveBeenCalled();
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(onExpire).toHaveBeenCalledTimes(1);
    expect(onUndo).not.toHaveBeenCalled();
    expect(screen.queryByRole("status")).toBeNull();
  });
});
