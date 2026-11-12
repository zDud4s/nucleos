import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Button } from "./Button";
import { createRef } from "react";
import { Modal } from "./Modal";

describe("Modal", () => {
  it("is a dialog named by its title and described by its description", () => {
    render(
      <Modal open onOpenChange={() => {}} title="Delete the series?" description="Every occurrence goes.">
        <p>body</p>
      </Modal>,
    );
    const dialog = screen.getByRole("dialog", { name: "Delete the series?" });
    expect(dialog.getAttribute("aria-describedby")).not.toBeNull();
    expect(screen.getByText("Every occurrence goes.").id).toBe(dialog.getAttribute("aria-describedby"));
    expect(screen.getByText("body")).toBeDefined();
  });

  it("draws nothing while closed", () => {
    render(<Modal open={false} onOpenChange={() => {}} title="Hidden" />);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("asks to close on Escape and on its close button", () => {
    const onOpenChange = vi.fn();
    render(<Modal open onOpenChange={onOpenChange} title="Ask" />);

    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(onOpenChange).toHaveBeenLastCalledWith(false);

    onOpenChange.mockClear();
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(onOpenChange).toHaveBeenLastCalledWith(false);
  });

  it("puts the answers in the footer, in the order they were given", () => {
    render(
      <Modal
        open
        onOpenChange={() => {}}
        title="Ask"
        footer={
          <>
            <Button>Cancel</Button>
            <Button variant="danger-solid">Delete</Button>
          </>
        }
      />,
    );
    const foot = screen.getByRole("dialog").querySelector(".ui-modal-foot");
    expect([...foot!.querySelectorAll("button")].map((button) => button.textContent)).toEqual(["Cancel", "Delete"]);
  });

  it("wears the size it was given", () => {
    render(<Modal open onOpenChange={() => {}} title="Form" size="md" />);
    expect(screen.getByRole("dialog").classList.contains("ui-modal-md")).toBe(true);
  });

  it("opens with focus on the first field, else the first footer button, else initialFocus", async () => {
    const { unmount } = render(
      <Modal open onOpenChange={() => {}} title="Form" footer={<Button>Save</Button>}>
        <p>intro</p>
        <input aria-label="Name" />
        <input aria-label="Other" />
      </Modal>,
    );
    await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText("Name")));
    unmount();

    const second = render(
      <Modal
        open
        onOpenChange={() => {}}
        title="Ask"
        footer={
          <>
            <Button>Cancel</Button>
            <Button variant="danger-solid">Delete</Button>
          </>
        }
      >
        <p>Sure?</p>
      </Modal>,
    );
    await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cancel" })));
    second.unmount();

    const ref = createRef<HTMLButtonElement>();
    render(
      <Modal open onOpenChange={() => {}} title="Pick" initialFocus={ref}>
        <p>body</p>
        <button type="button" ref={ref}>
          Chosen
        </button>
      </Modal>,
    );
    await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("button", { name: "Chosen" })));
  });
});
