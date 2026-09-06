import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { Section } from "./Section";

describe("Section", () => {
  it("names the region for the eye and for the accessibility tree at once", () => {
    render(
      <Section label="Routines">
        <p>one</p>
      </Section>,
    );

    // The visible heading and the region's accessible name are the same string,
    // which is the whole reason `label` is one prop and not two.
    expect(screen.getByRole("region", { name: "Routines" })).toBeTruthy();
    expect(screen.getByRole("heading", { level: 2, name: "Routines" })).toBeTruthy();
  });

  it("drops to h3 when it sits inside something that already has an h2", () => {
    render(
      <Section label="stdout" level={3}>
        <pre>…</pre>
      </Section>,
    );

    // A run's stream name inside a Panel titled "Stored output" is a child of
    // that panel, not its sibling. Announcing it as an h2 says the opposite.
    expect(screen.getByRole("heading", { level: 3, name: "stdout" })).toBeTruthy();
    expect(screen.queryByRole("heading", { level: 2 })).toBeNull();
  });
});
