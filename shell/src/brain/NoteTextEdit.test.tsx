import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { NoteTextEdit } from "./NoteTextEdit";
import { renderWithQuery } from "../test/harness";

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
