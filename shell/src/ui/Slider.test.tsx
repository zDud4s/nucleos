import { fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { Slider } from "./Slider";

const STEPS = ["low", "medium", "high"];

function Harness({ onLevel }: { onLevel: (i: number) => void }) {
  const [v, setV] = useState(0);
  return (
    <Slider
      steps={STEPS}
      value={v}
      label="Effort"
      onChange={(i) => {
        setV(i);
        onLevel(i);
      }}
    />
  );
}

describe("Slider", () => {
  it("moves between levels with the keyboard and reports the level", () => {
    const seen = vi.fn();
    render(<Harness onLevel={seen} />);
    const thumb = screen.getByRole("slider", { name: "Effort" });
    expect(thumb.getAttribute("aria-valuetext")).toBe("low");
    // Every step is named under the track.
    for (const s of STEPS) expect(screen.getAllByText(s).length).toBeGreaterThan(0);

    fireEvent.keyDown(thumb, { key: "ArrowRight" });
    expect(seen).toHaveBeenLastCalledWith(1);
    expect(screen.getByRole("slider", { name: "Effort" }).getAttribute("aria-valuetext")).toBe("medium");

    fireEvent.keyDown(thumb, { key: "End" });
    expect(seen).toHaveBeenLastCalledWith(2);
    fireEvent.keyDown(thumb, { key: "ArrowLeft" });
    expect(seen).toHaveBeenLastCalledWith(1);
  });
});
