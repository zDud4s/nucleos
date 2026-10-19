import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
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

  it("marks nothing when no section is current", () => {
    render(<SectionNav label="On this page" items={ITEMS} />);
    expect(document.querySelector("[aria-current]")).toBeNull();
  });
});
