import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";
import type { OwnerNote, TargetNote } from "../data/owner-notes";
import { NotesAbout } from "./NotesAbout";

const hooks = vi.hoisted(() => ({ useNotesAbout: vi.fn() }));
vi.mock("../data/owner-notes", async (original) => ({
  ...(await original<typeof import("../data/owner-notes")>()),
  ...hooks,
}));

function note(id: number, text: string, state: OwnerNote["state"] = "active"): OwnerNote {
  return {
    id,
    text,
    origin: "shell",
    state,
    created_at: "2026-09-01T09:00:00+00:00",
    updated_at: "2026-09-01T09:00:00+00:00",
  };
}

function answer(data: TargetNote[] | undefined, isError = false) {
  hooks.useNotesAbout.mockReturnValue({ data, isError });
}

beforeEach(() => {
  hooks.useNotesAbout.mockReset();
});

describe("NotesAbout", () => {
  it("asks for the notes of the given target", () => {
    answer([]);
    render(<NotesAbout kind="project" ref="nucleos" />);
    expect(hooks.useNotesAbout).toHaveBeenCalledWith("project", "nucleos");
  });

  it("shows a skeleton while loading", () => {
    answer(undefined);
    render(<NotesAbout kind="project" ref="nucleos" />);
    expect(screen.getByText("Loading the notes")).toBeTruthy();
  });

  it("says so when the request fails", () => {
    answer(undefined, true);
    render(<NotesAbout kind="project" ref="nucleos" />);
    expect(screen.getByRole("alert").textContent).toMatch(/not responding/);
  });

  it("keeps the rows it has when a refetch fails", () => {
    answer([{ note: note(2, "Ship the picker"), link_id: 11, link_type: "relates" }], true);
    render(<NotesAbout kind="project" ref="nucleos" />);
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByText("Ship the picker")).toBeTruthy();
  });

  it("says so when no note is about the target", () => {
    answer([]);
    render(<NotesAbout kind="project" ref="nucleos" />);
    expect(screen.getByText("No notes about this yet.")).toBeTruthy();
  });

  it("lists the rows in the daemon's order with first line, link type and archived tag", () => {
    answer([
      { note: note(2, "Ship the picker\nsecond line"), link_id: 11, link_type: "details" },
      { note: note(3, "Old idea", "archived"), link_id: 12, link_type: "relates" },
    ]);
    render(<NotesAbout kind="project" ref="nucleos" />);
    const rows = within(screen.getByRole("list", { name: "Notes about this" })).getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    expect(rows[0]!.textContent).toContain("Ship the picker");
    expect(rows[0]!.textContent).not.toContain("second line");
    expect(rows[0]!.textContent).toContain("details");
    expect(within(rows[0]!).queryByText("archived")).toBeNull();
    expect(rows[1]!.textContent).toContain("Old idea");
    expect(within(rows[1]!).getByText("archived")).toBeTruthy();
  });

  it("cuts a long first line at about 80 characters", () => {
    answer([{ note: note(2, "x".repeat(200)), link_id: 11, link_type: "relates" }]);
    render(<NotesAbout kind="project" ref="nucleos" />);
    const row = screen.getByRole("listitem");
    expect(row.textContent).toContain("x".repeat(79) + "…");
    expect(row.textContent).not.toContain("x".repeat(81));
  });

  it("opens a note in the Brain item format", () => {
    answer([{ note: note(7, "Ship the picker"), link_id: 11, link_type: "relates" }]);
    const onItem = vi.fn();
    render(<NotesAbout kind="project" ref="nucleos" onItem={onItem} />);
    fireEvent.click(screen.getByRole("button", { name: "Open note 7" }));
    expect(onItem).toHaveBeenCalledWith("note:7");
  });

  it("draws no button without an onItem", () => {
    answer([{ note: note(7, "Ship the picker"), link_id: 11, link_type: "relates" }]);
    render(<NotesAbout kind="project" ref="nucleos" />);
    expect(screen.queryByRole("button")).toBeNull();
  });
});
