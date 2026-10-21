import { describe, expect, it } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { Tip } from "./Tip";

describe("Tip", () => {
  it("keeps the tip closed until it is asked for", () => {
    render(<Tip tip="the reason">the line</Tip>);
    expect(screen.queryByText("the reason")).toBeNull();
    expect(screen.getByText("the line")).toBeTruthy();
  });

  it("opens on hover and closes when the pointer leaves", () => {
    const { container } = render(<Tip tip="the reason">the line</Tip>);
    const root = container.querySelector(".ui-tip") as HTMLElement;
    fireEvent.mouseEnter(root);
    expect(screen.getByText("the reason")).toBeTruthy();
    fireEvent.mouseLeave(root);
    expect(screen.queryByText("the reason")).toBeNull();
  });

  it("opens on keyboard focus without moving it", () => {
    render(<Tip tip="the reason">the line</Tip>);
    const trigger = screen.getByText("the line");
    trigger.focus();
    fireEvent.focus(trigger);
    expect(screen.getByText("the reason")).toBeTruthy();
    expect(document.activeElement).toBe(trigger);
  });

  it("opens on a press, which is what touch sends, and pins it", () => {
    const { container } = render(<Tip tip="the reason">the line</Tip>);
    const root = container.querySelector(".ui-tip") as HTMLElement;
    fireEvent.click(screen.getByText("the line"));
    fireEvent.mouseLeave(root);
    expect(screen.getByText("the reason")).toBeTruthy();
    fireEvent.click(screen.getByText("the line"));
    expect(screen.queryByText("the reason")).toBeNull();
  });

  it("is a button for the keyboard: Enter and Space pin it, and aria-expanded follows", () => {
    render(<Tip tip="the reason">the line</Tip>);
    const trigger = screen.getByRole("button", { name: "the line" });
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    fireEvent.keyDown(trigger, { key: "Enter" });
    expect(trigger.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByText("the reason")).toBeTruthy();
    fireEvent.keyDown(trigger, { key: " " });
    expect(trigger.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByText("the reason")).toBeNull();
  });

  it("closes on Escape and leaves focus where it was", () => {
    render(<Tip tip="the reason">the line</Tip>);
    const trigger = screen.getByText("the line");
    trigger.focus();
    fireEvent.focus(trigger);
    fireEvent.keyDown(trigger, { key: "Escape" });
    expect(screen.queryByText("the reason")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("closes on Escape even when only the pointer opened it", () => {
    const { container } = render(<Tip tip="the reason">the line</Tip>);
    fireEvent.mouseEnter(container.querySelector(".ui-tip") as HTMLElement);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByText("the reason")).toBeNull();
  });

  it("closes a pinned tip on a press elsewhere", () => {
    render(<Tip tip="the reason">the line</Tip>);
    fireEvent.click(screen.getByText("the line"));
    expect(screen.getByText("the reason")).toBeTruthy();
    fireEvent.pointerDown(document.body);
    expect(screen.queryByText("the reason")).toBeNull();
  });

  it("describes its trigger by the tip", () => {
    render(<Tip tip="the reason">the line</Tip>);
    const trigger = screen.getByText("the line");
    const id = trigger.getAttribute("aria-describedby");
    expect(id).toBeTruthy();
    fireEvent.focus(trigger);
    const tip = screen.getByRole("tooltip");
    expect(tip.id).toBe(id);
    expect(tip.textContent).toBe("the reason");
  });

  it("draws a named mark when it has a name and no children", () => {
    render(<Tip tip="the reason" name="About this" />);
    const mark = screen.getByRole("button", { name: "About this" });
    expect(mark.textContent).toBe("i");
    expect(mark.getAttribute("aria-describedby")).toBeTruthy();
  });

  it("can keep the tip in the document for a description that is always read", () => {
    render(
      <Tip tip="the reason" persist>
        the line
      </Tip>,
    );
    expect(screen.getByText("the reason")).toBeTruthy();
    expect(screen.getByRole("tooltip", { hidden: true }).hasAttribute("hidden")).toBe(true);
  });

  it("hands a render-prop trigger the state it needs to wire its own control", () => {
    render(
      <Tip tip="the reason">
        {({ tipId, open, pinned, toggle }) => (
          <button type="button" aria-controls={tipId} aria-expanded={pinned} onClick={toggle}>
            {open ? "open" : "shut"}
          </button>
        )}
      </Tip>,
    );
    const button = screen.getByRole("button");
    expect(button.textContent).toBe("shut");
    fireEvent.click(button);
    expect(button.textContent).toBe("open");
    expect(button.getAttribute("aria-expanded")).toBe("true");
    expect(button.getAttribute("aria-controls")).toBe(screen.getByRole("tooltip").id);
  });
});
