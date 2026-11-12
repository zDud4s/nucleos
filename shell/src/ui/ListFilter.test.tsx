import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { ListFilter } from "./ListFilter";

describe("ListFilter", () => {
  it("names the box by what it searches and reports the query", () => {
    const onQueryChange = vi.fn();
    render(<ListFilter noun="projects" query="" onQueryChange={onQueryChange} />);

    fireEvent.change(screen.getByRole("searchbox", { name: "Search projects" }), {
      target: { value: "nuc" },
    });
    expect(onQueryChange).toHaveBeenCalledWith("nuc");
  });

  it("focuses the box on / unless a text field already has the keys", () => {
    render(
      <div>
        <ListFilter noun="projects" query="" onQueryChange={() => {}} />
        <textarea aria-label="Notes" />
      </div>,
    );
    const box = screen.getByRole("searchbox", { name: "Search projects" });

    fireEvent.keyDown(screen.getByLabelText("Notes"), { key: "/" });
    expect(document.activeElement).not.toBe(box);

    fireEvent.keyDown(document.body, { key: "/" });
    expect(document.activeElement).toBe(box);
  });

  it("clears on Escape, and Enter opens only when exactly one result is left", () => {
    const onQueryChange = vi.fn();
    const onOpenOnly = vi.fn();
    const { rerender } = render(
      <ListFilter noun="projects" query="nuc" onQueryChange={onQueryChange} resultCount={2} onOpenOnly={onOpenOnly} />,
    );
    const box = screen.getByRole("searchbox", { name: "Search projects" });

    fireEvent.keyDown(box, { key: "Escape" });
    expect(onQueryChange).toHaveBeenCalledWith("");

    fireEvent.keyDown(box, { key: "Enter" });
    expect(onOpenOnly).not.toHaveBeenCalled();

    rerender(
      <ListFilter noun="projects" query="nuc" onQueryChange={onQueryChange} resultCount={1} onOpenOnly={onOpenOnly} />,
    );
    fireEvent.keyDown(screen.getByRole("searchbox", { name: "Search projects" }), { key: "Enter" });
    expect(onOpenOnly).toHaveBeenCalledTimes(1);
  });

  it("draws chips as toggles, says how many results are left, and offers to clear", () => {
    const onToggleChip = vi.fn();
    const onClear = vi.fn();
    render(
      <ListFilter
        noun="runs"
        query=""
        onQueryChange={() => {}}
        chips={[
          { id: "failed", label: "Failed", count: 2 },
          { id: "live", label: "Live", count: 5 },
        ]}
        selected={["failed"]}
        onToggleChip={onToggleChip}
        resultCount={2}
        onClear={onClear}
      />,
    );

    expect(screen.getByRole("button", { name: "Failed 2" }).getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Live 5" }));
    expect(onToggleChip).toHaveBeenCalledWith("live");
    expect(screen.getByRole("status").textContent).toBe("2 results");
    fireEvent.click(screen.getByRole("button", { name: "Clear 1 filter" }));
    expect(onClear).toHaveBeenCalledTimes(1);
  });
});
