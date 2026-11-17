import { describe, expect, it } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { ClampedText } from "./ClampedText";

describe("ClampedText", () => {
  it("is clamped by default and expands and collapses in place", () => {
    render(<ClampedText overflowing>{"line one\nline two\nline three\nline four"}</ClampedText>);
    const text = document.querySelector(".ui-clamped-text") as HTMLElement;
    const toggle = screen.getByRole("button", { name: "Show more" });
    expect(text.getAttribute("data-expanded")).toBeNull();
    expect(text.getAttribute("data-clamped")).toBe("true");
    expect(text.style.getPropertyValue("--ui-clamp-lines")).toBe("3");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");

    fireEvent.click(toggle);
    const less = screen.getByRole("button", { name: "Show less" });
    expect(less.getAttribute("aria-expanded")).toBe("true");
    expect(text.getAttribute("data-expanded")).toBe("true");

    fireEvent.click(less);
    expect(screen.getByRole("button", { name: "Show more" }).getAttribute("aria-expanded")).toBe("false");
    expect(text.getAttribute("data-expanded")).toBeNull();
  });

  it("shows no toggle when the text does not overflow", () => {
    render(<ClampedText>short</ClampedText>);
    expect(screen.queryByRole("button")).toBeNull();
  });
});
