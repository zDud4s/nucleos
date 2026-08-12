import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import BrainPicker from "./BrainPicker";

describe("BrainPicker", () => {
  it("says why local is unavailable instead of hiding it", () => {
    render(<BrainPicker brain="cloud" localAvailable={false} busy={false} onChange={() => {}} />);

    // An option that does not exist and an option that is unavailable today are different facts,
    // and only the second is something the reader can go and change.
    expect(screen.getByText(/no local model is configured/i)).toBeTruthy();
    expect(screen.getByRole("radio", { name: /local/i })).toHaveProperty("disabled", true);
  });

  it("warns that switching restarts the conversation's memory", () => {
    render(<BrainPicker brain="cloud" localAvailable busy={false} onChange={() => {}} />);

    expect(screen.getByText(/starts again/i)).toBeTruthy();
  });

  it("will not move the model under a running turn", () => {
    // The daemon refuses this with a 409 for a reason: which model answered is written when the
    // turn's row is born, so swapping mid-turn would make that record lie.
    const onChange = vi.fn();
    render(<BrainPicker brain="cloud" localAvailable busy onChange={onChange} />);

    fireEvent.click(screen.getByRole("radio", { name: /local/i }));

    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByText(/while a turn is in flight/i)).toBeTruthy();
  });

  it("reports the model the conversation is on, and changes to the other", () => {
    const onChange = vi.fn();
    render(<BrainPicker brain="local" localAvailable busy={false} onChange={onChange} />);

    expect(screen.getByRole("radio", { name: /local/i })).toHaveProperty("checked", true);
    fireEvent.click(screen.getByRole("radio", { name: /cloud/i }));

    expect(onChange).toHaveBeenCalledWith("cloud");
  });
});
