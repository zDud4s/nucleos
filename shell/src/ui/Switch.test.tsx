import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { Switch } from "./Switch";

describe("Switch", () => {
  it("is a real checkbox with the switch role, named by its label", () => {
    render(<Switch label="Send digests" checked={false} onChange={() => undefined} />);

    const control = screen.getByRole("switch", { name: "Send digests" }) as HTMLInputElement;
    expect(control.tagName).toBe("INPUT");
    expect(control.type).toBe("checkbox");
    expect(control.className).toBe("ui-switch-check");
    expect(control.checked).toBe(false);
  });

  it("reports the new state when pressed", () => {
    const onChange = vi.fn();
    render(<Switch label="Send digests" checked={false} onChange={onChange} />);

    fireEvent.click(screen.getByRole("switch", { name: "Send digests" }));
    expect(onChange).toHaveBeenCalledWith(true);
  });

  it("toggles from the words as well as the track", () => {
    const onChange = vi.fn();
    render(<Switch label="Send digests" checked onChange={onChange} />);

    fireEvent.click(screen.getByText("Send digests"));
    expect(onChange).toHaveBeenCalledWith(false);
  });

  it("can draw the track alone and keep the name", () => {
    render(<Switch label="Send digests" labelHidden checked onChange={() => undefined} />);

    expect(screen.getByRole("switch", { name: "Send digests" })).toBeTruthy();
    expect(screen.queryByText("Send digests")).toBeNull();
  });

  it("does not report a press while disabled", () => {
    const onChange = vi.fn();
    render(<Switch label="Send digests" checked={false} disabled onChange={onChange} />);

    fireEvent.click(screen.getByRole("switch", { name: "Send digests" }));
    expect(onChange).not.toHaveBeenCalled();
  });
});
