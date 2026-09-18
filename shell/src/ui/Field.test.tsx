import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { Field } from "./Field";

describe("Field", () => {
  it("is one label and one control with its own name", () => {
    const { rerender } = render(
      <Field label="New folder">
        <input aria-label="New folder name" />
      </Field>,
    );

    expect(screen.getByText("New folder")).toBeTruthy();
    expect(screen.getByLabelText("New folder name")).toBeInstanceOf(HTMLInputElement);

    rerender(
      <Field label="New folder" labelHidden>
        <input aria-label="New folder name" />
      </Field>,
    );

    expect(screen.getByText("New folder").classList.contains("ui-field-said")).toBe(true);
    expect(screen.getByLabelText("New folder name")).toBeInstanceOf(HTMLInputElement);
  });

  // The defect this pins: with the helper inside the `<label>`, the box's name was the label AND
  // the hint run together, and the only way out was an `aria-label` restating the visible word.
  it("names the control by its label alone, and describes it by its helper", () => {
    render(
      <Field label="Prompt" helper="Start run waits until this says what the run should do.">
        <textarea />
      </Field>,
    );

    const control = screen.getByRole("textbox", { name: "Prompt" });
    const described = document.getElementById(control.getAttribute("aria-describedby") ?? "");
    expect(described?.textContent).toBe("Start run waits until this says what the run should do.");
  });

  it("keeps the id and the description the caller already gave the control", () => {
    render(
      <>
        <p id="elsewhere">the kill switch is engaged</p>
        <Field label="Budget" helper="blank is the daemon's default">
          <input id="budget" aria-describedby="elsewhere" />
        </Field>
      </>,
    );

    const control = screen.getByLabelText("Budget");
    expect(control.id).toBe("budget");
    const ids = (control.getAttribute("aria-describedby") ?? "").split(" ");
    expect(ids).toHaveLength(2);
    expect(ids[0]).toBe("elsewhere");
    expect(document.getElementById(ids[1])?.textContent).toBe("blank is the daemon's default");
  });
});
