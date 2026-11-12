import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { Callout } from "./Callout";

describe("Callout", () => {
  it("wears the tone's class and says its body", () => {
    const { container } = render(<Callout tone="danger">The key was refused.</Callout>);

    expect(container.querySelector(".ui-callout.ui-callout-danger")).toBeTruthy();
    expect(screen.getByText("The key was refused.")).toBeTruthy();
    expect(container.querySelector(".ui-callout-title")).toBeNull();
  });

  it("is info unless told otherwise", () => {
    const { container } = render(<Callout>Read-only.</Callout>);
    expect(container.querySelector(".ui-callout-info")).toBeTruthy();
  });

  it("puts the title before the body", () => {
    const { container } = render(
      <Callout tone="pending" title="Waiting">
        Two approvals are open.
      </Callout>,
    );

    const title = container.querySelector(".ui-callout-title");
    expect(title?.textContent).toBe("Waiting");
    expect(container.querySelector(".ui-callout-body")?.textContent).toBe("Two approvals are open.");
  });

  it("announces only when given a role", () => {
    const { rerender } = render(<Callout>Quiet.</Callout>);
    expect(screen.queryByRole("status")).toBeNull();

    rerender(<Callout role="alert">Loud.</Callout>);
    expect(screen.getByRole("alert").textContent).toContain("Loud.");
  });
});
