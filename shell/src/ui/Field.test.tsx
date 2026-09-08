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
});
