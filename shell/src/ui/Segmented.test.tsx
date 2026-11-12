import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { Segmented } from "./Segmented";

const OPTIONS = [
  { value: "day", label: "Day" },
  { value: "week", label: "Week" },
  { value: "month", label: "Month", disabled: true },
] as const;

describe("Segmented", () => {
  it("is a named group of toggle buttons with the current one pressed", () => {
    render(<Segmented label="Window" value="week" options={OPTIONS} onChange={() => undefined} />);

    const group = screen.getByRole("group", { name: "Window" });
    expect(group.className).toBe("ui-switch");
    expect(screen.getByRole("button", { name: "Week" }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("button", { name: "Day" }).getAttribute("aria-pressed")).toBe("false");
    expect(screen.getByRole("button", { name: "Day" }).className).toBe("ui-switch-seg");
  });

  it("reports the value of the segment pressed", () => {
    const onChange = vi.fn();
    render(<Segmented label="Window" value="week" options={OPTIONS} onChange={onChange} />);

    fireEvent.click(screen.getByRole("button", { name: "Day" }));
    expect(onChange).toHaveBeenCalledWith("day");
  });

  it("keeps a disabled segment drawn but silent", () => {
    const onChange = vi.fn();
    render(<Segmented label="Window" value="week" options={OPTIONS} onChange={onChange} />);

    const month = screen.getByRole("button", { name: "Month" }) as HTMLButtonElement;
    expect(month.disabled).toBe(true);
    fireEvent.click(month);
    expect(onChange).not.toHaveBeenCalled();
  });

  it("takes numbers as values too", () => {
    const onChange = vi.fn();
    render(
      <Segmented
        label="Rows"
        value={10}
        options={[
          { value: 10, label: "10" },
          { value: 50, label: "50" },
        ]}
        onChange={onChange}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "50" }));
    expect(onChange).toHaveBeenCalledWith(50);
  });
});
