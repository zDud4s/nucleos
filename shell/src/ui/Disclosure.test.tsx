import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { Disclosure } from "./Disclosure";

describe("Disclosure", () => {
  it("starts closed and points its button at the region", () => {
    render(
      <Disclosure label="Advanced">
        <p>deep</p>
      </Disclosure>,
    );
    const btn = screen.getByRole("button", { name: "Advanced" });
    expect(btn.getAttribute("aria-expanded")).toBe("false");
    const region = document.getElementById(btn.getAttribute("aria-controls") ?? "");
    expect(region).not.toBeNull();
    expect(region?.hidden).toBe(true);
  });

  it("opens and closes on click when uncontrolled", () => {
    render(
      <Disclosure label="Advanced">
        <p>deep</p>
      </Disclosure>,
    );
    const btn = screen.getByRole("button", { name: "Advanced" });
    fireEvent.click(btn);
    expect(btn.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("deep").parentElement?.hidden).toBe(false);
    fireEvent.click(btn);
    expect(btn.getAttribute("aria-expanded")).toBe("false");
  });

  it("honours defaultOpen", () => {
    render(
      <Disclosure label="Advanced" defaultOpen>
        <p>deep</p>
      </Disclosure>,
    );
    expect(screen.getByRole("button", { name: "Advanced" }).getAttribute("aria-expanded")).toBe(
      "true",
    );
  });

  it("leaves the state to the caller when controlled", () => {
    const onOpenChange = vi.fn();
    render(
      <Disclosure label="Advanced" open={false} onOpenChange={onOpenChange}>
        <p>deep</p>
      </Disclosure>,
    );
    const btn = screen.getByRole("button", { name: "Advanced" });
    fireEvent.click(btn);
    expect(onOpenChange).toHaveBeenCalledWith(true);
    expect(btn.getAttribute("aria-expanded")).toBe("false");
  });
});
