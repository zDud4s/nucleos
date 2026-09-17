import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { ConflictNote } from "./ConflictNote";
import { ErrorNote } from "./ErrorNote";

describe("ConflictNote", () => {
  it("says what will happen without interrupting to say it", () => {
    render(<ConflictNote>merging these would drop the older address</ConflictNote>);

    expect(screen.getByText(/would drop the older address/)).toBeTruthy();
    // The contrast with ErrorNote is the whole placement of this rung: nothing
    // has been tried yet, so nothing should interrupt what is being read.
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("is a different rung from the error it is not", () => {
    const { container } = render(
      <>
        <ConflictNote>this will be refused</ConflictNote>
        <ErrorNote>this failed</ErrorNote>
      </>,
    );

    const [conflict, error] = Array.from(container.querySelectorAll("p"));
    expect(conflict?.className).toContain("ui-note-conflict");
    expect(error?.className).toContain("ui-note-error");
    expect(screen.getByRole("alert").textContent).toBe("this failed");
  });
});
