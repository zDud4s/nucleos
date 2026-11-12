import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { Plus } from "lucide-react";
import { IconButton } from "./IconButton";

describe("IconButton", () => {
  it("is named by its label, not by its glyph", () => {
    render(<IconButton label="New job in alpha" icon={Plus} />);
    const button = screen.getByRole("button", { name: "New job in alpha" });
    // The same words for the pointer, which has no other way to learn what a `+` does.
    expect(button.getAttribute("title")).toBe("New job in alpha");
    expect(button.querySelector("svg")?.getAttribute("aria-hidden")).toBe("true");
  });

  it("is a plain button unless told otherwise, so it never submits the form it sits in", () => {
    render(
      <form>
        <IconButton label="Add" icon={Plus} />
      </form>,
    );
    expect(screen.getByRole("button", { name: "Add" }).getAttribute("type")).toBe("button");
  });

  it("passes its handlers and relations through", () => {
    const onClick = vi.fn();
    render(<IconButton label="Add" icon={Plus} aria-controls="panel" onClick={onClick} />);
    const button = screen.getByRole("button", { name: "Add" });
    fireEvent.click(button);
    expect(onClick).toHaveBeenCalledOnce();
    expect(button.getAttribute("aria-controls")).toBe("panel");
  });
});
