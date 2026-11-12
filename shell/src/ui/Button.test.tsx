import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { Button } from "./Button";

describe("Button", () => {
  it("is a ghost, type=button, by default", () => {
    render(<Button>Save</Button>);
    const button = screen.getByRole("button", { name: "Save" });
    expect(button.getAttribute("type")).toBe("button");
    expect(button.className).toBe("ui-button ui-button-ghost");
  });

  it("primary carries its own class and composes with an intent", () => {
    render(
      <Button variant="primary" intent="go">
        Start run
      </Button>,
    );
    const button = screen.getByRole("button", { name: "Start run" });
    expect(button.classList.contains("ui-button-primary")).toBe(true);
    expect(button.classList.contains("ui-button-go")).toBe(true);
    expect(button.classList.contains("ui-button-ghost")).toBe(false);
  });
});
