import { afterEach, describe, expect, it, vi } from "vitest";
// `fireEvent` and not `userEvent`, as everywhere else in this directory: user-event
// schedules its own delays, and this button's whole behaviour is a state that reverts on
// a timer of its own.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { CopyButton } from "./CopyButton";

/**
 * The one thing this button must never do is claim a write that did not happen.
 *
 * jsdom has no `navigator.clipboard` at all, which makes the refusal path the DEFAULT here
 * rather than an exotic one — so the honest-failure case is asserted first, and the happy
 * path is the one that needs a stub.
 */

function withClipboard(writeText: (value: string) => Promise<void>) {
  const original = Object.getOwnPropertyDescriptor(navigator, "clipboard");
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
  return () => {
    if (original === undefined) delete (navigator as { clipboard?: unknown }).clipboard;
    else Object.defineProperty(navigator, "clipboard", original);
  };
}

afterEach(() => {
  delete (navigator as { clipboard?: unknown }).clipboard;
});

describe("CopyButton", () => {
  it("copies the value and says so", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    const restore = withClipboard(writeText);
    try {
      render(<CopyButton value="the whole answer" label="this answer" />);
      fireEvent.click(screen.getByRole("button", { name: /copy this answer/i }));
      expect(writeText).toHaveBeenCalledWith("the whole answer");
      await waitFor(() => expect(screen.getByText("Copied")).toBeTruthy());
    } finally {
      restore();
    }
  });

  it("does not say Copied when there is no clipboard to copy to", async () => {
    // The lie this test exists to prevent: somebody reads "Copied", pastes, and gets what
    // was on their clipboard an hour ago.
    render(<CopyButton value="the whole answer" label="this answer" />);
    fireEvent.click(screen.getByRole("button", { name: /copy this answer/i }));
    await waitFor(() => expect(screen.getByText("Select it instead")).toBeTruthy());
    expect(screen.queryByText("Copied")).toBeNull();
  });

  it("does not say Copied when the webview refuses the write", async () => {
    const restore = withClipboard(() => Promise.reject(new Error("denied")));
    try {
      render(<CopyButton value="the whole answer" label="this answer" />);
      fireEvent.click(screen.getByRole("button", { name: /copy this answer/i }));
      await waitFor(() => expect(screen.getByText("Select it instead")).toBeTruthy());
    } finally {
      restore();
    }
  });

  it("can be drawn without its word, for a corner it would not fit in", () => {
    render(<CopyButton value="fn main() {}" label="this code" spoken={false} />);
    // The name is still there for anybody who cannot see the icon.
    expect(screen.getByRole("button", { name: /copy this code/i })).toBeTruthy();
    expect(screen.queryByText("Copy")).toBeNull();
  });
});
