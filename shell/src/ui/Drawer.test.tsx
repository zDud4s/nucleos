import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { Drawer } from "./Drawer";

describe("Drawer", () => {
  it("is a dialog named by its title, and not modal by default", () => {
    render(
      <Drawer title="Activity" onClose={() => undefined}>
        <p>Nothing yet.</p>
      </Drawer>,
    );

    const dialog = screen.getByRole("dialog", { name: "Activity" });
    expect(dialog.getAttribute("aria-modal")).toBe("false");
    expect(dialog.className).toBe("ui-drawer");
    expect(screen.getByText("Nothing yet.")).toBeTruthy();
  });

  it("says aria-modal when asked to", () => {
    render(<Drawer title="Activity" modal onClose={() => undefined} />);
    expect(screen.getByRole("dialog").getAttribute("aria-modal")).toBe("true");
  });

  it("moves focus into itself on open", () => {
    render(<Drawer title="Activity" onClose={() => undefined} />);
    expect(document.activeElement).toBe(screen.getByRole("dialog"));
  });

  it("closes on Escape and on the close button", () => {
    const onClose = vi.fn();
    render(<Drawer title="Activity" onClose={onClose} />);

    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it("gives focus back to whatever held it", () => {
    const opener = document.createElement("button");
    document.body.appendChild(opener);
    opener.focus();

    const { unmount } = render(<Drawer title="Activity" onClose={() => undefined} />);
    expect(document.activeElement).not.toBe(opener);
    unmount();
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });
});
