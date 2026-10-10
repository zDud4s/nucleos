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
});
