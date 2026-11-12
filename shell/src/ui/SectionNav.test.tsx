import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { SectionNav } from "./SectionNav";

const ITEMS = [
  { id: "ledger", label: "Ledger" },
  { id: "brakes", label: "Brakes" },
];

describe("SectionNav", () => {
  it("is a named navigation landmark with one anchor per section", () => {
    render(<SectionNav label="On this page" items={ITEMS} />);
    expect(screen.getByRole("navigation", { name: "On this page" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Ledger" }).getAttribute("href")).toBe("#ledger");
    expect(screen.getByRole("link", { name: "Brakes" }).getAttribute("href")).toBe("#brakes");
  });

  it("marks only the current section with aria-current", () => {
    render(<SectionNav label="On this page" items={ITEMS} current="brakes" />);
    expect(screen.getByRole("link", { name: "Brakes" }).getAttribute("aria-current")).toBe(
      "location",
    );
    expect(screen.getByRole("link", { name: "Ledger" }).getAttribute("aria-current")).toBeNull();
  });

  it("scrolls smoothly to the section, updates the hash and focuses it", () => {
    const target = document.createElement("section");
    target.id = "ledger";
    target.scrollIntoView = vi.fn();
    document.body.appendChild(target);
    render(<SectionNav label="On this page" items={ITEMS} />);
    fireEvent.click(screen.getByRole("link", { name: "Ledger" }));
    expect(target.scrollIntoView).toHaveBeenCalledWith({ behavior: "smooth", block: "start" });
    expect(window.location.hash).toBe("#ledger");
    expect(document.activeElement).toBe(target);
    target.remove();
  });

  it("leaves a ctrl-click to the browser", () => {
    const target = document.createElement("section");
    target.id = "ledger";
    target.scrollIntoView = vi.fn();
    document.body.appendChild(target);
    render(<SectionNav label="On this page" items={ITEMS} />);
    fireEvent.click(screen.getByRole("link", { name: "Ledger" }), { ctrlKey: true });
    expect(target.scrollIntoView).not.toHaveBeenCalled();
    target.remove();
  });

  it("marks nothing when no section is current", () => {
    render(<SectionNav label="On this page" items={ITEMS} />);
    expect(document.querySelector("[aria-current]")).toBeNull();
  });
});
