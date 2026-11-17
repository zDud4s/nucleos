import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { Checkbox, Radio } from "./Checkbox";

describe("Checkbox", () => {
  it("is a real checkbox named by its label", () => {
    render(<Checkbox label="Forget history" checked={false} onChange={() => undefined} />);

    const box = screen.getByRole("checkbox", { name: "Forget history" }) as HTMLInputElement;
    expect(box.type).toBe("checkbox");
    expect(box.className).toContain("ui-check");
  });

  it("renders the bare input when there is no label", () => {
    render(<Checkbox aria-label="Select all" checked={false} onChange={() => undefined} />);

    const box = screen.getByRole("checkbox", { name: "Select all" });
    expect(box.parentElement?.tagName).not.toBe("LABEL");
  });

  it("reports the click through onChange with its native event", () => {
    const onChange = vi.fn();
    render(<Checkbox label="Pick" checked={false} onChange={onChange} />);

    fireEvent.click(screen.getByRole("checkbox", { name: "Pick" }), { shiftKey: true });

    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange.mock.calls[0][0].nativeEvent.shiftKey).toBe(true);
  });

  it("sets the indeterminate state on the node", () => {
    const { rerender } = render(<Checkbox aria-label="All" checked={false} indeterminate onChange={() => undefined} />);
    const box = screen.getByRole("checkbox", { name: "All" }) as HTMLInputElement;
    expect(box.indeterminate).toBe(true);

    rerender(<Checkbox aria-label="All" checked={false} indeterminate={false} onChange={() => undefined} />);
    expect(box.indeterminate).toBe(false);
  });

  it("does not fire when disabled", () => {
    const onChange = vi.fn();
    render(<Checkbox label="Locked" checked={false} disabled onChange={onChange} />);

    fireEvent.click(screen.getByRole("checkbox", { name: "Locked" }));

    expect(onChange).not.toHaveBeenCalled();
  });
});

describe("Radio", () => {
  it("is a real radio named by its label and fires onChange", () => {
    const onChange = vi.fn();
    render(<Radio label="Move now" name="when" checked={false} onChange={onChange} />);

    const radio = screen.getByRole("radio", { name: "Move now" }) as HTMLInputElement;
    expect(radio.type).toBe("radio");
    fireEvent.click(radio);

    expect(onChange).toHaveBeenCalledTimes(1);
  });
});
