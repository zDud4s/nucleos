import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { Well } from "./Well";

describe("Well", () => {
  it("keeps the content's own line breaks when they are part of what it says", () => {
    const { container } = render(<Well as="pre">{'{\n  "ok": false\n}'}</Well>);

    // A payload rendered in a div has its whitespace collapsed, which silently
    // changes what is on screen. That is why `as` has no default.
    const pre = container.querySelector("pre");
    expect(pre).toBeTruthy();
    expect(pre?.textContent).toContain('\n  "ok": false');
  });

  it("takes a name when the thing it holds is labelled elsewhere", () => {
    render(
      <Well as="div" label="worktree root">
        C:/Projects/nucleos
      </Well>,
    );

    expect(screen.getByLabelText("worktree root").textContent).toBe("C:/Projects/nucleos");
  });

  it("changes the face, not the recess, when a person wrote the content", () => {
    const { container } = render(
      <Well as="div" reads>
        the chairman's synthesis
      </Well>,
    );

    // Mono at 11px is a claim about authorship, not a size. A seat's answer set
    // in mono above a synthesis in the body face asserts two different authors.
    const well = container.querySelector("div.ui-well");
    expect(well?.className).toContain("ui-well-reading");
  });

  it("stops growing when the payload is unbounded", () => {
    const { container } = render(
      <Well as="pre" capped>
        {Array.from({ length: 400 }, (_, i) => `line ${i}`).join(String.fromCharCode(10))}
      </Well>,
    );

    expect(container.querySelector("pre")?.className).toContain("ui-well-capped");
  });

  it("keeps the ordering when the ordering is the content", () => {
    render(
      <Well as="ol" label="Navigation chain">
        <li>example.com</li>
        <li>example.com/docs</li>
      </Well>,
    );

    expect(screen.getByRole("list", { name: "Navigation chain" }).tagName).toBe("OL");
  });
});
