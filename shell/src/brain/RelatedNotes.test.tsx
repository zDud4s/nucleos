import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import type { OwnerNote, RelatedNote } from "../data/owner-notes";
import { RelatedNotes } from "./RelatedNotes";

const hooks = vi.hoisted(() => ({ useRelatedNotes: vi.fn() }));
vi.mock("../data/owner-notes", async (original) => ({
  ...(await original<typeof import("../data/owner-notes")>()),
  ...hooks,
}));

function note(id: number, text: string): OwnerNote {
  return {
    id,
    text,
    origin: "shell",
    state: "active",
    created_at: "2026-09-01T09:00:00+00:00",
    updated_at: "2026-09-01T09:00:00+00:00",
  };
}

function answer(data: RelatedNote[] | undefined, isError = false) {
  hooks.useRelatedNotes.mockReturnValue({ data, isError });
}

beforeEach(() => {
  hooks.useRelatedNotes.mockReset();
});

describe("RelatedNotes", () => {
  it("asks for the related notes of the given note", () => {
    answer([]);
    render(<RelatedNotes id={4} />);
    expect(hooks.useRelatedNotes).toHaveBeenCalledWith(4);
  });

  it("shows a skeleton while loading", () => {
    answer(undefined);
    render(<RelatedNotes id={4} />);
    expect(screen.getByText("Loading the related notes")).toBeTruthy();
  });

  it("says so when the request fails", () => {
    answer(undefined, true);
    render(<RelatedNotes id={4} />);
    expect(screen.getByRole("alert").textContent).toMatch(/not responding/);
  });

  it("says so when nothing is related", () => {
    answer([]);
    render(<RelatedNotes id={4} />);
    expect(screen.getByText("No related notes.")).toBeTruthy();
  });

  it("names each reason in words and never shows the score", () => {
    answer([
      { note: note(2, "Embedding twin\nmore"), score: 0.8123456, reasons: ["embedding", "shared_target"] },
      { note: note(3, "Word match"), score: 0.31, reasons: ["text"] },
    ]);
    render(<RelatedNotes id={4} />);
    const rows = within(screen.getByRole("list", { name: "Related notes" })).getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    expect(rows[0]!.textContent).toContain("Embedding twin");
    expect(rows[0]!.textContent).not.toContain("more");
    expect(rows[0]!.textContent).toContain("similar meaning, shared link");
    expect(rows[1]!.textContent).toContain("shared words");
    expect(document.body.textContent).not.toMatch(/0\.81|0\.31/);
  });

  it("opens a note in the Brain item format", () => {
    answer([{ note: note(9, "Word match"), score: 0.3, reasons: ["text"] }]);
    const onItem = vi.fn();
    render(<RelatedNotes id={4} onItem={onItem} />);
    fireEvent.click(screen.getByRole("button", { name: "Word match" }));
    expect(onItem).toHaveBeenCalledWith("note:9");
  });

  it("draws no button without an onItem", () => {
    answer([{ note: note(9, "Word match"), score: 0.3, reasons: ["text"] }]);
    render(<RelatedNotes id={4} />);
    expect(screen.queryByRole("button")).toBeNull();
  });
});
