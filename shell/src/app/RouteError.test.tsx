import { describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { RouteError } from "./RouteError";
import { renderWithRouter } from "../test/harness";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

/**
 * The boundary is the screen somebody sees on their worst minute with this app,
 * so what it must contain is asserted rather than assumed: what broke, where it
 * broke, a way out of the page, and a way to hand the whole thing to somebody
 * else. Rendered through the router harness because one of those four is a
 * `<Link>`, and a link with no router to resolve against throws inside the
 * component whose job is to survive a throw.
 */
describe("RouteError", () => {
  it("the report names the error, offers home, and offers a copy", async () => {
    // jsdom ships no Clipboard API, so this is normally already absent —
    // deleted anyway so the fallback assertion below states its precondition
    // instead of inheriting it from the environment.
    delete (navigator as { clipboard?: unknown }).clipboard;
    const error = new Error("the daemon answered []");

    const { container } = await renderWithRouter(<RouteError error={error} />);

    expect(screen.getByRole("alert").textContent).toContain("the daemon answered []");

    // Nothing has been pressed at this point, and that is the assertion: the
    // stack is on the first paint, not behind a disclosure.
    const stack = container.querySelector("pre.app-route-error-stack");
    expect(stack).not.toBeNull();
    expect(stack?.textContent).toBe(error.stack);

    expect(screen.getByRole("link", { name: "Back to Home" }).getAttribute("href")).toBe("/");

    // No clipboard, so the sentence and not a button — a control that cannot do
    // its job is worse than the instruction to do it by hand.
    expect(screen.getByText(/this window cannot copy for you/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /copy/i })).toBeNull();
  });

  it("hands the clipboard the message and the stack, and nothing else", async () => {
    // What lands on the clipboard is this component's contract and not
    // `CopyButton`'s: the button copies whatever it is given, and the thing
    // worth fixing in place is that what it is given carries no timestamp, no
    // invented route name and nothing else somebody would have to strip out.
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    try {
      const error = new Error("the daemon answered []");

      await renderWithRouter(<RouteError error={error} />);
      fireEvent.click(screen.getByRole("button", { name: /copy this report/i }));

      expect(writeText).toHaveBeenCalledWith(`the daemon answered []\n${error.stack}`);
      await waitFor(() => expect(screen.getByText("Copied")).toBeTruthy());
    } finally {
      delete (navigator as { clipboard?: unknown }).clipboard;
    }
  });
});
