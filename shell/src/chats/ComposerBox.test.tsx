// @vitest-environment jsdom
import { createRef } from "react";
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";

import { ComposerBox } from "./ComposerBox";

function mount(onSubmit: () => void) {
  render(
    <ComposerBox
      text="hello"
      onText={() => {}}
      onSubmit={onSubmit}
      boxRef={createRef<HTMLTextAreaElement>()}
      sendDisabled={false}
    />,
  );
  return screen.getByLabelText("Message");
}

describe("ComposerBox", () => {
  it("ComposerBox sends on Enter and breaks the line on Shift+Enter", () => {
    const onSubmit = vi.fn();
    const box = mount(onSubmit);

    // Shift+Enter is left to the textarea: no submit, default not prevented.
    const shifted = fireEvent.keyDown(box, { key: "Enter", shiftKey: true });
    expect(onSubmit).not.toHaveBeenCalled();
    expect(shifted).toBe(true);

    // Plain Enter submits and keeps the textarea from inserting a newline.
    const plain = fireEvent.keyDown(box, { key: "Enter" });
    expect(onSubmit).toHaveBeenCalledTimes(1);
    expect(plain).toBe(false);

    expect(screen.getByLabelText("Send")).toBeTruthy();
  });

  it("stop stays reachable while text is typed during a running turn", () => {
    const onStop = vi.fn();
    const { rerender } = render(
      <ComposerBox
        text=""
        onText={() => {}}
        onSubmit={() => {}}
        boxRef={createRef<HTMLTextAreaElement>()}
        sendDisabled={false}
        stop={{ onStop, pending: false }}
      />,
    );
    // Empty box: today's single-button swap.
    expect(screen.getByLabelText("Stop")).toBeTruthy();
    expect(screen.queryByLabelText("Send")).toBeNull();

    rerender(
      <ComposerBox
        text="and then"
        onText={() => {}}
        onSubmit={() => {}}
        boxRef={createRef<HTMLTextAreaElement>()}
        sendDisabled={false}
        stop={{ onStop, pending: false }}
      />,
    );
    // Typed text: Send is back beside Stop, which is still there.
    expect(screen.getByLabelText("Send")).toBeTruthy();
    fireEvent.click(screen.getByLabelText("Stop"));
    expect(onStop).toHaveBeenCalledTimes(1);
  });

  it("Enter during IME composition does not submit", () => {
    const onSubmit = vi.fn();
    const box = mount(onSubmit);

    fireEvent.keyDown(box, { key: "Enter", isComposing: true });
    expect(onSubmit).not.toHaveBeenCalled();

    fireEvent.keyDown(box, { key: "Enter" });
    expect(onSubmit).toHaveBeenCalledTimes(1);
  });

  it("an IME Enter never reaches the onKeyDown delegate", () => {
    const onKeyDown = vi.fn(() => false);
    render(
      <ComposerBox
        text="hello"
        onText={() => {}}
        onSubmit={() => {}}
        onKeyDown={onKeyDown}
        boxRef={createRef<HTMLTextAreaElement>()}
        sendDisabled={false}
      />,
    );
    const box = screen.getByLabelText("Message");

    fireEvent.keyDown(box, { key: "Enter", isComposing: true });
    expect(onKeyDown).not.toHaveBeenCalled();

    fireEvent.keyDown(box, { key: "Enter" });
    expect(onKeyDown).toHaveBeenCalledTimes(1);
  });
});
