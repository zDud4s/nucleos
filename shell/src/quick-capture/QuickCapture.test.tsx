import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { renderWithQuery } from "../test/harness";
import { ApiRefusal } from "../data/client";
import { QuickCapture } from "./QuickCapture";

const tauri = vi.hoisted(() => ({ hide: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ hide: tauri.hide }) }));

const daemon = vi.hoisted(() => ({ apiFetch: vi.fn(), apiText: vi.fn(), probeHealth: vi.fn() }));
vi.mock("../data/client", async (original) => ({
  ...(await original<typeof import("../data/client")>()),
  ...daemon,
}));

beforeEach(() => {
  tauri.hide.mockReset();
  tauri.hide.mockResolvedValue(undefined);
  daemon.apiFetch.mockReset();
  daemon.apiFetch.mockResolvedValue({ id: 1 });
});

function box(): HTMLTextAreaElement {
  return screen.getByRole("textbox", { name: "Capture a note" }) as HTMLTextAreaElement;
}

function type(text: string) {
  fireEvent.change(box(), { target: { value: text } });
}

describe("QuickCapture", () => {
  it("takes focus on mount and again on every focus event", () => {
    renderWithQuery(<QuickCapture />);
    expect(document.activeElement).toBe(box());

    box().blur();
    expect(document.activeElement).not.toBe(box());
    fireEvent.focus(window);
    expect(document.activeElement).toBe(box());
  });

  it("saves on Enter as a shell note, then clears the box and hides", async () => {
    renderWithQuery(<QuickCapture />);
    type("Remember the milk");

    fireEvent.keyDown(box(), { key: "Enter" });

    await waitFor(() => expect(tauri.hide).toHaveBeenCalledTimes(1));
    expect(daemon.apiFetch).toHaveBeenCalledTimes(1);
    const [path, init] = daemon.apiFetch.mock.calls[0] as [string, RequestInit];
    expect(path).toBe("/owner-notes");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({ text: "Remember the milk", origin: "shell" });
    expect(box().value).toBe("");
  });

  it("does not submit on Shift+Enter", () => {
    renderWithQuery(<QuickCapture />);
    type("first line");

    fireEvent.keyDown(box(), { key: "Enter", shiftKey: true });

    expect(daemon.apiFetch).not.toHaveBeenCalled();
    expect(tauri.hide).not.toHaveBeenCalled();
    expect(box().value).toBe("first line");
  });

  it("does not submit a blank box", () => {
    renderWithQuery(<QuickCapture />);
    type("   \n ");

    fireEvent.keyDown(box(), { key: "Enter" });

    expect(daemon.apiFetch).not.toHaveBeenCalled();
    expect(tauri.hide).not.toHaveBeenCalled();
  });

  it("hides on Escape and keeps the draft", async () => {
    renderWithQuery(<QuickCapture />);
    type("half a thought");

    fireEvent.keyDown(box(), { key: "Escape" });

    await waitFor(() => expect(tauri.hide).toHaveBeenCalledTimes(1));
    expect(daemon.apiFetch).not.toHaveBeenCalled();
    expect(box().value).toBe("half a thought");
  });

  it("hides when the window loses focus", async () => {
    renderWithQuery(<QuickCapture />);

    fireEvent.blur(window);

    await waitFor(() => expect(tauri.hide).toHaveBeenCalledTimes(1));
  });

  it("keeps the text and shows the daemon's refusal when the save fails", async () => {
    daemon.apiFetch.mockRejectedValue(new ApiRefusal(409, "kill_switch", "the kill switch is engaged"));
    renderWithQuery(<QuickCapture />);
    type("do not lose me");

    fireEvent.keyDown(box(), { key: "Enter" });

    expect(await screen.findByText(/kill switch/i)).toBeDefined();
    expect(box().value).toBe("do not lose me");
    expect(tauri.hide).not.toHaveBeenCalled();
  });

  it("says the daemon is not responding when the save gets no answer", async () => {
    daemon.apiFetch.mockRejectedValue(new Error("network down"));
    renderWithQuery(<QuickCapture />);
    type("still here");

    fireEvent.keyDown(box(), { key: "Enter" });

    expect(await screen.findByText(/not responding/i)).toBeDefined();
    expect(box().value).toBe("still here");
  });

  it("does not throw where there is no Tauri window to hide", async () => {
    tauri.hide.mockRejectedValue(new Error("no Tauri runtime"));
    renderWithQuery(<QuickCapture />);
    type("note");

    fireEvent.keyDown(box(), { key: "Enter" });

    await waitFor(() => expect(box().value).toBe(""));
  });
});
