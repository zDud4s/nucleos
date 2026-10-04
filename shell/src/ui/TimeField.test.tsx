import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { useState } from "react";
import { TimeField } from "./TimeField";

function Harness({ initial = "09:00", spy }: { initial?: string; spy?: (value: string) => void }) {
  const [value, setValue] = useState(initial);
  return (
    <TimeField
      label="Starts"
      value={value}
      onChange={(next) => {
        spy?.(next);
        setValue(next);
      }}
      within={{ from: "09:00", to: "18:00" }}
    />
  );
}

const trigger = () => screen.getByRole("button", { name: /^Starts,/ });

describe("TimeField", () => {
  it("says its label and its time in one name, and shows the time", () => {
    render(<Harness />);
    expect(screen.getByRole("button", { name: "Starts, 09:00" }).textContent).toBe("09:00");
  });

  it("nudges by five minutes with the arrows, without opening", () => {
    render(<Harness />);

    fireEvent.keyDown(trigger(), { key: "ArrowUp" });
    expect(trigger().textContent).toBe("09:05");
    fireEvent.keyDown(trigger(), { key: "ArrowDown" });
    fireEvent.keyDown(trigger(), { key: "ArrowDown" });
    expect(trigger().textContent).toBe("08:55");
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("snaps an off-step time to the next step rather than adding five to it", () => {
    render(<Harness initial="09:07" />);
    fireEvent.keyDown(trigger(), { key: "ArrowUp" });
    expect(trigger().textContent).toBe("09:10");
  });

  it("opens on two columns, hours and minutes, each with the current value chosen", () => {
    render(<Harness initial="14:30" />);
    fireEvent.click(trigger());

    const hours = screen.getByRole("listbox", { name: "Hour" });
    const minutes = screen.getByRole("listbox", { name: "Minute" });
    expect(within(hours).getAllByRole("option")).toHaveLength(24);
    expect(within(minutes).getAllByRole("option")).toHaveLength(12);
    expect(within(hours).getByRole("option", { selected: true }).textContent).toBe("14");
    expect(within(minutes).getByRole("option", { selected: true }).textContent).toBe("30");
  });

  it("takes the hour and the minute separately, and stays open between them", () => {
    const spy = vi.fn();
    render(<Harness spy={spy} />);
    fireEvent.click(trigger());

    fireEvent.click(within(screen.getByRole("listbox", { name: "Hour" })).getByRole("option", { name: "14" }));
    expect(spy).toHaveBeenLastCalledWith("14:00");
    fireEvent.click(within(screen.getByRole("listbox", { name: "Minute" })).getByRole("option", { name: "45" }));
    expect(spy).toHaveBeenLastCalledWith("14:45");
    expect(screen.getByRole("listbox", { name: "Hour" })).toBeDefined();
  });

  it("steps a column with the arrows and closes on Enter", () => {
    render(<Harness />);
    fireEvent.click(trigger());

    const hours = screen.getByRole("listbox", { name: "Hour" });
    fireEvent.keyDown(hours, { key: "ArrowDown" });
    fireEvent.keyDown(hours, { key: "ArrowDown" });
    expect(trigger().textContent).toBe("11:00");

    fireEvent.keyDown(hours, { key: "Enter" });
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("draws the hours outside the span quieter, and still lets them be picked", () => {
    render(<Harness />);
    fireEvent.click(trigger());

    const hours = screen.getByRole("listbox", { name: "Hour" });
    const evening = within(hours).getByRole("option", { name: "20" });
    expect(evening.className).toContain("ui-time-cell-outside");
    expect(within(hours).getByRole("option", { name: "10" }).className).not.toContain("ui-time-cell-outside");
    fireEvent.click(evening);
    expect(trigger().textContent).toBe("20:00");
  });
});
