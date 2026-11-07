import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { Kbd } from "./Kbd";

describe("Kbd", () => {
  it("renders children as one chip", () => {
    const { container } = render(<Kbd>⌘K</Kbd>);
    expect(container.querySelectorAll("kbd.ui-kbd")).toHaveLength(1);
  });
  it("renders one chip per key", () => {
    const { container } = render(<Kbd keys={["Ctrl", "K"]} />);
    expect(container.querySelectorAll("kbd")).toHaveLength(2);
  });
});
