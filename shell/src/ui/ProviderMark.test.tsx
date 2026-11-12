import { describe, expect, it } from "vitest";
import { render } from "@testing-library/react";
import { ProviderMark } from "./ProviderMark";

describe("ProviderMark", () => {
  it("gemini draws its glyph and agy draws the initial A", () => {
    const gemini = render(<ProviderMark provider="gemini" />).container;
    expect(gemini.querySelector("path")).not.toBeNull();
    expect(gemini.querySelector(".ui-provider-mark-initial")).toBeNull();

    // agy deliberately has no asset: it takes the unknown-provider fallback, the initial.
    const agy = render(<ProviderMark provider="agy" />).container;
    expect(agy.querySelector("path")).toBeNull();
    expect(agy.querySelector(".ui-provider-mark-initial")?.textContent).toBe("A");
  });
});
