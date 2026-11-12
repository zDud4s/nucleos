import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { Section } from "./Section";
import { SectionTitle } from "./SectionTitle";

describe("SectionTitle", () => {
  it("is a heading and not a landmark", () => {
    render(<SectionTitle>Your stamps</SectionTitle>);

    // The whole reason it exists: fifteen headings sit inside regions whose
    // accessible name deliberately differs from the visible words, and Section
    // would have renamed or duplicated a landmark to reuse four declarations.
    expect(screen.getByRole("heading", { level: 3, name: "Your stamps" })).toBeTruthy();
    expect(screen.queryByRole("region")).toBeNull();
  });

  it("wears the same rank Section does", () => {
    const { container } = render(
      <>
        <SectionTitle level={2}>Your stamps</SectionTitle>
        <Section label="Your stamps">
          <p>one</p>
        </Section>
      </>,
    );

    const headings = Array.from(container.querySelectorAll("h2"));
    expect(headings).toHaveLength(2);
    expect(headings.every((h) => h.className === "ui-section-title")).toBe(true);
  });
});
