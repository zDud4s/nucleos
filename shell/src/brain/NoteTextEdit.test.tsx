import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { NoteTextEdit } from "./NoteTextEdit";
import { renderWithQuery } from "../test/harness";
import { keys } from "../data/keys";
import type { NoteDetail, NotesGraph, OwnerNote } from "../data/owner-notes";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

import { ApiRefusal } from "../data/client";

const edit = () => fireEvent.click(screen.getByRole("button", { name: "Edit" }));
const area = () => screen.getByRole("textbox", { name: "Note text" }) as HTMLTextAreaElement;
const save = () => screen.getByRole("button", { name: "Save" }) as HTMLButtonElement;

beforeEach(() => {
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue({});
});

describe("NoteTextEdit", () => {
  it("shows the text and an Edit button, then a focused textarea holding the text", () => {
    renderWithQuery(<NoteTextEdit id={4} text="Rust owns the state" />);
    expect(screen.getByText("Rust owns the state")).toBeTruthy();
    edit();
    expect(area().value).toBe("Rust owns the state");
    expect(document.activeElement).toBe(area());
  });

  it("Save PATCHes the new text and returns to the view", async () => {
    renderWithQuery(<NoteTextEdit id={4} text="old" />);
    edit();
    fireEvent.change(area(), { target: { value: "new text" } });
    fireEvent.click(save());
    await waitFor(() =>
      expect(daemon.apiFetch).toHaveBeenCalledWith(
        "/owner-notes/4",
        expect.objectContaining({ method: "PATCH", body: JSON.stringify({ text: "new text" }) }),
      ),
    );
    await waitFor(() => expect(screen.queryByRole("textbox")).toBeNull());
    expect(screen.getByRole("button", { name: "Edit" })).toBeTruthy();
  });

  it("writes the saved note into the detail and graph caches", async () => {
    const note = (text: string): OwnerNote => ({
      id: 4,
      text,
      origin: "shell",
      state: "active",
      created_at: "2026-10-01T00:00:00Z",
      updated_at: "2026-10-01T00:00:00Z",
    });
    const saved = note("new text");
    daemon.apiFetch.mockResolvedValue(saved);
    const { queryClient } = renderWithQuery(<NoteTextEdit id={4} text="old" />);
    queryClient.setQueryData<NoteDetail>(keys.ownerNotes.detail(4), {
      note: note("old"),
      links_out: [],
      links_in: [],
      events: [],
    });
    queryClient.setQueryData<NotesGraph>(keys.ownerNotes.graph(false), {
      notes: [note("old"), { ...note("other"), id: 5 }],
      links: [],
      targets: [],
    });
    edit();
    fireEvent.change(area(), { target: { value: "new text" } });
    fireEvent.click(save());
    await waitFor(() => expect(screen.queryByRole("textbox")).toBeNull());
    expect(queryClient.getQueryData<NoteDetail>(keys.ownerNotes.detail(4))?.note.text).toBe("new text");
    const graph = queryClient.getQueryData<NotesGraph>(keys.ownerNotes.graph(false));
    expect(graph?.notes.map((n) => n.text)).toEqual(["new text", "other"]);
  });

  it("Esc cancels without saving", () => {
    renderWithQuery(<NoteTextEdit id={4} text="old" />);
    edit();
    fireEvent.change(area(), { target: { value: "changed" } });
    fireEvent.keyDown(area(), { key: "Escape" });
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(screen.getByText("old")).toBeTruthy();
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });

  it("Cancel restores the view without saving", () => {
    renderWithQuery(<NoteTextEdit id={4} text="old" />);
    edit();
    fireEvent.change(area(), { target: { value: "changed" } });
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(daemon.apiFetch).not.toHaveBeenCalled();
  });

  it("Save is disabled while the text is unchanged or blank", () => {
    renderWithQuery(<NoteTextEdit id={4} text="old" />);
    edit();
    expect(save().disabled).toBe(true);
    fireEvent.change(area(), { target: { value: "   " } });
    expect(save().disabled).toBe(true);
    fireEvent.change(area(), { target: { value: "new" } });
    expect(save().disabled).toBe(false);
  });

  it("Edit is disabled when the component is", () => {
    renderWithQuery(<NoteTextEdit id={4} text="old" disabled />);
    expect((screen.getByRole("button", { name: "Edit" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("shows the refusal and stays in the editor", async () => {
    daemon.apiFetch.mockRejectedValue(new ApiRefusal(409, "archived", "that note is archived"));
    renderWithQuery(<NoteTextEdit id={4} text="old" />);
    edit();
    fireEvent.change(area(), { target: { value: "new" } });
    fireEvent.click(save());
    expect(await screen.findByText("that note is archived")).toBeTruthy();
    expect(area().value).toBe("new");
  });
});
